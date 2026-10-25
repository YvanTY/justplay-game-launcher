//! Emulators JustPlay can set up by itself: the folder an emulator lives in is enough to know
//! which system it plays, how to start a game full screen, and which game files to look for.

use std::path::{Path, PathBuf};

use crate::config::SystemConfig;

pub struct Preset {
    /// The tab its games go in.
    pub system: &'static str,
    pub name: &'static str,
    /// Program names (lowercase, without .exe) this emulator ships as; a name only has to
    /// start with one ("pcsx2-qt", "pcsx2-avx2").
    pub exes: &'static [&'static str],
    /// `{rom}` is the game file.
    pub args: &'static str,
    pub extensions: &'static [&'static str],
}

pub const PRESETS: [Preset; 7] = [
    Preset {
        system: "PS2",
        name: "PCSX2",
        exes: &["pcsx2"],
        args: r#"-batch -fullscreen "{rom}""#,
        extensions: &["iso", "chd", "cso", "gz"],
    },
    Preset {
        system: "Switch",
        name: "Ryujinx",
        exes: &["ryujinx"],
        args: r#"--fullscreen "{rom}""#,
        extensions: &["nsp", "xci"],
    },
    Preset {
        system: "Switch",
        name: "yuzu",
        exes: &["yuzu", "suyu", "sudachi", "citron", "eden"],
        args: r#"-f -g "{rom}""#,
        extensions: &["nsp", "xci"],
    },
    Preset {
        system: "GameCube / Wii",
        name: "Dolphin",
        exes: &["dolphin"],
        args: r#"-b -e "{rom}""#,
        extensions: &["iso", "gcm", "rvz", "wbfs", "ciso", "gcz"],
    },
    Preset {
        system: "PS1",
        name: "DuckStation",
        exes: &["duckstation"],
        args: r#"-batch -fullscreen "{rom}""#,
        extensions: &["cue", "chd", "pbp", "m3u"],
    },
    Preset {
        system: "PSP",
        name: "PPSSPP",
        exes: &["ppssppwindows64", "ppssppwindows", "ppsspp"],
        args: r#"--fullscreen "{rom}""#,
        extensions: &["iso", "cso", "pbp"],
    },
    Preset {
        system: "Wii U",
        name: "Cemu",
        exes: &["cemu"],
        args: r#"-f -g "{rom}""#,
        extensions: &["wud", "wux", "wua", "rpx"],
    },
];

/// The preset for a program file, by its name.
pub fn preset_for(exe: &Path) -> Option<&'static Preset> {
    let is_exe = exe
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    let stem = exe.file_stem()?.to_string_lossy().to_lowercase();
    PRESETS
        .iter()
        .find(|p| is_exe && p.exes.iter().any(|name| stem.starts_with(name)))
}

/// The emulator in `folder` (its own folder, or a "bin" folder inside), with its preset.
/// Helper programs ("pcsx2-updater", "DolphinTool", "PPSSPPHeadless") are not the emulator;
/// a program named exactly like the emulator wins over its variants.
pub fn find_in(folder: &Path) -> Option<(PathBuf, &'static Preset)> {
    let stem = |path: &Path| {
        path.file_stem()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let mut found: Vec<(PathBuf, &'static Preset)> = [folder.to_path_buf(), folder.join("bin")]
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|path| path.is_file())
        .filter(|path| {
            let name = stem(path);
            ![
                "updater", "update", "cli", "cmd", "uninst", "crash", "tool", "headless",
            ]
            .iter()
            .any(|helper| name.contains(helper))
        })
        .filter_map(|path| preset_for(&path).map(|preset| (path, preset)))
        .collect();
    // Exact names first, then the shortest (the plain build before "-avx2" or "-dbg").
    found.sort_by_key(|(path, preset)| {
        let name = stem(path);
        (!preset.exes.contains(&name.as_str()), name.len(), name)
    });
    found.into_iter().next()
}

/// What the system's config looks like once its games folder is known.
pub fn system(preset: &Preset, exe: PathBuf, roms: PathBuf) -> SystemConfig {
    SystemConfig {
        name: preset.system.to_string(),
        emulator: exe,
        args: preset.args.to_string(),
        roms,
        extensions: preset.extensions.iter().map(|e| e.to_string()).collect(),
        art: None,
        xinput: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emulators_are_known_by_their_program_names() {
        let system = |exe: &str| preset_for(Path::new(exe)).map(|p| p.system);
        assert_eq!(system(r"E:\Emu\PCSX2\pcsx2-qt.exe"), Some("PS2"));
        assert_eq!(system(r"E:\Emu\Ryujinx\Ryujinx.exe"), Some("Switch"));
        assert_eq!(system(r"E:\Emu\suyu\suyu.exe"), Some("Switch"));
        assert_eq!(
            system(r"E:\Emu\Dolphin-x64\Dolphin.exe"),
            Some("GameCube / Wii")
        );
        assert_eq!(
            system(r"E:\Emu\duckstation-qt-x64-ReleaseLTCG.exe"),
            Some("PS1")
        );
        assert_eq!(system(r"E:\Emu\PPSSPPWindows64.exe"), Some("PSP"));
        assert_eq!(system(r"E:\Emu\Cemu\Cemu.exe"), Some("Wii U"));
        assert_eq!(system(r"E:\Emu\pcsx2.txt"), None, "only programs");
        assert_eq!(system(r"E:\Games\Hades\Hades.exe"), None);
    }

    #[test]
    fn finds_the_emulator_in_its_folder_and_skips_helpers() {
        let dir = std::env::temp_dir().join(format!(
            "game-launcher-test-emu-find-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("pcsx2-updater.exe"), b"").unwrap();
        std::fs::write(dir.join("readme.txt"), b"").unwrap();
        assert!(find_in(&dir).is_none(), "an updater isn't the emulator");
        std::fs::write(dir.join("bin").join("pcsx2-qt.exe"), b"").unwrap();
        std::fs::write(dir.join("bin").join("pcsx2-qt-avx2.exe"), b"").unwrap();
        let (exe, preset) = find_in(&dir).unwrap();
        assert_eq!(exe, dir.join("bin").join("pcsx2-qt.exe"), "the plain build");
        assert_eq!(preset.name, "PCSX2");
        let config = system(preset, exe, PathBuf::from(r"E:\Roms\PS2"));
        assert_eq!(config.name, "PS2");
        assert!(config.args.contains("{rom}"));
        assert!(config.extensions.contains(&"iso".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
