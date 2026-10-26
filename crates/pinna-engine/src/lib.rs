//! Pinna's audio engine: compiles a project into a graph of built-in instruments and
//! effects and renders it block by block and deterministically, offline with
//! [`render`] or in real time with [`live`].
//!
//! Contract:
//! - `render` always runs the song from tick 0 so voices, compressor state and tails
//!   that start before `from` are correct, and returns exactly the samples of
//!   `[from, to)` (sample positions via [`pinna_core::Timing::sample`]).
//! - Same project, same options and same build give bit-identical output.
//! - Stems are post-fader (after fx, gain, pan and automation): the sum of all stems
//!   is the master input. The mix is the master output after master fx and gain.
//! - Sidechain sources are the source track's stem signal (mono sum).
//! - Live playback runs the same [`Renderer`]: without jumps and fades it is
//!   bit-identical to the offline render.

use std::collections::BTreeMap;
use std::path::Path;

use pinna_core::Project;
use pinna_core::validate::{render_order, validate};

use crate::strip::Strip;

pub mod audio_io;
mod dsp;
mod effect;
mod instrument;
pub mod live;
mod strip;
#[cfg(test)]
mod tests;

pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const DEFAULT_BLOCK: usize = 256;

/// Two channels of equal length.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stereo {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

impl Stereo {
    pub fn silence(len: usize) -> Self {
        Self {
            l: vec![0.0; len],
            r: vec![0.0; len],
        }
    }
    pub fn len(&self) -> usize {
        self.l.len()
    }
    pub fn is_empty(&self) -> bool {
        self.l.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub sample_rate: u32,
    pub block_size: usize,
    /// Track ids whose stems to return. Empty: no stems.
    pub stems: Vec<String>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            block_size: DEFAULT_BLOCK,
            stems: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Render {
    pub sample_rate: u32,
    /// First sample of the returned range, counted from the song start.
    pub start_sample: u64,
    pub mix: Stereo,
    pub stems: BTreeMap<String, Stereo>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Audio(String),
    #[error("{0}")]
    Invalid(String),
}

/// Render `[from, to)` ticks of a validated project. `root` is the project folder that
/// audio clip paths are relative to.
pub fn render(
    project: &Project,
    root: &Path,
    from: u64,
    to: u64,
    opts: &RenderOptions,
) -> Result<Render, EngineError> {
    if from > to {
        return Err(EngineError::Invalid(format!(
            "range start {from} is after its end {to}"
        )));
    }
    let mut stem_tracks = Vec::new();
    for id in &opts.stems {
        let i = project
            .tracks
            .iter()
            .position(|t| &t.id == id)
            .ok_or_else(|| EngineError::Invalid(format!("no track {id:?} for a stem")))?;
        if !stem_tracks.iter().any(|(s, _)| s == id) {
            stem_tracks.push((id.clone(), i));
        }
    }
    let mut r = Renderer::new(project, root, opts.sample_rate, opts.block_size)?;
    let timing = project.timing();
    let sr = opts.sample_rate;
    let (start, end) = (timing.sample(from, sr), timing.sample(to, sr));

    let len = (end - start) as usize;
    let mut mix = Stereo {
        l: Vec::with_capacity(len),
        r: Vec::with_capacity(len),
    };
    let mut stems: BTreeMap<String, Stereo> = stem_tracks
        .iter()
        .map(|(id, _)| (id.clone(), mix.clone()))
        .collect();
    while r.position() < end {
        let b0 = r.next_block();
        if b0 + opts.block_size as u64 > start {
            let skip = start.saturating_sub(b0) as usize;
            let n = opts.block_size.min((end - b0) as usize);
            let copy = |dst: &mut Stereo, src: &Stereo| {
                dst.l.extend_from_slice(&src.l[skip..n]);
                dst.r.extend_from_slice(&src.r[skip..n]);
            };
            copy(&mut mix, r.mix());
            for (id, i) in &stem_tracks {
                copy(stems.get_mut(id).expect("stem allocated"), r.stem(*i));
            }
        }
    }
    Ok(Render {
        sample_rate: sr,
        start_sample: start,
        mix,
        stems,
    })
}

/// A running render of a project from sample 0, one block at a time. Cloning it
/// snapshots the whole state, so playback can jump back to a point without rendering
/// the song up to there again.
#[derive(Clone)]
pub struct Renderer {
    tracks: Vec<Strip>,
    master: Strip,
    order: Vec<usize>,
    bufs: Vec<Stereo>,
    sum: Stereo,
    out: Stereo,
    /// First sample of the next block.
    next: u64,
}

impl Renderer {
    pub fn new(
        project: &Project,
        root: &Path,
        sample_rate: u32,
        block_size: usize,
    ) -> Result<Self, EngineError> {
        let errs = validate(project);
        if !errs.is_empty() {
            return Err(EngineError::Invalid(errs.join("; ")));
        }
        if sample_rate == 0 || block_size == 0 {
            return Err(EngineError::Invalid(
                "sample_rate and block_size must be positive".into(),
            ));
        }
        let mut files = strip::FileCache::new();
        let tracks = project
            .tracks
            .iter()
            .map(|t| Strip::track(project, t, root, sample_rate, &mut files))
            .collect::<Result<Vec<_>, _>>()?;
        let order = render_order(project)
            .into_iter()
            .filter(|&i| !project.tracks[i].mute)
            .collect();
        Ok(Self {
            bufs: vec![Stereo::silence(block_size); tracks.len()],
            tracks,
            master: Strip::master(project, sample_rate),
            order,
            sum: Stereo::silence(block_size),
            out: Stereo::silence(block_size),
            next: 0,
        })
    }

    /// First sample of the block that [`Self::next_block`] renders next.
    pub fn position(&self) -> u64 {
        self.next
    }

    pub fn block_size(&self) -> usize {
        self.out.len()
    }

    /// Render the next block and return its first sample. [`Self::mix`] and
    /// [`Self::stem`] hold the result until the next call.
    pub fn next_block(&mut self) -> u64 {
        let b0 = self.next;
        for &i in &self.order {
            let mut buf = std::mem::take(&mut self.bufs[i]);
            self.tracks[i].process(b0, &self.sum, &self.bufs, &mut buf);
            self.bufs[i] = buf;
        }
        for k in 0..self.sum.len() {
            self.sum.l[k] = self.bufs.iter().map(|b| b.l[k]).sum();
            self.sum.r[k] = self.bufs.iter().map(|b| b.r[k]).sum();
        }
        self.master
            .process(b0, &self.sum, &self.bufs, &mut self.out);
        self.next += self.out.len() as u64;
        b0
    }

    /// Master output of the last block.
    pub fn mix(&self) -> &Stereo {
        &self.out
    }

    /// Post-fader output of track `i` in the last block.
    pub fn stem(&self, i: usize) -> &Stereo {
        &self.bufs[i]
    }

    /// Render ahead until the next block contains `sample`.
    pub fn skip_to(&mut self, sample: u64) {
        while self.next + self.out.len() as u64 <= sample {
            self.next_block();
        }
    }
}
