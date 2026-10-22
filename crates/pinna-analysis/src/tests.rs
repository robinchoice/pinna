use super::*;

const SR: u32 = 48_000;

fn secs(s: f64) -> usize {
    (s * SR as f64) as usize
}

fn sine(hz: f64, amp: f64, s: f64) -> Vec<f32> {
    (0..secs(s))
        .map(|i| (amp * (2.0 * PI * hz * i as f64 / SR as f64).sin()) as f32)
        .collect()
}

/// White noise in [-amp, amp] from a fixed seed (xorshift).
fn noise(seed: u64, amp: f64, s: f64) -> Vec<f32> {
    let mut x = seed;
    (0..secs(s))
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (amp * ((x >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0)) as f32
        })
        .collect()
}

/// 50 Hz kick: decaying sine starting at zero phase on each onset.
fn kick(onsets: &[f64], s: f64) -> Vec<f32> {
    (0..secs(s))
        .map(|i| {
            let t = i as f64 / SR as f64;
            onsets
                .iter()
                .filter(|&&o| t >= o)
                .map(|&o| 0.8 * (2.0 * PI * 50.0 * (t - o)).sin() * (-(t - o) / 0.12).exp())
                .sum::<f64>() as f32
        })
        .collect()
}

/// Gain that drops to 0.1 at each onset and recovers with a 100 ms time constant.
fn duck(x: &[f32], onsets: &[f64]) -> Vec<f32> {
    x.iter()
        .enumerate()
        .map(|(i, &v)| {
            let t = i as f64 / SR as f64;
            let g = onsets
                .iter()
                .filter(|&&o| t >= o)
                .map(|&o| 1.0 - 0.9 * (-(t - o) / 0.1).exp())
                .fold(1.0, f64::min);
            v * g as f32
        })
        .collect()
}

fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

/// 4th-order Butterworth high-pass (two RBJ biquads, Q 0.541 and 1.307).
fn highpass(x: &[f32], hz: f64) -> Vec<f32> {
    let mut y: Vec<f64> = x.iter().map(|&v| v as f64).collect();
    for q in [0.5412, 1.3066] {
        let w = 2.0 * PI * hz / SR as f64;
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = [
            (1.0 + w.cos()) / 2.0,
            -(1.0 + w.cos()),
            (1.0 + w.cos()) / 2.0,
        ]
        .map(|v| v / a0);
        let a = [-2.0 * w.cos() / a0, (1.0 - alpha) / a0];
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for v in y.iter_mut() {
            let out = b[0] * *v + b[1] * x1 + b[2] * x2 - a[0] * y1 - a[1] * y2;
            (x2, x1, y2, y1) = (x1, *v, y1, out);
            *v = out;
        }
    }
    y.into_iter().map(|v| v as f32).collect()
}

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn loudness_of_sine_and_silence() {
    let s = sine(1000.0, 0.1, 5.0);
    let l = loudness((&s, &s), SR);
    assert!(near(l.integrated_lufs, -20.0, 0.1), "{l:?}");
    assert!(near(l.short_term_max_lufs, -20.0, 0.1), "{l:?}");
    assert!(near(l.sample_peak_dbfs, -20.0, 0.01), "{l:?}");
    assert!(near(l.true_peak_dbtp, -20.0, 0.1), "{l:?}");
    assert!(l.lra < 0.5, "{l:?}");

    let z = vec![0.0; secs(5.0)];
    let l = loudness((&z, &z), SR);
    assert_eq!(l.integrated_lufs, f64::NEG_INFINITY);
    assert_eq!(l.sample_peak_dbfs, f64::NEG_INFINITY);
    assert_eq!(l.lra, 0.0);
}

