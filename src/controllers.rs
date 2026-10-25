//! The Controllers screen: the Xbox-style buttons the virtual pad exposes, their places in the
//! three lists around the controller picture (left column, D-pad box, right column), and the
//! remaps saved per controller model: replacements ("the pad's X now presses A") and extra
//! triggers ("keyboard Insert also presses L3").

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

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

/// Remaps for one controller model, keyed by SDL's GUID for that model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub extra: BTreeMap<XButton, Vec<Trigger>>,
    /// Button -> the pad input that now presses it instead (replacing, not adding: that input
    /// stops pressing its own button, and the button's own input stops pressing it).
    pub remap: BTreeMap<XButton, XButton>,
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

pub fn index_of(button: XButton) -> usize {
    ALL.iter().position(|b| *b == button).unwrap_or(0)
}

/// For each pad input (in `ALL` order), the button it presses under `remap`: itself unless
/// remapped. An input given to another button stops pressing its own, and a remapped button
/// is no longer pressed by its own input.
pub fn routes(remap: &BTreeMap<XButton, XButton>) -> [Option<XButton>; 25] {
    let mut routes = ALL.map(Some);
    for target in remap.keys() {
        routes[index_of(*target)] = None;
    }
    for (target, source) in remap {
        routes[index_of(*source)] = Some(*target);
    }
    routes
}

/// The buttons the game sees for the held pad inputs `raw` (bit i = ALL[i]).
pub fn output_mask(profile: &Profile, raw: u32, key_down: impl Fn(u16) -> bool) -> u32 {
    let held = |i: usize| raw & (1 << i) != 0;
    let routes = routes(&profile.remap);
    let mut mask = (0..ALL.len())
        .filter(|&i| held(i))
        .filter_map(|i| routes[i])
        .fold(0, |mask, button| mask | (1 << index_of(button)));
    for (button, triggers) in &profile.extra {
        let active = triggers.iter().any(|trigger| match trigger {
            Trigger::Key { vk, .. } => key_down(*vk),
            Trigger::Button { button } => held(index_of(*button)),
        });
        if active {
            mask |= 1 << index_of(*button);
        }
    }
    mask
}

/// How the Controllers screen reads the pad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Buttons only light up, so every one can be checked. The mouse remaps.
    #[default]
    Test,
    /// The D-pad and sticks move between buttons; A remaps, X adds a key, Y clears.
    Focus,
    /// Every button in turn: press the one that should act as the lit button.
    RemapAll,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Test, Mode::Focus, Mode::RemapAll];

    pub fn step(self, delta: isize) -> Mode {
        let index = Self::ALL.iter().position(|m| *m == self).unwrap_or(0) as isize;
        Self::ALL[(index + delta).rem_euclid(Self::ALL.len() as isize) as usize]
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Test => "Test",
            Mode::Focus => "Focus",
            Mode::RemapAll => "Remap all",
        }
    }
}

/// What holding a pad button does on the Controllers screen. A quick press does nothing
/// beyond lighting the button up, so every button can be tested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldAction {
    Leave,
    /// Stop a remap in progress, keeping the buttons done so far.
    StopRemap,
    /// The previous (-1) or next (+1) mode.
    SwitchMode(isize),
    /// Show the previous (-1) or next (+1) connected pad.
    SwitchPad(isize),
}

/// The action for holding the pad input at `index` (in `ALL` order), and how long it takes.
/// While remapping, every other button is being recorded, so only B's hold counts.
pub fn hold_action(index: usize, pads: usize, remapping: bool) -> Option<(HoldAction, Duration)> {
    let button = *ALL.get(index)?;
    let second = Duration::from_secs(1);
    Some(match button {
        XButton::B if remapping => (HoldAction::StopRemap, Duration::from_secs(2)),
        XButton::B => (HoldAction::Leave, Duration::from_secs(2)),
        _ if remapping => return None,
        XButton::Lt => (HoldAction::SwitchMode(-1), second),
        XButton::Rt => (HoldAction::SwitchMode(1), second),
        XButton::Lb if pads > 1 => (HoldAction::SwitchPad(-1), second),
        XButton::Rb if pads > 1 => (HoldAction::SwitchPad(1), second),
        _ => return None,
    })
}

/// What a press during a remap led to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemapEvent {
    Assigned {
        target: XButton,
        source: XButton,
    },
    /// Refused: one input can't press two buttons in the same remap.
    AlreadyUsed {
        source: XButton,
        by: XButton,
    },
    /// A stick moved while a button was lit: maybe by accident, so the user decides.
    AskStick {
        source: XButton,
    },
}

/// Remapping buttons one after another (one button for a single remap). A press counts when
/// it is let go, so B held to stop is never recorded. Each step is written to `remap` at
/// once, so stopping keeps the buttons done so far.
#[derive(Clone, Debug)]
pub struct RemapRun {
    pub targets: Vec<XButton>,
    pub step: usize,
    /// The input pressed for the lit button, recorded when released.
    pending: Option<XButton>,
    /// A stick direction waiting for "use it?".
    pub confirm: Option<XButton>,
    /// Per step done: the input used (None when skipped) and the remaps before it.
    done: Vec<(Option<XButton>, BTreeMap<XButton, XButton>)>,
    /// The mode to go back to afterwards.
    pub return_to: Mode,
}

