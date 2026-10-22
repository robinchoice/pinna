//! The listening return channel: render a range and measure it, or compare two
//! states loudness-matched. Results are compact text for agents.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use pinna_analysis::{self as an, Loudness, Masking, StereoImage, THIRD_OCTAVE_HZ};
use pinna_core::Project;
use pinna_core::range::{format_range, parse_range};
use pinna_engine::{Render, RenderOptions, Stereo, audio_io};

use crate::project::{WORKTREE, load_rev, track_ids};

pub const RENDERS: &str = ".pinna/renders";

/// A resolved range with a label like `Refrain 9.1-17.1, 15.5 s`.
pub struct Span {
    pub from: u64,
    pub to: u64,
    pub label: String,
}

pub fn span(p: &Project, range: &str) -> Result<Span, String> {
    let (from, to) = parse_range(p, range)?;
    let t = p.timing();
    let ticks = format_range(p, from, to);
    let name = match p.section(range) {
        Some(s) => format!("{} ", s.name),
        None if range.trim().is_empty() || range.trim().eq_ignore_ascii_case("all") => {
            "all ".into()
        }
        None => String::new(),
    };
    Ok(Span {
        from,
        to,
        label: format!("{name}{ticks}, {:.1} s", t.seconds(to) - t.seconds(from)),
    })
}

/// Kick and bass track ids for masking. Explicit ids must exist; by default the
/// tracks `kick` and `bass` are used when both exist, otherwise masking is skipped.
fn masking_pair(
    p: &Project,
    kick: Option<&str>,
    bass: Option<&str>,
) -> Result<Option<(String, String)>, String> {
    let pick = |given: Option<&str>, default: &str| match given {
        Some(id) if p.track(id).is_some() => Ok(Some(id.to_string())),
        Some(id) => Err(format!("no track {id:?}; tracks: {}", track_ids(p))),
        None => Ok(p.track(default).map(|_| default.to_string())),
    };
    Ok(pick(kick, "kick")?.zip(pick(bass, "bass")?))
}

const NO_MASKING: &str =
    "masking: skipped, no tracks \"kick\" and \"bass\"; pass kick and bass track ids";

pub fn render(
    p: &Project,
    root: &Path,
    span: &Span,
    pair: Option<&(String, String)>,
) -> Result<Render, String> {
    let stems = pair
        .map(|(k, b)| vec![k.clone(), b.clone()])
        .unwrap_or_default();
    let opts = RenderOptions {
        stems,
        ..Default::default()
    };
    pinna_engine::render(p, root, span.from, span.to, &opts).map_err(|e| format!("render: {e}"))
}

fn pair(s: &Stereo) -> an::Pair<'_> {
    (&s.l, &s.r)
}

struct Measures {
    loudness: Loudness,
    bands: Vec<f64>,
    stereo: StereoImage,
    masking: Option<Masking>,
}

fn measure(r: &Render, kb: Option<&(String, String)>) -> Measures {
    let (mix, sr) = (pair(&r.mix), r.sample_rate);
    Measures {
        loudness: an::loudness(mix, sr),
        bands: an::third_octave(mix, sr),
        stereo: an::stereo(mix, sr),
        masking: kb.map(|(k, b)| an::masking(pair(&r.stems[k]), pair(&r.stems[b]), sr)),
    }
}

/// A number with `d` decimals, `-inf` for silence.
fn num(x: f64, d: usize) -> String {
    if x.is_infinite() {
        if x < 0.0 { "-inf" } else { "inf" }.into()
    } else {
        format!("{x:.d$}")
    }
}

fn signed(x: f64, d: usize) -> String {
    if x.is_infinite() {
        if x < 0.0 { "-inf" } else { "+inf" }.into()
    } else {
        format!("{x:+.d$}")
    }
}

/// b - a, with equal values (also two silences) giving 0.
fn delta(a: f64, b: f64) -> f64 {
    if a == b { 0.0 } else { b - a }
}

fn hz(f: f64) -> String {
    if f >= 1000.0 {
        format!("{}k", f / 1000.0)
    } else {
        format!("{f}")
    }
}

