//! Measurements an agent uses to hear its changes. Pure functions on sample slices,
//! independent of the rest of Pinna, so other tools can use them too.
//!
//! All inputs are stereo pairs `(l, r)` of equal length at `sr` Hz. Levels are dB,
//! loudness follows ITU-R BS.1770 / EBU R128. No output is ever NaN: silence reads
//! `-inf` where noted, band levels bottom out at [`FLOOR_DB`], ratios and differences
//! that would diverge are capped at ±[`CAP`].

use std::f64::consts::PI;

use ebur128::{EbuR128, Mode};
use realfft::RealFftPlanner;
use realfft::num_complex::Complex64;

/// Centre frequencies of the 31 third-octave bands from 20 Hz to 20 kHz (IEC 61260).
pub const THIRD_OCTAVE_HZ: [f64; 31] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
    500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0,
    8000.0, 10_000.0, 12_500.0, 16_000.0, 20_000.0,
];

/// Level of a silent band, dB.
pub const FLOOR_DB: f64 = -150.0;

/// Cap for ratios and differences that diverge (mono loss of L = -R, width of a pure
/// side signal, difference to silence), dB, LU or plain ratio.
pub const CAP: f64 = 99.0;

pub type Pair<'a> = (&'a [f32], &'a [f32]);

#[derive(Clone, Debug, PartialEq)]
pub struct Loudness {
    /// Integrated loudness, LUFS. `-inf` for silence and for input shorter than one
    /// 400 ms gating block.
    pub integrated_lufs: f64,
    /// Highest short-term (3 s) loudness, LUFS, polled every 100 ms. Input shorter than
    /// 3 s counts as padded with silence. `-inf` for silence.
    pub short_term_max_lufs: f64,
    /// Loudness range, LU. 0 for input too short or too quiet to measure.
    pub lra: f64,
    /// True peak (4x oversampled below 96 kHz), dBTP. `-inf` for silence.
    pub true_peak_dbtp: f64,
    /// `-inf` for silence.
    pub sample_peak_dbfs: f64,
}

pub fn loudness(x: Pair, sr: u32) -> Loudness {
    let mode = Mode::I | Mode::S | Mode::LRA | Mode::TRUE_PEAK | Mode::SAMPLE_PEAK;
    let mut m = EbuR128::new(2, sr, mode).expect("valid sample rate");
    let step = (sr as usize / 10).max(1);
    let mut short_term_max_lufs = f64::NEG_INFINITY;
    for (l, r) in x.0.chunks(step).zip(x.1.chunks(step)) {
        m.add_frames_planar_f32(&[l, r]).expect("two channels");
        short_term_max_lufs = short_term_max_lufs.max(m.loudness_shortterm().unwrap());
    }
    let peak = |f: fn(&EbuR128, u32) -> Result<f64, ebur128::Error>| {
        20.0 * f(&m, 0).unwrap().max(f(&m, 1).unwrap()).log10()
    };
    Loudness {
        integrated_lufs: m.loudness_global().unwrap(),
        short_term_max_lufs,
        lra: m.loudness_range().unwrap(),
        true_peak_dbtp: peak(EbuR128::true_peak),
        sample_peak_dbfs: peak(EbuR128::sample_peak),
    }
}

