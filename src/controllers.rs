//! The Controllers screen: the Xbox-style buttons the virtual pad exposes, their places in the
//! three lists around the controller picture (left column, D-pad box, right column), and the
//! extra triggers ("keyboard Insert also presses L3") saved per controller model.

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, RwLock};

use sdl3_sys::everything::*;
use serde::{Deserialize, Serialize};
use vigem_client::XButtons;

use crate::Action;

/// Inputs in screen order: left column, then the D-pad box, then the right column. Stick
/// directions count as buttons here so keys can push a stick (WASD for the left stick).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XButton {
    Lt,
    Lb,
    Back,
    Start,
    Guide,
    L3,
    LsUp,
    LsLeft,
    LsRight,
    LsDown,
    DpadUp,
    DpadLeft,
    DpadRight,
    DpadDown,
    Rt,
    Rb,
    Y,
    X,
    B,
    A,
    R3,
    RsUp,
    RsLeft,
    RsRight,
    RsDown,
}

pub const ALL: [XButton; 25] = [
    XButton::Lt,
    XButton::Lb,
    XButton::Back,
    XButton::Start,
    XButton::Guide,
    XButton::L3,
    XButton::LsUp,
    XButton::LsLeft,
    XButton::LsRight,
    XButton::LsDown,
    XButton::DpadUp,
    XButton::DpadLeft,
    XButton::DpadRight,
    XButton::DpadDown,
    XButton::Rt,
    XButton::Rb,
    XButton::Y,
    XButton::X,
    XButton::B,
    XButton::A,
    XButton::R3,
    XButton::RsUp,
    XButton::RsLeft,
    XButton::RsRight,
    XButton::RsDown,
];

/// Indices into ALL for each list around the picture.
const COLUMNS: [std::ops::Range<usize>; 3] = [0..10, 10..14, 14..25];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stick {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Left,
    Right,
    Down,
}

impl XButton {
    pub fn label(self) -> &'static str {
        match self {
            XButton::Lt => "Left trigger",
            XButton::Lb => "Left bumper",
            XButton::Back => "Back / Select",
            XButton::Start => "Start",
            XButton::Guide => "Guide",
            XButton::L3 => "Left stick click (L3)",
            XButton::LsUp => "Left stick up",
            XButton::LsLeft => "Left stick left",
            XButton::LsRight => "Left stick right",
            XButton::LsDown => "Left stick down",
            XButton::DpadUp => "D-pad up",
            XButton::DpadLeft => "D-pad left",
            XButton::DpadRight => "D-pad right",
            XButton::DpadDown => "D-pad down",
            XButton::Rt => "Right trigger",
            XButton::Rb => "Right bumper",
            XButton::Y => "Y",
            XButton::X => "X",
            XButton::B => "B",
            XButton::A => "A",
            XButton::R3 => "Right stick click (R3)",
            XButton::RsUp => "Right stick up",
            XButton::RsLeft => "Right stick left",
            XButton::RsRight => "Right stick right",
            XButton::RsDown => "Right stick down",
        }
    }

    /// Short name for the badge at the start of each row.
    pub fn badge(self) -> &'static str {
        match self {
            XButton::Lt => "LT",
            XButton::Lb => "LB",
            XButton::Back => "BK",
            XButton::Start => "ST",
            XButton::Guide => "G",
            XButton::L3 => "L3",
            XButton::LsUp | XButton::RsUp | XButton::DpadUp => "↑",
            XButton::LsLeft | XButton::RsLeft | XButton::DpadLeft => "←",
            XButton::LsRight | XButton::RsRight | XButton::DpadRight => "→",
            XButton::LsDown | XButton::RsDown | XButton::DpadDown => "↓",
            XButton::Rt => "RT",
            XButton::Rb => "RB",
            XButton::Y => "Y",
            XButton::X => "X",
            XButton::B => "B",
            XButton::A => "A",
            XButton::R3 => "R3",
        }
    }

    /// XInput button flag; triggers and stick directions are analog and handled separately.
    pub fn xinput_flag(self) -> Option<u16> {
        Some(match self {
            XButton::Lb => XButtons::LB,
            XButton::Back => XButtons::BACK,
            XButton::Start => XButtons::START,
            XButton::Guide => XButtons::GUIDE,
            XButton::L3 => XButtons::LTHUMB,
            XButton::DpadUp => XButtons::UP,
            XButton::DpadLeft => XButtons::LEFT,
            XButton::DpadRight => XButtons::RIGHT,
            XButton::DpadDown => XButtons::DOWN,
            XButton::Rb => XButtons::RB,
            XButton::Y => XButtons::Y,
            XButton::X => XButtons::X,
            XButton::B => XButtons::B,
            XButton::A => XButtons::A,
            XButton::R3 => XButtons::RTHUMB,
            _ => return None,
        })
    }

    /// How SDL reports this button on the user's pad (triggers and sticks are axes).
    pub fn sdl_button(self) -> Option<SDL_GamepadButton> {
        Some(match self {
            XButton::Lb => SDL_GAMEPAD_BUTTON_LEFT_SHOULDER,
            XButton::Back => SDL_GAMEPAD_BUTTON_BACK,
            XButton::Start => SDL_GAMEPAD_BUTTON_START,
            XButton::Guide => SDL_GAMEPAD_BUTTON_GUIDE,
            XButton::L3 => SDL_GAMEPAD_BUTTON_LEFT_STICK,
            XButton::DpadUp => SDL_GAMEPAD_BUTTON_DPAD_UP,
            XButton::DpadLeft => SDL_GAMEPAD_BUTTON_DPAD_LEFT,
            XButton::DpadRight => SDL_GAMEPAD_BUTTON_DPAD_RIGHT,
            XButton::DpadDown => SDL_GAMEPAD_BUTTON_DPAD_DOWN,
            XButton::Rb => SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER,
            XButton::Y => SDL_GAMEPAD_BUTTON_NORTH,
            XButton::X => SDL_GAMEPAD_BUTTON_WEST,
            XButton::B => SDL_GAMEPAD_BUTTON_EAST,
            XButton::A => SDL_GAMEPAD_BUTTON_SOUTH,
            XButton::R3 => SDL_GAMEPAD_BUTTON_RIGHT_STICK,
            _ => return None,
        })
    }

    pub fn trigger_axis(self) -> Option<SDL_GamepadAxis> {
        match self {
            XButton::Lt => Some(SDL_GAMEPAD_AXIS_LEFT_TRIGGER),
            XButton::Rt => Some(SDL_GAMEPAD_AXIS_RIGHT_TRIGGER),
            _ => None,
        }
    }

    pub fn stick_direction(self) -> Option<(Stick, Direction)> {
        Some(match self {
            XButton::LsUp => (Stick::Left, Direction::Up),
            XButton::LsLeft => (Stick::Left, Direction::Left),
            XButton::LsRight => (Stick::Left, Direction::Right),
            XButton::LsDown => (Stick::Left, Direction::Down),
            XButton::RsUp => (Stick::Right, Direction::Up),
            XButton::RsLeft => (Stick::Right, Direction::Left),
            XButton::RsRight => (Stick::Right, Direction::Right),
            XButton::RsDown => (Stick::Right, Direction::Down),
            _ => return None,
        })
    }
}

