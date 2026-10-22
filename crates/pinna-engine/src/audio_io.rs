//! Audio file input (WAV, FLAC, MP3, Ogg via symphonia, resampled with rubato) and
//! WAV output (hound).

use std::path::Path;

use crate::{EngineError, Stereo};

/// Decode a file to stereo at `sample_rate`. Mono files are copied to both channels.
pub fn read_audio(path: &Path, sample_rate: u32) -> Result<Stereo, EngineError> {
    let _ = (path, sample_rate);
    todo!("read_audio")
}

/// Write 32-bit float stereo WAV.
pub fn write_wav(path: &Path, audio: &Stereo, sample_rate: u32) -> Result<(), EngineError> {
    let _ = (path, audio, sample_rate);
    todo!("write_wav")
}
