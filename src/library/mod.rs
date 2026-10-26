//! Builds the game list from the config: PC games, RPCS3 and folder-scanned emulators.

mod cache;
pub mod discover;
pub mod drives;
mod emulator;
mod iso;
mod rpcs3;
mod sfo;
pub mod snd0;

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
    /// "None" was chosen for the background: a plain backdrop, not the cover blown up.
    pub hero_off: bool,
    /// A PS3 game's own menu music, converted in the background when no music is set.
    pub snd0: Option<Snd0>,
}

/// Where a PS3 game's menu music (SND0.AT3, the tune its XMB icon plays) is.
#[derive(Debug, Clone, PartialEq)]
pub enum Snd0 {
    File(PathBuf),
    /// Inside the game's disc image, at PS3_GAME/SND0.AT3.
    Iso(PathBuf),
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
    /// The name it had before the user renamed it (None: not renamed).
    pub original_title: Option<String>,
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

/// `added` are the games added from the launcher and `renames` the names the user gave games
/// (game id -> title), both kept in state.json. `data_dir` holds the art folder and the
/// scanner cache.
pub fn load(
    config: &Config,
    added: &[PcGame],
    renames: &BTreeMap<String, String>,
    data_dir: &Path,
) -> Library {
    let cache_path = data_dir.join("games-cache.json");
    let mut cache = Cache::load(&cache_path);
    // Network and optical drives are left out: asking an offline share can stall for seconds.
    for letter in drives::scannable() {
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
    // Renamed before the art is looked up: the art folder carries the name the user sees.
    for game in &mut games {
        if let Some(title) = renames.get(&game.id).filter(|t| **t != game.title) {
            game.original_title = Some(std::mem::replace(&mut game.title, title.clone()));
        }
    }
    let art_dir = data_dir.join("art");
    for game in &mut games {
        apply_art_folder(game, &art_dir);
        // A PS3 game's menu music converted on an earlier run; music in the art folder wins.
        let converted = snd0::cached(data_dir, &game.id);
        if game.art.music.is_none() && game.art.snd0.is_some() && converted.is_file() {
            game.art.music = Some(converted);
        }
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
            hero_off: false,
            snd0: None,
        },
        xinput: entry.xinput,
        requires: vec![entry.exe.clone()],
        missing: None,
        removable,
        original_title: None,
    })
}

/// Art the user drops in `art\<title>\` wins over whatever a scanner found. A `<slot>.none`
/// file (the "None" choice of the Choose art screen) means no image for that slot.
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
        if dir.join(format!("{stem}.none")).exists() {
            *slot = None;
        } else if let Some(found) = find_image(&dir, stem) {
            *slot = Some(found);
        }
    }
    game.art.hero_off = art_turned_off(&dir, "hero");
    if art_turned_off(&dir, "music") {
        // "None" chosen for music: not even a PS3 game's own tune.
        game.art.music = None;
        game.art.snd0 = None;
    } else if let Some(music) = crate::music::find_music(&dir) {
        game.art.music = Some(music);
    }
}

