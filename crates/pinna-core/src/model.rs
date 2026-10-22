//! The project model. It is stored as RON in `pinna.ron` and is the single source of
//! truth: the engine renders it, agents edit it, Git versions it.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::time::{Len, PPQ, Pos, Timing};

pub const FORMAT_VERSION: u32 = 1;
/// Reserved id that addresses the master bus in ops and paths.
pub const MASTER: &str = "master";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub version: u32,
    pub name: String,
    /// Beats per minute, constant for the whole project.
    pub tempo: f64,
    pub beats_per_bar: u32,
    /// Named ranges like "Refrain". `to` is exclusive.
    #[serde(default)]
    pub sections: Vec<Section>,
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub master: Master,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Section {
    pub name: String,
    pub from: Pos,
    pub to: Pos,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Track {
    pub id: String,
    /// Built-in instrument that plays `Notes` clips. Tracks without one play `Audio` clips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<Device>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gain_db: f64,
    /// -1 (left) to 1 (right).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pan: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Insert effects in signal order.
    #[serde(default)]
    pub fx: Vec<Device>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<Lane>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Master {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gain_db: f64,
    #[serde(default)]
    pub fx: Vec<Device>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<Lane>,
}

/// An instrument or effect. Unset params use the defaults from [`Kind::params`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Device {
    pub id: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f64>,
    /// Track whose output drives this device's detector (Compressor only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bypass: bool,
}

impl Device {
    pub fn new(id: &str, kind: Kind) -> Self {
        Self {
            id: id.into(),
            kind,
            params: BTreeMap::new(),
            sidechain: None,
            bypass: false,
        }
    }

    pub fn with(mut self, param: &str, value: f64) -> Self {
        self.params.insert(param.into(), value);
        self
    }

    /// Value of a param, falling back to its default. Panics on unknown names,
    /// which validation rules out for loaded projects.
    pub fn param(&self, name: &str) -> f64 {
        self.params.get(name).copied().unwrap_or_else(|| {
            self.kind
                .param(name)
                .unwrap_or_else(|| panic!("{:?} has no param {name}", self.kind))
                .default
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Kind {
    // Instruments
    /// Monophonic saw bass with sub oscillator, resonant low-pass and ADSR.
    SawBass,
    /// Polyphonic detuned triangle pad.
    Pad,
    /// Plucked string (Karplus-Strong).
    Pluck,
    /// Drum synth. C2 (36) kick, D2 (38) snare, F#2 (42) closed hat, A#2 (46) open hat.
    Drums,
    // Effects
    Gain,
    HighPass,
    LowPass,
    Peak,
    LowShelf,
    HighShelf,
    /// Feed-forward compressor; with `sidechain` set, the named track drives it.
    Compressor,
    /// Soft clipper (tanh).
    Saturator,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub unit: &'static str,
}

const fn p(name: &'static str, default: f64, min: f64, max: f64, unit: &'static str) -> ParamSpec {
    ParamSpec {
        name,
        default,
        min,
        max,
        unit,
    }
}

const LEVEL: ParamSpec = p("level_db", 0.0, -60.0, 12.0, "dB");

impl Kind {
    pub const ALL: [Kind; 12] = [
        Kind::SawBass,
        Kind::Pad,
        Kind::Pluck,
        Kind::Drums,
        Kind::Gain,
        Kind::HighPass,
        Kind::LowPass,
        Kind::Peak,
        Kind::LowShelf,
        Kind::HighShelf,
        Kind::Compressor,
        Kind::Saturator,
    ];

    pub fn is_instrument(self) -> bool {
        matches!(self, Kind::SawBass | Kind::Pad | Kind::Pluck | Kind::Drums)
    }

    pub fn params(self) -> &'static [ParamSpec] {
        match self {
            Kind::SawBass => {
                const {
                    &[
                        p("cutoff", 800.0, 20.0, 20_000.0, "Hz"),
                        p("resonance", 0.2, 0.0, 1.0, ""),
                        p("sub", 0.3, 0.0, 1.0, ""),
                        p("attack_ms", 2.0, 0.0, 2000.0, "ms"),
                        p("decay_ms", 200.0, 0.0, 5000.0, "ms"),
                        p("sustain", 0.7, 0.0, 1.0, ""),
                        p("release_ms", 80.0, 0.0, 5000.0, "ms"),
                        p("glide_ms", 0.0, 0.0, 1000.0, "ms"),
                        LEVEL,
                    ]
                }
            }
            Kind::Pad => {
                const {
                    &[
                        p("cutoff", 2000.0, 20.0, 20_000.0, "Hz"),
                        p("detune_cents", 12.0, 0.0, 100.0, "ct"),
                        p("attack_ms", 300.0, 0.0, 5000.0, "ms"),
                        p("release_ms", 600.0, 0.0, 10_000.0, "ms"),
                        LEVEL,
                    ]
                }
            }
            Kind::Pluck => {
                const {
                    &[
                        p("decay_ms", 400.0, 10.0, 5000.0, "ms"),
                        p("brightness", 0.6, 0.0, 1.0, ""),
                        LEVEL,
                    ]
                }
            }
            Kind::Drums => {
                const {
                    &[
                        p("kick_tune", 50.0, 30.0, 120.0, "Hz"),
                        p("kick_decay_ms", 350.0, 50.0, 2000.0, "ms"),
                        p("kick_level_db", 0.0, -60.0, 12.0, "dB"),
                        p("snare_level_db", 0.0, -60.0, 12.0, "dB"),
                        p("hat_level_db", -6.0, -60.0, 12.0, "dB"),
                        p("hat_decay_ms", 50.0, 5.0, 1000.0, "ms"),
                    ]
                }
            }
            Kind::Gain => const { &[p("gain_db", 0.0, -60.0, 24.0, "dB")] },
            Kind::HighPass | Kind::LowPass => {
                const {
                    &[
                        p("freq", 1000.0, 10.0, 20_000.0, "Hz"),
                        p("q", 0.707, 0.1, 10.0, ""),
                    ]
                }
            }
            Kind::Peak | Kind::LowShelf | Kind::HighShelf => {
                const {
                    &[
                        p("freq", 1000.0, 10.0, 20_000.0, "Hz"),
                        p("gain_db", 0.0, -24.0, 24.0, "dB"),
                        p("q", 0.707, 0.1, 10.0, ""),
                    ]
                }
            }
            Kind::Compressor => {
                const {
                    &[
                        p("threshold_db", -20.0, -60.0, 0.0, "dB"),
                        p("ratio", 4.0, 1.0, 20.0, ":1"),
                        p("attack_ms", 5.0, 0.1, 200.0, "ms"),
                        p("release_ms", 100.0, 5.0, 2000.0, "ms"),
                        p("makeup_db", 0.0, -24.0, 24.0, "dB"),
                    ]
                }
            }
            Kind::Saturator => {
                const {
                    &[
                        p("drive_db", 0.0, 0.0, 36.0, "dB"),
                        p("mix", 1.0, 0.0, 1.0, ""),
                        p("output_db", 0.0, -24.0, 12.0, "dB"),
                    ]
                }
            }
        }
    }

    pub fn param(self, name: &str) -> Option<&'static ParamSpec> {
        self.params().iter().find(|s| s.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Clip {
    /// Notes for the track's instrument. With `loop_len`, the notes repeat every
    /// `loop_len` beats until the clip ends.
    Notes {
        id: String,
        at: Pos,
        len: Len,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_len: Option<Len>,
        notes: Vec<Note>,
    },
    /// An audio file inside the project folder, played from `offset` beats into the file.
    Audio {
        id: String,
        at: Pos,
        len: Len,
        file: String,
        #[serde(default, skip_serializing_if = "is_zero_len")]
        offset: Len,
        #[serde(default, skip_serializing_if = "is_zero")]
        gain_db: f64,
    },
}

impl Clip {
    pub fn id(&self) -> &str {
        match self {
            Clip::Notes { id, .. } | Clip::Audio { id, .. } => id,
        }
    }
    pub fn at(&self) -> Pos {
        match self {
            Clip::Notes { at, .. } | Clip::Audio { at, .. } => *at,
        }
    }
    pub fn len(&self) -> Len {
        match self {
            Clip::Notes { len, .. } | Clip::Audio { len, .. } => *len,
        }
    }
    /// Absolute (start, end) ticks.
    pub fn span(&self, timing: &Timing) -> (u64, u64) {
        let start = timing.tick(self.at());
        (start, start + self.len().ticks())
    }
}

/// A note, written `"<pitch> <at> <len> [vel]"`, e.g. `"C2 0 1/4 110"`. `at` is in beats
/// from the clip start (or loop start), pitch is a name (C4 = 60) or a MIDI number,
/// velocity is 1–127 and defaults to 100.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Note {
    pub pitch: u8,
    pub at: Len,
    pub len: Len,
    pub vel: u8,
}

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn pitch_name(pitch: u8) -> String {
    format!(
        "{}{}",
        NAMES[usize::from(pitch % 12)],
        i32::from(pitch / 12) - 1
    )
}

pub fn parse_pitch(s: &str) -> Result<u8, String> {
    if let Ok(n) = s.parse::<u8>() {
        return if n <= 127 {
            Ok(n)
        } else {
            Err(format!("pitch {n} above 127"))
        };
    }
    let bad = || format!("invalid pitch {s:?}, expected a name like C2, F#1, Bb3 or a MIDI number");
    let mut chars = s.chars();
    let base = match chars.next().ok_or_else(bad)?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return Err(bad()),
    };
    let rest = chars.as_str();
    let (shift, octave) = match rest.chars().next() {
        Some('#') => (1, &rest[1..]),
        Some('b') => (-1, &rest[1..]),
        _ => (0, rest),
    };
    let octave: i32 = octave.parse().map_err(|_| bad())?;
    let n = (octave + 1) * 12 + base + shift;
    u8::try_from(n).ok().filter(|n| *n <= 127).ok_or_else(bad)
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", pitch_name(self.pitch), self.at, self.len)?;
        if self.vel != 100 {
            write!(f, " {}", self.vel)?;
        }
        Ok(())
    }
}

impl FromStr for Note {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split_whitespace().collect();
        let (pitch, at, len, vel) = match parts.as_slice() {
            [p, a, l] => (p, a, l, "100"),
            [p, a, l, v] => (p, a, l, *v),
            _ => {
                return Err(format!(
                    "invalid note {s:?}, expected \"<pitch> <at> <len> [vel]\" like \"C2 0 1/4 110\""
                ));
            }
        };
        let vel: u8 = vel
            .parse()
            .ok()
            .filter(|v| (1..=127).contains(v))
            .ok_or_else(|| format!("invalid velocity in note {s:?}, expected 1-127"))?;
        let len: Len = len.parse()?;
        if len.ticks() == 0 {
            return Err(format!("note {s:?} has zero length"));
        }
        Ok(Note {
            pitch: parse_pitch(pitch)?,
            at: at.parse()?,
            len,
            vel,
        })
    }
}

impl Serialize for Note {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Note {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Note {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Note".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "\"<pitch> <at> <len> [vel]\": pitch name (C4 = 60) or MIDI number, at/len in beats from clip start, velocity 1-127 (default 100). Example: \"C2 0 1/4 110\""
        })
    }
}

/// Automation of one parameter, linear between points, constant before the first and
/// after the last. `target` is relative to the owner: `gain_db`, `pan`,
/// `instrument.<param>` or `fx.<device>.<param>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Lane {
    pub target: String,
    pub points: Vec<(Pos, f64)>,
}

