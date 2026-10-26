//! Controller input on its own SDL3 thread. It never touches the window, so it keeps
//! running (and can watch for the return-to-launcher combo) while a game has focus.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use sdl3_sys::everything::*;

use crate::Action;
use crate::controllers::{self, Direction, REMAPS, Stick, Trigger, XButton};
use crate::diag;
use crate::emulation::{self, Translate, VirtualPad};

pub enum InputEvent {
    Action(Action),
    Notice(String),
    /// Start + Select held: bring the launcher back over (or send it behind) a running game.
    HomeCombo,
    /// The connected controllers changed (for the Controllers screen).
    Pads(Vec<PadInfo>),
    /// The pad shown in the Controllers screen (bit i = controllers::ALL[i]): the inputs held
    /// on it, and the buttons a game would see from them after remaps.
    Live {
        raw: u32,
        out: u32,
    },
    /// LB (-1) or RB (+1) pressed on a pad the Controllers screen isn't showing: show the
    /// previous or next pad (any pad can switch, not only the one shown).
    OtherPadSwitch(isize),
    /// The key or button pressed while the Controllers screen was waiting for one.
    Captured(Trigger),
    /// Nothing was pressed in time; the screen stops waiting.
    CaptureTimedOut,
    /// B (Back) was let go: ends a hold-to-exit in the Controllers screen.
    BackReleased,
}

