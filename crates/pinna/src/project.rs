//! Loading, digest, edits with an undo journal, and `init`. Every call reads
//! `pinna.ron` fresh from disk, so hand edits by agents are always respected.

use std::fs;
use std::path::{Path, PathBuf};

use pinna_core::digest::{DigestOptions, digest};
use pinna_core::io::{self, PROJECT_FILE};
use pinna_core::ops::{self, Op};
use pinna_core::range::parse_range;
use pinna_core::{Clip, Error, MASTER, Master, Project};

use crate::git::{self, git};

/// Revision name for the file on disk.
pub const WORKTREE: &str = "worktree";
const UNDO_DIR: &str = ".pinna/undo";
const UNDO_KEEP: usize = 50;

pub const EXAMPLES: [(&str, &str); 1] = [(
    "nachtzug",
    include_str!("../../../examples/nachtzug/pinna.ron"),
)];

pub fn load(root: &Path) -> Result<Project, String> {
    io::load(root).map_err(|e| format!("{PROJECT_FILE}: {e}"))
}

/// The project at a git revision, or the file on disk for [`WORKTREE`].
pub fn load_rev(root: &Path, rev: &str) -> Result<Project, String> {
    if rev == WORKTREE {
        return load(root);
    }
    git::check_rev(rev)?;
    let text = git(root, &["show", &format!("{rev}:./{PROJECT_FILE}")])?;
    let p = io::parse(&text).map_err(|e| format!("{PROJECT_FILE} at {rev}: {e}"))?;
    // Renders read audio from the folder, so a revision is only rendered faithfully
    // while its audio files match the folder.
    for t in &p.tracks {
        for c in &t.clips {
            if let Clip::Audio { file, .. } = c
                && git(root, &["diff", "--quiet", rev, "--", file]).is_err()
            {
                return Err(format!(
                    "audio {file} differs between {rev} and the project folder; rendering {rev} with other audio is not supported yet"
                ));
            }
        }
    }
    Ok(p)
}

/// Writes go to `.pinna/` and `pinna.ron`; a symlink there (e.g. committed in a cloned
/// repo) would redirect them outside the project.
pub fn check_writable(root: &Path) -> Result<(), String> {
    for name in [".pinna", PROJECT_FILE] {
        if fs::symlink_metadata(root.join(name)).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!(
                "{name} is a symlink; pinna only writes to real files"
            ));
        }
    }
    Ok(())
}

pub fn digest_text(
    root: &Path,
    range: Option<&str>,
    tracks: Option<Vec<String>>,
    notes: bool,
) -> Result<String, String> {
    let p = load(root)?;
    let range = range.map(|r| parse_range(&p, r)).transpose()?;
    if let Some(ts) = &tracks {
        for id in ts {
            if id != MASTER && p.track(id).is_none() {
                return Err(format!("no track {id:?}; tracks: {}", track_ids(&p)));
            }
        }
    }
    Ok(digest(
        &p,
        &DigestOptions {
            range,
            tracks,
            notes,
        },
    ))
}

pub fn track_ids(p: &Project) -> String {
    let ids: Vec<&str> = p.tracks.iter().map(|t| t.id.as_str()).collect();
    format!("{}, master", ids.join(", "))
}

/// Ops as JSON values, parsed one by one so an error names the op.
pub fn parse_ops(values: Vec<serde_json::Value>) -> Result<Vec<Op>, String> {
    values
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::from_value(v).map_err(|e| format!("nothing written: op {i}: {e}"))
        })
        .collect()
}

/// Apply a batch atomically, journal the previous text and echo the touched tracks
/// as they are in the re-loaded file.
pub fn edit(root: &Path, ops: &[Op]) -> Result<String, String> {
    if ops.is_empty() {
        return Err(
            r#"no ops given; pass a list like [{"op": "set", "path": "bass.gain_db", "value": 3}]"#
                .into(),
        );
    }
    check_writable(root)?;
    let path = root.join(PROJECT_FILE);
    let before = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let p = io::parse(&before)
        .map_err(|e| format!("{PROJECT_FILE}: {e}\nfix the file before editing"))?;
    let next = ops::apply(&p, ops).map_err(|e| match e {
        Error::Op { index, .. } => format!(
            "nothing written: {e}\nop {index} was: {}",
            serde_json::to_string(&ops[index]).expect("ops serialize")
        ),
        e => format!("nothing written: {e}"),
    })?;
    let entry = journal_push(root, &before)?;
    io::save(root, &next).map_err(|e| e.to_string())?;
    let written = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let after = after_file(&entry);
    fs::write(&after, written).map_err(|e| format!("{}: {e}", after.display()))?;
    let reloaded = load(root)?;
    if reloaded != next {
        return Err(format!(
            "{PROJECT_FILE} on disk differs from the edit just written (another writer?); run digest to see the current state"
        ));
    }
    let mut scopes: Vec<String> = Vec::new();
    for op in ops {
        let s = op.scope();
        if s != "project" && !scopes.iter().any(|x| x == s) {
            scopes.push(s.to_string());
        }
    }
    Ok(format!(
        "ok, {} ops\n{}",
        ops.len(),
        digest(
            &reloaded,
            &DigestOptions {
                tracks: Some(scopes),
                ..Default::default()
            }
        )
    ))
}

