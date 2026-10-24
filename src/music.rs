//! Per-game music. A background thread decodes the selected game's music file (MP3, OGG, FLAC or
//! WAV) a fraction of a second ahead into its own SDL audio stream; SDL mixes it with the UI sounds
//! and resamples it for the device. Moving to another game fades the old track out and, after a
//! short pause (as on PS5), fades the new one in.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use sdl3_sys::everything::*;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::diag;

/// Background music sits well under the UI sounds.
const VOLUME: f32 = 0.45;
/// Resting on a game this long before its music starts, so scrolling past games stays quiet.
const START_DELAY: Duration = Duration::from_millis(700);
const FADE_IN: Duration = Duration::from_millis(1200);
const FADE_OUT: Duration = Duration::from_millis(600);
const TICK: Duration = Duration::from_millis(30);
/// Decoded audio kept queued ahead of playback.
const BUFFER_AHEAD: Duration = Duration::from_millis(500);

pub struct Music {
    commands: Sender<Option<PathBuf>>,
    /// True while any track is audible (or fading), so the device isn't paused under it.
    playing: Arc<AtomicBool>,
}

impl Music {
    pub fn start(device: SDL_AudioDeviceID) -> Music {
        let (commands, receiver) = mpsc::channel();
        let playing = Arc::new(AtomicBool::new(false));
        let flag = playing.clone();
        // A thread that fails to start just means no music; the launcher works without it.
        let spawned = std::thread::Builder::new()
            .name("music".into())
            .spawn(move || run(device, receiver, &flag));
        if let Err(e) = spawned {
            diag::log(&format!("music thread could not start: {e}"));
        }
        Music { commands, playing }
    }

