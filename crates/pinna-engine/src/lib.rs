//! Pinna's audio engine: compiles a project into a graph of built-in instruments and
//! effects and renders it offline, block by block and deterministically.
//!
//! Contract (Etappe 1):
//! - `render` always runs the song from tick 0 so voices, compressor state and tails
//!   that start before `from` are correct, and returns exactly the samples of
//!   `[from, to)` (sample positions via [`pinna_core::Timing::sample`]).
//! - Same project, same options and same build give bit-identical output.
//! - Stems are post-fader (after fx, gain, pan and automation): the sum of all stems
//!   is the master input. The mix is the master output after master fx and gain.
//! - Sidechain sources are the source track's stem signal (mono sum).

use std::collections::BTreeMap;
use std::path::Path;

use pinna_core::Project;
use pinna_core::validate::{render_order, validate};

use crate::strip::Strip;

pub mod audio_io;
mod dsp;
mod effect;
mod instrument;
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
    let errs = validate(project);
    if !errs.is_empty() {
        return Err(EngineError::Invalid(errs.join("; ")));
    }
    if opts.sample_rate == 0 || opts.block_size == 0 {
        return Err(EngineError::Invalid(
            "sample_rate and block_size must be positive".into(),
        ));
    }
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

    let sr = opts.sample_rate;
    let timing = project.timing();
    let (start, end) = (timing.sample(from, sr), timing.sample(to, sr));
    let mut files = strip::FileCache::new();
    let mut tracks = project
        .tracks
        .iter()
        .map(|t| Strip::track(project, t, root, sr, &mut files))
        .collect::<Result<Vec<_>, _>>()?;
    let mut master = Strip::master(project, sr);
    let order: Vec<usize> = render_order(project)
        .into_iter()
        .filter(|&i| !project.tracks[i].mute)
        .collect();

    let len = (end - start) as usize;
    let mut mix = Stereo {
        l: Vec::with_capacity(len),
        r: Vec::with_capacity(len),
    };
    let mut stems: BTreeMap<String, Stereo> = stem_tracks
        .iter()
        .map(|(id, _)| (id.clone(), mix.clone()))
        .collect();
    let mut bufs = vec![Stereo::silence(opts.block_size); tracks.len()];
    let mut sum = Stereo::silence(opts.block_size);
    let mut out = Stereo::silence(opts.block_size);
    let mut b0 = 0;
    while b0 < end {
        let n = opts.block_size.min((end - b0) as usize);
        for buf in bufs.iter_mut().chain([&mut sum, &mut out]) {
            buf.l.truncate(n);
            buf.r.truncate(n);
        }
        for &i in &order {
            let mut buf = std::mem::take(&mut bufs[i]);
            tracks[i].process(b0, &sum, &bufs, &mut buf);
            bufs[i] = buf;
        }
        for k in 0..n {
            sum.l[k] = bufs.iter().map(|b| b.l[k]).sum();
            sum.r[k] = bufs.iter().map(|b| b.r[k]).sum();
        }
        master.process(b0, &sum, &bufs, &mut out);
        if b0 + n as u64 > start {
            let skip = start.saturating_sub(b0) as usize;
            let copy = |dst: &mut Stereo, src: &Stereo| {
                dst.l.extend_from_slice(&src.l[skip..]);
                dst.r.extend_from_slice(&src.r[skip..]);
            };
            copy(&mut mix, &out);
            for (id, i) in &stem_tracks {
                copy(stems.get_mut(id).expect("stem allocated"), &bufs[*i]);
            }
        }
        b0 += n as u64;
    }
    Ok(Render {
        sample_rate: sr,
        start_sample: start,
        mix,
        stems,
    })
}
