//! Render invariants. Agents cannot listen, so every property here is checked on
//! numbers: exact zeros, sample positions, levels and smoothness.

use std::path::{Path, PathBuf};
use std::time::Instant;

use pinna_core::{
    Clip, Device, FORMAT_VERSION, Kind, Lane, Master, PPQ, Pos, Project, Timing, Track,
};

use crate::audio_io::{read_audio, write_wav};
use crate::strip::{NoteEvent, note_events, set_stepped_automation};
use crate::{EngineError, Render, RenderOptions, Stereo, render};

fn song(tracks: Vec<Track>) -> Project {
    Project {
        version: FORMAT_VERSION,
        name: "test".into(),
        tempo: 120.0,
        beats_per_bar: 4,
        sections: Vec::new(),
        tracks,
        master: Master::default(),
    }
}

fn track(id: &str, instrument: Option<Device>, clips: Vec<Clip>) -> Track {
    Track {
        id: id.into(),
        instrument,
        gain_db: 0.0,
        pan: 0.0,
        mute: false,
        clips,
        fx: Vec::new(),
        automation: Vec::new(),
    }
}

fn notes(at: &str, len: &str, loop_len: Option<&str>, notes: &[&str]) -> Clip {
    Clip::Notes {
        id: "c".into(),
        at: at.parse().unwrap(),
        len: len.parse().unwrap(),
        loop_len: loop_len.map(|l| l.parse().unwrap()),
        notes: notes.iter().map(|n| n.parse().unwrap()).collect(),
    }
}

fn audio(at: &str, len: &str, file: &str, offset: &str) -> Clip {
    Clip::Audio {
        id: "a".into(),
        at: at.parse().unwrap(),
        len: len.parse().unwrap(),
        file: file.into(),
        offset: offset.parse().unwrap(),
        gain_db: 0.0,
    }
}

fn lane(target: &str, points: &[(&str, f64)]) -> Lane {
    Lane {
        target: target.into(),
        points: points
            .iter()
            .map(|(p, v)| (p.parse().unwrap(), *v))
            .collect(),
    }
}

fn bar(project: &Project, n: u32) -> u64 {
    project.timing().tick(Pos::new(n, 1))
}

fn opts(sample_rate: u32, block_size: usize, stems: &[&str]) -> RenderOptions {
    RenderOptions {
        sample_rate,
        block_size,
        stems: stems.iter().map(|s| s.to_string()).collect(),
    }
}

fn run(project: &Project, root: &Path, from: u64, to: u64, o: &RenderOptions) -> Render {
    render(project, root, from, to, o).unwrap()
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn dbfs(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

fn example(name: &str) -> (Project, PathBuf) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name);
    (pinna_core::io::load(&root).unwrap(), root)
}

/// The example song with automation on every kind of target.
fn automated_nachtzug() -> (Project, PathBuf) {
    let (mut p, root) = example("nachtzug");
    let bass = p.track_mut("bass").unwrap();
    bass.automation = vec![
        lane("gain_db", &[("9.1", -20.0), ("10.3", 0.0)]),
        lane("instrument.cutoff", &[("9.1", 200.0), ("12.1", 2000.0)]),
        lane("pan", &[("9.1", -0.5), ("11.1", 0.5)]),
    ];
    let pad = p.track_mut("pad").unwrap();
    pad.automation = vec![lane("fx.lp.freq", &[("2.1", 500.0), ("8.1", 6000.0)])];
    p.master.automation = vec![lane("gain_db", &[("1.1", -12.0), ("5.1", -6.0)])];
    assert!(pinna_core::validate::validate(&p).is_empty());
    (p, root)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pinna-engine-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn notes_loop_and_truncate() {
    let t = Timing {
        bpm: 60.0,
        beats_per_bar: 4,
    };
    let tr = track(
        "t",
        Some(Device::new("d", Kind::Pad)),
        vec![notes(
            "2.1",
            "3",
            Some("2"),
            &["C2 0 1", "D2 3/2 1", "E2 1/2 4"],
        )],
    );
    // At 60 BPM and 960 Hz one sample is one tick.
    let events: Vec<(u64, bool, u8)> = note_events(&tr, &t, 960)
        .iter()
        .map(|e: &NoteEvent| (e.sample, e.on, e.pitch))
        .collect();
    let (c2, d2, e2) = (36, 38, 40);
    assert_eq!(
        events,
        vec![
            (3840, true, c2),
            (4320, true, e2),
            (4800, false, c2),
            (5280, true, d2),
            // Loop end cuts the first pass; offs come before ons at the same sample.
            (5760, false, d2),
            (5760, false, e2),
            (5760, true, c2),
            (6240, true, e2),
            // Clip end cuts the second pass, D2 at 3/2 lies beyond it.
            (6720, false, c2),
            (6720, false, e2),
        ]
    );
}

#[test]
fn repeat_render_is_bit_identical() {
    let (p, root) = automated_nachtzug();
    let o = opts(48_000, 256, &["bass", "pad"]);
    let a = run(&p, &root, 0, bar(&p, 13), &o);
    let b = run(&p, &root, 0, bar(&p, 13), &o);
    assert_eq!(a.mix, b.mix);
    assert_eq!(a.stems, b.stems);
}

#[test]
fn block_size_does_not_change_the_result() {
    let (p, root) = automated_nachtzug();
    let to = bar(&p, 13);
    let a = run(&p, &root, 0, to, &opts(48_000, 64, &[]));
    let b = run(&p, &root, 0, to, &opts(48_000, 256, &[]));
    let c = run(&p, &root, 0, to, &opts(48_000, 1000, &[]));
    for other in [&b, &c] {
        let diff = a
            .mix
            .l
            .iter()
            .chain(&a.mix.r)
            .zip(other.mix.l.iter().chain(&other.mix.r))
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max);
        assert!(diff <= 1e-5, "max difference {diff}");
    }
}

