//! Home screen logic: tabs, the game row, action buttons, recently played and the game sheet.
//! Kept free of UI calls so navigation can be unit-tested; main.rs mirrors it into the window.

use std::cmp::Reverse;
use std::collections::HashMap;

use crate::library::Game;
use crate::nav;
use crate::state::{ControllerMode, State};
use crate::{Action, Zone};

/// Games skipped by LT / RT.
pub const PAGE: isize = 8;
const RECENT_LIMIT: usize = 8;
pub const FAVORITES_TAB: &str = "Favorites";

pub struct Tab {
    pub name: String,
    /// Indices into `Home::games`.
    pub games: Vec<usize>,
    pub focus: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetItem {
    Play,
    Resume,
    CloseGame,
    ControllerMode,
    ToggleFavorite,
}

pub struct Sheet {
    pub game: usize,
    pub focus: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Tab list or the current tab's games changed.
    Tabs,
    /// A different game is selected in the row.
    Selection,
    Launch(usize),
    SaveState,
    /// Bring the running game back to the front.
    Resume,
    CloseGame,
    /// The game's controller mode changed (applied at once if it is running).
    ControllerMode(usize),
    /// The gear in the top bar was chosen: open the quick panel.
    OpenPanel,
}

/// Everything a key press can change, compared before/after to pick a feedback sound.
/// (PartialEq only: the Slint-generated Zone doesn't implement Eq.)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavState {
    pub tab: usize,
    pub focus: usize,
    pub zone: Zone,
    pub action: usize,
    pub recent_focus: usize,
    pub sheet_open: bool,
    pub sheet_focus: usize,
    pub gear: bool,
}

pub struct Home {
    pub games: Vec<Game>,
    pub state: State,
    pub tabs: Vec<Tab>,
    pub current: usize,
    pub zone: Zone,
    /// 0 = Play, 1 = ⋯
    pub action: usize,
    pub recent: Vec<usize>,
    pub recent_focus: usize,
    pub sheet: Option<Sheet>,
    /// In the tab bar, the cursor sits on the gear (past the last tab).
    pub gear: bool,
    /// The game currently running, if any.
    pub running: Option<usize>,
}

impl Home {
    pub fn new(games: Vec<Game>, state: State) -> Home {
        let mut home = Home {
            games,
            state,
            tabs: Vec::new(),
            current: 0,
            zone: Zone::Icons,
            action: 0,
            recent: Vec::new(),
            recent_focus: 0,
            sheet: None,
            gear: false,
            running: None,
        };
        home.rebuild();
        home
    }

