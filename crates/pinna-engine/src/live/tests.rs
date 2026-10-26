//! Live playback invariants: what the device gets equals the offline render, and every
//! jump or switch is smooth. The jump detector (`max_bend`) is shown to trip when the
//! fades are switched off.

use std::path::Path;
use std::time::{Duration, Instant};

use pinna_core::{PPQ, Project};

use super::{Output, Player, Song};
use crate::audio_io::write_wav;
use crate::tests::{audio, bar, example, max_bend, opts, song, temp_dir, track};
use crate::{Stereo, render};

const SR: u32 = 48_000;
const FADE: usize = 240;
/// Callback sizes vary on real devices; none of them lines up with the engine blocks.
const CHUNKS: [usize; 5] = [64, 441, 1000, 3, 512];

/// Pull `n` frames through the callback, waiting for the render thread like a device
/// that never underruns.
fn collect(out: &mut Output, n: usize) -> Stereo {
    let mut got = Stereo::default();
    let mut buf = vec![0.0f32; 2 * 1000];
    let started = Instant::now();
    for i in 0.. {
        if got.len() >= n {
            break;
        }
        let c = CHUNKS[i % CHUNKS.len()].min(n - got.len());
        while out.ready() < c {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "render stalled"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        out.fill(&mut buf[..2 * c], 2);
        for f in buf[..2 * c].chunks(2) {
            got.l.push(f[0]);
            got.r.push(f[1]);
        }
    }
    got
}

fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "timeout: {what}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A 200 Hz sine over 8 bars at amplitude 0.5 and gain `gain_db`, and the bend a
/// smooth version of it may reach.
fn tone(dir: &Path, gain_db: f64) -> (Project, f32) {
    let (freq, amp) = (200.0f64, 0.5f64);
    let s: Vec<f32> = (0..16 * SR)
        .map(|i| (amp * (std::f64::consts::TAU * freq * f64::from(i) / f64::from(SR)).sin()) as f32)
        .collect();
    write_wav(&dir.join("tone.wav"), &Stereo { l: s.clone(), r: s }, SR).unwrap();
    let mut t = track("tone", None, vec![audio("1.1", "32", "tone.wav", "0")]);
    t.gain_db = gain_db;
    let w = std::f64::consts::TAU * freq / f64::from(SR);
    (song(vec![t]), (2.0 * amp * w * w) as f32)
}

fn play(player: &Player, a: &Project, b: Option<&Project>, root: &Path, from: u64) {
    player.set_song(Song {
        a: a.clone(),
        b: b.cloned(),
        root: root.to_path_buf(),
    });
    player.play(from);
}

#[test]
fn live_output_equals_the_offline_render() {
    let (p, root) = example("nachtzug");
    let (player, mut out) = Player::new(SR);
    let from = p.timing().sample(bar(&p, 9), SR);
    play(&player, &p, None, &root, from);
    let live = collect(&mut out, 2 * SR as usize);
    let off = render(&p, &root, bar(&p, 9), bar(&p, 11), &opts(SR, 256, &[])).unwrap();
    assert_eq!(off.start_sample, from);
    let n = live.len();
    assert_eq!(live.l[FADE..], off.mix.l[FADE..n]);
    assert_eq!(live.r[FADE..], off.mix.r[FADE..n]);
    // The fade-in only scales down.
    for k in 0..FADE {
        assert!(live.l[k].abs() <= off.mix.l[k].abs());
    }
    assert_eq!(player.underruns(), 0);
    assert!(player.position() >= from + n as u64 - 1);
}

fn loop_bend(fade: usize) -> (f32, f32, Stereo, usize) {
    let dir = temp_dir(&format!("live-loop-{fade}"));
    let (p, limit) = tone(&dir, 0.0);
    let (player, mut out) = Player::with_fade(SR, fade);
    // Not a whole number of periods, so the wrap jumps in phase.
    let (ls, le) = (24_000, 62_437);
    player.set_loop(Some((ls, le)));
    player.set_song(Song {
        a: p,
        b: None,
        root: dir.clone(),
    });
    wait("loop armed", || player.loop_ready());
    player.play(ls);
    let len = (le - ls) as usize;
    let live = collect(&mut out, 3 * len);
    std::fs::remove_dir_all(&dir).unwrap();
    (max_bend(&live.l), limit, live, len)
}

#[test]
fn loop_wraps_smoothly_and_repeats_exactly() {
    let (bend, limit, live, len) = loop_bend(FADE);
    assert!(
        bend < 2.0 * limit,
        "loop seam bends {bend} > {}",
        2.0 * limit
    );
    // After the crossfade every pass is the first pass again.
    assert_eq!(live.l[len + FADE..2 * len], live.l[FADE..len]);
    assert_eq!(live.l[2 * len + FADE..3 * len], live.l[FADE..len]);
    let (hard, limit, _, _) = loop_bend(1);
    assert!(hard > 10.0 * limit, "detector missed the hard wrap: {hard}");
}

fn ab_bend(fade: usize) -> (f32, f32, Stereo, Stereo) {
    let dir = temp_dir(&format!("live-ab-{fade}"));
    let (a, limit) = tone(&dir, 0.0);
    let (b, _) = tone(&dir, -12.0);
    let (player, mut out) = Player::with_fade(SR, fade);
    play(&player, &a, Some(&b), &dir, 0);
    let mut live = collect(&mut out, SR as usize / 2);
    player.set_b(true);
    let after = collect(&mut out, SR as usize / 2);
    live.l.extend_from_slice(&after.l);
    let off = render(&b, &dir, 0, 4 * PPQ, &opts(SR, 256, &[])).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    // Skip the first samples: the tone starts hard at the song start.
    (max_bend(&live.l[2..]), limit, live, off.mix)
}

#[test]
fn ab_switch_is_smooth_and_then_plays_b_exactly() {
    let (bend, limit, live, b) = ab_bend(FADE);
    assert!(
        bend < 2.0 * limit,
        "A/B switch bends {bend} > {}",
        2.0 * limit
    );
    let n = live.len();
    assert_eq!(live.l[n / 2 + FADE..], b.l[n / 2 + FADE..n]);
    let (hard, limit, _, _) = ab_bend(1);
    assert!(
        hard > 10.0 * limit,
        "detector missed the hard switch: {hard}"
    );
}

#[test]
fn b_gain_matches_the_level() {
    let dir = temp_dir("live-gain");
    let (a, _) = tone(&dir, 0.0);
    let (b, _) = tone(&dir, -12.0);
    let (player, mut out) = Player::new(SR);
    player.set_b(true);
    player.set_b_gain_db(12.0);
    play(&player, &a, Some(&b), &dir, 0);
    let live = collect(&mut out, SR as usize / 2);
    let off = render(&a, &dir, 0, 2 * PPQ, &opts(SR, 256, &[])).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let diff = live.l[FADE..]
        .iter()
        .zip(&off.mix.l[FADE..])
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max);
    assert!(diff < 1e-4, "gain-matched B differs from A by {diff}");
}

