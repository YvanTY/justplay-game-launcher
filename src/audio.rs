//! UI sounds through SDL3's audio stream and a tiny mixer. Each sound is a few milliseconds of
//! band-passed noise, which reads as a soft physical tap (like the PS3/PS4 menus) rather than a
//! musical beep. Sounds are synthesized at startup, so nothing copyrighted ships, and the audio
//! device pauses itself whenever nothing is playing.

use std::f32::consts::{PI, TAU};
use std::ffi::{CStr, c_int, c_void};
use std::sync::{Arc, Mutex};

use sdl3_sys::everything::*;

const RATE: usize = 48_000;
const CHANNELS: usize = 2;
const VOLUME: f32 = 0.55;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    Move,
    Bump,
    Tab,
    Confirm,
    Back,
    Launch,
}

const ALL_SOUNDS: [Sound; 6] = [
    Sound::Move,
    Sound::Bump,
    Sound::Tab,
    Sound::Confirm,
    Sound::Back,
    Sound::Launch,
];

pub struct Audio {
    /// A regular device rather than SDL's "simplified" one-stream device, whose bindings can't
    /// change: the music stream binds to it as well.
    device: SDL_AudioDeviceID,
    stream: *mut SDL_AudioStream,
    mixer: Arc<Mutex<Mixer>>,
    sounds: Vec<Arc<[f32]>>,
}

impl Audio {
    pub fn start() -> Result<Audio, String> {
        unsafe {
            if !SDL_InitSubSystem(SDL_INIT_AUDIO) {
                return Err(format!("audio could not start: {}", sdl_error()));
            }
        }
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        let spec = SDL_AudioSpec {
            format: SDL_AUDIO_F32,
            channels: CHANNELS as c_int,
            freq: RATE as c_int,
        };
        let device = unsafe { SDL_OpenAudioDevice(SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, &spec) };
        if device.0 == 0 {
            return Err(format!("no audio output: {}", sdl_error()));
        }
        let stream = unsafe { SDL_CreateAudioStream(&spec, &spec) };
        if stream.is_null() {
            unsafe { SDL_CloseAudioDevice(device) };
            return Err(format!("audio stream failed: {}", sdl_error()));
        }
        // The stream holds one reference to the mixer; Drop gives it back after destroying the stream.
        let userdata = Arc::into_raw(mixer.clone()) as *mut c_void;
        // Built before checking so that Drop cleans up on failure too.
        let audio = Audio {
            device,
            stream,
            mixer,
            sounds: ALL_SOUNDS.iter().map(|&s| synthesize(s).into()).collect(),
        };
        let ready = unsafe {
            SDL_SetAudioStreamGetCallback(stream, Some(feed), userdata)
                && SDL_BindAudioStream(device, stream)
        };
        if !ready {
            return Err(format!("audio stream failed: {}", sdl_error()));
        }
        // Stays paused until the first sound.
        unsafe { SDL_PauseAudioDevice(device) };
        Ok(audio)
    }

    pub fn play(&self, sound: Sound) {
        let index = ALL_SOUNDS
            .iter()
            .position(|&s| s == sound)
            .expect("every sound is synthesized");
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.voices.push(Voice {
                samples: self.sounds[index].clone(),
                position: 0,
            });
        }
        unsafe { SDL_ResumeAudioDevice(self.device) };
    }

    /// The device the UI sounds play on; music streams bind to it too.
    pub fn device(&self) -> SDL_AudioDeviceID {
        self.device
    }

    /// Pauses the device once every sound has finished and no music plays, so idle costs no CPU.
    pub fn pause_if_idle(&self, music_playing: bool) {
        let idle = !music_playing && self.mixer.lock().is_ok_and(|mixer| mixer.voices.is_empty());
        if idle {
            unsafe { SDL_PauseAudioDevice(self.device) };
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        unsafe {
            SDL_DestroyAudioStream(self.stream);
            SDL_CloseAudioDevice(self.device);
            drop(Arc::from_raw(Arc::as_ptr(&self.mixer)));
        }
    }
}

