// Release builds run without a console window; debug builds keep it for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod controllers;
mod diag;
mod emulation;
mod home;
mod input;
mod launcher;
mod library;
mod menus;
mod music;
mod nav;
mod scaled_art;
mod state;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slint::{Color, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

use audio::{Audio, Sound};
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
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let data_dir = config::data_dir();
    // Created here so even config errors are logged; the folder may not exist on first run.
    let _ = std::fs::create_dir_all(&data_dir);
    diag::init(&data_dir);
    let mut problems = Vec::new();
    let state_path = data_dir.join("state.json");
    let (state, state_problem) = State::load(&state_path);
    problems.extend(state_problem);
    let drive_mask = library::drives::mask();
    let library = match config::load(&data_dir) {
        Ok(config) => library::load(&config, &state.added_games, &data_dir),
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
            // While a game has the screen, pads are only listened to for the home combo.
            if app.borrow().in_background {
                return;
            }
            if std::time::Instant::now() < app.borrow().input_ready_at {
                diag::log(&format!("ignored {action:?} during the input grace period"));
                return;
            }
            if action == Action::Menu || app.borrow().menus.is_open() {
                handle_menu_action(&ui, &mut app.borrow_mut(), action);
                return;
            }
            let before = app.borrow().home.nav_state();
            let effects = app.borrow_mut().home.handle(action);
            let after = app.borrow().home.nav_state();
            if let Some(sound) = sound_for(action, before, after, &effects) {
                play_sound(&app.borrow(), sound);
            }
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
            | InputEvent::Live(_)
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
                Some(InputEvent::Action(action)) => ui.invoke_action(action),
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
    slint::run_event_loop_until_quit()
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
                        start_game(ui, &mut app, *game);
                    }
                    Some(missing) => {
                        let message = missing.play_message(&app.home.games[*game].title);
                        app.home.games[*game].missing = Some(missing);
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

fn start_game(ui: &AppWindow, app: &mut App, game: usize) {
    if let Some(running) = app.home.running {
        let title = app.home.games[running].title.clone();
        app.toasts
            .show(ui, format!("Close {title} first (⋯ → Close game)"));
        return;
    }
    // Plugged in before the game starts so it finds the virtual pad when it enumerates pads.
    input::set_translation(translation_for(&app.home, game));
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
            app.toasts.show(ui, e);
        }
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
    input::set_translation(Translate::Off);
    // A sheet opened on the running game lists Resume / Close game, which no longer apply.
    app.home.sheet = None;
    if app.in_background {
        come_back(ui, app);
    } else {
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
    match window_handle(ui) {
        Some(hwnd) => {
            let in_front = launcher::bring_to_front(hwnd);
            diag::log(&format!("launcher shown; in foreground: {in_front}"));
        }
        None => diag::log("launcher shown, but no window handle to bring to the front"),
    }
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

/// Quick panel, Power and Settings: navigation, then whatever the chosen item does.
fn handle_menu_action(ui: &AppWindow, app: &mut App, action: Action) {
    if app.menus.overlay == Overlay::Controllers && action != Action::Menu {
        handle_controllers_action(ui, app, action);
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
}

/// The Controllers screen: move through the button lists, A to add a key/button, Y to clear.
fn handle_controllers_action(ui: &AppWindow, app: &mut App, action: Action) {
    // While waiting, the input thread delivers the next key/button; actions are ignored so the
    // press that is being captured doesn't also navigate.
    if app.capturing {
        return;
    }
    let before = (app.remap_selected, app.pad_index);
    let sound = match action {
        Action::Up | Action::Down | Action::Left | Action::Right => {
            app.remap_selected = controllers::navigate(app.remap_selected, action);
            Some(if before.0 != app.remap_selected {
                Sound::Move
            } else {
                Sound::Bump
            })
        }
        Action::TabPrev | Action::TabNext if app.pads.len() > 1 => {
            let delta = if action == Action::TabNext { 1 } else { -1 };
            app.pad_index = nav::wrap_index(app.pad_index, app.pads.len(), delta);
            Some(Sound::Tab)
        }
        Action::Confirm if app.pads.get(app.pad_index).is_some() => {
            app.capturing = true;
            input::CAPTURE.store(true, Ordering::Relaxed);
            Some(Sound::Confirm)
        }
        Action::Confirm => {
            app.toasts.show(ui, "Connect a controller first".into());
            None
        }
        Action::Options => {
            let button = controllers::ALL[app.remap_selected];
            if let Some(pad) = app.pads.get(app.pad_index)
                && let Some(profile) = app.home.state.controller_remaps.get_mut(&pad.guid)
            {
                profile.clear(button);
            }
            save_remaps(ui, app);
            Some(Sound::Back)
        }
        // Leaving needs B held for BACK_HOLD, so a stray press doesn't throw away the screen.
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

const BACK_HOLD: Duration = Duration::from_secs(3);

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
    cancel_back_hold(ui, app);
    app.menus.overlay = Overlay::Panel;
    play_sound(app, Sound::Back);
    push_menus(ui, app);
    sync_controllers_screen(ui, app);
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
                app.pads = pads;
                app.pad_index = app.pad_index.min(app.pads.len().saturating_sub(1));
            }
            InputEvent::Live(mask) => {
                let live: Vec<bool> = (0..controllers::ALL.len())
                    .map(|i| mask & (1 << i) != 0)
                    .collect();
                ui.set_live(ModelRc::new(VecModel::from(live)));
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
                app.toasts
                    .show(ui, "Nothing pressed: no trigger added".into());
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
            "{}  ·  {} of {n}  ·  LB / RB to switch",
            pad.name,
            app.pad_index + 1
        ),
    };
    ui.set_controllers_title(title.into());
    let profile = app
        .pads
        .get(app.pad_index)
        .and_then(|p| app.home.state.controller_remaps.get(&p.guid));
    let rows: Vec<RemapRow> = controllers::ALL
        .iter()
        .enumerate()
        .map(|(i, button)| {
            let (column, row) = controllers::place(i);
            let extra = profile
                .and_then(|p| p.extra.get(button))
                .filter(|list| !list.is_empty());
            RemapRow {
                badge: button.badge().into(),
                label: button.label().into(),
                value: extra
                    .map(|list| {
                        list.iter()
                            .map(|t| t.label())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
                    .into(),
                column: column as i32,
                row: row as i32,
            }
        })
        .collect();
    ui.set_remap_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_remap_index(app.remap_selected as i32);
    ui.set_capturing(app.capturing);
    ui.set_capture_text(
        format!(
            "Press a key or a pad button that should also act as {}",
            controllers::ALL[app.remap_selected].label()
        )
        .into(),
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
    });
    ui.set_panel_index(menus.panel_focus as i32);
    ui.set_power_index(menus.power_focus as i32);
    ui.set_settings_index(menus.settings_focus as i32);
    let settings = app.home.state.settings;
    let values: Vec<SharedString> = menus::SETTINGS
        .iter()
        .map(|setting| match setting {
            Setting::SoundVolume => format!("{}%", settings.sound_volume).into(),
            Setting::MusicVolume => format!("{}%", settings.music_volume).into(),
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
    let library = library::load(&config, &app.home.state.added_games, &data_dir);
    for warning in &library.warnings {
        diag::log(warning);
    }
    app.drive_labels = library.drive_labels;
    app.drive_mask = library::drives::mask();
    app.home.replace_games(library.games);
    push_tabs(ui, app);
    push_selection(ui, app);
    push_scalars(ui, &app.home);
    show_hero(ui, app);
    update_music(app);
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
            }
        });
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
