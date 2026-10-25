//! Game music made with ffmpeg (when installed): a PS3 game's SND0.AT3 (ATRAC3, which the
//! launcher's decoder can't play) converted once to FLAC in the cache, and the soundtrack of a
//! game's store trailer saved as `music.ogg` in its art folder.

use std::ffi::OsStr;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

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
    std::fs::write(&input, bytes).map_err(|e| fail(format!("{e}")))?;
    // A menu tune converts in a second or two.
    let finished = ffmpeg(
        input.as_os_str(),
        &["-c:a", "flac", "-f", "flac"],
        out,
        Duration::from_secs(60),
    );
    let _ = std::fs::remove_file(&input);
    finished
}

/// Saves the first 90 seconds of a trailer's soundtrack (a stream or video address) as Ogg
/// Vorbis at `out`; only the sound is kept. Downloading takes most of the time.
pub fn trailer_audio(stream: &str, out: &Path) -> Result<(), Failure> {
    if !stream.starts_with("https://") {
        return Err(Failure::Other(format!("not a web address: {stream}")));
    }
    let dir = out
        .parent()
        .ok_or_else(|| Failure::Other("bad music path".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| Failure::Other(format!("{e}")))?;
    ffmpeg(
        OsStr::new(stream),
        &[
            "-vn",
            "-t",
            "90",
            "-c:a",
            "libvorbis",
            "-q:a",
            "3",
            "-f",
            "ogg",
        ],
        out,
        Duration::from_secs(180),
    )
}

/// Runs ffmpeg hidden on `input` into `out` (through a temporary file, so a half-made file
/// never looks finished), killing it after `timeout`.
fn ffmpeg(input: &OsStr, options: &[&str], out: &Path, timeout: Duration) -> Result<(), Failure> {
    let fail = |e: String| Failure::Other(e);
    let mut temporary = out.as_os_str().to_owned();
    temporary.push(".part");
    let temporary = PathBuf::from(temporary);
    // No console window flashes up over the launcher.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let started = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "quiet", "-y", "-i"])
        .arg(input)
        .args(options)
        .arg(&temporary)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    let status = match started {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Failure::NoFfmpeg),
        Err(e) => Err(fail(format!("ffmpeg could not start: {e}"))),
        Ok(child) => wait_or_kill(child, timeout),
    };
    let finished =
        status.and_then(|()| std::fs::rename(&temporary, out).map_err(|e| fail(format!("{e}"))));
    if finished.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    finished
}

fn wait_or_kill(mut child: Child, timeout: Duration) -> Result<(), Failure> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(Failure::Other(format!("ffmpeg failed ({status})"))),
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::Other("ffmpeg took too long".into()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(Failure::Other(format!("ffmpeg: {e}"))),
        }
    }
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

    /// Saves a real trailer's sound: `$env:TRAILER = "https://...m3u8"`, then
    /// `cargo test real_trailer_audio -- --ignored --nocapture`. Needs ffmpeg and internet.
    #[test]
    #[ignore]
    fn real_trailer_audio() {
        let out = std::env::temp_dir().join("game-launcher-test-trailer.ogg");
        let result = trailer_audio(&std::env::var("TRAILER").unwrap(), &out);
        println!(
            "{result:?}: {} bytes",
            out.metadata().map_or(0, |m| m.len())
        );
        assert_eq!(result, Ok(()));
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn only_web_addresses_are_streamed() {
        let out = std::env::temp_dir().join("game-launcher-test-never.ogg");
        assert!(trailer_audio(r"C:\Windows\notepad.exe", &out).is_err());
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