    pub fn nav_state(&self) -> NavState {
        NavState {
            tab: self.current,
            focus: self.tab().focus,
            zone: self.zone,
            action: self.action,
            recent_focus: self.recent_focus,
            sheet_open: self.sheet.is_some(),
            sheet_focus: self.sheet.as_ref().map_or(0, |s| s.focus),
            gear: self.gear,
        }
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.current]
    }

    pub fn selected_game(&self) -> Option<usize> {
        let tab = self.tab();
        tab.games.get(tab.focus).copied()
    }

    pub fn sheet_items(&self) -> Vec<SheetItem> {
        let Some(game) = self.sheet.as_ref().map(|s| s.game) else {
            return Vec::new();
        };
        let mut items = if Some(game) == self.running {
            vec![SheetItem::Resume, SheetItem::CloseGame]
        } else {
            vec![SheetItem::Play]
        };
        items.push(SheetItem::ToggleFavorite);
        // Last, so the other items keep their positions whatever the game.
        items.push(SheetItem::ControllerMode);
        items
    }

    /// The mode chosen in the game's menu, else the default: Xbox for PC games, Off for
    /// emulator games (emulators read pads directly unless told otherwise).
    pub fn controller_mode(&self, game: usize) -> ControllerMode {
        let default = if self.games[game].xinput {
            ControllerMode::Xbox
        } else {
            ControllerMode::Off
        };
        self.state
            .controller_modes
            .get(&self.games[game].id)
            .copied()
            .unwrap_or(default)
    }

    /// Playing the game that is already running just brings it back.
    fn play(&self, game: Option<usize>) -> Vec<Effect> {
        match game {
            Some(game) if Some(game) == self.running => vec![Effect::Resume],
            Some(game) => vec![Effect::Launch(game)],
            None => Vec::new(),
        }
    }

    /// Over a running game: select it and open its menu on "Close game", so closing is one press.
    pub fn open_close_menu(&mut self) {
        let Some(game) = self.running else { return };
        if !self.tab().games.contains(&game) {
            // Home lists every game, so the running one is always there.
            self.current = 0;
        }
        let tab = &mut self.tabs[self.current];
        if let Some(position) = tab.games.iter().position(|&g| g == game) {
            tab.focus = position;
        }
        // The menu opens beside the ⋯ button, so show that button as the active one.
        self.zone = Zone::Actions;
        self.action = 1;
        self.sheet = Some(Sheet { game, focus: 0 });
        let close = self
            .sheet_items()
            .iter()
            .position(|&item| item == SheetItem::CloseGame);
        if let (Some(sheet), Some(close)) = (self.sheet.as_mut(), close) {
            sheet.focus = close;
        }
    }

    pub fn record_played(&mut self, game: usize, unix_seconds: u64) {
        let id = self.games[game].id.clone();
        self.state.last_played.insert(id, unix_seconds);
        self.rebuild();
    }

    pub fn is_favorite(&self, game: usize) -> bool {
        self.state.favorites.contains(&self.games[game].id)
    }

    /// Recomputes tabs after favorites or history change, keeping the user where they were.
    pub fn rebuild(&mut self) {
        let current_name = self.tabs.get(self.current).map(|t| t.name.clone());
        let focused: HashMap<String, (usize, usize)> = self
            .tabs
            .iter()
            .map(|t| {
                (
                    t.name.clone(),
                    (t.games.get(t.focus).copied().unwrap_or(usize::MAX), t.focus),
                )
            })
            .collect();

        self.tabs = build_tabs(&self.games, &self.state);
        for tab in &mut self.tabs {
            if let Some(&(game, old_focus)) = focused.get(&tab.name) {
                tab.focus = tab
                    .games
                    .iter()
                    .position(|&g| g == game)
                    .unwrap_or_else(|| old_focus.min(tab.games.len().saturating_sub(1)));
            }
        }
        self.current = current_name
            .and_then(|name| self.tabs.iter().position(|t| t.name == name))
            .unwrap_or(0);
        self.recent = recently_played(&self.games, &self.state);
        self.recent_focus = self.recent_focus.min(self.recent.len().saturating_sub(1));
        if self.zone == Zone::Recent && self.recent.is_empty() {
            self.zone = Zone::Actions;
        }
        if self.tab().games.is_empty() && matches!(self.zone, Zone::Actions | Zone::Recent) {
            self.zone = Zone::Icons;
        }
    }

    pub fn handle(&mut self, action: Action) -> Vec<Effect> {
        if self.sheet.is_some() {
            return self.handle_sheet(action);
        }
        match action {
            Action::TabPrev => return self.switch_tab(-1),
            Action::TabNext => return self.switch_tab(1),
            _ => {}
        }
        let has_games = !self.tab().games.is_empty();
        match (self.zone, action) {
            // The gear sits after the last tab: Right reaches it, Left comes back.
            (Zone::Tabs, Action::Left) if self.gear => {
                self.gear = false;
                Vec::new()
            }
            (Zone::Tabs, Action::Confirm) if self.gear => vec![Effect::OpenPanel],
            (Zone::Tabs, Action::Right) if self.gear => Vec::new(),
            (Zone::Tabs, Action::Right) if self.current + 1 == self.tabs.len() => {
                self.gear = true;
                Vec::new()
            }
            (Zone::Tabs, Action::Left) => self.switch_tab(-1),
            (Zone::Tabs, Action::Right) => self.switch_tab(1),
            (Zone::Tabs, Action::Down | Action::Confirm) => {
                self.gear = false;
                self.go(Zone::Icons)
            }

            (Zone::Icons, Action::Left) => self.move_focus(-1),
            (Zone::Icons, Action::Right) => self.move_focus(1),
            (Zone::Icons, Action::PagePrev) => self.move_focus(-PAGE),
            (Zone::Icons, Action::PageNext) => self.move_focus(PAGE),
            (Zone::Icons, Action::Up | Action::Back) => self.go(Zone::Tabs),
            (Zone::Icons, Action::Down) if has_games => {
                self.action = 0;
                self.go(Zone::Actions)
            }
            (Zone::Icons, Action::Confirm) => self.play(self.selected_game()),
            (Zone::Icons | Zone::Actions, Action::Options) => self.open_sheet(self.selected_game()),

            (Zone::Actions, Action::Left) => {
                self.action = 0;
                Vec::new()
            }
            (Zone::Actions, Action::Right) => {
                self.action = 1;
                Vec::new()
            }
            (Zone::Actions, Action::Up | Action::Back) => self.go(Zone::Icons),
            (Zone::Actions, Action::Down) if !self.recent.is_empty() => self.go(Zone::Recent),
            (Zone::Actions, Action::Confirm) if self.action == 0 => self.play(self.selected_game()),
            (Zone::Actions, Action::Confirm) => self.open_sheet(self.selected_game()),

            (Zone::Recent, Action::Left) => {
                self.recent_focus = self.recent_focus.saturating_sub(1);
                Vec::new()
            }
            (Zone::Recent, Action::Right) => {
                self.recent_focus =
                    (self.recent_focus + 1).min(self.recent.len().saturating_sub(1));
                Vec::new()
            }
            (Zone::Recent, Action::Up | Action::Back) => self.go(Zone::Actions),
            (Zone::Recent, Action::Confirm) => {
                self.play(self.recent.get(self.recent_focus).copied())
            }
            (Zone::Recent, Action::Options) => {
                self.open_sheet(self.recent.get(self.recent_focus).copied())
            }
            _ => Vec::new(),
        }
    }

    fn handle_sheet(&mut self, action: Action) -> Vec<Effect> {
        let items = self.sheet_items();
        let Some(sheet) = self.sheet.as_mut() else {
            return Vec::new();
        };
        match action {
            Action::Up => sheet.focus = sheet.focus.saturating_sub(1),
            Action::Down => sheet.focus = (sheet.focus + 1).min(items.len() - 1),
            Action::Back | Action::Options => self.sheet = None,
            Action::Confirm => {
                let game = sheet.game;
                // The item list can shrink under an open sheet (the game exits), so clamp.
                let item = items[sheet.focus.min(items.len() - 1)];
                if item == SheetItem::ControllerMode {
                    // Cycles in place so the new mode is visible on the same line.
                    let next = self.controller_mode(game).next();
                    let id = self.games[game].id.clone();
                    self.state.controller_modes.insert(id, next);
                    return vec![Effect::SaveState, Effect::ControllerMode(game)];
                }
                self.sheet = None;
                return match item {
                    SheetItem::Play => vec![Effect::Launch(game)],
                    SheetItem::Resume => vec![Effect::Resume],
                    SheetItem::CloseGame => vec![Effect::CloseGame],
                    SheetItem::ControllerMode => unreachable!("handled above"),
                    SheetItem::ToggleFavorite => {
                        let id = self.games[game].id.clone();
                        self.state.toggle_favorite(&id);
                        self.rebuild();
                        vec![Effect::Tabs, Effect::Selection, Effect::SaveState]
                    }
                };
            }
            _ => {}
        }
        Vec::new()
    }

    fn open_sheet(&mut self, game: Option<usize>) -> Vec<Effect> {
        if let Some(game) = game {
            self.sheet = Some(Sheet { game, focus: 0 });
        }
        Vec::new()
    }

    fn go(&mut self, zone: Zone) -> Vec<Effect> {
        self.zone = zone;
        Vec::new()
    }

    fn switch_tab(&mut self, delta: isize) -> Vec<Effect> {
        self.gear = false;
        if self.tabs.len() < 2 {
            return Vec::new();
        }
        self.current = nav::wrap_index(self.current, self.tabs.len(), delta);
        if self.tab().games.is_empty() || matches!(self.zone, Zone::Recent) {
            self.zone = if self.zone == Zone::Tabs {
                Zone::Tabs
            } else {
                Zone::Icons
            };
        }
        vec![Effect::Tabs, Effect::Selection]
    }

    fn move_focus(&mut self, delta: isize) -> Vec<Effect> {
        let tab = &mut self.tabs[self.current];
        let next = nav::step(tab.focus, tab.games.len(), delta);
        if next == tab.focus {
            return Vec::new();
        }
        tab.focus = next;
        vec![Effect::Selection]
    }
}

