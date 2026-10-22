//! Musical time. Positions are bar/beat/tick (1-based bar and beat), lengths are
//! beats written as integers or fractions. Internally everything is integer ticks at
//! [`PPQ`] per beat, so timing stays exact and renders stay deterministic.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Ticks per beat (quarter note).
pub const PPQ: u64 = 960;

/// Absolute position, written `"bar.beat"` or `"bar.beat.tick"`, e.g. `"9.1"`, `"9.3.480"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub bar: u32,
    pub beat: u32,
    pub tick: u32,
}

impl Pos {
    pub const fn new(bar: u32, beat: u32) -> Self {
        Self { bar, beat, tick: 0 }
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.tick == 0 {
            write!(f, "{}.{}", self.bar, self.beat)
        } else {
            write!(f, "{}.{}.{}", self.bar, self.beat, self.tick)
        }
    }
}

impl FromStr for Pos {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("invalid position {s:?}, expected \"bar.beat\" or \"bar.beat.tick\"");
        let parts: Vec<&str> = s.trim().split('.').collect();
        let num = |p: &str| p.parse::<u32>().map_err(|_| bad());
        let pos = match parts.as_slice() {
            [bar] => Pos::new(num(bar)?, 1),
            [bar, beat] => Pos::new(num(bar)?, num(beat)?),
            [bar, beat, tick] => Pos {
                bar: num(bar)?,
                beat: num(beat)?,
                tick: num(tick)?,
            },
            _ => return Err(bad()),
        };
        if pos.bar == 0 || pos.beat == 0 || u64::from(pos.tick) >= PPQ {
            return Err(format!(
                "invalid position {s:?}: bar and beat start at 1, tick must be below {PPQ}"
            ));
        }
        Ok(pos)
    }
}

/// Length or offset in beats, written `"4"`, `"1/2"` or `"7/4"`. Must land on the tick grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Len(pub u64);

impl Len {
    pub const fn beats(n: u64) -> Self {
        Self(n * PPQ)
    }
    pub const fn ticks(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Len {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_multiple_of(PPQ) {
            return write!(f, "{}", self.0 / PPQ);
        }
        let g = gcd(self.0, PPQ);
        write!(f, "{}/{}", self.0 / g, PPQ / g)
    }
}

impl FromStr for Len {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("invalid length {s:?}, expected beats like \"4\", \"1/2\" or \"7/4\"");
        let (num, den) = match s.trim().split_once('/') {
            Some((n, d)) => (
                n.trim().parse::<u64>().map_err(|_| bad())?,
                d.trim().parse::<u64>().map_err(|_| bad())?,
            ),
            None => (s.trim().parse::<u64>().map_err(|_| bad())?, 1),
        };
        let ticks = num.checked_mul(PPQ).ok_or_else(bad)?;
        if den == 0 || !ticks.is_multiple_of(den) {
            return Err(format!(
                "length {s:?} does not land on the {PPQ} ticks per beat grid"
            ));
        }
        Ok(Len(ticks / den))
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

macro_rules! string_serde {
    ($t:ty, $desc:expr) => {
        impl Serialize for $t {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }
        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
            }
        }
        impl JsonSchema for $t {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($t).into()
            }
            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string", "description": $desc })
            }
        }
    };
}

string_serde!(
    Pos,
    "Position \"bar.beat\" or \"bar.beat.tick\", 1-based, e.g. \"9.1\""
);
string_serde!(
    Len,
    "Beats as integer or fraction, e.g. \"4\", \"1/2\", \"7/4\""
);

/// Tempo and meter. Tempo is constant per project in this version.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    pub bpm: f64,
    pub beats_per_bar: u32,
}

impl Timing {
    pub fn tick(&self, pos: Pos) -> u64 {
        (u64::from(pos.bar - 1) * u64::from(self.beats_per_bar) + u64::from(pos.beat - 1)) * PPQ
            + u64::from(pos.tick)
    }

    pub fn pos(&self, tick: u64) -> Pos {
        let beat = tick / PPQ;
        let bpb = u64::from(self.beats_per_bar);
        Pos {
            bar: (beat / bpb) as u32 + 1,
            beat: (beat % bpb) as u32 + 1,
            tick: (tick % PPQ) as u32,
        }
    }

    pub fn seconds(&self, tick: u64) -> f64 {
        tick as f64 * 60.0 / (self.bpm * PPQ as f64)
    }

    /// First sample at or after `tick`. Rounding is the same everywhere, so stems,
    /// mixes and ranges line up sample-exactly.
    pub fn sample(&self, tick: u64, sample_rate: u32) -> u64 {
        (self.seconds(tick) * f64::from(sample_rate)).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pos_roundtrip_and_ticks() {
        let t = Timing {
            bpm: 120.0,
            beats_per_bar: 4,
        };
        let p: Pos = "9.3.480".parse().unwrap();
        assert_eq!(t.tick(p), (8 * 4 + 2) * PPQ + 480);
        assert_eq!(t.pos(t.tick(p)), p);
        assert_eq!(p.to_string(), "9.3.480");
        assert!("0.1".parse::<Pos>().is_err());
        assert!("1.1.960".parse::<Pos>().is_err());
        assert_eq!(t.sample(PPQ, 48_000), 24_000);
    }

    #[test]
    fn len_fractions() {
        assert_eq!("1/2".parse::<Len>().unwrap(), Len(480));
        assert_eq!("7/4".parse::<Len>().unwrap().to_string(), "7/4");
        assert_eq!(Len::beats(4).to_string(), "4");
        assert!("1/7".parse::<Len>().is_err());
    }
}