/// What an automation target or `set` path points at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
}

impl Project {
    pub fn timing(&self) -> Timing {
        Timing {
            bpm: self.tempo,
            beats_per_bar: self.beats_per_bar,
        }
    }

    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: &str) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// End of the song in ticks: the later of the last clip end and the last section end.
    pub fn end_tick(&self) -> u64 {
        let t = self.timing();
        let clips = self
            .tracks
            .iter()
            .flat_map(|tr| tr.clips.iter())
            .map(|c| c.span(&t).1);
        let sections = self.sections.iter().map(|s| t.tick(s.to));
        clips.chain(sections).max().unwrap_or(0)
    }

    /// Resolve the valid range for an automation target relative to an owner.
    /// `owner` is a track id or [`MASTER`].
    pub fn target_range(&self, owner: &str, target: &str) -> Result<Range, String> {
        let (instrument, fx) = if owner == MASTER {
            (None, &self.master.fx)
        } else {
            let track = self
                .track(owner)
                .ok_or_else(|| format!("no track {owner:?}"))?;
            (track.instrument.as_ref(), &track.fx)
        };
        let parts: Vec<&str> = target.split('.').collect();
        match parts.as_slice() {
            ["gain_db"] => Ok(Range {
                min: -60.0,
                max: 24.0,
            }),
            ["pan"] if owner != MASTER => Ok(Range {
                min: -1.0,
                max: 1.0,
            }),
            ["instrument", param] => {
                let dev = instrument.ok_or_else(|| format!("{owner:?} has no instrument"))?;
                spec_range(dev, param)
            }
            ["fx", id, param] => {
                let dev = fx
                    .iter()
                    .find(|d| d.id == *id)
                    .ok_or_else(|| format!("{owner:?} has no fx {id:?}"))?;
                spec_range(dev, param)
            }
            _ => Err(format!(
                "unknown target {target:?}; use gain_db, pan, instrument.<param> or fx.<device>.<param>"
            )),
        }
    }
}

