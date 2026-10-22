//! Compact plain-text view of a project for agents. It names every id an op needs,
//! shows only params that differ from their defaults and can zoom to a range and to
//! single tracks, so a whole song costs few tokens.

use std::fmt::Write;

use crate::model::{Clip, Device, Lane, Project, Track};
use crate::range::format_range;

#[derive(Clone, Debug, Default)]
pub struct DigestOptions {
    /// Only clips, notes and automation points overlapping this tick range.
    pub range: Option<(u64, u64)>,
    /// Only these tracks (plus the header). `master` selects the master bus.
    pub tracks: Option<Vec<String>>,
    /// List every note instead of a summary per clip.
    pub notes: bool,
}

pub fn digest(p: &Project, opts: &DigestOptions) -> String {
    let t = p.timing();
    let mut out = String::new();
    let bars = p.end_tick().div_ceil(crate::model::bar_ticks(&t));
    let _ = writeln!(
        out,
        "{} · {} BPM · {}/4 · {bars} bars",
        p.name, p.tempo, p.beats_per_bar
    );
    if !p.sections.is_empty() {
        let s: Vec<String> = p
            .sections
            .iter()
            .map(|s| format!("{} {}-{}", s.name, s.from, s.to))
            .collect();
        let _ = writeln!(out, "sections: {}", s.join(" | "));
    }
    if let Some((from, to)) = opts.range {
        let _ = writeln!(out, "range: {}", format_range(p, from, to));
    }
    let wanted = |id: &str| {
        opts.tracks
            .as_ref()
            .is_none_or(|ts| ts.iter().any(|t| t == id))
    };
    for track in p.tracks.iter().filter(|tr| wanted(&tr.id)) {
        track_digest(p, track, opts, &mut out);
    }
    if wanted(crate::model::MASTER) {
        let m = &p.master;
        let _ = writeln!(
            out,
            "master{}",
            if m.gain_db != 0.0 {
                format!(" gain {} dB", m.gain_db)
            } else {
                String::new()
            }
        );
        fx_digest(&m.fx, &mut out);
        lanes_digest(p, &m.automation, opts, &mut out);
    }
    out
}

fn track_digest(p: &Project, tr: &Track, opts: &DigestOptions, out: &mut String) {
    let t = p.timing();
    let mut head = format!("track {}", tr.id);
    if let Some(inst) = &tr.instrument {
        let _ = write!(head, " {}", device(inst));
    } else {
        head.push_str(" audio");
    }
    if tr.gain_db != 0.0 {
        let _ = write!(head, " gain {} dB", tr.gain_db);
    }
    if tr.pan != 0.0 {
        let _ = write!(head, " pan {}", tr.pan);
    }
    if tr.mute {
        head.push_str(" MUTED");
    }
    let _ = writeln!(out, "{head}");
    for c in &tr.clips {
        let (start, end) = c.span(&t);
        if opts
            .range
            .is_some_and(|(from, to)| end <= from || start >= to)
        {
            continue;
        }
        match c {
            Clip::Notes {
                id,
                at,
                len,
                loop_len,
                notes,
            } => {
                let looped = loop_len.map(|l| format!(" loop {l}")).unwrap_or_default();
                let _ = write!(
                    out,
                    "  clip {id} notes {at} +{len}{looped}: {} notes",
                    notes.len()
                );
                if opts.notes || notes.len() <= 8 {
                    let list: Vec<String> = notes.iter().map(|n| n.to_string()).collect();
                    let _ = writeln!(out, " [{}]", list.join(", "));
                } else {
                    let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(0);
                    let hi = notes.iter().map(|n| n.pitch).max().unwrap_or(0);
                    let _ = writeln!(
                        out,
                        " {}-{}",
                        crate::model::pitch_name(lo),
                        crate::model::pitch_name(hi)
                    );
                }
            }
            Clip::Audio {
                id,
                at,
                len,
                file,
                offset,
                gain_db,
            } => {
                let _ = write!(out, "  clip {id} audio {at} +{len} {file}");
                if offset.ticks() > 0 {
                    let _ = write!(out, " from {offset}");
                }
                if *gain_db != 0.0 {
                    let _ = write!(out, " gain {gain_db} dB");
                }
                out.push('\n');
            }
        }
    }
    fx_digest(&tr.fx, out);
    lanes_digest(p, &tr.automation, opts, out);
}

fn fx_digest(fx: &[Device], out: &mut String) {
    if !fx.is_empty() {
        let list: Vec<String> = fx.iter().map(device).collect();
        let _ = writeln!(out, "  fx: {}", list.join(" → "));
    }
}

fn lanes_digest(p: &Project, lanes: &[Lane], opts: &DigestOptions, out: &mut String) {
    let t = p.timing();
    for lane in lanes {
        let points: Vec<String> = lane
            .points
            .iter()
            .filter(|(pos, _)| {
                opts.range
                    .is_none_or(|(from, to)| (from..to).contains(&t.tick(*pos)))
            })
            .map(|(pos, v)| format!("{pos}:{v}"))
            .collect();
        if !points.is_empty() {
            let _ = writeln!(out, "  auto {}: {}", lane.target, points.join(" "));
        }
    }
}

/// `id Kind(param=value …) ← sidechain` with only non-default params.
pub fn device(d: &Device) -> String {
    let params: Vec<String> = d
        .kind
        .params()
        .iter()
        .filter_map(|s| {
            d.params
                .get(s.name)
                .filter(|v| **v != s.default)
                .map(|v| format!("{}={v}", s.name))
        })
        .collect();
    let mut s = format!("{} {:?}", d.id, d.kind);
    if !params.is_empty() {
        let _ = write!(s, "({})", params.join(" "));
    }
    if let Some(src) = &d.sidechain {
        let _ = write!(s, " ←{src}");
    }
    if d.bypass {
        s.push_str(" BYPASSED");
    }
    s
}
