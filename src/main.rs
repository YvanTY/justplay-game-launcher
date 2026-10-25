// Release builds run without a console window; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod add_games;
mod art_picker;
mod audio;
mod background;
mod config;
mod controllers;
mod diag;
mod display;
mod emulation;
mod home;
mod input;
mod launcher;
mod library;
mod menus;
mod msstore;
mod music;
mod nav;
mod net;
mod pcgw;
mod scaled_art;
mod sgdb;
mod state;
mod steam;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use controllers::XButton;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slint::{Color, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

use audio::{Audio, Sound};
use background::{ArtJob, ArtOutcome};
use emulation::Translate;
use home::{Effect, Home, NavState, SheetItem};
use input::{InputEvent, PadInfo};
use launcher::{Ended, Running};
use library::{Game, Library, Missing};
use menus::{MenuEffect, Menus, Overlay, Setting};
use music::Music;
use state::{ControllerMode, State};

slint::include_modules!();

const TOAST_DURATION: Duration = Duration::from_millis(2600);
/// How long input is ignored after the launcher appears.
const INPUT_GRACE: Duration = Duration::from_millis(1000);
/// Full-screen art loads only once the selection rests, so scrolling never stutters on decoding.
const HERO_DELAY: Duration = Duration::from_millis(140);
/// Current and previous background: stepping back is instant, and each extra one would cost
/// 5-8 MB for the whole session. Others decode again (a few ms) behind HERO_DELAY.
const HERO_CACHE: usize = 2;
/// How often plugged/unplugged drives are noticed while the launcher is visible (one cheap call).
const DRIVE_POLL: Duration = Duration::from_secs(2);
/// When a failed art lookup (no connection) is tried again during the session.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];
/// The mouse pointer hides again after this long without moving.
const POINTER_IDLE: Duration = Duration::from_secs(3);

struct App {
    home: Home,
    images: ImageCache,
    state_path: PathBuf,
    shown_hero: Option<PathBuf>,
    toasts: Rc<Toasts>,
    hero_timer: Timer,
    clock_timer: Timer,
    running: Option<Running>,
    /// True while the launcher is hidden behind a running game.
    in_background: bool,
    menus: Menus,
    /// Controllers screen: connected pads, which one is shown, the selected button, and
    /// whether it waits for a key/button to add.
    pads: Vec<PadInfo>,
    pad_index: usize,
    remap_selected: usize,
    capturing: bool,
    controller_events: std::sync::mpsc::Receiver<InputEvent>,
    /// Hold-B-to-exit on the Controllers screen: when B went down, and the ring's tick timer.
    back_hold_since: Option<std::time::Instant>,
    back_hold_timer: Timer,
    /// Input is ignored until this moment: right after the window appears (startup or back from
    /// a game) a stale press, or A still held as a game exits, must not start a game.
    input_ready_at: std::time::Instant,
    /// None when the PC has no usable audio output; the launcher works silently then.
    audio: Option<Rc<Audio>>,
    music: Option<Rc<Music>>,
    audio_timer: Timer,
    /// Drive letter -> name, including unplugged drives, for "Connect drive E: (name)".
    drive_labels: BTreeMap<String, String>,
    /// Connected drives when the library was last loaded; a change reloads it.
    drive_mask: u32,
    drive_timer: Timer,
    /// Results from the worker threads (backgrounds, art), drained on the UI thread.
    background: std::sync::mpsc::Receiver<background::Event>,
    hero_loader: std::sync::mpsc::Sender<(PathBuf, f32)>,
    art_fetcher: std::sync::mpsc::Sender<ArtJob>,
    /// Game ids whose Steam lookup is queued or running.
    art_pending: HashSet<String>,
    /// Lookups that failed (offline): how many times this session, and when to try again.
    art_failed: HashMap<String, (u32, std::time::Instant)>,
    /// SteamGridDB key from config.toml (None: that source is skipped).
    sgdb_key: Option<String>,
    notifier: background::Notifier,
    /// The Add games screen, and the switch that stops its drive scan.
    add_games: add_games::AddGames,
    scan_cancel: Arc<AtomicBool>,
    /// The Choose art screen while open, its thumbnails (same order as its choices), and the
    /// switch that stops its downloads.
    art_picker: Option<art_picker::ArtPicker>,
    art_thumbs: [Vec<Image>; 3],
    art_cancel: Arc<AtomicBool>,
    /// (game id, slot) -> (token, art folder) of a picked preview whose sharp version is on
    /// its way; a later pick for the same slot replaces the token, so a late arrival is ignored.
    art_upgrades: HashMap<(String, usize), (u64, PathBuf)>,
    art_upgrade_token: u64,
    /// Counts openings of the Choose art screen (each gets its own session key).
    art_sessions: u64,
    /// The last thing clicked: a second click on the same thing activates it.
    last_click: Option<(Target, usize, usize)>,
    /// The click being handled hit the same thing as the one before.
    repeated_click: bool,
    /// Hides the mouse pointer once it stops moving.
    pointer_timer: Timer,
    /// A check of greyed-out games' files is running (see check_unavailable).
    reach_check: bool,
    /// Games whose files are all there yet still greyed out after a reload: not checked again,
    /// so they can't reload the library every tick.
    reach_stuck: HashSet<String>,
    /// The action being handled came from a pad (not the keyboard, the mouse or a hint).
    pad_input: bool,
    /// Controllers screen: when each held pad button (index in controllers::ALL) went down,
    /// the holds whose action already ran (until released), and the timer that times them.
    held_since: HashMap<usize, std::time::Instant>,
    holds_done: HashSet<usize>,
    hold_timer: Timer,
    /// Controllers screen: how the pad is read, the remap in progress (one button, or all),
    /// and the last Live masks (pad inputs held, buttons the game sees).
    controllers_mode: controllers::Mode,
    remap_run: Option<controllers::RemapRun>,
    live_raw: u32,
    live_out: u32,
    /// While a game runs: which screen its window was last seen on (logged when it changes).
    game_screen_timer: Timer,
    game_screen_seen: Option<String>,
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let data_dir = config::data_dir();
    // Created here so even config errors are logged; the folder may not exist on first run.
    let _ = std::fs::create_dir_all(&data_dir);
    // Pictures downloaded for the Choose art screen are only needed while it is open.
    let _ = std::fs::remove_dir_all(art_choices_dir(&data_dir));
    diag::init(&data_dir);
    let mut problems = Vec::new();
    let state_path = data_dir.join("state.json");
    let (mut state, state_problem) = State::load(&state_path);
    // 2: Steam editions and subtitles, then PCGamingWiki for games Steam no longer sells.
    // 3: the Microsoft Store and SteamGridDB, and plain-word searches.
    // Saved at once only if state.json was read fine: a file that was locked or unreadable
    // must not be replaced by an empty state before the user has done anything.
    if state.retry_art_misses(3)
        && state_problem.is_none()
        && let Err(e) = state.save(&state_path)
    {
        problems.push(e);
    }
    problems.extend(state_problem);
    let drive_mask = library::drives::mask();
    let mut sgdb_key = None;
    let library = match config::load(&data_dir) {
        Ok(config) => {
            sgdb_key = config
                .art
                .steamgriddb_key
                .clone()
                .filter(|k| !k.trim().is_empty());
            library::load(&config, &state.added_games, &data_dir)
        }
        Err(e) => {
            problems.push(e);
            Library {
                games: Vec::new(),
                warnings: Vec::new(),
                drive_labels: BTreeMap::new(),
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
    let audio = match Audio::start() {
        Ok(audio) => Some(Rc::new(audio)),
        Err(e) => {
            problems.push(format!("Sound is off: {e}"));
            None
        }
    };
    let music = audio
        .as_ref()
        .map(|audio| Rc::new(Music::start(audio.device())));
    let (controller_sender, controller_events) = std::sync::mpsc::channel::<InputEvent>();
    let (notifier, background_events) = background::Notifier::new(ui.as_weak());
    let app = Rc::new(RefCell::new(App {
        home: Home::new(library.games, state),
        images: ImageCache::default(),
        state_path,
        shown_hero: None,
        toasts: Rc::new(Toasts::default()),
        hero_timer: Timer::default(),
        clock_timer: Timer::default(),
        running: None,
        in_background: false,
        menus: Menus::new(),
        pads: Vec::new(),
        pad_index: 0,
        remap_selected: 0,
        capturing: false,
        controller_events,
        back_hold_since: None,
        back_hold_timer: Timer::default(),
        input_ready_at: std::time::Instant::now() + INPUT_GRACE,
        audio,
        music,
        audio_timer: Timer::default(),
        drive_labels: library.drive_labels,
        drive_mask,
        drive_timer: Timer::default(),
        background: background_events,
        hero_loader: background::spawn_hero_loader(notifier.clone()),
        art_fetcher: background::spawn_art_fetcher(notifier.clone()),
        art_pending: HashSet::new(),
        art_failed: HashMap::new(),
        sgdb_key,
        notifier,
        add_games: add_games::AddGames::default(),
        scan_cancel: Arc::new(AtomicBool::new(false)),
        art_picker: None,
        art_thumbs: Default::default(),
        art_cancel: Arc::new(AtomicBool::new(false)),
        art_upgrades: HashMap::new(),
        art_upgrade_token: 0,
        art_sessions: 0,
        last_click: None,
        repeated_click: false,
        pointer_timer: Timer::default(),
        reach_check: false,
        reach_stuck: HashSet::new(),
        pad_input: false,
        held_since: HashMap::new(),
        holds_done: HashSet::new(),
        hold_timer: Timer::default(),
        controllers_mode: controllers::Mode::Test,
        remap_run: None,
        live_raw: 0,
        live_out: 0,
        game_screen_timer: Timer::default(),
        game_screen_seen: None,
    }));

    {
        let mut app = app.borrow_mut();
        apply_volumes(&app);
        if let Ok(mut remaps) = controllers::REMAPS.write() {
            *remaps = app.home.state.controller_remaps.clone();
        }
        push_menus(&ui, &app);
        push_tabs(&ui, &mut app);
        push_selection(&ui, &mut app);
        push_scalars(&ui, &app.home);
        show_hero(&ui, &mut app);
        update_music(&app);
        queue_art_lookups(&mut app);
        problems.append(&mut app.images.problems);
        for problem in &problems {
            diag::log(problem);
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
    start_drive_watch(&ui, &app);

    ui.on_action({
        let weak = ui.as_weak();
        let app = app.clone();
        move |action| {
            let ui = weak.unwrap();
            // A pad or the keyboard is in use: the mouse pointer gets out of the way.
            if ui.global::<Mouse>().get_visible() {
                ui.global::<Mouse>().set_visible(false);
            }
            handle_action(&ui, &app, action);
        }
    });
    ui.on_pad_action({
        let weak = ui.as_weak();
        let app = app.clone();
        move |action| {
            let ui = weak.unwrap();
            if ui.global::<Mouse>().get_visible() {
                ui.global::<Mouse>().set_visible(false);
            }
            app.borrow_mut().pad_input = true;
            handle_action(&ui, &app, action);
            app.borrow_mut().pad_input = false;
        }
    });
    ui.on_hold_tick({
        let weak = ui.as_weak();
        let app = app.clone();
        move || hold_tick(&weak.unwrap(), &mut app.borrow_mut())
    });
    ui.on_game_screen_check({
        let app = app.clone();
        move || game_screen_check(&mut app.borrow_mut())
    });
    let mouse = ui.global::<Mouse>();
    mouse.on_act({
        let weak = ui.as_weak();
        let app = app.clone();
        move |action| handle_action(&weak.unwrap(), &app, action)
    });
    mouse.on_clicked({
        let weak = ui.as_weak();
        let app = app.clone();
        move |target, a, b| handle_click(&weak.unwrap(), &app, target, a, b)
    });
    mouse.on_back({
        let weak = ui.as_weak();
        let app = app.clone();
        move || mouse_back(&weak.unwrap(), &app)
    });
    mouse.on_wheel({
        let weak = ui.as_weak();
        let app = app.clone();
        move |delta| mouse_wheel(&weak.unwrap(), &app, delta)
    });
    mouse.on_moved({
        let weak = ui.as_weak();
        let app = app.clone();
        move || show_pointer(&weak.unwrap(), &app.borrow())
    });
    ui.on_notify({
        let weak = ui.as_weak();
        let app = app.clone();
        move |text| {
            let toasts = app.borrow().toasts.clone();
            toasts.show(&weak.unwrap(), text.into());
        }
    });
    ui.on_home_combo({
        let weak = ui.as_weak();
        let app = app.clone();
        move || home_combo(&weak.unwrap(), &mut app.borrow_mut())
    });
    ui.on_game_ended({
        let weak = ui.as_weak();
        let app = app.clone();
        move |message| game_ended(&weak.unwrap(), &mut app.borrow_mut(), &message)
    });

    let weak = ui.as_weak();
    ui.on_back_hold_tick({
        let weak = ui.as_weak();
        let app = app.clone();
        move || back_hold_tick(&weak.unwrap(), &mut app.borrow_mut())
    });
    ui.on_back_released({
        let weak = ui.as_weak();
        let app = app.clone();
        move || cancel_back_hold(&weak.unwrap(), &mut app.borrow_mut())
    });
    ui.on_background_event({
        let weak = ui.as_weak();
        let app = app.clone();
        move || drain_background(&weak.unwrap(), &mut app.borrow_mut())
    });
    ui.on_controller_event({
        let weak = ui.as_weak();
        let app = app.clone();
        move || drain_controller_events(&weak.unwrap(), &mut app.borrow_mut())
    });

    input::spawn(move |event| {
        let weak = weak.clone();
        // Controllers-screen events carry data Slint callbacks can't, so they queue in a channel
        // and the callback only says "something arrived".
        let event = match event {
            InputEvent::Pads(_)
            | InputEvent::Live { .. }
            | InputEvent::Captured(_)
            | InputEvent::CaptureTimedOut => {
                let _ = controller_sender.send(event);
                None
            }
            other => Some(other),
        };
        // Only fails once the event loop has shut down, when there is nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match event {
                Some(InputEvent::Action(action)) => ui.invoke_pad_action(action),
                Some(InputEvent::Notice(text)) => ui.invoke_notify(text.into()),
                Some(InputEvent::HomeCombo) => ui.invoke_home_combo(),
                Some(InputEvent::BackReleased) => ui.invoke_back_released(),
                _ => ui.invoke_controller_event(),
            }
        });
    });

    ui.window().on_close_requested(|| {
        // Hiding the window for a game must not end the app, so only an explicit close quits.
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
    ui.window().set_fullscreen(true);
    ui.show()?;
    // Once the window exists: undo a layout left by a launcher that didn't close normally, then
    // put the launcher on its screen.
    slint::Timer::single_shot(Duration::ZERO, {
        let weak = ui.as_weak();
        let app = app.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                let mut app = app.borrow_mut();
                restore_screens(&mut app);
                show_launcher_on_its_screen(&ui, &mut app);
            }
        }
    });
    let result = slint::run_event_loop_until_quit();
    restore_screens(&mut app.borrow_mut());
    result
}

/// While a game has the screen, or in the second after the launcher appears, input is ignored
/// (pads are only listened to for the home combo; a stale press must not start a game).
fn input_paused(app: &App) -> bool {
    app.in_background || std::time::Instant::now() < app.input_ready_at
}

/// One controller, keyboard or clicked-hint action.
fn handle_action(ui: &AppWindow, app: &Rc<RefCell<App>>, action: Action) {
    if input_paused(&app.borrow()) {
        diag::log(&format!("ignored {action:?} while input is paused"));
        return;
    }
    // A pad or keyboard press in between means the next click starts over.
    app.borrow_mut().last_click = None;
    if action == Action::Menu || app.borrow().menus.is_open() {
        handle_menu_action(ui, &mut app.borrow_mut(), action);
        return;
    }
    let before = app.borrow().home.nav_state();
    let effects = app.borrow_mut().home.handle(action);
    let after = app.borrow().home.nav_state();
    if let Some(sound) = sound_for(action, before, after, &effects) {
        play_sound(&app.borrow(), sound);
    }
    apply_effects(ui, app, &effects);
}

/// What a click leads to once the clicked thing is selected.
enum Then {
    Nothing,
    /// Run this action as if pressed (Confirm activates what is now selected).
    Action(Action),
    Effects(Vec<Effect>),
}

/// A left click: the first click on something selects it, a click on what is already selected
/// activates it, so a stray click never starts a game. Buttons (Play, menu items) act at once.
fn handle_click(ui: &AppWindow, app_rc: &Rc<RefCell<App>>, target: Target, a: i32, b: i32) {
    let then = {
        let mut app = app_rc.borrow_mut();
        if input_paused(&app) {
            return;
        }
        let this = (target, a.max(0) as usize, b.max(0) as usize);
        let repeated = app.last_click == Some(this);
        app.last_click = Some(this);
        app.repeated_click = repeated;
        click(
            ui,
            &mut app,
            target,
            a.max(0) as usize,
            b.max(0) as usize,
            b,
        )
    };
    match then {
        Then::Nothing => {}
        Then::Action(action) => handle_action(ui, app_rc, action),
        Then::Effects(effects) => apply_effects(ui, app_rc, &effects),
    }
}

fn click(ui: &AppWindow, app: &mut App, target: Target, a: usize, b: usize, step: i32) -> Then {
    let moved = |app: &App| play_sound(app, Sound::Move);
    match target {
        Target::None => Then::Nothing,
        // --- Home screen (only reachable while no menu covers it) ---
        Target::Tab => {
            let effects = app.home.select_tab(a);
            if !effects.is_empty() {
                play_sound(app, Sound::Tab);
            }
            Then::Effects(effects)
        }
        Target::Gear => {
            app.home.zone = Zone::Tabs;
            app.home.gear = true;
            Then::Action(Action::Confirm)
        }
        Target::Tile => {
            let home = &mut app.home;
            if a >= home.tab().games.len() {
                return Then::Nothing;
            }
            let repeated = app.repeated_click;
            let home = &mut app.home;
            if repeated && home.zone == Zone::Icons && home.tab().focus == a && home.sheet.is_none()
            {
                return Then::Action(Action::Confirm);
            }
            home.sheet = None;
            home.gear = false;
            home.zone = Zone::Icons;
            let current = home.current;
            home.tabs[current].focus = a;
            moved(app);
            Then::Effects(vec![Effect::Selection])
        }
        Target::Play | Target::More if app.home.tab().games.is_empty() => Then::Nothing,
        Target::Play | Target::More => {
            app.home.sheet = None;
            app.home.zone = Zone::Actions;
            app.home.action = usize::from(target == Target::More);
            Then::Action(Action::Confirm)
        }
        Target::Recent => {
            let home = &mut app.home;
            if a >= home.recent.len() {
                return Then::Nothing;
            }
            let repeated = app.repeated_click;
            let home = &mut app.home;
            if repeated
                && home.zone == Zone::Recent
                && home.recent_focus == a
                && home.sheet.is_none()
            {
                return Then::Action(Action::Confirm);
            }
            home.sheet = None;
            home.zone = Zone::Recent;
            home.recent_focus = a;
            moved(app);
            Then::Effects(Vec::new())
        }
        Target::SheetItem => {
            let count = app.home.sheet_items().len();
            match app.home.sheet.as_mut() {
                Some(sheet) if a < count => {
                    sheet.focus = a;
                    Then::Action(Action::Confirm)
                }
                _ => Then::Nothing,
            }
        }
        Target::SheetOutside => {
            app.home.sheet = None;
            play_sound(app, Sound::Back);
            Then::Effects(Vec::new())
        }
        // --- Quick panel, Power, Settings ---
        Target::Dismiss => Then::Action(Action::Back),
        Target::PanelTile => {
            app.menus.panel_focus = a.min(menus::PANEL.len() - 1);
            Then::Action(Action::Confirm)
        }
        Target::PowerItem => {
            app.menus.power_focus = a.min(menus::POWER.len() - 1);
            Then::Action(Action::Confirm)
        }
        Target::SettingsRow => {
            app.menus.settings_focus = a.min(menus::SETTINGS.len() - 1);
            moved(app);
            push_menus(ui, app);
            Then::Nothing
        }
        Target::SettingsStep => {
            app.menus.settings_focus = a.min(menus::SETTINGS.len() - 1);
            Then::Action(if step < 0 {
                Action::Left
            } else {
                Action::Right
            })
        }
        // --- Controllers screen ---
        // A row (the boxes around the picture): add a key or button to it.
        Target::RemapRow => {
            if a < controllers::ALL.len() && !app.capturing && app.remap_run.is_none() {
                app.remap_selected = a;
                if let Some(sound) = start_capture(ui, app) {
                    play_sound(app, sound);
                }
                sync_controllers_screen(ui, app);
            }
            Then::Nothing
        }
        // A button on the controller picture: remap it.
        Target::RemapButton => {
            if a < controllers::ALL.len() && !app.capturing && app.remap_run.is_none() {
                app.remap_selected = a;
                if let Some(sound) = start_remap_selected(ui, app) {
                    play_sound(app, sound);
                }
                sync_controllers_screen(ui, app);
            }
            Then::Nothing
        }
        Target::ControllerMode => {
            if let Some(mode) = controllers::Mode::ALL.get(a)
                && *mode != app.controllers_mode
            {
                set_controllers_mode(ui, app, *mode);
                sync_controllers_screen(ui, app);
            }
            Then::Nothing
        }
        Target::RemapCommand => {
            remap_command(ui, app, a);
            Then::Nothing
        }
        Target::HoldExit => {
            leave_controllers_screen(ui, app);
            Then::Nothing
        }
        // --- Add games ---
        Target::AddRow => {
            let screen = &mut app.add_games;
            let (focus, count) = match screen.mode {
                add_games::Mode::Results => (&mut screen.focus, screen.rows.len()),
                _ => (&mut screen.place_focus, screen.places.len()),
            };
            if a >= count {
                return Then::Nothing;
            }
            if *focus == a && app.repeated_click {
                return Then::Action(Action::Confirm);
            }
            *focus = a;
            moved(app);
            sync_add_games_screen(ui, app);
            Then::Nothing
        }
        // --- Choose art ---
        Target::ArtThumb => {
            let Some(picker) = app.art_picker.as_mut() else {
                return Then::Nothing;
            };
            if a > 2 || b >= picker.choices[a].len() {
                return Then::Nothing;
            }
            if picker.slot == a && picker.focus[a] == b && app.repeated_click {
                return Then::Action(Action::Confirm);
            }
            picker.slot = a;
            picker.focus[a] = b;
            moved(app);
            sync_art_screen(ui, app);
            Then::Nothing
        }
    }
}

/// Right click: back. The Controllers screen normally wants B held (a pad button is easy to
/// brush); a click is deliberate, so it leaves at once.
fn mouse_back(ui: &AppWindow, app_rc: &Rc<RefCell<App>>) {
    if input_paused(&app_rc.borrow()) {
        return;
    }
    if app_rc.borrow().menus.overlay == Overlay::Controllers {
        let mut app = app_rc.borrow_mut();
        // A right click first stops a remap or an add in progress.
        if app.capturing || app.remap_run.is_some() {
            cancel_remapping(ui, &mut app);
            sync_controllers_screen(ui, &mut app);
        } else {
            leave_controllers_screen(ui, &mut app);
        }
        return;
    }
    handle_action(ui, app_rc, Action::Back);
}

/// The wheel moves through whatever is on screen: lists up and down, rows left and right.
fn mouse_wheel(ui: &AppWindow, app_rc: &Rc<RefCell<App>>, delta: i32) {
    if delta == 0 || input_paused(&app_rc.borrow()) {
        return;
    }
    let vertical = {
        let mut app = app_rc.borrow_mut();
        let App { home, menus, .. } = &mut *app;
        // On the home screen the wheel scrolls games: from the tab bar or the buttons it moves
        // to the game row first (the recently played row scrolls itself).
        if menus.overlay == Overlay::None
            && home.sheet.is_none()
            && matches!(home.zone, Zone::Tabs | Zone::Actions)
            && !home.tab().games.is_empty()
        {
            home.gear = false;
            home.zone = Zone::Icons;
        }
        match menus.overlay {
            Overlay::None => home.sheet.is_some(),
            Overlay::Art => false,
            _ => true,
        }
    };
    let action = match (vertical, delta > 0) {
        (true, true) => Action::Down,
        (true, false) => Action::Up,
        (false, true) => Action::Right,
        (false, false) => Action::Left,
    };
    handle_action(ui, app_rc, action);
}

/// The mouse moved: show the pointer until it rests for POINTER_IDLE.
fn show_pointer(ui: &AppWindow, app: &App) {
    let mouse = ui.global::<Mouse>();
    if !mouse.get_visible() {
        mouse.set_visible(true);
    }
    let weak = ui.as_weak();
    app.pointer_timer
        .start(TimerMode::SingleShot, POINTER_IDLE, move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<Mouse>().set_visible(false);
            }
        });
}

fn leave_controllers_screen(ui: &AppWindow, app: &mut App) {
    if app.menus.overlay != Overlay::Controllers {
        return;
    }
    cancel_back_hold(ui, app);
    // Done so far stays saved; the screen opens in Test mode next time.
    app.remap_run = None;
    app.controllers_mode = controllers::Mode::Test;
    app.menus.overlay = Overlay::Panel;
    play_sound(app, Sound::Back);
    push_menus(ui, app);
    sync_controllers_screen(ui, app);
}

fn apply_effects(ui: &AppWindow, app_rc: &Rc<RefCell<App>>, effects: &[Effect]) {
    let mut app = app_rc.borrow_mut();
    for effect in effects {
        match effect {
            Effect::Tabs => push_tabs(ui, &mut app),
            Effect::Selection => push_selection(ui, &mut app),
            Effect::Launch(game) => start_game(ui, &mut app, *game),
            Effect::Resume => resume_game(ui, &mut app),
            Effect::ControllerMode(game) => {
                if app.home.running == Some(*game) {
                    input::set_translation(translation_for(&app.home, *game));
                }
            }
            Effect::OpenPanel => {
                app.menus.overlay = Overlay::Panel;
                push_menus(ui, &app);
            }
            Effect::Remove(game) => remove_game(ui, &mut app, *game),
            Effect::ChangeArt(game) => open_art_picker(ui, &mut app, *game),
            Effect::Unavailable(game) => {
                // The drive may have been plugged in since the last check.
                let missing = {
                    let labels = &app.drive_labels;
                    library::check(&app.home.games[*game].requires, |l| {
                        labels.get(&l.to_string()).cloned()
                    })
                };
                match missing {
                    None => {
                        app.home.games[*game].missing = None;
                        push_tabs(ui, &mut app);
                        push_selection(ui, &mut app);
                        start_game(ui, &mut app, *game);
                    }
                    Some(missing) => {
                        let message = missing.play_message(&app.home.games[*game].title);
                        if app.home.games[*game].missing.as_ref() != Some(&missing) {
                            app.home.games[*game].missing = Some(missing);
                            push_tabs(ui, &mut app);
                            push_selection(ui, &mut app);
                        }
                        app.toasts.show(ui, message);
                    }
                }
            }
            Effect::CloseGame => {
                // The watcher notices the processes ending and brings the launcher back.
                if let Some(running) = &app.running {
                    launcher::close(running);
                }
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
        diag::log(&problem);
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
                    update_music(&app.borrow());
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
    let info = match (&game.missing, played) {
        (Some(missing), _) => format!("{} · {}", game.platform, missing.describe()),
        (None, Some(played)) => format!("{} · {played}", game.platform),
        (None, None) => game.platform.clone(),
    };
    ui.set_info(info.into());
    ui.set_selected_available(game.missing.is_none());
    ui.set_is_favorite(home.is_favorite(index));
    ui.set_selected_running(home.running == Some(index));
    ui.set_backdrop(title_color(&game.title));
    let logo = game
        .art
        .logo
        .as_deref()
        .and_then(|p| images.small(p, scaled_art::Kind::Logo));
    ui.set_has_logo(logo.is_some());
    ui.set_logo(logo.unwrap_or_default());
}

/// Cheap per-keypress values: focus positions, zone and the game sheet.
fn push_scalars(ui: &AppWindow, home: &Home) {
    ui.set_zone(home.zone);
    ui.set_gear_focused(home.gear && home.zone == Zone::Tabs);
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
                SheetItem::Resume => "Resume".into(),
                SheetItem::CloseGame => "Close game".into(),
                SheetItem::ControllerMode => match home.controller_mode(sheet.game) {
                    ControllerMode::Xbox => "Controller: Xbox for every pad".into(),
                    ControllerMode::PlaystationNative => {
                        "Controller: PlayStation pads native".into()
                    }
                    ControllerMode::Off => "Controller: game reads pads itself".into(),
                },
                SheetItem::ToggleFavorite if home.is_favorite(sheet.game) => {
                    "Remove from Favorites".into()
                }
                SheetItem::ToggleFavorite => "Add to Favorites".into(),
                SheetItem::Remove => "Remove from library".into(),
                SheetItem::ChangeArt => "Change art".into(),
            })
            .collect();
        ui.set_sheet_items(ModelRc::new(VecModel::from(items)));
        ui.set_sheet_index(sheet.focus as i32);
    }
}

/// Crossfades to the selected game's background: its hero art, else its icon, else a color.
/// Uncached art decodes on the background thread; the previous art stays up until it arrives.
fn show_hero(ui: &AppWindow, app: &mut App) {
    let App {
        home,
        images,
        shown_hero,
        hero_loader,
        ..
    } = app;
    let path = home
        .selected_game()
        .map(|i| &home.games[i].art)
        .and_then(|art| {
            // Without hero art the cover fills the screen, unless "None" was chosen for it.
            if art.hero_off {
                None
            } else {
                art.hero.clone().or_else(|| art.icon.clone())
            }
        });
    if path == *shown_hero {
        return;
    }
    shown_hero.clone_from(&path);
    let Some(path) = path else {
        ui.set_has_hero(false);
        return;
    };
    match images.cached_hero(&path) {
        Some(image) => display_hero(ui, image),
        None => {
            // Only fails if the decoder thread is gone; the old art then simply stays.
            let _ = hero_loader.send((path, scaled_art::screen_scale()));
        }
    }
}

fn display_hero(ui: &AppWindow, image: Image) {
    if ui.get_show_a() {
        ui.set_hero_b(image);
        ui.set_show_a(false);
    } else {
        ui.set_hero_a(image);
        ui.set_show_a(true);
    }
    ui.set_has_hero(true);
}

/// Results from the worker threads.
fn drain_background(ui: &AppWindow, app: &mut App) {
    while let Ok(event) = app.background.try_recv() {
        match event {
            background::Event::Hero { path, pixels } => hero_loaded(ui, app, path, pixels),
            background::Event::ArtFile { id } => refresh_art(ui, app, &id),
            background::Event::Art { id, outcome } => art_fetched(ui, app, id, outcome),
            background::Event::ArtChoice {
                game,
                slot,
                path,
                label,
                hash,
                in_use,
                full,
                thumbnail,
            } => {
                let Some(picker) = app.art_picker.as_mut().filter(|p| p.session == game) else {
                    continue;
                };
                if let Some(index) = picker.add(slot, path, label, hash, in_use, full) {
                    let image = thumbnail
                        .map(scaled_art::Pixels::into_image)
                        .unwrap_or_default();
                    app.art_thumbs[slot].insert(index, image);
                    sync_art_screen(ui, app);
                }
            }
            background::Event::ArtUpgrade {
                game,
                slot,
                token,
                path,
            } => {
                let key = (game.clone(), slot);
                let Some((_, dir)) = app.art_upgrades.get(&key).filter(|(t, _)| *t == token) else {
                    continue;
                };
                let dir = dir.clone();
                app.art_upgrades.remove(&key);
                match library::replace_art(&dir, art_picker::SLOTS[slot], Some(&path)) {
                    Ok(()) => refresh_game_art(ui, app, &game, &dir),
                    Err(e) => diag::log(&e),
                }
            }
            background::Event::ArtChoicesDone { game, error } => {
                if let Some(picker) = app.art_picker.as_mut().filter(|p| p.session == game) {
                    picker.searching = false;
                    if let Some(e) = error {
                        diag::log(&format!("art choices for {game}: {e}"));
                        picker.trouble = true;
                    }
                    sync_art_screen(ui, app);
                }
            }
            background::Event::Reachable { ids } => {
                app.reach_check = false;
                if !ids.is_empty() && app.home.running.is_none() && !app.in_background {
                    diag::log(&format!("games reachable again: {ids:?}"));
                    match reload_library(ui, app) {
                        Ok(_) => {
                            let still_missing = |id: &String| {
                                app.home
                                    .games
                                    .iter()
                                    .any(|g| g.id == *id && g.missing.is_some())
                            };
                            let (stuck, back): (Vec<String>, Vec<String>) =
                                ids.into_iter().partition(|id| still_missing(id));
                            app.reach_stuck.extend(stuck);
                            let count = back.len();
                            if count > 0 {
                                let games = if count == 1 { "game is" } else { "games are" };
                                app.toasts
                                    .show(ui, format!("{count} {games} available again"));
                            }
                        }
                        Err(e) => diag::log(&e),
                    }
                }
            }
            background::Event::Listing { dir, folders } => {
                app.add_games.set_listing(&dir, folders);
                sync_add_games_screen(ui, app);
            }
            background::Event::ScanProgress { folders } if app.add_games.scanning => {
                app.add_games.folders = folders;
                sync_add_games_screen(ui, app);
            }
            background::Event::ScanDone { found } if app.add_games.scanning => {
                diag::log(&format!("drive scan found {} games", found.len()));
                app.add_games
                    .set_results(found, &app.home.state.ignored_folders);
                sync_add_games_screen(ui, app);
            }
            // A scan that was cancelled (screen closed) still finishing its last step.
            background::Event::ScanProgress { .. } | background::Event::ScanDone { .. } => {}
        }
    }
    for problem in app.images.problems.drain(..) {
        diag::log(&problem);
    }
}

fn hero_loaded(ui: &AppWindow, app: &mut App, path: PathBuf, pixels: Option<scaled_art::Pixels>) {
    // A game took the screen meanwhile: its memory matters more than this picture. Forgetting
    // it as shown makes come_back ask for it again.
    if app.in_background {
        if app.shown_hero.as_ref() == Some(&path) {
            app.shown_hero = None;
        }
        return;
    }
    // The decoder can't read every format (e.g. WebP); Slint can, at full size.
    let image = match pixels {
        Some(pixels) => Some(pixels.into_image()),
        None => app.images.load(&path),
    };
    let wanted = app.shown_hero.as_ref() == Some(&path);
    match image {
        Some(image) => {
            app.images.insert_hero(path, image.clone());
            if wanted {
                display_hero(ui, image);
            }
        }
        None if wanted => ui.set_has_hero(false),
        None => {}
    }
}

/// PC games missing an icon, background or logo get one Steam lookup each; the answer is
/// remembered in state.json, so a game Steam doesn't know isn't searched again every start.
fn queue_art_lookups(app: &mut App) {
    let art_dir = config::data_dir().join("art");
    let App {
        home,
        art_fetcher,
        art_pending,
        art_failed,
        sgdb_key,
        ..
    } = app;
    let now = std::time::Instant::now();
    for game in &home.games {
        let folder = art_dir.join(library::safe_file_name(&game.title));
        if game.platform != "PC"
            || home.state.art_lookups.contains_key(&game.id)
            || art_pending.contains(&game.id)
            || art_failed
                .get(&game.id)
                .is_some_and(|&(tries, next)| tries > RETRY_DELAYS.len() as u32 || next > now)
        {
            continue;
        }
        let wanted: Vec<&'static str> = [
            (game.art.icon.is_none(), "icon"),
            (game.art.hero.is_none(), "hero"),
            (game.art.logo.is_none(), "logo"),
        ]
        .into_iter()
        // A slot turned off on the Choose art screen stays empty.
        .filter(|(_, slot)| !library::art_turned_off(&folder, slot))
        .filter_map(|(missing, file)| missing.then_some(file))
        .collect();
        if wanted.is_empty() {
            continue;
        }
        let job = ArtJob {
            id: game.id.clone(),
            title: game.title.clone(),
            dir: folder,
            wanted,
            sgdb_key: sgdb_key.clone(),
        };
        if art_fetcher.send(job).is_ok() {
            art_pending.insert(game.id.clone());
        }
    }
}

