//! Builds the game list from the config: PC games, RPCS3 and folder-scanned emulators.

mod cache;
pub mod drives;
mod emulator;
mod rpcs3;
mod sfo;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{Config, PcGame};
use cache::Cache;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Art {
    pub icon: Option<PathBuf>,
    pub hero: Option<PathBuf>,
    pub logo: Option<PathBuf>,
    /// Background music played while the game is selected.
    pub music: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Game {
    /// Stable key for favorites and play history, e.g. `ps3:BLUS30405`.
    pub id: String,
    pub title: String,
    /// Tab the game belongs to: "PC", "PS3", or a configured system name.
    pub platform: String,
    pub launch: Launch,
    pub art: Art,
    /// Drive a virtual Xbox pad from the user's pad while this game runs.
    pub xinput: bool,
    /// Files that must exist to play: the exe, or the emulator plus the game file.
    pub requires: Vec<PathBuf>,
    /// Why the game can't be played right now; the tile is greyed out while this is set.
    pub missing: Option<Missing>,
    /// Added from the launcher (not config.toml), so it can be removed from the launcher.
    pub removable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Missing {
    /// The game lives on a drive that isn't connected (an unplugged external drive).
    Drive { letter: char, label: Option<String> },
    /// The drive is there but this file isn't.
    Files(PathBuf),
}

impl Missing {
    /// What to do about it, short enough for the info line under Play.
    pub fn describe(&self) -> String {
        match self {
            Missing::Drive {
                letter,
                label: Some(label),
            } => format!("Connect drive {letter}: ({label})"),
            Missing::Drive {
                letter,
                label: None,
            } => format!("Connect drive {letter}:"),
            Missing::Files(_) => "Game files not found".into(),
        }
    }

    /// The message shown when Play is pressed anyway.
    pub fn play_message(&self, title: &str) -> String {
        match self {
            Missing::Drive { .. } => format!("{} to play {title}", self.describe()),
            Missing::Files(path) => format!("{title}: files not found at {}", path.display()),
        }
    }
}

/// None when every required file exists. An unplugged drive explains everything on it, so it is
/// reported before files missing elsewhere.
pub fn check(requires: &[PathBuf], label: impl Fn(char) -> Option<String>) -> Option<Missing> {
    let absent: Vec<&PathBuf> = requires.iter().filter(|p| !p.exists()).collect();
    if let Some(letter) = absent
        .iter()
        .filter_map(|p| drives::letter_of(p))
        .find(|&l| !drives::is_connected(l))
    {
        return Some(Missing::Drive {
            letter,
            label: label(letter),
        });
    }
    absent.first().map(|p| Missing::Files((*p).clone()))
}

pub struct Library {
    pub games: Vec<Game>,
    /// Problems worth telling the user about (missing files, unreadable lists).
    pub warnings: Vec<String>,
    /// Drive letter -> name, including drives that are unplugged now.
    pub drive_labels: BTreeMap<String, String>,
}

/// `added` are the games added from the launcher (kept in state.json). `data_dir` holds the art
/// folder and the scanner cache.
pub fn load(config: &Config, added: &[PcGame], data_dir: &Path) -> Library {
    let cache_path = data_dir.join("games-cache.json");
    let mut cache = Cache::load(&cache_path);
    for letter in drives::connected() {
        if let Some(label) = drives::label(letter) {
            cache.drive_labels.insert(letter.to_string(), label);
        }
    }
    let mut warnings = Vec::new();
    let mut games: Vec<Game> = config
        .games
        .iter()
        .filter_map(|g| pc_game(g, false, &mut warnings))
        .collect();
    games.extend(added.iter().filter_map(|g| pc_game(g, true, &mut warnings)));
    let mut scanned = Vec::new();
    if let Some(rpcs3) = &config.rpcs3 {
        scanned.extend(rpcs3::scan(rpcs3, &cache, &mut warnings));
    }
    for system in &config.systems {
        scanned.extend(emulator::scan(system, &cache, &mut warnings));
    }
    for game in games.iter_mut().chain(scanned.iter_mut()) {
        game.missing = check(&game.requires, |l| cache.label(l));
    }
    for game in &games {
        if let Some(Missing::Files(path)) = &game.missing {
            warnings.push(format!(
                "{}: exe not found at {}",
                game.title,
                path.display()
            ));
        }
    }
    cache.update(&scanned, &data_dir.join("cache").join("icons"));
    if let Err(e) = cache.save(&cache_path) {
        crate::diag::log(&e);
    }
    games.extend(scanned);
    let art_dir = data_dir.join("art");
    for game in &mut games {
        apply_art_folder(game, &art_dir);
    }
    games.sort_by_cached_key(|g| g.title.to_lowercase());
    Library {
        games,
        warnings,
        drive_labels: cache.drive_labels,
    }
}

/// The exe may be missing (unplugged drive); the game is still listed and greyed out.
fn pc_game(entry: &PcGame, removable: bool, warnings: &mut Vec<String>) -> Option<Game> {
    let args = match split_args(&entry.args) {
        Ok(args) => args,
        Err(e) => {
            warnings.push(format!("{}: {e}", entry.name));
            return None;
        }
    };
    Some(Game {
        id: format!("pc:{}", entry.name),
        title: entry.name.clone(),
        platform: "PC".into(),
        launch: Launch {
            exe: entry.exe.clone(),
            args,
        },
        art: Art {
            icon: entry.icon.clone(),
            hero: entry.hero.clone(),
            logo: entry.logo.clone(),
            music: None,
        },
        xinput: entry.xinput,
        requires: vec![entry.exe.clone()],
        missing: None,
        removable,
    })
}

/// Art the user drops in `art\<title>\` wins over whatever a scanner found.
pub fn apply_art_folder(game: &mut Game, art_dir: &Path) {
    let dir = art_dir.join(safe_file_name(&game.title));
    if !dir.is_dir() {
        return;
    }
    for (slot, stem) in [
        (&mut game.art.icon, "icon"),
        (&mut game.art.hero, "hero"),
        (&mut game.art.logo, "logo"),
    ] {
        if let Some(found) = find_image(&dir, stem) {
            *slot = Some(found);
        }
    }
    if let Some(music) = crate::music::find_music(&dir) {
        game.art.music = Some(music);
    }
}

pub(crate) fn find_image(dir: &Path, stem: &str) -> Option<PathBuf> {
    ["png", "jpg", "jpeg", "webp"]
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|p| p.is_file())
}

pub fn safe_file_name(title: &str) -> String {
    title
        .chars()
        .filter(|c| !r#"\/:*?"<>|"#.contains(*c))
        .collect::<String>()
        .trim()
        .to_string()
}

/// Splits an argument template on spaces, honouring double quotes, then fills in `{rom}`.
pub(crate) fn expand_args(template: &str, rom: &Path) -> Result<Vec<String>, String> {
    let rom = rom.to_string_lossy();
    Ok(split_args(template)?
        .into_iter()
        .map(|arg| arg.replace("{rom}", &rom))
        .collect())
}

pub(crate) fn split_args(template: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for c in template.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if in_quotes {
        return Err(format!("unclosed quote in arguments: {template}"));
    }
    if has_token {
        args.push(current);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_rom_inside_quotes_as_one_argument() {
        let args = expand_args(
            r#"--no-gui "{rom}""#,
            Path::new(r"E:\Games\God of War III.iso"),
        )
        .unwrap();
        assert_eq!(args, ["--no-gui", r"E:\Games\God of War III.iso"]);
    }

    #[test]
    fn splits_plain_and_empty_arguments() {
        assert_eq!(
            split_args("-batch  -fullscreen").unwrap(),
            ["-batch", "-fullscreen"]
        );
        assert!(split_args("   ").unwrap().is_empty());
        assert_eq!(split_args(r#"-L "" x"#).unwrap(), ["-L", "", "x"]);
    }

    #[test]
    fn unclosed_quote_is_an_error() {
        assert!(split_args(r#"--fullscreen "{rom}"#).is_err());
    }

    #[test]
    fn titles_become_safe_folder_names() {
        assert_eq!(
            safe_file_name("God of War: Ascension"),
            "God of War Ascension"
        );
        assert_eq!(safe_file_name("What? <Yes>"), "What Yes");
    }

    fn temp_data_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-lib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_pc_exe_is_listed_greyed_and_reported() {
        let data = temp_data_dir("missing");
        let exe = data.join("nope").join("ghost.exe");
        let config = crate::config::parse(&format!(
            "[[games]]\nname = \"Ghost\"\nexe = '{}'\n",
            exe.display()
        ))
        .unwrap();
        let library = load(&config, &[], &data);
        assert_eq!(library.games.len(), 1);
        assert_eq!(library.games[0].missing, Some(Missing::Files(exe)));
        assert!(library.warnings[0].contains("Ghost"));
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn an_unplugged_drive_wins_over_missing_files() {
        // A letter with no drive behind it.
        let free = ('G'..='Z')
            .rev()
            .find(|&l| !drives::is_connected(l))
            .unwrap();
        let on_free = PathBuf::from(format!(r"{free}:\Games\x.exe"));
        let elsewhere = std::env::temp_dir().join("gl-surely-missing").join("y.exe");
        let missing = check(&[elsewhere.clone(), on_free], |_| Some("Seagate".into()));
        assert_eq!(
            missing,
            Some(Missing::Drive {
                letter: free,
                label: Some("Seagate".into())
            })
        );
        assert_eq!(
            missing.unwrap().describe(),
            format!("Connect drive {free}: (Seagate)")
        );
        assert_eq!(
            check(std::slice::from_ref(&elsewhere), |_| None),
            Some(Missing::Files(elsewhere))
        );
        assert_eq!(check(&[std::env::temp_dir()], |_| None), None);
    }

    #[test]
    fn added_games_are_listed_and_removable() {
        let data = temp_data_dir("added");
        let config = crate::config::parse("").unwrap();
        let added = PcGame {
            name: "Added".into(),
            exe: std::env::current_exe().unwrap(),
            args: String::new(),
            icon: None,
            hero: None,
            logo: None,
            xinput: true,
        };
        let library = load(&config, &[added], &data);
        assert_eq!(library.games.len(), 1);
        assert!(library.games[0].removable);
        assert_eq!(library.games[0].missing, None);
        let _ = std::fs::remove_dir_all(&data);
    }
}
