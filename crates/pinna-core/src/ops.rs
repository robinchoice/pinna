//! Edit operations. A batch applies atomically: either every op succeeds and the
//! result validates, or nothing changes and the error names the failing op.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::model::{Clip, Device, Lane, MASTER, Master, Note, Project, Section, Track};
use crate::time::Pos;
use crate::validate::validate;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Set a number. Paths: `tempo`, `<track>.gain_db`, `<track>.pan`, `<track>.mute`
    /// (0/1), `<track>.instrument.<param>`, `<track>.fx.<id>.<param>`,
    /// `<track>.fx.<id>.bypass` (0/1). Use `master` as track for the master bus.
    Set {
        path: String,
        value: f64,
    },
    /// Insert an effect, at the end or before the effect `before`.
    AddFx {
        track: String,
        device: Device,
        #[serde(default)]
        before: Option<String>,
    },
    RemoveFx {
        track: String,
        id: String,
    },
    /// Set or clear (`source: null`) the sidechain source track of a Compressor.
    SetSidechain {
        track: String,
        fx: String,
        source: Option<String>,
    },
    /// Replace the instrument of a track.
    SetInstrument {
        track: String,
        device: Device,
    },
    AddClip {
        track: String,
        clip: Clip,
    },
    RemoveClip {
        track: String,
        id: String,
    },
    MoveClip {
        track: String,
        id: String,
        at: Pos,
    },
    /// Replace all notes of a Notes clip.
    SetNotes {
        track: String,
        clip: String,
        notes: Vec<Note>,
    },
    /// Replace the automation of one target; empty `points` removes it.
    SetAutomation {
        track: String,
        target: String,
        points: Vec<(Pos, f64)>,
    },
    AddTrack {
        track: Track,
        #[serde(default)]
        before: Option<String>,
    },
    RemoveTrack {
        id: String,
    },
    /// Create or move a section.
    SetSection {
        name: String,
        from: Pos,
        to: Pos,
    },
    RemoveSection {
        name: String,
    },
}

impl Op {
    /// The track (or `master`/`project`) this op touches, for echoing the new state.
    pub fn scope(&self) -> &str {
        match self {
            Op::Set { path, .. } => path
                .split('.')
                .next()
                .filter(|s| *s != "tempo")
                .unwrap_or("project"),
            Op::AddFx { track, .. }
            | Op::RemoveFx { track, .. }
            | Op::SetSidechain { track, .. }
            | Op::SetInstrument { track, .. }
            | Op::AddClip { track, .. }
            | Op::RemoveClip { track, .. }
            | Op::MoveClip { track, .. }
            | Op::SetNotes { track, .. }
            | Op::SetAutomation { track, .. } => track,
            Op::AddTrack { track, .. } => &track.id,
            Op::RemoveTrack { .. } | Op::SetSection { .. } | Op::RemoveSection { .. } => "project",
        }
    }
}

/// Apply a batch to a copy of the project and validate the result.
pub fn apply(project: &Project, ops: &[Op]) -> Result<Project, Error> {
    let mut p = project.clone();
    for (i, op) in ops.iter().enumerate() {
        apply_one(&mut p, op).map_err(|msg| Error::Op { index: i, msg })?;
    }
    let errs = validate(&p);
    if errs.is_empty() {
        Ok(p)
    } else {
        Err(Error::Invalid(errs))
    }
}