#[test]
fn third_octave_resolves_a_sine_at_each_band_centre() {
    for (hz, band) in [(31.5, 2), (50.0, 4), (1000.0, 17), (10_000.0, 27)] {
        let s = sine(hz, 0.5, 4.0);
        let levels = third_octave((&s, &s), SR);
        let want = 20.0 * 0.5f64.log10();
        assert!(near(levels[band], want, 0.5), "{hz} Hz: {levels:?}");
        assert!(levels[band - 1] < want - 10.0, "{hz} Hz: {levels:?}");
        assert!(levels[band + 1] < want - 10.0, "{hz} Hz: {levels:?}");
    }
    // One channel only: 3 dB less.
    let s = sine(1000.0, 1.0, 2.0);
    let z = vec![0.0; s.len()];
    assert!(near(third_octave((&s, &z), SR)[17], -3.01, 0.1));
}

#[test]
fn filter_bank_resolves_a_sine_at_low_band_centres() {
    for (hz, band) in [(25.0, 1), (31.5, 2), (50.0, 4), (250.0, 11)] {
        let s = sine(hz, 1.0, 4.0);
        let (l, r, fs) = decimate((&s, &s), SR);
        let settled = (fs * 1.0) as usize;
        let level = |b: usize| {
            let (f1, f2) = band_edges(b);
            let e = band_energy((&l, &r), fs, f1, f2, 3);
            db(e[settled..].iter().sum::<f64>() / (e.len() - settled) as f64)
        };
        // Full-scale sine on both channels: mean of L² + R² is 1, 0 dB.
        let want = level(band);
        assert!(near(want, 0.0, 0.5), "{hz} Hz: {want}");
        assert!(level(band - 1) < want - 10.0, "{hz} Hz");
        assert!(level(band + 1) < want - 10.0, "{hz} Hz");
    }
}

#[test]
fn decimation_rejects_what_would_alias_into_the_low_bands() {
    // 2.95 kHz would fold to 50 Hz at the 3 kHz rate.
    let s = sine(2950.0, 1.0, 2.0);
    let (l, r, fs) = decimate((&s, &s), SR);
    assert_eq!(fs, 3000.0);
    let (f1, f2) = band_edges(4);
    let e = band_energy((&l, &r), fs, f1, f2, 3);
    assert!(db(e.iter().sum::<f64>() / e.len() as f64) < -50.0);
}

#[test]
fn stereo_mono_inverted_and_uncorrelated() {
    let n = noise(1, 0.5, 5.0);
    let st = stereo((&n, &n), SR);
    assert!(near(st.correlation, 1.0, 1e-9), "{st:?}");
    assert_eq!(st.width, 0.0);
    assert_eq!(st.mono_loss_lu, 0.0);
    assert!(
        st.mono_loss_bands.iter().all(|&b| near(b, 0.0, 1e-6)),
        "{st:?}"
    );

    let inv: Vec<f32> = n.iter().map(|v| -v).collect();
    let st = stereo((&n, &inv), SR);
    assert!(near(st.correlation, -1.0, 1e-9), "{st:?}");
    assert_eq!(st.width, CAP);
    assert_eq!(st.mono_loss_lu, CAP);
    assert!(st.mono_loss_bands.iter().all(|&b| b == CAP), "{st:?}");

    let m = noise(2, 0.5, 5.0);
    let st = stereo((&n, &m), SR);
    assert!(near(st.correlation, 0.0, 0.02), "{st:?}");
    assert!(near(st.width, 1.0, 0.05), "{st:?}");
    assert!(near(st.mono_loss_lu, 3.01, 0.2), "{st:?}");
    // Narrow low bands hold few independent noise samples, so only check from 80 Hz.
    assert!(
        st.mono_loss_bands[6..].iter().all(|&b| near(b, 3.01, 0.5)),
        "{st:?}"
    );

    let z = vec![0.0; n.len()];
    let st = stereo((&n, &z), SR);
    assert_eq!(st.correlation, 0.0);
    assert!(near(st.width, 1.0, 1e-9), "{st:?}");
    assert!(near(st.mono_loss_lu, 3.01, 0.2), "{st:?}");
}