fn bands_line(values: impl Iterator<Item = String>) -> String {
    THIRD_OCTAVE_HZ
        .iter()
        .zip(values)
        .map(|(f, v)| format!("{}:{v}", hz(*f)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn worst_band(m: &Masking) -> String {
    m.bands
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map_or("-".into(), |(f, i)| format!("{} Hz {i:.2}", hz(*f)))
}

fn punch(m: &Masking) -> String {
    if m.kick_onsets == 0 {
        "n/a (no kick onsets)".into()
    } else {
        format!(
            "{} dB over {} onsets",
            num(m.kick_punch_db, 1),
            m.kick_onsets
        )
    }
}

pub fn analyze(
    root: &Path,
    range: &str,
    rev: &str,
    kick: Option<&str>,
    bass: Option<&str>,
    reference: Option<&str>,
) -> Result<String, String> {
    let p = load_rev(root, rev)?;
    let span = span(&p, range)?;
    let reference = reference
        .map(|rel| pinna_core::path::resolve(root, rel).map(|path| (rel, path)))
        .transpose()?;
    let kb = masking_pair(&p, kick, bass)?;
    let r = render(&p, root, &span, kb.as_ref())?;
    let m = measure(&r, kb.as_ref());
    let mut out = format!("range: {} · rev {rev}\n", span.label);
    let l = &m.loudness;
    if l.integrated_lufs.is_infinite() {
        out.push_str("loudness: silent\n");
    } else {
        let _ = writeln!(
            out,
            "loudness: {} LUFS integrated, short-term max {} LUFS, true peak {} dBTP, LRA {} LU",
            num(l.integrated_lufs, 1),
            num(l.short_term_max_lufs, 1),
            num(l.true_peak_dbtp, 1),
            num(l.lra, 1)
        );
    }
    match (&m.masking, &kb) {
        (Some(mk), Some((k, b))) => {
            let _ = writeln!(
                out,
                "masking {k}/{b}: index {:.2} (worst {}), kick punch {}",
                mk.index,
                worst_band(mk),
                punch(mk)
            );
        }
        _ => {
            let _ = writeln!(out, "{NO_MASKING}");
        }
    }
    let s = &m.stereo;
    let _ = writeln!(
        out,
        "stereo: correlation {:.2}, width {:.2}, mono loss {} LU",
        s.correlation,
        s.width,
        num(s.mono_loss_lu, 1)
    );
    let _ = writeln!(
        out,
        "third-octave dB: {}",
        bands_line(m.bands.iter().map(|v| num(*v, 1)))
    );
    if let Some((rel, path)) = reference {
        let audio =
            audio_io::read_audio(&path, r.sample_rate).map_err(|e| format!("{rel}: {e}"))?;
        let c = an::compare_reference(pair(&r.mix), pair(&audio), r.sample_rate);
        let _ = writeln!(
            out,
            "reference {rel} (mix minus reference): loudness {} LU, crest {} dB, width {}",
            signed(c.lufs_diff, 1),
            signed(c.crest_diff_db, 1),
            signed(c.width_diff, 2)
        );
        let _ = writeln!(
            out,
            "reference band diff dB (loudness matched): {}",
            bands_line(c.band_diff_db.iter().map(|v| signed(*v, 1)))
        );
    }
    Ok(out)
}

pub struct Comparison {
    pub a: String,
    pub b: String,
    pub label: String,
    /// Gain applied to B to match A's integrated loudness, dB.
    pub gain_db: f64,
    /// `metric: A -> B (delta)` lines.
    pub rows: Vec<String>,
    pub notes: Vec<String>,
    pub files: [PathBuf; 2],
}

impl Comparison {
    pub fn text(&self) -> String {
        let mut out = format!(
            "compare {}: A = {}, B = {}\nB gain-matched by {} dB; wav: {}, {}\n",
            self.label,
            self.a,
            self.b,
            signed(self.gain_db, 1),
            self.files[0].display(),
            self.files[1].display()
        );
        for line in self.notes.iter().chain(&self.rows) {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

fn row(name: &str, a: f64, b: f64, d: usize) -> String {
    format!(
        "{name}: {} -> {} ({})",
        num(a, d),
        num(b, d),
        signed(delta(a, b), d)
    )
}

/// File name for a revision's render: anything but `[A-Za-z0-9._-]` becomes `_`.
fn render_name(rev: &str) -> String {
    rev.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn scaled(s: &Stereo, gain_db: f64) -> Stereo {
    let k = 10f64.powf(gain_db / 20.0) as f32;
    Stereo {
        l: s.l.iter().map(|x| x * k).collect(),
        r: s.r.iter().map(|x| x * k).collect(),
    }
}

/// Render A and B over the same range (resolved on A), match B's loudness to A and
/// compare. Writes both renders to `out_dir`, B gain-matched.
pub fn compare(
    root: &Path,
    a: &str,
    b: &str,
    range: &str,
    kick: Option<&str>,
    bass: Option<&str>,
    out_dir: &Path,
) -> Result<Comparison, String> {
    crate::project::check_writable(root)?;
    let pa = load_rev(root, a)?;
    let pb = load_rev(root, b)?;
    let span = span(&pa, range)?;
    let mut notes = Vec::new();
    let kb = match (
        masking_pair(&pa, kick, bass)?,
        masking_pair(&pb, kick, bass)?,
    ) {
        (Some(x), Some(y)) if x == y => Some(x),
        _ => {
            notes.push(NO_MASKING.to_string());
            None
        }
    };
    let ra = render(&pa, root, &span, kb.as_ref())?;
    let rb = render(&pb, root, &span, kb.as_ref())?;
    let ma = measure(&ra, kb.as_ref());
    let mb = measure(&rb, kb.as_ref());
    let mut gain_db = an::match_gain_db(&ma.loudness, &mb.loudness);
    if !(ma.loudness.integrated_lufs.is_finite() && mb.loudness.integrated_lufs.is_finite()) {
        notes.push("loudness match skipped: A or B is silent".into());
        gain_db = 0.0;
    }
    let b_matched = scaled(&rb.mix, gain_db);
    let b_bands = an::third_octave(pair(&b_matched), rb.sample_rate);

    let (la, lb) = (&ma.loudness, &mb.loudness);
    let mut rows = vec![
        row("integrated LUFS", la.integrated_lufs, lb.integrated_lufs, 1),
        row(
            "short-term max LUFS",
            la.short_term_max_lufs,
            lb.short_term_max_lufs,
            1,
        ),
        row("true peak dBTP", la.true_peak_dbtp, lb.true_peak_dbtp, 1),
        row("LRA LU", la.lra, lb.lra, 1),
    ];
    if let (Some(x), Some(y), Some((k, bs))) = (&ma.masking, &mb.masking, &kb) {
        rows.push(row(&format!("masking index {k}/{bs}"), x.index, y.index, 2));
        rows.push(format!(
            "masking worst band: {} -> {}",
            worst_band(x),
            worst_band(y)
        ));
        rows.push(row("kick punch dB", x.kick_punch_db, y.kick_punch_db, 1));
    }
    let (sa, sb) = (&ma.stereo, &mb.stereo);
    rows.push(row("correlation", sa.correlation, sb.correlation, 2));
    rows.push(row("width", sa.width, sb.width, 2));
    rows.push(row("mono loss LU", sa.mono_loss_lu, sb.mono_loss_lu, 1));
    rows.push(format!(
        "third-octave B-A dB (matched): {}",
        bands_line(
            ma.bands
                .iter()
                .zip(&b_bands)
                .map(|(x, y)| signed(delta(*x, *y), 1))
        )
    ));

    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let name_a = render_name(a);
    let mut name_b = render_name(b);
    if name_b == name_a {
        name_b.push_str("-b");
    }
    let files = [
        out_dir.join(format!("{name_a}.wav")),
        out_dir.join(format!("{name_b}.wav")),
    ];
    audio_io::write_wav(&files[0], &ra.mix, ra.sample_rate).map_err(|e| e.to_string())?;
    audio_io::write_wav(&files[1], &b_matched, rb.sample_rate).map_err(|e| e.to_string())?;
    Ok(Comparison {
        a: a.into(),
        b: b.into(),
        label: span.label,
        gain_db,
        rows,
        notes,
        files,
    })
}

/// Render a range of the worktree project to a WAV file.
pub fn render_to(root: &Path, range: &str, out: &Path) -> Result<String, String> {
    let p = load_rev(root, WORKTREE)?;
    let span = span(&p, range)?;
    let r = render(&p, root, &span, None)?;
    audio_io::write_wav(out, &r.mix, r.sample_rate).map_err(|e| e.to_string())?;
    Ok(format!(
        "wrote {}: {}, {} Hz",
        out.display(),
        span.label,
        r.sample_rate
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_silence() {
        assert_eq!(num(f64::NEG_INFINITY, 1), "-inf");
        assert_eq!(num(-14.26, 1), "-14.3");
        assert_eq!(signed(0.25, 2), "+0.25");
        assert_eq!(signed(f64::INFINITY, 1), "+inf");
        assert_eq!(signed(f64::NEG_INFINITY, 1), "-inf");
        assert_eq!(delta(f64::NEG_INFINITY, f64::NEG_INFINITY), 0.0);
        assert_eq!(
            row("masking index", 0.62, 0.31, 2),
            "masking index: 0.62 -> 0.31 (-0.31)"
        );
        assert_eq!(hz(31.5), "31.5");
        assert_eq!(hz(12_500.0), "12.5k");
        assert_eq!(render_name("HEAD~1"), "HEAD_1");
    }

    #[test]
    fn spans_name_sections() {
        let p = pinna_core::io::parse(crate::project::EXAMPLES[0].1).unwrap();
        assert_eq!(
            span(&p, "Refrain").unwrap().label,
            "Refrain 9.1-17.1, 15.5 s"
        );
        assert_eq!(span(&p, "9-17").unwrap().label, "9.1-17.1, 15.5 s");
        assert!(span(&p, "Bridge").is_err());
    }

    #[test]
    fn masking_pair_defaults_and_checks() {
        let p = pinna_core::io::parse(crate::project::EXAMPLES[0].1).unwrap();
        assert_eq!(
            masking_pair(&p, None, None).unwrap(),
            Some(("kick".into(), "bass".into()))
        );
        assert!(masking_pair(&p, Some("drums"), None).is_err());
        assert_eq!(
            masking_pair(&p, None, Some("pad")).unwrap().unwrap().1,
            "pad"
        );
    }
}
