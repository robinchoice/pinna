//! Live playback. A render thread runs the engine ahead of the audio device and hands
//! frames over a lock-free ring; the device callback ([`Output::fill`]) only copies,
//! blends A/B and fades, so it never blocks, locks or allocates.
//!
//! - Every frame carries A (the song) and B (a proposal, or A again without one), so
//!   switching A/B is instant and happens in the callback.
//! - Jumps (loop wrap, a new song while playing) crossfade on the render thread from
//!   the old to the new render; play and stop fade in and out in the callback.
//! - Without jumps and fades the output is bit-identical to [`crate::render`] with the
//!   same block size, because both run the same [`Renderer`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pinna_core::Project;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{DEFAULT_BLOCK, Renderer};

/// Length of every fade and crossfade.
pub const FADE_MS: f64 = 5.0;
/// How far the render thread runs ahead of the device.
const AHEAD_MS: f64 = 200.0;
/// Rendered after the song end so tails ring out.
const TAIL_MS: f64 = 1000.0;
const SCOPE_LEN: usize = 16_384;

/// What to play: the song (A) and optionally a proposal (B), both from `root`.
#[derive(Clone)]
pub struct Song {
    pub a: Project,
    pub b: Option<Project>,
    pub root: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Frame {
    pos: u64,
    epoch: u32,
    a: [f32; 2],
    b: [f32; 2],
}

#[derive(Default)]
struct Shared {
    /// Bumped on play and stop; the callback fades out frames of older epochs.
    epoch: AtomicU32,
    /// The render thread is producing frames; an empty ring now is a dropout.
    producing: AtomicBool,
    /// Play was asked for and its render is being prepared.
    starting: AtomicBool,
    b_on: AtomicBool,
    /// Linear gain on B (f32 bits), for loudness-matched A/B.
    b_gain: AtomicU32,
    /// Sample position of the frame the device played last.
    pos: AtomicU64,
    underruns: AtomicU64,
    /// Songs handed in, and how many of them are now what plays (or would play).
    songs_sent: AtomicU64,
    songs_applied: AtomicU64,
    /// The loop start is rendered and wraps will happen.
    loop_ready: AtomicBool,
    closed: AtomicBool,
    error: Mutex<Option<String>>,
}

/// Rising half of a smooth fade: zero slope at both ends, so fades add no kinks.
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// The audio-device side. Move it into the device callback.
pub struct Output {
    frames: Consumer<Frame>,
    scope: Producer<f32>,
    shared: Arc<Shared>,
    epoch: u32,
    /// Fade-in/out phase of the current epoch, 0..=1.
    env: f32,
    /// A/B phase, 0 = A, 1 = B.
    mix: f32,
    step: f32,
}

impl Output {
    /// Fill an interleaved buffer with `channels` channels (1 gets the mono sum).
    pub fn fill(&mut self, out: &mut [f32], channels: usize) {
        let target = self.shared.epoch.load(Ordering::Acquire);
        let b_on = self.shared.b_on.load(Ordering::Relaxed);
        let b_gain = f32::from_bits(self.shared.b_gain.load(Ordering::Relaxed));
        let mut underrun = false;
        for frame in out.chunks_mut(channels.max(1)) {
            if self.epoch != target && self.env <= 0.0 {
                while self.frames.peek().is_ok_and(|f| f.epoch != target) {
                    let _ = self.frames.pop();
                }
                self.epoch = target;
            }
            let fading_out = self.epoch != target;
            let (l, r) = match self.frames.peek().copied() {
                Ok(f) if f.epoch == self.epoch => {
                    let _ = self.frames.pop();
                    self.env = if fading_out {
                        (self.env - self.step).max(0.0)
                    } else {
                        (self.env + self.step).min(1.0)
                    };
                    self.mix = if b_on {
                        (self.mix + self.step).min(1.0)
                    } else {
                        (self.mix - self.step).max(0.0)
                    };
                    let (env, m) = (smoothstep(self.env), smoothstep(self.mix));
                    self.shared.pos.store(f.pos, Ordering::Relaxed);
                    (
                        env * (f.a[0] * (1.0 - m) + f.b[0] * b_gain * m),
                        env * (f.a[1] * (1.0 - m) + f.b[1] * b_gain * m),
                    )
                }
                _ => {
                    if fading_out {
                        // Nothing left of the old epoch to fade.
                        self.env = 0.0;
                    } else if self.shared.producing.load(Ordering::Acquire) {
                        underrun = true;
                    }
                    (0.0, 0.0)
                }
            };
            let _ = self.scope.push(0.5 * (l + r));
            match frame {
                [m] => *m = 0.5 * (l + r),
                [fl, fr, rest @ ..] => {
                    (*fl, *fr) = (l, r);
                    rest.fill(0.0);
                }
                [] => {}
            }
        }
        if underrun {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Frames rendered ahead and ready for the device.
    pub fn ready(&self) -> usize {
        self.frames.slots()
    }
}

enum Msg {
    Song(Box<Song>),
    Play(u64),
    Stop,
    Loop(Option<(u64, u64)>),
    Built(u64, Result<Box<Deck>, String>),
}

/// The control side, for the UI thread. Dropping it ends the render thread.
pub struct Player {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    scope: Consumer<f32>,
    sample_rate: u32,
}

impl Player {
    pub fn new(sample_rate: u32) -> (Self, Output) {
        let fade = (FADE_MS * f64::from(sample_rate) / 1000.0).round() as usize;
        Self::with_fade(sample_rate, fade.max(1))
    }

    /// `fade` in samples; tests use 1 to show that the jump detectors trip.
    fn with_fade(sample_rate: u32, fade: usize) -> (Self, Output) {
        let ahead = (AHEAD_MS * f64::from(sample_rate) / 1000.0) as usize;
        let (frames_tx, frames_rx) = RingBuffer::new(ahead.max(DEFAULT_BLOCK));
        let (scope_tx, scope_rx) = RingBuffer::new(SCOPE_LEN);
        let shared = Arc::new(Shared::default());
        shared.b_gain.store(1.0f32.to_bits(), Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let mut engine = Engine {
            sr: sample_rate,
            fade,
            frames: frames_tx,
            shared: shared.clone(),
            tx: tx.clone(),
            rx,
            song: None,
            end: 0,
            deck: None,
            old: None,
            epoch: 0,
            play_from: 0,
            looping: None,
            snapshot: None,
            songs: 0,
            next_req: 0,
            want_play: 0,
            want_switch: 0,
            want_snap: 0,
        };
        thread::Builder::new()
            .name("pinna-render".into())
            .spawn(move || engine.run())
            .expect("spawn render thread");
        let output = Output {
            frames: frames_rx,
            scope: scope_tx,
            shared: shared.clone(),
            epoch: 0,
            env: 0.0,
            mix: 0.0,
            step: 1.0 / fade as f32,
        };
        let player = Self {
            tx,
            shared,
            scope: scope_rx,
            sample_rate,
        };
        (player, output)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Replace what plays. While playing, it continues at the same position.
    pub fn set_song(&self, song: Song) {
        self.shared.songs_sent.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Song(Box::new(song)));
    }

    /// Start (or restart) at a sample position.
    pub fn play(&self, from: u64) {
        self.shared.starting.store(true, Ordering::Release);
        let _ = self.tx.send(Msg::Play(from));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Msg::Stop);
    }

    /// Loop `[start, end)` in samples. Playback started before `end` wraps at `end`.
    pub fn set_loop(&self, range: Option<(u64, u64)>) {
        let _ = self.tx.send(Msg::Loop(range));
    }

    /// Hear B instead of A, crossfaded.
    pub fn set_b(&self, on: bool) {
        self.shared.b_on.store(on, Ordering::Relaxed);
    }

    pub fn set_b_gain_db(&self, db: f64) {
        let g = 10f64.powf(db / 20.0) as f32;
        self.shared.b_gain.store(g.to_bits(), Ordering::Relaxed);
    }

    /// Sample position the device played last.
    pub fn position(&self) -> u64 {
        self.shared.pos.load(Ordering::Relaxed)
    }

    /// Playing or about to: true from [`Self::play`] until stop or the song end.
    pub fn is_playing(&self) -> bool {
        self.shared.producing.load(Ordering::Acquire)
            || self.shared.starting.load(Ordering::Acquire)
    }

    /// The last song handed to [`Self::set_song`] is what plays now.
    pub fn song_ready(&self) -> bool {
        self.shared.songs_applied.load(Ordering::Acquire)
            == self.shared.songs_sent.load(Ordering::Relaxed)
    }

    /// The loop is armed: playback reaching its end wraps.
    pub fn loop_ready(&self) -> bool {
        self.shared.loop_ready.load(Ordering::Acquire)
    }

    /// Device callbacks that found no frame while playing.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// The last render error, cleared by reading it.
    pub fn take_error(&self) -> Option<String> {
        self.shared.error.lock().ok()?.take()
    }

    /// Move what the device played since the last call (mono) into `out`.
    pub fn drain_scope(&mut self, out: &mut Vec<f32>) {
        // Only what is there now: a fast device would otherwise keep this busy.
        if let Ok(chunk) = self.scope.read_chunk(self.scope.slots()) {
            out.extend(chunk);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
    }
}

/// A and B rendered in lockstep, read one frame at a time.
#[derive(Clone)]
struct Deck {
    a: Renderer,
    b: Option<Renderer>,
    /// First sample of the rendered block.
    block: u64,
    filled: bool,
    /// Next sample to read.
    next: u64,
}

impl Deck {
    fn new(song: &Song, sr: u32, at: u64) -> Result<Self, String> {
        let make = |p: &Project| {
            let mut r =
                Renderer::new(p, &song.root, sr, DEFAULT_BLOCK).map_err(|e| e.to_string())?;
            r.skip_to(at);
            Ok::<_, String>(r)
        };
        Ok(Self {
            a: make(&song.a)?,
            b: song.b.as_ref().map(make).transpose()?,
            block: 0,
            filled: false,
            next: at,
        })
    }

    /// Render ahead to `at` now rather than when the next frame is read.
    fn advance(&mut self, at: u64) {
        if at > self.next {
            self.next = at;
            self.filled = false;
            self.a.skip_to(at);
            if let Some(b) = &mut self.b {
                b.skip_to(at);
            }
        }
    }

    fn frame(&mut self) -> ([f32; 2], [f32; 2]) {
        let size = self.a.block_size() as u64;
        if !self.filled || self.next >= self.block + size {
            self.a.skip_to(self.next);
            self.block = self.a.next_block();
            if let Some(b) = &mut self.b {
                b.skip_to(self.next);
                b.next_block();
            }
            self.filled = true;
        }
        let k = (self.next - self.block) as usize;
        self.next += 1;
        let at = |r: &Renderer| [r.mix().l[k], r.mix().r[k]];
        let a = at(&self.a);
        (a, self.b.as_ref().map_or(a, at))
    }
}

struct Engine {
    sr: u32,
    fade: usize,
    frames: Producer<Frame>,
    shared: Arc<Shared>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    song: Option<Arc<Song>>,
    /// Last sample to play, the song end plus a tail.
    end: u64,
    deck: Option<Deck>,
    /// The render being crossfaded out, with the samples left.
    old: Option<(Deck, usize)>,
    epoch: u32,
    play_from: u64,
    looping: Option<(u64, u64)>,
    /// A deck ready at the loop start; every wrap plays a clone of it.
    snapshot: Option<Deck>,
    songs: u64,
    next_req: u64,
    want_play: u64,
    want_switch: u64,
    want_snap: u64,
}

impl Engine {
    fn run(&mut self) {
        while !self.shared.closed.load(Ordering::Relaxed) {
            let msg = if self.deck.is_some() {
                match self.rx.try_recv() {
                    Ok(msg) => Some(msg),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                match self.rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            };
            if let Some(msg) = msg {
                self.handle(msg);
            } else if self.deck.is_some() && !self.produce() {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Build a deck off this thread; the result comes back as `Msg::Built`.
    fn request(&mut self, at: u64) -> u64 {
        self.next_req += 1;
        let (id, tx, sr) = (self.next_req, self.tx.clone(), self.sr);
        if let Some(song) = self.song.clone() {
            thread::spawn(move || {
                let _ = tx.send(Msg::Built(id, Deck::new(&song, sr, at).map(Box::new)));
            });
        }
        id
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Song(song) => {
                let t = song.a.timing();
                self.end = t.sample(song.a.end_tick(), self.sr)
                    + (TAIL_MS * f64::from(self.sr) / 1000.0) as u64;
                self.song = Some(Arc::new(*song));
                self.songs += 1;
                self.set_snapshot(None);
                self.want_snap = self.looping.map_or(0, |(s, _)| self.request(s));
                if let Some(d) = &self.deck {
                    self.want_switch = self.request(d.next);
                } else if self.want_play != 0 {
                    self.want_play = self.request(self.play_from);
                } else {
                    self.applied();
                }
            }
            Msg::Play(from) => {
                self.halt();
                self.shared
                    .starting
                    .store(self.song.is_some(), Ordering::Release);
                self.play_from = from;
                let ready = self
                    .snapshot
                    .as_ref()
                    .filter(|s| self.looping.is_some_and(|(ls, _)| ls == from) && s.next == from);
                if let Some(s) = ready {
                    self.deck = Some(s.clone());
                } else {
                    self.want_play = self.request(from);
                }
            }
            Msg::Stop => self.halt(),
            Msg::Loop(range) => {
                self.looping = range.filter(|(s, e)| s < e);
                self.set_snapshot(None);
                self.want_snap = self.looping.map_or(0, |(s, _)| self.request(s));
            }
            Msg::Built(id, Err(e)) => {
                if [self.want_play, self.want_switch, self.want_snap].contains(&id) {
                    if let Ok(mut slot) = self.shared.error.lock() {
                        *slot = Some(e);
                    }
                    if id == self.want_play {
                        self.want_play = 0;
                        self.shared.starting.store(false, Ordering::Release);
                    }
                }
            }
            Msg::Built(id, Ok(deck)) => {
                if id == self.want_play {
                    self.want_play = 0;
                    self.deck = Some(*deck);
                    self.applied();
                } else if id == self.want_switch {
                    let Some(cur) = self.deck.take() else {
                        self.want_switch = 0;
                        return;
                    };
                    // It was built for where playback stood when asked. Catching up
                    // here stalls the device feed, so a long way goes back to a builder.
                    let mut new = deck;
                    if cur.next > new.next + u64::from(self.sr) / 20 {
                        let (tx, at) = (self.tx.clone(), cur.next);
                        thread::spawn(move || {
                            new.advance(at);
                            let _ = tx.send(Msg::Built(id, Ok(new)));
                        });
                        self.deck = Some(cur);
                        return;
                    }
                    new.advance(cur.next);
                    self.want_switch = 0;
                    self.applied();
                    self.old = Some((cur, self.fade));
                    self.deck = Some(*new);
                } else if id == self.want_snap {
                    self.want_snap = 0;
                    self.set_snapshot(Some(*deck));
                }
            }
        }
    }

    fn applied(&self) {
        self.shared
            .songs_applied
            .store(self.songs, Ordering::Release);
    }

    fn set_snapshot(&mut self, deck: Option<Deck>) {
        self.shared
            .loop_ready
            .store(deck.is_some(), Ordering::Release);
        self.snapshot = deck;
    }

    /// Stop producing and fade out what the device still has.
    fn halt(&mut self) {
        // The device fades out over the frames it still has; make sure there are
        // enough, even when it had caught up with us.
        for _ in 0..self.fade + 2048 {
            if self.frames.slots() == 0 {
                break;
            }
            match self.next_frame() {
                Some(f) => {
                    let _ = self.frames.push(f);
                }
                None => break,
            }
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.shared.epoch.store(self.epoch, Ordering::Release);
        self.shared.producing.store(false, Ordering::Release);
        self.shared.starting.store(false, Ordering::Release);
        self.deck = None;
        self.old = None;
        self.want_play = 0;
        self.want_switch = 0;
    }

    /// Fill the free slots of the ring. Returns false when there was none.
    fn produce(&mut self) -> bool {
        let mut wrote = false;
        while self.frames.slots() > 0 {
            let Some(frame) = self.next_frame() else {
                break;
            };
            let _ = self.frames.push(frame);
            if !wrote {
                // Only now: an empty ring before the first frame is no dropout.
                self.shared.producing.store(true, Ordering::Release);
                self.shared.starting.store(false, Ordering::Release);
                wrote = true;
            }
        }
        wrote
    }

    /// The next frame: wraps the loop, crossfades, ends after the song.
    fn next_frame(&mut self) -> Option<Frame> {
        let deck = self.deck.as_mut()?;
        if let (Some((_, le)), Some(snap)) = (self.looping, &self.snapshot)
            && self.play_from < le
            && deck.next >= le
        {
            let cur = std::mem::replace(deck, snap.clone());
            self.old = Some((cur, self.fade));
        } else if deck.next >= self.end {
            self.shared.producing.store(false, Ordering::Release);
            self.deck = None;
            self.old = None;
            return None;
        }
        let pos = deck.next;
        let (mut a, mut b) = deck.frame();
        if let Some((old, left)) = &mut self.old {
            let (oa, ob) = old.frame();
            let g = smoothstep(*left as f32 / self.fade as f32);
            for c in 0..2 {
                a[c] = a[c] * (1.0 - g) + oa[c] * g;
                b[c] = b[c] * (1.0 - g) + ob[c] * g;
            }
            *left -= 1;
            if *left == 0 {
                self.old = None;
            }
        }
        Some(Frame {
            pos,
            epoch: self.epoch,
            a,
            b,
        })
    }
}

#[cfg(test)]
mod tests;
