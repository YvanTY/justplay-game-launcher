// Release builds run without a console window; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod input;
mod nav;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{Color, ModelRc, SharedString, Timer, VecModel};

use input::InputEvent;
use nav::{Dir, Move};

slint::include_modules!();

const COLUMNS: usize = 5;
const TOAST_DURATION: Duration = Duration::from_millis(2600);

struct Tab {
    name: &'static str,
    tiles: Vec<Tile>,
    focus: usize,
}

struct State {
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
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let state = Rc::new(RefCell::new(State {
        tabs: demo_tabs(),
        current: 0,
        in_tabs: false,
        toast_timer: Timer::default(),
    }));

    ui.set_columns(COLUMNS as i32);
    let names: Vec<SharedString> = state.borrow().tabs.iter().map(|t| t.name.into()).collect();
    ui.set_tabs(ModelRc::new(VecModel::from(names)));
    let counts: Vec<i32> = state
        .borrow()
        .tabs
        .iter()
        .map(|t| t.tiles.len() as i32)
        .collect();
    ui.set_tab_counts(ModelRc::new(VecModel::from(counts)));
    load_tab(&ui, &state.borrow());

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
                let title = state
                    .tab()
                    .tiles
                    .get(state.tab().focus)
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                show_toast(
                    ui,
                    state,
                    format!("Launching {title} arrives in step 4").into(),
                );
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

/// Placeholder library until the real scanners land in step 2.
fn demo_tabs() -> Vec<Tab> {
    [
        ("Home", 23),
        ("PC", 12),
        ("SNES", 30),
        ("PS2", 7),
        ("Favorites", 4),
    ]
    .into_iter()
    .enumerate()
    .map(|(tab_index, (name, count))| Tab {
        name,
        focus: 0,
        tiles: (0..count)
            .map(|i| Tile {
                title: format!("{name} Game {}", i + 1).into(),
                subtitle: format!("{name} · demo tile").into(),
                color: hsv_color((tab_index * 67 + i * 23) as f32 % 360.0, 0.55, 0.62),
            })
            .collect(),
    })
    .collect()
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
