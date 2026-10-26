//! Work kept off the UI thread: decoding backgrounds, fetching art from Steam and scanning the
//! drives for games. Each worker
//! blocks on its own queue (no CPU while idle) and sends results through one channel, then pokes
//! the UI, which drains the channel on its own thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::AppWindow;
use crate::library;
use crate::library::discover::{self, Candidate, Known};
use crate::library::find_image;
use crate::scaled_art::{self, Kind, Pixels};
use crate::{msstore, net, pcgw, sgdb, steam};

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
    /// One more picture for the Choose art screen (`slot` 0 cover, 1 background, 2 logo,
    /// 3 a trailer for music), with its thumbnail already decoded.
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
    /// Greyed-out games whose files can all be reached again (their drive came back).
    Reachable {
        ids: Vec<String>,
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
    /// A PS3 game's menu music was converted (`music`), or couldn't be (`error`).
    Ps3Music {
        id: String,
        result: Result<PathBuf, String>,
    },
    /// GitHub's latest release, newer or not.
    LatestRelease(Result<crate::update::Update, String>),
    /// A game's music from its store trailer: staged (`Some(file, trailer)`, to be put in
    /// place as music.ogg), no trailer found (`None`), or a failure worth trying again later.
    TrailerMusic {
        id: String,
        result: Result<Option<(PathBuf, u64)>, String>,
        /// Picked on the Choose art screen (rather than found automatically).
        picked: bool,
    },
}

/// Music from a game's store trailer: (game id, title, Steam app if known, art folder).
pub struct MusicJob {
    pub id: String,
    pub title: String,
    pub app: Option<u32>,
    pub dir: PathBuf,
}

/// Gives games without music the soundtrack of their main store trailer, one at a time (a
/// trailer's sound takes ~15-30 s to fetch). Stops without ffmpeg or internet.
pub fn spawn_trailer_music(notifier: Notifier, jobs: Vec<MusicJob>) {
    let spawned = std::thread::Builder::new()
        .name("trailer-music".into())
        .spawn(move || {
            for job in jobs {
                let found = (|| -> Result<Option<steam::Trailer>, net::Error> {
                    let app = match job.app {
                        Some(app) => Some(app),
                        None => steam::find(&job.title)?,
                    };
                    let Some(app) = app else { return Ok(None) };
                    Ok(steam::trailers(app)?.into_iter().next())
                })();
                let trailer = match found {
                    Ok(trailer) => trailer,
                    Err(e) => {
                        crate::diag::log(&format!("trailer music paused (offline?): {e}"));
                        return;
                    }
                };
                let Some(trailer) = trailer else {
                    notifier.send(Event::TrailerMusic {
                        id: job.id,
                        result: Ok(None),
                        picked: false,
                    });
                    continue;
                };
                // Staged: the UI thread puts it in place, unless "None" or another trailer
                // was picked meanwhile.
                let out = job.dir.join("music.ogg.auto");
                let result = match library::snd0::trailer_audio(&trailer.stream, &out) {
                    Ok(()) => Ok(Some((out, trailer.id))),
                    Err(library::snd0::Failure::NoFfmpeg) => {
                        crate::diag::log("ffmpeg not found: no trailer music");
                        return;
                    }
                    Err(library::snd0::Failure::Other(e)) => Err(e),
                };
                notifier.send(Event::TrailerMusic {
                    id: job.id,
                    result,
                    picked: false,
                });
            }
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("trailer music could not start: {e}"));
    }
}