/// SDL calls this on its audio thread whenever the device needs more samples.
unsafe extern "C" fn feed(
    userdata: *mut c_void,
    stream: *mut SDL_AudioStream,
    additional: c_int,
    _total: c_int,
) {
    let mixer = unsafe { &*(userdata as *const Mutex<Mixer>) };
    let frames = additional.max(0) as usize / (size_of::<f32>() * CHANNELS);
    if frames == 0 {
        return;
    }
    let Ok(mut mixer) = mixer.lock() else { return };
    mixer.render(frames);
    let bytes = (mixer.scratch.len() * size_of::<f32>()) as c_int;
    unsafe { SDL_PutAudioStreamData(stream, mixer.scratch.as_ptr().cast(), bytes) };
}

struct Voice {
    samples: Arc<[f32]>,
    /// Index into the interleaved stereo samples.
    position: usize,
}

#[derive(Default)]
struct Mixer {
    voices: Vec<Voice>,
    scratch: Vec<f32>,
}

impl Mixer {
    fn render(&mut self, frames: usize) {
        self.scratch.clear();
        self.scratch.resize(frames * CHANNELS, 0.0);
        for voice in &mut self.voices {
            let remaining = &voice.samples[voice.position..];
            let count = remaining.len().min(self.scratch.len());
            for (out, sample) in self.scratch.iter_mut().zip(&remaining[..count]) {
                *out += sample * VOLUME;
            }
            voice.position += count;
        }
        self.voices
            .retain(|voice| voice.position < voice.samples.len());
        for sample in &mut self.scratch {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
}

/// One UI sound as interleaved stereo samples.
fn synthesize(sound: Sound) -> Vec<f32> {
    let mono = match sound {
        // A tiny, bright, quiet tap: the sound heard most, so it is the most discreet.
        Sound::Move => tap(2600.0, 0.0025, 0.03, 0.10, 1),
        // A soft page turn: tabs feel like leafing through a book.
        Sound::Tab => page_turn(),
        // Muted and low: "nothing further this way".
        Sound::Bump => tap(380.0, 0.006, 0.05, 0.10, 3),
        // A soft "tock"; back is the same gesture, a little lower.
        Sound::Confirm => tap(1300.0, 0.006, 0.06, 0.15, 4),
        Sound::Back => tap(900.0, 0.005, 0.05, 0.13, 5),
        Sound::Launch => whoosh(),
    };
    mono.iter().flat_map(|&s| [s, s]).collect()
}

/// White noise through a band-pass filter with a 0.5 ms attack and exponential decay
/// (`decay` = time constant in seconds), normalised to `peak`.
fn tap(center_hz: f32, decay: f32, seconds: f32, peak: f32, seed: u32) -> Vec<f32> {
    let count = (seconds * RATE as f32) as usize;
    let mut noise = Noise(seed.wrapping_mul(0x9E37_79B9) | 1);
    let mut filter = BandPass::new(center_hz, 1.4);
    let mut out: Vec<f32> = (0..count)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let envelope = (t / 0.0005).min(1.0) * (-t / decay).exp();
            filter.process(noise.next()) * envelope
        })
        .collect();
    normalise(&mut out, peak);
    out
}

