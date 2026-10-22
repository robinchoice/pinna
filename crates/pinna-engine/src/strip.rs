//! A channel strip: source (instrument, audio clips or the master input), insert
//! effects, gain, pan and automation, processed one sample at a time.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use pinna_core::{Clip, Device, Lane, Project, Timing, Track};

use crate::audio_io::read_audio;
use crate::dsp::{Auto, db};
use crate::effect::Effect;
use crate::instrument::Instrument;
use crate::{EngineError, Stereo};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct NoteEvent {
    pub sample: u64,
    /// Offs sort before ons at the same sample, so back-to-back notes retrigger.
    pub on: bool,
    pub id: usize,
    pub pitch: u8,
    pub vel: u8,
}

/// Expand the notes clips of a track into sorted on/off events. Loops repeat from the
/// clip start every `loop_len`; notes are cut at the loop end and at the clip end.
pub fn note_events(track: &Track, timing: &Timing, sr: u32) -> Vec<NoteEvent> {
    let mut events = Vec::new();
    for clip in &track.clips {
        let Clip::Notes {
            loop_len, notes, ..
        } = clip
        else {
            continue;
        };
        let (start, end) = clip.span(timing);
        let span = loop_len.map_or(end - start, |l| l.ticks());
        let mut loop_start = start;
        while loop_start < end {
            let loop_end = (loop_start + span).min(end);
            for n in notes {
                let on = loop_start + n.at.ticks();
                if on >= loop_end {
                    continue;
                }
                let off = (on + n.len.ticks()).min(loop_end);
                let on = timing.sample(on, sr);
                // A note too short for the sample rate still gets one sample.
                let off = timing.sample(off, sr).max(on + 1);
                let id = events.len() / 2;
                for (sample, is_on) in [(on, true), (off, false)] {
                    events.push(NoteEvent {
                        sample,
                        on: is_on,
                        id,
                        pitch: n.pitch,
                        vel: n.vel,
                    });
                }
            }
            loop_start += span;
        }
    }
    events.sort();
    events
}

struct AudioClip {
    start: u64,
    end: u64,
    offset: u64,
    gain: f64,
    data: Rc<Stereo>,
}

enum Source {
    /// The master: the sum of all stems.
    Input,
    Notes {
        inst: Instrument,
        events: Vec<NoteEvent>,
        next: usize,
    },
    Audio(Vec<AudioClip>),
}

