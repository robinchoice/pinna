//! Proposals: an agent's change as a branch `proposal/<slug>` with one commit that
//! carries the measured evidence. Accepting and discarding is the human's step.

use std::path::Path;

use pinna_core::io::PROJECT_FILE;

use crate::git::{self, git};
use crate::measure::{self, RENDERS};
use crate::project::WORKTREE;

const PREFIX: &str = "proposal/";
const EVIDENCE: &str = "Pinna-Evidence";

/// Compare HEAD with the worktree and commit the change as a proposal branch.
pub fn propose(
    root: &Path,
    title: &str,
    why: &str,
    range: &str,
    kick: Option<&str>,
    bass: Option<&str>,
) -> Result<String, String> {
    if title.trim().is_empty() || title.contains('\n') || why.trim().is_empty() {
        return Err("title (one line) and why must not be empty".into());
    }
    let paths = changed_paths(root)?;
    let base = git::short_hash(root, "HEAD")?;
    let c = measure::compare(
        root,
        "HEAD",
        WORKTREE,
        range,
        kick,
        bass,
        &root.join(RENDERS),
    )?;
    let mut evidence = format!("{EVIDENCE} ({}, A = {base}, B = proposal):", c.label);
    for line in c.notes.iter().chain(&c.rows) {
        evidence.push('\n');
        evidence.push_str(line);
    }
    let (branch, hash) = commit(root, title, why, &evidence, &paths)?;
    let name = &branch[PREFIX.len()..];
    Ok(format!(
        "proposed {branch} @ {hash} (base {base}); the worktree is back at the base state\n{evidence}\nthe human decides: pinna accept {name} | pinna discard {name}\n"
    ))
}

/// Changed paths relative to the project folder. Only `pinna.ron` and `audio/` may
/// differ from HEAD; `.pinna/` is Pinna's scratch space and ignored.
pub fn changed_paths(root: &Path) -> Result<Vec<String>, String> {
    let prefix = git(root, &["rev-parse", "--show-prefix"])
        .map_err(|e| format!("propose needs a git repository: {e}"))?
        .trim_end_matches('\n')
        .to_string();
    // Porcelain paths are relative to the repository root.
    let status = git(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let mut entries = status.split('\0').filter(|e| !e.is_empty());
    let (mut paths, mut other) = (Vec::new(), Vec::new());
    while let Some(entry) = entries.next() {
        let (code, path) = entry.split_at(3);
        let mut all = vec![path];
        if code.starts_with(['R', 'C']) {
            all.extend(entries.next());
        }
        for path in all {
            match path.strip_prefix(prefix.as_str()) {
                Some(rel) if rel == PROJECT_FILE || rel.starts_with("audio/") => {
                    paths.push(rel.to_string())
                }
                Some(rel) if rel.starts_with(".pinna/") => {}
                _ => other.push(path.to_string()),
            }
        }
    }
    if !other.is_empty() {
        return Err(format!(
            "a proposal only carries {PROJECT_FILE} and audio/; commit or remove these changes first: {}",
            other.join(", ")
        ));
    }
    if paths.is_empty() {
        return Err(format!(
            "nothing to propose: {PROJECT_FILE} and audio/ match HEAD; edit first"
        ));
    }
    Ok(paths)
}

/// `proposal/<slug>` from a title, ASCII only.
fn slug(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        let t = match c {
            'ä' => "ae",
            'ö' => "oe",
            'ü' => "ue",
            'ß' => "ss",
            c if c.is_ascii_alphanumeric() => {
                s.push(c);
                continue;
            }
            _ if s.is_empty() || s.ends_with('-') => continue,
            _ => "-",
        };
        s.push_str(t);
    }
    s.truncate(48);
    let s = s.trim_matches('-');
    if s.is_empty() { "change" } else { s }.to_string()
}

