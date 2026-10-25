//! Things the launcher remembers between runs: favorites and when each game was last played.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub favorites: BTreeSet<String>,
    /// Game id -> Unix seconds of the last launch.
    pub last_played: HashMap<String, u64>,
    /// Per-game controller translation chosen in the game's menu (absent = default).
    pub controller_modes: HashMap<String, ControllerMode>,
    pub settings: Settings,
    /// Extra triggers per controller model (SDL GUID), from the Controllers screen.
    pub controller_remaps: HashMap<String, crate::controllers::Profile>,
    /// PC games added from the Add games screen; config.toml is never rewritten.
    pub added_games: Vec<crate::config::PcGame>,
    /// Folders the Add games screen should no longer suggest.
    pub ignored_folders: BTreeSet<PathBuf>,
    /// Where the Add games screen last looked (None: every drive).
    pub scan_place: Option<PathBuf>,
    /// Folders chosen to look in, most recent first, offered again as one-press places.
    pub scan_folders: Vec<PathBuf>,
    /// Where the launcher, and games, are shown (Settings).
    pub launcher_screen: crate::display::ScreenChoice,
    pub game_screen: crate::display::ScreenChoice,
    /// The screen layout the user had before the launcher changed it. Restored when the
    /// launcher closes, or at its next start if it was closed some other way.
    pub display_restore: Option<crate::display::Layout>,
    /// Games whose sound the launcher sent to a screen's output. Windows remembers that per
    /// program, so the next start on another screen puts them back on the default output.
    pub sound_routed: Vec<String>,
    /// Where games' sound goes (Settings).
    pub game_sound: crate::sound_output::SoundChoice,
    /// When GitHub was last asked for a newer release (Unix seconds), and the newer release
    /// found, offered in Settings until this version catches up.
    pub update_checked: u64,
    pub update: Option<crate::update::Update>,
    /// Names the user gave games (game id -> title), applied over the scanned names.
    pub renames: BTreeMap<String, String>,
    /// Emulators set up from the launcher (Emulators screen), next to config.toml's.
    pub added_systems: Vec<crate::config::SystemConfig>,
    /// Game id -> Steam app id its art came from (None: no Steam app). Looked up once.
    pub art_lookups: HashMap<String, Option<u32>>,
    /// Version of the art lookup that filled `art_lookups`; a newer lookup retries the misses.
    pub art_lookup_version: u32,
}

/// User preferences from the Settings page. Percentages in steps of 10.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub sound_volume: u8,
    pub music_volume: u8,
    /// How much darker the background art is drawn (0: as it is).
    pub background_dim: u8,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            sound_volume: 80,
            music_volume: 60,
            background_dim: 0,
        }
    }
}

impl Settings {
    /// Steps a percentage by 10 points, clamped to 0-100.
    pub fn step(value: u8, delta: i32) -> u8 {
        (value as i32 + delta * 10).clamp(0, 100) as u8
    }
}

/// How a PC game receives pads that aren't Xbox pads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerMode {
    /// Every non-Xbox pad gets a virtual Xbox twin.
    #[default]
    Xbox,
    /// PlayStation pads are left for the game to read natively; other pads are translated.
    PlaystationNative,
    /// No translation; the game reads pads itself.
    Off,
}

impl ControllerMode {
    pub fn next(self) -> ControllerMode {
        match self {
            ControllerMode::Xbox => ControllerMode::PlaystationNative,
            ControllerMode::PlaystationNative => ControllerMode::Off,
            ControllerMode::Off => ControllerMode::Xbox,
        }
    }
}