#[test]
fn stop_fades_out_to_exact_silence() {
    let dir = temp_dir("live-stop");
    let (a, limit) = tone(&dir, 0.0);
    let (player, mut out) = Player::new(SR);
    play(&player, &a, None, &dir, 0);
    let mut live = collect(&mut out, SR as usize / 4);
    player.stop();
    wait("stopped", || !player.is_playing());
    let mut rest = vec![0.0f32; 2 * 2000];
    out.fill(&mut rest, 2);
    live.l.extend(rest.iter().step_by(2));
    std::fs::remove_dir_all(&dir).unwrap();
    let bend = max_bend(&live.l[2..]);
    assert!(bend < 2.0 * limit, "stop bends {bend} > {}", 2.0 * limit);
    assert!(live.l[live.len() - 1000..].iter().all(|&x| x == 0.0));
    assert_eq!(player.underruns(), 0);
}

#[test]
fn a_new_song_while_playing_crossfades_at_the_same_position() {
    let dir = temp_dir("live-song");
    let (a, limit) = tone(&dir, 0.0);
    let (b, _) = tone(&dir, -12.0);
    let (player, mut out) = Player::new(SR);
    play(&player, &a, None, &dir, 0);
    let mut live = collect(&mut out, SR as usize / 2);
    player.set_song(Song {
        a: b.clone(),
        b: None,
        root: dir.clone(),
    });
    // The new render is built beside the playing one; play on in real time meanwhile.
    while !player.song_ready() {
        let more = collect(&mut out, 64);
        live.l.extend_from_slice(&more.l);
        std::thread::sleep(Duration::from_micros(1300));
    }
    let after = collect(&mut out, SR as usize);
    live.l.extend_from_slice(&after.l);
    let off = render(&b, &dir, 0, 32 * PPQ, &opts(SR, 256, &[])).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let bend = max_bend(&live.l[2..]);
    assert!(
        bend < 2.0 * limit,
        "song change bends {bend} > {}",
        2.0 * limit
    );
    let n = live.len();
    let tail = n - SR as usize / 4;
    assert_eq!(live.l[tail..], off.mix.l[tail..n]);
}

#[test]
fn nothing_plays_past_the_song_end() {
    let dir = temp_dir("live-end");
    let (a, _) = tone(&dir, 0.0);
    let (player, mut out) = Player::new(SR);
    let end = a.timing().sample(a.end_tick(), SR);
    play(&player, &a, None, &dir, end - 1000);
    // One second of tail after the end, then the render thread stops by itself.
    let live = collect(&mut out, 1000 + SR as usize);
    wait("song end", || !player.is_playing());
    assert!(live.l[..1000].iter().any(|&x| x != 0.0));
    assert_eq!(out.ready(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A device that asks for 512 frames every 512 samples of wall time. The example song
/// with a proposal (two renders), a song change and a jump while playing: the render
/// thread keeps up and nothing drops out.
#[test]
fn real_time_feed_has_no_dropouts() {
    let (a, root) = example("nachtzug");
    let mut b = a.clone();
    b.track_mut("bass").unwrap().gain_db -= 3.0;
    let (player, mut out) = Player::new(SR);
    play(
        &player,
        &a,
        Some(&b),
        &root,
        a.timing().sample(bar(&a, 9), SR),
    );
    wait("playing", || out.ready() > 0);
    let period = Duration::from_secs_f64(512.0 / f64::from(SR));
    let mut buf = vec![0.0f32; 2 * 512];
    let started = Instant::now();
    let mut next = started;
    let mut peak = 0.0f32;
    for i in 0..(3 * SR as usize / 512) {
        if i == 100 {
            let mut c = b.clone();
            c.master.gain_db -= 1.0;
            player.set_song(Song {
                a: a.clone(),
                b: Some(c),
                root: root.clone(),
            });
            player.set_b(true);
        }
        next += period;
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        out.fill(&mut buf, 2);
        peak = buf.iter().fold(peak, |m, x| {
            assert!(x.is_finite());
            m.max(x.abs())
        });
    }
    assert_eq!(player.underruns(), 0, "dropouts while playing");
    assert!(player.song_ready(), "the song change never arrived");
    assert!(peak > 0.01, "silent");
}