#[derive(Clone, Debug)]
pub struct PadInfo {
    pub id: u32,
    pub name: String,
    /// SDL's GUID string: the same for every pad of one model, used to key its remaps.
    pub guid: String,
    /// Wireless pads that report it; None for wired ones (and pads that don't say).
    pub battery: Option<Battery>,
    /// Tells this physical pad apart (for the player order).
    pub key: String,
    rank: (usize, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// How often battery levels are read again.
const BATTERY_CHECK: Duration = Duration::from_secs(30);

/// Player order chosen on the Controllers screen (pad keys, player 1 first). Pads not in it
/// come after, in connection order.
static PAD_ORDER: std::sync::LazyLock<std::sync::RwLock<Vec<String>>> =
    std::sync::LazyLock::new(Default::default);

pub fn set_pad_order(order: Vec<String>) {
    if let Ok(mut current) = PAD_ORDER.write() {
        *current = order;
    }
}

/// Tells one physical pad from another of the same model: its device path (stable while it
/// stays on the same port or pairing), else its serial number, else its name.
fn pad_key(pad: *mut SDL_Gamepad) -> String {
    let path = cstr_or_empty(unsafe { SDL_GetGamepadPath(pad) });
    if !path.is_empty() {
        return path;
    }
    let name = cstr(unsafe { SDL_GetGamepadName(pad) });
    let serial = cstr_or_empty(unsafe { SDL_GetGamepadSerial(pad) });
    if serial.is_empty() {
        name
    } else {
        format!("{name} #{serial}")
    }
}

/// Where a pad sorts: its place in the chosen order, then connection order (SDL ids grow).
fn player_rank(pad: *mut SDL_Gamepad, id: u32) -> (usize, u32) {
    let key = pad_key(pad);
    let place = PAD_ORDER
        .read()
        .ok()
        .and_then(|order| order.iter().position(|k| *k == key))
        .unwrap_or(usize::MAX);
    (place, id)
}

fn battery(pad: *mut SDL_Gamepad) -> Option<Battery> {
    let mut percent: std::ffi::c_int = -1;
    let state = unsafe { SDL_GetGamepadPowerInfo(pad, &mut percent) };
    let charging = match state {
        SDL_POWERSTATE_ON_BATTERY => false,
        SDL_POWERSTATE_CHARGING => true,
        SDL_POWERSTATE_CHARGED => {
            percent = percent.max(100);
            true
        }
        _ => return None,
    };
    (percent >= 0).then(|| Battery {
        percent: percent.min(100) as u8,
        charging,
    })
}

/// SDL id of the pad shown in the Controllers screen (0 = screen closed): its buttons are streamed live.
pub static LIVE_PAD: AtomicU32 = AtomicU32::new(0);
/// Set while the Controllers screen waits for a key or button to add as an extra trigger.
pub static CAPTURE: AtomicBool = AtomicBool::new(false);
const TRIGGER_DOWN: i16 = 16_000;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(6);

/// Set while a game runs: the pads are then only watched for the home combo, so polling slows down.
pub static IN_GAME: AtomicBool = AtomicBool::new(false);
/// Which pads get a virtual Xbox twin while the current game runs (a `Translate` as u8).
static TRANSLATION: AtomicU8 = AtomicU8::new(0);

pub fn set_translation(mode: Translate) {
    TRANSLATION.store(mode as u8, Ordering::Relaxed);
}

fn translation() -> Translate {
    match TRANSLATION.load(Ordering::Relaxed) {
        1 => Translate::AllPads,
        2 => Translate::ExceptPlayStation,
        _ => Translate::Off,
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(8);
const IN_GAME_POLL_INTERVAL: Duration = Duration::from_millis(33);
const COMBO_HOLD: Duration = Duration::from_secs(1);
const REPEAT_DELAY: Duration = Duration::from_millis(380);
const REPEAT_INTERVAL: Duration = Duration::from_millis(110);
// Separate press/release thresholds stop a stick resting near the edge from chattering.
const STICK_PRESS: i32 = 16_000;
const STICK_RELEASE: i32 = 9_000;
const TRIGGER_PRESS: i32 = 16_000;
const TRIGGER_RELEASE: i32 = 8_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Button(u32, u8),
    Stick(u32),
    Hat(u32),
    Trigger(u32, u8),
}

impl Source {
    fn device(self) -> u32 {
        match self {
            Source::Button(id, _)
            | Source::Stick(id)
            | Source::Hat(id)
            | Source::Trigger(id, _) => id,
        }
    }
}

struct Held {
    action: Action,
    source: Source,
    next: Instant,
}

pub fn spawn(emit: impl Fn(InputEvent) + Send + 'static) {
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || run(emit))
        .expect("failed to start input thread");
}

fn run(emit: impl Fn(InputEvent)) {
    unsafe {
        SDL_SetHint(SDL_HINT_JOYSTICK_ALLOW_BACKGROUND_EVENTS, c"1".as_ptr());
        // We create no SDL window, so device hotplug needs SDL's own notification thread.
        SDL_SetHint(SDL_HINT_JOYSTICK_THREAD, c"1".as_ptr());
        if !SDL_Init(SDL_INIT_GAMEPAD) {
            emit(InputEvent::Notice(format!(
                "Controller support failed to start: {}",
                sdl_error()
            )));
            return;
        }
    }
    load_mapping_file(&emit);

    let mut pads = Pads {
        emit,
        held: None,
        sticks: HashMap::new(),
        triggers: HashSet::new(),
        gamepads: HashMap::new(),
        joysticks: HashMap::new(),
        combos: HashMap::new(),
        held_at_open: HashMap::new(),
        virtual_pads: HashMap::new(),
        emulation_failed: false,
        own_pads: HashSet::new(),
        last_emulation_error: String::new(),
        guids: HashMap::new(),
        live_mask: (0, 0),
        capture_keys: None,
        capture_mask: 0,
        capture_started: Instant::now(),
        shoulders: HashMap::new(),
        batteries_read: Instant::now(),
        batteries: Vec::new(),
    };
    let mut event = SDL_Event::default();
    // SDL_WaitEvent re-polls open pads every ~1 ms, which costs real CPU with chatty
    // Bluetooth controllers. Polling on our own fixed interval is ~8x cheaper.
    loop {
        while unsafe { SDL_PollEvent(&mut event) } {
            pads.handle(&event);
        }
        pads.repeat_if_due();
        pads.update_emulation();
        pads.update_live_and_capture();
        pads.update_batteries();
        // Gameplay through the virtual pad needs full-rate polling; otherwise a game only
        // needs the home combo watched.
        let interval = if IN_GAME.load(Ordering::Relaxed) && pads.virtual_pads.is_empty() {
            IN_GAME_POLL_INTERVAL
        } else {
            POLL_INTERVAL
        };
        std::thread::sleep(interval);
    }
}

/// Optional community mappings (gamecontrollerdb.txt next to the exe) for pads SDL doesn't know.
fn load_mapping_file(emit: &impl Fn(InputEvent)) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let path = exe.with_file_name("gamecontrollerdb.txt");
    if !path.exists() {
        return;
    }
    let c_path = CString::new(path.to_string_lossy().as_bytes()).expect("path contains NUL");
    if unsafe { SDL_AddGamepadMappingsFromFile(c_path.as_ptr()) } < 0 {
        emit(InputEvent::Notice(format!(
            "gamecontrollerdb.txt could not be loaded: {}",
            sdl_error()
        )));
    }
}

struct Pads<F: Fn(InputEvent)> {
    emit: F,
    held: Option<Held>,
    sticks: HashMap<u32, (i32, i32)>,
    gamepads: HashMap<u32, *mut SDL_Gamepad>,
    /// Triggers currently past the press threshold, keyed by (device, axis).
    triggers: HashSet<(u32, u8)>,
    /// Devices SDL has no gamepad mapping for, read as raw joysticks.
    joysticks: HashMap<u32, *mut SDL_Joystick>,
    /// Per device: Select and Start held state, when both went down, and whether it already fired.
    combos: HashMap<u32, Combo>,
    /// Per pad: buttons already held when it was opened. They are ignored until released once,
    /// so a button reported "down" at connection can't act as a fresh press.
    held_at_open: HashMap<u32, HashSet<u8>>,
    /// Virtual Xbox pads keyed by the SDL id of the controller each one mirrors.
    virtual_pads: HashMap<u32, VirtualPad>,
    /// Set after a failed attempt so it isn't retried every poll; cleared when emulation stops.
    emulation_failed: bool,
    /// The launcher's own virtual pad as SDL sees it. Never opened, so its input is ignored
    /// (otherwise every press, including the home combo, would arrive twice).
    own_pads: HashSet<u32>,
    last_emulation_error: String,
    /// SDL GUID string per opened gamepad, to find its remap profile.
    guids: HashMap<u32, String>,
    /// The last Live event sent: (pad inputs held, buttons the game sees).
    live_mask: (u32, u32),
    /// Keys already held when capture started (only new presses count).
    capture_keys: Option<HashSet<u16>>,
    /// Buttons already held when capture started.
    capture_mask: u32,
    capture_started: Instant,
    /// LB / RB held on each pad not shown in the Controllers screen.
    shoulders: HashMap<u32, (bool, bool)>,
    /// When battery levels were last read, and what was reported.
    batteries_read: Instant,
    batteries: Vec<(u32, Option<Battery>)>,
}

#[derive(Default)]
struct Combo {
    select: bool,
    start: bool,
    since: Option<Instant>,
    fired: bool,
}

impl<F: Fn(InputEvent)> Pads<F> {
    fn handle(&mut self, event: &SDL_Event) {
        let kind = SDL_EventType(unsafe { event.r#type });
        match kind {
            SDL_EVENT_GAMEPAD_ADDED => {
                let id = unsafe { event.gdevice.which };
                if self.is_own_virtual_pad(id) {
                    self.own_pads.insert(id.0);
                    diag::log(&format!(
                        "pad {} is the launcher's own virtual Xbox pad; ignored",
                        id.0
                    ));
                    return;
                }
                let pad = unsafe { SDL_OpenGamepad(id) };
                if pad.is_null() {
                    self.notice(format!(
                        "A controller was detected but could not be opened: {}",
                        sdl_error()
                    ));
                } else {
                    self.gamepads.insert(id.0, pad);
                    let held: HashSet<u8> = (0..SDL_GamepadButton::COUNT.0)
                        .filter(|&b| unsafe { SDL_GetGamepadButton(pad, SDL_GamepadButton(b)) })
                        .map(|b| b as u8)
                        .collect();
                    if !held.is_empty() {
                        diag::log(&format!(
                            "pad {} had buttons held at connection: {held:?}",
                            id.0
                        ));
                    }
                    self.held_at_open.insert(id.0, held);
                    self.guids
                        .insert(id.0, guid_string(unsafe { SDL_GetGamepadGUIDForID(id) }));
                    let name = cstr(unsafe { SDL_GetGamepadName(pad) });
                    let mapping = unsafe { SDL_GetGamepadMapping(pad) };
                    diag::log(&format!(
                        "pad {} connected as gamepad: {name} | mapping: {}",
                        id.0,
                        cstr(mapping)
                    ));
                    unsafe { SDL_free(mapping.cast()) };
                    // Our virtual pad can be opened before its slot is known; claim_own_pad then
                    // closes it, so don't announce Xbox pads appearing while emulation runs.
                    let xbox = emulation::is_xbox(unsafe { SDL_GetGamepadType(pad) });
                    if !(xbox && !self.virtual_pads.is_empty()) {
                        self.notice(format!("{name} connected"));
                    }
                    self.emit_pads();
                }
            }
            SDL_EVENT_GAMEPAD_REMOVED => {
                let id = unsafe { event.gdevice.which }.0;
                if self.own_pads.remove(&id) {
                    return;
                }
                if let Some(pad) = self.gamepads.remove(&id) {
                    unsafe { SDL_CloseGamepad(pad) };
                    self.forget_device(id);
                    self.guids.remove(&id);
                    self.notice("Controller disconnected".into());
                    self.emit_pads();
                }
            }
            SDL_EVENT_GAMEPAD_BUTTON_DOWN | SDL_EVENT_GAMEPAD_BUTTON_UP => {
                let b = unsafe { event.gbutton };
                if let Some(held) = self.held_at_open.get_mut(&b.which.0)
                    && held.contains(&b.button)
                {
                    if !b.down {
                        held.remove(&b.button);
                    }
                    return;
                }
                let button = SDL_GamepadButton(b.button as i32);
                // During a game only the home combo matters, so log presses then to trace it.
                if IN_GAME.load(Ordering::Relaxed) {
                    let name = cstr(unsafe { SDL_GetGamepadStringForButton(button) });
                    diag::log(&format!(
                        "in game: pad {} button {name} {}",
                        b.which.0,
                        if b.down { "down" } else { "up" }
                    ));
                }
                if button == SDL_GAMEPAD_BUTTON_BACK || button == SDL_GAMEPAD_BUTTON_START {
                    self.combo_button(b.which.0, button == SDL_GAMEPAD_BUTTON_START, b.down);
                }
                if let Some(action) = gamepad_button_action(b.button) {
                    self.button(Source::Button(b.which.0, b.button), action, b.down);
                }
            }
            SDL_EVENT_GAMEPAD_AXIS_MOTION => {
                let a = unsafe { event.gaxis };
                match a.axis as i32 {
                    x if x == SDL_GAMEPAD_AXIS_LEFTX.0 => {
                        self.stick(a.which.0, Some(a.value as i32), None)
                    }
                    y if y == SDL_GAMEPAD_AXIS_LEFTY.0 => {
                        self.stick(a.which.0, None, Some(a.value as i32))
                    }
                    t if t == SDL_GAMEPAD_AXIS_LEFT_TRIGGER.0 => {
                        self.trigger(a.which.0, a.axis, a.value as i32, Action::PagePrev)
                    }
                    t if t == SDL_GAMEPAD_AXIS_RIGHT_TRIGGER.0 => {
                        self.trigger(a.which.0, a.axis, a.value as i32, Action::PageNext)
                    }
                    _ => {}
                }
            }
            SDL_EVENT_JOYSTICK_ADDED => {
                let id = unsafe { event.jdevice.which };
                // Mapped pads also raise JOYSTICK_ADDED; those are handled as gamepads above.
                if unsafe { SDL_IsGamepad(id) } {
                    return;
                }
                let joy = unsafe { SDL_OpenJoystick(id) };
                if joy.is_null() {
                    self.notice(format!(
                        "A controller was detected but could not be opened: {}",
                        sdl_error()
                    ));
                } else {
                    self.joysticks.insert(id.0, joy);
                    let name = cstr(unsafe { SDL_GetJoystickName(joy) });
                    diag::log(&format!("pad {} connected in generic mode: {name}", id.0));
                    self.notice(format!("{name} connected (generic mode)"));
                }
            }
            SDL_EVENT_JOYSTICK_REMOVED => {
                let id = unsafe { event.jdevice.which }.0;
                if let Some(joy) = self.joysticks.remove(&id) {
                    unsafe { SDL_CloseJoystick(joy) };
                    self.forget_device(id);
                    self.notice("Controller disconnected".into());
                }
            }
            SDL_EVENT_JOYSTICK_HAT_MOTION => {
                let h = unsafe { event.jhat };
                if self.joysticks.contains_key(&h.which.0) {
                    self.hat(h.which.0, h.value);
                }
            }
            SDL_EVENT_JOYSTICK_AXIS_MOTION => {
                let a = unsafe { event.jaxis };
                if self.joysticks.contains_key(&a.which.0) {
                    match a.axis {
                        0 => self.stick(a.which.0, Some(a.value as i32), None),
                        1 => self.stick(a.which.0, None, Some(a.value as i32)),
                        _ => {}
                    }
                }
            }
            SDL_EVENT_JOYSTICK_BUTTON_DOWN | SDL_EVENT_JOYSTICK_BUTTON_UP => {
                let b = unsafe { event.jbutton };
                if IN_GAME.load(Ordering::Relaxed) && self.joysticks.contains_key(&b.which.0) {
                    diag::log(&format!(
                        "in game: generic pad {} button {} {}",
                        b.which.0,
                        b.button,
                        if b.down { "down" } else { "up" }
                    ));
                }
                // Generic pads usually number Select 8 and Start 9.
                if self.joysticks.contains_key(&b.which.0) && (b.button == 8 || b.button == 9) {
                    self.combo_button(b.which.0, b.button == 9, b.down);
                }
                if self.joysticks.contains_key(&b.which.0)
                    && let Some(action) = generic_button_action(b.button)
                {
                    self.button(Source::Button(b.which.0, b.button), action, b.down);
                }
            }
            _ => {}
        }
    }

    fn notice(&self, text: String) {
        (self.emit)(InputEvent::Notice(text));
    }

    fn button(&mut self, source: Source, action: Action, down: bool) {
        if down {
            self.press(source, action);
        } else {
            self.release(source);
            if action == Action::Back {
                (self.emit)(InputEvent::BackReleased);
            }
        }
    }

    fn trigger(&mut self, id: u32, axis: u8, value: i32, action: Action) {
        let pressed = self.triggers.contains(&(id, axis));
        if !pressed && value >= TRIGGER_PRESS {
            self.triggers.insert((id, axis));
            self.press(Source::Trigger(id, axis), action);
        } else if pressed && value < TRIGGER_RELEASE {
            self.triggers.remove(&(id, axis));
            self.release(Source::Trigger(id, axis));
        }
    }

    fn press(&mut self, source: Source, action: Action) {
        (self.emit)(InputEvent::Action(action));
        if repeats(action) {
            self.held = Some(Held {
                action,
                source,
                next: Instant::now() + REPEAT_DELAY,
            });
        }
    }

    fn release(&mut self, source: Source) {
        if self.held.as_ref().is_some_and(|h| h.source == source) {
            self.held = None;
        }
    }

    fn forget_device(&mut self, id: u32) {
        if self.held.as_ref().is_some_and(|h| h.source.device() == id) {
            self.held = None;
        }
        self.sticks.remove(&id);
        self.combos.remove(&id);
        self.held_at_open.remove(&id);
        self.triggers.retain(|&(device, _)| device != id);
    }

    fn stick(&mut self, id: u32, x: Option<i32>, y: Option<i32>) {
        let axes = self.sticks.entry(id).or_insert((0, 0));
        if let Some(x) = x {
            axes.0 = x;
        }
        if let Some(y) = y {
            axes.1 = y;
        }
        let (x, y) = *axes;
        let source = Source::Stick(id);
        let active = self
            .held
            .as_ref()
            .filter(|h| h.source == source)
            .map(|h| h.action);
        let threshold = if active.is_some() {
            STICK_RELEASE
        } else {
            STICK_PRESS
        };
        match (active, stick_direction(x, y, threshold)) {
            (Some(current), Some(dir)) if current == dir => {}
            (_, Some(dir)) => self.press(source, dir),
            (Some(_), None) => self.release(source),
            (None, None) => {}
        }
    }

    fn hat(&mut self, id: u32, value: u8) {
        let source = Source::Hat(id);
        let dir = if value & SDL_HAT_UP != 0 {
            Some(Action::Up)
        } else if value & SDL_HAT_DOWN != 0 {
            Some(Action::Down)
        } else if value & SDL_HAT_LEFT != 0 {
            Some(Action::Left)
        } else if value & SDL_HAT_RIGHT != 0 {
            Some(Action::Right)
        } else {
            None
        };
        let active = self
            .held
            .as_ref()
            .filter(|h| h.source == source)
            .map(|h| h.action);
        match (active, dir) {
            (Some(current), Some(dir)) if current == dir => {}
            (_, Some(dir)) => self.press(source, dir),
            (_, None) => self.release(source),
        }
    }

    /// One virtual Xbox pad per translated controller. A controller disconnecting unplugs its
    /// twin, so the game sees a real disconnect (and can pause); reconnecting plugs a new one in.
    fn update_emulation(&mut self) {
        let mode = translation();
        if mode == Translate::Off {
            if !self.virtual_pads.is_empty() {
                self.virtual_pads.clear();
                diag::log("virtual Xbox pads unplugged");
            }
            self.emulation_failed = false;
            return;
        }
        let mut sources: Vec<(u32, *mut SDL_Gamepad)> = self
            .gamepads
            .iter()
            .filter(|(id, _)| !self.own_pads.contains(id))
            .filter(|&(_, &pad)| {
                emulation::should_translate(unsafe { SDL_GetGamepadType(pad) }, mode)
            })
            .map(|(&id, &pad)| (id, pad))
            .collect();
        // Virtual pads are plugged in this order, so player 1 is the first chosen on the
        // Controllers screen, then the others in connection order.
        sources.sort_by_key(|&(id, pad)| player_rank(pad, id));

        let before = self.virtual_pads.len();
        self.virtual_pads
            .retain(|id, _| sources.iter().any(|(source, _)| source == id));
        if self.virtual_pads.len() < before {
            diag::log("controller gone: its virtual Xbox pad was unplugged");
        }
        for &(id, _) in &sources {
            if self.virtual_pads.contains_key(&id) || self.emulation_failed {
                continue;
            }
            match VirtualPad::connect() {
                Ok(pad) => {
                    self.virtual_pads.insert(id, pad);
                    diag::log(&format!("virtual Xbox pad plugged in for pad {id}"));
                }
                Err(e) => {
                    self.emulation_failed = true;
                    diag::log(&format!("Xbox controller emulation unavailable: {e}"));
                    self.notice(format!("Xbox controller emulation unavailable: {e}"));
                }
            }
        }
        self.claim_own_pads();

        // While the launcher is on screen over the game, the game gets neutral pads so menu
        // navigation doesn't also move things in the game.
        let playing = IN_GAME.load(Ordering::Relaxed);
        for (id, source) in sources {
            let Some(virtual_pad) = self.virtual_pads.get_mut(&id) else {
                continue;
            };
            let report = if playing {
                let mut report = emulation::report(
                    |button| unsafe { SDL_GetGamepadButton(source, button) },
                    |axis| unsafe { SDL_GetGamepadAxis(source, axis) },
                );
                if let Some(guid) = self.guids.get(&id) {
                    apply_remaps(&mut report, guid, source);
                }
                report
            } else {
                emulation::report(|_| false, |_| 0)
            };
            if let Err(e) = virtual_pad.send(report)
                && e != self.last_emulation_error
            {
                diag::log(&e);
                self.last_emulation_error = e;
            }
        }
    }

    fn own_slots(&mut self) -> Vec<i32> {
        self.virtual_pads
            .values_mut()
            .filter_map(VirtualPad::xinput_slot)
            .map(|slot| slot as i32)
            .collect()
    }

    /// Our virtual pads are the Xbox pads in the XInput slots ViGEm gave them.
    fn is_own_virtual_pad(&mut self, id: SDL_JoystickID) -> bool {
        emulation::is_xbox(unsafe { SDL_GetGamepadTypeForID(id) })
            && self
                .own_slots()
                .contains(&unsafe { SDL_GetGamepadPlayerIndexForID(id) })
    }

    /// Closes any of our virtual pads that SDL reported before their slot was known.
    fn claim_own_pads(&mut self) {
        let slots = self.own_slots();
        if slots.is_empty() {
            return;
        }
        let own: Vec<u32> = self
            .gamepads
            .iter()
            .filter(|&(_, &pad)| unsafe {
                emulation::is_xbox(SDL_GetGamepadType(pad))
                    && slots.contains(&SDL_GetGamepadPlayerIndex(pad))
            })
            .map(|(&id, _)| id)
            .collect();
        for id in own {
            if let Some(pad) = self.gamepads.remove(&id) {
                unsafe { SDL_CloseGamepad(pad) };
            }
            self.forget_device(id);
            self.own_pads.insert(id);
            self.guids.remove(&id);
            self.emit_pads();
            diag::log(&format!(
                "pad {id} is one of the launcher's own virtual Xbox pads; closed"
            ));
        }
    }

    fn emit_pads(&mut self) {
        let mut pads: Vec<PadInfo> = self
            .gamepads
            .iter()
            .filter(|(id, _)| !self.own_pads.contains(id))
            .map(|(&id, &pad)| PadInfo {
                id,
                name: cstr(unsafe { SDL_GetGamepadName(pad) }),
                guid: self.guids.get(&id).cloned().unwrap_or_default(),
                battery: battery(pad),
                key: pad_key(pad),
                rank: player_rank(pad, id),
            })
            .collect();
        pads.sort_by_key(|pad| pad.rank);
        self.batteries = pads.iter().map(|p| (p.id, p.battery)).collect();
        self.batteries_read = Instant::now();
        (self.emit)(InputEvent::Pads(pads));
    }

    /// Battery levels change slowly: read every BATTERY_CHECK, sent only when one changed.
    fn update_batteries(&mut self) {
        if self.batteries_read.elapsed() < BATTERY_CHECK {
            return;
        }
        self.batteries_read = Instant::now();
        let mut now: Vec<(u32, Option<Battery>)> = self
            .gamepads
            .iter()
            .filter(|(id, _)| !self.own_pads.contains(id))
            .map(|(&id, &pad)| (id, battery(pad)))
            .collect();
        let rank = |id: &u32| {
            self.gamepads
                .get(id)
                .map_or((usize::MAX, *id), |&pad| player_rank(pad, *id))
        };
        now.sort_by_key(|(id, _)| rank(id));
        if now != self.batteries {
            self.emit_pads();
        }
    }

    /// LB / RB pressed on a pad the Controllers screen isn't showing.
    fn watch_other_pads(&mut self, live_id: u32) {
        if live_id == 0 {
            self.shoulders.clear();
            return;
        }
        for (&id, &pad) in &self.gamepads {
            if self.own_pads.contains(&id) {
                continue;
            }
            let held = |button| unsafe { SDL_GetGamepadButton(pad, button) };
            let now = (
                held(SDL_GAMEPAD_BUTTON_LEFT_SHOULDER),
                held(SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER),
            );
            // The pad shown is tracked too, so a hold that just switched away from it isn't
            // taken for a fresh press once it becomes "another pad".
            let before = self.shoulders.insert(id, now).unwrap_or(now);
            if id == live_id {
                continue;
            }
            if now.0 && !before.0 {
                (self.emit)(InputEvent::OtherPadSwitch(-1));
            } else if now.1 && !before.1 {
                (self.emit)(InputEvent::OtherPadSwitch(1));
            }
        }
    }

    /// Streams the Controllers screen pad's buttons and, while capturing, reports the first
    /// newly pressed key or button.
    fn update_live_and_capture(&mut self) {
        let live_id = LIVE_PAD.load(Ordering::Relaxed);
        self.watch_other_pads(live_id);
        let mask = self.gamepads.get(&live_id).map_or(0, |&pad| held_mask(pad));
        let out = self
            .guids
            .get(&live_id)
            .and_then(|guid| {
                let remaps = REMAPS.read().ok()?;
                let profile = remaps.get(guid)?;
                Some(controllers::output_mask(profile, mask, key_down))
            })
            .unwrap_or(mask);
        if (mask, out) != self.live_mask {
            self.live_mask = (mask, out);
            (self.emit)(InputEvent::Live { raw: mask, out });
        }
        if !CAPTURE.load(Ordering::Relaxed) {
            self.capture_keys = None;
            return;
        }
        let Some(baseline) = &self.capture_keys else {
            self.capture_keys = Some(held_keys());
            self.capture_mask = mask;
            self.capture_started = Instant::now();
            return;
        };
        if self.capture_started.elapsed() > CAPTURE_TIMEOUT {
            CAPTURE.store(false, Ordering::Relaxed);
            self.capture_keys = None;
            (self.emit)(InputEvent::CaptureTimedOut);
            return;
        }
        let new_button = mask & !self.capture_mask;
        let captured = if new_button != 0 {
            Some(Trigger::Button {
                button: controllers::ALL[new_button.trailing_zeros() as usize],
            })
        } else {
            pressed_keys()
                .into_iter()
                .find(|vk| !baseline.contains(vk))
                .map(|vk| Trigger::Key {
                    vk,
                    name: controllers::key_name(vk),
                })
        };
        // Releasing a key held at the start makes it eligible again.
        let still_held = held_keys();
        if let Some(baseline) = self.capture_keys.as_mut() {
            baseline.retain(|vk| still_held.contains(vk));
        }
        self.capture_mask &= mask;
        if let Some(trigger) = captured {
            CAPTURE.store(false, Ordering::Relaxed);
            self.capture_keys = None;
            (self.emit)(InputEvent::Captured(trigger));
        }
    }

    fn combo_button(&mut self, id: u32, is_start: bool, down: bool) {
        diag::log(&format!(
            "combo: pad {id} {} {}",
            if is_start { "start" } else { "select" },
            if down { "down" } else { "up" }
        ));
        let combo = self.combos.entry(id).or_default();
        if is_start {
            combo.start = down;
        } else {
            combo.select = down;
        }
        if combo.start && combo.select {
            combo.since.get_or_insert_with(Instant::now);
        } else {
            combo.since = None;
            combo.fired = false;
        }
    }

    fn repeat_if_due(&mut self) {
        let now = Instant::now();
        for combo in self.combos.values_mut() {
            if !combo.fired
                && combo
                    .since
                    .is_some_and(|since| now.duration_since(since) >= COMBO_HOLD)
            {
                combo.fired = true;
                diag::log("combo: held 1 s, sending home combo");
                (self.emit)(InputEvent::HomeCombo);
            }
        }
        if let Some(held) = self.held.as_mut()
            && now >= held.next
        {
            (self.emit)(InputEvent::Action(held.action));
            held.next = now + REPEAT_INTERVAL;
        }
    }
}

fn gamepad_button_action(button: u8) -> Option<Action> {
    let b = SDL_GamepadButton(button as i32);
    Some(match b {
        SDL_GAMEPAD_BUTTON_SOUTH => Action::Confirm,
        SDL_GAMEPAD_BUTTON_EAST => Action::Back,
        SDL_GAMEPAD_BUTTON_NORTH => Action::Options,
        SDL_GAMEPAD_BUTTON_LEFT_SHOULDER => Action::TabPrev,
        SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER => Action::TabNext,
        SDL_GAMEPAD_BUTTON_START => Action::Menu,
        SDL_GAMEPAD_BUTTON_DPAD_UP => Action::Up,
        SDL_GAMEPAD_BUTTON_DPAD_DOWN => Action::Down,
        SDL_GAMEPAD_BUTTON_DPAD_LEFT => Action::Left,
        SDL_GAMEPAD_BUTTON_DPAD_RIGHT => Action::Right,
        _ => return None,
    })
}

/// Best guess for unmapped DirectInput pads; button numbering varies by vendor.
/// The remapping screen (step 6) replaces this.
fn generic_button_action(button: u8) -> Option<Action> {
    Some(match button {
        0 => Action::Confirm,
        1 => Action::Back,
        2 => Action::Options,
        4 => Action::TabPrev,
        5 => Action::TabNext,
        6 => Action::PagePrev,
        7 => Action::PageNext,
        9 => Action::Menu,
        _ => return None,
    })
}

/// Held directions and page jumps auto-repeat; everything else fires once per press.
fn repeats(action: Action) -> bool {
    matches!(
        action,
        Action::Up
            | Action::Down
            | Action::Left
            | Action::Right
            | Action::PagePrev
            | Action::PageNext
    )
}

fn stick_direction(x: i32, y: i32, threshold: i32) -> Option<Action> {
    if x.abs() >= y.abs() {
        if x >= threshold {
            Some(Action::Right)
        } else if x <= -threshold {
            Some(Action::Left)
        } else {
            None
        }
    } else if y >= threshold {
        Some(Action::Down)
    } else if y <= -threshold {
        Some(Action::Up)
    } else {
        None
    }
}

/// Bit i set when controllers::ALL[i] is held on `pad` (triggers count past half travel).
fn held_mask(pad: *mut SDL_Gamepad) -> u32 {
    controllers::ALL
        .iter()
        .enumerate()
        .fold(0, |mask, (i, button)| {
            let held = match (
                button.sdl_button(),
                button.trigger_axis(),
                button.stick_direction(),
            ) {
                (Some(b), _, _) => unsafe { SDL_GetGamepadButton(pad, b) },
                (_, Some(axis), _) => (unsafe { SDL_GetGamepadAxis(pad, axis) }) > TRIGGER_DOWN,
                (_, _, Some((stick, direction))) => {
                    let (x_axis, y_axis) = match stick {
                        Stick::Left => (SDL_GAMEPAD_AXIS_LEFTX, SDL_GAMEPAD_AXIS_LEFTY),
                        Stick::Right => (SDL_GAMEPAD_AXIS_RIGHTX, SDL_GAMEPAD_AXIS_RIGHTY),
                    };
                    let (x, y) = unsafe {
                        (
                            SDL_GetGamepadAxis(pad, x_axis),
                            SDL_GetGamepadAxis(pad, y_axis),
                        )
                    };
                    controllers::stick_pushed(x, y, direction)
                }
                _ => false,
            };
            if held { mask | (1 << i) } else { mask }
        })
}

/// Keyboard keys currently down. Mouse buttons and the generic Shift/Ctrl/Alt codes are skipped
/// (their left/right variants are reported instead).
fn held_keys() -> HashSet<u16> {
    keys_matching(|state| state < 0)
}

/// Keys held now or tapped since the last check. Windows' "pressed since last call" bit catches
/// taps shorter than the poll interval; it is shared between apps, so it only ever adds presses.
fn pressed_keys() -> HashSet<u16> {
    keys_matching(|state| state < 0 || state & 1 != 0)
}

fn keys_matching(pressed: impl Fn(i16) -> bool) -> HashSet<u16> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    (0x08u16..=0xFE)
        .filter(|vk| !matches!(vk, 0x10..=0x12))
        .filter(|&vk| pressed(unsafe { GetAsyncKeyState(vk as i32) }))
        .collect()
}

fn key_down(vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    (unsafe { GetAsyncKeyState(vk as i32) }) < 0
}

/// Applies the pad model's remaps to `report`: replacements first (inputs moved to other
/// buttons), then extra triggers (keyboard keys or other pad buttons that also press one).
/// Inputs left in place keep their analog values.
fn apply_remaps(report: &mut vigem_client::XGamepad, guid: &str, source: *mut SDL_Gamepad) {
    let Ok(remaps) = REMAPS.read() else { return };
    let Some(profile) = remaps.get(guid) else {
        return;
    };
    let source_held = held_mask(source);
    let held = |button: XButton| source_held & (1 << controllers::index_of(button)) != 0;
    if !profile.remap.is_empty() {
        let routes = controllers::routes(&profile.remap);
        // Buttons no longer pressed by their own input lose what that input put in the report.
        for (i, button) in controllers::ALL.iter().enumerate() {
            if routes[i] != Some(*button) {
                release(report, *button);
            }
        }
        for (i, button) in controllers::ALL.iter().enumerate() {
            if let Some(target) = routes[i]
                && target != *button
                && held(*button)
            {
                press(report, target);
            }
        }
    }
    for (button, triggers) in &profile.extra {
        let active = triggers.iter().any(|trigger| match trigger {
            Trigger::Key { vk, .. } => key_down(*vk),
            Trigger::Button { button } => held(*button),
        });
        if active {
            press(report, *button);
        }
    }
}

/// Presses `button` fully: sticks to full deflection, triggers all the way.
fn press(report: &mut vigem_client::XGamepad, button: XButton) {
    if let Some(flag) = button.xinput_flag() {
        report.buttons.raw |= flag;
    } else if let Some((stick, direction)) = button.stick_direction() {
        // XInput's y axis points up.
        let (x, y) = match stick {
            Stick::Left => (&mut report.thumb_lx, &mut report.thumb_ly),
            Stick::Right => (&mut report.thumb_rx, &mut report.thumb_ry),
        };
        match direction {
            Direction::Up => *y = i16::MAX,
            Direction::Down => *y = i16::MIN,
            Direction::Left => *x = i16::MIN,
            Direction::Right => *x = i16::MAX,
        }
    } else if button == XButton::Lt {
        report.left_trigger = u8::MAX;
    } else {
        report.right_trigger = u8::MAX;
    }
}

/// Takes `button` out of the report: a stick direction clears only its half of the axis.
fn release(report: &mut vigem_client::XGamepad, button: XButton) {
    if let Some(flag) = button.xinput_flag() {
        report.buttons.raw &= !flag;
    } else if let Some((stick, direction)) = button.stick_direction() {
        let (x, y) = match stick {
            Stick::Left => (&mut report.thumb_lx, &mut report.thumb_ly),
            Stick::Right => (&mut report.thumb_rx, &mut report.thumb_ry),
        };
        match direction {
            Direction::Up if *y > 0 => *y = 0,
            Direction::Down if *y < 0 => *y = 0,
            Direction::Left if *x < 0 => *x = 0,
            Direction::Right if *x > 0 => *x = 0,
            _ => {}
        }
    } else if button == XButton::Lt {
        report.left_trigger = 0;
    } else {
        report.right_trigger = 0;
    }
}

fn guid_string(guid: SDL_GUID) -> String {
    let mut buffer = [0 as c_char; 33];
    unsafe { SDL_GUIDToString(guid, buffer.as_mut_ptr(), buffer.len() as i32) };
    cstr(buffer.as_ptr())
}

fn sdl_error() -> String {
    cstr(SDL_GetError())
}

fn cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return "Unknown controller".into();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// Like `cstr`, but empty when SDL has nothing to say.
fn cstr_or_empty(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    cstr(ptr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_picks_dominant_axis() {
        assert!(stick_direction(20_000, 5_000, STICK_PRESS) == Some(Action::Right));
        assert!(stick_direction(-3_000, -25_000, STICK_PRESS) == Some(Action::Up));
        assert!(stick_direction(0, 30_000, STICK_PRESS) == Some(Action::Down));
    }

    #[test]
    fn stick_uses_hysteresis() {
        // 12k is past the release threshold but short of the press threshold.
        assert!(stick_direction(12_000, 0, STICK_PRESS).is_none());
        assert!(stick_direction(12_000, 0, STICK_RELEASE) == Some(Action::Right));
    }
}