impl State {
    /// A corrupt file is moved aside (not overwritten) so nothing the user had is silently lost.
    pub fn load(path: &Path) -> (State, Option<String>) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (State::default(), None),
            Err(e) => {
                return (
                    State::default(),
                    Some(format!("Could not read {}: {e}", path.display())),
                );
            }
        };
        match serde_json::from_str(strip_bom(&text)) {
            Ok(state) => (state, None),
            Err(e) => {
                let backup = path.with_extension("json.bad");
                let moved = fs::rename(path, &backup).is_ok();
                let note = if moved {
                    format!(", moved to {}", backup.display())
                } else {
                    String::new()
                };
                (
                    State::default(),
                    Some(format!("state.json was unreadable ({e}){note}")),
                )
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Write-then-rename so a crash mid-write can't leave a half-written file.
        let temp: PathBuf = path.with_extension("json.tmp");
        fs::write(&temp, json).map_err(|e| format!("Could not save {}: {e}", temp.display()))?;
        fs::rename(&temp, path).map_err(|e| format!("Could not save {}: {e}", path.display()))
    }

    /// Forgets the lookups made by an older, weaker lookup, so games still missing art are
    /// looked up again (games with complete art aren't). True when something changed.
    pub fn retry_art_misses(&mut self, version: u32) -> bool {
        if self.art_lookup_version >= version {
            return false;
        }
        self.art_lookups.clear();
        self.art_lookup_version = version;
        true
    }

    pub fn toggle_favorite(&mut self, id: &str) {
        if !self.favorites.remove(id) {
            self.favorites.insert(id.to_string());
        }
    }
}

/// Some editors (and PowerShell 5's `-Encoding utf8`) prefix files with a byte-order mark.
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "game-launcher-test-state-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("state.json")
    }

    #[test]
    fn round_trips_favorites_and_history() {
        let path = temp_path("roundtrip");
        let mut state = State::default();
        state.toggle_favorite("ps3:BLUS30405");
        state
            .last_played
            .insert("pc:God of War".into(), 1_700_000_000);
        state.save(&path).unwrap();

        let (loaded, problem) = State::load(&path);
        assert!(problem.is_none());
        assert!(loaded.favorites.contains("ps3:BLUS30405"));
        assert_eq!(loaded.last_played["pc:God of War"], 1_700_000_000);
    }

    #[test]
    fn missing_file_is_a_fresh_start() {
        let (state, problem) = State::load(&temp_path("missing"));
        assert!(problem.is_none() && state.favorites.is_empty());
    }

    #[test]
    fn corrupt_file_is_kept_aside_and_reported() {
        let path = temp_path("corrupt");
        fs::write(&path, "{ not json").unwrap();
        let (state, problem) = State::load(&path);
        assert!(state.favorites.is_empty());
        assert!(problem.unwrap().contains("unreadable"));
        assert!(path.with_extension("json.bad").exists());
    }

    #[test]
    fn file_with_byte_order_mark_loads() {
        let path = temp_path("bom");
        fs::write(&path, "\u{feff}{\"favorites\": [\"pc:X\"]}").unwrap();
        let (state, problem) = State::load(&path);
        assert!(problem.is_none(), "{problem:?}");
        assert!(state.favorites.contains("pc:X"));
    }

    #[test]
    fn controller_modes_round_trip_and_cycle() {
        let path = temp_path("modes");
        let mut state = State::default();
        state
            .controller_modes
            .insert("pc:God of War".into(), ControllerMode::PlaystationNative);
        state.save(&path).unwrap();
        let (loaded, _) = State::load(&path);
        assert_eq!(
            loaded.controller_modes["pc:God of War"],
            ControllerMode::PlaystationNative
        );
        assert_eq!(ControllerMode::Off.next(), ControllerMode::Xbox);
    }

    #[test]
    fn settings_default_and_clamp() {
        assert_eq!(Settings::default().sound_volume, 80);
        assert_eq!(Settings::step(95, 1), 100);
        assert_eq!(Settings::step(0, -1), 0);
        assert_eq!(Settings::step(60, -1), 50);
        // Older state files without a settings section still load with defaults.
        let state: State = serde_json::from_str(r#"{"favorites": []}"#).unwrap();
        assert_eq!(state.settings, Settings::default());
    }

    #[test]
    fn toggling_twice_removes_favorite() {
        let mut state = State::default();
        state.toggle_favorite("a");
        state.toggle_favorite("a");
        assert!(state.favorites.is_empty());
    }
}
