//! The quick panel (Menu button) and the screens it opens: Power and Settings (the Controllers
//! and Add games screens have their own logic). UI-free so the navigation can be unit-tested;
//! main.rs mirrors it into the window.

use crate::Action;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    None,
    Panel,
    Power,
    Settings,
    Controllers,
    AddGames,
    /// The Choose art screen (opened from a game's menu, not the panel).
    Art,
    /// "Quit JustPlay?" (B on the home screen).
    Quit,
}

/// Quick panel tiles, laid out two per row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelItem {
    Settings,
    Controllers,
    AddGames,
    Emulators,
    Rescan,
    Power,
    Exit,
}

pub const PANEL: [PanelItem; 7] = [
    PanelItem::Settings,
    PanelItem::Controllers,
    PanelItem::AddGames,
    PanelItem::Emulators,
    PanelItem::Rescan,
    PanelItem::Power,
    PanelItem::Exit,
];
const PANEL_COLUMNS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerItem {
    Sleep,
    Restart,
    ShutDown,
    Cancel,
}

pub const POWER: [PowerItem; 4] = [
    PowerItem::Sleep,
    PowerItem::Restart,
    PowerItem::ShutDown,
    PowerItem::Cancel,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    SoundVolume,
    MusicVolume,
    BackgroundDim,
    LauncherScreen,
    GameScreen,
    GameSound,
}

pub const SETTINGS: [Setting; 6] = [
    Setting::SoundVolume,
    Setting::MusicVolume,
    Setting::BackgroundDim,
    Setting::LauncherScreen,
    Setting::GameScreen,
    Setting::GameSound,
];

#[derive(Debug, PartialEq, Eq)]
pub enum MenuEffect {
    Rescan,
    /// The Add games screen opened: look for games on the drives.
    FindGames,
    /// The Add games screen opened on the emulators.
    ShowEmulators,
    Exit,
    Sleep,
    Restart,
    ShutDown,
    /// Step a setting by -1 or +1.
    Adjust(Setting, i32),
}

pub struct Menus {
    pub overlay: Overlay,
    pub panel_focus: usize,
    pub power_focus: usize,
    pub settings_focus: usize,
    /// In "Quit JustPlay?": 0 Quit, 1 Cancel.
    pub quit_focus: usize,
}

impl Menus {
    pub fn new() -> Menus {
        Menus {
            overlay: Overlay::None,
            panel_focus: 0,
            power_focus: 0,
            settings_focus: 0,
            quit_focus: 1,
        }
    }

    /// Asks before quitting. Cancel is selected: B is often pressed by reflex, so B then A
    /// must not close the launcher.
    pub fn ask_quit(&mut self) {
        self.overlay = Overlay::Quit;
        self.quit_focus = 1;
    }