fn branch_exists(root: &Path, branch: &str) -> bool {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// Commit `paths` on a new proposal branch from HEAD and switch back to the base
/// branch, which returns the worktree to the base state. Returns branch and hash.
pub fn commit(
    root: &Path,
    title: &str,
    why: &str,
    evidence: &str,
    paths: &[String],
) -> Result<(String, String), String> {
    let base = git(root, &["symbolic-ref", "--short", "-q", "HEAD"])
        .map_err(|_| "propose needs a checked-out branch, HEAD is detached".to_string())?
        .trim()
        .to_string();
    let slug = slug(title);
    let branch = (1..)
        .map(|n| match n {
            1 => format!("{PREFIX}{slug}"),
            n => format!("{PREFIX}{slug}-{n}"),
        })
        .find(|b| !branch_exists(root, b))
        .expect("some suffix is free");
    git(root, &["switch", "-q", "-c", &branch])?;
    let message = format!("{title}\n\n{why}\n\n{evidence}");
    let committed = git(root, &with_paths(&["add", "-A", "--"], paths))
        .and_then(|_| git(root, &["commit", "-q", "-m", &message]));
    if let Err(e) = committed {
        // Back to where we started: changes unstaged on the base branch, no proposal.
        let _ = git(root, &with_paths(&["reset", "-q", "--"], paths));
        let _ = git(root, &["switch", "-q", &base]);
        let _ = git(root, &["branch", "-q", "-D", &branch]);
        return Err(format!("commit failed, nothing proposed: {e}"));
    }
    git(root, &["switch", "-q", &base])?;
    let hash = git::short_hash(root, &branch)?;
    Ok((branch, hash))
}

fn with_paths<'a>(args: &[&'a str], paths: &'a [String]) -> Vec<&'a str> {
    let mut v = args.to_vec();
    v.extend(paths.iter().map(String::as_str));
    v
}

fn open_branches(root: &Path) -> Result<Vec<String>, String> {
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/proposal/",
        ],
    )?;
    Ok(refs.lines().map(str::to_string).collect())
}

/// Open proposals with subject, short hash and evidence.
pub fn list(root: &Path) -> Result<String, String> {
    let mut out = String::new();
    for branch in open_branches(root)? {
        let log = git(root, &["log", "-1", "--format=%h %s%n%b", &branch])?;
        let (head, body) = log.split_once('\n').unwrap_or((&log, ""));
        out.push_str(&format!("{branch} {head}\n"));
        if let Some(i) = body.find(EVIDENCE) {
            for line in body[i..].lines().filter(|l| !l.is_empty()) {
                out.push_str(&format!("  {line}\n"));
            }
        }
    }
    if out.is_empty() {
        out.push_str("no open proposals\n");
    }
    Ok(out)
}

/// A proposal as Pinna Studio shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// Branch name without `proposal/`; empty once accepted.
    pub name: String,
    /// Short hash of the proposal commit.
    pub hash: String,
    pub title: String,
    pub why: String,
    /// Range the evidence was measured over, e.g. `Refrain 9.1-17.1, 15.5 s`.
    pub label: String,
    /// `metric: A -> B (delta)` lines and notes, as committed.
    pub evidence: Vec<String>,
}

impl Info {
    /// Ticks range of the label, like `9.1-17.1`.
    pub fn range(&self) -> &str {
        let head = self.label.split(", ").next().unwrap_or_default();
        head.rsplit(' ').next().unwrap_or_default()
    }
}

/// Split a proposal commit message (`%h` line, then `%B`). None without evidence.
fn parse_info(name: &str, log: &str) -> Option<Info> {
    let (hash, message) = log.split_once('\n')?;
    let at = message.find(EVIDENCE)?;
    let (head, evidence) = message.split_at(at);
    let (title, why) = head.split_once('\n').unwrap_or((head, ""));
    let mut lines = evidence.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next()?;
    let label = header
        .strip_prefix(EVIDENCE)
        .and_then(|h| h.trim_start().strip_prefix('('))
        .and_then(|h| h.rsplit_once(", A = "))
        .map_or("", |(label, _)| label);
    Some(Info {
        name: name.to_string(),
        hash: hash.trim().to_string(),
        title: title.trim().to_string(),
        why: why.trim().to_string(),
        label: label.to_string(),
        evidence: lines.map(|l| l.trim().to_string()).collect(),
    })
}

/// Open proposals, oldest branch name first.
pub fn open(root: &Path) -> Result<Vec<Info>, String> {
    let mut out = Vec::new();
    for branch in open_branches(root)? {
        let log = git(root, &["log", "-1", "--format=%h%n%B", &branch])?;
        out.extend(parse_info(&branch[PREFIX.len()..], &log));
    }
    Ok(out)
}

/// Accepted proposals in the history of HEAD, newest first, at most `max`.
pub fn accepted(root: &Path, max: usize) -> Result<Vec<Info>, String> {
    let log = git(
        root,
        &[
            "log",
            "-n",
            "200",
            "--no-merges",
            "--format=%h%n%B%x00",
            "HEAD",
        ],
    )?;
    Ok(log
        .split('\0')
        .filter_map(|entry| parse_info("", entry.trim_start_matches('\n')))
        .take(max)
        .collect())
}

/// The project as accepting the proposal would leave it: HEAD merged with the
/// proposal. Without a clean merge, the proposal branch as committed.
pub fn preview(root: &Path, name: &str) -> Result<pinna_core::Project, String> {
    let branch = proposal_branch(root, name)?;
    let merged = git(root, &["merge-tree", "--write-tree", "HEAD", &branch])
        .ok()
        .and_then(|out| out.lines().next().map(str::to_string));
    match merged {
        Some(tree) => crate::project::load_rev(root, &tree),
        None => crate::project::load_rev(root, &branch),
    }
}