fn art_fetched(ui: &AppWindow, app: &mut App, id: String, outcome: ArtOutcome) {
    app.art_pending.remove(&id);
    let lookup = match outcome {
        ArtOutcome::Found {
            app: steam_app,
            saved,
        } => {
            // Each image was already shown as it arrived (ArtFile).
            diag::log(&format!(
                "art for {id}: Steam app {steam_app:?}, {saved} new images"
            ));
            steam_app
        }
        ArtOutcome::NoMatch => {
            diag::log(&format!("art for {id}: unknown to Steam and PCGamingWiki"));
            None
        }
        ArtOutcome::Failed(e) => {
            // Not saved: tried again later this session (see RETRY_DELAYS), then next start.
            diag::log(&format!("art for {id}: {e}"));
            let tries = app.art_failed.get(&id).map_or(0, |&(t, _)| t) + 1;
            let delay = RETRY_DELAYS
                .get(tries as usize - 1)
                .copied()
                .unwrap_or(Duration::MAX / 4);
            app.art_failed
                .insert(id, (tries, std::time::Instant::now() + delay));
            return;
        }
    };
    app.home.state.art_lookups.insert(id, lookup);
    if let Err(e) = app.home.state.save(&app.state_path) {
        app.toasts.show(ui, e);
    }
}