#[test]
fn reference_comparison() {
    let (l, r) = (noise(3, 0.3, 6.0), noise(4, 0.3, 6.0));
    let c = compare_reference((&l, &r), (&l, &r), SR);
    assert_eq!(c.lufs_diff, 0.0);
    assert_eq!(c.crest_diff_db, 0.0);
    assert_eq!(c.width_diff, 0.0);
    assert!(c.band_diff_db.iter().all(|&b| b == 0.0), "{c:?}");

    let half = |x: &[f32]| x.iter().map(|v| v * 0.5).collect::<Vec<f32>>();
    let (ql, qr) = (half(&l), half(&r));
    let c = compare_reference((&l, &r), (&ql, &qr), SR);
    assert!(near(c.lufs_diff, 6.02, 0.1), "{c:?}");
    assert!(near(c.crest_diff_db, 0.0, 0.1), "{c:?}");
    assert!(near(c.width_diff, 0.0, 1e-9), "{c:?}");
    assert!(c.band_diff_db.iter().all(|&b| near(b, 0.0, 0.1)), "{c:?}");

    // A brighter mix shows up in the high bands.
    let bright = add(&l, &highpass(&l, 4000.0));
    let c = compare_reference((&bright, &r), (&l, &r), SR);
    assert!(c.band_diff_db[29] > c.band_diff_db[10] + 3.0, "{c:?}");
}

const HITS: [f64; 4] = [0.5, 1.0, 1.5, 2.0];

fn bass() -> Vec<f32> {
    let parts = [
        sine(50.0, 0.3, 3.0),
        sine(100.0, 0.15, 3.0),
        sine(150.0, 0.1, 3.0),
    ];
    add(&add(&parts[0], &parts[1]), &parts[2])
}

#[test]
fn masking_index() {
    let k = kick(&HITS, 3.0);
    let same = masking((&k, &k), (&k, &k), SR);
    assert!(near(same.index, 0.5, 1e-9), "{same:?}");
    assert!(
        same.bands
            .iter()
            .all(|&(_, i)| i == 0.0 || near(i, 0.5, 1e-9))
    );

    let z = vec![0.0; k.len()];
    let silent = masking((&k, &k), (&z, &z), SR);
    assert_eq!(silent.index, 0.0);

    let b = bass();
    let full = masking((&k, &k), (&b, &b), SR);
    let hp = highpass(&b, 60.0);
    let cut = masking((&k, &k), (&hp, &hp), SR);
    assert!(
        cut.index < 0.6 * full.index,
        "{} vs {}",
        cut.index,
        full.index
    );
    let ducked = duck(&b, &HITS);
    let ducked = masking((&k, &k), (&ducked, &ducked), SR);
    assert!(
        ducked.index < 0.6 * full.index,
        "{} vs {}",
        ducked.index,
        full.index
    );
}

#[test]
fn kick_onsets_and_punch() {
    let k = kick(&HITS, 3.0);
    let z = vec![0.0; k.len()];
    let alone = masking((&k, &k), (&z, &z), SR);
    assert_eq!(alone.kick_onsets, 4);
    assert_eq!(alone.kick_punch_db, CAP);

    let b = sine(50.0, 0.5, 3.0);
    let steady = masking((&k, &k), (&b, &b), SR);
    assert_eq!(steady.kick_onsets, 4);
    assert!(steady.kick_punch_db < 20.0, "{steady:?}");

    let bd = duck(&b, &HITS);
    let ducked = masking((&k, &k), (&bd, &bd), SR);
    assert!(
        ducked.kick_punch_db > steady.kick_punch_db + 2.0,
        "{} vs {}",
        ducked.kick_punch_db,
        steady.kick_punch_db
    );
    // One onset without bass must not dominate: energies are summed, not capped dBs.
    let mut gap = b.clone();
    gap[secs(0.95)..secs(1.1)].fill(0.0);
    let one_gap = masking((&k, &k), (&gap, &gap), SR);
    assert!(
        one_gap.kick_punch_db < steady.kick_punch_db + 3.0,
        "{one_gap:?}"
    );
}

