//! Imports PS3 games from RPCS3's own game list (`config/games.yml`) and installed HDD games.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::{Art, Game, Launch, expand_args, sfo};
use crate::config::Rpcs3Config;

pub fn scan(config: &Rpcs3Config, warnings: &mut Vec<String>) -> Vec<Game> {
    let Some(root) = config.exe.parent().filter(|_| config.exe.is_file()) else {
        warnings.push(format!("RPCS3 not found at {}", config.exe.display()));
        return Vec::new();
    };
    let hdd_games = root.join("dev_hdd0").join("game");
    let hidden: HashSet<&str> = config.hide.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    let mut games = Vec::new();

    let list_path = root.join("config").join("games.yml");
    match fs::read_to_string(&list_path) {
        Ok(text) => {
            for (serial, path) in parse_games_yml(&text) {
                if hidden.contains(serial.as_str()) || !seen.insert(serial.clone()) {
                    continue;
                }
                let path = PathBuf::from(path.trim_end_matches(['/', '\\']));
                if !path.exists() {
                    warnings.push(format!("PS3 game {serial} not found at {}", path.display()));
                    continue;
                }
                // Disc games keep their data in PS3_GAME; RPCS3 also stores title data for
                // installed updates under dev_hdd0/game/<serial>, which is the only source for ISOs.
                let mut info_dirs = disc_dirs(&path);
                info_dirs.push(hdd_games.join(&serial));
                push_game(&mut games, config, &serial, &path, &info_dirs, warnings);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warnings.push(format!("Could not read {}: {e}", list_path.display())),
    }

    // PSN titles installed into RPCS3 itself; "HG" excludes patches ("GD") and save data.
    if let Ok(entries) = fs::read_dir(&hdd_games) {
        for dir in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            let Some(values) = read_sfo(&dir) else {
                continue;
            };
            let serial = values.get("TITLE_ID").cloned().unwrap_or_default();
            if values.get("CATEGORY").map(String::as_str) != Some("HG")
                || serial.is_empty()
                || hidden.contains(serial.as_str())
                || !seen.insert(serial.clone())
            {
                continue;
            }
            let eboot = dir.join("USRDIR").join("EBOOT.BIN");
            if !eboot.is_file() {
                warnings.push(format!(
                    "PS3 game {serial} is installed in RPCS3 but has no EBOOT.BIN"
                ));
                continue;
            }
            push_game(
                &mut games,
                config,
                &serial,
                &eboot,
                std::slice::from_ref(&dir),
                warnings,
            );
        }
    }
    games
}

fn push_game(
    games: &mut Vec<Game>,
    config: &Rpcs3Config,
    serial: &str,
    boot_path: &Path,
    info_dirs: &[PathBuf],
    warnings: &mut Vec<String>,
) {
    let args = match expand_args(&config.args, boot_path) {
        Ok(args) => args,
        Err(e) => {
            warnings.push(format!("RPCS3: {e}"));
            return;
        }
    };
    let title = info_dirs
        .iter()
        .filter_map(|d| read_sfo(d))
        .find_map(|v| v.get("TITLE").map(|t| sfo::clean_title(t)))
        .unwrap_or_else(|| serial.to_string());
    let first = |name: &str| info_dirs.iter().map(|d| d.join(name)).find(|p| p.is_file());
    games.push(Game {
        id: format!("ps3:{serial}"),
        title,
        platform: "PS3".into(),
        launch: Launch {
            exe: config.exe.clone(),
            args,
        },
        art: Art {
            icon: first("ICON0.PNG"),
            hero: first("PIC1.PNG"),
            logo: first("PIC0.PNG"),
        },
    });
}

/// Where a disc game's PS3_GAME folder may be: inside a game folder, or (for ISOs)
/// beside the image when the disc was also extracted.
fn disc_dirs(path: &Path) -> Vec<PathBuf> {
    let candidates: Vec<PathBuf> = if path.is_dir() {
        vec![path.join("PS3_GAME"), path.to_path_buf()]
    } else {
        path.ancestors()
            .skip(1)
            .take(2)
            .map(|a| a.join("PS3_GAME"))
            .collect()
    };
    candidates
        .into_iter()
        .filter(|d| d.join("PARAM.SFO").is_file())
        .collect()
}

fn read_sfo(dir: &Path) -> Option<std::collections::HashMap<String, String>> {
    let bytes = fs::read(dir.join("PARAM.SFO")).ok()?;
    sfo::parse(&bytes).ok()
}

/// games.yml is a flat `SERIAL: path` map; a full YAML parser isn't worth the weight.
fn parse_games_yml(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            (key.trim().to_string(), value.to_string())
        })
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rpcs3_game_list() {
        let yml = "BCES01741: C:/Games/God Of War Ascension/\n\
                   BLUS30405: \"E:/Yvan/Games/Dante's Inferno/Dante: Divine Edition/\"\n\
                   # comment: ignored\n\n";
        let entries = parse_games_yml(yml);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            ("BCES01741".into(), "C:/Games/God Of War Ascension/".into())
        );
        assert_eq!(
            entries[1].1,
            "E:/Yvan/Games/Dante's Inferno/Dante: Divine Edition/"
        );
    }

    #[test]
    fn imports_disc_folder_game_with_art() {
        let root = test_dir("rpcs3_disc");
        let exe = root.join("emu").join("rpcs3.exe");
        let game = root.join("Dante");
        let ps3_game = game.join("PS3_GAME");
        fs::create_dir_all(exe.parent().unwrap().join("config")).unwrap();
        fs::create_dir_all(&ps3_game).unwrap();
        fs::write(&exe, b"").unwrap();
        fs::write(
            ps3_game.join("PARAM.SFO"),
            sfo::build(&[("CATEGORY", "DG"), ("TITLE", "Dante's Inferno")]),
        )
        .unwrap();
        fs::write(ps3_game.join("ICON0.PNG"), b"").unwrap();
        fs::write(ps3_game.join("PIC1.PNG"), b"").unwrap();
        let yml = format!(
            "BLUS30405: {}/\nBCUS00000: {}/missing/\n",
            game.display(),
            root.display()
        );
        fs::write(exe.parent().unwrap().join("config").join("games.yml"), yml).unwrap();

        let config = Rpcs3Config {
            exe: exe.clone(),
            hide: vec![],
            args: r#"--no-gui "{rom}""#.into(),
        };
        let mut warnings = Vec::new();
        let games = scan(&config, &mut warnings);

        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title, "Dante's Inferno");
        assert_eq!(games[0].id, "ps3:BLUS30405");
        assert_eq!(
            games[0].launch.args,
            ["--no-gui".to_string(), game.display().to_string()]
        );
        assert_eq!(games[0].art.hero, Some(ps3_game.join("PIC1.PNG")));
        assert!(games[0].art.logo.is_none());
        assert_eq!(
            warnings.len(),
            1,
            "missing game should be reported: {warnings:?}"
        );
    }

    #[test]
    fn imports_hdd_games_but_skips_patches_and_hidden() {
        let root = test_dir("rpcs3_hdd");
        let exe = root.join("rpcs3.exe");
        fs::write(&exe, b"").unwrap();
        // (serial, category, has EBOOT.BIN)
        for (serial, category, bootable) in [
            ("NPUB00001", "HG", true),
            ("BCUS98111", "GD", true),
            ("NPUO70219", "HG", true),
            ("NPUB00002", "HG", false),
        ] {
            let dir = root.join("dev_hdd0").join("game").join(serial);
            fs::create_dir_all(dir.join("USRDIR")).unwrap();
            let sfo = sfo::build(&[
                ("CATEGORY", category),
                ("TITLE", serial),
                ("TITLE_ID", serial),
            ]);
            fs::write(dir.join("PARAM.SFO"), sfo).unwrap();
            if bootable {
                fs::write(dir.join("USRDIR").join("EBOOT.BIN"), b"").unwrap();
            }
        }
        let config = Rpcs3Config {
            exe,
            hide: vec!["NPUO70219".into()],
            args: r#""{rom}""#.into(),
        };
        let mut warnings = Vec::new();
        let games = scan(&config, &mut warnings);
        let ids: Vec<&str> = games.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["ps3:NPUB00001"]);
        assert!(games[0].launch.args[0].ends_with("EBOOT.BIN"));
        assert!(
            warnings.iter().any(|w| w.contains("NPUB00002")),
            "{warnings:?}"
        );
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("game-launcher-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
