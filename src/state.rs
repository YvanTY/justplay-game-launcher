//! Things the launcher remembers between runs: favorites and when each game was last played.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub favorites: BTreeSet<String>,
    /// Game id -> Unix seconds of the last launch.
    pub last_played: HashMap<String, u64>,
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
    fn toggling_twice_removes_favorite() {
        let mut state = State::default();
        state.toggle_favorite("a");
        state.toggle_favorite("a");
        assert!(state.favorites.is_empty());
    }
}
