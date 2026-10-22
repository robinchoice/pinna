//! Measurements an agent uses to hear its changes. Pure functions on sample slices,
//! independent of the rest of Pinna, so other tools can use them too.
//!
//! All inputs are stereo pairs `(l, r)` of equal length at `sr` Hz. Levels are dB,
//! loudness follows ITU-R BS.1770 / EBU R128.

/// Centre frequencies of the 31 third-octave bands from 20 Hz to 20 kHz (IEC 61260).
pub const THIRD_OCTAVE_HZ: [f64; 31] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
    500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0,
    8000.0, 10_000.0, 12_500.0, 16_000.0, 20_000.0,
];

pub type Pair<'a> = (&'a [f32], &'a [f32]);

#[derive(Clone, Debug, PartialEq)]
pub struct Loudness {
    /// Integrated loudness, LUFS. `-inf` for silence.
    pub integrated_lufs: f64,
    /// Highest short-term (3 s) loudness, LUFS.
    pub short_term_max_lufs: f64,
    /// Loudness range, LU.
    pub lra: f64,
    /// True peak (4x oversampled), dBTP.
    pub true_peak_dbtp: f64,
    pub sample_peak_dbfs: f64,
}

pub fn loudness(x: Pair, sr: u32) -> Loudness {
    let _ = (x, sr);
    todo!()
}

/// Long-term level per third-octave band (energy of L+R, Hann-windowed STFT), dB,
/// one value per entry of [`THIRD_OCTAVE_HZ`].
pub fn third_octave(x: Pair, sr: u32) -> Vec<f64> {
    let _ = (x, sr);
    todo!()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Masking {
    /// 0 (bass never covers the kick) to 1. Definition: STFT frames, third-octave bands
    /// from 25 to 250 Hz, K = kick energy and B = bass energy per frame and band:
    /// `index = Σ K·B/(K+B) / Σ K`. Equal energies give 0.5 where the kick plays.
    pub index: f64,
    /// Per band (centre Hz, index of that band), 25–250 Hz.
    pub bands: Vec<(f64, f64)>,
    /// Kick punch in the mix, dB: mean over kick onsets of the mix level (40–160 Hz)
    /// in the 30 ms after the onset minus the level in the 60–10 ms before it.
    pub kick_punch_db: f64,
    pub kick_onsets: usize,
}

/// Masking of the kick by the bass, from the two stems and the mix.
pub fn masking(kick: Pair, bass: Pair, mix: Pair, sr: u32) -> Masking {
    let _ = (kick, bass, mix, sr);
    todo!()
}

#[derive(Clone, Debug, PartialEq)]
pub struct StereoImage {
    /// Pearson correlation of L and R, -1 to 1.
    pub correlation: f64,
    /// Side energy / mid energy, 0 for mono.
    pub width: f64,
    /// Integrated loudness lost when folded to mono ((L+R)/2 on both channels), LU.
    pub mono_loss_lu: f64,
    /// Per third-octave band: 10·log10((E_L + E_R) / (2·E_mono)), dB.
    pub mono_loss_bands: Vec<f64>,
}

pub fn stereo(x: Pair, sr: u32) -> StereoImage {
    let _ = (x, sr);
    todo!()
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceComparison {
    /// Mix minus reference, LU.
    pub lufs_diff: f64,
    /// Per third-octave band after matching both to the same integrated loudness:
    /// mix minus reference, dB.
    pub band_diff_db: Vec<f64>,
    /// Crest factor (true peak minus integrated loudness) mix minus reference, dB.
    pub crest_diff_db: f64,
    /// Width mix minus reference.
    pub width_diff: f64,
}

pub fn compare_reference(mix: Pair, reference: Pair, sr: u32) -> ReferenceComparison {
    let _ = (mix, reference, sr);
    todo!()
}

/// Gain in dB to apply to B so its integrated loudness matches A.
pub fn match_gain_db(a: &Loudness, b: &Loudness) -> f64 {
    a.integrated_lufs - b.integrated_lufs
}
