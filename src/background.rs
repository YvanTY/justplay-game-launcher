//! Work kept off the UI thread: decoding backgrounds, fetching art from Steam and scanning the
//! drives for games. Each worker
//! blocks on its own queue (no CPU while idle) and sends results through one channel, then pokes
//! the UI, which drains the channel on its own thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::AppWindow;
use crate::library::discover::{self, Candidate, Known};
use crate::library::find_image;
use crate::scaled_art::{self, Kind, Pixels};
use crate::{net, pcgw, steam};

pub enum Event {
    /// A background finished decoding; None when this decoder can't read the file.
    Hero {
        path: PathBuf,
        pixels: Option<Pixels>,
    },
    /// One more image landed in this game's art folder; show it without waiting for the rest.
    ArtFile {
        id: String,
    },
    Art {
        id: String,
        outcome: ArtOutcome,
    },
    /// The drive scan has looked at this many folders so far.
    ScanProgress {
        folders: usize,
    },
    ScanDone {
        found: Vec<Candidate>,
    },
}

pub enum ArtOutcome {
    /// The game was identified (`app`: its Steam app, if it has one); `saved` new images are
    /// now in its art folder.
    Found { app: Option<u32>, saved: usize },
    /// Neither Steam nor PCGamingWiki knows a game with this title.
    NoMatch,
    /// Offline or Steam unreachable: worth trying again next start.
    Failed(String),
}

pub struct ArtJob {
    pub id: String,
    pub title: String,
    pub dir: PathBuf,
    /// Slots ("icon", "hero", "logo") the game has no image for.
    pub wanted: Vec<&'static str>,
}

#[derive(Clone)]
pub struct Notifier {
    sender: Sender<Event>,
    ui: slint::Weak<AppWindow>,
}

impl Notifier {
    pub fn new(ui: slint::Weak<AppWindow>) -> (Notifier, Receiver<Event>) {
        let (sender, receiver) = mpsc::channel();
        (Notifier { sender, ui }, receiver)
    }

    pub fn send(&self, event: Event) {
        if self.sender.send(event).is_err() {
            return;
        }
        let ui = self.ui.clone();
        // Only fails once the event loop has shut down, when there is nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui.upgrade() {
                ui.invoke_background_event();
            }
        });
    }
}

/// Requests are (background path, screen scale).
pub fn spawn_hero_loader(notifier: Notifier) -> Sender<(PathBuf, f32)> {
    let (sender, requests) = mpsc::channel::<(PathBuf, f32)>();
    let spawned = std::thread::Builder::new()
        .name("hero-loader".into())
        .spawn(move || {
            while let Ok(mut request) = requests.recv() {
                // Scrolling queues several; only the newest selection still matters.
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let (path, scale) = request;
                let pixels = scaled_art::decode(&path, Kind::Hero, scale);
                notifier.send(Event::Hero { path, pixels });
            }
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("background decoder could not start: {e}"));
    }
    sender
}

pub fn spawn_art_fetcher(notifier: Notifier) -> Sender<ArtJob> {
    let (sender, jobs) = mpsc::channel::<ArtJob>();
    let spawned = std::thread::Builder::new()
        .name("art-fetcher".into())
        .spawn(move || {
            while let Ok(job) = jobs.recv() {
                let id = job.id.clone();
                let landed = || notifier.send(Event::ArtFile { id: id.clone() });
                let outcome = fetch_art(&job, landed).unwrap_or_else(|e| {
                    // Whatever was saved before the failure stays; the rest is retried.
                    ArtOutcome::Failed(e.to_string())
                });
                notifier.send(Event::Art {
                    id: job.id,
                    outcome,
                });
            }
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("art fetcher could not start: {e}"));
    }
    sender
}

