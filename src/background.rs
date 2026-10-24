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
    /// One more picture for the Choose art screen (`slot` 0 cover, 1 background, 2 logo),
    /// with its thumbnail already decoded.
    ArtChoice {
        game: String,
        slot: usize,
        path: PathBuf,
        label: String,
        hash: u64,
        in_use: bool,
        /// The sharp version to fetch if this choice is picked.
        full: Option<String>,
        thumbnail: Option<Pixels>,
    },
    /// The sharp version of a picked choice arrived (`token` tells a later pick apart).
    ArtUpgrade {
        game: String,
        slot: usize,
        token: u64,
        path: PathBuf,
    },
    /// The Choose art screen has every choice there is; `error` when Steam couldn't be reached.
    ArtChoicesDone {
        game: String,
        error: Option<String>,
    },
    /// The folder browser's listing of `dir`.
    Listing {
        dir: PathBuf,
        folders: Vec<PathBuf>,
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

pub struct ChoicesJob {
    pub game: String,
    pub title: String,
    /// Where downloaded choices are kept (reused if the screen is opened again).
    pub cache: PathBuf,
    /// The art in use per slot.
    pub current: [Option<PathBuf>; 3],
    pub scale: f32,
}

/// Gathers every picture worth offering on the Choose art screen.
pub fn spawn_art_choices(notifier: Notifier, job: ChoicesJob, cancel: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new()
        .name("art-choices".into())
        .spawn(move || {
            let error = gather_choices(&notifier, &job, &cancel)
                .err()
                .map(|e| e.to_string());
            notifier.send(Event::ArtChoicesDone {
                game: job.game,
                error,
            });
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("art choices could not start: {e}"));
    }
}

/// The art in use, then every store match's pictures (one worker per match, so the covers of
/// all of them show within a second or two), then PCGamingWiki's cover.
fn gather_choices(
    notifier: &Notifier,
    job: &ChoicesJob,
    cancel: &AtomicBool,
) -> Result<(), net::Error> {
    use crate::art_picker::SLOTS;
    // The art in use, copied so it can be picked again after being replaced.
    let _ = std::fs::create_dir_all(&job.cache);
    for (slot, current) in job.current.iter().enumerate() {
        let Some(source) = current else { continue };
        let extension = source
            .extension()
            .map_or("jpg".into(), |e| e.to_string_lossy().to_lowercase());
        let copy = job
            .cache
            .join(format!("in-use-{}.{extension}", SLOTS[slot]));
        if std::fs::copy(source, &copy).is_ok() {
            let label = "In use now".to_string();
            offer(notifier, job, slot, copy, label, true, None);
        }
    }

    let apps: Vec<(u32, String)> = steam::search(&job.title)?
        .into_iter()
        .filter(|(_, name)| !steam::is_add_on(name))
        .take(4)
        .collect();

    // Steam's matches start downloading at once; PCGamingWiki (a slower site, several requests)
    // is asked alongside them rather than before.
    std::thread::scope(|scope| {
        let workers: Vec<_> = apps
            .iter()
            .map(|(app, name)| {
                let notifier = notifier.clone();
                scope.spawn(move || choices_from_app(&notifier, job, cancel, *app, name))
            })
            .collect();
        let mut result = choices_from_wiki(notifier, job, cancel, &apps);
        for worker in workers {
            if let Ok(Err(e)) = worker.join() {
                result = Err(e);
            }
        }
        result
    })
}

/// PCGamingWiki's cover, and the pictures of the store page it knows if Steam's search didn't
/// list it (a game Steam no longer sells).
fn choices_from_wiki(
    notifier: &Notifier,
    job: &ChoicesJob,
    cancel: &AtomicBool,
    apps: &[(u32, String)],
) -> Result<(), net::Error> {
    let Some(wiki) = pcgw::lookup(&job.title)? else {
        return Ok(());
    };
    if let Some(cover) = wiki.cover.as_deref()
        && let Some(path) = fetch(job, cancel, cover, "wiki-cover")?
    {
        let label = "PCGamingWiki cover".to_string();
        offer(notifier, job, 0, path, label, false, None);
    }
    match wiki.steam_app {
        Some(app) if !apps.iter().any(|(id, _)| *id == app) => {
            let name = format!("{} (older store page)", job.title);
            choices_from_app(notifier, job, cancel, app, &name)
        }
        _ => Ok(()),
    }
}

/// One store match: cover, logo, background, then the banner and three screenshots. Previews
/// are the light versions (a 4K background is ~1.5 MB); the sharp one is fetched if picked.
fn choices_from_app(
    notifier: &Notifier,
    job: &ChoicesJob,
    cancel: &AtomicBool,
    app: u32,
    name: &str,
) -> Result<(), net::Error> {
    use crate::art_picker::SLOTS;
    for slot in [0, 2, 1] {
        let urls = steam::image_urls(app, SLOTS[slot]);
        for url in urls.iter().rev() {
            if let Some(path) = fetch(job, cancel, url, &format!("{app}-{}", SLOTS[slot]))? {
                let full = urls.first().filter(|sharp| *sharp != url).cloned();
                let label = format!("Steam · {name}");
                offer(notifier, job, slot, path, label, false, full);
                break;
            }
        }
    }
    let banner = steam::banner_url(app);
    if let Some(path) = fetch(job, cancel, &banner, &format!("{app}-banner"))? {
        let label = format!("Store banner · {name}");
        offer(notifier, job, 0, path, label, false, None);
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    for (i, shot) in steam::screenshots(app)?.into_iter().take(3).enumerate() {
        if let Some(path) = fetch(job, cancel, &shot.thumbnail, &format!("{app}-shot{i}"))? {
            let label = format!("Screenshot {} · {name}", i + 1);
            offer(notifier, job, 1, path, label, false, Some(shot.full));
        }
    }
    Ok(())
}

/// Downloaded once into the job's cache; Ok(None) when the server has no such picture or the
/// screen was closed.
fn fetch(
    job: &ChoicesJob,
    cancel: &AtomicBool,
    url: &str,
    name: &str,
) -> Result<Option<PathBuf>, net::Error> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    if let Some(found) = find_image(&job.cache, name) {
        return Ok(Some(found));
    }
    steam::save(url, &job.cache, name)?;
    Ok(find_image(&job.cache, name))
}

/// Sends one choice with its thumbnail decoded here, off the UI thread.
fn offer(
    notifier: &Notifier,
    job: &ChoicesJob,
    slot: usize,
    path: PathBuf,
    label: String,
    in_use: bool,
    full: Option<String>,
) {
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let kind = if slot == 2 { Kind::Logo } else { Kind::Icon };
    notifier.send(Event::ArtChoice {
        game: job.game.clone(),
        slot,
        hash: crate::art_picker::picture_hash(&bytes),
        thumbnail: scaled_art::decode(&path, kind, job.scale),
        path,
        label,
        in_use,
        full,
    });
}

/// Fetches the sharp version of a picked choice into `cache`.
pub fn spawn_art_upgrade(
    notifier: Notifier,
    url: String,
    cache: PathBuf,
    game: String,
    slot: usize,
    token: u64,
) {
    let spawned = std::thread::Builder::new()
        .name("art-upgrade".into())
        .spawn(move || {
            let name = format!("full-{token}");
            match steam::save(&url, &cache, &name) {
                Ok(_) => {
                    if let Some(path) = find_image(&cache, &name) {
                        notifier.send(Event::ArtUpgrade {
                            game,
                            slot,
                            token,
                            path,
                        });
                    }
                }
                // The preview stays; it is the same picture, only smaller.
                Err(e) => crate::diag::log(&format!("sharp art for {game}: {e}")),
            }
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("art upgrade could not start: {e}"));
    }
}

/// Lists a folder for the Add games browser without freezing the UI on a sleeping drive.
pub fn spawn_listing(notifier: Notifier, dir: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("folder-listing".into())
        .spawn(move || {
            let folders = crate::add_games::list_folders(&dir);
            notifier.send(Event::Listing { dir, folders });
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("folder listing could not start: {e}"));
    }
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
