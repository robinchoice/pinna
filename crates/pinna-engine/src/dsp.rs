//! Small DSP building blocks shared by instruments and effects. State is f64 so long
//! renders do not drift; buffers between stages are f32.

use std::f64::consts::PI;

use pinna_core::{Lane, Timing};

pub fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

pub fn midi_hz(pitch: f64) -> f64 {
    440.0 * 2f64.powf((pitch - 69.0) / 12.0)
}

/// Duration in samples, at least one so ramps never divide by zero.
pub fn samples(ms: f64, sr: f64) -> f64 {
    (ms * 0.001 * sr).max(1.0)
}

/// Per-sample factor of an exponential decay that falls by 60 dB in `ms`.
pub fn decay60(ms: f64, sr: f64) -> f64 {
    10f64.powf(-3.0 / samples(ms, sr))
}

/// One-pole smoothing coefficient for a time constant in ms (0 = no smoothing).
pub fn smooth(ms: f64, sr: f64) -> f64 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * sr)).exp()
    }
}

/// Filters must stay below Nyquist whatever the sample rate.
pub fn clamp_freq(freq: f64, sr: f64) -> f64 {
    freq.clamp(1.0, 0.45 * sr)
}

/// Deterministic white noise (xorshift32) in [-1, 1).
#[derive(Clone, Debug)]
pub struct Noise(u32);

impl Noise {
    pub fn new(seed: u32) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9) | 1)
    }

    pub fn sample(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        f64::from(x) / 2_147_483_648.0 - 1.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    LowPass,
    HighPass,
    Peak,
    LowShelf,
    HighShelf,
}

/// RBJ cookbook biquad, transposed direct form II.
#[derive(Clone, Debug, Default)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn new(shape: Shape, freq: f64, q: f64, gain_db: f64, sr: f64) -> Self {
        let mut b = Self::default();
        b.set(shape, freq, q, gain_db, sr);
        b
    }

    pub fn set(&mut self, shape: Shape, freq: f64, q: f64, gain_db: f64, sr: f64) {
        let w0 = 2.0 * PI * clamp_freq(freq, sr) / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a = 10f64.powf(gain_db / 40.0);
        let sq = 2.0 * a.sqrt() * alpha;
        let [b0, b1, b2, a0, a1, a2] = match shape {
            Shape::LowPass => [
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ],
            Shape::HighPass => [
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ],
            Shape::Peak => [
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ],
            Shape::LowShelf => [
                a * ((a + 1.0) - (a - 1.0) * cos + sq),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - sq),
                (a + 1.0) + (a - 1.0) * cos + sq,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - sq,
            ],
            Shape::HighShelf => [
                a * ((a + 1.0) + (a - 1.0) * cos + sq),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - sq),
                (a + 1.0) - (a - 1.0) * cos + sq,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - sq,
            ],
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    pub fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Topology-preserving-transform state-variable low-pass (Simper). Stays stable when
/// the cutoff moves every sample.
#[derive(Clone, Debug, Default)]
pub struct Svf {
    a1: f64,
    a2: f64,
    a3: f64,
    ic1: f64,
    ic2: f64,
}

impl Svf {
    /// `k` is 1/Q: 2 is no resonance, near 0 self-oscillation.
    pub fn set(&mut self, cutoff: f64, k: f64, sr: f64) {
        let g = (PI * clamp_freq(cutoff, sr) / sr).tan();
        self.a1 = 1.0 / (1.0 + g * (g + k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    pub fn low(&mut self, x: f64) -> f64 {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        v2
    }
}

/// An automation lane in sample positions. Same shape as [`Lane::value_at`], but
/// continuous between ticks so ramps have no steps.
#[derive(Clone, Debug)]
pub struct Auto {
    points: Vec<(f64, f64)>,
    cursor: usize,
}

impl Auto {
    pub fn new(lane: &Lane, timing: &Timing, sr: u32) -> Self {
        let points = lane
            .points
            .iter()
            .map(|(pos, v)| (timing.seconds(timing.tick(*pos)) * f64::from(sr), *v))
            .collect();
        Self { points, cursor: 0 }
    }

    /// Value at sample position `s`; `s` must not decrease between calls.
    pub fn at(&mut self, s: f64) -> f64 {
        while self.cursor + 1 < self.points.len() && s >= self.points[self.cursor + 1].0 {
            self.cursor += 1;
        }
        let (a, va) = self.points[self.cursor];
        match self.points.get(self.cursor + 1) {
            Some(&(b, vb)) if s > a => va + (vb - va) * (s - a) / (b - a),
            _ => va,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinna_core::Pos;

    #[test]
    fn auto_matches_lane() {
        let t = Timing {
            bpm: 120.0,
            beats_per_bar: 4,
        };
        let lane = Lane {
            target: "gain_db".into(),
            points: vec![
                (Pos::new(1, 2), 0.0),
                (Pos::new(2, 1), -12.0),
                (Pos::new(3, 1), -12.0),
                (Pos::new(3, 1), 6.0),
            ],
        };
        let mut auto = Auto::new(&lane, &t, 48_000);
        for tick in (0..4 * 4 * 960).step_by(97) {
            let s = t.seconds(tick) * 48_000.0;
            let want = lane.value_at(&t, tick).unwrap();
            assert!((auto.at(s) - want).abs() < 1e-9, "tick {tick}");
        }
    }

    #[test]
    fn noise_is_deterministic_and_bounded() {
        let (mut a, mut b) = (Noise::new(7), Noise::new(7));
        for _ in 0..10_000 {
            let x = a.sample();
            assert_eq!(x, b.sample());
            assert!((-1.0..1.0).contains(&x));
        }
        assert_ne!(Noise::new(7).sample(), Noise::new(8).sample());
    }
}
