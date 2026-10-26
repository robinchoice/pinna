//! Built-in instruments. They render mono, one sample at a time; the track pans them.
//! Voices read the instrument's params every sample, so automation applies at once,
//! except Pluck and Drums, which take their params when a note starts.

use std::f64::consts::TAU;

use pinna_core::{Device, Kind};

use crate::dsp::{Biquad, Noise, Shape, Svf, db, decay60, midi_hz, samples, smooth};

#[derive(Clone)]
pub enum Instrument {
    SawBass(Box<SawBass>),
    Pad(Pad),
    Pluck(Pluck),
    Drums(Drums),
}

impl Instrument {
    pub fn new(dev: &Device, sr: f64) -> Self {
        let mut inst = match dev.kind {
            Kind::SawBass => Self::SawBass(Box::new(SawBass::new(sr))),
            Kind::Pad => Self::Pad(Pad::new(sr)),
            Kind::Pluck => Self::Pluck(Pluck::new(sr)),
            Kind::Drums => Self::Drums(Drums::new(sr)),
            kind => unreachable!("validation rejects {kind:?} as instrument"),
        };
        for spec in dev.kind.params() {
            inst.set(spec.name, dev.param(spec.name));
        }
        inst
    }

    pub fn set(&mut self, name: &str, v: f64) {
        match self {
            Self::SawBass(i) => i.set(name, v),
            Self::Pad(i) => i.set(name, v),
            Self::Pluck(i) => i.set(name, v),
            Self::Drums(i) => i.set(name, v),
        }
    }

    pub fn note_on(&mut self, id: usize, pitch: u8, vel: u8) {
        let vel = f64::from(vel) / 127.0;
        match self {
            Self::SawBass(i) => i.note_on(id, pitch, vel),
            Self::Pad(i) => i.note_on(id, pitch, vel),
            Self::Pluck(i) => i.note_on(id, pitch, vel),
            Self::Drums(i) => i.note_on(pitch, vel),
        }
    }

    pub fn note_off(&mut self, id: usize) {
        match self {
            Self::SawBass(i) => i.note_off(id),
            Self::Pad(i) => i.note_off(id),
            Self::Pluck(i) => i.note_off(id),
            // Drum hits are one-shots.
            Self::Drums(_) => {}
        }
    }

    pub fn sample(&mut self) -> f64 {
        match self {
            Self::SawBass(i) => i.sample(),
            Self::Pad(i) => i.sample(),
            Self::Pluck(i) => i.sample(),
            Self::Drums(i) => i.sample(),
        }
    }

    /// Drop finished voices. They output exact zeros, so when this runs does not
    /// change the result.
    pub fn tidy(&mut self) {
        match self {
            Self::SawBass(_) => {}
            Self::Pad(i) => i.voices.retain(|v| !v.done),
            Self::Pluck(i) => i.voices.retain(|v| !v.done),
            Self::Drums(i) => i.voices.retain(|v| !v.done),
        }
    }
}