/// New files landed in a game's art folder: show them without reloading the library.
fn refresh_art(ui: &AppWindow, app: &mut App, id: &str) {
    let art_dir = config::data_dir().join("art");
    if let Some(game) = app.home.games.iter_mut().find(|g| g.id == id) {
        library::apply_art_folder(game, &art_dir);
    }
    // During a game nothing is drawn: decoding now would only hold memory the game needs.
    // come_back pushes the tiles and background again.
    if app.in_background {
        return;
    }
    push_tabs(ui, app);
    push_selection(ui, app);
    show_hero(ui, app);
}

fn start_game(ui: &AppWindow, app: &mut App, game: usize) {
    if let Some(running) = app.home.running {
        let title = app.home.games[running].title.clone();
        app.toasts
            .show(ui, format!("Close {title} first (⋯ → Close game)"));
        return;
    }
    // Plugged in before the game starts so it finds the virtual pad when it enumerates pads.
    input::set_translation(translation_for(&app.home, game));
    // Games open on the main display: switch first if they belong on another screen.
    let game_screen = app.home.state.game_screen.clone();
    use_screen_layout(ui, app, &game_screen, true);
    let weak = ui.as_weak();
    let started = launcher::start(&app.home.games[game].launch, move |ended| {
        let message = match ended {
            Ended::Normally => String::new(),
            Ended::Failed(message) => message,
        };
        // Only fails once the event loop has shut down, when there is nothing left to update.
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_game_ended(message.into());
            }
        });
    });
    match started {
        Ok(running) => {
            diag::log(&format!(
                "started {} (pid {})",
                app.home.games[game].title, running.root_pid
            ));
            app.running = Some(running);
            app.home.running = Some(game);
            app.game_screen_seen = None;
            let weak = ui.as_weak();
            app.game_screen_timer
                .start(TimerMode::Repeated, Duration::from_secs(15), move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_game_screen_check();
                    }
                });
            app.home.record_played(game, unix_now());
            if let Err(e) = app.home.state.save(&app.state_path) {
                app.toasts.show(ui, e);
            }
            push_tabs(ui, app);
            push_selection(ui, app);
            push_scalars(ui, &app.home);
            go_background(ui, app);
        }
        Err(e) => {
            input::set_translation(Translate::Off);
            show_launcher_on_its_screen(ui, app);
            app.toasts.show(ui, e);
        }
    }
}