#[test]
fn range_is_a_slice_of_the_full_render() {
    let (p, root) = automated_nachtzug();
    let o = opts(44_100, 256, &["bass", "kick"]);
    let full = run(&p, &root, 0, bar(&p, 13), &o);
    let (from, to) = (bar(&p, 10) + 333, bar(&p, 12) + 17);
    let part = run(&p, &root, from, to, &o);
    let t = p.timing();
    let (a, b) = (
        t.sample(from, 44_100) as usize,
        t.sample(to, 44_100) as usize,
    );
    assert_eq!(part.start_sample, a as u64);
    assert_eq!(part.mix.len(), b - a);
    assert_eq!(part.mix.l, full.mix.l[a..b]);
    assert_eq!(part.mix.r, full.mix.r[a..b]);
    for id in ["bass", "kick"] {
        assert_eq!(part.stems[id].l, full.stems[id].l[a..b]);
        assert_eq!(part.stems[id].r, full.stems[id].r[a..b]);
    }
    let empty = run(&p, &root, from, from, &o);
    assert!(empty.mix.is_empty());
}

#[test]
fn stems_sum_to_the_mix_and_pan_follows_the_balance_law() {
    let (mut p, root) = example("nachtzug");
    p.master.gain_db = 0.0;
    p.track_mut("lead").unwrap().pan = -1.0;
    let ids: Vec<String> = p.tracks.iter().map(|t| t.id.clone()).collect();
    let o = RenderOptions {
        stems: ids.clone(),
        ..RenderOptions::default()
    };
    let r = run(&p, &root, bar(&p, 9), bar(&p, 11), &o);
    for k in 0..r.mix.len() {
        let l: f32 = ids.iter().map(|id| r.stems[id].l[k]).sum();
        let rr: f32 = ids.iter().map(|id| r.stems[id].r[k]).sum();
        assert_eq!((l, rr), (r.mix.l[k], r.mix.r[k]), "sample {k}");
    }
    let lead = &r.stems["lead"];
    assert!(lead.r.iter().all(|x| *x == 0.0));
    assert!(rms(&lead.l) > 1e-3);
    // Hats sit at pan 0.2: the left side is turned down to 0.8, the right stays.
    let hats = &r.stems["hats"];
    let ratio = rms(&hats.l) / rms(&hats.r);
    assert!((ratio - 0.8).abs() < 1e-4, "{ratio}");
}

#[test]
fn empty_and_muted_projects_are_exactly_silent() {
    let root = Path::new(".");
    let empty = song(Vec::new());
    let r = run(&empty, root, 0, 4 * PPQ, &RenderOptions::default());
    assert_eq!(r.mix, Stereo::silence(96_000));

    let (mut p, root) = example("nachtzug");
    p.tracks.iter_mut().for_each(|t| t.mute = true);
    let r = run(&p, &root, 0, bar(&p, 11), &opts(48_000, 256, &["kick"]));
    assert!(r.mix.l.iter().chain(&r.mix.r).all(|x| *x == 0.0));
    assert!(r.stems["kick"].l.iter().all(|x| *x == 0.0));
}

