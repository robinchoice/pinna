//! Validation of a whole project. Every load, edit and save runs it, so the engine
//! and agents never see a project that silently drops something.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Clip, Device, Kind, Lane, MASTER, Project};
use crate::path::check_relative;

/// All problems found, one line each. Empty means valid.
pub fn validate(p: &Project) -> Vec<String> {
    let mut errs = Vec::new();
    let mut err = |s: String| errs.push(s);
    if p.version != crate::model::FORMAT_VERSION {
        err(format!(
            "version {} is not supported, expected {}",
            p.version,
            crate::model::FORMAT_VERSION
        ));
    }
    if !(20.0..=400.0).contains(&p.tempo) {
        err(format!("tempo {} outside 20-400 BPM", p.tempo));
    }
    if !(1..=16).contains(&p.beats_per_bar) {
        err(format!("beats_per_bar {} outside 1-16", p.beats_per_bar));
    }
    let timing = p.timing();
    let mut names = BTreeSet::new();
    for s in &p.sections {
        if !names.insert(s.name.to_lowercase()) {
            err(format!("section {:?} defined twice", s.name));
        }
        if s.from.beat > p.beats_per_bar || s.to.beat > p.beats_per_bar {
            err(format!(
                "section {:?}: beat beyond {} beats per bar",
                s.name, p.beats_per_bar
            ));
        } else if timing.tick(s.from) >= timing.tick(s.to) {
            err(format!(
                "section {:?}: from {} must be before to {}",
                s.name, s.from, s.to
            ));
        }
    }
    let mut ids = BTreeSet::new();
    for t in &p.tracks {
        if !valid_id(&t.id) || t.id == MASTER {
            err(format!(
                "track id {:?} must be lowercase ascii, digits, '-' or '_' and not \"master\"",
                t.id
            ));
        }
        if !ids.insert(t.id.as_str()) {
            err(format!("track id {:?} used twice", t.id));
        }
    }
    for t in &p.tracks {
        let at = |what: &str| format!("track {:?}: {what}", t.id);
        check_finite(t.gain_db, -60.0, 24.0, &at("gain_db"), &mut err);
        check_finite(t.pan, -1.0, 1.0, &at("pan"), &mut err);
        if let Some(inst) = &t.instrument {
            if !inst.kind.is_instrument() {
                err(at(&format!("instrument {:?} is an effect", inst.kind)));
            }
            check_device(inst, &ids, &t.id, &mut err);
        }
        let mut clip_ids = BTreeSet::new();
        for c in &t.clips {
            if !valid_id(c.id()) || !clip_ids.insert(c.id()) {
                err(at(&format!("clip id {:?} invalid or used twice", c.id())));
            }
            if c.at().beat > p.beats_per_bar {
                err(at(&format!(
                    "clip {:?} starts on beat {} of a {}-beat bar",
                    c.id(),
                    c.at().beat,
                    p.beats_per_bar
                )));
            }
            if c.len().ticks() == 0 {
                err(at(&format!("clip {:?} has zero length", c.id())));
            }
            match c {
                Clip::Notes {
                    loop_len, notes, ..
                } => {
                    if t.instrument.is_none() {
                        err(at(&format!(
                            "clip {:?} has notes but the track has no instrument",
                            c.id()
                        )));
                    }
                    let span = loop_len.unwrap_or(c.len());
                    if span.ticks() == 0 {
                        err(at(&format!("clip {:?} has loop_len 0", c.id())));
                    }
                    for n in notes {
                        if n.at >= span {
                            err(at(&format!(
                                "clip {:?}: note \"{n}\" starts at or after the {} end ({span} beats) and would never play",
                                c.id(),
                                if loop_len.is_some() { "loop" } else { "clip" }
                            )));
                        }
                    }
                }
                Clip::Audio { file, gain_db, .. } => {
                    if t.instrument.is_some() {
                        err(at(&format!(
                            "audio clip {:?} on a track with an instrument",
                            c.id()
                        )));
                    }
                    if let Err(e) = check_relative(file) {
                        err(at(&format!("clip {:?}: {e}", c.id())));
                    }
                    check_finite(
                        *gain_db,
                        -60.0,
                        24.0,
                        &at(&format!("clip {:?} gain_db", c.id())),
                        &mut err,
                    );
                }
            }
        }
        check_fx(&t.fx, &ids, &t.id, &mut err);
        check_lanes(p, &t.id, &t.automation, &mut err);
    }
    check_finite(p.master.gain_db, -60.0, 24.0, "master: gain_db", &mut err);
    check_fx(&p.master.fx, &ids, MASTER, &mut err);
    check_lanes(p, MASTER, &p.master.automation, &mut err);
    if let Some(cycle) = sidechain_cycle(p) {
        errs.push(format!("sidechain cycle: {}", cycle.join(" -> ")));
    }
    errs
}

