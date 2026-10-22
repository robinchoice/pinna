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

pub mod audio_io;

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
    let _ = (project, root, from, to, opts);
    todo!("engine")
}