/// The buttons a "Remap all" goes through: all but Guide, which many pads lack and which
/// Windows or Steam often take for themselves.
pub fn remap_all_targets() -> Vec<XButton> {
    ALL.into_iter().filter(|b| *b != XButton::Guide).collect()
}

impl RemapRun {
    pub fn new(targets: Vec<XButton>, return_to: Mode) -> Self {
        RemapRun {
            targets,
            step: 0,
            pending: None,
            confirm: None,
            done: Vec::new(),
            return_to,
        }
    }

    pub fn target(&self) -> Option<XButton> {
        self.targets.get(self.step).copied()
    }

    /// A pad input went down. A button beats a stick moved alongside it.
    pub fn press(&mut self, source: XButton) {
        if self.confirm.is_some() {
            return;
        }
        match self.pending {
            None => self.pending = Some(source),
            Some(p) if p.stick_direction().is_some() && source.stick_direction().is_none() => {
                self.pending = Some(source)
            }
            _ => {}
        }
    }

    /// A pad input was let go: if it is the pressed one, it is recorded for the lit button.
    pub fn release(
        &mut self,
        source: XButton,
        remap: &mut BTreeMap<XButton, XButton>,
    ) -> Option<RemapEvent> {
        if self.pending != Some(source) || self.confirm.is_some() {
            return None;
        }
        self.pending = None;
        let target = self.target()?;
        if let Some(by) = self.used_by(source) {
            return Some(RemapEvent::AlreadyUsed { source, by });
        }
        if source.stick_direction().is_some() && target.stick_direction().is_none() {
            self.confirm = Some(source);
            return Some(RemapEvent::AskStick { source });
        }
        Some(self.assign(target, source, remap))
    }

    /// "Use it": the stick direction asked about is recorded.
    pub fn use_stick(&mut self, remap: &mut BTreeMap<XButton, XButton>) -> Option<RemapEvent> {
        let source = self.confirm.take()?;
        let target = self.target()?;
        Some(self.assign(target, source, remap))
    }

    /// "Press again": forget the stick direction and wait for another press.
    pub fn retry(&mut self) {
        self.confirm = None;
        self.pending = None;
    }

    /// Leaves the lit button as it is and moves on.
    pub fn skip(&mut self, remap: &BTreeMap<XButton, XButton>) {
        if self.target().is_none() {
            return;
        }
        self.retry();
        self.done.push((None, remap.clone()));
        self.step += 1;
    }

    /// Goes back one button, undoing what was recorded for it.
    pub fn back(&mut self, remap: &mut BTreeMap<XButton, XButton>) {
        self.retry();
        if let Some((_, before)) = self.done.pop() {
            *remap = before;
            self.step -= 1;
        }
    }

    fn used_by(&self, source: XButton) -> Option<XButton> {
        self.done
            .iter()
            .zip(&self.targets)
            .find(|((used, _), _)| *used == Some(source))
            .map(|(_, target)| *target)
    }

