//! Insert effects. Each processes one stereo sample at a time, so automation can move
//! any parameter per sample.

use pinna_core::{Device, Kind};

use crate::dsp::{Biquad, Shape, db, smooth};

/// Detector floor; silence sits here and never triggers gain reduction.
const FLOOR_DB: f64 = -120.0;

#[derive(Clone, Debug)]
enum Fx {
    Gain {
        gain: f64,
    },
    Filter {
        shape: Shape,
        freq: f64,
        q: f64,
        gain_db: f64,
        l: Biquad,
        r: Biquad,
    },
    Compressor {
        threshold: f64,
        ratio: f64,
        attack: f64,
        release: f64,
        makeup: f64,
        level_db: f64,
    },
    Saturator {
        drive: f64,
        mix: f64,
        out: f64,
    },
}

#[derive(Clone, Debug)]
pub struct Effect {
    fx: Fx,
    bypass: bool,
    sr: f64,
    /// Track index whose stem drives the compressor detector.
    pub sidechain: Option<usize>,
}

impl Effect {
    pub fn new(dev: &Device, sr: f64, sidechain: Option<usize>) -> Self {
        let filter = |shape| Fx::Filter {
            shape,
            freq: 1000.0,
            q: 0.707,
            gain_db: 0.0,
            l: Biquad::default(),
            r: Biquad::default(),
        };
        let fx = match dev.kind {
            Kind::Gain => Fx::Gain { gain: 1.0 },
            Kind::HighPass => filter(Shape::HighPass),
            Kind::LowPass => filter(Shape::LowPass),
            Kind::Peak => filter(Shape::Peak),
            Kind::LowShelf => filter(Shape::LowShelf),
            Kind::HighShelf => filter(Shape::HighShelf),
            Kind::Compressor => Fx::Compressor {
                threshold: 0.0,
                ratio: 1.0,
                attack: 0.0,
                release: 0.0,
                makeup: 0.0,
                level_db: FLOOR_DB,
            },
            Kind::Saturator => Fx::Saturator {
                drive: 1.0,
                mix: 1.0,
                out: 1.0,
            },
            Kind::SawBass | Kind::Pad | Kind::Pluck | Kind::Drums => {
                unreachable!("validation keeps instruments out of fx chains")
            }
        };
        let mut effect = Self {
            fx,
            bypass: dev.bypass,
            sr,
            sidechain,
        };
        for spec in dev.kind.params() {
            effect.set(spec.name, dev.param(spec.name));
        }
        effect
    }

    pub fn set(&mut self, name: &str, v: f64) {
        let sr = self.sr;
        match (&mut self.fx, name) {
            (Fx::Gain { gain }, "gain_db") => *gain = db(v),
            (Fx::Filter { freq, .. }, "freq") => *freq = v,
            (Fx::Filter { q, .. }, "q") => *q = v,
            (Fx::Filter { gain_db, .. }, "gain_db") => *gain_db = v,
            (Fx::Compressor { threshold, .. }, "threshold_db") => *threshold = v,
            (Fx::Compressor { ratio, .. }, "ratio") => *ratio = v,
            (Fx::Compressor { attack, .. }, "attack_ms") => *attack = smooth(v, sr),
            (Fx::Compressor { release, .. }, "release_ms") => *release = smooth(v, sr),
            (Fx::Compressor { makeup, .. }, "makeup_db") => *makeup = v,
            (Fx::Saturator { drive, .. }, "drive_db") => *drive = db(v),
            (Fx::Saturator { mix, .. }, "mix") => *mix = v,
            (Fx::Saturator { out, .. }, "output_db") => *out = db(v),
            (fx, _) => unreachable!("validated param {name} on {fx:?}"),
        }
        if let Fx::Filter {
            shape,
            freq,
            q,
            gain_db,
            l,
            r,
        } = &mut self.fx
        {
            l.set(*shape, *freq, *q, *gain_db, sr);
            r.set(*shape, *freq, *q, *gain_db, sr);
        }
    }