/// Logs the screen the running game's window is on, when it changes (games often open a
/// small launcher first, then the real window).
fn game_screen_check(app: &mut App) {
    let Some(running) = &app.running else {
        app.game_screen_timer.stop();
        return;
    };
    let screen = launcher::game_window(running).and_then(display::screen_of);
    if screen.is_some() && screen != app.game_screen_seen {
        diag::log(&format!(
            "game window is on {}",
            screen.as_deref().unwrap_or("?")
        ));
        app.game_screen_seen = screen;
    }
}

/// Switches Windows to the layout `choice` needs (for a game, or for the launcher), first
/// remembering the user's own layout so it can be put back.
fn use_screen_layout(
    ui: &AppWindow,
    app: &mut App,
    choice: &display::ScreenChoice,
    for_game: bool,
) {
    let screens = display::screens();
    let state = &mut app.home.state;
    if screens.len() < 2 && state.display_restore.is_none() {
        return;
    }
    let original = state
        .display_restore
        .clone()
        .unwrap_or_else(|| display::current(&screens));
    let wanted = display::wanted(choice, &original, for_game);
    let now = display::current(&screens);
    let names: Vec<String> = screens
        .iter()
        .map(|s| format!("{} at ({}, {})", s.device, s.x, s.y))
        .collect();
    diag::log(&format!(
        "screens for {}: chosen {choice:?}; connected {names:?}; now {now:?}; wanted {wanted:?}",
        if for_game { "game" } else { "launcher" }
    ));
    if wanted != now {
        if state.display_restore.is_none() {
            state.display_restore = Some(original.clone());
            if let Err(e) = state.save(&app.state_path) {
                diag::log(&e);
            }
        }
        if let Err(e) = display::apply(&wanted) {
            diag::log(&e);
            app.toasts.show(ui, e);
            return;
        }
        diag::log(&format!(
            "screens switched; Windows now reports {:?}",
            display::current(&display::screens())
        ));
    }
    let state = &mut app.home.state;
    if wanted == original
        && state.display_restore.take().is_some()
        && let Err(e) = state.save(&app.state_path)
    {
        diag::log(&e);
    }
}

/// The launcher's screen: Windows' layout for it, then its window on the chosen screen.
fn show_launcher_on_its_screen(ui: &AppWindow, app: &mut App) {
    let choice = app.home.state.launcher_screen.clone();
    // Over a running game the game's layout stays: Resume goes straight back to it.
    if app.running.is_none() {
        use_screen_layout(ui, app, &choice, false);
    }
    let screens = display::screens();
    if screens.len() < 2 {
        return;
    }
    let target = match &choice {
        display::ScreenChoice::Screen(device) => screens.iter().find(|s| s.device == *device),
        _ => None,
    }
    .or_else(|| screens.iter().find(|s| s.primary))
    .or(screens.first());
    if let Some(screen) = target {
        // Full screen covers the screen the window is on, so move it there first.
        let window = ui.window();
        window.set_fullscreen(false);
        window.set_position(slint::PhysicalPosition::new(screen.x, screen.y));
        window.set_fullscreen(true);
    }
}

/// Puts back the screen layout the user had (on exit, or at start after a crash).
fn restore_screens(app: &mut App) {
    let Some(original) = app.home.state.display_restore.take() else {
        return;
    };
    if let Err(e) = display::apply(&original) {
        diag::log(&format!("could not restore the screen layout: {e}"));
        // Kept, so the next start tries again.
        app.home.state.display_restore = Some(original);
        return;
    }
    if let Err(e) = app.home.state.save(&app.state_path) {
        diag::log(&e);
    }
}

fn translation_for(home: &Home, game: usize) -> Translate {
    match home.controller_mode(game) {
        ControllerMode::Xbox => Translate::AllPads,
        ControllerMode::PlaystationNative => Translate::ExceptPlayStation,
        ControllerMode::Off => Translate::Off,
    }
}

fn resume_game(ui: &AppWindow, app: &mut App) {
    app.home.sheet = None;
    let Some(running) = &app.running else { return };
    match launcher::game_window(running) {
        Some(game_window) => {
            go_background(ui, app);
            let in_front = launcher::bring_to_front(game_window);
            diag::log(&format!(
                "resumed game; game window in foreground: {in_front}"
            ));
        }
        None => app
            .toasts
            .show(ui, "Couldn't find the game's window".into()),
    }
}

/// Start + Select: over a game it brings the launcher up; in the launcher it returns to the game.
fn home_combo(ui: &AppWindow, app: &mut App) {
    diag::log(&format!(
        "home combo received: game running: {}, launcher hidden: {}",
        app.home.running.is_some(),
        app.in_background
    ));
    // Start + Select is pressed to test the pad there; only holding B leaves that screen.
    if app.menus.overlay == Overlay::Controllers && !app.in_background {
        return;
    }
    let Some(game) = app.home.running else { return };
    if app.in_background {
        app.home.open_close_menu();
        push_tabs(ui, app);
        come_back(ui, app);
        let title = app.home.games[game].title.clone();
        app.toasts.show(
            ui,
            format!("Press A to close {title}, or hold Start + Select to go back to it"),
        );
    } else {
        resume_game(ui, app);
    }
}

fn game_ended(ui: &AppWindow, app: &mut App, message: &str) {
    diag::log(&format!("game ended {message}"));
    let title = app
        .home
        .running
        .map(|g| app.home.games[g].title.clone())
        .unwrap_or_default();
    app.running = None;
    app.home.running = None;
    app.game_screen_timer.stop();
    input::set_translation(Translate::Off);
    // A sheet opened on the running game lists Resume / Close game, which no longer apply.
    app.home.sheet = None;
    if app.in_background {
        come_back(ui, app);
    } else {
        // The launcher was already up over the game, still on the game's screen layout.
        show_launcher_on_its_screen(ui, app);
        push_selection(ui, app);
        push_scalars(ui, &app.home);
    }
    if !message.is_empty() {
        app.toasts.show(ui, format!("{title} {message}"));
    }
}

/// Gets out of the game's way: hidden window, full-screen art released, slow pad polling.
/// Feedback sound for a key press, from what it changed on screen.
fn sound_for(
    action: Action,
    before: NavState,
    after: NavState,
    effects: &[Effect],
) -> Option<Sound> {
    if effects.iter().any(|e| matches!(e, Effect::Unavailable(_))) {
        return Some(Sound::Bump);
    }
    if effects.iter().any(|e| matches!(e, Effect::Launch(_))) {
        return Some(Sound::Launch);
    }
    if effects.iter().any(|e| {
        matches!(
            e,
            Effect::Resume
                | Effect::CloseGame
                | Effect::ControllerMode(_)
                | Effect::OpenPanel
                | Effect::Remove(_)
                | Effect::ChangeArt(_)
        )
    }) {
        return Some(Sound::Confirm);
    }
    match (before.sheet_open, after.sheet_open) {
        (false, true) => return Some(Sound::Confirm),
        (true, false) if matches!(action, Action::Back | Action::Options) => {
            return Some(Sound::Back);
        }
        (true, false) => return Some(Sound::Confirm),
        _ => {}
    }
    if before.tab != after.tab {
        return Some(Sound::Tab);
    }
    if before != after {
        return Some(if action == Action::Back {
            Sound::Back
        } else {
            Sound::Move
        });
    }
    let navigation = matches!(
        action,
        Action::Up
            | Action::Down
            | Action::Left
            | Action::Right
            | Action::PagePrev
            | Action::PageNext
    );
    navigation.then_some(Sound::Bump)
}

fn go_background(ui: &AppWindow, app: &mut App) {
    app.in_background = true;
    app.drive_timer.stop();
    update_music(app);
    input::IN_GAME.store(true, Ordering::Relaxed);
    app.hero_timer.stop();
    if let Err(e) = ui.hide() {
        diag::log(&format!("could not hide the window: {e}"));
    }
    ui.set_has_hero(false);
    ui.set_hero_a(Image::default());
    ui.set_hero_b(Image::default());
    app.images.heroes.clear();
    app.shown_hero = None;
    launcher::trim_memory();
}

/// Plays a UI sound; the audio device pauses again shortly after, so silence costs no CPU.
fn play_sound(app: &App, sound: Sound) {
    if let Some(audio) = &app.audio {
        audio.play(sound);
    }
    // The longest sound (the launch whoosh) lasts 0.8 s.
    schedule_audio_pause(app, Duration::from_secs(1));
}

/// Lets the audio device pause after `delay` if no sound or music is playing by then.
fn schedule_audio_pause(app: &App, delay: Duration) {
    let Some(audio) = app.audio.clone() else {
        return;
    };
    let music = app.music.clone();
    app.audio_timer
        .start(TimerMode::SingleShot, delay, move || {
            let music_playing = music.as_ref().is_some_and(|m| m.is_playing());
            audio.pause_if_idle(music_playing);
        });
}

/// Plays the selected game's music (or fades to silence when it has none).
fn update_music(app: &App) {
    let Some(music) = &app.music else { return };
    let path = if app.in_background {
        None
    } else {
        app.home
            .selected_game()
            .and_then(|i| app.home.games[i].art.music.clone())
    };
    music.play(path);
    // Covers the fade-out when switching to a game without music.
    schedule_audio_pause(app, Duration::from_millis(1500));
}

fn come_back(ui: &AppWindow, app: &mut App) {
    app.in_background = false;
    app.input_ready_at = std::time::Instant::now() + INPUT_GRACE;
    play_sound(app, Sound::Confirm);
    input::IN_GAME.store(false, Ordering::Relaxed);
    if let Err(e) = ui.show() {
        app.toasts
            .show(ui, format!("Could not show the launcher: {e}"));
        return;
    }
    ui.window().set_fullscreen(true);
    show_launcher_on_its_screen(ui, app);
    match window_handle(ui) {
        Some(hwnd) => {
            let in_front = launcher::bring_to_front(hwnd);
            diag::log(&format!("launcher shown; in foreground: {in_front}"));
        }
        None => diag::log("launcher shown, but no window handle to bring to the front"),
    }
    // Art may have arrived while the game ran.
    push_tabs(ui, app);
    push_selection(ui, app);
    push_scalars(ui, &app.home);
    show_hero(ui, app);
    update_music(app);
    app.drive_timer.restart();
    check_drives(ui, app);
}

fn window_handle(ui: &AppWindow) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = ui.window().window_handle();
    match handle.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get() as _),
        _ => None,
    }
}

fn make_tile(game: &Game, images: &mut ImageCache) -> Tile {
    let icon = game
        .art
        .icon
        .as_deref()
        .and_then(|p| images.small(p, scaled_art::Kind::Icon));
    Tile {
        title: game.title.as_str().into(),
        color: title_color(&game.title),
        has_icon: icon.is_some(),
        icon: icon.unwrap_or_default(),
        available: game.missing.is_none(),
        // Segoe MDL2: a drive with a cross, or a warning sign.
        badge: match game.missing {
            Some(Missing::Drive { .. }) => "\u{E8CD}".into(),
            Some(Missing::Files(_)) => "\u{E7BA}".into(),
            None => SharedString::new(),
        },
    }
}

/// Decoded images, so moving back and forth never decodes the same file twice.
#[derive(Default)]
struct ImageCache {
    /// Small art (icons, logos), kept for the whole session. `None` remembers a failed load.
    /// Keyed by use too: one file set as both icon and logo is sized for each.
    icons: HashMap<(PathBuf, scaled_art::Kind), Option<Image>>,
    /// Screen scale the small art was sized for; a bigger screen appearing reloads it.
    scale: f32,
    /// Full-screen art is large (8 MB decoded each), so only the most recent few are kept.
    heroes: VecDeque<(PathBuf, Image)>,
    problems: Vec<String>,
}

impl ImageCache {
    fn small(&mut self, path: &Path, kind: scaled_art::Kind) -> Option<Image> {
        let scale = scaled_art::screen_scale();
        if scale > self.scale {
            diag::log(&format!("small art sized for screen scale {scale}"));
            self.icons.clear();
            self.scale = scale;
        }
        let key = (path.to_path_buf(), kind);
        if let Some(cached) = self.icons.get(&key) {
            return cached.clone();
        }
        let image = scaled_art::load(path, kind, scale).or_else(|| self.load(path));
        self.icons.insert(key, image.clone());
        image
    }

