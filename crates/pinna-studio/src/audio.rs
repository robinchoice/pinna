//! The audio device: the default output, fed by the engine's live [`Output`].

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use pinna_engine::live::{Output, Player};

pub struct Audio {
    pub player: Player,
    pub device: String,
    _stream: Stream,
}

pub fn start() -> Result<Audio, String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output device")?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "audio".into());
    let supported = device
        .default_output_config()
        .map_err(|e| format!("{name}: {e}"))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let (player, output) = Player::new(config.sample_rate);
    let stream = match format {
        SampleFormat::F32 => stream::<f32>(&device, &config, output),
        SampleFormat::I16 => stream::<i16>(&device, &config, output),
        SampleFormat::I32 => stream::<i32>(&device, &config, output),
        SampleFormat::U16 => stream::<u16>(&device, &config, output),
        SampleFormat::F64 => stream::<f64>(&device, &config, output),
        other => return Err(format!("{name}: sample format {other} is not supported")),
    }
    .map_err(|e| format!("{name}: {e}"))?;
    stream.play().map_err(|e| format!("{name}: {e}"))?;
    Ok(Audio {
        player,
        device: format!("{name} · {} Hz", config.sample_rate),
        _stream: stream,
    })
}

fn stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut output: Output,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    // Reused for formats other than f32; sized once, before the first callback.
    let mut scratch = vec![0.0f32; 1 << 16];
    device.build_output_stream(
        *config,
        move |data: &mut [T], _| {
            for chunk in data.chunks_mut(scratch.len() / channels * channels) {
                let buf = &mut scratch[..chunk.len()];
                output.fill(buf, channels);
                for (d, s) in chunk.iter_mut().zip(buf.iter()) {
                    *d = T::from_sample(*s);
                }
            }
        },
        |e| eprintln!("audio: {e}"),
        None,
    )
}