/// The trailer picked on the Choose art screen, staged as the game's music (the UI thread
/// moves the music there before into `_previous` and puts this file in place).
pub fn spawn_picked_trailer(
    notifier: Notifier,
    id: String,
    trailer: u64,
    stream: String,
    dir: PathBuf,
) {
    let spawned = std::thread::Builder::new()
        .name("picked-trailer".into())
        .spawn(move || {
            let out = dir.join("music.ogg.picked");
            let result = match library::snd0::trailer_audio(&stream, &out) {
                Ok(()) => Ok(Some((out, trailer))),
                Err(library::snd0::Failure::NoFfmpeg) => Err(
                    "ffmpeg isn't installed: it's needed to take the music from a trailer".into(),
                ),
                Err(library::snd0::Failure::Other(e)) => Err(e),
            };
            notifier.send(Event::TrailerMusic {
                id,
                result,
                picked: true,
            });
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("trailer music could not start: {e}"));
    }
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
    /// SteamGridDB key from config.toml; without it that source is skipped.
    pub sgdb_key: Option<String>,
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
    // The other sources are extras: one failing doesn't stop the rest. If something is still
    // missing afterwards, the failure is reported so the lookup is tried again later.
    let mut trouble = None;
    let mut soft = |result: Result<bool, net::Error>| match result {
        Ok(found) => found,
        Err(e) => {
            trouble.get_or_insert(e);
            false
        }
    };
    // SteamGridDB (with a key): community art for almost any game.
    if let Some(key) = &job.sgdb_key
        && !missing().is_empty()
    {
        soft((|| {
            let Some(game) = sgdb::find(key, &job.title)? else {
                return Ok(false);
            };
            known = true;
            for (slot, kind) in [("icon", "grids"), ("hero", "heroes"), ("logo", "logos")] {
                if missing().contains(&slot)
                    && let Some(image) = sgdb::images(key, game, kind)?.into_iter().next()
                {
                    landed(usize::from(steam::save(&image.url, &job.dir, slot)?));
                }
            }
            Ok(true)
        })());
    }
    // The Microsoft Store: a portrait cover and 16:9 key art, also for games Steam never sold.
    if missing().contains(&"icon") || missing().contains(&"hero") {
        soft((|| {
            let Some(id) = msstore::find(&job.title)? else {
                return Ok(false);
            };
            known = true;
            let art = msstore::art(&id)?;
            if missing().contains(&"icon")
                && let Some(cover) = art.covers.first()
            {
                landed(usize::from(steam::save(
                    &msstore::sized(cover, 600),
                    &job.dir,
                    "icon",
                )?));
            }
            if missing().contains(&"hero")
                && let Some(background) = art.backgrounds.first()
            {
                landed(usize::from(steam::save(background, &job.dir, "hero")?));
            }
            Ok(true)
        })());
    }
    let wiki = if missing().is_empty() {
        None
    } else {
        match pcgw::lookup(&job.title) {
            Ok(wiki) => wiki,
            Err(e) => {
                trouble.get_or_insert(e);
                None
            }
        }
    };
    if let Some(wiki) = wiki {
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
    if let Some(e) = trouble
        && !missing().is_empty()
    {
        return Err(e);
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
    pub sgdb_key: Option<String>,
    /// The folder the game's exe is in: repacks often ship a cover there.
    pub game_dir: Option<PathBuf>,
    /// The store trailer the game's music came from, shown as in use.
    pub music_in_use: Option<u64>,
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

    if let Some(dir) = &job.game_dir {
        for (slot, path) in folder_art(dir) {
            offer(
                notifier,
                job,
                slot,
                path,
                "From the game's folder".into(),
                false,
                None,
            );
        }
    }

    let mut apps: Vec<(u32, String)> = steam::search(&job.title)?
        .into_iter()
        .filter(|(_, name)| !steam::is_add_on(name))
        .collect();
    // The game itself before its sequels (stable: store relevance otherwise).
    apps.sort_by_key(|(_, name)| steam::adds_a_number(&job.title, name));
    apps.truncate(4);

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
        let store = {
            let notifier = notifier.clone();
            scope.spawn(move || choices_from_msstore(&notifier, job, cancel))
        };
        let grid = job.sgdb_key.as_ref().map(|key| {
            let notifier = notifier.clone();
            scope.spawn(move || choices_from_sgdb(&notifier, job, cancel, key))
        });
        let mut result = choices_from_wiki(notifier, job, cancel, &apps);
        for worker in std::iter::once(store).chain(grid) {
            if let Ok(Err(e)) = worker.join() {
                result = Err(e);
            }
        }
        for worker in workers {
            if let Ok(Err(e)) = worker.join() {
                result = Err(e);
            }
        }
        result
    })
}

