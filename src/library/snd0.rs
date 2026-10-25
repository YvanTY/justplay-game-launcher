//! PS3 menu music: SND0.AT3 is ATRAC3, which the launcher's decoder can't play, so it is
//! converted once to FLAC with ffmpeg (when installed) and kept in the cache.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Snd0, iso, safe_file_name};

/// Where a game's converted menu music is kept.
pub fn cached(data_dir: &Path, game_id: &str) -> PathBuf {
    data_dir
        .join("cache")
        .join("music")
        .join(format!("{}.flac", safe_file_name(game_id)))
}

/// Why a conversion didn't happen.
#[derive(Debug, PartialEq)]
pub enum Failure {
    /// ffmpeg isn't installed: no game can be converted, so stop trying.
    NoFfmpeg,
    Other(String),
}

/// Converts `source` to FLAC at `out`. The temporary files are removed whatever happens.
pub fn convert(source: &Snd0, out: &Path) -> Result<(), Failure> {
    let fail = |e: String| Failure::Other(e);
    let bytes = match source {
        Snd0::File(path) => std::fs::read(path).map_err(|e| fail(format!("{e}")))?,
        Snd0::Iso(image) => iso::read_file(image, &["PS3_GAME", "SND0.AT3"])
            .map_err(|e| fail(format!("{e}")))?
            .ok_or_else(|| fail("the disc image has no menu music".into()))?,
    };
    let dir = out.parent().ok_or_else(|| fail("bad cache path".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| fail(format!("{e}")))?;
    let input = out.with_extension("at3.part");
    let output = out.with_extension("flac.part");
    std::fs::write(&input, bytes).map_err(|e| fail(format!("{e}")))?;
    // No console window flashes up over the launcher.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let result = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&input)
        .args(["-c:a", "flac", "-f", "flac"])
        .arg(&output)
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = std::fs::remove_file(&input);
    let finished = match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Failure::NoFfmpeg),
        Err(e) => Err(fail(format!("ffmpeg could not start: {e}"))),
        Ok(run) if !run.status.success() => Err(fail(format!(
            "ffmpeg: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        ))),
        Ok(_) => std::fs::rename(&output, out).map_err(|e| fail(format!("{e}"))),
    };
    if finished.is_err() {
        let _ = std::fs::remove_file(&output);
    }
    finished
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converted_music_is_named_after_the_game() {
        let path = cached(Path::new(r"C:\data"), "ps3:BLUS30405");
        assert_eq!(path, Path::new(r"C:\data\cache\music\ps3BLUS30405.flac"));
    }

    #[test]
    fn a_missing_source_fails_and_leaves_nothing_behind() {
        let dir =
            std::env::temp_dir().join(format!("game-launcher-test-snd0-{}", std::process::id()));
        let out = dir.join("game.flac");
        let result = convert(&Snd0::File(dir.join("missing.at3")), &out);
        assert!(matches!(result, Err(Failure::Other(_))));
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Converts a real game's menu music: `$env:SND0 = "...\SND0.AT3"` (or an .iso), then
    /// `cargo test convert_real_snd0 -- --ignored --nocapture`. Needs ffmpeg.
    #[test]
    #[ignore]
    fn convert_real_snd0() {
        let path = PathBuf::from(std::env::var("SND0").unwrap());
        let source = if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("iso"))
        {
            Snd0::Iso(path)
        } else {
            Snd0::File(path)
        };
        let out = std::env::temp_dir().join("game-launcher-test-snd0.flac");
        let result = convert(&source, &out);
        println!(
            "{result:?}: {} bytes",
            out.metadata().map_or(0, |m| m.len())
        );
        assert_eq!(result, Ok(()));
        let _ = std::fs::remove_file(&out);
    }
}