/// Paper sliding over paper: noise whose band sweeps upward as the page lifts, a gentle
/// rise-and-fall over 0.2 s, then a faint low tap as the page settles.
fn page_turn() -> Vec<f32> {
    let seconds = 0.22;
    let count = (seconds * RATE as f32) as usize;
    let mut noise = Noise(0x6A09_E667);
    let mut out: Vec<f32> = Vec::with_capacity(count);
    let mut filter = BandPass::new(1800.0, 0.9);
    for i in 0..count {
        let t = i as f32 / RATE as f32;
        let progress = (t / 0.16).min(1.0);
        // Re-tuning the filter every 64 samples is smooth enough for a sweep this slow.
        if i % 64 == 0 {
            let next = BandPass::new(1800.0 + 2700.0 * progress, 0.9);
            filter = BandPass {
                x1: filter.x1,
                x2: filter.x2,
                y1: filter.y1,
                y2: filter.y2,
                ..next
            };
        }
        // Half-sine swell: no hard edge at either end, which keeps it calm.
        let swell = if t < 0.16 { (PI * t / 0.16).sin() } else { 0.0 };
        out.push(filter.process(noise.next()) * swell);
    }
    normalise(&mut out, 0.10);
    let settle = tap(700.0, 0.008, 0.05, 0.035, 9);
    let start = (0.15 * RATE as f32) as usize;
    for (i, sample) in settle.iter().enumerate() {
        if let Some(slot) = out.get_mut(start + i) {
            *slot += sample;
        }
    }
    out
}

/// Air rushing past: noise through a low-pass that opens and closes, no tone at all.
fn whoosh() -> Vec<f32> {
    let seconds = 0.8;
    let count = (seconds * RATE as f32) as usize;
    let mut noise = Noise(0x2545_F491);
    let mut smooth = 0.0f32;
    let mut out: Vec<f32> = (0..count)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let swell = if t < 0.35 {
                (t / 0.35).powf(2.0)
            } else {
                (-(t - 0.35) / 0.15).exp()
            };
            // The filter opens with the swell, so the sound brightens as it grows.
            smooth += (noise.next() - smooth) * (0.02 + 0.12 * swell);
            smooth * swell
        })
        .collect();
    normalise(&mut out, 0.22);
    out
}

fn normalise(samples: &mut [f32], peak: f32) {
    let max = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if max > 0.0 {
        let scale = peak / max;
        samples.iter_mut().for_each(|s| *s *= scale);
    }
}

/// Deterministic white noise in -1..1 (xorshift), so every launch sounds identical.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }
}

/// Biquad band-pass (constant 0 dB peak gain), from the RBJ audio EQ cookbook.
struct BandPass {
    b0: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BandPass {
    fn new(center_hz: f32, q: f32) -> BandPass {
        let w = TAU * center_hz / RATE as f32;
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        BandPass {
            b0: alpha / a0,
            b2: -alpha / a0,
            a1: -2.0 * w.cos() / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

fn sdl_error() -> String {
    unsafe { CStr::from_ptr(SDL_GetError()) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_is_short_quiet_stereo() {
        for sound in ALL_SOUNDS {
            let samples = synthesize(sound);
            assert_eq!(samples.len() % 2, 0, "{sound:?} must be stereo");
            let seconds = samples.len() as f32 / (RATE * CHANNELS) as f32;
            let limit = match sound {
                Sound::Launch => 0.8,
                Sound::Tab => 0.25,
                _ => 0.06,
            };
            assert!(
                seconds > 0.0 && seconds <= limit,
                "{sound:?} lasts {seconds}s"
            );
            let peak = samples.iter().fold(0.0f32, |p, s| p.max(s.abs()));
            assert!((0.05..=0.25).contains(&peak), "{sound:?} peak {peak}");
        }
    }

    #[test]
    fn taps_fade_within_milliseconds() {
        // No lingering tone: the last third of a tap is near-silent.
        let samples = synthesize(Sound::Move);
        let tail = &samples[samples.len() * 2 / 3..];
        assert!(tail.iter().all(|s| s.abs() < 0.01));
    }

    #[test]
    fn mixer_plays_voices_to_the_end_then_drops_them() {
        let mut mixer = Mixer::default();
        mixer.voices.push(Voice {
            samples: synthesize(Sound::Confirm).into(),
            position: 0,
        });
        mixer.render(256);
        assert!(mixer.scratch.iter().any(|s| s.abs() > 0.0));
        mixer.render(RATE);
        assert!(mixer.voices.is_empty());
        mixer.render(64);
        assert!(
            mixer.scratch.iter().all(|&s| s == 0.0),
            "silence once every voice ended"
        );
    }
}
