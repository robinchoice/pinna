//! What a proposal changes, per track, in the words of `pinna.ron`.

use pinna_core::{Device, MASTER, Project};

/// Changes from `a` to `b`: `(track id or "master", description)`.
pub fn changes(a: &Project, b: &Project) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut push = |id: &str, what: String| out.push((id.to_string(), what));
    if a.tempo != b.tempo {
        push(MASTER, format!("tempo {} → {}", num(a.tempo), num(b.tempo)));
    }
    let ids = a.tracks.iter().chain(&b.tracks).map(|t| t.id.as_str());
    let mut seen = Vec::new();
    for id in ids {
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let (ta, tb) = match (a.track(id), b.track(id)) {
            (Some(x), Some(y)) => (x, y),
            (None, Some(_)) => {
                push(id, "+ track".into());
                continue;
            }
            (Some(_), None) => {
                push(id, "− track".into());
                continue;
            }
            (None, None) => continue,
        };
        if ta == tb {
            continue;
        }
        for (name, x, y) in [("gain_db", ta.gain_db, tb.gain_db), ("pan", ta.pan, tb.pan)] {
            if x != y {
                push(id, format!("{name} {} → {}", num(x), num(y)));
            }
        }
        if ta.mute != tb.mute {
            push(id, if tb.mute { "mute" } else { "unmute" }.into());
        }
        match (&ta.instrument, &tb.instrument) {
            (Some(x), Some(y)) if x.kind == y.kind => device(id, "instrument", x, y, &mut push),
            (x, y) if x != y => push(id, "instrument".into()),
            _ => {}
        }
        chain(id, &ta.fx, &tb.fx, &mut push);
        for c in &tb.clips {
            if !ta.clips.contains(c) {
                push(id, format!("clip {}", c.id()));
            }
        }
        for c in &ta.clips {
            if !tb.clips.iter().any(|d| d.id() == c.id()) {
                push(id, format!("− clip {}", c.id()));
            }
        }
        lanes(id, &ta.automation, &tb.automation, &mut push);
    }
    let (ma, mb) = (&a.master, &b.master);
    if ma.gain_db != mb.gain_db {
        push(
            MASTER,
            format!("gain_db {} → {}", num(ma.gain_db), num(mb.gain_db)),
        );
    }
    chain(MASTER, &ma.fx, &mb.fx, &mut push);
    lanes(MASTER, &ma.automation, &mb.automation, &mut push);
    out
}

fn chain(id: &str, a: &[Device], b: &[Device], push: &mut impl FnMut(&str, String)) {
    for d in b {
        match a.iter().find(|x| x.id == d.id) {
            Some(x) if x.kind == d.kind => device(id, &format!("fx.{}", d.id), x, d, push),
            Some(_) => push(id, format!("fx.{} → {:?}", d.id, d.kind)),
            None => {
                let params: Vec<String> = d
                    .params
                    .iter()
                    .map(|(k, v)| format!("{k} {}", num(*v)))
                    .collect();
                push(
                    id,
                    format!("+ {:?} {}", d.kind, params.join(", "))
                        .trim()
                        .into(),
                );
            }
        }
    }
    for d in a {
        if !b.iter().any(|x| x.id == d.id) {
            push(id, format!("− fx.{}", d.id));
        }
    }
}

fn device(id: &str, at: &str, a: &Device, b: &Device, push: &mut impl FnMut(&str, String)) {
    for spec in b.kind.params() {
        let (x, y) = (a.param(spec.name), b.param(spec.name));
        if x != y {
            push(
                id,
                format!("{at}.{} {} → {} {}", spec.name, num(x), num(y), spec.unit)
                    .trim()
                    .into(),
            );
        }
    }
    if a.sidechain != b.sidechain {
        let src = b.sidechain.as_deref().unwrap_or("–");
        push(id, format!("{at} sidechain ← {src}"));
    }
    if a.bypass != b.bypass {
        push(id, format!("{at} bypass {}", b.bypass));
    }
}

fn lanes(
    id: &str,
    a: &[pinna_core::Lane],
    b: &[pinna_core::Lane],
    push: &mut impl FnMut(&str, String),
) {
    for l in b {
        if !a.contains(l) {
            push(id, format!("automation {}", l.target));
        }
    }
    for l in a {
        if !b.iter().any(|x| x.target == l.target) {
            push(id, format!("− automation {}", l.target));
        }
    }
}

fn num(x: f64) -> String {
    let s = format!("{x:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.replace('-', "−")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_param_fx_and_master_changes() {
        let a =
            pinna_core::io::parse(include_str!("../../../examples/nachtzug/pinna.ron")).unwrap();
        assert!(changes(&a, &a).is_empty());
        let mut b = a.clone();
        let bass = b.track_mut("bass").unwrap();
        bass.gain_db = -3.0;
        bass.fx
            .push(Device::new("hp", pinna_core::Kind::HighPass).with("freq", 40.0));
        b.master.gain_db = 2.0;
        let got = changes(&a, &b);
        assert_eq!(
            got,
            [
                ("bass".to_string(), "gain_db 0 → −3".to_string()),
                ("bass".to_string(), "+ HighPass freq 40".to_string()),
                ("master".to_string(), "gain_db −6 → 2".to_string()),
            ]
        );
    }
}