/// Finds the game and fills its missing slots from the best source that has them:
/// 1. Steam's library images, for a game the store search finds;
/// 2. PCGamingWiki, for games Steam stopped selling (their Steam images often remain) and box
///    art for games that were never on Steam;
/// 3. a store screenshot as the background and the store banner as the icon, for old games
///    without library images.
///
/// Each image appears as soon as it lands: the cover is small and the background large.
/// `on_image` runs after each saved image.
fn fetch_art(job: &ArtJob, mut on_image: impl FnMut()) -> Result<ArtOutcome, net::Error> {
    let missing = || -> Vec<&'static str> {
        job.wanted
            .iter()
            .copied()
            .filter(|slot| find_image(&job.dir, slot).is_none())
            .collect()
    };
    let mut saved = 0;
    let mut landed = |count: usize| {
        if count > 0 {
            saved += count;
            on_image();
        }
    };
    let mut apps: Vec<u32> = Vec::new();
    let mut known = false;
    if let Some(app) = steam::find(&job.title)? {
        known = true;
        apps.push(app);
        for slot in missing() {
            landed(steam::download(app, &job.dir, &[slot])?);
        }
    }
    if !missing().is_empty()
        && let Some(wiki) = pcgw::lookup(&job.title)?
    {
        known = true;
        if let Some(app) = wiki.steam_app.filter(|app| !apps.contains(app)) {
            apps.push(app);
            for slot in missing() {
                landed(steam::download(app, &job.dir, &[slot])?);
            }
        }
        if missing().contains(&"icon")
            && let Some(cover) = &wiki.cover
        {
            landed(usize::from(steam::save(cover, &job.dir, "icon")?));
        }
    }
    for &app in &apps {
        if missing().contains(&"hero") {
            landed(usize::from(steam::download_screenshot(app, &job.dir)?));
        }
        if missing().contains(&"icon") {
            landed(usize::from(steam::download_banner(app, &job.dir)?));
        }
    }
    Ok(if known {
        ArtOutcome::Found {
            app: apps.first().copied(),
            saved,
        }
    } else {
        ArtOutcome::NoMatch
    })
}

/// Looks for games on `roots` for the Add games screen. Setting `cancel` stops it; a cancelled
/// scan reports nothing.
pub fn spawn_scan(notifier: Notifier, roots: Vec<PathBuf>, known: Known, cancel: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new()
        .name("drive-scan".into())
        .spawn(move || {
            let found = discover::scan(&roots, &known, &cancel, |folders| {
                notifier.send(Event::ScanProgress { folders });
            });
            if !cancel.load(Ordering::Relaxed) {
                notifier.send(Event::ScanDone { found });
            }
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("drive scan could not start: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Yvan's games that found no art with the first lookup. Downloads into a temporary folder:
    /// `cargo test yvans_titles -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn yvans_titles() {
        let root = std::env::temp_dir().join(format!("gl-art-live-{}", std::process::id()));
        for title in [
            "Blur",
            "Demon Slayer - Kimetsu no Yaiba",
            "eFootball PES 2021",
            "FIFA 19",
            "Grand Theft Auto V",
            "Pro Evolution Soccer 2017",
            "PRO EVOLUTION SOCCER 2019",
        ] {
            let job = ArtJob {
                id: title.into(),
                title: title.into(),
                dir: root.join(crate::library::safe_file_name(title)),
                wanted: vec!["icon", "hero", "logo"],
            };
            let outcome = fetch_art(&job, || {}).unwrap();
            let files: Vec<String> = std::fs::read_dir(&job.dir)
                .map(|d| {
                    d.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            let found = match outcome {
                ArtOutcome::Found { app, saved } => format!("app {app:?}, {saved} saved"),
                ArtOutcome::NoMatch => "no match".into(),
                ArtOutcome::Failed(e) => format!("failed: {e}"),
            };
            println!("{title:<34} {found:<24} {files:?}");
            if title.starts_with("Demon Slayer") {
                assert_eq!(files_in(&job.dir), 0, "ambiguous: left to the art picker");
            } else {
                assert!(files_in(&job.dir) > 0, "{title}");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    fn files_in(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
    }
}