/// Makes `staged` (a trailer's sound, made beside it) the game's `music.ogg`, or with None
/// turns music off. The music there before moves to `_previous`, so nothing is lost and no
/// other file shadows the new one. Returns the music file now in use.
pub fn set_music(dir: &Path, staged: Option<&Path>) -> Result<Option<PathBuf>, String> {
    let fail = |e: std::io::Error| format!("Could not change the music in {}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(fail)?;
    let marker = dir.join("music.none");
    for extension in ["mp3", "ogg", "flac", "wav"] {
        let file = dir.join(format!("music.{extension}"));
        if !file.is_file() {
            continue;
        }
        let previous = dir.join("_previous");
        std::fs::create_dir_all(&previous).map_err(fail)?;
        std::fs::rename(&file, previous.join(format!("music.{extension}"))).map_err(fail)?;
    }
    match staged {
        Some(staged) => {
            let music = dir.join("music.ogg");
            std::fs::rename(staged, &music).map_err(fail)?;
            let _ = std::fs::remove_file(&marker);
            Ok(Some(music))
        }
        None => {
            std::fs::write(&marker, b"").map_err(fail)?;
            Ok(None)
        }
    }
}

/// The user chose "None" for this slot: automatic art must not fill it again.
pub fn art_turned_off(dir: &Path, stem: &str) -> bool {
    dir.join(format!("{stem}.none")).exists()
}

/// Makes `source` this slot's art in `dir` (None: a `.none` marker, no image). Nothing is lost:
/// the first image ever replaced stays in `_original`, the most recent one in `_previous` (so
/// art dropped in by hand survives one more change), and the new file is copied before the old
/// one moves, so a failed copy leaves the slot as it was.
pub fn replace_art(dir: &Path, stem: &str, source: Option<&Path>) -> Result<(), String> {
    const IMAGES: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
    let fail = |e: std::io::Error| format!("Could not change the art in {}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(fail)?;
    let staged = match source {
        Some(source) => {
            let extension = source
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .filter(|e| IMAGES.contains(&e.as_str()))
                .ok_or_else(|| format!("{} is not a PNG, JPEG or WebP image", source.display()))?;
            let staged = dir.join(format!("{stem}.{extension}.part"));
            std::fs::copy(source, &staged).map_err(fail)?;
            Some((staged, dir.join(format!("{stem}.{extension}"))))
        }
        None => None,
    };
    let original = dir.join("_original");
    let previous = dir.join("_previous");
    for extension in IMAGES.iter().chain(&["none"]) {
        let old = dir.join(format!("{stem}.{extension}"));
        if !old.exists() {
            continue;
        }
        if *extension == "none" {
            std::fs::remove_file(&old).map_err(fail)?;
            continue;
        }
        let keep = if find_image(&original, stem).is_none() {
            &original
        } else {
            // Only the latest replaced image is kept here.
            for stale in IMAGES {
                let _ = std::fs::remove_file(previous.join(format!("{stem}.{stale}")));
            }
            &previous
        };
        std::fs::create_dir_all(keep).map_err(fail)?;
        std::fs::rename(&old, keep.join(format!("{stem}.{extension}"))).map_err(fail)?;
    }
    match staged {
        Some((staged, target)) => std::fs::rename(&staged, &target).map_err(fail)?,
        None => std::fs::write(dir.join(format!("{stem}.none")), b"").map_err(fail)?,
    }
    Ok(())
}

pub(crate) fn find_image(dir: &Path, stem: &str) -> Option<PathBuf> {
    ["png", "jpg", "jpeg", "webp"]
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|p| p.is_file())
}

pub fn safe_file_name(title: &str) -> String {
    let name: String = title
        .chars()
        .filter(|c| !r#"\/:*?"<>|"#.contains(*c))
        .collect();
    // Windows drops trailing dots and spaces, and "." or ".." would name a parent folder.
    let name = name.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if name.is_empty() {
        "Untitled".to_string()
    } else {
        name.to_string()
    }
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
        assert_eq!(safe_file_name(".."), "Untitled");
        assert_eq!(safe_file_name("Stray."), "Stray");
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
        let library = load(&config, &[], &BTreeMap::new(), &data);
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
    fn replacing_art_keeps_the_first_original_and_none_clears_the_slot() {
        let data = temp_data_dir("replace");
        let dir = data.join("art").join("Game");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("icon.jpg"), b"first").unwrap();
        let picked = data.join("picked.png");
        std::fs::write(&picked, b"second").unwrap();
        replace_art(&dir, "icon", Some(&picked)).unwrap();
        assert_eq!(std::fs::read(dir.join("icon.png")).unwrap(), b"second");
        assert!(!dir.join("icon.jpg").exists());
        assert_eq!(
            std::fs::read(dir.join("_original").join("icon.jpg")).unwrap(),
            b"first"
        );
        replace_art(&dir, "icon", None).unwrap();
        assert!(art_turned_off(&dir, "icon") && !dir.join("icon.png").exists());
        assert_eq!(
            std::fs::read(dir.join("_original").join("icon.jpg")).unwrap(),
            b"first",
            "the first original stays"
        );
        assert_eq!(
            std::fs::read(dir.join("_previous").join("icon.png")).unwrap(),
            b"second",
            "and the latest replaced image is kept too"
        );
        // A copy that can't happen changes nothing.
        let missing = data.join("gone.png");
        assert!(replace_art(&dir, "icon", Some(&missing)).is_err());
        assert!(art_turned_off(&dir, "icon"));
        assert!(replace_art(&dir, "icon", Some(&data.join("x.ico"))).is_err());
        let mut game = Game {
            id: "pc:Game".into(),
            title: "Game".into(),
            platform: "PC".into(),
            launch: Launch {
                exe: "x.exe".into(),
                args: vec![],
            },
            art: Art {
                icon: Some("ICON0.PNG".into()),
                ..Art::default()
            },
            xinput: true,
            requires: vec![],
            missing: None,
            removable: false,
            original_title: None,
        };
        apply_art_folder(&mut game, &data.join("art"));
        assert_eq!(game.art.icon, None, "None wins over the scanner's icon");
        let _ = std::fs::remove_dir_all(&data);
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
        let library = load(
            &config,
            std::slice::from_ref(&added),
            &BTreeMap::new(),
            &data,
        );
        assert_eq!(library.games.len(), 1);
        assert!(library.games[0].removable);
        assert_eq!(library.games[0].missing, None);
        // Renamed: the new name is shown and its art folder used; the original is kept.
        let art = data.join("art").join("My Game");
        std::fs::create_dir_all(&art).unwrap();
        std::fs::write(art.join("icon.png"), b"png").unwrap();
        let renames = BTreeMap::from([("pc:Added".to_string(), "My Game".to_string())]);
        let library = load(&config, &[added], &renames, &data);
        let game = &library.games[0];
        assert_eq!(game.title, "My Game");
        assert_eq!(game.original_title.as_deref(), Some("Added"));
        assert_eq!(game.art.icon, Some(art.join("icon.png")));
        let _ = std::fs::remove_dir_all(&data);
    }
}
