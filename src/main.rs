// Release builds run without a console window; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod home;
mod input;
mod library;
mod nav;
mod state;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slint::{Color, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

use home::{Effect, Home, SheetItem};
use input::InputEvent;
use library::{Game, Library};
use state::State;

slint::include_modules!();

const TOAST_DURATION: Duration = Duration::from_millis(2600);
/// Full-screen art loads only once the selection rests, so scrolling never stutters on decoding.
const HERO_DELAY: Duration = Duration::from_millis(140);
const HERO_CACHE: usize = 6;

struct App {
    home: Home,
    images: ImageCache,
    state_path: PathBuf,
    shown_hero: Option<PathBuf>,
    toasts: Rc<Toasts>,
    hero_timer: Timer,
    clock_timer: Timer,
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let data_dir = config::data_dir();
    let mut problems = Vec::new();
    let library = match config::load(&data_dir) {
        Ok(config) => library::load(&config, &data_dir.join("art")),
        Err(e) => {
            problems.push(e);
            Library {
                games: Vec::new(),
                warnings: Vec::new(),
            }
        }
    };
    problems.extend(library.warnings.iter().cloned());
    if library.games.is_empty() && problems.is_empty() {
        problems.push(format!(
            "No games yet: add them in {}",
            data_dir.join("config.toml").display()
        ));
    }
    let state_path = data_dir.join("state.json");
    let (state, state_problem) = State::load(&state_path);
    problems.extend(state_problem);

    let app = Rc::new(RefCell::new(App {
        home: Home::new(library.games, state),
        images: ImageCache::default(),
        state_path,
        shown_hero: None,
        toasts: Rc::new(Toasts::default()),
        hero_timer: Timer::default(),
        clock_timer: Timer::default(),
    }));

    {
        let mut app = app.borrow_mut();
        push_tabs(&ui, &mut app);
        push_selection(&ui, &mut app);
        push_scalars(&ui, &app.home);
        show_hero(&ui, &mut app);
        problems.append(&mut app.images.problems);
        for problem in &problems {
            eprintln!("[launcher] {problem}");
        }
        if let Some(first) = problems.first() {
            let more = if problems.len() > 1 {
                format!(" (+{} more)", problems.len() - 1)
            } else {
                String::new()
            };
            app.toasts.show(&ui, format!("{first}{more}"));
        }
    }
    start_clock(&ui, &app);

    ui.on_action({
        let weak = ui.as_weak();
        let app = app.clone();
        move |action| {
            let ui = weak.unwrap();
            if action == Action::Menu {
                let toasts = app.borrow().toasts.clone();
                toasts.show(&ui, "The quick panel arrives in step 6".into());
                return;
            }
            let effects = app.borrow_mut().home.handle(action);
            apply_effects(&ui, &app, &effects);
        }
    });
    ui.on_notify({
        let weak = ui.as_weak();
        let app = app.clone();
        move |text| {
            let toasts = app.borrow().toasts.clone();
            toasts.show(&weak.unwrap(), text.into());
        }
    });

    let weak = ui.as_weak();
    input::spawn(move |event| {
        let weak = weak.clone();
        // Only fails once the event loop has shut down, when there is nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match event {
                InputEvent::Action(action) => ui.invoke_action(action),
                InputEvent::Notice(text) => ui.invoke_notify(text.into()),
            }
        });
    });

    ui.window().set_fullscreen(true);
    ui.run()
}

fn apply_effects(ui: &AppWindow, app_rc: &Rc<RefCell<App>>, effects: &[Effect]) {
    let mut app = app_rc.borrow_mut();
    for effect in effects {
        match effect {
            Effect::Tabs => push_tabs(ui, &mut app),
            Effect::Selection => push_selection(ui, &mut app),
            Effect::Launch(game) => {
                // Until launching lands in step 4, show the exact command it will run.
                let launch = &app.home.games[*game].launch;
                let exe = launch.exe.file_name().unwrap_or_default().to_string_lossy();
                let text = format!("Step 4 will run: {exe} {}", launch.args.join(" "));
                app.toasts.show(ui, text);
            }
            Effect::SaveState => {
                if let Err(e) = app.home.state.save(&app.state_path) {
                    app.toasts.show(ui, e);
                }
            }
        }
    }
    push_scalars(ui, &app.home);
    for problem in app.images.problems.drain(..) {
        eprintln!("[launcher] {problem}");
    }
    drop(app);

    if effects.contains(&Effect::Selection) {
        let weak = ui.as_weak();
        let app = app_rc.clone();
        app_rc
            .borrow()
            .hero_timer
            .start(TimerMode::SingleShot, HERO_DELAY, move || {
                if let Some(ui) = weak.upgrade() {
                    show_hero(&ui, &mut app.borrow_mut());
                }
            });
    }
}

