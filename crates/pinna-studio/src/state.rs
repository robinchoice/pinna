//! The project as the studio shows it, reloaded in the background whenever
//! `pinna.ron`, HEAD or a proposal branch changes (agents and Git work beside us).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use pinna::git::git;
use pinna::project;
use pinna::proposal::{self, Info};
use pinna_core::Project;
use pinna_core::io::PROJECT_FILE;
use pinna_core::range::parse_range;

use crate::diff::changes;

/// How many accepted proposals the column keeps showing.
const ACCEPTED_SHOWN: usize = 3;

pub struct Prop {
    pub info: Info,
    /// What it changes, per track id or `master`.
    pub changes: Vec<(String, String)>,
    /// Where it acts, in ticks.
    pub range: (u64, u64),
    /// The project after accepting it (open proposals only).
    pub preview: Option<Project>,
}

pub struct Loaded {
    pub root: PathBuf,
    pub project: Project,
    pub branch: String,
    pub head: String,
    pub open: Vec<Prop>,
    pub accepted: Vec<Prop>,
    /// Problems that did not stop loading, e.g. a proposal that does not render.
    pub warnings: Vec<String>,
}

pub struct Watcher {
    pub rx: Receiver<Result<Loaded, String>>,
    poke: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Watcher {
    /// Load `root` now and again on every change. `repaint` wakes the UI.
    pub fn start(root: PathBuf, repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let poke = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (poke.clone(), stop.clone());
        thread::spawn(move || {
            let mut last = String::new();
            while !s.load(Ordering::Relaxed) {
                let key = key(&root);
                if key != last || p.swap(false, Ordering::Relaxed) {
                    last = key;
                    if tx.send(load(&root)).is_err() {
                        return;
                    }
                    repaint();
                }
                thread::sleep(Duration::from_millis(700));
            }
        });
        Self { rx, poke, stop }
    }

    /// Reload at the next tick even if nothing seems changed.
    pub fn poke(&self) {
        self.poke.store(true, Ordering::Relaxed);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Changes whenever something the studio shows may have changed.
fn key(root: &Path) -> String {
    let file = std::fs::metadata(root.join(PROJECT_FILE))
        .map(|m| format!("{:?} {}", m.modified().ok(), m.len()))
        .unwrap_or_default();
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/heads/proposal/",
        ],
    )
    .unwrap_or_default();
    let head = git(root, &["rev-parse", "HEAD"]).unwrap_or_default();
    format!("{file}|{head}|{refs}")
}

fn load(root: &Path) -> Result<Loaded, String> {
    let project = project::load(root)?;
    let in_git = git(root, &["rev-parse", "--is-inside-work-tree"]).is_ok();
    let mut warnings = Vec::new();
    let (mut open, mut accepted) = (Vec::new(), Vec::new());
    let (mut branch, mut head) = (String::new(), String::new());
    if in_git {
        branch = git(root, &["branch", "--show-current"])
            .unwrap_or_default()
            .trim()
            .to_string();
        head = pinna::git::short_hash(root, "HEAD").unwrap_or_default();
        for info in proposal::open(root).unwrap_or_default() {
            match proposal::preview(root, &info.name) {
                Ok(b) => open.push(prop(&project, info, &project, Some(b))),
                Err(e) => warnings.push(format!("{}: {e}", info.name)),
            }
        }
        for info in proposal::accepted(root, ACCEPTED_SHOWN).unwrap_or_default() {
            let before = project::load_rev(root, &format!("{}^", info.hash));
            let after = project::load_rev(root, &info.hash);
            if let (Ok(a), Ok(b)) = (before, after) {
                accepted.push(prop(&project, info, &a, Some(b)).without_preview());
            }
        }
    }
    Ok(Loaded {
        root: root.to_path_buf(),
        project,
        branch,
        head,
        open,
        accepted,
        warnings,
    })
}

fn prop(song: &Project, info: Info, a: &Project, b: Option<Project>) -> Prop {
    let changes = b.as_ref().map(|b| changes(a, b)).unwrap_or_default();
    let range = parse_range(song, info.range()).unwrap_or((0, song.end_tick()));
    Prop {
        info,
        changes,
        range,
        preview: b,
    }
}

impl Prop {
    fn without_preview(mut self) -> Self {
        self.preview = None;
        self
    }

    /// Track ids (or `master`) it changes, in order.
    pub fn tracks(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        for (id, _) in &self.changes {
            if !ids.contains(&id.as_str()) {
                ids.push(id);
            }
        }
        ids
    }
}
