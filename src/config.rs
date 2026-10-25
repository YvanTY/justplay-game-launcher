//! User settings in `%APPDATA%\GameLauncher\config.toml`, created with examples on first run.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DEFAULT_CONFIG: &str = r#"# JustPlay - Game Launcher settings. Paths use single quotes so backslashes stay as-is.
# Restart the launcher (or use Rescan library) after editing.

# --- PC games ---------------------------------------------------------------
# [[games]]
# name = "God of War"
# exe = 'E:\Games\God of War\GoW.exe'
# args = ""                      # optional command-line arguments
# icon = 'C:\Art\gow-icon.png'   # optional square/portrait art
# hero = 'C:\Art\gow-hero.jpg'   # optional full-screen background
# logo = 'C:\Art\gow-logo.png'   # optional transparent logo
# xinput = false                 # turn off the virtual Xbox pad for this game
#
# PC games get a virtual Xbox controller driven by your pad (like x360ce), so pads that
# aren't Xbox pads work in Xbox-only games. Needs the ViGEmBus driver.
#
# Art can also be dropped into %APPDATA%\GameLauncher\art\<game title>\ as
# icon.png|jpg, hero.png|jpg and logo.png — this works for every game.

# --- RPCS3 (PS3): games are imported from RPCS3's own game list -------------
# [rpcs3]
# exe = 'E:\Emulators\RPCS3\rpcs3.exe'
# hide = ["NPUO70219"]           # serials to leave out
# args = '--no-gui "{rom}"'      # default

# --- Other emulators: one block per system, ROMs found by file extension ----
# [[systems]]
# name = "PS2"
# emulator = 'E:\Emulators\PCSX2\pcsx2-qt.exe'
# args = '-batch -fullscreen "{rom}"'
# roms = 'E:\Roms\PS2'
# extensions = ["iso", "chd"]
# art = 'E:\Roms\PS2\art'        # optional: <rom name>.png, <rom name>-hero.jpg, <rom name>-logo.png
# xinput = true                  # only if this emulator needs an Xbox pad (most read your pad directly)
#
# [[systems]]
# name = "Switch"
# emulator = 'E:\Emulators\Ryujinx\Ryujinx.exe'
# args = '--fullscreen "{rom}"'
# roms = 'E:\Roms\Switch'
# extensions = ["nsp", "xci"]

# --- Game art ----------------------------------------------------------------
# Art comes from Steam, the Microsoft Store and PCGamingWiki without any account.
# For many more covers, backgrounds and logos (what Playnite uses), add a free
# SteamGridDB key: sign in at steamgriddb.com, then Preferences > API.
# [art]
# steamgriddb_key = "your key here"
"#;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub games: Vec<PcGame>,
    pub rpcs3: Option<Rpcs3Config>,
    pub systems: Vec<SystemConfig>,
    pub art: ArtConfig,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArtConfig {
    /// Personal SteamGridDB API key; without it that source is skipped.
    pub steamgriddb_key: Option<String>,
}

/// Also the shape of games added from the launcher, which state.json stores.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PcGame {
    pub name: String,
    pub exe: PathBuf,
    #[serde(default)]
    pub args: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hero: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<PathBuf>,
    #[serde(default = "enabled")]
    pub xinput: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rpcs3Config {
    pub exe: PathBuf,
    #[serde(default)]
    pub hide: Vec<String>,
    #[serde(default = "default_rpcs3_args")]
    pub args: String,
}

/// Also the shape of emulators set up from the launcher, which state.json stores.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    pub name: String,
    pub emulator: PathBuf,
    #[serde(default = "default_rom_args")]
    pub args: String,
    pub roms: PathBuf,
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub art: Option<PathBuf>,
    #[serde(default)]
    pub xinput: bool,
}

fn enabled() -> bool {
    true
}

fn default_rpcs3_args() -> String {
    r#"--no-gui "{rom}""#.into()
}

fn default_rom_args() -> String {
    r#""{rom}""#.into()
}

/// `%APPDATA%\GameLauncher` — config, state and art live here so rebuilds never touch them.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("GameLauncher")
}

pub fn load(dir: &Path) -> Result<Config, String> {
    let path = dir.join("config.toml");
    if !path.exists() {
        fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        fs::write(&path, DEFAULT_CONFIG)
            .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    }
    let text =
        fs::read_to_string(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    parse(crate::state::strip_bom(&text)).map_err(|e| format!("config.toml: {e}"))
}

pub fn parse(text: &str) -> Result<Config, String> {
    toml::from_str(text).map_err(|e| e.message().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid_and_empty() {
        let config = parse(DEFAULT_CONFIG).unwrap();
        assert!(config.games.is_empty() && config.rpcs3.is_none() && config.systems.is_empty());
    }

    #[test]
    fn parses_all_sections_with_defaults() {
        let config = parse(
            r#"
            [[games]]
            name = "God of War"
            exe = 'E:\Games\God of War\GoW.exe'

            [rpcs3]
            exe = 'E:\RPCS3\rpcs3.exe'
            hide = ["NPUO70219"]

            [[systems]]
            name = "PS2"
            emulator = 'E:\PCSX2\pcsx2-qt.exe'
            roms = 'E:\Roms\PS2'
            extensions = ["iso"]
            "#,
        )
        .unwrap();
        assert_eq!(
            config.games[0].exe,
            PathBuf::from(r"E:\Games\God of War\GoW.exe")
        );
        assert_eq!(config.games[0].args, "");
        assert!(
            config.games[0].xinput,
            "PC games get the virtual Xbox pad by default"
        );
        assert!(
            !config.systems[0].xinput,
            "emulators read pads directly by default"
        );
        let rpcs3 = config.rpcs3.unwrap();
        assert_eq!(rpcs3.hide, ["NPUO70219"]);
        assert_eq!(rpcs3.args, r#"--no-gui "{rom}""#);
        assert_eq!(config.systems[0].args, r#""{rom}""#);
    }

    #[test]
    fn typo_in_key_is_reported() {
        let err = parse("[[games]]\nname = \"X\"\nexee = 'C:\\x.exe'\n").unwrap_err();
        assert!(err.contains("exee"), "{err}");
    }
}