#[test]
fn every_instrument_stays_finite_at_param_extremes() {
    let root = Path::new(".");
    let pitches = [0, 24, 36, 38, 42, 46, 60, 96, 127];
    let mut held: Vec<String> = pitches
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{p} {}/4 1/2 {}", i, 1 + i * 15))
        .collect();
    // Overlapping notes exercise legato and voice stealing paths.
    held.push("E1 3 2 127".into());
    held.push("G1 4 2 1".into());
    let held: Vec<&str> = held.iter().map(String::as_str).collect();
    for kind in Kind::ALL.into_iter().filter(|k| k.is_instrument()) {
        let specs = kind.params();
        let devices = [
            Device::new("i", kind),
            specs
                .iter()
                .fold(Device::new("i", kind), |d, s| d.with(s.name, s.min)),
            specs
                .iter()
                .fold(Device::new("i", kind), |d, s| d.with(s.name, s.max)),
        ];
        for (n, dev) in devices.into_iter().enumerate() {
            let p = song(vec![track(
                "t",
                Some(dev.clone()),
                vec![notes("1.1", "8", None, &held)],
            )]);
            let r = run(&p, root, 0, 12 * PPQ, &RenderOptions::default());
            let peak = r
                .mix
                .l
                .iter()
                .inspect(|x| assert!(x.is_finite(), "{dev:?}"))
                .fold(0.0f32, |m, x| m.max(x.abs()));
            assert!(peak < 100.0, "{dev:?} peak {peak}");
            if n == 0 {
                assert!(peak > 0.01, "{kind:?} is silent at defaults");
            }
        }
    }
}

#[test]
fn kick_onset_lands_on_the_timing_sample() {
    let root = Path::new(".");
    let mut p = song(vec![track(
        "kick",
        Some(Device::new("d", Kind::Drums)),
        vec![notes("2.1", "1", None, &["C2 1/3 1/4 120"])],
    )]);
    p.tempo = 124.0;
    let tick = p.timing().tick(Pos::new(2, 1)) + PPQ / 3;
    for sr in [44_100, 48_000] {
        let onset = p.timing().sample(tick, sr) as usize;
        let r = run(&p, root, 0, bar(&p, 3), &opts(sr, 256, &["kick"]));
        let x = &r.stems["kick"].l;
        assert!(x[..onset].iter().all(|v| *v == 0.0), "{sr}: early sound");
        assert!(x[onset] != 0.0, "{sr}: nothing at the onset");
        assert_eq!(r.mix.l[..=onset], x[..=onset]);
    }
}

#[test]
fn sidechain_ducks_the_bass_at_kick_onsets() {
    let root = Path::new(".");
    let kicks = notes("1.1", "16", Some("1"), &["C2 0 1/4 120"]);
    let comp = |sidechain: Option<&str>| {
        let mut bass = track(
            "bass",
            Some(Device::new("saw", Kind::SawBass)),
            vec![notes("1.1", "16", None, &["E1 0 16"])],
        );
        bass.fx = vec![Device {
            sidechain: sidechain.map(Into::into),
            ..Device::new("duck", Kind::Compressor)
                .with("threshold_db", -30.0)
                .with("ratio", 10.0)
                .with("attack_ms", 1.0)
                .with("release_ms", 60.0)
        }];
        let p = song(vec![
            track(
                "kick",
                Some(Device::new("d", Kind::Drums)),
                vec![kicks.clone()],
            ),
            bass,
        ]);
        let r = run(&p, root, 0, 16 * PPQ, &opts(48_000, 256, &["bass"]));
        // Bass level right after each kick against later in the same beat.
        let x = &r.stems["bass"].l;
        let beat = 24_000;
        let ms = 48;
        let (mut after, mut between) = (Vec::new(), Vec::new());
        for b in 2..15 {
            let onset = b * beat;
            after.extend_from_slice(&x[onset + 5 * ms..onset + 50 * ms]);
            between.extend_from_slice(&x[onset + 350 * ms..onset + 450 * ms]);
        }
        dbfs(rms(&after)) - dbfs(rms(&between))
    };
    let ducked = comp(Some("kick"));
    let plain = comp(None);
    assert!(ducked < -10.0, "sidechain: {ducked:.1} dB");
    assert!(plain.abs() < 1.0, "no sidechain: {plain:.1} dB");
}

