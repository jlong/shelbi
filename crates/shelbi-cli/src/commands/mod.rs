pub mod archive;
pub mod diff;
pub mod init;
pub mod list;
pub mod merge;
pub mod orchestrate;
pub mod send;
pub mod spawn;
pub mod status;
pub mod tail;

use anyhow::{anyhow, Result};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

/// Resolve the active project name. Precedence:
///
/// 1. The `--project` / `$SHELBI_PROJECT` value passed in.
/// 2. The contents of the nearest `.shelbi/project` marker file walking up
///    from the current directory.
///
/// Errors if nothing resolves.
pub fn require_project(explicit: Option<String>) -> Result<String> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(name) = discover_project_marker(&cwd)? {
            return Ok(name);
        }
    }
    Err(anyhow!(
        "no project specified — pass --project NAME, set SHELBI_PROJECT, or write the project \
         name into a `.shelbi/project` file at the top of your repo"
    ))
}

/// Walk up from `start`, looking for `.shelbi/project`. Returns the trimmed
/// contents of the first one found, or `None` if no marker exists.
fn discover_project_marker(start: &Path) -> Result<Option<String>> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let marker = dir.join(".shelbi").join("project");
        if marker.is_file() {
            let name = std::fs::read_to_string(&marker)?.trim().to_string();
            if name.is_empty() {
                return Err(anyhow!(
                    "`.shelbi/project` at {} is empty",
                    marker.display()
                ));
            }
            return Ok(Some(name));
        }
        cur = dir.parent();
    }
    Ok(None)
}

/// Resolve the working session (workspace) name. Precedence: explicit > env >
/// "default".
pub fn _resolve_session(explicit: Option<String>) -> String {
    explicit
        .or_else(|| std::env::var("SHELBI_SESSION").ok())
        .unwrap_or_else(|| "default".to_string())
}

#[cfg(test)]
pub(crate) fn _marker_for_test(start: &Path) -> Result<Option<String>> {
    discover_project_marker(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_walks_up() {
        let tmp = tempfile_dir();
        let sub = tmp.join("a/b/c");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(tmp.join(".shelbi")).unwrap();
        std::fs::write(tmp.join(".shelbi/project"), "myapp\n").unwrap();

        let found = _marker_for_test(&sub).unwrap();
        assert_eq!(found.as_deref(), Some("myapp"));
    }

    #[test]
    fn marker_absent_returns_none() {
        let tmp = tempfile_dir();
        let found = _marker_for_test(&tmp).unwrap();
        assert!(found.is_none());
    }

    fn tempfile_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-test-{}-{}",
            std::process::id(),
            // poor-man's unique suffix
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
