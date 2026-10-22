//! Time ranges as agents write them: a section name, `"9.1-17.1"`, `"9-17"` (bars,
//! end exclusive) or `"all"`.

use crate::model::Project;
use crate::time::Pos;

/// Resolve a range to (start, end) ticks, end exclusive.
pub fn parse_range(p: &Project, s: &str) -> Result<(u64, u64), String> {
    let s = s.trim();
    let t = p.timing();
    if s.is_empty() || s.eq_ignore_ascii_case("all") {
        let end = p.end_tick();
        return if end > 0 {
            Ok((0, end))
        } else {
            Err("the project is empty".into())
        };
    }
    if let Some(sec) = p.section(s) {
        return Ok((t.tick(sec.from), t.tick(sec.to)));
    }
    let Some((a, b)) = s.split_once('-') else {
        let names: Vec<_> = p.sections.iter().map(|s| s.name.as_str()).collect();
        return Err(format!(
            "unknown range {s:?}; use all, a section ({}) or \"9.1-17.1\"",
            names.join(", ")
        ));
    };
    let a: Pos = a.parse()?;
    let b: Pos = b.parse()?;
    for pos in [a, b] {
        if pos.beat > p.beats_per_bar {
            return Err(format!(
                "position {pos}: beat beyond {} beats per bar",
                p.beats_per_bar
            ));
        }
    }
    let (from, to) = (t.tick(a), t.tick(b));
    if from >= to {
        return Err(format!("range {s:?} is empty"));
    }
    Ok((from, to))
}

/// Human-readable form of a tick range.
pub fn format_range(p: &Project, from: u64, to: u64) -> String {
    let t = p.timing();
    format!("{}-{}", t.pos(from), t.pos(to))
}
