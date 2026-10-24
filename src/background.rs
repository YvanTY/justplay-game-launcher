//! Work kept off the UI thread: decoding backgrounds and fetching art from Steam. Each worker
//! blocks on its own queue (no CPU while idle) and sends results through one channel, then pokes
//! the UI, which drains the channel on its own thread.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::AppWindow;
use crate::scaled_art::{self, Kind, Pixels};
use crate::steam;

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
}

pub enum ArtOutcome {
    /// Steam has the game; `saved` new images are now in its art folder.
    Found { app: u32, saved: usize },
    /// Steam has no game with exactly this title.
    NoMatch,
    /// Offline or Steam unreachable: worth trying again next start.
    Failed(String),
}

pub struct ArtJob {
    pub id: String,
    pub title: String,
    pub dir: PathBuf,
    /// File names from `steam::SLOTS` that the game is missing.
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
                let outcome = match steam::find(&job.title) {
                    Ok(Some(app)) => download_one_by_one(&notifier, &job, app),
                    Ok(None) => ArtOutcome::NoMatch,
                    Err(e) => ArtOutcome::Failed(e.to_string()),
                };
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

/// The cover is small and the background large: showing each image as it lands means the tile
/// changes within a second instead of after the whole set.
fn download_one_by_one(notifier: &Notifier, job: &ArtJob, app: u32) -> ArtOutcome {
    let mut saved = 0;
    for file in &job.wanted {
        match steam::download(app, &job.dir, &[file]) {
            Ok(0) => {}
            Ok(count) => {
                saved += count;
                notifier.send(Event::ArtFile { id: job.id.clone() });
            }
            Err(e) => return ArtOutcome::Failed(e.to_string()),
        }
    }
    ArtOutcome::Found { app, saved }
}