/// Restore the text from before the last edit and show the tracks that changed.
pub fn undo(root: &Path) -> Result<String, String> {
    let mut entries = journal(root)?;
    let Some(entry) = entries.pop() else {
        return Err(format!(
            "nothing to undo: the journal {UNDO_DIR} is empty (only edits made with pinna are journaled)"
        ));
    };
    check_writable(root)?;
    // Only undo over the text this edit wrote: anything else (a hand edit, accept,
    // checkout) would be lost. The journal then no longer applies and is dropped.
    let path = root.join(PROJECT_FILE);
    let on_disk = fs::read_to_string(&path).ok();
    if on_disk != fs::read_to_string(after_file(&entry)).ok() {
        let dir = root.join(UNDO_DIR);
        fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        return Err(format!(
            "nothing undone: {PROJECT_FILE} changed since the last pinna edit (hand edit, accept or checkout), so the undo journal was dropped; use git to go back"
        ));
    }
    let text = fs::read_to_string(&entry).map_err(|e| format!("{}: {e}", entry.display()))?;
    let current = load(root).ok();
    write_atomic(root, &text)?;
    for f in [after_file(&entry), entry] {
        fs::remove_file(&f).map_err(|e| format!("{}: {e}", f.display()))?;
    }
    let restored = load(root)?;
    let tracks = current.map(|cur| changed_tracks(&cur, &restored));
    Ok(format!(
        "undone, {} more undo steps\n{}",
        entries.len(),
        digest(
            &restored,
            &DigestOptions {
                tracks,
                ..Default::default()
            }
        )
    ))
}

fn changed_tracks(a: &Project, b: &Project) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for t in a.tracks.iter().chain(&b.tracks) {
        if a.track(&t.id) != b.track(&t.id) && !ids.contains(&t.id) {
            ids.push(t.id.clone());
        }
    }
    if a.master != b.master {
        ids.push(MASTER.into());
    }
    ids
}

/// Journal entries, oldest first.
fn journal(root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = root.join(UNDO_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let read = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries: Vec<(u64, PathBuf)> = read
        .filter_map(|e| {
            let path = e.ok()?.path();
            let n = path.file_stem()?.to_str()?.parse().ok()?;
            Some((n, path))
        })
        .collect();
    entries.sort();
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

/// The text an edit wrote, next to its journal entry. Its stem is not a number, so
/// [`journal`] skips it.
fn after_file(entry: &Path) -> PathBuf {
    let stem = entry.file_stem().unwrap_or_default().to_string_lossy();
    entry.with_file_name(format!("{stem}.after.ron"))
}

/// Journal the text from before an edit; returns the new entry.
fn journal_push(root: &Path, text: &str) -> Result<PathBuf, String> {
    let dir = root.join(UNDO_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let entries = journal(root)?;
    let next = entries
        .last()
        .and_then(|p| p.file_stem()?.to_str()?.parse::<u64>().ok())
        .map_or(1, |n| n + 1);
    let path = dir.join(format!("{next:06}.ron"));
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    let excess = (entries.len() + 1).saturating_sub(UNDO_KEEP);
    for old in &entries[..excess] {
        fs::remove_file(old).map_err(|e| format!("{}: {e}", old.display()))?;
        let _ = fs::remove_file(after_file(old));
    }
    Ok(path)
}

fn write_atomic(root: &Path, text: &str) -> Result<(), String> {
    let path = root.join(PROJECT_FILE);
    let tmp = root.join(format!(".{PROJECT_FILE}.tmp"));
    fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Create a project folder with `pinna.ron` (empty or an example), a `.gitignore`
/// for `.pinna/` and, outside an existing repository, a git repo with a first commit.
pub fn init(dir: &Path, example: Option<&str>) -> Result<String, String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(PROJECT_FILE);
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let name = match example {
        Some(ex) => {
            let (_, text) = EXAMPLES
                .iter()
                .find(|(n, _)| *n == ex)
                .ok_or_else(|| format!("unknown example {ex:?}"))?;
            fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            load(dir)?.name
        }
        None => {
            let name = dir
                .canonicalize()
                .ok()
                .and_then(|d| Some(d.file_name()?.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "Untitled".into());
            let p = Project {
                version: pinna_core::FORMAT_VERSION,
                name: name.clone(),
                tempo: 120.0,
                beats_per_bar: 4,
                sections: Vec::new(),
                tracks: Vec::new(),
                master: Master::default(),
            };
            io::save(dir, &p).map_err(|e| e.to_string())?;
            name
        }
    };
    let ignore = dir.join(".gitignore");
    let old = fs::read_to_string(&ignore).unwrap_or_default();
    if !old.lines().any(|l| l.trim() == ".pinna/") {
        let sep = if old.is_empty() || old.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        fs::write(&ignore, format!("{old}{sep}.pinna/\n"))
            .map_err(|e| format!("{}: {e}", ignore.display()))?;
    }
    let mut out = format!("created {} ({name})", path.display());
    if git(dir, &["rev-parse", "--is-inside-work-tree"]).is_ok() {
        out.push_str("\nalready inside a git repository: commit pinna.ron and .gitignore yourself");
    } else {
        git(dir, &["init", "-q", "-b", "main"])?;
        git(dir, &["add", "--", PROJECT_FILE, ".gitignore"])?;
        git(dir, &["commit", "-q", "-m", &format!("Start {name}")])?;
        out.push_str(&format!(
            "\ngit repository on main @ {}",
            git::short_hash(dir, "HEAD")?
        ));
    }
    Ok(out)
}