fn seed(pitch: u8, vel: f64) -> u32 {
    u32::from(pitch) << 8 | (vel * 127.0).round() as u32
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Monophonic saw with sine sub, resonant low-pass and ADSR. Overlapping notes play
/// legato: no retrigger, the pitch glides over `glide_ms`.
#[derive(Clone)]
pub struct SawBass {
    sr: f64,
    cutoff: f64,
    k: f64,
    sub: f64,
    attack: f64,
    decay: f64,
    sustain: f64,
    release: f64,
    glide: f64,
    level: f64,
    filter: Svf,
    note: Option<usize>,
    pitch: f64,
    target: f64,
    dt: f64,
    phase: f64,
    sub_phase: f64,
    vel: f64,
    env: f64,
    stage: Stage,
    release_step: f64,
}

impl SawBass {
    fn new(sr: f64) -> Self {
        Self {
            sr,
            cutoff: 800.0,
            k: 2.0,
            sub: 0.0,
            attack: 1.0,
            decay: 1.0,
            sustain: 1.0,
            release: 1.0,
            glide: 0.0,
            level: 1.0,
            filter: Svf::default(),
            note: None,
            pitch: 0.0,
            target: 0.0,
            dt: 0.0,
            phase: 0.0,
            sub_phase: 0.0,
            vel: 0.0,
            env: 0.0,
            stage: Stage::Idle,
            release_step: 0.0,
        }
    }

    fn set(&mut self, name: &str, v: f64) {
        let sr = self.sr;
        match name {
            "cutoff" => self.cutoff = v,
            // Resonance 1 is Q 25: loud ringing, still stable.
            "resonance" => self.k = 2.0 * (1.0 - 0.98 * v),
            "sub" => self.sub = v,
            "attack_ms" => self.attack = samples(v, sr),
            "decay_ms" => self.decay = samples(v, sr),
            "sustain" => self.sustain = v,
            "release_ms" => self.release = samples(v, sr),
            "glide_ms" => self.glide = smooth(v, sr),
            "level_db" => self.level = db(v),
            _ => unreachable!("validated SawBass param {name}"),
        }
        self.filter.set(self.cutoff, self.k, sr);
    }

    fn note_on(&mut self, id: usize, pitch: u8, vel: f64) {
        self.target = f64::from(pitch);
        if self.note.is_none() {
            self.pitch = self.target;
            self.dt = midi_hz(self.pitch) / self.sr;
            self.vel = vel;
            self.stage = Stage::Attack;
        }
        self.note = Some(id);
    }

    fn note_off(&mut self, id: usize) {
        if self.note == Some(id) {
            self.note = None;
            self.stage = Stage::Release;
            self.release_step = self.env / self.release;
        }
    }

    fn sample(&mut self) -> f64 {
        if self.pitch != self.target {
            self.pitch = self.target + (self.pitch - self.target) * self.glide;
            if (self.pitch - self.target).abs() < 1e-6 {
                self.pitch = self.target;
            }
            self.dt = midi_hz(self.pitch) / self.sr;
        }
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.env += 1.0 / self.attack;
                if self.env >= 1.0 {
                    self.env = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.env -= (1.0 - self.sustain) / self.decay;
                if self.env <= self.sustain {
                    self.env = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.env = self.sustain,
            Stage::Release => {
                self.env -= self.release_step;
                if self.env <= 0.0 {
                    self.env = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        let saw = 2.0 * self.phase - 1.0 - poly_blep(self.phase, self.dt);
        let sub = (TAU * self.sub_phase).sin();
        self.phase = (self.phase + self.dt).fract();
        self.sub_phase = (self.sub_phase + 0.5 * self.dt).fract();
        let y = self.filter.low(0.5 * saw + self.sub * sub);
        y * self.env * self.vel * self.level
    }
}

/// Smooths the saw's jump so it does not alias.
fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// Polyphonic pad: three detuned triangles per voice, shared low-pass, AR envelope.
#[derive(Clone)]
pub struct Pad {
    sr: f64,
    filter: Svf,
    ratios: [f64; 3],
    attack: f64,
    release: f64,
    level: f64,
    voices: Vec<PadVoice>,
}

#[derive(Clone)]
struct PadVoice {
    id: usize,
    dt: f64,
    phases: [f64; 3],
    vel: f64,
    env: f64,
    release_step: Option<f64>,
    done: bool,
}

impl Pad {
    fn new(sr: f64) -> Self {
        Self {
            sr,
            filter: Svf::default(),
            ratios: [1.0; 3],
            attack: 1.0,
            release: 1.0,
            level: 1.0,
            voices: Vec::new(),
        }
    }

    fn set(&mut self, name: &str, v: f64) {
        let sr = self.sr;
        match name {
            "cutoff" => self.filter.set(v, std::f64::consts::SQRT_2, sr),
            "detune_cents" => {
                let r = 2f64.powf(v / 1200.0);
                self.ratios = [1.0 / r, 1.0, r];
            }
            "attack_ms" => self.attack = samples(v, sr),
            "release_ms" => self.release = samples(v, sr),
            "level_db" => self.level = db(v),
            _ => unreachable!("validated Pad param {name}"),
        }
    }

    fn note_on(&mut self, id: usize, pitch: u8, vel: f64) {
        self.voices.push(PadVoice {
            id,
            dt: midi_hz(f64::from(pitch)) / self.sr,
            phases: [0.0, 1.0 / 3.0, 2.0 / 3.0],
            vel,
            env: 0.0,
            release_step: None,
            done: false,
        });
    }

    fn note_off(&mut self, id: usize) {
        let release = self.release;
        for v in self.voices.iter_mut().filter(|v| v.id == id) {
            v.release_step = Some(v.env / release);
        }
    }

    fn sample(&mut self) -> f64 {
        let mut sum = 0.0;
        for v in &mut self.voices {
            if v.done {
                continue;
            }
            match v.release_step {
                None => v.env = (v.env + 1.0 / self.attack).min(1.0),
                Some(step) => {
                    v.env -= step;
                    if v.env <= 0.0 {
                        v.env = 0.0;
                        v.done = true;
                    }
                }
            }
            let mut tri = 0.0;
            for (phase, ratio) in v.phases.iter_mut().zip(self.ratios) {
                tri += 4.0 * (*phase - 0.5).abs() - 1.0;
                *phase = (*phase + v.dt * ratio).fract();
            }
            sum += tri * v.env * v.vel;
        }
        self.filter.low(sum) * 0.1 * self.level
    }
}

/// Karplus-Strong string. `decay_ms` is the time to fall by 60 dB, `brightness`
/// sets both the excitation and the damping in the loop.
#[derive(Clone)]
pub struct Pluck {
    sr: f64,
    decay_ms: f64,
    brightness: f64,
    level: f64,
    voices: Vec<PluckVoice>,
}

#[derive(Clone)]
struct PluckVoice {
    id: usize,
    line: Vec<f64>,
    write: usize,
    delay: f64,
    feedback: f64,
    damp: f64,
    last: f64,
    gain: f64,
    life: f64,
    fade: f64,
    fade_step: f64,
    done: bool,
}

/// Note-off fades the string out over this time instead of cutting it.
const PLUCK_FADE_MS: f64 = 30.0;

impl Pluck {
    fn new(sr: f64) -> Self {
        Self {
            sr,
            decay_ms: 400.0,
            brightness: 0.6,
            level: 1.0,
            voices: Vec::new(),
        }
    }

    fn set(&mut self, name: &str, v: f64) {
        match name {
            "decay_ms" => self.decay_ms = v,
            "brightness" => self.brightness = v,
            "level_db" => self.level = db(v),
            _ => unreachable!("validated Pluck param {name}"),
        }
    }

    fn note_on(&mut self, id: usize, pitch: u8, vel: f64) {
        // The averaging filter in the loop adds half a sample of delay.
        let delay = (self.sr / midi_hz(f64::from(pitch)) - 0.5).max(1.0);
        let len = delay.ceil() as usize + 2;
        let mut noise = Noise::new(seed(pitch, vel));
        let smooth = 0.9 * (1.0 - self.brightness);
        let mut prev = 0.0;
        let mut line: Vec<f64> = (0..len)
            .map(|_| {
                prev = (1.0 - smooth) * noise.sample() + smooth * prev;
                prev
            })
            .collect();
        let mean = line.iter().sum::<f64>() / len as f64;
        line.iter_mut().for_each(|x| *x -= mean);
        self.voices.push(PluckVoice {
            id,
            line,
            write: 0,
            delay,
            feedback: decay60(self.decay_ms, self.sr).powf(delay + 0.5),
            damp: 0.5 * (1.0 - self.brightness),
            last: 0.0,
            gain: vel * 0.5 * self.level,
            // -90 dB: the rest is inaudible.
            life: 1.5 * samples(self.decay_ms, self.sr),
            fade: 1.0,
            fade_step: 0.0,
            done: false,
        });
    }

    fn note_off(&mut self, id: usize) {
        let step = 1.0 / samples(PLUCK_FADE_MS, self.sr);
        for v in self.voices.iter_mut().filter(|v| v.id == id) {
            v.fade_step = step;
        }
    }

    fn sample(&mut self) -> f64 {
        let mut sum = 0.0;
        for v in &mut self.voices {
            if v.done {
                continue;
            }
            let len = v.line.len();
            let pos = (v.write as f64 - v.delay).rem_euclid(len as f64);
            let i = pos as usize % len;
            let frac = pos - pos.floor();
            let x = v.line[i] * (1.0 - frac) + v.line[(i + 1) % len] * frac;
            let y = (1.0 - v.damp) * x + v.damp * v.last;
            v.last = x;
            v.line[v.write] = v.feedback * y;
            v.write = (v.write + 1) % len;
            sum += x * v.gain * v.fade;
            v.fade -= v.fade_step;
            v.life -= 1.0;
            if v.life <= 0.0 || v.fade <= 0.0 {
                v.done = true;
            }
        }
        sum
    }
}

/// Drum synth: C2 kick, D2 snare, F#2 closed hat, A#2 open hat. Other pitches are
/// silent. Noise is seeded from pitch and velocity, so every hit renders the same.
#[derive(Clone)]
pub struct Drums {
    sr: f64,
    kick_tune: f64,
    kick_decay_ms: f64,
    kick_level: f64,
    snare_level: f64,
    hat_level: f64,
    hat_decay_ms: f64,
    voices: Vec<DrumVoice>,
}

pub const KICK: u8 = 36;
pub const SNARE: u8 = 38;
pub const CLOSED_HAT: u8 = 42;
pub const OPEN_HAT: u8 = 46;

/// Below this the envelopes are inaudible and a voice ends.
const DRUM_END: f64 = 1e-4;

#[derive(Clone)]
struct DrumVoice {
    /// Sine frequency at the end of the pitch sweep (0: no tone).
    tune: f64,
    phase: f64,
    sweep: f64,
    sweep_k: f64,
    tone: f64,
    tone_k: f64,
    noise: Noise,
    noise_amp: f64,
    noise_k: f64,
    hp: Option<Biquad>,
    done: bool,
}

impl Drums {
    fn new(sr: f64) -> Self {
        Self {
            sr,
            kick_tune: 50.0,
            kick_decay_ms: 350.0,
            kick_level: 1.0,
            snare_level: 1.0,
            hat_level: 1.0,
            hat_decay_ms: 50.0,
            voices: Vec::new(),
        }
    }

    fn set(&mut self, name: &str, v: f64) {
        match name {
            "kick_tune" => self.kick_tune = v,
            "kick_decay_ms" => self.kick_decay_ms = v,
            "kick_level_db" => self.kick_level = db(v),
            "snare_level_db" => self.snare_level = db(v),
            "hat_level_db" => self.hat_level = db(v),
            "hat_decay_ms" => self.hat_decay_ms = v,
            _ => unreachable!("validated Drums param {name}"),
        }
    }

    fn note_on(&mut self, pitch: u8, vel: f64) {
        let sr = self.sr;
        let silent = DrumVoice {
            tune: 0.0,
            phase: 0.0,
            sweep: 0.0,
            sweep_k: 0.0,
            tone: 0.0,
            tone_k: 0.0,
            noise: Noise::new(seed(pitch, vel)),
            noise_amp: 0.0,
            noise_k: 0.0,
            hp: None,
            done: false,
        };
        let voice = match pitch {
            KICK => DrumVoice {
                tune: self.kick_tune,
                // Starts at 3x the tune and falls to it within ~100 ms.
                sweep: 2.0,
                sweep_k: decay60(100.0, sr),
                tone: vel * self.kick_level,
                tone_k: decay60(self.kick_decay_ms, sr),
                ..silent
            },
            SNARE => DrumVoice {
                tune: 185.0,
                tone: 0.5 * vel * self.snare_level,
                tone_k: decay60(150.0, sr),
                noise_amp: 0.6 * vel * self.snare_level,
                noise_k: decay60(250.0, sr),
                hp: Some(Biquad::new(Shape::HighPass, 1500.0, 0.707, 0.0, sr)),
                ..silent
            },
            CLOSED_HAT | OPEN_HAT => {
                let decay = if pitch == OPEN_HAT { 6.0 } else { 1.0 } * self.hat_decay_ms;
                DrumVoice {
                    noise_amp: 0.6 * vel * self.hat_level,
                    noise_k: decay60(decay, sr),
                    hp: Some(Biquad::new(Shape::HighPass, 7000.0, 0.707, 0.0, sr)),
                    ..silent
                }
            }
            _ => return,
        };
        self.voices.push(voice);
    }

    fn sample(&mut self) -> f64 {
        let mut sum = 0.0;
        for v in &mut self.voices {
            if v.done {
                continue;
            }
            if v.tone > 0.0 {
                // Advance first, so the hit's first sample is already non-zero.
                v.phase = (v.phase + v.tune * (1.0 + v.sweep) / self.sr).fract();
                v.sweep *= v.sweep_k;
                sum += (TAU * v.phase).sin() * v.tone;
                v.tone *= v.tone_k;
            }
            if v.noise_amp > 0.0 {
                let n = v.noise.sample() * v.noise_amp;
                sum += v.hp.as_mut().map_or(n, |hp| hp.run(n));
                v.noise_amp *= v.noise_k;
            }
            v.done = v.tone < DRUM_END && v.noise_amp < DRUM_END;
        }
        sum
    }
}