    /// One stereo sample. `side` is the sidechain signal when a source is set.
    pub fn process(&mut self, l: f64, r: f64, side: Option<f64>) -> (f64, f64) {
        if self.bypass {
            return (l, r);
        }
        match &mut self.fx {
            Fx::Gain { gain } => (l * *gain, r * *gain),
            Fx::Filter { l: fl, r: fr, .. } => (fl.run(l), fr.run(r)),
            Fx::Compressor {
                threshold,
                ratio,
                attack,
                release,
                makeup,
                level_db,
            } => {
                let x = side.unwrap_or(l.abs().max(r.abs())).abs();
                let x_db = if x > 0.0 {
                    (20.0 * x.log10()).max(FLOOR_DB)
                } else {
                    FLOOR_DB
                };
                let coef = if x_db > *level_db { *attack } else { *release };
                *level_db = x_db + coef * (*level_db - x_db);
                let over = (*level_db - *threshold).max(0.0);
                let gain = db(*makeup - over * (1.0 - 1.0 / *ratio));
                (l * gain, r * gain)
            }
            Fx::Saturator { drive, mix, out } => {
                let sat = |x: f64| (x * (1.0 - *mix) + (x * *drive).tanh() * *mix) * *out;
                (sat(l), sat(r))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Noise;

    const SR: f64 = 48_000.0;

    fn effects() -> impl Iterator<Item = Kind> {
        Kind::ALL.into_iter().filter(|k| !k.is_instrument())
    }

    /// Every combination of min and max for all params of a kind.
    fn extremes(kind: Kind) -> Vec<Device> {
        let specs = kind.params();
        (0..1u32 << specs.len())
            .map(|mask| {
                specs
                    .iter()
                    .enumerate()
                    .fold(Device::new("fx", kind), |d, (i, s)| {
                        d.with(s.name, if mask >> i & 1 == 1 { s.max } else { s.min })
                    })
            })
            .collect()
    }

    fn run(e: &mut Effect, input: &[f64], side: Option<&[f64]>) -> Vec<(f64, f64)> {
        input
            .iter()
            .enumerate()
            .map(|(i, x)| e.process(*x, -*x, side.map(|s| s[i])))
            .collect()
    }

    #[test]
    fn silence_in_gives_exact_silence_out() {
        let zeros = vec![0.0; 4800];
        for kind in effects() {
            for dev in extremes(kind) {
                for side in [None, Some(0)] {
                    let mut e = Effect::new(&dev, SR, side);
                    let loud: Vec<f64> = vec![1.0; 4800];
                    let side_signal = side.map(|_| loud.as_slice());
                    for (l, r) in run(&mut e, &zeros, side_signal) {
                        assert!(l == 0.0 && r == 0.0, "{dev:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn extreme_params_stay_finite_and_bounded() {
        let mut noise = Noise::new(1);
        let loud: Vec<f64> = (0..24_000).map(|_| 10.0 * noise.sample()).collect();
        for kind in effects() {
            for dev in extremes(kind) {
                let mut e = Effect::new(&dev, SR, None);
                let peak = run(&mut e, &loud, None)
                    .into_iter()
                    .map(|(l, r)| {
                        assert!(l.is_finite() && r.is_finite(), "{dev:?}");
                        l.abs().max(r.abs())
                    })
                    .fold(0.0, f64::max);
                assert!(peak < 1e4, "{dev:?} peak {peak}");
            }
        }
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn sine(freq: f64, secs: f64) -> Vec<f64> {
        (0..(secs * SR) as usize)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / SR).sin())
            .collect()
    }

    /// Gain in dB of a steady sine through the effect, measured after it settled.
    fn sine_gain_db(dev: &Device, freq: f64) -> f64 {
        let input = sine(freq, 3.0);
        let mut e = Effect::new(dev, SR, None);
        let out: Vec<f64> = run(&mut e, &input, None).into_iter().map(|s| s.0).collect();
        let settled = SR as usize;
        20.0 * (rms(&out[settled..]) / rms(&input[settled..])).log10()
    }

    #[test]
    fn highpass_40_hz() {
        let hp = Device::new("hp", Kind::HighPass)
            .with("freq", 40.0)
            .with("q", 0.707);
        // Second-order Butterworth: |H(f)| = (f/fc)^2 / sqrt(1 + (f/fc)^4).
        let expected = |f: f64| {
            let x = (f / 40.0).powi(2);
            20.0 * (x / (1.0 + x * x).sqrt()).log10()
        };
        let low = sine_gain_db(&hp, 25.0);
        assert!((low - expected(25.0)).abs() < 0.5, "25 Hz: {low} dB");
        assert!(low < -8.0, "25 Hz: {low} dB");
        let high = sine_gain_db(&hp, 200.0);
        assert!(high.abs() < 0.1, "200 Hz: {high} dB");
    }

    #[test]
    fn eq_shapes_hit_their_gain() {
        let at = |kind, freq| {
            let dev = Device::new("eq", kind)
                .with("freq", 1000.0)
                .with("gain_db", 6.0);
            sine_gain_db(&dev, freq)
        };
        assert!((at(Kind::Peak, 1000.0) - 6.0).abs() < 0.1);
        assert!(at(Kind::Peak, 100.0).abs() < 0.5);
        assert!((at(Kind::LowShelf, 50.0) - 6.0).abs() < 0.3);
        assert!(at(Kind::LowShelf, 15_000.0).abs() < 0.3);
        assert!((at(Kind::HighShelf, 15_000.0) - 6.0).abs() < 0.3);
        assert!(at(Kind::HighShelf, 50.0).abs() < 0.3);
        let lp = Device::new("lp", Kind::LowPass).with("freq", 1000.0);
        assert!(sine_gain_db(&lp, 100.0).abs() < 0.1);
        assert!(sine_gain_db(&lp, 8000.0) < -30.0);
    }

    #[test]
    fn compressor_reduces_above_threshold_only() {
        let dev = Device::new("c", Kind::Compressor)
            .with("threshold_db", -20.0)
            .with("ratio", 4.0);
        let quiet: Vec<f64> = sine(1000.0, 1.0).iter().map(|x| x * 0.01).collect();
        let mut e = Effect::new(&dev, SR, None);
        let out: Vec<f64> = run(&mut e, &quiet, None).iter().map(|s| s.0).collect();
        assert!((rms(&out) / rms(&quiet) - 1.0).abs() < 1e-12);
        // 0 dBFS peak is 20 dB over: 15 dB reduction at 4:1.
        let loud = sine(1000.0, 1.0);
        let mut e = Effect::new(&dev, SR, None);
        let out: Vec<f64> = run(&mut e, &loud, None).iter().map(|s| s.0).collect();
        let reduction = 20.0 * (rms(&out[24_000..]) / rms(&loud[24_000..])).log10();
        assert!((-16.0..-13.0).contains(&reduction), "{reduction} dB");
    }

    #[test]
    fn saturator_soft_clips() {
        let dev = Device::new("s", Kind::Saturator).with("drive_db", 24.0);
        let mut e = Effect::new(&dev, SR, None);
        let out = run(&mut e, &sine(100.0, 0.1), None);
        let peak = out.iter().map(|s| s.0.abs()).fold(0.0, f64::max);
        assert!(peak <= 1.0 && peak > 0.99);
        let bypassed = Device {
            bypass: true,
            ..dev
        };
        let mut e = Effect::new(&bypassed, SR, None);
        assert_eq!(e.process(0.5, -0.25, None), (0.5, -0.25));
    }
}