    /// A recently shown background, moved to the fresh end of the cache.
    fn cached_hero(&mut self, path: &Path) -> Option<Image> {
        let pos = self.heroes.iter().position(|(p, _)| p == path)?;
        let entry = self.heroes.remove(pos)?;
        self.heroes.push_back(entry.clone());
        Some(entry.1)
    }

    fn insert_hero(&mut self, path: PathBuf, image: Image) {
        self.heroes.retain(|(p, _)| *p != path);
        if self.heroes.len() == HERO_CACHE {
            self.heroes.pop_front();
        }
        self.heroes.push_back((path, image));
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

/// Quick panel, Power and Settings: navigation, then whatever the chosen item does.
fn handle_menu_action(ui: &AppWindow, app: &mut App, action: Action) {
    // Start too: pressing it to test the pad must not close the Controllers screen.
    if app.menus.overlay == Overlay::Controllers {
        handle_controllers_action(ui, app, action);
        return;
    }
    if app.menus.overlay == Overlay::Art && action != Action::Menu {
        handle_art_action(ui, app, action);
        return;
    }
    if app.menus.overlay == Overlay::AddGames && action != Action::Menu {
        handle_add_games_action(ui, app, action);
        return;
    }
    let snapshot = |m: &Menus| (m.overlay, m.panel_focus, m.power_focus, m.settings_focus);
    let before = snapshot(&app.menus);
    let effects = app.menus.handle(action);
    let after = snapshot(&app.menus);
    let navigation = matches!(
        action,
        Action::Up | Action::Down | Action::Left | Action::Right
    );
    let sound = if before.0 != after.0 {
        Some(if after.0 == Overlay::None || action == Action::Back {
            Sound::Back
        } else {
            Sound::Confirm
        })
    } else if before != after || !effects.is_empty() {
        Some(Sound::Move)
    } else {
        navigation.then_some(Sound::Bump)
    };
    if let Some(sound) = sound {
        play_sound(app, sound);
    }
    for effect in effects {
        match effect {
            MenuEffect::Rescan => rescan(ui, app),
            MenuEffect::FindGames => {
                // Where the last scan looked, if that folder is still there.
                let place = match &app.home.state.scan_place {
                    Some(folder) if folder.is_dir() => add_games::Place::Folder(folder.clone()),
                    _ => add_games::Place::AllDrives,
                };
                start_find_games(app, place);
            }
            MenuEffect::Exit => {
                let _ = slint::quit_event_loop();
            }
            MenuEffect::Sleep => report(ui, app, launcher::sleep()),
            MenuEffect::Restart => report(ui, app, launcher::restart()),
            MenuEffect::ShutDown => report(ui, app, launcher::shut_down()),
            MenuEffect::Adjust(setting, delta) => {
                let settings = &mut app.home.state.settings;
                match setting {
                    Setting::SoundVolume => {
                        settings.sound_volume = state::Settings::step(settings.sound_volume, delta)
                    }
                    Setting::MusicVolume => {
                        settings.music_volume = state::Settings::step(settings.music_volume, delta)
                    }
                    Setting::LauncherScreen => {
                        let screens = display::screens();
                        let state = &mut app.home.state;
                        state.launcher_screen =
                            display::step(&state.launcher_screen, &screens, delta);
                        show_launcher_on_its_screen(ui, app);
                    }
                    Setting::GameScreen => {
                        let screens = display::screens();
                        let state = &mut app.home.state;
                        state.game_screen = display::step(&state.game_screen, &screens, delta);
                    }
                }
                apply_volumes(app);
                if let Err(e) = app.home.state.save(&app.state_path) {
                    app.toasts.show(ui, e);
                }
            }
        }
    }
    push_menus(ui, app);
    sync_controllers_screen(ui, app);
    sync_add_games_screen(ui, app);
    sync_art_screen(ui, app);
}

fn art_choices_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("cache").join("art-choices")
}

/// Opens the Choose art screen for a game and starts gathering its choices.
fn open_art_picker(ui: &AppWindow, app: &mut App, game: usize) {
    let data_dir = config::data_dir();
    let game = &app.home.games[game];
    let folder = library::safe_file_name(&game.title);
    let mut picker = art_picker::ArtPicker::new(
        game.id.clone(),
        game.title.clone(),
        data_dir.join("art").join(&folder),
    );
    let current = [
        game.art.icon.clone(),
        game.art.hero.clone(),
        game.art.logo.clone(),
    ];
    for (slot, art) in current.iter().enumerate() {
        // Without a background the cover fills the screen, so "None" is only in use when it
        // was chosen.
        let none_shown = if slot == 1 {
            game.art.hero_off
        } else {
            art.is_none()
        };
        if none_shown {
            picker.none_in_use(slot);
        }
    }
    app.art_sessions += 1;
    picker.session = format!("{}#{}", game.id, app.art_sessions);
    let job = background::ChoicesJob {
        game: picker.session.clone(),
        title: game.title.clone(),
        cache: art_choices_dir(&data_dir).join(folder),
        current,
        scale: scaled_art::screen_scale(),
        sgdb_key: app.sgdb_key.clone(),
        game_dir: game.launch.exe.parent().map(Path::to_path_buf),
    };
    app.art_cancel.store(true, Ordering::Relaxed);
    app.art_cancel = Arc::new(AtomicBool::new(false));
    background::spawn_art_choices(app.notifier.clone(), job, app.art_cancel.clone());
    app.art_picker = Some(picker);
    app.art_thumbs = Default::default();
    app.menus.overlay = Overlay::Art;
    push_menus(ui, app);
    sync_art_screen(ui, app);
}

/// The Choose art screen: Up/Down pick the row, Left/Right a picture, A uses it, B closes.
fn handle_art_action(ui: &AppWindow, app: &mut App, action: Action) {
    let Some(picker) = app.art_picker.as_mut() else {
        return;
    };
    let before = (picker.slot, picker.focus);
    let effect = picker.handle(action);
    let after = (picker.slot, picker.focus);
    let sound = match effect {
        Some(art_picker::Effect::Apply { slot, index }) => {
            apply_art(ui, app, slot, index);
            Some(Sound::Confirm)
        }
        Some(art_picker::Effect::Retry) => {
            let id = app.art_picker.as_ref().map(|p| p.game.clone());
            if let Some(game) = id.and_then(|id| app.home.games.iter().position(|g| g.id == id)) {
                open_art_picker(ui, app, game);
            }
            Some(Sound::Confirm)
        }
        Some(art_picker::Effect::Close) => {
            app.menus.overlay = Overlay::None;
            push_menus(ui, app);
            Some(Sound::Back)
        }
        None if before.0 != after.0 => Some(Sound::Tab),
        None if before != after => Some(Sound::Move),
        None => matches!(
            action,
            Action::Up | Action::Down | Action::Left | Action::Right
        )
        .then_some(Sound::Bump),
    };
    if let Some(sound) = sound {
        play_sound(app, sound);
    }
    sync_art_screen(ui, app);
}

/// Makes the picked picture the game's art and shows it at once.
fn apply_art(ui: &AppWindow, app: &mut App, slot: usize, index: usize) {
    let Some(picker) = app.art_picker.as_mut() else {
        return;
    };
    let choice = picker.choices[slot][index].clone();
    let dir = picker.art_dir.clone();
    let game_id = picker.game.clone();
    if let Err(e) = library::replace_art(&dir, art_picker::SLOTS[slot], choice.path.as_deref()) {
        diag::log(&e);
        app.toasts.show(ui, e);
        return;
    }
    picker.applied(slot, index);
    // The preview shows at once; the sharp version replaces it when it arrives.
    let key = (game_id.clone(), slot);
    match choice.full {
        Some(url) => {
            app.art_upgrade_token += 1;
            let token = app.art_upgrade_token;
            app.art_upgrades.insert(key, (token, dir.clone()));
            let folder = library::safe_file_name(&picker.title);
            let cache = art_choices_dir(&config::data_dir()).join(folder);
            let notifier = app.notifier.clone();
            background::spawn_art_upgrade(notifier, url, cache, game_id.clone(), slot, token);
        }
        None => {
            app.art_upgrades.remove(&key);
        }
    }
    refresh_game_art(ui, app, &game_id, &dir);
}

/// A game's art folder changed: show its new pictures everywhere.
fn refresh_game_art(ui: &AppWindow, app: &mut App, game_id: &str, dir: &Path) {
    let art_root = config::data_dir().join("art");
    if let Some(game) = app.home.games.iter_mut().find(|g| g.id == game_id) {
        library::apply_art_folder(game, &art_root);
    }
    // The new file can have the old one's name: forget what was decoded from that folder.
    app.images
        .icons
        .retain(|(path, _), _| !path.starts_with(dir));
    app.images.heroes.retain(|(path, _)| !path.starts_with(dir));
    app.shown_hero = None;
    if app.in_background {
        return;
    }
    push_tabs(ui, app);
    push_selection(ui, app);
    show_hero(ui, app);
}

/// Mirrors the Choose art screen into the window; leaving it stops its downloads.
fn sync_art_screen(ui: &AppWindow, app: &mut App) {
    if app.menus.overlay != Overlay::Art {
        if app.art_picker.take().is_some() {
            app.art_cancel.store(true, Ordering::Relaxed);
            app.art_thumbs = Default::default();
            ui.set_art_rows(ModelRc::default());
        }
        return;
    }
    let Some(picker) = &app.art_picker else {
        return;
    };
    const NAMES: [&str; 3] = ["Cover", "Background", "Logo"];
    let rows: Vec<ArtRow> = (0..3)
        .map(|slot| {
            let choices = &picker.choices[slot];
            let focus = picker.focus[slot].min(choices.len() - 1);
            let thumbs: Vec<ArtThumb> = choices
                .iter()
                .enumerate()
                .map(|(i, choice)| ArtThumb {
                    has_image: choice.path.is_some(),
                    image: app.art_thumbs[slot].get(i).cloned().unwrap_or_default(),
                    in_use: choice.in_use,
                })
                .collect();
            ArtRow {
                name: NAMES[slot].into(),
                label: format!(
                    "{} of {}  ·  {}",
                    focus + 1,
                    choices.len(),
                    choices[focus].label
                )
                .into(),
                focus: focus as i32,
                thumbs: ModelRc::new(VecModel::from(thumbs)),
            }
        })
        .collect();
    let found: usize = picker.choices.iter().map(|row| row.len() - 1).sum();
    ui.set_art_title(picker.title.as_str().into());
    ui.set_art_status(
        if picker.searching {
            format!("Looking for art… {found} pictures so far")
        } else if picker.trouble {
            format!("{found} pictures found · some sources couldn't be reached (internet?) · RB to look again")
        } else {
            format!("{found} pictures found · RB to look again")
        }
        .into(),
    );
    ui.set_art_slot(picker.slot as i32);
    ui.set_art_rows(ModelRc::new(VecModel::from(rows)));
}

/// Starts looking for games in `place`: every fixed and removable drive, or one drive or folder.
fn start_find_games(app: &mut App, place: add_games::Place) {
    // A previous scan still running is no longer wanted.
    app.scan_cancel.store(true, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    app.scan_cancel = cancel.clone();
    let roots = match &place {
        add_games::Place::AllDrives => library::drives::scannable()
            .into_iter()
            .map(|letter| PathBuf::from(format!("{letter}:\\")))
            .collect(),
        add_games::Place::Folder(folder) => vec![folder.clone()],
    };
    app.add_games.start_scan(place);
    let mut exes = Vec::new();
    let mut skip = vec![config::data_dir()];
    for game in &app.home.games {
        exes.push(game.launch.exe.clone());
        exes.extend(game.requires.iter().cloned());
        // Emulator folders hold the emulator, not PC games.
        if game.platform != "PC"
            && let Some(dir) = game.launch.exe.parent()
        {
            skip.push(dir.to_path_buf());
        }
    }
    let known = library::discover::Known::new(&exes, &skip);
    background::spawn_scan(app.notifier.clone(), roots, known, cancel);
}

/// Remembers where the user chose to look: next time the screen starts there, and a folder
/// becomes a one-press place.
fn remember_place(ui: &AppWindow, app: &mut App, place: &add_games::Place) {
    let state = &mut app.home.state;
    match place {
        add_games::Place::AllDrives => state.scan_place = None,
        add_games::Place::Folder(folder) => {
            state.scan_place = Some(folder.clone());
            // Drive roots are always listed; only deeper folders are worth remembering.
            if folder.parent().is_some() {
                state.scan_folders.retain(|f| f != folder);
                state.scan_folders.insert(0, folder.clone());
                state.scan_folders.truncate(5);
            }
        }
    }
    if let Err(e) = state.save(&app.state_path) {
        app.toasts.show(ui, e);
    }
}

/// The Add games screen: A adds, Left/Right picks another exe, Y ignores or restores, LB
/// chooses where to look, RB looks again, B goes back (out of the place list or the browser
/// first).
fn handle_add_games_action(ui: &AppWindow, app: &mut App, action: Action) {
    let snapshot = |screen: &add_games::AddGames| {
        (
            screen.mode.clone(),
            screen.focus,
            screen.place_focus,
            screen.rows.get(screen.focus).map(|r| r.exe),
        )
    };
    let before = snapshot(&app.add_games);
    let effect = app.add_games.handle(action);
    let after = snapshot(&app.add_games);
    let sound = match &effect {
        Some(add_games::Effect::Add(index)) => {
            add_found_game(ui, app, *index);
            Some(Sound::Confirm)
        }
        Some(add_games::Effect::Ignore(index)) => {
            set_ignored(ui, app, *index, true);
            Some(Sound::Back)
        }
        Some(add_games::Effect::Restore(index)) => {
            set_ignored(ui, app, *index, false);
            Some(Sound::Confirm)
        }
        Some(add_games::Effect::Rescan) => {
            let place = app.add_games.place.clone();
            start_find_games(app, place);
            Some(Sound::Confirm)
        }
        Some(add_games::Effect::ChoosePlace) => {
            let recent = app.home.state.scan_folders.clone();
            app.add_games.open_places(&recent);
            Some(Sound::Confirm)
        }
        Some(add_games::Effect::Scan(place)) => {
            remember_place(ui, app, place);
            start_find_games(app, place.clone());
            Some(Sound::Confirm)
        }
        Some(add_games::Effect::List(dir)) => {
            background::spawn_listing(app.notifier.clone(), dir.clone());
            Some(Sound::Confirm)
        }
        Some(add_games::Effect::Close) => {
            app.menus.overlay = Overlay::Panel;
            push_menus(ui, app);
            Some(Sound::Back)
        }
        None if before.0 != after.0 => Some(if action == Action::Back {
            Sound::Back
        } else {
            Sound::Confirm
        }),
        None if before != after => Some(Sound::Move),
        None => matches!(
            action,
            Action::Up | Action::Down | Action::Left | Action::Right
        )
        .then_some(Sound::Bump),
    };
    if let Some(sound) = sound {
        play_sound(app, sound);
    }
    sync_add_games_screen(ui, app);
}

fn add_found_game(ui: &AppWindow, app: &mut App, index: usize) {
    let row = &app.add_games.rows[index];
    // Ids are "pc:<name>", so a second game with the same name gets a number.
    let base = row.candidate.title.clone();
    let taken = |name: &str| app.home.games.iter().any(|g| g.id == format!("pc:{name}"));
    let mut name = base.clone();
    let mut number = 2;
    while taken(&name) {
        name = format!("{base} ({number})");
        number += 1;
    }
    let exe = row.exe_path().clone();
    app.home.state.added_games.push(config::PcGame {
        name: name.clone(),
        exe,
        args: String::new(),
        icon: None,
        hero: None,
        logo: None,
        xinput: true,
    });
    if let Err(e) = app.home.state.save(&app.state_path) {
        app.home.state.added_games.pop();
        app.toasts.show(ui, e);
        return;
    }
    app.add_games.rows[index].status = add_games::Status::Added;
    if let Err(e) = reload_library(ui, app) {
        app.toasts.show(ui, e);
        return;
    }
    let fetching = app.art_pending.contains(&format!("pc:{name}"));
    app.toasts.show(
        ui,
        if fetching {
            format!("{name} added to your PC games · getting its art")
        } else {
            format!("{name} added to your PC games")
        },
    );
}

fn set_ignored(ui: &AppWindow, app: &mut App, index: usize, ignored: bool) {
    let row = &mut app.add_games.rows[index];
    let folder = row.candidate.folder.clone();
    let title = row.candidate.title.clone();
    if ignored {
        row.status = add_games::Status::Ignored;
        app.home.state.ignored_folders.insert(folder);
        app.toasts.show(
            ui,
            format!("{title} won't be suggested again (Y restores it)"),
        );
    } else {
        row.status = add_games::Status::New;
        app.home.state.ignored_folders.remove(&folder);
    }
    if let Err(e) = app.home.state.save(&app.state_path) {
        app.toasts.show(ui, e);
    }
}

/// Mirrors the Add games screen into the window; leaving the screen stops a running scan.
fn sync_add_games_screen(ui: &AppWindow, app: &mut App) {
    if app.menus.overlay != Overlay::AddGames {
        if app.add_games.scanning {
            app.scan_cancel.store(true, Ordering::Relaxed);
            app.add_games.scanning = false;
        }
        return;
    }
    let screen = &app.add_games;
    let (mode, title, rows, index, status) = match &screen.mode {
        add_games::Mode::Results => (
            0,
            "Add games".to_string(),
            game_rows(screen),
            screen.focus,
            results_status(screen),
        ),
        add_games::Mode::Places => (
            1,
            "Where to look".to_string(),
            place_rows(screen, "Look here"),
            screen.place_focus,
            "Every drive, one drive, or a folder you choose".to_string(),
        ),
        add_games::Mode::Browse(dir) => (
            2,
            "Choose a folder".to_string(),
            place_rows(screen, "Open"),
            screen.place_focus,
            match dir {
                None => "Pick a drive".to_string(),
                Some(dir) if screen.listing => format!(
                    "Opening {}… (a sleeping drive can take a few seconds)",
                    dir.display()
                ),
                Some(dir) => dir.display().to_string(),
            },
        ),
    };
    ui.set_add_mode(mode);
    ui.set_add_title(title.into());
    ui.set_add_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_add_index(index as i32);
    ui.set_add_scanning(screen.scanning);
    ui.set_add_status(status.into());
    ui.set_add_place(format!("Looking in {}", screen.place.describe()).into());
}

fn game_rows(screen: &add_games::AddGames) -> Vec<AddRow> {
    screen
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = i == screen.focus;
            let (icon, tone, action, note) = match row.status {
                // Segoe MDL2: game pad, check mark, crossed-out eye.
                add_games::Status::New => ("\u{E7FC}", 0, "Add to library", ""),
                add_games::Status::Added => ("\u{E8FB}", 1, "", "Added"),
                add_games::Status::Ignored if selected => {
                    ("\u{ED1A}", 2, "", "Ignored · Y to restore")
                }
                add_games::Status::Ignored => ("\u{ED1A}", 2, "", "Ignored"),
            };
            AddRow {
                title: row.candidate.title.as_str().into(),
                detail: row.exe_path().display().to_string().into(),
                icon: icon.into(),
                tone,
                action: action.into(),
                note: note.into(),
                choice: if row.candidate.exes.len() > 1 && row.status == add_games::Status::New {
                    format!("exe {} of {}", row.exe + 1, row.candidate.exes.len()).into()
                } else {
                    SharedString::new()
                },
            }
        })
        .collect()
}