/// Long-term level per third-octave band, dB, one value per entry of
/// [`THIRD_OCTAVE_HZ`]: `10·log10(ms_L + ms_R)` with `ms` the mean square of the channel
/// in that band, so a full-scale sine at a band centre on both channels reads 0 dB and
/// on one channel -3 dB. Bands are base-10 (edges `fc·10^±0.05`), measured with a
/// Hann-windowed STFT of 50 % overlap and bins at most 0.75 Hz apart, which resolves
/// the 20 Hz band. Bands above Nyquist and silent bands read [`FLOOR_DB`].
pub fn third_octave(x: Pair, sr: u32) -> Vec<f64> {
    let (l, r) = (band_power(x.0, sr), band_power(x.1, sr));
    l.iter().zip(&r).map(|(a, b)| db(a + b)).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Masking {
    /// 0 (bass never covers the kick) to 1. Definition: 10 ms frames, third-octave bands
    /// from 25 to 250 Hz (3rd-order Butterworth band-passes, so 25 Hz resolves at 10 ms
    /// steps), K = kick energy and B = bass energy per frame and band:
    /// `index = Σ K·B/(K+B) / Σ K`. Equal energies give 0.5 where the kick plays,
    /// a silent kick gives 0.
    pub index: f64,
    /// Per band (centre Hz, index of that band), 25–250 Hz.
    pub bands: Vec<(f64, f64)>,
    /// Kick punch, dB: kick stem energy against bass stem energy (40–160 Hz) in the
    /// 30 ms after each kick onset, summed over all onsets. Rises when the bass makes
    /// room for the kick; independent of master gain. Capped at [`CAP`] (bass silent at
    /// every onset); 0 without onsets.
    pub kick_punch_db: f64,
    /// Kick hits: 10 ms frames of the kick stem whose energy jumps more than 6 dB over
    /// the two frames before, at most 30 dB below the loudest frame, 50 ms apart or more.
    pub kick_onsets: usize,
}

/// Masking of the kick by the bass, from the two stems.
pub fn masking(kick: Pair, bass: Pair, sr: u32) -> Masking {
    let frames = |e: Vec<f64>, len: usize| -> Vec<f64> {
        e.chunks(len.max(1)).map(|c| c.iter().sum()).collect()
    };
    let (kl, kr, fs) = decimate(kick, sr);
    let (bl, br, _) = decimate(bass, sr);
    let frame = (fs * 0.01).round() as usize;
    let (mut num, mut den) = (0.0, 0.0);
    let bands = (1..=11)
        .map(|b| {
            let (f1, f2) = band_edges(b);
            let k = frames(band_energy((&kl, &kr), fs, f1, f2, 3), frame);
            let bs = frames(band_energy((&bl, &br), fs, f1, f2, 3), frame);
            let (n, d) = k.iter().zip(&bs).fold((0.0, 0.0), |(n, d), (&k, &b)| {
                (if k + b > 0.0 { n + k * b / (k + b) } else { n }, d + k)
            });
            num += n;
            den += d;
            (THIRD_OCTAVE_HZ[b], share(n, d))
        })
        .collect();

    // Onsets on the full-rate kick, where 10 ms frames still see the attack.
    let full_frame = (sr as f64 * 0.01).round() as usize;
    let onsets = onsets(&frames(
        kick.0
            .iter()
            .zip(kick.1)
            .map(|(&l, &r)| (l as f64).powi(2) + (r as f64).powi(2))
            .collect(),
        full_frame,
    ));
    let k = band_energy((&kl, &kr), fs, 40.0, 160.0, 2);
    let b = band_energy((&bl, &br), fs, 40.0, 160.0, 2);
    let len = k.len().min(b.len());
    let after = (fs * 0.03).round() as usize;
    let (mut kick_sum, mut bass_sum) = (0.0, 0.0);
    for s in onsets
        .iter()
        .map(|&o| ((o * full_frame) as f64 * fs / sr as f64) as usize)
        .filter(|&s| s < len)
    {
        let w = s..(s + after).min(len);
        kick_sum += k[w.clone()].iter().sum::<f64>();
        bass_sum += b[w].iter().sum::<f64>();
    }
    Masking {
        index: share(num, den),
        bands,
        kick_punch_db: ratio_db(kick_sum, bass_sum),
        kick_onsets: onsets.len(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StereoImage {
    /// Correlation of L and R as a phase correlation meter shows it,
    /// `Σ L·R / √(Σ L² · Σ R²)`, -1 to 1. Pearson without mean removal, so DC input
    /// stays defined. 1 for silence, 0 when one channel is silent.
    pub correlation: f64,
    /// Side energy / mid energy (M = (L+R)/2, S = (L-R)/2): 0 for mono, 1 for one-sided
    /// or uncorrelated channels, capped at [`CAP`] for pure side signals.
    pub width: f64,
    /// Integrated loudness lost when folded to mono ((L+R)/2 on both channels), LU.
    /// [`CAP`] when the fold cancels to silence, 0 when the input itself is silent or
    /// shorter than 400 ms.
    pub mono_loss_lu: f64,
    /// Per third-octave band: 10·log10((E_L + E_R) / (2·E_mono)), dB, 0 for silent
    /// bands, [`CAP`] where the fold cancels the band.
    pub mono_loss_bands: Vec<f64>,
}

pub fn stereo(x: Pair, sr: u32) -> StereoImage {
    let (correlation, width) = correlation_width(x);
    let mono: Vec<f32> = x.0.iter().zip(x.1).map(|(l, r)| (l + r) / 2.0).collect();
    let (l, r, m) = (
        band_power(x.0, sr),
        band_power(x.1, sr),
        band_power(&mono, sr),
    );
    StereoImage {
        correlation,
        width,
        mono_loss_lu: diff(integrated(x, sr), integrated((&mono, &mono), sr)),
        mono_loss_bands: (0..31).map(|b| ratio_db(l[b] + r[b], 2.0 * m[b])).collect(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceComparison {
    /// Mix minus reference, LU.
    pub lufs_diff: f64,
    /// Per third-octave band after matching both to the same integrated loudness:
    /// mix minus reference, dB. Unmatched when either is silent; 0 for bands silent in
    /// both.
    pub band_diff_db: Vec<f64>,
    /// Crest factor (true peak minus integrated loudness) mix minus reference, dB.
    pub crest_diff_db: f64,
    /// Width mix minus reference.
    pub width_diff: f64,
}

pub fn compare_reference(mix: Pair, reference: Pair, sr: u32) -> ReferenceComparison {
    let (lm, lr) = (loudness(mix, sr), loudness(reference, sr));
    let lufs_diff = diff(lm.integrated_lufs, lr.integrated_lufs);
    let gain = if lm.integrated_lufs.is_finite() && lr.integrated_lufs.is_finite() {
        lufs_diff
    } else {
        0.0
    };
    let crest = |l: &Loudness| diff(l.true_peak_dbtp, l.integrated_lufs);
    ReferenceComparison {
        lufs_diff,
        band_diff_db: third_octave(mix, sr)
            .into_iter()
            .zip(third_octave(reference, sr))
            .map(|(m, r)| {
                if m == FLOOR_DB && r == FLOOR_DB {
                    0.0
                } else {
                    (m - r - gain).clamp(-CAP, CAP)
                }
            })
            .collect(),
        crest_diff_db: diff(crest(&lm), crest(&lr)),
        width_diff: correlation_width(mix).1 - correlation_width(reference).1,
    }
}

/// Gain in dB to apply to B so its integrated loudness matches A.
pub fn match_gain_db(a: &Loudness, b: &Loudness) -> f64 {
    diff(a.integrated_lufs, b.integrated_lufs)
}

fn db(power: f64) -> f64 {
    (10.0 * power.log10()).max(FLOOR_DB)
}

/// `a - b` for levels that may be `-inf`: equal levels give 0, the rest is capped.
fn diff(a: f64, b: f64) -> f64 {
    if a == b {
        0.0
    } else {
        (a - b).clamp(-CAP, CAP)
    }
}

fn ratio_db(num: f64, den: f64) -> f64 {
    if num == 0.0 {
        0.0
    } else {
        (10.0 * (num / den).log10()).clamp(-CAP, CAP)
    }
}

fn share(num: f64, den: f64) -> f64 {
    if den > 0.0 { num / den } else { 0.0 }
}

fn integrated(x: Pair, sr: u32) -> f64 {
    let mut m = EbuR128::new(2, sr, Mode::I).expect("valid sample rate");
    m.add_frames_planar_f32(&[x.0, x.1]).expect("two channels");
    m.loudness_global().unwrap()
}

fn correlation_width((l, r): Pair) -> (f64, f64) {
    let (mut ll, mut rr, mut lr) = (0.0, 0.0, 0.0);
    for (&a, &b) in l.iter().zip(r) {
        let (a, b) = (a as f64, b as f64);
        ll += a * a;
        rr += b * b;
        lr += a * b;
    }
    let correlation = if ll == 0.0 && rr == 0.0 {
        1.0
    } else if ll == 0.0 || rr == 0.0 {
        0.0
    } else {
        (lr / (ll * rr).sqrt()).clamp(-1.0, 1.0)
    };
    // Mid and side energies times 4; rounding can push either just below 0.
    let (mid, side) = ((ll + rr + 2.0 * lr).max(0.0), ll + rr - 2.0 * lr);
    let width = if side > 0.0 {
        (side / mid).min(CAP)
    } else {
        0.0
    };
    (correlation, width)
}

/// Exact base-10 edges of band `i` of [`THIRD_OCTAVE_HZ`].
fn band_edges(i: usize) -> (f64, f64) {
    let fm = 1000.0 * 10f64.powf((i as f64 - 17.0) / 10.0);
    (fm * 10f64.powf(-0.05), fm * 10f64.powf(0.05))
}

/// Mean square of `x` per third-octave band (Welch: Hann, 50 % overlap).
fn band_power(x: &[f32], sr: u32) -> [f64; 31] {
    let n = ((sr as f64 / 0.75).ceil() as usize).next_power_of_two();
    let win = n.min(x.len());
    if win == 0 {
        return [0.0; 31];
    }
    // Offset by half a sample so a one-sample window is not all zero.
    let w: Vec<f64> = (0..win)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * (i as f64 + 0.5) / win as f64).cos())
        .collect();
    let w_energy: f64 = w.iter().map(|v| v * v).sum();
    let mut starts: Vec<usize> = (0..=x.len() - win).step_by((win / 2).max(1)).collect();
    if starts.last() != Some(&(x.len() - win)) {
        starts.push(x.len() - win);
    }
    let bin = |f: f64| ((f * n as f64 / sr as f64).ceil() as usize).min(n / 2 + 1);
    let bins: Vec<(usize, usize)> = (0..31)
        .map(|b| {
            let (f1, f2) = band_edges(b);
            (bin(f1), bin(f2))
        })
        .collect();

    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(n);
    let (mut buf, mut spec) = (fft.make_input_vec(), fft.make_output_vec());
    let mut out = [0.0; 31];
    for &s in &starts {
        for (i, v) in buf.iter_mut().enumerate() {
            *v = if i < win { x[s + i] as f64 * w[i] } else { 0.0 };
        }
        fft.process(&mut buf, &mut spec).unwrap();
        for (o, &(k1, k2)) in out.iter_mut().zip(&bins) {
            // One-sided spectrum: every bin but Nyquist stands for two.
            *o += (k1..k2)
                .map(|k| spec[k].norm_sqr() * if k == n / 2 { 1.0 } else { 2.0 })
                .sum::<f64>();
        }
    }
    let scale = 1.0 / (n as f64 * w_energy * starts.len() as f64);
    out.map(|v| v * scale)
}

/// Biquad in transposed direct form II, `b` numerator, `a` denominator without `a0 = 1`.
#[derive(Clone)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    s: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Biquad { b, a, s: [0.0; 2] }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.s[0];
        self.s[0] = self.b[1] * x - self.a[0] * y + self.s[1];
        self.s[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

fn filter(cascade: &[Biquad], x: &[f32]) -> Vec<f64> {
    let mut f = cascade.to_vec();
    x.iter()
        .map(|&v| f.iter_mut().fold(v as f64, |y, q| q.run(y)))
        .collect()
}

/// 4th-order Butterworth low-pass: two cookbook biquads with the Butterworth Qs.
fn lowpass(fc: f64, fs: f64) -> Vec<Biquad> {
    let w = 2.0 * PI * fc / fs;
    [0.5412, 1.3066]
        .map(|q| {
            let (alpha, c) = (w.sin() / (2.0 * q), w.cos());
            let a0 = 1.0 + alpha;
            let b = (1.0 - c) / 2.0 / a0;
            Biquad::new([b, 2.0 * b, b], [-2.0 * c / a0, (1.0 - alpha) / a0])
        })
        .to_vec()
}

/// `x` low-passed at 500 Hz and decimated to about 3 kHz, plus the new rate. The
/// bands up to 250 Hz need no more, and their filters then run on 1/16 of the samples.
fn decimate(x: Pair, sr: u32) -> (Vec<f32>, Vec<f32>, f64) {
    let d = (sr as usize / 3000).max(1);
    let lp = lowpass(500.0, sr as f64);
    let run = |ch: &[f32]| -> Vec<f32> {
        filter(&lp, ch)
            .into_iter()
            .step_by(d)
            .map(|v| v as f32)
            .collect()
    };
    (run(x.0), run(x.1), sr as f64 / d as f64)
}

/// Butterworth band-pass from `f1` to `f2` Hz, `order` poles per side, as a biquad
/// cascade with unity gain at the centre: analog low-pass prototype, low-pass to
/// band-pass transform, bilinear transform with prewarped edges.
fn bandpass(f1: f64, f2: f64, order: usize, fs: f64) -> Vec<Biquad> {
    let warp = |f: f64| 2.0 * fs * (PI * f / fs).tan();
    let (w1, w2) = (warp(f1), warp(f2));
    let (w0, bw) = ((w1 * w2).sqrt(), w2 - w1);
    let c = Complex64::from_polar(1.0, -2.0 * (w0 / (2.0 * fs)).atan()); // z⁻¹ at the centre
    (0..order)
        .map(|k| {
            let p =
                Complex64::from_polar(1.0, PI * (2 * k + order + 1) as f64 / (2 * order) as f64);
            // Each prototype pole maps to two band-pass poles whose product is w0²,
            // so exactly one lies in the upper half plane; its conjugate comes from
            // the conjugate prototype pole.
            let d = (p * p * bw * bw - 4.0 * w0 * w0).sqrt();
            let s = [(p * bw + d) / 2.0, (p * bw - d) / 2.0]
                .into_iter()
                .find(|s| s.im > 0.0)
                .unwrap();
            let z = (2.0 * fs + s) / (2.0 * fs - s);
            let a = [-2.0 * z.re, z.norm_sqr()];
            // Zeros at z = 1 and z = -1: numerator 1 - z⁻², scaled to unity at the centre.
            let g = 1.0 / ((1.0 - c * c) / (1.0 + a[0] * c + a[1] * c * c)).norm();
            Biquad::new([g, 0.0, -g], a)
        })
        .collect()
}

/// Per-sample energy `L² + R²` of `x` band-passed from `f1` to `f2` Hz.
fn band_energy(x: Pair, fs: f64, f1: f64, f2: f64, order: usize) -> Vec<f64> {
    let bp = bandpass(f1, f2, order, fs);
    let (l, r) = (filter(&bp, x.0), filter(&bp, x.1));
    l.iter().zip(&r).map(|(a, b)| a * a + b * b).collect()
}

/// Frames whose energy jumps more than 6 dB over the two frames before, at most 30 dB
/// below the loudest frame, at least five frames apart.
fn onsets(e: &[f64]) -> Vec<usize> {
    let threshold = e.iter().fold(0.0, |a: f64, &b| a.max(b)) * 1e-3;
    let mut out: Vec<usize> = vec![];
    for i in 0..e.len() {
        let before = e[i.saturating_sub(2)..i]
            .iter()
            .fold(0.0, |a: f64, &b| a.max(b));
        if e[i] > threshold && e[i] > 4.0 * before && out.last().is_none_or(|&o| i - o >= 5) {
            out.push(i);
        }
    }
    out
}

#[cfg(test)]
mod tests;
