//! Virtual Xbox 360 controller (through the ViGEmBus driver) so pads that only speak
//! DirectInput work in XInput-only PC games, replacing tools like x360ce. SDL's mapping
//! database already knows each pad's layout, so no manual mapping is needed.

use sdl3_sys::everything::*;
use vigem_client::{Client, TargetId, XButtons, XGamepad, Xbox360Wired};

/// Dropping it unplugs the virtual pad (Xbox360Wired's own Drop does that).
pub struct VirtualPad {
    target: Xbox360Wired<Client>,
    last: XGamepad,
    /// XInput slot Windows gave the virtual pad, once known.
    slot: Option<u32>,
    slot_lookups: u32,
}

/// The slot is assigned shortly after plug-in; give up after about 2 s of polls.
const MAX_SLOT_LOOKUPS: u32 = 250;

impl VirtualPad {
    pub fn connect() -> Result<VirtualPad, String> {
        let client = Client::connect().map_err(|e| match e {
            vigem_client::Error::BusNotFound => "the ViGEmBus driver is not installed".to_string(),
            other => format!("ViGEmBus error: {other}"),
        })?;
        let mut target = Xbox360Wired::new(client, TargetId::XBOX360_WIRED);
        target
            .plugin()
            .map_err(|e| format!("could not plug in the virtual pad: {e}"))?;
        target
            .wait_ready()
            .map_err(|e| format!("virtual pad not ready: {e}"))?;
        Ok(VirtualPad {
            target,
            last: XGamepad::default(),
            slot: None,
            slot_lookups: 0,
        })
    }

    /// The XInput slot (0-3) of the virtual pad. SDL reports Xbox pads' slot as their player
    /// index, which is how the launcher recognises its own pad exactly.
    pub fn xinput_slot(&mut self) -> Option<u32> {
        if self.slot.is_none() && self.slot_lookups < MAX_SLOT_LOOKUPS {
            self.slot_lookups += 1;
            self.slot = self.target.get_user_index().ok();
        }
        self.slot
    }

    /// Sends the pad's state, skipping the driver call when nothing changed.
    pub fn send(&mut self, report: XGamepad) -> Result<(), String> {
        if report == self.last {
            return Ok(());
        }
        match self.target.update(&report) {
            Ok(()) => {
                self.last = report;
                Ok(())
            }
            // ViGEmBus before 1.17 has no "wait until ready" call and answers the first updates
            // after plug-in with ERROR_NO_MORE_ITEMS; `last` stays unchanged, so the next poll retries.
            Err(
                vigem_client::Error::WinError(ERROR_NO_MORE_ITEMS)
                | vigem_client::Error::TargetNotReady,
            ) => Ok(()),
            Err(e) => Err(format!("virtual pad update failed: {e}")),
        }
    }
}

const ERROR_NO_MORE_ITEMS: u32 = 259;

const BUTTONS: [(SDL_GamepadButton, u16); 15] = [
    (SDL_GAMEPAD_BUTTON_SOUTH, XButtons::A),
    (SDL_GAMEPAD_BUTTON_EAST, XButtons::B),
    (SDL_GAMEPAD_BUTTON_WEST, XButtons::X),
    (SDL_GAMEPAD_BUTTON_NORTH, XButtons::Y),
    (SDL_GAMEPAD_BUTTON_BACK, XButtons::BACK),
    (SDL_GAMEPAD_BUTTON_START, XButtons::START),
    (SDL_GAMEPAD_BUTTON_GUIDE, XButtons::GUIDE),
    (SDL_GAMEPAD_BUTTON_LEFT_STICK, XButtons::LTHUMB),
    (SDL_GAMEPAD_BUTTON_RIGHT_STICK, XButtons::RTHUMB),
    (SDL_GAMEPAD_BUTTON_LEFT_SHOULDER, XButtons::LB),
    (SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER, XButtons::RB),
    (SDL_GAMEPAD_BUTTON_DPAD_UP, XButtons::UP),
    (SDL_GAMEPAD_BUTTON_DPAD_DOWN, XButtons::DOWN),
    (SDL_GAMEPAD_BUTTON_DPAD_LEFT, XButtons::LEFT),
    (SDL_GAMEPAD_BUTTON_DPAD_RIGHT, XButtons::RIGHT),
];