/// `open` labels rows that go deeper (a drive or folder in the browser).
fn place_rows(screen: &add_games::AddGames, open: &str) -> Vec<AddRow> {
    screen
        .places
        .iter()
        .map(|row| {
            let (icon, action) = match row.kind {
                // Segoe MDL2: magnifier, drive, folder.
                add_games::PlaceKind::AllDrives => ("\u{E721}", "Look here"),
                add_games::PlaceKind::LookHere => ("\u{E721}", "Look here"),
                add_games::PlaceKind::Drive => ("\u{EDA2}", open),
                add_games::PlaceKind::Folder => ("\u{E8B7}", open),
                add_games::PlaceKind::Browse => ("\u{E8B7}", "Browse"),
            };
            AddRow {
                title: row.title.as_str().into(),
                detail: row.detail.as_str().into(),
                icon: icon.into(),
                tone: 0,
                action: action.into(),
                note: SharedString::new(),
                choice: SharedString::new(),
            }
        })
        .collect()
}

fn results_status(screen: &add_games::AddGames) -> String {
    if screen.scanning {
        return format!("Looking for games… {} folders checked", screen.folders);
    }
    if screen.rows.is_empty() {
        return "No new games found here. Games already in your library aren't listed.".into();
    }
    let (new, added) = (screen.new_count(), screen.added_count());
    let found = new + added;
    let games = if found == 1 { "game" } else { "games" };
    match (new, added) {
        (0, 0) => "Only ignored games found (Y restores one)".to_string(),
        (0, _) => format!("All {found} added · find them in your PC tab"),
        (_, 0) => format!("{found} new {games} found"),
        _ => format!("{found} new {games} found · {added} added"),
    }
}

/// The Controllers screen from the keyboard or a clicked hint. Pad presses arrive as Live masks
/// instead (see pad_presses), so every pad button can be tested without anything happening.
fn handle_controllers_action(ui: &AppWindow, app: &mut App, action: Action) {
    if app.pad_input {
        return;
    }
    if app.capturing || app.remap_run.is_some() {
        if action == Action::Back {
            cancel_remapping(ui, app);
            sync_controllers_screen(ui, app);
        }
        return;
    }
    let focus = app.controllers_mode == controllers::Mode::Focus;
    let sound = match action {
        Action::Up | Action::Down | Action::Left | Action::Right => {
            app.controllers_mode = controllers::Mode::Focus;
            Some(move_focus(app, action))
        }
        // Enter and Y act on the focused button, which only Focus mode shows: switch to it first.
        Action::Confirm | Action::Options if !focus => {
            app.controllers_mode = controllers::Mode::Focus;
            Some(Sound::Tab)
        }
        Action::TabPrev | Action::TabNext if app.pads.len() > 1 => {
            let delta = if action == Action::TabNext { 1 } else { -1 };
            app.pad_index = nav::wrap_index(app.pad_index, app.pads.len(), delta);
            Some(Sound::Tab)
        }
        Action::Confirm => start_remap_selected(ui, app),
        Action::Options => {
            clear_selected(ui, app);
            Some(Sound::Back)
        }
        Action::Menu => {
            leave_controllers_screen(ui, app);
            return;
        }
        // Leaving needs Esc held for BACK_HOLD, like B on the pad.
        Action::Back if app.back_hold_since.is_none() => {
            app.back_hold_since = Some(std::time::Instant::now());
            let weak = ui.as_weak();
            app.back_hold_timer
                .start(TimerMode::Repeated, Duration::from_millis(30), move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_back_hold_tick();
                    }
                });
            None
        }
        _ => None,
    };
    if let Some(sound) = sound {
        play_sound(app, sound);
    }
    push_menus(ui, app);
    sync_controllers_screen(ui, app);
}

/// Holding Esc on the keyboard to leave the Controllers screen (the pad's B uses hold_tick).
const BACK_HOLD: Duration = Duration::from_secs(2);

fn move_focus(app: &mut App, action: Action) -> Sound {
    let before = app.remap_selected;
    app.remap_selected = controllers::navigate(before, action);
    if before != app.remap_selected {
        Sound::Move
    } else {
        Sound::Bump
    }
}

/// Pad buttons that went down (`pressed`) or up (`released`) on the Controllers screen.
/// Remapping records them; Focus mode moves and acts with them; Test mode only lights them up.
fn pad_presses(ui: &AppWindow, app: &mut App, pressed: u32, released: u32) {
    let buttons = |mask: u32| -> Vec<XButton> {
        (0..controllers::ALL.len())
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| controllers::ALL[i])
            .collect()
    };
    if app.capturing {
        return;
    }
    if let Some(run) = app.remap_run.as_mut() {
        if run.confirm.is_some() {
            if buttons(pressed).contains(&XButton::A) {
                let event = run_and_remap(app).and_then(|(run, remap)| run.use_stick(remap));
                remap_event(ui, app, event);
            } else if buttons(released).contains(&XButton::B) {
                run.retry();
                play_sound(app, Sound::Back);
            }
            return;
        }
        for button in buttons(pressed) {
            run.press(button);
        }
        for button in buttons(released) {
            let event = run_and_remap(app).and_then(|(run, remap)| run.release(button, remap));
            remap_event(ui, app, event);
        }
        return;
    }
    if app.controllers_mode != controllers::Mode::Focus {
        return;
    }
    let mut moved = false;
    for button in buttons(pressed) {
        // A diagonal push crosses two directions at once: it moves one step.
        let direction = matches!(
            button,
            XButton::DpadUp
                | XButton::DpadDown
                | XButton::DpadLeft
                | XButton::DpadRight
                | XButton::LsUp
                | XButton::LsDown
                | XButton::LsLeft
                | XButton::LsRight
                | XButton::RsUp
                | XButton::RsDown
                | XButton::RsLeft
                | XButton::RsRight
        );
        if direction && std::mem::replace(&mut moved, true) {
            continue;
        }
        let sound = match button {
            XButton::DpadUp | XButton::LsUp | XButton::RsUp => Some(move_focus(app, Action::Up)),
            XButton::DpadDown | XButton::LsDown | XButton::RsDown => {
                Some(move_focus(app, Action::Down))
            }
            XButton::DpadLeft | XButton::LsLeft | XButton::RsLeft => {
                Some(move_focus(app, Action::Left))
            }
            XButton::DpadRight | XButton::LsRight | XButton::RsRight => {
                Some(move_focus(app, Action::Right))
            }
            XButton::A => start_remap_selected(ui, app),
            XButton::X => start_capture(ui, app),
            XButton::Y => {
                clear_selected(ui, app);
                let label = controllers::ALL[app.remap_selected].label();
                app.toasts.show(ui, format!("{label}: added keys cleared"));
                Some(Sound::Back)
            }
            _ => None,
        };
        if let Some(sound) = sound {
            play_sound(app, sound);
        }
        // A remap or capture started: later presses in this batch belong to it.
        if app.remap_run.is_some() || app.capturing {
            break;
        }
    }
}

