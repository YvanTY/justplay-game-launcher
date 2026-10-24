//! Folder-scanned systems (PCSX2, Switch emulators, …): every ROM file with a listed extension.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Art, Game, Launch, expand_args, find_image};
use crate::config::SystemConfig;

/// ROM sets are usually flat or one folder per game; deeper trees are almost always junk.
const MAX_DEPTH: usize = 2;

pub fn scan(system: &SystemConfig, warnings: &mut Vec<String>) -> Vec<Game> {
    if !system.emulator.is_file() {
        warnings.push(format!(
            "{}: emulator not found at {}",
            system.name,
            system.emulator.display()
        ));
        return Vec::new();
    }
    if !system.roms.is_dir() {
        warnings.push(format!(
            "{}: ROM folder not found at {}",
            system.name,
            system.roms.display()
        ));
        return Vec::new();
    }
    let extensions: Vec<String> = system
        .extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_lowercase())
        .collect();
    let mut roms = Vec::new();
    collect_roms(&system.roms, &extensions, 0, &mut roms);

    let mut games = Vec::new();
    for rom in roms {
        let stem = rom
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let args = match expand_args(&system.args, &rom) {
            Ok(args) => args,
            Err(e) => {
                warnings.push(format!("{}: {e}", system.name));
                return games;
            }
        };
        let art = system.art.as_deref().map_or_else(Art::default, |dir| Art {
            icon: find_image(dir, &stem),
            hero: find_image(dir, &format!("{stem}-hero")),
            logo: find_image(dir, &format!("{stem}-logo")),
        });
        // The path inside the ROM folder keeps same-named ROMs in different subfolders apart.
        let relative = rom.strip_prefix(&system.roms).unwrap_or(&rom);
        let relative = relative.to_string_lossy().replace('\\', "/");
        games.push(Game {
            id: format!("{}:{relative}", system.name),
            title: clean_rom_title(&stem),
            platform: system.name.clone(),
            launch: Launch {
                exe: system.emulator.clone(),
                args,
            },
            art,
            xinput: system.xinput,
        });
    }
    games
}

fn collect_roms(dir: &Path, extensions: &[String], depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.is_dir() {
            if depth < MAX_DEPTH {
                collect_roms(&path, extensions, depth + 1, out);
            }
        } else if path
            .extension()
            .is_some_and(|ext| extensions.contains(&ext.to_string_lossy().to_lowercase()))
        {
            out.push(path);
        }
    }
}

/// "God of War II (USA) (En,Fr,De) [SLUS-21665]" -> "God of War II".
fn clean_rom_title(stem: &str) -> String {
    let mut title = String::with_capacity(stem.len());
    let mut depth = 0usize;
    for c in stem.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            c if depth == 0 => title.push(c),
            _ => {}
        }
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        stem.to_string()
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_region_and_serial_tags() {
        assert_eq!(
            clean_rom_title("God of War II (USA) (En,Fr,De) [SLUS-21665]"),
            "God of War II"
        );
        assert_eq!(
            clean_rom_title("Zelda - Tears of the Kingdom [0100F2C0115B6000][v0]"),
            "Zelda - Tears of the Kingdom"
        );
        assert_eq!(clean_rom_title("(Prototype)"), "(Prototype)");
    }

    #[test]
    fn finds_roms_by_extension_with_art() {
        let root =
            std::env::temp_dir().join(format!("game-launcher-test-emu-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let roms = root.join("roms");
        fs::create_dir_all(roms.join("Shadow of the Colossus")).unwrap();
        fs::create_dir_all(root.join("art")).unwrap();
        let emulator = root.join("pcsx2-qt.exe");
        fs::write(&emulator, b"").unwrap();
        fs::write(roms.join("God of War II (USA).ISO"), b"").unwrap();
        fs::write(
            roms.join("Shadow of the Colossus").join("SotC (USA).chd"),
            b"",
        )
        .unwrap();
        fs::write(roms.join("readme.txt"), b"").unwrap();
        fs::write(root.join("art").join("God of War II (USA).png"), b"").unwrap();

        let system = SystemConfig {
            name: "PS2".into(),
            emulator: emulator.clone(),
            args: r#"-batch "{rom}""#.into(),
            roms: roms.clone(),
            extensions: vec!["iso".into(), ".chd".into()],
            art: Some(root.join("art")),
            xinput: false,
        };
        let mut games = scan(&system, &mut Vec::new());
        games.sort_by(|a, b| a.title.cmp(&b.title));

        assert_eq!(games.len(), 2);
        assert_eq!(games[0].title, "God of War II");
        assert_eq!(games[0].id, "PS2:God of War II (USA).ISO");
        assert_eq!(games[1].id, "PS2:Shadow of the Colossus/SotC (USA).chd");
        assert_eq!(
            games[0].launch.args[1],
            roms.join("God of War II (USA).ISO").display().to_string()
        );
        assert!(games[0].art.icon.is_some());
        assert_eq!(games[1].title, "SotC");
    }

    #[test]
    fn missing_emulator_is_reported() {
        let system = SystemConfig {
            name: "Switch".into(),
            emulator: PathBuf::from(r"Z:\nope\Ryujinx.exe"),
            args: r#""{rom}""#.into(),
            roms: PathBuf::from(r"Z:\nope"),
            extensions: vec!["nsp".into()],
            art: None,
            xinput: false,
        };
        let mut warnings = Vec::new();
        assert!(scan(&system, &mut warnings).is_empty());
        assert!(warnings[0].starts_with("Switch: emulator not found"));
    }
}
