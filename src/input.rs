//! Controller input on its own SDL3 thread. It never touches the window, so it keeps
//! running (and can watch for the return-to-launcher combo) while a game has focus.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::time::{Duration, Instant};

use sdl3_sys::everything::*;

use crate::Action;

pub enum InputEvent {
    Action(Action),
    Notice(String),
}

const POLL_INTERVAL: Duration = Duration::from_millis(8);
const REPEAT_DELAY: Duration = Duration::from_millis(380);
const REPEAT_INTERVAL: Duration = Duration::from_millis(110);
// Separate press/release thresholds stop a stick resting near the edge from chattering.
const STICK_PRESS: i32 = 16_000;
const STICK_RELEASE: i32 = 9_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Button(u32, u8),
    Stick(u32),
    Hat(u32),
}

impl Source {
    fn device(self) -> u32 {
        match self {
            Source::Button(id, _) | Source::Stick(id) | Source::Hat(id) => id,
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
        gamepads: HashMap::new(),
        joysticks: HashMap::new(),
    };
    let mut event = SDL_Event::default();
    // SDL_WaitEvent re-polls open pads every ~1 ms, which costs real CPU with chatty
    // Bluetooth controllers. Polling on our own fixed interval is ~8x cheaper.
    loop {
        while unsafe { SDL_PollEvent(&mut event) } {
            pads.handle(&event);
        }
        pads.repeat_if_due();
        std::thread::sleep(POLL_INTERVAL);
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
    /// Devices SDL has no gamepad mapping for, read as raw joysticks.
    joysticks: HashMap<u32, *mut SDL_Joystick>,
}

impl<F: Fn(InputEvent)> Pads<F> {
    fn handle(&mut self, event: &SDL_Event) {
        let kind = SDL_EventType(unsafe { event.r#type });
        match kind {
            SDL_EVENT_GAMEPAD_ADDED => {
                let id = unsafe { event.gdevice.which };
                let pad = unsafe { SDL_OpenGamepad(id) };
                if pad.is_null() {
                    self.notice(format!(
                        "A controller was detected but could not be opened: {}",
                        sdl_error()
                    ));
                } else {
                    self.gamepads.insert(id.0, pad);
                    let name = cstr(unsafe { SDL_GetGamepadName(pad) });
                    self.notice(format!("{name} connected"));
                }
            }
            SDL_EVENT_GAMEPAD_REMOVED => {
                let id = unsafe { event.gdevice.which }.0;
                if let Some(pad) = self.gamepads.remove(&id) {
                    unsafe { SDL_CloseGamepad(pad) };
                    self.forget_device(id);
                    self.notice("Controller disconnected".into());
                }
            }
            SDL_EVENT_GAMEPAD_BUTTON_DOWN | SDL_EVENT_GAMEPAD_BUTTON_UP => {
                let b = unsafe { event.gbutton };
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
        }
    }

    fn press(&mut self, source: Source, action: Action) {
        (self.emit)(InputEvent::Action(action));
        if is_direction(action) {
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

    fn repeat_if_due(&mut self) {
        let now = Instant::now();
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
        9 => Action::Menu,
        _ => return None,
    })
}

fn is_direction(action: Action) -> bool {
    matches!(
        action,
        Action::Up | Action::Down | Action::Left | Action::Right
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