/// Builds an XInput report from SDL's view of a pad. Takes readers instead of a pad pointer
/// so the mapping can be unit-tested.
pub fn report(
    button: impl Fn(SDL_GamepadButton) -> bool,
    axis: impl Fn(SDL_GamepadAxis) -> i16,
) -> XGamepad {
    let raw = BUTTONS
        .iter()
        .filter(|(b, _)| button(*b))
        .fold(0, |acc, (_, x)| acc | x);
    XGamepad {
        buttons: XButtons { raw },
        left_trigger: trigger(axis(SDL_GAMEPAD_AXIS_LEFT_TRIGGER)),
        right_trigger: trigger(axis(SDL_GAMEPAD_AXIS_RIGHT_TRIGGER)),
        thumb_lx: axis(SDL_GAMEPAD_AXIS_LEFTX),
        thumb_ly: flip(axis(SDL_GAMEPAD_AXIS_LEFTY)),
        thumb_rx: axis(SDL_GAMEPAD_AXIS_RIGHTX),
        thumb_ry: flip(axis(SDL_GAMEPAD_AXIS_RIGHTY)),
    }
}

/// SDL triggers run 0..=32767; XInput's run 0..=255.
fn trigger(value: i16) -> u8 {
    (value.max(0) >> 7) as u8
}

/// SDL's Y axes point down, XInput's point up. `!v` is `-v - 1`, which maps the full
/// i16 range onto itself without overflowing at -32768.
fn flip(value: i16) -> i16 {
    !value
}

pub fn is_xbox(kind: SDL_GamepadType) -> bool {
    kind == SDL_GAMEPAD_TYPE_XBOX360 || kind == SDL_GAMEPAD_TYPE_XBOXONE
}

fn is_playstation(kind: SDL_GamepadType) -> bool {
    kind == SDL_GAMEPAD_TYPE_PS3 || kind == SDL_GAMEPAD_TYPE_PS4 || kind == SDL_GAMEPAD_TYPE_PS5
}

/// Which pads get a virtual Xbox twin while a game runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Translate {
    Off = 0,
    /// Every pad that isn't already an Xbox pad.
    AllPads = 1,
    /// Leave PS3/PS4/PS5 pads alone for games that support them natively.
    ExceptPlayStation = 2,
}

