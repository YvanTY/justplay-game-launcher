//! `games-cache.json`: what the scanners (RPCS3, emulators) last saw, so their games keep a name,
//! a launch command and an icon while their drive is unplugged; plus the names of drives, so a
//! message can say "Connect drive E: (Seagate)" when E: isn't there to ask.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Art, Game, Launch};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Cache {
    /// Drive letter ("E") -> name, remembered while the drive was connected.
    pub drive_labels: BTreeMap<String, String>,
    pub games: Vec<CachedGame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGame {
    pub id: String,
    pub title: String,
    pub platform: String,
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub requires: Vec<PathBuf>,
    /// A copy kept in the launcher's folder: the original sits on the drive that may be gone.
    pub icon: Option<PathBuf>,
    pub xinput: bool,
}

impl Cache {
    /// A missing or damaged cache only costs names and icons of offline games, so it starts empty.
    pub fn load(path: &Path) -> Cache {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(crate::state::strip_bom(&text)).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, json).map_err(|e| format!("Could not save {}: {e}", temp.display()))?;
        fs::rename(&temp, path).map_err(|e| format!("Could not save {}: {e}", path.display()))
    }

    pub fn label(&self, letter: char) -> Option<String> {
        self.drive_labels.get(&letter.to_string()).cloned()
    }

    pub fn game(&self, id: &str) -> Option<Game> {
        self.games
            .iter()
            .find(|g| g.id == id)
            .map(CachedGame::to_game)
    }

    pub fn games_on(&self, platform: &str) -> Vec<Game> {
        self.games
            .iter()
            .filter(|g| g.platform == platform)
            .map(CachedGame::to_game)
            .collect()
    }

    /// Replaces the cached scanner games with what was just found. Games that are offline right
    /// now came from the cache in the first place, so their entries are kept as they were.
    pub fn update(&mut self, scanned: &[Game], icon_dir: &Path) {
        let previous = std::mem::take(&mut self.games);
        for game in scanned {
            if game.missing.is_some() {
                if let Some(old) = previous.iter().find(|g| g.id == game.id) {
                    self.games.push(old.clone());
                }
                continue;
            }
            let icon = game
                .art
                .icon
                .as_deref()
                .and_then(|icon| copy_icon(icon, &game.id, icon_dir));
            self.games.push(CachedGame {
                id: game.id.clone(),
                title: game.title.clone(),
                platform: game.platform.clone(),
                exe: game.launch.exe.clone(),
                args: game.launch.args.clone(),
                requires: game.requires.clone(),
                icon,
                xinput: game.xinput,
            });
        }
    }
}

impl CachedGame {
    fn to_game(&self) -> Game {
        Game {
            id: self.id.clone(),
            title: self.title.clone(),
            platform: self.platform.clone(),
            launch: Launch {
                exe: self.exe.clone(),
                args: self.args.clone(),
            },
            art: Art {
                icon: self.icon.clone(),
                ..Art::default()
            },
            xinput: self.xinput,
            requires: self.requires.clone(),
            missing: None,
            removable: false,
        }
    }
}

/// Copies the icon once; an unchanged size means it is already there.
fn copy_icon(source: &Path, id: &str, icon_dir: &Path) -> Option<PathBuf> {
    let extension = source.extension()?.to_string_lossy().to_lowercase();
    let name: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let target = icon_dir.join(format!("{name}.{extension}"));
    let size = |p: &Path| fs::metadata(p).map(|m| m.len()).ok();
    if size(&target).is_none() || size(&target) != size(source) {
        fs::create_dir_all(icon_dir).ok()?;
        fs::copy(source, &target).ok()?;
    }
    Some(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Missing;

    fn game(id: &str, icon: Option<PathBuf>) -> Game {
        Game {
            id: id.into(),
            title: format!("Title {id}"),
            platform: "PS3".into(),
            launch: Launch {
                exe: PathBuf::from(r"E:\rpcs3\rpcs3.exe"),
                args: vec!["--no-gui".into()],
            },
            art: Art {
                icon,
                ..Art::default()
            },
            xinput: false,
            requires: vec![PathBuf::from(r"E:\rpcs3\rpcs3.exe")],
            missing: None,
            removable: false,
        }
    }

    #[test]
    fn offline_games_keep_their_entry_and_icons_are_copied() {
        let dir = std::env::temp_dir().join(format!("gl-cache-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let icon = dir.join("ICON0.PNG");
        fs::write(&icon, b"png bytes").unwrap();

        let mut cache = Cache::default();
        cache.update(
            &[game("ps3:A", Some(icon)), game("ps3:B", None)],
            &dir.join("icons"),
        );
        let copied = cache.games[0].icon.clone().expect("icon copied");
        assert_eq!(fs::read(&copied).unwrap(), b"png bytes");

        // Next run: A's drive is unplugged (served from the cache), B was deleted.
        let mut offline = cache.game("ps3:A").unwrap();
        offline.missing = Some(Missing::Drive {
            letter: 'E',
            label: None,
        });
        cache.update(&[offline], &dir.join("icons"));
        assert_eq!(cache.games.len(), 1);
        assert_eq!(cache.games[0].icon.as_ref(), Some(&copied));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_cache_starts_empty() {
        let path = std::env::temp_dir().join(format!("gl-cache-bad-{}.json", std::process::id()));
        fs::write(&path, "{ not json").unwrap();
        assert!(Cache::load(&path).games.is_empty());
        let _ = fs::remove_file(&path);
    }
}