fn apply_one(p: &mut Project, op: &Op) -> Result<(), String> {
    match op {
        Op::Set { path, value } => set(p, path, *value),
        Op::AddFx {
            track,
            device,
            before,
        } => {
            let fx = fx_mut(p, track)?;
            if fx.iter().any(|d| d.id == device.id) {
                return Err(format!("{track:?} already has fx {:?}", device.id));
            }
            let at = match before {
                Some(b) => fx
                    .iter()
                    .position(|d| &d.id == b)
                    .ok_or_else(|| format!("{track:?} has no fx {b:?}"))?,
                None => fx.len(),
            };
            fx.insert(at, device.clone());
            Ok(())
        }
        Op::RemoveFx { track, id } => {
            let fx = fx_mut(p, track)?;
            let i = fx
                .iter()
                .position(|d| &d.id == id)
                .ok_or_else(|| format!("{track:?} has no fx {id:?}"))?;
            fx.remove(i);
            drop_lanes(p, track, &format!("fx.{id}."));
            Ok(())
        }
        Op::SetSidechain { track, fx, source } => {
            let dev = fx_mut(p, track)?
                .iter_mut()
                .find(|d| &d.id == fx)
                .ok_or_else(|| format!("{track:?} has no fx {fx:?}"))?;
            dev.sidechain = source.clone();
            Ok(())
        }
        Op::SetInstrument { track, device } => {
            track_mut(p, track)?.instrument = Some(device.clone());
            drop_lanes(p, track, "instrument.");
            Ok(())
        }
        Op::AddClip { track, clip } => {
            let t = track_mut(p, track)?;
            if t.clips.iter().any(|c| c.id() == clip.id()) {
                return Err(format!("{track:?} already has clip {:?}", clip.id()));
            }
            t.clips.push(clip.clone());
            Ok(())
        }
        Op::RemoveClip { track, id } => {
            let t = track_mut(p, track)?;
            let i = t
                .clips
                .iter()
                .position(|c| c.id() == id)
                .ok_or_else(|| format!("{track:?} has no clip {id:?}"))?;
            t.clips.remove(i);
            Ok(())
        }
        Op::MoveClip {
            track,
            id,
            at: new_at,
        } => {
            let clip = clip_mut(p, track, id)?;
            match clip {
                Clip::Notes { at, .. } | Clip::Audio { at, .. } => *at = *new_at,
            }
            Ok(())
        }
        Op::SetNotes {
            track,
            clip,
            notes: new,
        } => match clip_mut(p, track, clip)? {
            Clip::Notes { notes, .. } => {
                *notes = new.clone();
                Ok(())
            }
            Clip::Audio { .. } => Err(format!("clip {clip:?} on {track:?} is an audio clip")),
        },
        Op::SetAutomation {
            track,
            target,
            points,
        } => {
            p.target_range(track, target)?;
            let lanes = lanes_mut(p, track)?;
            lanes.retain(|l| &l.target != target);
            if !points.is_empty() {
                let mut points = points.clone();
                points.sort_by_key(|(pos, _)| *pos);
                lanes.push(Lane {
                    target: target.clone(),
                    points,
                });
                lanes.sort_by(|a, b| a.target.cmp(&b.target));
            }
            Ok(())
        }
        Op::AddTrack { track, before } => {
            if p.track(&track.id).is_some() {
                return Err(format!("track {:?} already exists", track.id));
            }
            let at = match before {
                Some(b) => p
                    .tracks
                    .iter()
                    .position(|t| &t.id == b)
                    .ok_or_else(|| format!("no track {b:?}"))?,
                None => p.tracks.len(),
            };
            p.tracks.insert(at, track.clone());
            Ok(())
        }
        Op::RemoveTrack { id } => {
            let i = p
                .tracks
                .iter()
                .position(|t| &t.id == id)
                .ok_or_else(|| format!("no track {id:?}"))?;
            let users: Vec<String> = p
                .tracks
                .iter()
                .flat_map(|t| {
                    t.fx.iter()
                        .filter(|d| d.sidechain.as_deref() == Some(id))
                        .map(move |d| format!("{}.fx.{}", t.id, d.id))
                })
                .collect();
            if !users.is_empty() {
                return Err(format!(
                    "track {id:?} drives sidechains of {}; clear them first",
                    users.join(", ")
                ));
            }
            p.tracks.remove(i);
            Ok(())
        }
        Op::SetSection { name, from, to } => {
            match p
                .sections
                .iter_mut()
                .find(|s| s.name.eq_ignore_ascii_case(name))
            {
                Some(s) => {
                    s.from = *from;
                    s.to = *to;
                }
                None => p.sections.push(Section {
                    name: name.clone(),
                    from: *from,
                    to: *to,
                }),
            }
            let t = p.timing();
            p.sections.sort_by_key(|s| t.tick(s.from));
            Ok(())
        }
        Op::RemoveSection { name } => {
            let before = p.sections.len();
            p.sections.retain(|s| !s.name.eq_ignore_ascii_case(name));
            if p.sections.len() == before {
                Err(format!("no section {name:?}"))
            } else {
                Ok(())
            }
        }
    }
}

fn set(p: &mut Project, path: &str, value: f64) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').collect();
    let flag = |v: f64| {
        if v == 0.0 || v == 1.0 {
            Ok(v == 1.0)
        } else {
            Err(format!("{path} takes 0 or 1"))
        }
    };
    match parts.as_slice() {
        ["tempo"] => p.tempo = value,
        [MASTER, "gain_db"] => p.master.gain_db = value,
        [track, "gain_db"] => track_mut(p, track)?.gain_db = value,
        [track, "pan"] if *track != MASTER => track_mut(p, track)?.pan = value,
        [track, "mute"] if *track != MASTER => track_mut(p, track)?.mute = flag(value)?,
        [track, "instrument", param] => {
            let dev = track_mut(p, track)?
                .instrument
                .as_mut()
                .ok_or_else(|| format!("{track:?} has no instrument"))?;
            set_param(dev, param, value)?;
        }
        [track, "fx", id, "bypass"] => {
            let on = flag(value)?;
            fx_mut(p, track)?
                .iter_mut()
                .find(|d| d.id == *id)
                .ok_or_else(|| format!("{track:?} has no fx {id:?}"))?
                .bypass = on;
        }
        [track, "fx", id, param] => {
            let dev = fx_mut(p, track)?
                .iter_mut()
                .find(|d| d.id == *id)
                .ok_or_else(|| format!("{track:?} has no fx {id:?}"))?;
            set_param(dev, param, value)?;
        }
        _ => {
            return Err(format!(
                "unknown path {path:?}; use tempo, <track>.gain_db|pan|mute, <track>.instrument.<param>, <track>.fx.<id>.<param>|bypass"
            ));
        }
    }
    Ok(())
}