/// Tab bar, the current tab's game row and the recently played cards.
fn push_tabs(ui: &AppWindow, app: &mut App) {
    let App { home, images, .. } = app;
    let tabs: Vec<TabInfo> = home
        .tabs
        .iter()
        .map(|t| TabInfo {
            name: t.name.as_str().into(),
            count: t.games.len() as i32,
        })
        .collect();
    ui.set_tabs(ModelRc::new(VecModel::from(tabs)));
    ui.set_current_tab(home.current as i32);
    let icons: Vec<Tile> = home
        .tab()
        .games
        .iter()
        .map(|&i| make_tile(&home.games[i], images))
        .collect();
    ui.set_icons(ModelRc::new(VecModel::from(icons)));
    let recent: Vec<Tile> = home
        .recent
        .iter()
        .map(|&i| make_tile(&home.games[i], images))
        .collect();
    ui.set_recent(ModelRc::new(VecModel::from(recent)));
}

/// Title, logo and info for the selected game (the background follows after HERO_DELAY).
fn push_selection(ui: &AppWindow, app: &mut App) {
    let App { home, images, .. } = app;
    let Some(index) = home.selected_game() else {
        ui.set_game_title(SharedString::new());
        ui.set_info(SharedString::new());
        ui.set_has_logo(false);
        return;
    };
    let game = &home.games[index];
    ui.set_game_title(game.title.as_str().into());
    let played = home
        .state
        .last_played
        .get(&game.id)
        .map(|&t| played_ago(unix_now().saturating_sub(t)));
    let info = match played {
        Some(played) => format!("{} · {played}", game.platform),
        None => game.platform.clone(),
    };
    ui.set_info(info.into());
    ui.set_is_favorite(home.is_favorite(index));
    ui.set_backdrop(title_color(&game.title));
    let logo = game.art.logo.as_deref().and_then(|p| images.icon(p));
    ui.set_has_logo(logo.is_some());
    ui.set_logo(logo.unwrap_or_default());
}

/// Cheap per-keypress values: focus positions, zone and the game sheet.
fn push_scalars(ui: &AppWindow, home: &Home) {
    ui.set_zone(home.zone);
    ui.set_selected(home.tab().focus as i32);
    ui.set_action_index(home.action as i32);
    ui.set_recent_index(home.recent_focus as i32);
    ui.set_sheet_open(home.sheet.is_some());
    if let Some(sheet) = &home.sheet {
        let items: Vec<SharedString> = home
            .sheet_items()
            .iter()
            .map(|item| match item {
                SheetItem::Play => "Play".into(),
                SheetItem::ToggleFavorite if home.is_favorite(sheet.game) => {
                    "Remove from Favorites".into()
                }
                SheetItem::ToggleFavorite => "Add to Favorites".into(),
            })
            .collect();
        ui.set_sheet_items(ModelRc::new(VecModel::from(items)));
        ui.set_sheet_index(sheet.focus as i32);
    }
}

/// Crossfades to the selected game's background: its hero art, else its icon, else a color.
fn show_hero(ui: &AppWindow, app: &mut App) {
    let App {
        home,
        images,
        shown_hero,
        ..
    } = app;
    let path = home
        .selected_game()
        .map(|i| &home.games[i].art)
        .and_then(|art| art.hero.clone().or_else(|| art.icon.clone()));
    if path == *shown_hero {
        return;
    }
    match path.as_deref().and_then(|p| images.hero(p)) {
        Some(image) => {
            if ui.get_show_a() {
                ui.set_hero_b(image);
                ui.set_show_a(false);
            } else {
                ui.set_hero_a(image);
                ui.set_show_a(true);
            }
            ui.set_has_hero(true);
        }
        None => ui.set_has_hero(false),
    }
    *shown_hero = path;
}

fn make_tile(game: &Game, images: &mut ImageCache) -> Tile {
    let icon = game.art.icon.as_deref().and_then(|p| images.icon(p));
    Tile {
        title: game.title.as_str().into(),
        color: title_color(&game.title),
        has_icon: icon.is_some(),
        icon: icon.unwrap_or_default(),
    }
}

/// Decoded images, so moving back and forth never decodes the same file twice.
#[derive(Default)]
struct ImageCache {
    /// Small art (icons, logos), kept for the whole session. `None` remembers a failed load.
    icons: HashMap<PathBuf, Option<Image>>,
    /// Full-screen art is large (8 MB decoded each), so only the most recent few are kept.
    heroes: VecDeque<(PathBuf, Image)>,
    problems: Vec<String>,
}

impl ImageCache {
    fn icon(&mut self, path: &Path) -> Option<Image> {
        if let Some(cached) = self.icons.get(path) {
            return cached.clone();
        }
        let image = self.load(path);
        self.icons.insert(path.to_path_buf(), image.clone());
        image
    }