/// Is a stick pushed in `direction`, given SDL's axis values (y points down in SDL)?
pub fn stick_pushed(x: i16, y: i16, direction: Direction) -> bool {
    const PUSHED: i16 = 16_000;
    match direction {
        Direction::Up => y < -PUSHED,
        Direction::Down => y > PUSHED,
        Direction::Left => x < -PUSHED,
        Direction::Right => x > PUSHED,
    }
}

/// Something that also presses a button: a keyboard key or another button on the same pad.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Trigger {
    Key { vk: u16, name: String },
    Button { button: XButton },
}

impl Trigger {
    pub fn label(&self) -> String {
        match self {
            Trigger::Key { name, .. } => format!("Key {name}"),
            Trigger::Button { button } => format!("Pad {}", button.label()),
        }
    }
}

/// Extra triggers for one controller model, keyed by SDL's GUID for that model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub extra: BTreeMap<XButton, Vec<Trigger>>,
}

/// A button can have at most this many extra triggers; older ones drop off.
pub const MAX_TRIGGERS: usize = 3;

impl Profile {
    pub fn add(&mut self, button: XButton, trigger: Trigger) {
        let list = self.extra.entry(button).or_default();
        list.retain(|t| t != &trigger);
        list.push(trigger);
        if list.len() > MAX_TRIGGERS {
            list.remove(0);
        }
    }

    pub fn clear(&mut self, button: XButton) {
        self.extra.remove(&button);
    }
}

/// Remaps shared with the input thread, which applies them to the virtual Xbox pads.
pub static REMAPS: LazyLock<RwLock<HashMap<String, Profile>>> = LazyLock::new(Default::default);

/// Cursor movement through the three lists in all four directions.
pub fn navigate(selected: usize, action: Action) -> usize {
    let column = COLUMNS
        .iter()
        .position(|c| c.contains(&selected))
        .unwrap_or(0);
    let row = selected - COLUMNS[column].start;
    let move_to = |column: usize| {
        let range = &COLUMNS[column];
        range.start + row.min(range.len() - 1)
    };
    match action {
        Action::Up if row > 0 => selected - 1,
        Action::Down if row + 1 < COLUMNS[column].len() => selected + 1,
        Action::Left if column > 0 => move_to(column - 1),
        Action::Right if column + 1 < COLUMNS.len() => move_to(column + 1),
        _ => selected,
    }
}