/// Pictures repacks and game folders often ship (game.jpg, cover.png, folder.jpg…), with the
/// row each fits: backgrounds and logos by name, anything else as a cover.
fn folder_art(dir: &std::path::Path) -> Vec<(usize, PathBuf)> {
    const COVERS: &[&str] = &[
        "game", "cover", "folder", "poster", "boxart", "box", "front", "icon",
    ];
    const BACKGROUNDS: &[&str] = &["background", "fanart", "hero", "banner", "wallpaper"];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(usize, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter_map(|path| {
            let extension = path.extension()?.to_string_lossy().to_lowercase();
            if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
                return None;
            }
            let stem = path.file_stem()?.to_string_lossy().to_lowercase();
            let slot = if stem.contains("logo") {
                2
            } else if BACKGROUNDS.iter().any(|b| stem.contains(b)) {
                1
            } else if COVERS.contains(&stem.as_str()) {
                0
            } else {
                return None;
            };
            Some((slot, path))
        })
        .collect();
    found.sort();
    found
}

/// The Microsoft Store's best matches: posters and key art as covers, 16:9 key art and
/// screenshots as backgrounds.
fn choices_from_msstore(
    notifier: &Notifier,
    job: &ChoicesJob,
    cancel: &AtomicBool,
) -> Result<(), net::Error> {
    let mut products = msstore::search(&job.title)?;
    // The game itself before its sequels (stable: store relevance otherwise).
    products.sort_by_key(|p| steam::adds_a_number(&job.title, &p.title));
    std::thread::scope(|scope| {
        let workers: Vec<_> = products
            .into_iter()
            .take(5)
            .enumerate()
            .map(|(p, product)| {
                let notifier = notifier.clone();
                scope.spawn(move || msstore_product(&notifier, job, cancel, p, &product))
            })
            .collect();
        let mut result = Ok(());
        for worker in workers {
            if let Ok(Err(e)) = worker.join() {
                result = Err(e);
            }
        }
        result
    })
}

/// One Microsoft Store product's covers, backgrounds and screenshots.
fn msstore_product(
    notifier: &Notifier,
    job: &ChoicesJob,
    cancel: &AtomicBool,
    p: usize,
    product: &msstore::Product,
) -> Result<(), net::Error> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    {
        let art = msstore::art(&product.id)?;
        let name = &product.title;
        let picks = art
            .covers
            .iter()
            .map(|url| (0, url, 300, format!("Microsoft Store · {name}")))
            .chain(
                art.backgrounds
                    .iter()
                    .map(|url| (1, url, 640, format!("Microsoft Store · {name}"))),
            )
            .chain(
                art.screenshots
                    .iter()
                    .take(4)
                    .enumerate()
                    .map(|(i, url)| (1, url, 640, format!("Screenshot {} · {name}", i + 1))),
            );
        for (n, (slot, url, width, label)) in picks.enumerate() {
            if let Some(path) = fetch(
                job,
                cancel,
                &msstore::sized(url, width),
                &format!("ms{p}-{n}"),
            )? {
                offer(notifier, job, slot, path, label, false, Some(url.clone()));
            }
        }
    }
    Ok(())
}