fn proposal_branch(root: &Path, name: &str) -> Result<String, String> {
    let name = name.strip_prefix(PREFIX).unwrap_or(name);
    git::check_rev(name)?;
    let branch = format!("{PREFIX}{name}");
    if branch_exists(root, &branch) {
        Ok(branch)
    } else {
        Err(format!(
            "no proposal {name:?}; open: {}",
            open_branches(root)?.join(", ")
        ))
    }
}

/// Merge a proposal (fast-forward if possible, else a merge commit) and delete it.
pub fn accept(root: &Path, name: &str) -> Result<String, String> {
    let branch = proposal_branch(root, name)?;
    git(root, &["symbolic-ref", "-q", "HEAD"])
        .map_err(|_| "accept needs a checked-out branch, HEAD is detached".to_string())?;
    let how = if git(root, &["merge", "-q", "--ff-only", &branch]).is_ok() {
        "fast-forward"
    } else {
        git(root, &["merge", "-q", "--no-ff", "--no-edit", &branch]).map_err(|e| {
            let _ = git(root, &["merge", "--abort"]);
            format!("merge failed and was aborted, {branch} is kept: {e}")
        })?;
        "merge commit"
    };
    git(root, &["branch", "-q", "-d", &branch])?;
    Ok(format!(
        "accepted {branch} ({how}), HEAD {}\n",
        git::short_hash(root, "HEAD")?
    ))
}

