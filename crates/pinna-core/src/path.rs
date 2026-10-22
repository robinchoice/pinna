//! Path hardening: every file a project references stays inside the project folder.

use std::path::{Component, Path, PathBuf};

/// Syntactic check of a project-relative path: no absolute paths, no `..`, no empty parts.
pub fn check_relative(rel: &str) -> Result<(), String> {
    let path = Path::new(rel);
    if rel.is_empty() || rel.contains('\0') || rel.contains('\\') {
        return Err(format!(
            "path {rel:?} is empty or contains invalid characters"
        ));
    }
    let normal = path.components().all(|c| matches!(c, Component::Normal(_)));
    if !normal || rel.split('/').any(|part| part.is_empty() || part == ".") {
        return Err(format!(
            "path {rel:?} must be relative to the project and must not use '..' or '.'"
        ));
    }
    Ok(())
}

/// Join a project-relative path onto `root` and make sure the existing file (after
/// following symlinks) is still inside `root`.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    check_relative(rel)?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("project folder {}: {e}", root.display()))?;
    let full = root.join(rel);
    let real = full.canonicalize().map_err(|e| format!("{rel}: {e}"))?;
    if !real.starts_with(&root) {
        return Err(format!("path {rel:?} leaves the project folder"));
    }
    Ok(real)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escapes() {
        assert!(check_relative("audio/vox.wav").is_ok());
        assert!(check_relative("../x.wav").is_err());
        assert!(check_relative("/etc/passwd").is_err());
        assert!(check_relative("a/./b").is_err());
        assert!(check_relative("").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_out() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link.wav")).unwrap();
        std::fs::write(dir.path().join("in.wav"), b"x").unwrap();
        assert!(resolve(dir.path(), "link.wav").is_err());
        assert!(resolve(dir.path(), "in.wav").is_ok());
    }
}