enum Target {
    Gain,
    Pan,
    Instrument(&'static str),
    Fx(usize, &'static str),
}

struct AutoLane {
    target: Target,
    auto: Auto,
    last: f64,
}

pub struct Strip {
    source: Source,
    fx: Vec<Effect>,
    gain: f64,
    pan: f64,
    lanes: Vec<AutoLane>,
}

/// Audio files decoded at the render rate, shared between clips.
pub type FileCache = BTreeMap<String, Rc<Stereo>>;

impl Strip {
    pub fn track(
        project: &Project,
        track: &Track,
        root: &Path,
        sr: u32,
        files: &mut FileCache,
    ) -> Result<Self, EngineError> {
        let timing = project.timing();
        let source = match &track.instrument {
            Some(dev) => Source::Notes {
                inst: Instrument::new(dev, f64::from(sr)),
                events: note_events(track, &timing, sr),
                next: 0,
            },
            None => {
                let mut clips = Vec::new();
                for clip in &track.clips {
                    let Clip::Audio {
                        file,
                        offset,
                        gain_db,
                        ..
                    } = clip
                    else {
                        continue;
                    };
                    if !files.contains_key(file) {
                        let path =
                            pinna_core::path::resolve(root, file).map_err(EngineError::Invalid)?;
                        files.insert(file.clone(), Rc::new(read_audio(&path, sr)?));
                    }
                    let (start, end) = clip.span(&timing);
                    clips.push(AudioClip {
                        start: timing.sample(start, sr),
                        end: timing.sample(end, sr),
                        offset: timing.sample(offset.ticks(), sr),
                        gain: db(*gain_db),
                        data: files[file].clone(),
                    });
                }
                Source::Audio(clips)
            }
        };
        let fx = effects(project, &track.fx, sr);
        let lanes = lanes(
            &track.automation,
            track.instrument.as_ref(),
            &track.fx,
            &timing,
            sr,
        );
        Ok(Self {
            source,
            fx,
            gain: db(track.gain_db),
            pan: track.pan,
            lanes,
        })
    }

    pub fn master(project: &Project, sr: u32) -> Self {
        let m = &project.master;
        Self {
            source: Source::Input,
            fx: effects(project, &m.fx, sr),
            gain: db(m.gain_db),
            pan: 0.0,
            lanes: lanes(&m.automation, None, &m.fx, &project.timing(), sr),
        }
    }

    /// Render samples `[b0, b0 + out.len())`. `input` is the master input, `stems` the
    /// current block of every track (sidechain sources).
    pub fn process(&mut self, b0: u64, input: &Stereo, stems: &[Stereo], out: &mut Stereo) {
        let stepped = stepped_automation();
        for k in 0..out.len() {
            let s = b0 + k as u64;
            let at = if stepped { b0 } else { s } as f64;
            for lane in &mut self.lanes {
                let v = lane.auto.at(at);
                if v == lane.last {
                    continue;
                }
                lane.last = v;
                match lane.target {
                    Target::Gain => self.gain = db(v),
                    Target::Pan => self.pan = v,
                    Target::Instrument(name) => {
                        if let Source::Notes { inst, .. } = &mut self.source {
                            inst.set(name, v);
                        }
                    }
                    Target::Fx(i, name) => self.fx[i].set(name, v),
                }
            }
            let (mut l, mut r) = match &mut self.source {
                Source::Input => (f64::from(input.l[k]), f64::from(input.r[k])),
                Source::Notes { inst, events, next } => {
                    while let Some(e) = events.get(*next).filter(|e| e.sample <= s) {
                        if e.on {
                            inst.note_on(e.id, e.pitch, e.vel);
                        } else {
                            inst.note_off(e.id);
                        }
                        *next += 1;
                    }
                    let x = inst.sample();
                    (x, x)
                }
                Source::Audio(clips) => {
                    let (mut l, mut r) = (0.0, 0.0);
                    for c in clips.iter().filter(|c| c.start <= s && s < c.end) {
                        let i = (s - c.start + c.offset) as usize;
                        if i < c.data.len() {
                            l += f64::from(c.data.l[i]) * c.gain;
                            r += f64::from(c.data.r[i]) * c.gain;
                        }
                    }
                    (l, r)
                }
            };
            for fx in &mut self.fx {
                let side = fx.sidechain.map(|t| {
                    let st = &stems[t];
                    0.5 * (f64::from(st.l[k]) + f64::from(st.r[k]))
                });
                (l, r) = fx.process(l, r, side);
            }
            // Balance law: unity at the centre, the far side fades out.
            out.l[k] = (l * self.gain * (1.0 - self.pan).min(1.0)) as f32;
            out.r[k] = (r * self.gain * (1.0 + self.pan).min(1.0)) as f32;
        }
        if let Source::Notes { inst, .. } = &mut self.source {
            inst.tidy();
        }
    }
}

fn effects(project: &Project, fx: &[Device], sr: u32) -> Vec<Effect> {
    fx.iter()
        .map(|d| {
            let side = d
                .sidechain
                .as_ref()
                .and_then(|id| project.tracks.iter().position(|t| &t.id == id));
            Effect::new(d, f64::from(sr), side)
        })
        .collect()
}

fn lanes(
    automation: &[Lane],
    instrument: Option<&Device>,
    fx: &[Device],
    timing: &Timing,
    sr: u32,
) -> Vec<AutoLane> {
    let spec = |d: &Device, name: &str| d.kind.param(name).expect("validated param").name;
    automation
        .iter()
        .map(|lane| {
            let parts: Vec<&str> = lane.target.split('.').collect();
            let target = match parts.as_slice() {
                ["gain_db"] => Target::Gain,
                ["pan"] => Target::Pan,
                ["instrument", p] => {
                    Target::Instrument(spec(instrument.expect("validated instrument"), p))
                }
                ["fx", id, p] => {
                    let i = fx.iter().position(|d| d.id == *id).expect("validated fx");
                    Target::Fx(i, spec(&fx[i], p))
                }
                _ => unreachable!("validated target {}", lane.target),
            };
            AutoLane {
                target,
                auto: Auto::new(lane, timing, sr),
                last: f64::NAN,
            }
        })
        .collect()
}

thread_local! {
    /// Test switch: hold automation per block instead of per sample, so tests can show
    /// that the click detector trips on zipper steps.
    static STEPPED: Cell<bool> = const { Cell::new(false) };
}

fn stepped_automation() -> bool {
    cfg!(test) && STEPPED.with(Cell::get)
}

#[cfg(test)]
pub fn set_stepped_automation(on: bool) {
    STEPPED.with(|c| c.set(on));
}