/// Home lists recently played games first; then one tab per platform; Favorites last.
fn build_tabs(games: &[Game], state: &State) -> Vec<Tab> {
    let last_played = |i: &usize| state.last_played.get(&games[*i].id).copied().unwrap_or(0);
    let mut home: Vec<usize> = (0..games.len()).collect();
    // Stable sort: games arrive sorted by title, so never-played games stay alphabetical.
    home.sort_by_key(|i| Reverse(last_played(i)));
    let mut tabs = vec![Tab {
        name: "Home".into(),
        games: home,
        focus: 0,
    }];

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
    for platform in platforms {
        let indices = (0..games.len())
            .filter(|&i| games[i].platform == platform)
            .collect();
        tabs.push(Tab {
            name: platform.into(),
            games: indices,
            focus: 0,
        });
    }

    let favorites: Vec<usize> = (0..games.len())
        .filter(|&i| state.favorites.contains(&games[i].id))
        .collect();
    if !favorites.is_empty() {
        tabs.push(Tab {
            name: FAVORITES_TAB.into(),
            games: favorites,
            focus: 0,
        });
    }
    tabs
}

fn recently_played(games: &[Game], state: &State) -> Vec<usize> {
    let mut played: Vec<(u64, usize)> = (0..games.len())
        .filter_map(|i| state.last_played.get(&games[i].id).map(|&t| (t, i)))
        .collect();
    played.sort_by_key(|&(t, _)| Reverse(t));
    played
        .into_iter()
        .take(RECENT_LIMIT)
        .map(|(_, i)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Art, Launch};

    fn game(id: &str, title: &str, platform: &str) -> Game {
        Game {
            id: id.into(),
            title: title.into(),
            platform: platform.into(),
            launch: Launch {
                exe: "x.exe".into(),
                args: vec![],
            },
            art: Art::default(),
            xinput: false,
        }
    }

    fn sample() -> Vec<Game> {
        vec![
            game("ps3:BLUS30405", "Dante's Inferno", "PS3"),
            game("pc:God of War", "God of War", "PC"),
            game("ps3:BCUS98111", "God of War III", "PS3"),
            game("pc:TLOU", "The Last of Us Part I", "PC"),
        ]
    }

    fn names(home: &Home) -> Vec<&str> {
        home.tabs.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn tabs_are_home_then_pc_then_ps3() {
        let home = Home::new(sample(), State::default());
        assert_eq!(names(&home), ["Home", "PC", "PS3"]);
        assert_eq!(home.tabs[1].games, [1, 3]);
    }

    #[test]
    fn home_puts_recently_played_first_and_fills_recent_row() {
        let mut state = State::default();
        state.last_played.insert("pc:TLOU".into(), 200);
        state.last_played.insert("ps3:BLUS30405".into(), 100);
        let home = Home::new(sample(), state);
        assert_eq!(home.tabs[0].games, [3, 0, 1, 2]);
        assert_eq!(home.recent, [3, 0]);
    }

    #[test]
    fn vertical_navigation_moves_between_zones() {
        let mut home = Home::new(sample(), State::default());
        assert_eq!(home.zone, Zone::Icons);
        home.handle(Action::Down);
        assert_eq!(home.zone, Zone::Actions);
        // No play history yet, so there is no recent row to move into.
        home.handle(Action::Down);
        assert_eq!(home.zone, Zone::Actions);
        home.handle(Action::Up);
        home.handle(Action::Up);
        assert_eq!(home.zone, Zone::Tabs);
        home.handle(Action::Right);
        assert_eq!(home.current, 1);
    }

    #[test]
    fn page_jump_clamps_to_ends() {
        let mut home = Home::new(sample(), State::default());
        assert_eq!(home.handle(Action::PageNext), [Effect::Selection]);
        assert_eq!(home.tab().focus, 3);
        assert!(home.handle(Action::PageNext).is_empty());
        home.handle(Action::PagePrev);
        assert_eq!(home.tab().focus, 0);
    }

    #[test]
    fn confirm_launches_selected_game_from_row_and_play_button() {
        let mut home = Home::new(sample(), State::default());
        home.handle(Action::Right);
        assert_eq!(home.handle(Action::Confirm), [Effect::Launch(1)]);
        home.handle(Action::Down);
        assert_eq!(home.handle(Action::Confirm), [Effect::Launch(1)]);
    }

    #[test]
    fn favoriting_from_sheet_adds_and_removes_favorites_tab() {
        let mut home = Home::new(sample(), State::default());
        home.handle(Action::Options);
        home.handle(Action::Down);
        let effects = home.handle(Action::Confirm);
        assert!(effects.contains(&Effect::SaveState));
        assert_eq!(names(&home), ["Home", "PC", "PS3", "Favorites"]);

        // Unfavorite from inside the Favorites tab: it disappears and we land back on Home.
        home.handle(Action::TabPrev);
        assert_eq!(home.tab().name, "Favorites");
        home.handle(Action::Options);
        home.handle(Action::Down);
        home.handle(Action::Confirm);
        assert_eq!(names(&home), ["Home", "PC", "PS3"]);
        assert_eq!(home.current, 0);
        assert!(home.sheet.is_none());
    }

    #[test]
    fn running_game_offers_resume_and_close() {
        let mut home = Home::new(sample(), State::default());
        home.running = Some(0);
        assert_eq!(home.handle(Action::Confirm), [Effect::Resume]);
        home.handle(Action::Options);
        assert_eq!(
            home.sheet_items(),
            [
                SheetItem::Resume,
                SheetItem::CloseGame,
                SheetItem::ToggleFavorite,
                SheetItem::ControllerMode
            ]
        );
        home.handle(Action::Down);
        assert_eq!(home.handle(Action::Confirm), [Effect::CloseGame]);
        // Another game still launches normally (main.rs decides what to do about the running one).
        home.handle(Action::Right);
        assert_eq!(home.handle(Action::Confirm), [Effect::Launch(1)]);
    }

    #[test]
    fn recording_a_launch_moves_game_to_front_of_home_and_recent() {
        let mut home = Home::new(sample(), State::default());
        home.record_played(2, 1_000);
        assert_eq!(home.tabs[0].games[0], 2);
        assert_eq!(home.recent, [2]);
    }

    #[test]
    fn sheet_survives_game_exiting_while_open() {
        let mut home = Home::new(sample(), State::default());
        home.running = Some(0);
        home.handle(Action::Options);
        home.handle(Action::Down);
        home.handle(Action::Down);
        // Cursor is on the third item when the game exits and the list drops to two items.
        home.running = None;
        let effects = home.handle(Action::Confirm);
        assert!(effects.contains(&Effect::SaveState), "{effects:?}");
    }

    #[test]
    fn close_menu_selects_running_game_and_closes_in_one_press() {
        let mut home = Home::new(sample(), State::default());
        home.running = Some(3);
        home.handle(Action::TabNext); // PC tab
        home.open_close_menu();
        assert_eq!(home.selected_game(), Some(3));
        assert!(home.sheet.is_some());
        assert_eq!(home.handle(Action::Confirm), [Effect::CloseGame]);
    }

    #[test]
    fn close_menu_switches_to_home_when_running_game_is_not_in_tab() {
        let mut home = Home::new(sample(), State::default());
        home.running = Some(0); // a PS3 game
        home.handle(Action::TabNext); // PC tab
        home.open_close_menu();
        assert_eq!(home.current, 0);
        assert_eq!(home.selected_game(), Some(0));
    }

    #[test]
    fn controller_mode_line_cycles_and_defaults_by_game_kind() {
        let mut games = sample();
        games[1].xinput = true; // a PC game
        let mut home = Home::new(games, State::default());
        home.handle(Action::Right); // God of War
        home.handle(Action::Options);
        assert_eq!(
            home.sheet_items(),
            [
                SheetItem::Play,
                SheetItem::ToggleFavorite,
                SheetItem::ControllerMode
            ]
        );
        home.handle(Action::Down);
        home.handle(Action::Down);
        assert_eq!(
            home.handle(Action::Confirm),
            [Effect::SaveState, Effect::ControllerMode(1)]
        );
        assert!(
            home.sheet.is_some(),
            "sheet stays open to show the new mode"
        );
        assert_eq!(home.controller_mode(1), ControllerMode::PlaystationNative);
        // Emulator games default to Off but can be switched on like any other game.
        assert_eq!(home.controller_mode(0), ControllerMode::Off);
        home.state
            .controller_modes
            .insert("ps3:BLUS30405".into(), ControllerMode::Xbox);
        assert_eq!(home.controller_mode(0), ControllerMode::Xbox);
    }

    #[test]
    fn gear_is_reachable_from_the_last_tab() {
        let mut home = Home::new(sample(), State::default());
        home.handle(Action::Up); // tab bar, on Home
        home.handle(Action::Right); // PC
        home.handle(Action::Right); // PS3, the last tab
        assert!(!home.gear);
        home.handle(Action::Right);
        assert!(home.gear, "Right past the last tab lands on the gear");
        assert_eq!(home.handle(Action::Confirm), [Effect::OpenPanel]);
        home.handle(Action::Left);
        assert!(!home.gear);
        assert_eq!(home.current, 2, "back on the last tab");
        home.handle(Action::Right);
        home.handle(Action::Down);
        assert!(!home.gear && home.zone == Zone::Icons);
    }

    #[test]
    fn empty_library_does_not_panic() {
        let mut home = Home::new(Vec::new(), State::default());
        for action in [
            Action::Down,
            Action::Confirm,
            Action::Right,
            Action::PageNext,
            Action::Options,
            Action::Up,
        ] {
            home.handle(action);
        }
        assert_eq!(names(&home), ["Home"]);
        assert!(home.selected_game().is_none());
    }
}