/// The remap in progress and the shown pad's remaps, which it writes to.
fn run_and_remap(
    app: &mut App,
) -> Option<(&mut controllers::RemapRun, &mut BTreeMap<XButton, XButton>)> {
    let pad = app.pads.get(app.pad_index)?.clone();
    let App {
        remap_run, home, ..
    } = app;
    let run = remap_run.as_mut()?;
    let profile = home.state.controller_remaps.entry(pad.guid).or_default();
    profile.name = pad.name;
    Some((run, &mut profile.remap))
}

/// Remaps `targets` one after another, then goes back to `return_to`.
fn start_remap(
    ui: &AppWindow,
    app: &mut App,
    targets: Vec<XButton>,
    return_to: controllers::Mode,
) -> Option<Sound> {
    if app.pads.get(app.pad_index).is_none() {
        app.toasts.show(ui, "Connect a controller first".into());
        return None;
    }
    // Inputs already held (the press that started it) only count once let go and pressed again.
    app.remap_run = Some(controllers::RemapRun::new(targets, return_to));
    Some(Sound::Confirm)
}

fn start_remap_selected(ui: &AppWindow, app: &mut App) -> Option<Sound> {
    let target = controllers::ALL[app.remap_selected];
    let mode = app.controllers_mode;
    start_remap(ui, app, vec![target], mode)
}

fn remap_event(ui: &AppWindow, app: &mut App, event: Option<controllers::RemapEvent>) {
    let Some(event) = event else { return };
    match event {
        controllers::RemapEvent::Assigned { target, source } => {
            save_remaps(ui, app);
            play_sound(app, Sound::Confirm);
            if app
                .remap_run
                .as_ref()
                .is_some_and(|run| run.target().is_none())
            {
                finish_remap(ui, app, Some((target, source)));
            }
        }
        controllers::RemapEvent::AlreadyUsed { source, by } => {
            play_sound(app, Sound::Bump);
            app.toasts.show(
                ui,
                format!(
                    "{} is already used for {} · press another button",
                    source.label(),
                    by.label()
                ),
            );
        }
        controllers::RemapEvent::AskStick { .. } => play_sound(app, Sound::Move),
    }
}

fn finish_remap(ui: &AppWindow, app: &mut App, last: Option<(XButton, XButton)>) {
    let Some(run) = app.remap_run.take() else {
        return;
    };
    app.controllers_mode = run.return_to;
    let text = match last {
        _ if run.targets.len() > 1 => "All buttons remapped · press them to check".to_string(),
        Some((target, source)) if target == source => {
            format!("{} is back on its own button", target.label())
        }
        Some((target, source)) => format!("{} is now on {}", target.label(), source.label()),
        None => "Nothing changed".to_string(),
    };
    app.toasts.show(ui, text);
}

/// Stops the remap in progress, keeping the buttons done so far.
fn stop_remap(ui: &AppWindow, app: &mut App) {
    let Some(run) = app.remap_run.take() else {
        return;
    };
    app.controllers_mode = run.return_to;
    play_sound(app, Sound::Back);
    if run.targets.len() > 1 && run.step > 0 {
        let done = run.step;
        let buttons = if done == 1 { "button" } else { "buttons" };
        app.toasts
            .show(ui, format!("Remap stopped · {done} {buttons} kept"));
    }
}

fn cancel_remapping(ui: &AppWindow, app: &mut App) {
    if app.capturing {
        app.capturing = false;
        input::CAPTURE.store(false, Ordering::Relaxed);
        play_sound(app, Sound::Back);
    }
    stop_remap(ui, app);
}

/// Switches how the Controllers screen reads the pad. Remap all starts a remap of every button.
fn set_controllers_mode(ui: &AppWindow, app: &mut App, mode: controllers::Mode) {
    // The add-key box is waiting for a press: it is closed first (Cancel, Esc, right click).
    if app.capturing {
        return;
    }
    if mode == controllers::Mode::RemapAll {
        if app.remap_run.is_some() {
            return;
        }
        let targets = controllers::remap_all_targets();
        if let Some(sound) = start_remap(ui, app, targets, controllers::Mode::Test) {
            app.controllers_mode = mode;
            play_sound(app, sound);
        }
        return;
    }
    stop_remap(ui, app);
    app.controllers_mode = mode;
    play_sound(app, Sound::Tab);
}

/// The remap panel's and the add-key dialog's buttons (mouse), and Focus mode's X hint.
fn remap_command(ui: &AppWindow, app: &mut App, command: usize) {
    match command {
        0 => {
            if let Some((run, remap)) = run_and_remap(app) {
                run.back(remap);
                save_remaps(ui, app);
                play_sound(app, Sound::Back);
            }
        }
        1 => {
            if let Some((run, remap)) = run_and_remap(app) {
                run.skip(remap);
                let finished = run.target().is_none();
                play_sound(app, Sound::Move);
                if finished {
                    finish_remap(ui, app, None);
                }
            }
        }
        2 => stop_remap(ui, app),
        3 => {
            let event = run_and_remap(app).and_then(|(run, remap)| run.use_stick(remap));
            remap_event(ui, app, event);
        }
        4 => {
            if let Some(run) = app.remap_run.as_mut() {
                run.retry();
            }
        }
        5 => cancel_remapping(ui, app),
        6 => {
            cancel_remapping(ui, app);
            clear_selected(ui, app);
        }
        7 if app.remap_run.is_none() && !app.capturing => {
            if let Some(sound) = start_capture(ui, app) {
                play_sound(app, sound);
            }
        }
        _ => {}
    }
    sync_controllers_screen(ui, app);
}

/// Waits for the next key or pad button, which becomes an extra trigger for the selected row.
fn start_capture(ui: &AppWindow, app: &mut App) -> Option<Sound> {
    if app.pads.get(app.pad_index).is_none() {
        app.toasts.show(ui, "Connect a controller first".into());
        return None;
    }
    app.capturing = true;
    input::CAPTURE.store(true, Ordering::Relaxed);
    Some(Sound::Confirm)
}

fn clear_selected(ui: &AppWindow, app: &mut App) {
    let button = controllers::ALL[app.remap_selected];
    if let Some(pad) = app.pads.get(app.pad_index)
        && let Some(profile) = app.home.state.controller_remaps.get_mut(&pad.guid)
    {
        profile.clear(button);
    }
    save_remaps(ui, app);
}

/// The pad's held buttons changed (Controllers screen): note when each went down.
fn track_holds(ui: &AppWindow, app: &mut App, mask: u32) {
    let now = std::time::Instant::now();
    for i in 0..controllers::ALL.len() {
        if mask & (1 << i) != 0 {
            app.held_since.entry(i).or_insert(now);
        } else {
            app.held_since.remove(&i);
            app.holds_done.remove(&i);
        }
    }
    if app.held_since.is_empty() {
        app.hold_timer.stop();
    } else if !app.hold_timer.running() {
        let weak = ui.as_weak();
        app.hold_timer
            .start(TimerMode::Repeated, Duration::from_millis(30), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_hold_tick();
                }
            });
    }
    hold_tick(ui, app);
}

/// Runs a held button's action once its time is up, and shows how far along the hold is.
fn hold_tick(ui: &AppWindow, app: &mut App) {
    if app.menus.overlay != Overlay::Controllers || app.capturing {
        // Buttons down now (such as the one being captured) start a hold only after a release.
        app.holds_done.extend(app.held_since.keys().copied());
        app.held_since.clear();
        app.hold_timer.stop();
        show_hold(ui, app, None);
        return;
    }
    // The hold closest to its goal is the one shown (and the one that fires).
    let (pads, remapping) = (app.pads.len(), app.remap_run.is_some());
    let leading = app
        .held_since
        .iter()
        .filter(|(i, _)| !app.holds_done.contains(i))
        .filter_map(|(&i, since)| {
            let (action, needed) = controllers::hold_action(i, pads, remapping)?;
            Some((
                i,
                action,
                since.elapsed().as_secs_f32() / needed.as_secs_f32(),
            ))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2));
    let Some((index, action, progress)) = leading else {
        show_hold(ui, app, None);
        return;
    };
    if progress < 1.0 {
        // A quick test press shouldn't flash a progress bar.
        let held = app.held_since[&index].elapsed();
        let visible = held > Duration::from_millis(200);
        show_hold(ui, app, visible.then_some((index, action, progress)));
        return;
    }
    app.holds_done.insert(index);
    show_hold(ui, app, None);
    match action {
        controllers::HoldAction::Leave => leave_controllers_screen(ui, app),
        controllers::HoldAction::StopRemap => stop_remap(ui, app),
        controllers::HoldAction::SwitchMode(delta) => {
            let mode = app.controllers_mode.step(delta);
            set_controllers_mode(ui, app, mode);
        }
        controllers::HoldAction::SwitchPad(delta) => {
            app.pad_index = nav::wrap_index(app.pad_index, app.pads.len(), delta);
            play_sound(app, Sound::Tab);
        }
    }
    push_menus(ui, app);
    sync_controllers_screen(ui, app);
}

/// The hold bar (and B's ring) on the Controllers screen.
fn show_hold(ui: &AppWindow, app: &App, hold: Option<(usize, controllers::HoldAction, f32)>) {
    let Some((index, action, progress)) = hold else {
        ui.set_hold_progress(0.0);
        ui.set_back_progress(0.0);
        return;
    };
    let button = controllers::ALL[index].badge();
    let text = match action {
        controllers::HoldAction::Leave | controllers::HoldAction::StopRemap => {
            ui.set_back_progress(progress.min(0.999));
            ui.set_hold_progress(0.0);
            return;
        }
        controllers::HoldAction::SwitchMode(delta) => format!(
            "Keep holding {button} for {} mode",
            app.controllers_mode.step(delta).label()
        ),
        controllers::HoldAction::SwitchPad(_) => {
            format!("Keep holding {button} to show the other controller")
        }
    };
    ui.set_hold_text(text.into());
    ui.set_hold_progress(progress.clamp(0.01, 1.0));
}

/// Fills the ring around the B badge; leaves the Controllers screen once it is full.
fn back_hold_tick(ui: &AppWindow, app: &mut App) {
    let Some(since) = app.back_hold_since else {
        app.back_hold_timer.stop();
        return;
    };
    let progress = since.elapsed().as_secs_f32() / BACK_HOLD.as_secs_f32();
    if progress < 1.0 {
        // A full circle would have identical start and end points, which draws nothing.
        ui.set_back_progress(progress.min(0.999));
        return;
    }
    leave_controllers_screen(ui, app);
}

fn cancel_back_hold(ui: &AppWindow, app: &mut App) {
    app.back_hold_since = None;
    app.back_hold_timer.stop();
    ui.set_back_progress(0.0);
}

/// Drains Controllers-screen events from the input thread.
fn drain_controller_events(ui: &AppWindow, app: &mut App) {
    while let Ok(event) = app.controller_events.try_recv() {
        match event {
            InputEvent::Pads(pads) => {
                let shown = app.pads.get(app.pad_index).map(|p| p.id);
                app.pads = pads;
                app.pad_index = app.pad_index.min(app.pads.len().saturating_sub(1));
                // The remap belongs to the pad that was shown.
                if app.pads.get(app.pad_index).map(|p| p.id) != shown {
                    stop_remap(ui, app);
                }
            }
            InputEvent::Live { raw, out } => {
                let pressed = raw & !app.live_raw;
                let released = app.live_raw & !raw;
                app.live_raw = raw;
                app.live_out = out;
                track_holds(ui, app, raw);
                pad_presses(ui, app, pressed, released);
            }
            InputEvent::Captured(trigger) if app.capturing => {
                app.capturing = false;
                let button = controllers::ALL[app.remap_selected];
                if let Some(pad) = app.pads.get(app.pad_index).cloned() {
                    let profile = app
                        .home
                        .state
                        .controller_remaps
                        .entry(pad.guid)
                        .or_default();
                    profile.name = pad.name;
                    profile.add(button, trigger.clone());
                    save_remaps(ui, app);
                    app.toasts.show(
                        ui,
                        format!(
                            "{} now also responds to {}",
                            button.label(),
                            trigger.label()
                        ),
                    );
                    play_sound(app, Sound::Confirm);
                }
            }
            InputEvent::CaptureTimedOut => {
                app.capturing = false;
                app.toasts.show(ui, "Nothing pressed: no key added".into());
            }
            _ => {}
        }
    }
    sync_controllers_screen(ui, app);
}

fn save_remaps(ui: &AppWindow, app: &App) {
    if let Ok(mut remaps) = controllers::REMAPS.write() {
        *remaps = app.home.state.controller_remaps.clone();
    }
    if let Err(e) = app.home.state.save(&app.state_path) {
        app.toasts.show(ui, e);
    }
}