#[test]
fn onset_detector_ignores_steady_tones_and_silence() {
    let s = sine(50.0, 0.5, 2.0);
    assert_eq!(masking((&s, &s), (&s, &s), SR).kick_onsets, 1);
    let z = vec![0.0; secs(2.0)];
    let m = masking((&z, &z), (&z, &z), SR);
    assert_eq!(m.kick_onsets, 0);
    assert_eq!(m.kick_punch_db, 0.0);
    // Kicks 30 ms apart merge into one hit.
    assert_eq!(onsets(&[0.0, 1.0, 0.5, 5.0, 0.5, 0.2, 0.1]), vec![1]);
}

fn all_values(x: Pair) -> Vec<f64> {
    let l = loudness(x, SR);
    let m = masking(x, x, SR);
    let s = stereo(x, SR);
    let c = compare_reference(x, x, SR);
    let c2 = compare_reference(x, (&[0.0; 100], &[0.0; 100]), SR);
    let mut v = vec![
        l.integrated_lufs,
        l.short_term_max_lufs,
        l.lra,
        l.true_peak_dbtp,
        l.sample_peak_dbfs,
        m.index,
        m.kick_punch_db,
        s.correlation,
        s.width,
        s.mono_loss_lu,
    ];
    v.extend(third_octave(x, SR));
    v.extend(m.bands.iter().map(|b| b.1));
    v.extend(s.mono_loss_bands);
    for c in [c, c2] {
        v.extend([c.lufs_diff, c.crest_diff_db, c.width_diff]);
        v.extend(c.band_diff_db);
    }
    v
}

#[test]
fn no_nan_for_silent_short_or_dc_input() {
    let inputs = [
        vec![],
        vec![0.5],
        vec![0.0; secs(2.0)],
        vec![0.5; secs(2.0)],
        vec![0.5; secs(0.1)],
        sine(1000.0, 0.5, 0.2),
        noise(5, 1.0, 0.3),
    ];
    for x in &inputs {
        let v = all_values((x, x));
        assert!(v.iter().all(|v| !v.is_nan()), "len {}: {v:?}", x.len());
    }
}

#[test]
fn sixty_seconds_analyse_in_seconds() {
    let (l, r) = (noise(6, 0.3, 60.0), noise(7, 0.3, 60.0));
    let x = (&l[..], &r[..]);
    let start = std::time::Instant::now();
    let lap = |name: &str, t: &mut std::time::Instant| {
        eprintln!("{name}: {:?}", t.elapsed());
        *t = std::time::Instant::now();
    };
    let mut t = start;
    loudness(x, SR);
    lap("loudness", &mut t);
    third_octave(x, SR);
    lap("third_octave", &mut t);
    masking(x, x, SR);
    lap("masking", &mut t);
    stereo(x, SR);
    lap("stereo", &mut t);
    compare_reference(x, x, SR);
    lap("compare_reference", &mut t);
    let took = start.elapsed();
    assert!(took.as_secs() < 30, "{took:?}");
}

#[test]
fn zz_profile_loudness() {
    let (l, r) = (noise(6, 0.3, 60.0), noise(7, 0.3, 60.0));
    for (name, mode, poll) in [
        ("I", Mode::I, false),
        ("I|S|LRA", Mode::I | Mode::S | Mode::LRA, false),
        ("I|SP", Mode::I | Mode::SAMPLE_PEAK, false),
        ("I|TP", Mode::I | Mode::TRUE_PEAK, false),
        ("I|S poll", Mode::I | Mode::S, true),
    ] {
        let t = std::time::Instant::now();
        let mut m = EbuR128::new(2, SR, mode).unwrap();
        if poll {
            for (a, b) in l.chunks(4800).zip(r.chunks(4800)) {
                m.add_frames_planar_f32(&[a, b]).unwrap();
                m.loudness_shortterm().unwrap();
            }
        } else {
            m.add_frames_planar_f32(&[&l, &r]).unwrap();
        }
        eprintln!("{name}: {:?}", t.elapsed());
    }
}
