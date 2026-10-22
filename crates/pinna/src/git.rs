//! Git through the `git` CLI, so the user's signing config and hooks apply.

use std::path::Path;
use std::process::{Command, Stdio};

/// Run git in `dir` and return its stdout. Errors carry git's stderr.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    // stdin is the MCP channel when serving; git must never read it.
    cmd.current_dir(dir).args(args).stdin(Stdio::null());
    #[cfg(test)]
    cmd.env("GIT_CONFIG_GLOBAL", tests::config())
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Revisions come from agents; one starting with '-' would be read as an option.
pub fn check_rev(rev: &str) -> Result<(), String> {
    if rev.is_empty() || rev.starts_with('-') {
        Err(format!(
            "revision {rev:?} is invalid: it must not be empty or start with '-'; use e.g. HEAD, HEAD~1, main or worktree"
        ))
    } else {
        Ok(())
    }
}

pub fn short_hash(dir: &Path, rev: &str) -> Result<String, String> {
    check_rev(rev)?;
    Ok(git(dir, &["rev-parse", "--short", rev])?.trim().to_string())
}

#[cfg(test)]
pub mod tests {
    use std::path::Path;
    use std::sync::LazyLock;

    /// Global git config for tests: an identity and nothing else, so the user's
    /// signing setup and hooks stay out of test repos.
    pub fn config() -> &'static Path {
        static FILE: LazyLock<tempfile::NamedTempFile> = LazyLock::new(|| {
            let f = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(
                f.path(),
                "[user]\n\tname = Pinna Test\n\temail = test@example.org\n",
            )
            .unwrap();
            f
        });
        FILE.path()
    }

    #[test]
    fn revs_starting_with_dash_are_rejected() {
        assert!(super::check_rev("--output=/tmp/x").is_err());
        assert!(super::check_rev("").is_err());
        assert!(super::check_rev("HEAD~1").is_ok());
    }
}