    fn hero(&mut self, path: &Path) -> Option<Image> {
        if let Some(pos) = self.heroes.iter().position(|(p, _)| p == path) {
            let entry = self.heroes.remove(pos)?;
            self.heroes.push_back(entry.clone());
            return Some(entry.1);
        }
        let image = self.load(path)?;
        if self.heroes.len() == HERO_CACHE {
            self.heroes.pop_front();
        }
        self.heroes.push_back((path.to_path_buf(), image.clone()));
        Some(image)
    }

    fn load(&mut self, path: &Path) -> Option<Image> {
        match Image::load_from_path(path) {
            Ok(image) => Some(image),
            Err(_) => {
                self.problems
                    .push(format!("Could not load image {}", path.display()));
                None
            }
        }
    }
}

/// Messages shown one after another, so a startup warning isn't replaced a moment later
/// by "controller connected".
#[derive(Default)]
struct Toasts {
    queue: RefCell<VecDeque<String>>,
    last: RefCell<String>,
    timer: Timer,
}

impl Toasts {
    const MAX_QUEUED: usize = 5;

    fn show(self: &Rc<Self>, ui: &AppWindow, text: String) {
        let showing = self.timer.running();
        // Hot-plug and repeated presses produce identical messages; one is enough.
        if (showing && *self.last.borrow() == text) || self.queue.borrow().contains(&text) {
            return;
        }
        if self.queue.borrow().len() < Self::MAX_QUEUED {
            self.queue.borrow_mut().push_back(text);
        }
        if !showing {
            self.next(ui);
        }
    }

    fn next(self: &Rc<Self>, ui: &AppWindow) {
        let Some(text) = self.queue.borrow_mut().pop_front() else {
            ui.set_toast_visible(false);
            self.last.borrow_mut().clear();
            return;
        };
        ui.set_toast(text.as_str().into());
        ui.set_toast_visible(true);
        *self.last.borrow_mut() = text;
        let weak_ui = ui.as_weak();
        let weak_self = Rc::downgrade(self);
        self.timer
            .start(TimerMode::SingleShot, TOAST_DURATION, move || {
                if let (Some(ui), Some(toasts)) = (weak_ui.upgrade(), weak_self.upgrade()) {
                    toasts.next(&ui);
                }
            });
    }
}

/// Updates the clock right after each minute boundary, so it wakes the app once a minute.
fn start_clock(ui: &AppWindow, app: &Rc<RefCell<App>>) {
    ui.set_clock(local_time().into());
    let until_next_minute = Duration::from_millis(60_200 - (unix_millis() % 60_000));
    let weak = ui.as_weak();
    let app_rc = app.clone();
    app.borrow()
        .clock_timer
        .start(TimerMode::SingleShot, until_next_minute, move || {
            if let Some(ui) = weak.upgrade() {
                start_clock(&ui, &app_rc);
            }
        });
}

/// Current time in the user's Windows time format (12 h / 24 h as configured).
fn local_time() -> String {
    use windows_sys::Win32::Globalization::{GetTimeFormatEx, TIME_NOSECONDS};
    let mut buffer = [0u16; 64];
    let len = unsafe {
        GetTimeFormatEx(
            std::ptr::null(),
            TIME_NOSECONDS,
            std::ptr::null(),
            std::ptr::null(),
            buffer.as_mut_ptr(),
            buffer.len() as i32,
        )
    };
    // The returned length includes the terminating NUL; 0 means the call failed.
    if len <= 1 {
        String::new()
    } else {
        String::from_utf16_lossy(&buffer[..len as usize - 1])
    }
}

fn played_ago(seconds: u64) -> String {
    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;
    match seconds {
        s if s < 60 => "Played just now".into(),
        s if s < HOUR => format!("Played {} min ago", s / 60),
        s if s < DAY => format!("Played {} h ago", s / HOUR),
        s if s < 2 * DAY => "Played yesterday".into(),
        s => format!("Played {} days ago", s / DAY),
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn unix_now() -> u64 {
    unix_millis() / 1000
}

/// Stable per-title color for games without art, so each one stays recognisable.
fn title_color(title: &str) -> Color {
    let hash = title
        .bytes()
        .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    hsv_color((hash % 360) as f32, 0.5, 0.55)
}

fn hsv_color(hue: f32, saturation: f32, value: f32) -> Color {
    let chroma = value * saturation;
    let h = hue / 60.0;
    let x = chroma * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = value - chroma;
    let to_u8 = |c: f32| ((c + m) * 255.0).round() as u8;
    Color::from_rgb_u8(to_u8(r), to_u8(g), to_u8(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn played_ago_reads_naturally() {
        assert_eq!(played_ago(10), "Played just now");
        assert_eq!(played_ago(5 * 60), "Played 5 min ago");
        assert_eq!(played_ago(3 * 3600), "Played 3 h ago");
        assert_eq!(played_ago(30 * 3600), "Played yesterday");
        assert_eq!(played_ago(5 * 86400), "Played 5 days ago");
    }

    #[test]
    fn clock_text_is_not_empty() {
        assert!(!local_time().is_empty());
    }
}
