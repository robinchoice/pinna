//! Reading and writing `pinna.ron`. Output is canonical: the same project always
//! serializes to the same text, one element per line, so Git diffs stay small.

use std::path::Path;

use ron::extensions::Extensions;
use ron::ser::PrettyConfig;

use crate::Error;
use crate::model::Project;
use crate::validate::validate;

pub const PROJECT_FILE: &str = "pinna.ron";

/// Parse and validate project text.
pub fn parse(text: &str) -> Result<Project, Error> {
    let project: Project = ron::Options::default()
        .with_default_extension(Extensions::IMPLICIT_SOME)
        .from_str(text)
        .map_err(|e| Error::Parse(e.to_string()))?;
    let errs = validate(&project);
    if errs.is_empty() {
        Ok(project)
    } else {
        Err(Error::Invalid(errs))
    }
}

/// Canonical text of a project. Does not validate.
pub fn to_text(project: &Project) -> String {
    let config = PrettyConfig::new()
        .indentor("  ")
        .struct_names(false)
        .separate_tuple_members(false)
        .extensions(Extensions::IMPLICIT_SOME);
    let mut text = ron::ser::to_string_pretty(project, config).expect("project serializes");
    text.push('\n');
    text
}

pub fn load(root: &Path) -> Result<Project, Error> {
    let path = root.join(PROJECT_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    parse(&text)
}

/// Validate, then write atomically (temp file and rename).
pub fn save(root: &Path, project: &Project) -> Result<(), Error> {
    let errs = validate(project);
    if !errs.is_empty() {
        return Err(Error::Invalid(errs));
    }
    let path = root.join(PROJECT_FILE);
    let tmp = root.join(format!(".{PROJECT_FILE}.tmp"));
    std::fs::write(&tmp, to_text(project))
        .map_err(|e| Error::Io(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Example projects must load and already be in canonical form, so a load/save
    /// cycle never produces a diff. `PINNA_BLESS=1` rewrites them canonically.
    #[test]
    fn examples_are_canonical() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let root = entry.unwrap().path();
            let text = std::fs::read_to_string(root.join(PROJECT_FILE)).unwrap();
            let project = parse(&text).unwrap_or_else(|e| panic!("{}: {e}", root.display()));
            if std::env::var_os("PINNA_BLESS").is_some() {
                save(&root, &project).unwrap();
            } else {
                assert_eq!(
                    to_text(&project),
                    text,
                    "{} is not canonical, run with PINNA_BLESS=1",
                    root.display()
                );
            }
            assert_eq!(parse(&to_text(&project)).unwrap(), project);
            seen += 1;
        }
        assert!(seen > 0);
    }
}