fn set_param(dev: &mut Device, param: &str, value: f64) -> Result<(), String> {
    if dev.kind.param(param).is_none() {
        let names: Vec<_> = dev.kind.params().iter().map(|s| s.name).collect();
        return Err(format!(
            "{:?} ({:?}) has no param {param:?}; params: {}",
            dev.id,
            dev.kind,
            names.join(", ")
        ));
    }
    dev.params.insert(param.into(), value);
    Ok(())
}

fn track_mut<'a>(p: &'a mut Project, id: &str) -> Result<&'a mut Track, String> {
    p.track_mut(id).ok_or_else(|| format!("no track {id:?}"))
}

fn clip_mut<'a>(p: &'a mut Project, track: &str, id: &str) -> Result<&'a mut Clip, String> {
    track_mut(p, track)?
        .clips
        .iter_mut()
        .find(|c| c.id() == id)
        .ok_or_else(|| format!("{track:?} has no clip {id:?}"))
}

fn fx_mut<'a>(p: &'a mut Project, owner: &str) -> Result<&'a mut Vec<Device>, String> {
    if owner == MASTER {
        Ok(&mut p.master.fx)
    } else {
        Ok(&mut track_mut(p, owner)?.fx)
    }
}

fn lanes_mut<'a>(p: &'a mut Project, owner: &str) -> Result<&'a mut Vec<Lane>, String> {
    if owner == MASTER {
        let Master { automation, .. } = &mut p.master;
        Ok(automation)
    } else {
        Ok(&mut track_mut(p, owner)?.automation)
    }
}

/// Removing a device takes its automation with it.
fn drop_lanes(p: &mut Project, owner: &str, prefix: &str) {
    if let Ok(lanes) = lanes_mut(p, owner) {
        lanes.retain(|l| !l.target.starts_with(prefix));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::{DigestOptions, digest};
    use crate::model::Kind;

    fn example() -> Project {
        let text = include_str!("../../../examples/nachtzug/pinna.ron");
        crate::io::parse(text).unwrap()
    }

    #[test]
    fn batch_applies_and_digest_shows_it() {
        let ops: Vec<Op> = serde_json::from_str(
            r#"[
                {"op": "add_fx", "track": "bass", "device": {"id": "hp", "kind": "HighPass", "params": {"freq": 40}}},
                {"op": "add_fx", "track": "bass", "device": {"id": "duck", "kind": "Compressor", "sidechain": "kick", "params": {"ratio": 8}}},
                {"op": "set", "path": "bass.instrument.cutoff", "value": 900},
                {"op": "set_automation", "track": "bass", "target": "gain_db", "points": [["9.1", 0], ["17.1", 3]]}
            ]"#,
        )
        .unwrap();
        let p = apply(&example(), &ops).unwrap();
        let bass = p.track("bass").unwrap();
        assert_eq!(bass.fx[1].sidechain.as_deref(), Some("kick"));
        let text = digest(
            &p,
            &DigestOptions {
                tracks: Some(vec!["bass".into()]),
                ..Default::default()
            },
        );
        assert!(
            text.contains("hp HighPass(freq=40) → duck Compressor(ratio=8) ←kick"),
            "{text}"
        );
        assert!(text.contains("auto gain_db: 9.1:0 17.1:3"), "{text}");
        assert_eq!(crate::io::parse(&crate::io::to_text(&p)).unwrap(), p);
    }

    #[test]
    fn failing_batch_changes_nothing_and_names_the_op() {
        let p = example();
        let ops = vec![
            Op::Set {
                path: "bass.gain_db".into(),
                value: 3.0,
            },
            Op::Set {
                path: "bass.instrument.nope".into(),
                value: 1.0,
            },
        ];
        match apply(&p, &ops) {
            Err(crate::Error::Op { index: 1, msg }) => {
                assert!(msg.contains("params: cutoff"), "{msg}")
            }
            other => panic!("{other:?}"),
        }
        let out_of_range = vec![Op::Set {
            path: "bass.instrument.cutoff".into(),
            value: 1e9,
        }];
        assert!(matches!(
            apply(&p, &out_of_range),
            Err(crate::Error::Invalid(_))
        ));
    }

    #[test]
    fn sidechain_cycles_are_rejected() {
        let mut duck = Device::new("duck", Kind::Compressor);
        duck.sidechain = Some("bass".into());
        let mut back = Device::new("back", Kind::Compressor);
        back.sidechain = Some("kick".into());
        let ops = vec![
            Op::AddFx {
                track: "kick".into(),
                device: duck,
                before: None,
            },
            Op::AddFx {
                track: "bass".into(),
                device: back,
                before: None,
            },
        ];
        match apply(&example(), &ops) {
            Err(crate::Error::Invalid(errs)) => assert!(
                errs.iter().any(|e| e.contains("sidechain cycle")),
                "{errs:?}"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn notes_past_the_loop_are_rejected() {
        let ops = vec![Op::SetNotes {
            track: "bass".into(),
            clip: "refrain".into(),
            notes: vec!["E1 8 1".parse().unwrap()],
        }];
        assert!(matches!(
            apply(&example(), &ops),
            Err(crate::Error::Invalid(_))
        ));
    }
}