/// Largest second difference. A smooth signal bends a little every sample; a step
/// such as zipper noise or a click shows up as a spike.
fn max_bend(x: &[f32]) -> f32 {
    x.windows(3)
        .map(|w| (w[0] - 2.0 * w[1] + w[2]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn automated_gain_has_no_clicks_and_the_detector_trips_on_steps() {
    let dir = temp_dir("clicks");
    let (sr, freq, amp) = (48_000u32, 200.0f64, 0.5f64);
    let tone: Vec<f32> = (0..4 * sr)
        // f64 phase: an f32 phase this far into the tone jitters enough to trip the limit.
        .map(|i| (amp * (std::f64::consts::TAU * freq * f64::from(i) / f64::from(sr)).sin()) as f32)
        .collect();
    write_wav(
        &dir.join("tone.wav"),
        &Stereo {
            l: tone.clone(),
            r: tone,
        },
        sr,
    )
    .unwrap();
    let mut tr = track("tone", None, vec![audio("1.1", "8", "tone.wav", "0")]);
    tr.automation = vec![lane(
        "gain_db",
        &[("1.2", 0.0), ("1.4", -40.0), ("2.2", 0.0)],
    )];
    let p = song(vec![tr]);
    // A sine bends at most amp * w^2 per sample; the gain never exceeds unity.
    let w = std::f64::consts::TAU * freq / f64::from(sr);
    let limit = (2.0 * amp * w * w) as f32;
    let bend = |stepped: bool| {
        set_stepped_automation(stepped);
        let r = run(&p, &dir, 0, 8 * PPQ, &opts(sr, 256, &[]));
        set_stepped_automation(false);
        // Skip the clip edges: the clip starts and stops the tone hard.
        max_bend(&r.mix.l[2..r.mix.len() - 2])
    };
    let smooth = bend(false);
    assert!(
        smooth < limit,
        "smoothed automation bends {smooth} > {limit}"
    );
    let stepped = bend(true);
    assert!(
        stepped > 10.0 * limit,
        "detector missed zipper steps: {stepped}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn audio_clip_plays_at_its_position_with_offset() {
    let dir = temp_dir("clip");
    let ramp: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
    let file = Stereo {
        l: ramp.clone(),
        r: ramp.iter().map(|x| -x).collect(),
    };
    write_wav(&dir.join("ramp.wav"), &file, 48_000).unwrap();
    assert_eq!(read_audio(&dir.join("ramp.wav"), 48_000).unwrap(), file);

    let p = song(vec![track(
        "vox",
        None,
        vec![audio("2.1", "1", "ramp.wav", "1/4")],
    )]);
    let r = run(&p, &dir, 0, bar(&p, 3), &RenderOptions::default());
    // Bar 2 at 120 BPM is 2 s; the offset of 1/4 beat skips 6000 samples.
    let (start, len, offset) = (96_000, 24_000, 6_000);
    assert!(r.mix.l[..start].iter().all(|x| *x == 0.0));
    assert_eq!(r.mix.l[start..start + len], ramp[offset..offset + len]);
    assert_eq!(r.mix.r[start], -ramp[offset]);
    assert!(r.mix.l[start + len..].iter().all(|x| *x == 0.0));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn audio_at_another_rate_is_resampled() {
    let dir = temp_dir("resample");
    // Mono 16-bit at 44.1 kHz: silence, then a 1 kHz burst from 0.1 s to 0.3 s.
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(dir.join("burst.wav"), spec).unwrap();
    for i in 0..22_050 {
        let burst = (4_410..13_230).contains(&i);
        let x = if burst {
            0.5 * (std::f64::consts::TAU * 1000.0 * f64::from(i - 4_410) / 44_100.0).sin()
        } else {
            0.0
        };
        w.write_sample((x * 32_767.0).round() as i16).unwrap();
    }
    w.finalize().unwrap();

    let audio48 = read_audio(&dir.join("burst.wav"), 48_000).unwrap();
    assert!(audio48.len().abs_diff(24_000) <= 2, "{}", audio48.len());
    assert_eq!(audio48.l, audio48.r);
    let burst = &audio48.l[5_000..14_000];
    assert!(
        (rms(burst) - 0.5 / 2f64.sqrt()).abs() < 0.005,
        "{}",
        rms(burst)
    );

    let p = song(vec![track(
        "fx",
        None,
        vec![audio("2.1", "1", "burst.wav", "0")],
    )]);
    let r = run(&p, &dir, 0, bar(&p, 3), &RenderOptions::default());
    let onset = r.mix.l.iter().position(|x| x.abs() > 0.05).unwrap();
    let expected = 96_000 + 4_800;
    assert!(
        onset.abs_diff(expected) <= 4,
        "onset {onset}, expected {expected}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn audio_paths_cannot_leave_the_project() {
    let dir = temp_dir("escape");
    let root = dir.join("song");
    std::fs::create_dir_all(&root).unwrap();
    write_wav(&dir.join("outside.wav"), &Stereo::silence(10), 48_000).unwrap();
    let clip_to = |file: &str| song(vec![track("a", None, vec![audio("1.1", "1", file, "0")])]);
    let err = render(
        &clip_to("../outside.wav"),
        &root,
        0,
        PPQ,
        &RenderOptions::default(),
    );
    assert!(matches!(err, Err(EngineError::Invalid(_))), "{err:?}");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("outside.wav"), root.join("link.wav")).unwrap();
        let err = render(
            &clip_to("link.wav"),
            &root,
            0,
            PPQ,
            &RenderOptions::default(),
        );
        assert!(
            matches!(&err, Err(EngineError::Invalid(m)) if m.contains("leaves")),
            "{err:?}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn invalid_requests_are_errors() {
    let (p, root) = example("nachtzug");
    let bad = |from, to, o: RenderOptions| render(&p, &root, from, to, &o).is_err();
    assert!(bad(10, 5, RenderOptions::default()));
    assert!(bad(0, PPQ, opts(0, 256, &[])));
    assert!(bad(0, PPQ, opts(48_000, 0, &[])));
    assert!(bad(0, PPQ, opts(48_000, 256, &["nope"])));
    let mut broken = p.clone();
    broken.tempo = 0.0;
    assert!(render(&broken, &root, 0, PPQ, &RenderOptions::default()).is_err());
}

/// The whole example song with every stem. Prints levels so the mix can be balanced.
#[test]
fn nachtzug_full_song() {
    let (p, root) = example("nachtzug");
    let ids: Vec<String> = p.tracks.iter().map(|t| t.id.clone()).collect();
    let o = RenderOptions {
        stems: ids.clone(),
        ..RenderOptions::default()
    };
    let started = Instant::now();
    let r = run(&p, &root, 0, p.end_tick(), &o);
    let took = started.elapsed();
    let t = p.timing();
    assert_eq!(r.mix.len() as u64, t.sample(p.end_tick(), 48_000));
    let peak = r
        .mix
        .l
        .iter()
        .chain(&r.mix.r)
        .inspect(|x| assert!(x.is_finite()))
        .fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(rms(&r.mix.l) > 1e-3, "mix is silent");
    println!(
        "nachtzug: {:.1} s rendered in {took:.2?}, mix peak {:.1} dBFS, RMS {:.1} dBFS",
        r.mix.len() as f64 / 48_000.0,
        dbfs(f64::from(peak)),
        dbfs(rms(&r.mix.l))
    );
    for id in &ids {
        let st = &r.stems[id];
        // Measure where the track plays, not over the whole song.
        let (from, to) = p
            .track(id)
            .unwrap()
            .clips
            .iter()
            .map(|c| c.span(&t))
            .fold((u64::MAX, 0), |(a, b), (c, d)| (a.min(c), b.max(d)));
        let (a, b) = (
            t.sample(from, 48_000) as usize,
            t.sample(to, 48_000) as usize,
        );
        let level = rms(&st.l[a..b]).max(rms(&st.r[a..b]));
        let peak = st.l.iter().chain(&st.r).fold(0.0f32, |m, x| m.max(x.abs()));
        println!(
            "  {id:<5} RMS {:6.1} dBFS, peak {:6.1} dBFS (while playing, post-fader)",
            dbfs(level),
            dbfs(f64::from(peak))
        );
        assert!(level > 1e-3, "{id} is silent");
    }
}