/// SteamGridDB (with a key): many community covers, backgrounds and logos.
fn choices_from_sgdb(
    notifier: &Notifier,
    job: &ChoicesJob,
    cancel: &AtomicBool,
    key: &str,
) -> Result<(), net::Error> {
    let games = sgdb::search(key, &job.title)?;
    // The clearly matching game, else the most relevant result: the user picks what fits.
    let index =
        steam::best_title(&job.title, games.iter().map(|(_, name)| name.as_str())).unwrap_or(0);
    let Some((game, name)) = games.get(index) else {
        return Ok(());
    };
    for (slot, kind, limit) in [(0, "grids", 12), (2, "logos", 8), (1, "heroes", 8)] {
        for (i, image) in sgdb::images(key, *game, kind)?
            .into_iter()
            .take(limit)
            .enumerate()
        {
            if let Some(path) = fetch(job, cancel, &image.thumb, &format!("sg-{kind}-{i}"))? {
                let label = format!("SteamGridDB · {name}");
                offer(notifier, job, slot, path, label, false, Some(image.url));
            }
        }
    }
    Ok(())
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
    for (i, shot) in steam::screenshots(app)?.into_iter().take(6).enumerate() {
        if let Some(path) = fetch(job, cancel, &shot.thumbnail, &format!("{app}-shot{i}"))? {
            let label = format!("Screenshot {} · {name}", i + 1);
            offer(notifier, job, 1, path, label, false, Some(shot.full));
        }
    }
    // Music: the trailers, shown by their thumbnails; `full` is "<trailer id> <stream>".
    for (i, trailer) in steam::trailers(app)?.into_iter().take(4).enumerate() {
        if let Some(path) = fetch(
            job,
            cancel,
            &trailer.thumbnail,
            &format!("{app}-trailer{i}"),
        )? {
            let label = format!("{} · {name}", trailer.name);
            let in_use = job.music_in_use == Some(trailer.id);
            let full = Some(format!("{} {}", trailer.id, trailer.stream));
            offer(notifier, job, 3, path, label, in_use, full);
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

/// Checks whether greyed-out games' files are back. Off the UI thread: a disk that is dropping
/// out (as Yvan's external drive did) can take seconds to answer. A drive can also come back
/// without its letter ever disappearing, so watching the drive letters isn't enough.
/// Converts PS3 menu music one game at a time: (game id, where SND0.AT3 is, the cache file).
/// Without ffmpeg nothing can be converted, so the rest of the queue is dropped.
pub fn spawn_ps3_music(notifier: Notifier, jobs: Vec<(String, library::Snd0, PathBuf)>) {
    let spawned = std::thread::Builder::new()
        .name("ps3-music".into())
        .spawn(move || {
            for (id, source, out) in jobs {
                let result = match library::snd0::convert(&source, &out) {
                    Ok(()) => Ok(out),
                    Err(library::snd0::Failure::NoFfmpeg) => {
                        crate::diag::log("ffmpeg not found: PS3 games keep the ambient music");
                        return;
                    }
                    Err(library::snd0::Failure::Other(e)) => Err(e),
                };
                notifier.send(Event::Ps3Music { id, result });
            }
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("PS3 music conversion could not start: {e}"));
    }
}

pub fn spawn_update_check(notifier: Notifier) {
    let spawned = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || notifier.send(Event::LatestRelease(crate::update::latest())));
    if let Err(e) = spawned {
        crate::diag::log(&format!("update check could not start: {e}"));
    }
}

pub fn spawn_reach_check(notifier: Notifier, games: Vec<(String, Vec<PathBuf>)>) {
    let spawned = std::thread::Builder::new()
        .name("reach-check".into())
        .spawn(move || {
            let ids = games
                .into_iter()
                .filter(|(_, requires)| requires.iter().all(|path| path.exists()))
                .map(|(id, _)| id)
                .collect();
            notifier.send(Event::Reachable { ids });
        });
    if let Err(e) = spawned {
        crate::diag::log(&format!("reach check could not start: {e}"));
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
    /// Every source the Choose art screen uses, for an ambiguous Switch title:
    /// `cargo test live_picker_choices -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_picker_choices() {
        let (notifier, events) = Notifier::new(slint::Weak::default());
        let cache = std::env::temp_dir().join(format!("gl-picker-{}", std::process::id()));
        let job = ChoicesJob {
            game: "x".into(),
            title: "Demon Slayer - Kimetsu no Yaiba".into(),
            cache: cache.clone(),
            current: [None, None, None],
            scale: 1.0,
            sgdb_key: None,
            game_dir: None,
            music_in_use: None,
        };
        let started = std::time::Instant::now();
        let cancel = AtomicBool::new(false);
        let result = gather_choices(&notifier, &job, &cancel);
        println!(
            "gather: {:?} after {:?}",
            result.err().map(|e| e.to_string()),
            started.elapsed()
        );
        let mut counts = [0; 3];
        for event in events.try_iter() {
            if let Event::ArtChoice { slot, label, .. } = event {
                counts[slot] += 1;
                if counts[slot] <= 3 {
                    println!("  slot {slot}: {label}");
                }
            }
        }
        println!("choices per slot: {counts:?}");
        assert!(
            counts.iter().all(|&n| n > 0),
            "covers, backgrounds and logos"
        );
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn folder_pictures_go_to_the_row_they_fit() {
        let dir = std::env::temp_dir().join(format!("gl-folder-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "game.jpg",
            "Background.png",
            "logo_big.png",
            "readme.txt",
            "shot01.jpg",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let found: Vec<(usize, String)> = folder_art(&dir)
            .into_iter()
            .map(|(slot, p)| (slot, p.file_name().unwrap().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(
            found,
            [
                (0, "game.jpg".to_string()),
                (1, "Background.png".to_string()),
                (2, "logo_big.png".to_string())
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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
                sgdb_key: None,
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
