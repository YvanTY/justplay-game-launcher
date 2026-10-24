//! Sound through SDL3's audio stream and a tiny mixer. UI sounds are synthesized at startup and
//! the ambient pad is generated live by a few table oscillators, so nothing copyrighted ships,
//! no decoder is needed, and the device can be fully paused while a game runs.

use std::f32::consts::TAU;
use std::ffi::{CStr, c_int, c_void};
use std::sync::{Arc, Mutex};

use sdl3_sys::everything::*;

const RATE: usize = 48_000;
const CHANNELS: usize = 2;
const MASTER: f32 = 0.8;
const SFX_VOLUME: f32 = 0.7;
const AMBIENT_VOLUME: f32 = 0.22;
/// Seconds for the ambient pad to fade fully in or out.
const AMBIENT_FADE: f32 = 1.5;
const TABLE_BITS: u32 = 11;
const TABLE_SIZE: usize = 1 << TABLE_BITS;

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
        let mixer = Arc::new(Mutex::new(Mixer::new()));
        let spec = SDL_AudioSpec {
            format: SDL_AUDIO_F32,
            channels: CHANNELS as c_int,
            freq: RATE as c_int,
        };
        // The stream holds one reference to the mixer; Drop gives it back after destroying the stream.
        let userdata = Arc::into_raw(mixer.clone()) as *mut c_void;
        let stream = unsafe {
            SDL_OpenAudioDeviceStream(
                SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK,
                &spec,
                Some(feed),
                userdata,
            )
        };
        if stream.is_null() {
            unsafe { drop(Arc::from_raw(userdata as *const Mutex<Mixer>)) };
            return Err(format!("no audio output: {}", sdl_error()));
        }
        let audio = Audio {
            stream,
            mixer,
            sounds: ALL_SOUNDS.iter().map(|&s| synthesize(s).into()).collect(),
        };
        audio.resume();
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
    }

    pub fn set_ambient(&self, on: bool) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.ambient_target = if on { 1.0 } else { 0.0 };
        }
    }

    /// Stops the audio device entirely (no callbacks, no CPU) until `resume`.
    pub fn pause(&self) {
        unsafe { SDL_PauseAudioStreamDevice(self.stream) };
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.voices.clear();
        }
    }

    pub fn resume(&self) {
        unsafe { SDL_ResumeAudioStreamDevice(self.stream) };
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        unsafe {
            SDL_DestroyAudioStream(self.stream);
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

struct Mixer {
    voices: Vec<Voice>,
    pad: Pad,
    ambient_level: f32,
    ambient_target: f32,
    scratch: Vec<f32>,
}

impl Mixer {
    fn new() -> Mixer {
        Mixer {
            voices: Vec::new(),
            pad: Pad::new(),
            ambient_level: 0.0,
            ambient_target: 1.0,
            scratch: Vec::new(),
        }
    }

    fn render(&mut self, frames: usize) {
        self.scratch.clear();
        self.scratch.resize(frames * CHANNELS, 0.0);

        let step = 1.0 / (AMBIENT_FADE * RATE as f32);
        if self.ambient_level > 0.0 || self.ambient_target > 0.0 {
            self.pad.render(
                &mut self.scratch,
                |level| {
                    // Linear fade toward the target, one step per frame.
                    if *level < self.ambient_target {
                        *level = (*level + step).min(self.ambient_target);
                    } else if *level > self.ambient_target {
                        *level = (*level - step).max(self.ambient_target);
                    }
                },
                &mut self.ambient_level,
            );
        }

        for voice in &mut self.voices {
            let remaining = &voice.samples[voice.position..];
            let count = remaining.len().min(self.scratch.len());
            for (out, sample) in self.scratch.iter_mut().zip(&remaining[..count]) {
                *out += sample * SFX_VOLUME;
            }
            voice.position += count;
        }
        self.voices
            .retain(|voice| voice.position < voice.samples.len());

        for sample in &mut self.scratch {
            *sample = (*sample * MASTER).clamp(-1.0, 1.0);
        }
    }
}

/// The ambient bed: a soft A-minor chord of detuned sine pairs whose loudness drifts slowly.
struct Pad {
    table: Vec<f32>,
    voices: Vec<PadVoice>,
    /// Frames left before the swells are recomputed.
    until_swell: usize,
}

/// Phases are 32-bit fixed point (a full cycle = 2^32), so they wrap for free: a float `%`
/// per voice per sample cost more than everything else in the mixer combined.
struct PadVoice {
    phase: u32,
    increment: u32,
    swell_phase: u32,
    swell_increment: u32,
    swell: f32,
    left: f32,
    right: f32,
}

/// The swells move over seconds, so updating them every 32 frames is inaudible.
const SWELL_BLOCK: usize = 32;

fn cycles_to_increment(hz: f64) -> u32 {
    (hz / RATE as f64 * 4_294_967_296.0) as u32
}

impl Pad {
    fn new() -> Pad {
        let table = (0..TABLE_SIZE)
            .map(|i| (TAU * i as f32 / TABLE_SIZE as f32).sin())
            .collect();
        // A2 E3 A3 C4 E4 G4; each note gets a partner 0.15 Hz higher for a slow chorus.
        let notes = [110.0, 164.81, 220.0, 261.63, 329.63, 392.0];
        let mut voices = Vec::new();
        for (i, &note) in notes.iter().enumerate() {
            for (j, detune) in [0.0, 0.15].into_iter().enumerate() {
                let n = i * 2 + j;
                let pan = (n as f32 / (notes.len() * 2 - 1) as f32) * 0.6 + 0.2;
                voices.push(PadVoice {
                    phase: (n as u32).wrapping_mul(0x9E37_79B9),
                    increment: cycles_to_increment(note + detune),
                    swell_phase: (n as u32).wrapping_mul(0x7F4A_7C15),
                    // One swell every 9-20 s per voice, all different, so the texture never repeats exactly.
                    swell_increment: cycles_to_increment(1.0 / (9.0 + n as f64))
                        .wrapping_mul(SWELL_BLOCK as u32),
                    swell: 1.0,
                    left: (1.0 - pan).sqrt(),
                    right: pan.sqrt(),
                });
            }
        }
        Pad {
            table,
            voices,
            until_swell: 0,
        }
    }

    fn render(&mut self, out: &mut [f32], mut fade: impl FnMut(&mut f32), level: &mut f32) {
        let gain = AMBIENT_VOLUME / self.voices.len() as f32 * 2.5;
        for frame in out.as_chunks_mut::<CHANNELS>().0 {
            fade(level);
            if *level <= 0.0 {
                continue;
            }
            if self.until_swell == 0 {
                self.until_swell = SWELL_BLOCK;
                for voice in &mut self.voices {
                    voice.swell = 0.55 + 0.45 * lookup(&self.table, voice.swell_phase);
                    voice.swell_phase = voice.swell_phase.wrapping_add(voice.swell_increment);
                }
            }
            self.until_swell -= 1;
            let (mut l, mut r) = (0.0, 0.0);
            for voice in &mut self.voices {
                let sample = lookup(&self.table, voice.phase) * voice.swell;
                l += sample * voice.left;
                r += sample * voice.right;
                voice.phase = voice.phase.wrapping_add(voice.increment);
            }
            frame[0] += l * gain * *level;
            frame[1] += r * gain * *level;
        }
    }
}

/// The top bits of the fixed-point phase index the table directly.
fn lookup(table: &[f32], phase: u32) -> f32 {
    table[(phase >> (32 - TABLE_BITS)) as usize]
}

/// One UI sound as interleaved stereo samples.
fn synthesize(sound: Sound) -> Vec<f32> {
    let mono = match sound {
        // A soft, short tick like the PS5 menu.
        Sound::Move => tone(1800.0, 1800.0, 0.045, 0.010, 0.22),
        // A low thud when there is nowhere further to go.
        Sound::Bump => tone(150.0, 120.0, 0.09, 0.025, 0.35),
        Sound::Tab => tone(1150.0, 1350.0, 0.07, 0.018, 0.22),
        Sound::Confirm => mix(
            &tone(880.0, 880.0, 0.10, 0.035, 0.22),
            &tone(1320.0, 1320.0, 0.16, 0.045, 0.22),
            0.05,
        ),
        Sound::Back => mix(
            &tone(1100.0, 1100.0, 0.10, 0.035, 0.2),
            &tone(740.0, 740.0, 0.16, 0.045, 0.2),
            0.05,
        ),
        Sound::Launch => launch_swoosh(),
    };
    mono.iter().flat_map(|&s| [s, s]).collect()
}

/// A sine sweep with a 2 ms attack and exponential decay (`decay` = time constant in seconds).
fn tone(start_hz: f32, end_hz: f32, seconds: f32, decay: f32, amplitude: f32) -> Vec<f32> {
    let count = (seconds * RATE as f32) as usize;
    let mut phase = 0.0f32;
    (0..count)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let progress = i as f32 / count as f32;
            phase += TAU * (start_hz + (end_hz - start_hz) * progress) / RATE as f32;
            let attack = (t / 0.002).min(1.0);
            let envelope = attack * (-t / decay).exp();
            // A touch of the second harmonic keeps it from sounding like a test tone.
            (phase.sin() + 0.25 * (2.0 * phase).sin()) * envelope * amplitude
        })
        .collect()
}