fn spec_range(dev: &Device, param: &str) -> Result<Range, String> {
    let spec = dev.kind.param(param).ok_or_else(|| {
        let names: Vec<_> = dev.kind.params().iter().map(|s| s.name).collect();
        format!(
            "{:?} {:?} has no param {param:?}; params: {}",
            dev.kind,
            dev.id,
            names.join(", ")
        )
    })?;
    Ok(Range {
        min: spec.min,
        max: spec.max,
    })
}

impl Lane {
    /// Value at an absolute tick, linear between points.
    pub fn value_at(&self, timing: &Timing, tick: u64) -> Option<f64> {
        let first = self.points.first()?;
        if tick <= timing.tick(first.0) {
            return Some(first.1);
        }
        for w in self.points.windows(2) {
            let (a, b) = (timing.tick(w[0].0), timing.tick(w[1].0));
            if tick < b {
                let f = (tick - a) as f64 / (b - a).max(1) as f64;
                return Some(w[0].1 + (w[1].1 - w[0].1) * f);
            }
        }
        self.points.last().map(|p| p.1)
    }
}

/// Ticks of one bar, handy for loops and ranges.
pub fn bar_ticks(timing: &Timing) -> u64 {
    u64::from(timing.beats_per_bar) * PPQ
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn is_zero_len(v: &Len) -> bool {
    v.ticks() == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitches() {
        assert_eq!(parse_pitch("C4").unwrap(), 60);
        assert_eq!(parse_pitch("C2").unwrap(), 36);
        assert_eq!(parse_pitch("F#1").unwrap(), 30);
        assert_eq!(parse_pitch("Bb0").unwrap(), 22);
        assert_eq!(pitch_name(42), "F#2");
        assert!(parse_pitch("H2").is_err());
    }

    #[test]
    fn note_strings() {
        let n: Note = "C2 1/2 1/4 110".parse().unwrap();
        assert_eq!(
            n,
            Note {
                pitch: 36,
                at: Len(480),
                len: Len(240),
                vel: 110
            }
        );
        assert_eq!(n.to_string(), "C2 1/2 1/4 110");
        assert_eq!("E1 0 1".parse::<Note>().unwrap().to_string(), "E1 0 1");
        assert!("C2 0 0".parse::<Note>().is_err());
    }

    #[test]
    fn lane_interpolates() {
        let t = Timing {
            bpm: 120.0,
            beats_per_bar: 4,
        };
        let lane = Lane {
            target: "gain_db".into(),
            points: vec![(Pos::new(1, 1), 0.0), (Pos::new(2, 1), -12.0)],
        };
        assert_eq!(lane.value_at(&t, 0), Some(0.0));
        assert_eq!(lane.value_at(&t, 2 * PPQ), Some(-6.0));
        assert_eq!(lane.value_at(&t, 100 * PPQ), Some(-12.0));
    }
}