pub fn discard(root: &Path, name: &str) -> Result<String, String> {
    let branch = proposal_branch(root, name)?;
    let hash = git::short_hash(root, &branch)?;
    git(root, &["branch", "-q", "-D", &branch])?;
    Ok(format!("discarded {branch} (was {hash})\n"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::project::EXAMPLES;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(PROJECT_FILE), EXAMPLES[0].1).unwrap();
        fs::write(root.join(".gitignore"), ".pinna/\n").unwrap();
        git(root, &["init", "-q", "-b", "main"]).unwrap();
        git(root, &["add", "--", PROJECT_FILE, ".gitignore"]).unwrap();
        git(root, &["commit", "-q", "-m", "start"]).unwrap();
        dir
    }

    fn change(root: &Path) {
        let text = EXAMPLES[0].1.replace("tempo: 124.0", "tempo: 126.0");
        fs::write(root.join(PROJECT_FILE), text).unwrap();
    }

    #[test]
    fn slugs() {
        assert_eq!(
            slug("Duck the bass under the kick!"),
            "duck-the-bass-under-the-kick"
        );
        assert_eq!(slug("Bass öffnen"), "bass-oeffnen");
        assert_eq!(slug("  ---  "), "change");
    }

    #[test]
    fn changes_must_be_project_or_audio() {
        let dir = repo();
        let root = dir.path();
        let err = changed_paths(root).unwrap_err();
        assert!(err.contains("nothing to propose"), "{err}");

        change(root);
        fs::create_dir_all(root.join(".pinna/renders")).unwrap();
        fs::write(root.join(".pinna/renders/HEAD.wav"), b"x").unwrap();
        fs::create_dir_all(root.join("audio")).unwrap();
        fs::write(root.join("audio/vox.wav"), b"x").unwrap();
        let mut paths = changed_paths(root).unwrap();
        paths.sort();
        assert_eq!(paths, ["audio/vox.wav", "pinna.ron"]);

        fs::write(root.join("notes.txt"), b"x").unwrap();
        let err = changed_paths(root).unwrap_err();
        assert!(err.contains("notes.txt") && !err.contains("vox"), "{err}");
    }

    #[test]
    fn commit_creates_branch_and_returns_to_base() {
        let dir = repo();
        let root = dir.path();
        let base = git::short_hash(root, "HEAD").unwrap();
        change(root);
        fs::create_dir_all(root.join("audio")).unwrap();
        fs::write(root.join("audio/vox.wav"), b"x").unwrap();
        let evidence = format!(
            "{EVIDENCE} (Refrain 9.1-17.1, 15.5 s, A = {base}, B = proposal):\nmasking index kick/bass: 0.62 -> 0.31 (-0.31)"
        );
        let paths = changed_paths(root).unwrap();
        let (branch, hash) = commit(
            root,
            "Duck the bass",
            "The bass covers the kick.",
            &evidence,
            &paths,
        )
        .unwrap();
        assert_eq!(branch, "proposal/duck-the-bass");
        assert_eq!(git::short_hash(root, &branch).unwrap(), hash);

        // Back on main with the base state and a clean worktree.
        assert_eq!(
            git(root, &["branch", "--show-current"]).unwrap().trim(),
            "main"
        );
        assert_eq!(
            fs::read_to_string(root.join(PROJECT_FILE)).unwrap(),
            EXAMPLES[0].1
        );
        assert!(!root.join("audio/vox.wav").exists());
        assert_eq!(git(root, &["status", "--porcelain"]).unwrap(), "");

        let msg = git(root, &["log", "-1", "--format=%B", &branch]).unwrap();
        assert!(
            msg.starts_with(
                "Duck the bass\n\nThe bass covers the kick.\n\nPinna-Evidence (Refrain"
            ),
            "{msg}"
        );
        assert!(
            msg.contains("masking index kick/bass: 0.62 -> 0.31"),
            "{msg}"
        );
        let files = git(root, &["show", "--name-only", "--format=", &branch]).unwrap();
        assert_eq!(files, "audio/vox.wav\npinna.ron\n");
        assert_eq!(
            git(root, &["rev-parse", &format!("{branch}~1")]).unwrap(),
            git(root, &["rev-parse", "HEAD"]).unwrap()
        );

        // The same title again gets a suffix.
        change(root);
        let paths = changed_paths(root).unwrap();
        let (second, _) = commit(root, "Duck the bass", "again", &evidence, &paths).unwrap();
        assert_eq!(second, "proposal/duck-the-bass-2");

        let listed = list(root).unwrap();
        assert!(
            listed.contains("proposal/duck-the-bass ") && listed.contains("  masking index"),
            "{listed}"
        );
    }

    #[test]
    fn failed_commit_leaves_no_branch() {
        let dir = repo();
        let root = dir.path();
        change(root);
        let hook = root.join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\necho hook says no >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let paths = changed_paths(root).unwrap();
        let err = commit(root, "Tempo", "faster", "e", &paths).unwrap_err();
        assert!(err.contains("hook says no"), "{err}");
        assert_eq!(
            git(root, &["branch", "--show-current"]).unwrap().trim(),
            "main"
        );
        assert_eq!(open_branches(root).unwrap(), Vec::<String>::new());
        assert!(
            fs::read_to_string(root.join(PROJECT_FILE))
                .unwrap()
                .contains("126.0")
        );
        assert_eq!(git(root, &["diff", "--cached", "--name-only"]).unwrap(), "");
    }

    #[test]
    fn info_lists_open_and_accepted_and_previews_the_merge() {
        let dir = repo();
        let root = dir.path();
        let evidence = |label: &str| {
            format!(
                "{EVIDENCE} ({label}, A = abc1234, B = proposal):\nintegrated LUFS: -10.4 -> -10.3 (+0.1)\nloudness match skipped: A or B is silent"
            )
        };
        change(root);
        let paths = changed_paths(root).unwrap();
        commit(
            root,
            "Faster",
            "More drive.\n\nTwo lines.",
            &evidence("Refrain 9.1-17.1, 15.5 s"),
            &paths,
        )
        .unwrap();
        let renamed = EXAMPLES[0].1.replace("name: \"Outro\"", "name: \"Ende\"");
        fs::write(root.join(PROJECT_FILE), renamed).unwrap();
        let paths = changed_paths(root).unwrap();
        commit(
            root,
            "Rename",
            "A new name.",
            &evidence("all 1.1-33.1, 61.9 s"),
            &paths,
        )
        .unwrap();

        let open = open(root).unwrap();
        assert_eq!(open.len(), 2);
        let faster = &open[0];
        assert_eq!(faster.name, "faster");
        assert_eq!(faster.title, "Faster");
        assert_eq!(faster.why, "More drive.\n\nTwo lines.");
        assert_eq!(faster.label, "Refrain 9.1-17.1, 15.5 s");
        assert_eq!(faster.range(), "9.1-17.1");
        assert_eq!(open[1].range(), "1.1-33.1");
        assert_eq!(
            faster.evidence,
            [
                "integrated LUFS: -10.4 -> -10.3 (+0.1)",
                "loudness match skipped: A or B is silent"
            ]
        );
        assert_eq!(
            faster.hash,
            git::short_hash(root, "proposal/faster").unwrap()
        );
        assert!(accepted(root, 5).unwrap().is_empty());

        accept(root, "faster").unwrap();
        let done = accepted(root, 5).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(
            (done[0].name.as_str(), done[0].title.as_str()),
            ("", "Faster")
        );
        // The preview of the other proposal is what accepting it would give: both changes.
        let p = preview(root, "rename").unwrap();
        assert_eq!((p.sections[3].name.as_str(), p.tempo), ("Ende", 126.0));
    }
}