/// Streams the shown pad's buttons only while the screen is open, and pushes its contents.
fn sync_controllers_screen(ui: &AppWindow, app: &mut App) {
    let open = app.menus.overlay == Overlay::Controllers;
    if !open && app.capturing {
        app.capturing = false;
        input::CAPTURE.store(false, Ordering::Relaxed);
    }
    let pad = app.pads.get(app.pad_index).filter(|_| open);
    input::LIVE_PAD.store(pad.map_or(0, |p| p.id), Ordering::Relaxed);
    if !open {
        return;
    }
    let title = match (app.pads.get(app.pad_index), app.pads.len()) {
        (None, _) => "No controller connected".to_string(),
        (Some(pad), 1) => pad.name.clone(),
        (Some(pad), n) => format!(
            "{}  ·  {} of {n}  ·  hold LB / RB to switch",
            pad.name,
            app.pad_index + 1
        ),
    };
    ui.set_controllers_title(title.into());
    let profile = app
        .pads
        .get(app.pad_index)
        .and_then(|p| app.home.state.controller_remaps.get(&p.guid));
    let routes = profile.map(|p| controllers::routes(&p.remap));
    let rows: Vec<RemapRow> = controllers::ALL
        .iter()
        .enumerate()
        .map(|(i, button)| {
            let (column, row) = controllers::place(i);
            let mut parts = Vec::new();
            match profile.and_then(|p| p.remap.get(button)) {
                Some(source) => parts.push(format!("Now on {}", source.label())),
                None if routes.is_some_and(|r| r[i] != Some(*button)) => {
                    parts.push("Not on any button".to_string())
                }
                None => {}
            }
            if let Some(extra) = profile
                .and_then(|p| p.extra.get(button))
                .filter(|list| !list.is_empty())
            {
                let keys: Vec<String> = extra.iter().map(|t| t.label()).collect();
                parts.push(format!("+ {}", keys.join(", ")));
            }
            RemapRow {
                badge: button.badge().into(),
                label: button.label().into(),
                value: parts.join("  ·  ").into(),
                column: column as i32,
                row: row as i32,
            }
        })
        .collect();
    ui.set_remap_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_remap_index(app.remap_selected as i32);
    let mode = app.controllers_mode;
    ui.set_controllers_mode(
        controllers::Mode::ALL
            .iter()
            .position(|m| *m == mode)
            .unwrap_or(0) as i32,
    );
    ui.set_multiple_pads(app.pads.len() > 1);
    let run = app.remap_run.as_ref();
    ui.set_remap_highlight(match run {
        Some(run) => run.target().map_or(-1, |t| controllers::index_of(t) as i32),
        None if mode == controllers::Mode::Focus => app.remap_selected as i32,
        None => -1,
    });
    // While remapping, only the button being remapped is lit.
    let shown = if run.is_some() { 0 } else { app.live_out };
    let live: Vec<bool> = (0..controllers::ALL.len())
        .map(|i| shown & (1 << i) != 0)
        .collect();
    ui.set_live(ModelRc::new(VecModel::from(live)));
    ui.set_run_active(run.is_some());
    if let Some(run) = run
        && let Some(target) = run.target()
    {
        let (kind, title, detail) = match run.confirm {
            Some(source) => (
                2,
                format!(
                    "You moved the {}. Use it for {}?",
                    source.label().to_lowercase(),
                    target.label()
                ),
                "A: use it  ·  B: press again  ·  Hold B: stop".to_string(),
            ),
            None if run.targets.len() == 1 => (
                0,
                format!("Press the button that should act as {}", target.label()),
                "Its own button puts it back to normal  ·  Hold B to cancel".to_string(),
            ),
            None => (
                1,
                format!("Press the button for {}", target.label()),
                format!(
                    "{} of {}  ·  Hold B to stop",
                    run.step + 1,
                    run.targets.len()
                ),
            ),
        };
        ui.set_run_kind(kind);
        ui.set_run_title(title.into());
        ui.set_run_detail(detail.into());
        ui.set_run_can_back(run.step > 0);
    }
    ui.set_capturing(app.capturing);
    let selected = controllers::ALL[app.remap_selected];
    ui.set_capture_text(
        format!(
            "Press a key or a pad button that should also press {}",
            selected.label()
        )
        .into(),
    );
    ui.set_capture_can_clear(
        profile
            .and_then(|p| p.extra.get(&selected))
            .is_some_and(|list| !list.is_empty()),
    );
}

fn report(ui: &AppWindow, app: &App, result: Result<(), String>) {
    if let Err(e) = result {
        diag::log(&e);
        app.toasts.show(ui, e);
    }
}

fn apply_volumes(app: &App) {
    let settings = app.home.state.settings;
    if let Some(audio) = &app.audio {
        audio.set_volume(settings.sound_volume);
    }
    if let Some(music) = &app.music {
        music.set_volume(settings.music_volume);
    }
}

fn push_menus(ui: &AppWindow, app: &App) {
    let menus = &app.menus;
    ui.set_overlay(match menus.overlay {
        Overlay::None => 0,
        Overlay::Panel => 1,
        Overlay::Power => 2,
        Overlay::Settings => 3,
        Overlay::Controllers => 4,
        Overlay::AddGames => 5,
        Overlay::Art => 6,
    });
    ui.set_panel_index(menus.panel_focus as i32);
    ui.set_power_index(menus.power_focus as i32);
    ui.set_settings_index(menus.settings_focus as i32);
    let settings = app.home.state.settings;
    // Listing screens costs a few system calls: only while Settings is open.
    let screens = if menus.overlay == Overlay::Settings {
        display::screens()
    } else {
        Vec::new()
    };
    let values: Vec<SharedString> = menus::SETTINGS
        .iter()
        .map(|setting| match setting {
            Setting::SoundVolume => format!("{}%", settings.sound_volume).into(),
            Setting::MusicVolume => format!("{}%", settings.music_volume).into(),
            Setting::LauncherScreen => {
                display::describe(&app.home.state.launcher_screen, &screens).into()
            }
            Setting::GameScreen => display::describe(&app.home.state.game_screen, &screens).into(),
        })
        .collect();
    ui.set_settings_values(ModelRc::new(VecModel::from(values)));
    if menus.is_open() {
        ui.set_date(local_date().into());
    }
}

/// Re-reads config.toml and the emulator libraries without restarting the launcher.
fn rescan(ui: &AppWindow, app: &mut App) {
    if app.home.running.is_some() {
        app.toasts
            .show(ui, "Close the running game before rescanning".into());
        return;
    }
    let warnings = match reload_library(ui, app) {
        Ok(warnings) => warnings,
        Err(e) => {
            diag::log(&e);
            app.toasts.show(ui, e);
            return;
        }
    };
    app.toasts.show(
        ui,
        format!("Library rescanned: {} games", app.home.games.len()),
    );
    if let Some(first) = warnings.first() {
        let more = if warnings.len() > 1 {
            format!(" (+{} more)", warnings.len() - 1)
        } else {
            String::new()
        };
        app.toasts.show(ui, format!("{first}{more}"));
    }
}

/// Takes a game added from the launcher out of the library. Its files stay where they are.
fn remove_game(ui: &AppWindow, app: &mut App, game: usize) {
    let (title, exe) = {
        let game = &app.home.games[game];
        (game.title.clone(), game.launch.exe.clone())
    };
    app.home
        .state
        .added_games
        .retain(|g| !(g.name == title && g.exe == exe));
    if let Err(e) = app.home.state.save(&app.state_path) {
        app.toasts.show(ui, e);
        return;
    }
    match reload_library(ui, app) {
        Ok(_) => app.toasts.show(
            ui,
            format!("{title} removed from the library (its files are untouched)"),
        ),
        Err(e) => app.toasts.show(ui, e),
    }
}

/// Reloads config.toml, the added games and the scanners, keeping the user's place.
/// Returns the warnings for the caller to show (or not).
fn reload_library(ui: &AppWindow, app: &mut App) -> Result<Vec<String>, String> {
    let data_dir = config::data_dir();
    let config = config::load(&data_dir)?;
    // Read before loading: a drive plugged in during the load then still counts as a change.
    let mask = library::drives::mask();
    app.sgdb_key = config
        .art
        .steamgriddb_key
        .clone()
        .filter(|k| !k.trim().is_empty());
    let library = library::load(&config, &app.home.state.added_games, &data_dir);
    for warning in &library.warnings {
        diag::log(warning);
    }
    app.drive_labels = library.drive_labels;
    app.drive_mask = mask;
    app.home.replace_games(library.games);
    push_tabs(ui, app);
    push_selection(ui, app);
    push_scalars(ui, &app.home);
    show_hero(ui, app);
    update_music(app);
    queue_art_lookups(app);
    Ok(library.warnings)
}

fn start_drive_watch(ui: &AppWindow, app_rc: &Rc<RefCell<App>>) {
    let weak = ui.as_weak();
    let app = app_rc.clone();
    app_rc
        .borrow()
        .drive_timer
        .start(TimerMode::Repeated, DRIVE_POLL, move || {
            // A callback already holding the app (e.g. mid-reload) just skips this tick.
            if let (Some(ui), Ok(mut app)) = (weak.upgrade(), app.try_borrow_mut()) {
                check_drives(&ui, &mut app);
                check_unavailable(&mut app);
                let now = std::time::Instant::now();
                let due = app
                    .art_failed
                    .values()
                    .any(|&(tries, next)| tries <= RETRY_DELAYS.len() as u32 && next <= now);
                if due {
                    queue_art_lookups(&mut app);
                }
            }
        });
}

/// While the launcher is visible, greyed-out games' files are checked again in the background
/// (every DRIVE_POLL); games that are back light up without a restart.
fn check_unavailable(app: &mut App) {
    if app.reach_check || app.in_background || app.home.running.is_some() {
        return;
    }
    let games: Vec<(String, Vec<PathBuf>)> = app
        .home
        .games
        .iter()
        .filter(|g| g.missing.is_some() && !app.reach_stuck.contains(&g.id))
        .map(|g| (g.id.clone(), g.requires.clone()))
        .collect();
    if games.is_empty() {
        return;
    }
    app.reach_check = true;
    background::spawn_reach_check(app.notifier.clone(), games);
}

/// A drive was plugged in or out: reload so its games light up or grey out.
fn check_drives(ui: &AppWindow, app: &mut App) {
    let mask = library::drives::mask();
    // Over a running game the reload waits; the next tick after it ends picks the change up.
    if mask == app.drive_mask || app.in_background || app.home.running.is_some() {
        return;
    }
    let before = app.drive_mask;
    let describe = |bits: u32, labels: &BTreeMap<String, String>| -> Vec<String> {
        (0..26u8)
            .filter(|i| bits & (1 << i) != 0)
            .map(|i| {
                let letter = ((b'A' + i) as char).to_string();
                match labels.get(&letter) {
                    Some(label) => format!("{letter}: ({label})"),
                    None => format!("{letter}:"),
                }
            })
            .collect()
    };
    let removed = describe(before & !mask, &app.drive_labels);
    if let Err(e) = reload_library(ui, app) {
        diag::log(&e);
        return;
    }
    let added = describe(mask & !before, &app.drive_labels);
    diag::log(&format!("drives changed: +{added:?} -{removed:?}"));
    for drive in added {
        app.toasts.show(ui, format!("Drive {drive} connected"));
    }
    for drive in removed {
        app.toasts.show(ui, format!("Drive {drive} disconnected"));
    }
}

/// Today's date in the user's long Windows date format.
fn local_date() -> String {
    use windows_sys::Win32::Globalization::{DATE_LONGDATE, GetDateFormatEx};
    let mut buffer = [0u16; 128];
    let len = unsafe {
        GetDateFormatEx(
            std::ptr::null(),
            DATE_LONGDATE,
            std::ptr::null(),
            std::ptr::null(),
            buffer.as_mut_ptr(),
            buffer.len() as i32,
            std::ptr::null(),
        )
    };
    if len <= 1 {
        String::new()
    } else {
        String::from_utf16_lossy(&buffer[..len as usize - 1])
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

    fn nav(focus: usize) -> NavState {
        NavState {
            tab: 0,
            focus,
            zone: Zone::Icons,
            action: 0,
            recent_focus: 0,
            sheet_open: false,
            sheet_focus: 0,
            gear: false,
        }
    }

    #[test]
    fn sounds_follow_what_changed() {
        let moved = sound_for(Action::Right, nav(0), nav(1), &[Effect::Selection]);
        assert_eq!(moved, Some(Sound::Move));
        // Pressing right on the last game changes nothing: a bump, not silence.
        assert_eq!(
            sound_for(Action::Right, nav(3), nav(3), &[]),
            Some(Sound::Bump)
        );
        let tab = NavState { tab: 1, ..nav(0) };
        assert_eq!(
            sound_for(Action::TabNext, nav(0), tab, &[Effect::Tabs]),
            Some(Sound::Tab)
        );
        assert_eq!(
            sound_for(Action::Confirm, nav(0), nav(0), &[Effect::Launch(0)]),
            Some(Sound::Launch)
        );
        let open = NavState {
            sheet_open: true,
            ..nav(0)
        };
        assert_eq!(
            sound_for(Action::Options, nav(0), open, &[]),
            Some(Sound::Confirm)
        );
        assert_eq!(
            sound_for(Action::Back, open, nav(0), &[]),
            Some(Sound::Back)
        );
        // Confirm with nothing to do (e.g. empty tab) stays silent.
        assert_eq!(sound_for(Action::Confirm, nav(0), nav(0), &[]), None);
    }

    #[test]
    fn clock_text_is_not_empty() {
        assert!(!local_time().is_empty());
    }
}