fn check_fx(fx: &[Device], tracks: &BTreeSet<&str>, owner: &str, err: &mut impl FnMut(String)) {
    let mut ids = BTreeSet::new();
    for d in fx {
        if !valid_id(&d.id) || !ids.insert(d.id.as_str()) {
            err(format!("{owner:?}: fx id {:?} invalid or used twice", d.id));
        }
        if d.kind.is_instrument() {
            err(format!(
                "{owner:?}: fx {:?} is an instrument ({:?})",
                d.id, d.kind
            ));
        }
        check_device(d, tracks, owner, err);
    }
}

fn check_device(d: &Device, tracks: &BTreeSet<&str>, owner: &str, err: &mut impl FnMut(String)) {
    for (name, v) in &d.params {
        match d.kind.param(name) {
            Some(spec) => check_finite(
                *v,
                spec.min,
                spec.max,
                &format!("{owner:?}: {:?} {name}", d.id),
                err,
            ),
            None => {
                let names: Vec<_> = d.kind.params().iter().map(|s| s.name).collect();
                err(format!(
                    "{owner:?}: {:?} ({:?}) has no param {name:?}; params: {}",
                    d.id,
                    d.kind,
                    names.join(", ")
                ));
            }
        }
    }
    if let Some(src) = &d.sidechain {
        if d.kind != Kind::Compressor {
            err(format!(
                "{owner:?}: {:?} sidechain only works on a Compressor",
                d.id
            ));
        }
        if src == owner || !tracks.contains(src.as_str()) {
            err(format!(
                "{owner:?}: {:?} sidechain source {src:?} must be another existing track",
                d.id
            ));
        }
    }
}

fn check_lanes(p: &Project, owner: &str, lanes: &[Lane], err: &mut impl FnMut(String)) {
    let timing = p.timing();
    let mut targets = BTreeSet::new();
    for lane in lanes {
        if !targets.insert(lane.target.as_str()) {
            err(format!(
                "{owner:?}: automation for {:?} defined twice",
                lane.target
            ));
        }
        let range = match p.target_range(owner, &lane.target) {
            Ok(r) => r,
            Err(e) => {
                err(format!("{owner:?}: automation: {e}"));
                continue;
            }
        };
        if lane.points.is_empty() {
            err(format!(
                "{owner:?}: automation {:?} has no points",
                lane.target
            ));
        }
        for w in lane.points.windows(2) {
            if timing.tick(w[0].0) > timing.tick(w[1].0) {
                err(format!(
                    "{owner:?}: automation {:?} points out of order at {}",
                    lane.target, w[1].0
                ));
            }
        }
        for (pos, v) in &lane.points {
            check_finite(
                *v,
                range.min,
                range.max,
                &format!("{owner:?}: automation {:?} at {pos}", lane.target),
                err,
            );
        }
    }
}

fn check_finite(v: f64, min: f64, max: f64, what: &str, err: &mut impl FnMut(String)) {
    if !v.is_finite() || v < min || v > max {
        err(format!("{what} = {v} outside {min}..{max}"));
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Tracks feed each other only through sidechains, so a cycle there is the only way
/// the render order can be undefined.
fn sidechain_cycle(p: &Project) -> Option<Vec<String>> {
    let edges: BTreeMap<&str, Vec<&str>> = p
        .tracks
        .iter()
        .map(|t| {
            (
                t.id.as_str(),
                t.fx.iter().filter_map(|d| d.sidechain.as_deref()).collect(),
            )
        })
        .collect();
    fn visit<'a>(
        n: &'a str,
        edges: &BTreeMap<&'a str, Vec<&'a str>>,
        stack: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(i) = stack.iter().position(|s| *s == n) {
            let mut cycle: Vec<String> = stack[i..].iter().map(|s| s.to_string()).collect();
            cycle.push(n.into());
            return Some(cycle);
        }
        if !done.insert(n) {
            return None;
        }
        stack.push(n);
        for m in edges.get(n).into_iter().flatten() {
            if let Some(c) = visit(m, edges, stack, done) {
                return Some(c);
            }
        }
        stack.pop();
        None
    }
    let mut done = BTreeSet::new();
    edges
        .keys()
        .find_map(|n| visit(n, &edges, &mut Vec::new(), &mut done))
}

/// Tracks in an order where every sidechain source comes before the tracks it drives.
/// Assumes a validated project (no cycles).
pub fn render_order(p: &Project) -> Vec<usize> {
    let mut order = Vec::with_capacity(p.tracks.len());
    let mut placed = vec![false; p.tracks.len()];
    while order.len() < p.tracks.len() {
        let before = order.len();
        for (i, t) in p.tracks.iter().enumerate() {
            let ready =
                t.fx.iter()
                    .filter_map(|d| d.sidechain.as_deref())
                    .all(|src| {
                        p.tracks
                            .iter()
                            .position(|o| o.id == src)
                            .is_none_or(|j| placed[j])
                    });
            if !placed[i] && ready {
                placed[i] = true;
                order.push(i);
            }
        }
        assert!(
            order.len() > before,
            "sidechain cycle in a validated project"
        );
    }
    order
}