/// Which list a button sits in (0 left, 1 D-pad box, 2 right) and its row there.
pub fn place(index: usize) -> (usize, usize) {
    let column = COLUMNS.iter().position(|c| c.contains(&index)).unwrap_or(0);
    (column, index - COLUMNS[column].start)
}

/// Windows' own name for a key ("Insert", "F5", "A"), as on the keyboard.
pub fn key_name(vk: u16) -> String {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyNameTextW, MAPVK_VK_TO_VSC, MapVirtualKeyW,
    };
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) };
    // The navigation block and arrows share scan codes with the numpad; the extended bit
    // picks the dedicated keys ("Insert", not "Num 0").
    let extended = matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0x6F | 0x90);
    let lparam = ((scan << 16) | if extended { 1 << 24 } else { 0 }) as i32;
    let mut buffer = [0u16; 64];
    let len = unsafe { GetKeyNameTextW(lparam, buffer.as_mut_ptr(), buffer.len() as i32) };
    if len > 0 {
        String::from_utf16_lossy(&buffer[..len as usize])
    } else {
        format!("key {vk}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_moves_in_all_four_directions() {
        // Left column 0..10 (with left-stick directions), D-pad box 10..14, right column 14..25.
        assert_eq!(navigate(0, Action::Down), 1);
        assert_eq!(navigate(0, Action::Up), 0);
        assert_eq!(navigate(1, Action::Right), 11, "row 1 of the D-pad box");
        assert_eq!(navigate(11, Action::Right), 15, "row 1 of the right column");
        assert_eq!(navigate(15, Action::Left), 11);
        assert_eq!(navigate(24, Action::Down), 24);
        // Rows past the D-pad box's four clamp to its last row.
        assert_eq!(navigate(24, Action::Left), 13);
        assert_eq!(navigate(9, Action::Right), 13);
        assert_eq!(navigate(0, Action::Left), 0);
    }

    #[test]
    fn every_input_has_a_place_and_exactly_one_source() {
        for (i, button) in ALL.iter().enumerate() {
            let (column, row) = place(i);
            assert!(column < 3 && row < 11);
            let kinds = [
                button.sdl_button().is_some(),
                button.trigger_axis().is_some(),
                button.stick_direction().is_some(),
            ];
            assert_eq!(kinds.iter().filter(|k| **k).count(), 1, "{button:?}");
            assert_eq!(
                button.xinput_flag().is_some(),
                button.sdl_button().is_some(),
                "{button:?}"
            );
        }
    }

    #[test]
    fn stick_directions_follow_sdl_axes() {
        assert!(
            stick_pushed(0, -30_000, Direction::Up),
            "SDL y is negative when pushed up"
        );
        assert!(stick_pushed(30_000, 0, Direction::Right));
        assert!(
            !stick_pushed(5_000, 0, Direction::Right),
            "inside the dead zone"
        );
    }

    #[test]
    fn profile_keeps_up_to_three_unique_triggers() {
        let mut profile = Profile::default();
        let insert = Trigger::Key {
            vk: 0x2D,
            name: "Insert".into(),
        };
        profile.add(XButton::L3, insert.clone());
        profile.add(XButton::L3, insert.clone());
        assert_eq!(profile.extra[&XButton::L3].len(), 1, "no duplicates");
        for vk in [0x70, 0x71, 0x72] {
            profile.add(
                XButton::L3,
                Trigger::Key {
                    vk,
                    name: format!("F{}", vk - 0x6F),
                },
            );
        }
        assert_eq!(profile.extra[&XButton::L3].len(), MAX_TRIGGERS);
        assert!(
            !profile.extra[&XButton::L3].contains(&insert),
            "oldest dropped"
        );
        profile.clear(XButton::L3);
        assert!(profile.extra.is_empty());
    }

    #[test]
    fn profiles_round_trip_through_json() {
        let mut profile = Profile {
            name: "PS1 Controller".into(),
            ..Default::default()
        };
        profile.add(
            XButton::L3,
            Trigger::Key {
                vk: 0x2D,
                name: "Insert".into(),
            },
        );
        profile.add(XButton::A, Trigger::Button { button: XButton::X });
        let json = serde_json::to_string(&profile).unwrap();
        assert_eq!(serde_json::from_str::<Profile>(&json).unwrap(), profile);
    }

    #[test]
    fn insert_key_has_its_own_name_not_the_numpad_one() {
        // Names are in the Windows display language ("Insert", "INSER", ...), so compare instead.
        let insert = key_name(0x2D);
        assert!(!insert.is_empty() && !insert.starts_with("key "));
        assert_ne!(
            insert,
            key_name(0x60),
            "Insert must not be named like numpad 0"
        );
    }
}