/// Lays `second` over `first`, starting `offset` seconds in.
fn mix(first: &[f32], second: &[f32], offset: f32) -> Vec<f32> {
    let start = (offset * RATE as f32) as usize;
    let mut out = first.to_vec();
    out.resize(out.len().max(start + second.len()), 0.0);
    for (i, sample) in second.iter().enumerate() {
        out[start + i] += sample;
    }
    out
}

/// Rising tone plus filtered noise that swells and fades: the "game is starting" sound.
fn launch_swoosh() -> Vec<f32> {
    let seconds = 1.1;
    let count = (seconds * RATE as f32) as usize;
    let mut phase = 0.0f32;
    let mut noise_state = 0x1234_5678u32;
    let mut filtered = 0.0f32;
    (0..count)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let rise = (t / 0.45).min(1.0);
            phase += TAU * (260.0 + 900.0 * rise * rise) / RATE as f32;
            // Swell for 0.45 s, then fade out.
            let envelope = if t < 0.45 {
                (t / 0.45).powf(1.5)
            } else {
                (-(t - 0.45) / 0.18).exp()
            };
            noise_state = noise_state
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            let white = (noise_state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            filtered += (white - filtered) * (0.04 + 0.2 * rise);
            ((phase.sin() + 0.3 * (2.0 * phase).sin()) * 0.18 + filtered * 0.5) * envelope
        })
        .collect()
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
    fn every_sound_is_short_stereo_and_never_clips() {
        for sound in ALL_SOUNDS {
            let samples = synthesize(sound);
            assert_eq!(samples.len() % 2, 0, "{sound:?} must be stereo");
            let seconds = samples.len() as f32 / (RATE * CHANNELS) as f32;
            assert!(
                seconds > 0.0 && seconds <= 1.2,
                "{sound:?} lasts {seconds}s"
            );
            let peak = samples.iter().fold(0.0f32, |p, s| p.max(s.abs()));
            assert!(peak < 1.0 && peak > 0.05, "{sound:?} peak {peak}");
        }
    }

    #[test]
    fn mixer_plays_voices_to_the_end_then_drops_them() {
        let mut mixer = Mixer::new();
        mixer.ambient_target = 0.0;
        mixer.voices.push(Voice {
            samples: synthesize(Sound::Move).into(),
            position: 0,
        });
        mixer.render(512);
        assert!(mixer.scratch.iter().any(|s| s.abs() > 0.0));
        mixer.render(RATE);
        assert!(mixer.voices.is_empty());
    }

    #[test]
    fn ambient_fades_in_gradually_and_stays_in_range() {
        let mut mixer = Mixer::new();
        mixer.render(RATE / 10);
        let early = mixer.ambient_level;
        assert!(early > 0.0 && early < 0.2, "level after 0.1 s: {early}");
        mixer.render(RATE * 2);
        assert_eq!(mixer.ambient_level, 1.0);
        assert!(
            mixer
                .scratch
                .iter()
                .all(|s| s.abs() <= AMBIENT_VOLUME * 2.0)
        );
    }

    #[test]
    fn ambient_off_produces_silence_after_fade() {
        let mut mixer = Mixer::new();
        mixer.render(RATE * 2);
        mixer.ambient_target = 0.0;
        mixer.render(RATE * 2);
        mixer.render(256);
        assert!(mixer.scratch.iter().all(|&s| s == 0.0));
    }
}