    /// Switches to `path` (fading), or fades out to silence with `None`.
    pub fn play(&self, path: Option<PathBuf>) {
        let _ = self.commands.send(path);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
}

struct Track {
    path: PathBuf,
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    /// Created on the first decoded packet, once the real sample rate and channels are known.
    stream: *mut SDL_AudioStream,
    bytes_per_second: usize,
    gain: f32,
    samples: Vec<f32>,
}

impl Drop for Track {
    fn drop(&mut self) {
        if !self.stream.is_null() {
            unsafe { SDL_DestroyAudioStream(self.stream) };
        }
    }
}

fn run(device: SDL_AudioDeviceID, commands: Receiver<Option<PathBuf>>, playing: &AtomicBool) {
    let mut current: Option<Track> = None;
    let mut fading: Vec<Track> = Vec::new();
    let mut pending: Option<(PathBuf, Instant)> = None;
    loop {
        let busy = current.is_some() || !fading.is_empty() || pending.is_some();
        let command = if busy {
            commands.recv_timeout(TICK)
        } else {
            commands.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        match command {
            Ok(path) => {
                if path.is_some() && current.as_ref().map(|t| &t.path) == path.as_ref() {
                    pending = None;
                } else {
                    fading.extend(current.take());
                    pending = path.map(|p| (p, Instant::now() + START_DELAY));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if let Some((path, at)) = &pending
            && Instant::now() >= *at
        {
            match open(path) {
                Ok(track) => {
                    unsafe { SDL_ResumeAudioDevice(device) };
                    current = Some(track);
                }
                Err(e) => diag::log(&format!("music {}: {e}", path.display())),
            }
            pending = None;
        }

        let fade_in_step = TICK.as_secs_f32() / FADE_IN.as_secs_f32();
        let fade_out_step = TICK.as_secs_f32() / FADE_OUT.as_secs_f32();
        if let Some(track) = current.as_mut() {
            track.gain = (track.gain + fade_in_step).min(1.0);
            if let Err(e) = feed(track, device) {
                diag::log(&format!("music {}: {e}", track.path.display()));
                current = None;
            }
        }
        for track in &mut fading {
            track.gain = (track.gain - fade_out_step).max(0.0);
            let _ = feed(track, device);
        }
        fading.retain(|track| track.gain > 0.0);
        playing.store(current.is_some() || !fading.is_empty(), Ordering::Relaxed);
    }
}

fn open(path: &Path) -> Result<Track, String> {
    let (reader, decoder, track_id) = open_decoder(path)?;
    Ok(Track {
        path: path.to_path_buf(),
        reader,
        decoder,
        track_id,
        stream: std::ptr::null_mut(),
        bytes_per_second: 0,
        gain: 0.0,
        samples: Vec::new(),
    })
}

type Decoding = (Box<dyn FormatReader>, Box<dyn AudioDecoder>, u32);

fn open_decoder(path: &Path) -> Result<Decoding, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let reader = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported file: {e}"))?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio parameters")?;
    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("unsupported codec: {e}"))?;
    let track_id = track.id;
    Ok((reader, decoder, track_id))
}

/// Keeps about BUFFER_AHEAD of audio queued; loops the file at its end.
fn feed(track: &mut Track, device: SDL_AudioDeviceID) -> Result<(), String> {
    if !track.stream.is_null() {
        unsafe { SDL_SetAudioStreamGain(track.stream, track.gain * VOLUME) };
    }
    loop {
        if !track.stream.is_null() {
            let queued = unsafe { SDL_GetAudioStreamQueued(track.stream) }.max(0) as usize;
            if queued as f32 >= track.bytes_per_second as f32 * BUFFER_AHEAD.as_secs_f32() {
                return Ok(());
            }
        }
        let packet = match track.reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => {
                // End of the track: start it again from the top.
                (track.reader, track.decoder, track.track_id) = open_decoder(&track.path)?;
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != track.track_id {
            continue;
        }
        let decoded = match track.decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A damaged packet is skipped; the rest of the track still plays.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        if track.stream.is_null() {
            let spec = decoded.spec();
            let channels = spec.channels().count();
            let source = SDL_AudioSpec {
                format: SDL_AUDIO_F32,
                channels: channels as i32,
                freq: spec.rate() as i32,
            };
            let stream = unsafe { SDL_CreateAudioStream(&source, &source) };
            if stream.is_null() || !unsafe { SDL_BindAudioStream(device, stream) } {
                let reason = unsafe { std::ffi::CStr::from_ptr(SDL_GetError()) }
                    .to_string_lossy()
                    .into_owned();
                if !stream.is_null() {
                    unsafe { SDL_DestroyAudioStream(stream) };
                }
                return Err(format!(
                    "could not open an audio stream for music: {reason}"
                ));
            }
            unsafe { SDL_SetAudioStreamGain(stream, track.gain * VOLUME) };
            track.stream = stream;
            track.bytes_per_second = spec.rate() as usize * channels * size_of::<f32>();
        }
        decoded.copy_to_vec_interleaved(&mut track.samples);
        let bytes = (track.samples.len() * size_of::<f32>()) as i32;
        unsafe { SDL_PutAudioStreamData(track.stream, track.samples.as_ptr().cast(), bytes) };
    }
}

/// A game's music file in its art folder: music.mp3, .ogg, .flac or .wav.
pub fn find_music(dir: &Path) -> Option<PathBuf> {
    ["mp3", "ogg", "flac", "wav"]
        .iter()
        .map(|ext| dir.join(format!("music.{ext}")))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-second 440 Hz WAV, written by hand so the test needs no sample files.
    fn write_test_wav(path: &Path) {
        let rate = 22_050u32;
        let samples: Vec<i16> = (0..rate)
            .map(|i| {
                ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 8000.0) as i16
            })
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + data_len).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes()); // PCM
        bytes.extend(1u16.to_le_bytes()); // mono
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 2).to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data_len.to_le_bytes());
        for s in samples {
            bytes.extend(s.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn opens_and_decodes_a_wav_file() {
        let dir =
            std::env::temp_dir().join(format!("game-launcher-test-music-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("music.wav");
        write_test_wav(&path);

        assert_eq!(find_music(&dir), Some(path.clone()));
        let mut track = open(&path).unwrap();
        let packet = track.reader.next_packet().unwrap().unwrap();
        let decoded = track.decoder.decode(&packet).unwrap();
        assert_eq!(decoded.spec().rate(), 22_050);
        assert_eq!(decoded.spec().channels().count(), 1);
    }

    #[test]
    fn missing_or_unsupported_files_are_errors_not_panics() {
        assert!(open(Path::new(r"Z:\nope\music.mp3")).is_err());
        let path = std::env::temp_dir().join(format!(
            "game-launcher-test-junk-{}.mp3",
            std::process::id()
        ));
        std::fs::write(&path, b"definitely not audio").unwrap();
        assert!(open(&path).is_err());
    }
}
