// Release builds run without a console window; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod input;
mod library;
mod nav;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{Color, Image, ModelRc, SharedString, Timer, VecModel};

use input::InputEvent;
use library::{Game, Library};
use nav::{Dir, Move};

slint::include_modules!();

const COLUMNS: usize = 5;
const TOAST_DURATION: Duration = Duration::from_millis(2600);

struct Tab {
    name: String,
    tiles: Vec<Tile>,
    /// Index into `State::games` for each tile.
    games: Vec<usize>,
    focus: usize,
}

struct State {
    games: Vec<Game>,
    tabs: Vec<Tab>,
    current: usize,
    in_tabs: bool,
    toast_timer: Timer,
}

impl State {
    fn tab(&self) -> &Tab {
        &self.tabs[self.current]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.current]
    }

    fn focused_game(&self) -> Option<&Game> {
        let tab = self.tab();
        tab.games.get(tab.focus).map(|&i| &self.games[i])
    }
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
    let tabs = build_tabs(&library.games, &mut problems);

    let state = Rc::new(RefCell::new(State {
        games: library.games,
        tabs,
        current: 0,
        in_tabs: false,
        toast_timer: Timer::default(),
    }));

    ui.set_columns(COLUMNS as i32);
    let names: Vec<SharedString> = state
        .borrow()
        .tabs
        .iter()
        .map(|t| t.name.as_str().into())
        .collect();
    ui.set_tabs(ModelRc::new(VecModel::from(names)));
    let counts: Vec<i32> = state
        .borrow()
        .tabs
        .iter()
        .map(|t| t.tiles.len() as i32)
        .collect();
    ui.set_tab_counts(ModelRc::new(VecModel::from(counts)));
    load_tab(&ui, &state.borrow());
    for problem in &problems {
        eprintln!("[library] {problem}");
    }
    if let Some(first) = problems.first() {
        let more = match problems.len() {
            1 => String::new(),
            n => format!(" (+{} more)", n - 1),
        };
        show_toast(&ui, &state.borrow(), format!("{first}{more}").into());
    }

    ui.on_action({
        let weak = ui.as_weak();
        let state = state.clone();
        move |action| handle_action(&weak.unwrap(), &mut state.borrow_mut(), action)
    });
    ui.on_notify({
        let weak = ui.as_weak();
        let state = state.clone();
        move |text| show_toast(&weak.unwrap(), &state.borrow(), text)
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

fn handle_action(ui: &AppWindow, state: &mut State, action: Action) {
    let dir = match action {
        Action::Up => Some(Dir::Up),
        Action::Down => Some(Dir::Down),
        Action::Left => Some(Dir::Left),
        Action::Right => Some(Dir::Right),
        _ => None,
    };

    if state.in_tabs {
        match (action, dir) {
            (_, Some(Dir::Left)) | (Action::TabPrev, _) => switch_tab(ui, state, -1),
            (_, Some(Dir::Right)) | (Action::TabNext, _) => switch_tab(ui, state, 1),
            (_, Some(Dir::Down)) | (Action::Confirm, _) | (Action::Back, _) => {
                state.in_tabs = false
            }
            (Action::Menu, _) => show_toast(ui, state, "The quick panel arrives in step 6".into()),
            _ => {}
        }
    } else if let Some(dir) = dir {
        let tab = state.tab();
        match nav::grid_move(tab.focus, tab.tiles.len(), COLUMNS, dir) {
            Move::To(index) => state.tab_mut().focus = index,
            Move::ExitTop => state.in_tabs = true,
            Move::Stay => {}
        }
    } else {
        match action {
            Action::TabPrev => switch_tab(ui, state, -1),
            Action::TabNext => switch_tab(ui, state, 1),
            Action::Back => state.in_tabs = true,
            Action::Confirm => {
                // Until launching lands in step 4, show the exact command it will run.
                if let Some(game) = state.focused_game() {
                    let exe = game
                        .launch
                        .exe
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy();
                    let text = format!("Step 4 will run: {exe} {}", game.launch.args.join(" "));
                    show_toast(ui, state, text.into());
                }
            }
            Action::Options => show_toast(ui, state, "The game sheet arrives in step 3".into()),
            Action::Menu => show_toast(ui, state, "The quick panel arrives in step 6".into()),
            _ => {}
        }
    }

    ui.set_in_tabs(state.in_tabs);
    ui.set_selected(state.tab().focus as i32);
}

fn switch_tab(ui: &AppWindow, state: &mut State, delta: isize) {
    state.current = nav::wrap_index(state.current, state.tabs.len(), delta);
    load_tab(ui, state);
}

fn load_tab(ui: &AppWindow, state: &State) {
    let tab = state.tab();
    ui.set_current_tab(state.current as i32);
    ui.set_tiles(ModelRc::new(VecModel::from(tab.tiles.clone())));
    ui.set_selected(tab.focus as i32);
}

fn show_toast(ui: &AppWindow, state: &State, text: SharedString) {
    ui.set_toast(text);
    ui.set_toast_visible(true);
    let weak = ui.as_weak();
    // Restarting the same timer means a new toast always gets its full display time.
    state
        .toast_timer
        .start(slint::TimerMode::SingleShot, TOAST_DURATION, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_toast_visible(false);
            }
        });
}

/// Home holds everything; then one tab per platform in a fixed, familiar order.
fn build_tabs(games: &[Game], problems: &mut Vec<String>) -> Vec<Tab> {
    let tiles: Vec<Tile> = games.iter().map(|g| make_tile(g, problems)).collect();
    let mut platforms: Vec<&str> = Vec::new();
    for game in games {
        if !platforms.contains(&game.platform.as_str()) {
            platforms.push(&game.platform);
        }
    }
    platforms.sort_by_key(|p| match *p {
        "PC" => 0,
        "PS3" => 1,
        _ => 2,
    });

    let all: Vec<usize> = (0..games.len()).collect();
    let mut tabs = vec![Tab {
        name: "Home".into(),
        tiles: tiles.clone(),
        games: all,
        focus: 0,
    }];
    for platform in platforms {
        let indices: Vec<usize> = (0..games.len())
            .filter(|&i| games[i].platform == platform)
            .collect();
        let platform_tiles = indices.iter().map(|&i| tiles[i].clone()).collect();
        tabs.push(Tab {
            name: platform.into(),
            tiles: platform_tiles,
            games: indices,
            focus: 0,
        });
    }
    tabs
}

fn make_tile(game: &Game, problems: &mut Vec<String>) -> Tile {
    let icon = game
        .art
        .icon
        .as_ref()
        .and_then(|path| match Image::load_from_path(path) {
            Ok(image) => Some(image),
            Err(_) => {
                problems.push(format!("{}: could not load {}", game.title, path.display()));
                None
            }
        });
    Tile {
        title: game.title.as_str().into(),
        subtitle: game.platform.as_str().into(),
        color: title_color(&game.title),
        has_icon: icon.is_some(),
        icon: icon.unwrap_or_default(),
    }
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