    fn handle_quit(&mut self, action: Action) -> Vec<MenuEffect> {
        match action {
            Action::Left | Action::Up => self.quit_focus = 0,
            Action::Right | Action::Down => self.quit_focus = 1,
            Action::Back => self.overlay = Overlay::None,
            Action::Confirm => {
                self.overlay = Overlay::None;
                if self.quit_focus == 0 {
                    return vec![MenuEffect::Exit];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    pub fn is_open(&self) -> bool {
        self.overlay != Overlay::None
    }

    #[cfg(test)]
    pub fn open_panel(&mut self) {
        self.overlay = Overlay::Panel;
    }

    pub fn handle(&mut self, action: Action) -> Vec<MenuEffect> {
        if action == Action::Menu {
            // The Menu button toggles the panel from anywhere in it.
            self.overlay = if self.overlay == Overlay::None {
                Overlay::Panel
            } else {
                Overlay::None
            };
            return Vec::new();
        }
        match self.overlay {
            Overlay::None => Vec::new(),
            Overlay::Panel => self.handle_panel(action),
            Overlay::Power => self.handle_power(action),
            Overlay::Settings => self.handle_settings(action),
            Overlay::Quit => self.handle_quit(action),
            // These screens have their own logic; this only covers leaving them.
            Overlay::Controllers | Overlay::AddGames | Overlay::Art => {
                if action == Action::Back {
                    self.overlay = Overlay::Panel;
                }
                Vec::new()
            }
        }
    }

    fn handle_panel(&mut self, action: Action) -> Vec<MenuEffect> {
        let count = PANEL.len();
        let focus = self.panel_focus;
        match action {
            Action::Left if !focus.is_multiple_of(PANEL_COLUMNS) => self.panel_focus -= 1,
            Action::Right if focus % PANEL_COLUMNS + 1 < PANEL_COLUMNS && focus + 1 < count => {
                self.panel_focus += 1
            }
            Action::Up if focus >= PANEL_COLUMNS => self.panel_focus -= PANEL_COLUMNS,
            // Moving down from a full row onto a shorter last row lands on its last tile.
            Action::Down if focus + PANEL_COLUMNS < count => self.panel_focus += PANEL_COLUMNS,
            Action::Down if focus / PANEL_COLUMNS < (count - 1) / PANEL_COLUMNS => {
                self.panel_focus = count - 1
            }
            Action::Back => self.overlay = Overlay::None,
            Action::Confirm => {
                return match PANEL[focus] {
                    PanelItem::Settings => {
                        self.overlay = Overlay::Settings;
                        self.settings_focus = 0;
                        Vec::new()
                    }
                    PanelItem::Controllers => {
                        self.overlay = Overlay::Controllers;
                        Vec::new()
                    }
                    PanelItem::AddGames => {
                        self.overlay = Overlay::AddGames;
                        vec![MenuEffect::FindGames]
                    }
                    PanelItem::Emulators => {
                        self.overlay = Overlay::AddGames;
                        vec![MenuEffect::ShowEmulators]
                    }
                    PanelItem::Rescan => {
                        self.overlay = Overlay::None;
                        vec![MenuEffect::Rescan]
                    }
                    PanelItem::Power => {
                        self.overlay = Overlay::Power;
                        self.power_focus = 0;
                        Vec::new()
                    }
                    PanelItem::Exit => vec![MenuEffect::Exit],
                };
            }
            _ => {}
        }
        Vec::new()
    }

    fn handle_power(&mut self, action: Action) -> Vec<MenuEffect> {
        match action {
            Action::Up => self.power_focus = self.power_focus.saturating_sub(1),
            Action::Down => self.power_focus = (self.power_focus + 1).min(POWER.len() - 1),
            Action::Back => self.overlay = Overlay::Panel,
            Action::Confirm => {
                let effect = match POWER[self.power_focus] {
                    PowerItem::Sleep => MenuEffect::Sleep,
                    PowerItem::Restart => MenuEffect::Restart,
                    PowerItem::ShutDown => MenuEffect::ShutDown,
                    PowerItem::Cancel => {
                        self.overlay = Overlay::Panel;
                        return Vec::new();
                    }
                };
                self.overlay = Overlay::None;
                return vec![effect];
            }
            _ => {}
        }
        Vec::new()
    }

    fn handle_settings(&mut self, action: Action) -> Vec<MenuEffect> {
        let setting = SETTINGS[self.settings_focus];
        match action {
            Action::Up => self.settings_focus = self.settings_focus.saturating_sub(1),
            Action::Down => self.settings_focus = (self.settings_focus + 1).min(SETTINGS.len() - 1),
            Action::Left => return vec![MenuEffect::Adjust(setting, -1)],
            Action::Right => return vec![MenuEffect::Adjust(setting, 1)],
            Action::Back => self.overlay = Overlay::Panel,
            _ => {}
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_button_toggles_the_panel() {
        let mut menus = Menus::new();
        menus.handle(Action::Menu);
        assert_eq!(menus.overlay, Overlay::Panel);
        menus.handle(Action::Menu);
        assert_eq!(menus.overlay, Overlay::None);
    }

    #[test]
    fn panel_grid_navigation_reaches_every_tile() {
        let mut menus = Menus::new();
        menus.open_panel();
        // Rows: Settings Controllers / Add games Emulators / Rescan Power / Exit.
        menus.handle(Action::Right);
        assert_eq!(PANEL[menus.panel_focus], PanelItem::Controllers);
        menus.handle(Action::Down);
        assert_eq!(PANEL[menus.panel_focus], PanelItem::Emulators);
        menus.handle(Action::Down);
        assert_eq!(PANEL[menus.panel_focus], PanelItem::Power);
        // Down from a full row onto the shorter last row lands on its last tile.
        menus.handle(Action::Down);
        assert_eq!(PANEL[menus.panel_focus], PanelItem::Exit);
        menus.handle(Action::Down);
        assert_eq!(PANEL[menus.panel_focus], PanelItem::Exit);
        menus.handle(Action::Up);
        assert_eq!(PANEL[menus.panel_focus], PanelItem::Rescan);
        menus.handle(Action::Up);
        menus.handle(Action::Up);
        assert_eq!(PANEL[menus.panel_focus], PanelItem::Settings);
        menus.handle(Action::Down);
        assert_eq!(PANEL[menus.panel_focus], PanelItem::AddGames);
    }

    #[test]
    fn power_needs_a_second_deliberate_choice() {
        let mut menus = Menus::new();
        menus.open_panel();
        menus.panel_focus = 5; // Power
        assert!(menus.handle(Action::Confirm).is_empty());
        assert_eq!(menus.overlay, Overlay::Power);
        menus.handle(Action::Down);
        menus.handle(Action::Down);
        assert_eq!(menus.handle(Action::Confirm), [MenuEffect::ShutDown]);
        assert_eq!(menus.overlay, Overlay::None);
    }

    #[test]
    fn cancel_and_back_return_to_the_panel() {
        let mut menus = Menus::new();
        menus.open_panel();
        menus.panel_focus = 5; // Power
        menus.handle(Action::Confirm);
        menus.handle(Action::Back);
        assert_eq!(menus.overlay, Overlay::Panel);
        menus.handle(Action::Confirm);
        menus.power_focus = 3; // Cancel
        assert!(menus.handle(Action::Confirm).is_empty());
        assert_eq!(menus.overlay, Overlay::Panel);
    }

    #[test]
    fn quitting_asks_first_and_cancel_is_selected() {
        let mut menus = Menus::new();
        menus.ask_quit();
        assert_eq!(menus.overlay, Overlay::Quit);
        assert!(
            menus.handle(Action::Confirm).is_empty(),
            "A right away cancels"
        );
        assert_eq!(menus.overlay, Overlay::None);
        menus.ask_quit();
        menus.handle(Action::Back);
        assert_eq!(menus.overlay, Overlay::None);
        menus.ask_quit();
        menus.handle(Action::Left);
        assert_eq!(menus.handle(Action::Confirm), [MenuEffect::Exit]);
    }

    #[test]
    fn add_games_opens_and_starts_looking() {
        let mut menus = Menus::new();
        menus.open_panel();
        menus.panel_focus = 2;
        assert_eq!(menus.handle(Action::Confirm), [MenuEffect::FindGames]);
        assert_eq!(menus.overlay, Overlay::AddGames);
        menus.handle(Action::Back);
        assert_eq!(menus.overlay, Overlay::Panel);
    }

    #[test]
    fn settings_adjust_with_left_and_right() {
        let mut menus = Menus::new();
        menus.open_panel();
        menus.handle(Action::Confirm); // Settings
        assert_eq!(menus.overlay, Overlay::Settings);
        assert_eq!(
            menus.handle(Action::Right),
            [MenuEffect::Adjust(Setting::SoundVolume, 1)]
        );
        menus.handle(Action::Down);
        assert_eq!(
            menus.handle(Action::Left),
            [MenuEffect::Adjust(Setting::MusicVolume, -1)]
        );
    }
}
