//! Builds the game list from the config: PC games, RPCS3 and folder-scanned emulators.

mod emulator;
mod rpcs3;
mod sfo;

use std::path::{Path, PathBuf};

use crate::config::{Config, PcGame};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Art {
    pub icon: Option<PathBuf>,
    pub hero: Option<PathBuf>,
    pub logo: Option<PathBuf>,
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
}

pub struct Library {
    pub games: Vec<Game>,
    /// Problems worth telling the user about (missing files, unreadable lists).
    pub warnings: Vec<String>,
}

pub fn load(config: &Config, art_dir: &Path) -> Library {
    let mut warnings = Vec::new();
    let mut games: Vec<Game> = config
        .games
        .iter()
        .filter_map(|g| pc_game(g, &mut warnings))
        .collect();
    if let Some(rpcs3) = &config.rpcs3 {
        games.extend(rpcs3::scan(rpcs3, &mut warnings));
    }
    for system in &config.systems {
        games.extend(emulator::scan(system, &mut warnings));
    }
    for game in &mut games {
        apply_art_folder(game, art_dir);
    }
    games.sort_by_cached_key(|g| g.title.to_lowercase());
    Library { games, warnings }
}

fn pc_game(entry: &PcGame, warnings: &mut Vec<String>) -> Option<Game> {
    if !entry.exe.is_file() {
        warnings.push(format!(
            "{}: exe not found at {}",
            entry.name,
            entry.exe.display()
        ));
        return None;
    }
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
        },
        xinput: entry.xinput,
    })
}

/// Art the user drops in `art\<title>\` wins over whatever a scanner found.
fn apply_art_folder(game: &mut Game, art_dir: &Path) {
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
}

pub(crate) fn find_image(dir: &Path, stem: &str) -> Option<PathBuf> {
    ["png", "jpg", "jpeg", "webp"]
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|p| p.is_file())
}

fn safe_file_name(title: &str) -> String {
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

    #[test]
    fn missing_pc_exe_is_reported_not_listed() {
        let config =
            crate::config::parse("[[games]]\nname = \"Ghost\"\nexe = 'Z:\\nope\\ghost.exe'\n")
                .unwrap();
        let library = load(&config, Path::new("Z:\\no-art"));
        assert!(library.games.is_empty());
        assert!(library.warnings[0].contains("Ghost"));
    }
}