    fn assign(
        &mut self,
        target: XButton,
        source: XButton,
        remap: &mut BTreeMap<XButton, XButton>,
    ) -> RemapEvent {
        self.done.push((Some(source), remap.clone()));
        remap.retain(|_, s| *s != source);
        if source == target {
            remap.remove(&target);
        } else {
            remap.insert(target, source);
        }
        self.step += 1;
        RemapEvent::Assigned { target, source }
    }
}

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

    fn index(button: XButton) -> usize {
        ALL.iter().position(|b| *b == button).unwrap()
    }

    #[test]
    fn holds_leave_switch_mode_or_pad() {
        let secs = |s| Duration::from_secs(s);
        let hold = |button, pads, remapping| hold_action(index(button), pads, remapping);
        assert_eq!(
            hold(XButton::B, 1, false),
            Some((HoldAction::Leave, secs(2)))
        );
        assert_eq!(
            hold(XButton::Lt, 1, false),
            Some((HoldAction::SwitchMode(-1), secs(1)))
        );
        assert_eq!(
            hold(XButton::Rt, 1, false),
            Some((HoldAction::SwitchMode(1), secs(1)))
        );
        assert_eq!(hold(XButton::Lb, 1, false), None, "one pad");
        assert_eq!(
            hold(XButton::Rb, 2, false),
            Some((HoldAction::SwitchPad(1), secs(1)))
        );
        // No more hold-to-remap: other buttons only light up.
        for button in [
            XButton::A,
            XButton::X,
            XButton::Y,
            XButton::Start,
            XButton::DpadUp,
        ] {
            assert_eq!(hold(button, 2, false), None, "{button:?}");
        }
        // While remapping every button is recorded; only B's hold stops it.
        assert_eq!(
            hold(XButton::B, 2, true),
            Some((HoldAction::StopRemap, secs(2)))
        );
        assert_eq!(hold(XButton::Rt, 2, true), None);
        assert_eq!(hold(XButton::Lb, 2, true), None);
        assert_eq!(hold_action(99, 1, false), None);
    }

    #[test]
    fn modes_cycle_both_ways() {
        assert_eq!(Mode::Test.step(1), Mode::Focus);
        assert_eq!(Mode::Focus.step(1), Mode::RemapAll);
        assert_eq!(Mode::RemapAll.step(1), Mode::Test);
        assert_eq!(Mode::Test.step(-1), Mode::RemapAll);
    }

    fn bit(button: XButton) -> u32 {
        1 << index(button)
    }

    #[test]
    fn remapping_replaces_instead_of_adding() {
        let swap = BTreeMap::from([(XButton::A, XButton::B), (XButton::B, XButton::A)]);
        let routes = routes(&swap);
        assert_eq!(routes[index(XButton::A)], Some(XButton::B));
        assert_eq!(routes[index(XButton::B)], Some(XButton::A));
        assert_eq!(routes[index(XButton::X)], Some(XButton::X), "untouched");
        // A given to X: the pad's A presses X only, and the pad's X presses nothing.
        let one = BTreeMap::from([(XButton::X, XButton::A)]);
        let routes = super::routes(&one);
        assert_eq!(routes[index(XButton::A)], Some(XButton::X));
        assert_eq!(routes[index(XButton::X)], None);
    }

    #[test]
    fn the_glow_shows_what_the_game_gets() {
        let mut profile = Profile::default();
        profile.remap.insert(XButton::X, XButton::A);
        profile.add(
            XButton::L3,
            Trigger::Key {
                vk: 0x2D,
                name: "Insert".into(),
            },
        );
        let no_keys = |_| false;
        assert_eq!(
            output_mask(&profile, bit(XButton::A), no_keys),
            bit(XButton::X)
        );
        assert_eq!(output_mask(&profile, bit(XButton::X), no_keys), 0);
        assert_eq!(
            output_mask(&profile, bit(XButton::Y), |vk| vk == 0x2D),
            bit(XButton::Y) | bit(XButton::L3)
        );
    }

    #[test]
    fn remap_all_records_on_release_and_can_go_back() {
        let mut remap = BTreeMap::new();
        let mut run = RemapRun::new(vec![XButton::A, XButton::B, XButton::X], Mode::Test);
        assert_eq!(run.target(), Some(XButton::A));
        run.press(XButton::B);
        assert_eq!(run.target(), Some(XButton::A), "nothing until let go");
        assert_eq!(
            run.release(XButton::B, &mut remap),
            Some(RemapEvent::Assigned {
                target: XButton::A,
                source: XButton::B
            })
        );
        // The same input can't press two buttons.
        run.press(XButton::B);
        assert_eq!(
            run.release(XButton::B, &mut remap),
            Some(RemapEvent::AlreadyUsed {
                source: XButton::B,
                by: XButton::A
            })
        );
        run.press(XButton::A);
        run.release(XButton::A, &mut remap);
        assert_eq!(
            remap,
            BTreeMap::from([(XButton::A, XButton::B), (XButton::B, XButton::A)])
        );
        run.back(&mut remap);
        assert_eq!(run.target(), Some(XButton::B));
        assert_eq!(remap, BTreeMap::from([(XButton::A, XButton::B)]));
        run.skip(&remap);
        // Pressing a button's own input keeps it as it is (no entry).
        run.press(XButton::X);
        run.release(XButton::X, &mut remap);
        assert_eq!(run.target(), None, "finished");
        assert_eq!(remap, BTreeMap::from([(XButton::A, XButton::B)]));
    }

    #[test]
    fn a_stick_moved_for_a_button_is_asked_about() {
        let mut remap = BTreeMap::new();
        let mut run = RemapRun::new(vec![XButton::A, XButton::LsUp], Mode::Test);
        run.press(XButton::LsRight);
        assert_eq!(
            run.release(XButton::LsRight, &mut remap),
            Some(RemapEvent::AskStick {
                source: XButton::LsRight
            })
        );
        run.retry();
        assert!(remap.is_empty());
        // A button pressed while the stick was nudged wins.
        run.press(XButton::LsRight);
        run.press(XButton::X);
        assert_eq!(run.release(XButton::LsRight, &mut remap), None);
        run.release(XButton::X, &mut remap);
        assert_eq!(remap, BTreeMap::from([(XButton::A, XButton::X)]));
        // For a stick direction, the stick is expected.
        run.press(XButton::LsUp);
        run.release(XButton::LsUp, &mut remap);
        assert_eq!(run.target(), None);
        // "Use it" records the stick.
        let mut run = RemapRun::new(vec![XButton::A], Mode::Focus);
        run.press(XButton::RsDown);
        run.release(XButton::RsDown, &mut remap);
        run.use_stick(&mut remap);
        assert_eq!(remap, BTreeMap::from([(XButton::A, XButton::RsDown)]));
    }

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
