//! Audio file input (WAV, FLAC, MP3, Ogg via symphonia, resampled with rubato) and
//! WAV output (hound).

use std::fmt::Display;
use std::path::Path;

use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::{EngineError, Stereo};

/// Decode a file to stereo at `sample_rate`. Mono files are copied to both channels.
pub fn read_audio(path: &Path, sample_rate: u32) -> Result<Stereo, EngineError> {
    let err = |e: &dyn Display| EngineError::Audio(format!("{}: {e}", path.display()));
    let file = std::fs::File::open(path).map_err(|e| err(&e))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| err(&e))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| err(&"no audio track"))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| err(&"unsupported codec"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| err(&e))?;

    let mut rate = sample_rate;
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    while let Some(packet) = format.next_packet().map_err(|e| err(&e))? {
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // A corrupt packet is skipped, like players do.
            Err(DecodeError::DecodeError(_)) => continue,
            Err(e) => return Err(err(&e)),
        };
        rate = buf.spec().rate();
        buf.copy_to_vecs_planar(&mut planes);
        channels.resize(planes.len().min(2), Vec::new());
        for (ch, plane) in channels.iter_mut().zip(&planes) {
            ch.extend_from_slice(plane);
        }
    }
    let Some(l) = channels.first().cloned() else {
        return Ok(Stereo::default());
    };
    let r = channels.get(1).cloned().unwrap_or_else(|| l.clone());
    let audio = Stereo { l, r };
    if rate == sample_rate || audio.is_empty() {
        return Ok(audio);
    }
    resample(&audio, rate, sample_rate).map_err(|e| err(&e))
}

fn resample(audio: &Stereo, from: u32, to: u32) -> Result<Stereo, String> {
    let frames = audio.len();
    let mut resampler = Fft::<f32>::new(from as usize, to as usize, 1024, 2, FixedSync::Input)
        .map_err(|e| e.to_string())?;
    let planes = [audio.l.clone(), audio.r.clone()];
    let input = SequentialSliceOfVecs::new(&planes, 2, frames).map_err(|e| e.to_string())?;
    let data = resampler
        .process_all(&input, frames, None)
        .map_err(|e| e.to_string())?
        .take_data();
    Ok(Stereo {
        l: data.iter().step_by(2).copied().collect(),
        r: data.iter().skip(1).step_by(2).copied().collect(),
    })
}

/// Write 32-bit float stereo WAV.
pub fn write_wav(path: &Path, audio: &Stereo, sample_rate: u32) -> Result<(), EngineError> {
    let err = |e: hound::Error| EngineError::Audio(format!("{}: {e}", path.display()));
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(err)?;
    for (l, r) in audio.l.iter().zip(&audio.r) {
        w.write_sample(*l).map_err(err)?;
        w.write_sample(*r).map_err(err)?;
    }
    w.finalize().map_err(err)
}