pub fn should_translate(kind: SDL_GamepadType, mode: Translate) -> bool {
    match mode {
        Translate::Off => false,
        // Games read Xbox pads directly, so translating them would only duplicate them.
        _ if is_xbox(kind) => false,
        Translate::AllPads => true,
        Translate::ExceptPlayStation => !is_playstation(kind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_face_buttons_dpad_and_start() {
        let pressed = [
            SDL_GAMEPAD_BUTTON_SOUTH,
            SDL_GAMEPAD_BUTTON_DPAD_UP,
            SDL_GAMEPAD_BUTTON_START,
        ];
        let r = report(|b| pressed.contains(&b), |_| 0);
        assert_eq!(r.buttons.raw, XButtons::A | XButtons::UP | XButtons::START);
    }

    #[test]
    fn sticks_flip_y_and_triggers_scale() {
        let r = report(
            |_| false,
            |a| match a {
                SDL_GAMEPAD_AXIS_LEFTX => 1000,
                SDL_GAMEPAD_AXIS_LEFTY => -32768, // stick fully up in SDL
                SDL_GAMEPAD_AXIS_RIGHTY => 32767, // stick fully down in SDL
                SDL_GAMEPAD_AXIS_LEFT_TRIGGER => 32767,
                SDL_GAMEPAD_AXIS_RIGHT_TRIGGER => 0,
                _ => 0,
            },
        );
        assert_eq!(r.thumb_lx, 1000);
        assert_eq!(r.thumb_ly, 32767, "fully up must be +max in XInput");
        assert_eq!(r.thumb_ry, -32768, "fully down must be -max in XInput");
        assert_eq!(r.left_trigger, 255);
        assert_eq!(r.right_trigger, 0);
    }

    #[test]
    fn resting_stick_stays_near_center() {
        assert_eq!(flip(0), -1);
        assert_eq!(trigger(-5), 0);
    }

    #[test]
    fn translation_rules_by_pad_type_and_mode() {
        use Translate::*;
        // Xbox pads are never translated.
        assert!(!should_translate(SDL_GAMEPAD_TYPE_XBOXONE, AllPads));
        // Generic pads (like the UCom) are translated unless translation is off.
        assert!(should_translate(SDL_GAMEPAD_TYPE_STANDARD, AllPads));
        assert!(should_translate(
            SDL_GAMEPAD_TYPE_STANDARD,
            ExceptPlayStation
        ));
        assert!(!should_translate(SDL_GAMEPAD_TYPE_STANDARD, Off));
        // PlayStation pads only when the game doesn't support them natively.
        assert!(should_translate(SDL_GAMEPAD_TYPE_PS5, AllPads));
        assert!(!should_translate(SDL_GAMEPAD_TYPE_PS4, ExceptPlayStation));
    }

    #[test]
    fn only_xbox_types_count_as_xbox() {
        assert!(is_xbox(SDL_GAMEPAD_TYPE_XBOX360));
        assert!(is_xbox(SDL_GAMEPAD_TYPE_XBOXONE));
        assert!(!is_xbox(SDL_GAMEPAD_TYPE_STANDARD));
    }

    /// A stand-in pad for trying the Controllers screen without a real one: plugs in a virtual
    /// Xbox pad and plays the script at `PAD_SCRIPT`, one step per line: inputs joined by `+`
    /// (or `none`) and how long to hold them in ms, e.g. `rt 1300` or `a+lsright 200`.
    /// `$env:PAD_SCRIPT = "..."; cargo test test_pad -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn test_pad() {
        let script = std::fs::read_to_string(std::env::var("PAD_SCRIPT").unwrap()).unwrap();
        let mut pad = VirtualPad::connect().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        for line in script.lines().filter(|l| !l.trim().is_empty()) {
            let (inputs, ms) = line.trim().rsplit_once(' ').unwrap();
            let mut report = XGamepad::default();
            for input in inputs.split('+') {
                match input {
                    "none" => {}
                    "a" => report.buttons.raw |= XButtons::A,
                    "b" => report.buttons.raw |= XButtons::B,
                    "x" => report.buttons.raw |= XButtons::X,
                    "y" => report.buttons.raw |= XButtons::Y,
                    "lb" => report.buttons.raw |= XButtons::LB,
                    "rb" => report.buttons.raw |= XButtons::RB,
                    "start" => report.buttons.raw |= XButtons::START,
                    "back" => report.buttons.raw |= XButtons::BACK,
                    "up" => report.buttons.raw |= XButtons::UP,
                    "down" => report.buttons.raw |= XButtons::DOWN,
                    "left" => report.buttons.raw |= XButtons::LEFT,
                    "right" => report.buttons.raw |= XButtons::RIGHT,
                    "lt" => report.left_trigger = 255,
                    "rt" => report.right_trigger = 255,
                    "lsright" => report.thumb_lx = 30_000,
                    "lsup" => report.thumb_ly = 30_000,
                    other => panic!("unknown input {other}"),
                }
            }
            println!("{line}");
            pad.send(report).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(ms.parse().unwrap()));
        }
        pad.send(XGamepad::default()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
