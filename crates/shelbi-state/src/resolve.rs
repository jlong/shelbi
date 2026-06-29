//! Project resolution by reverse-lookup against the registered project
//! YAMLs. Instead of dropping a `.shelbi/project` sentinel into every repo
//! and walking up to find it, we scan `~/.shelbi/projects/*.yaml` once,
//! collect each project's local `work_dir`(s), and match the current
//! directory (or one of its ancestors) against them — deepest match wins.
//!
//! The marker file is gone; the project metadata on disk is the single
//! source of truth for "which project does this directory belong to".

use std::fs;
use std::path::{Path, PathBuf};

use shelbi_core::{MachineKind, Project, Result};

use crate::projects_dir;

/// A candidate project root used for cwd-based resolution: one entry per
/// local `work_dir` declared by a registered project.
#[derive(Debug, Clone)]
pub struct ProjectRoot {
    /// The project name (from the YAML).
    pub name: String,
    /// The `work_dir` exactly as written in the YAML.
    pub work_dir: PathBuf,
    /// `work_dir` run through [`fs::canonicalize`], resolving symlinks.
    /// `None` when the path no longer exists on disk — such a project is
    /// excluded from resolution and surfaced as a warning by `shelbi
    /// reload`.
    pub canonical: Option<PathBuf>,
}

/// Scan `<shelbi-root>/projects/*.yaml` and collect every local machine's
/// `work_dir` as a [`ProjectRoot`]. Files that fail to read or parse are
/// skipped silently — a single malformed YAML shouldn't break resolution
/// for every other project. Only `kind: local` machines contribute: a
/// remote `work_dir` is a path on another host and matching it against the
/// local cwd would be a false positive.
pub fn project_roots() -> Result<Vec<ProjectRoot>> {
    let dir = projects_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let project: Project = match serde_yaml::from_str(&text) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for machine in &project.machines {
            if machine.kind != MachineKind::Local {
                continue;
            }
            let canonical = fs::canonicalize(&machine.work_dir).ok();
            out.push(ProjectRoot {
                name: project.name.clone(),
                work_dir: machine.work_dir.clone(),
                canonical,
            });
        }
    }
    Ok(out)
}

/// Resolve the project owning `cwd` (or one of its ancestors) by matching
/// against registered project work_dirs. Returns the deepest match — when
/// two projects nest (`~/foo` and `~/foo/sub`), a cwd inside `sub` resolves
/// to the sub-project. Projects whose `work_dir` no longer exists are
/// excluded. Returns `None` when nothing matches.
pub fn resolve_project_for_cwd(cwd: &Path) -> Result<Option<String>> {
    Ok(match_root(cwd, &project_roots()?))
}

/// Pure matching core, factored out for tests: canonicalize `cwd`, walk up
/// its ancestors, and return the name of the first (hence deepest) root
/// whose canonical `work_dir` equals an ancestor.
fn match_root(cwd: &Path, roots: &[ProjectRoot]) -> Option<String> {
    // Canonicalize the start so the comparison is symlink-stable on both
    // sides. If cwd itself can't be canonicalized (e.g. it was deleted out
    // from under us) fall back to the literal path.
    let start = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());

    // Walking up from cwd visits the deepest directory first, so the first
    // ancestor that equals any root's canonical work_dir is the deepest
    // match — no separate length sort needed.
    let mut cur: Option<&Path> = Some(&start);
    while let Some(dir) = cur {
        if let Some(root) = roots
            .iter()
            .find(|r| r.canonical.as_deref() == Some(dir))
        {
            return Some(root.name.clone());
        }
        cur = dir.parent();
    }
    None
}

/// What `shelbi reload` did (or noticed) for one registered project root
/// while cleaning up legacy state.
#[derive(Debug, Clone)]
pub struct MarkerCleanup {
    pub name: String,
    pub work_dir: PathBuf,
    /// A legacy `<work_dir>/.shelbi/project` marker existed and was removed.
    pub marker_removed: bool,
    /// The `work_dir` no longer exists on disk — the project is excluded
    /// from resolution until the user re-points or removes it.
    pub work_dir_missing: bool,
}

/// Sweep every registered project's local `work_dir`: delete any leftover
/// `.shelbi/project` marker (now redundant — resolution reads the YAMLs)
/// and flag work_dirs that have gone missing. Idempotent and best-effort;
/// a project whose tree is gone is reported, not deleted.
pub fn cleanup_legacy_markers() -> Result<Vec<MarkerCleanup>> {
    let mut out = Vec::new();
    for root in project_roots()? {
        let mut cleanup = MarkerCleanup {
            name: root.name,
            work_dir: root.work_dir.clone(),
            marker_removed: false,
            work_dir_missing: root.canonical.is_none(),
        };
        if !cleanup.work_dir_missing {
            let marker = root.work_dir.join(".shelbi").join("project");
            if marker.is_file() && fs::remove_file(&marker).is_ok() {
                cleanup.marker_removed = true;
            }
        }
        out.push(cleanup);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;

    /// A unique temp home for one test. The nanosecond suffix keeps
    /// parallel tests from colliding even though they share `SHELBI_HOME`.
    fn fresh_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-resolve-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// Write a minimal project YAML registering `name` with a single local
    /// hub whose `work_dir` is `work_dir`.
    fn write_project(home: &Path, name: &str, work_dir: &Path) {
        let dir = home.join("projects");
        fs::create_dir_all(&dir).unwrap();
        let yaml = format!(
            "name: {name}\n\
             repo: {wd}\n\
             machines:\n\
             \x20\x20- {{ name: hub, kind: local, work_dir: {wd} }}\n\
             orchestrator: {{ runner: claude }}\n\
             agent_runners:\n\
             \x20\x20claude: {{ command: claude, flags: [] }}\n",
            wd = work_dir.display(),
        );
        fs::write(dir.join(format!("{name}.yaml")), yaml).unwrap();
    }

    #[test]
    fn cwd_at_project_root_matches() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let root = fresh_dir("foo");
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "foo", &root);

        let found = resolve_project_for_cwd(&root).unwrap();
        assert_eq!(found.as_deref(), Some("foo"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn cwd_below_project_root_matches() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let root = fresh_dir("foo");
        let deep = root.join("src/bar");
        fs::create_dir_all(&deep).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "foo", &root);

        let found = resolve_project_for_cwd(&deep).unwrap();
        assert_eq!(found.as_deref(), Some("foo"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn deepest_overlapping_work_dir_wins() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let outer = fresh_dir("foo");
        let inner = outer.join("sub-project");
        fs::create_dir_all(&inner).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "outer", &outer);
        write_project(&home, "inner", &inner);

        // cwd inside the inner project resolves to it, not the outer one.
        let nested = inner.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            resolve_project_for_cwd(&nested).unwrap().as_deref(),
            Some("inner")
        );
        // cwd between the two (in outer but above inner) resolves to outer.
        assert_eq!(
            resolve_project_for_cwd(&outer).unwrap().as_deref(),
            Some("outer")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn missing_work_dir_is_excluded() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let ghost = std::env::temp_dir().join(format!(
            "shelbi-resolve-ghost-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Note: `ghost` is never created on disk.
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "ghost", &ghost);

        let roots = project_roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert!(roots[0].canonical.is_none());
        // Even querying from inside the (nonexistent) path resolves to nothing.
        assert!(resolve_project_for_cwd(&ghost).unwrap().is_none());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn symlinked_project_root_resolves() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let real = fresh_dir("real");
        let link_parent = fresh_dir("links");
        let link = link_parent.join("link-to-real");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        // Register the project by its REAL path; query through the symlink.
        write_project(&home, "foo", &real);

        assert_eq!(
            resolve_project_for_cwd(&link).unwrap().as_deref(),
            Some("foo")
        );

        // And the reverse: register through a symlink, query the real path.
        let home2 = fresh_dir("home2");
        std::env::set_var("SHELBI_HOME", &home2);
        write_project(&home2, "bar", &link);
        assert_eq!(
            resolve_project_for_cwd(&real).unwrap().as_deref(),
            Some("bar")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn cleanup_removes_legacy_marker_and_flags_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_dir("home");
        let root = fresh_dir("foo");
        let ghost = std::env::temp_dir().join(format!(
            "shelbi-resolve-cleanup-ghost-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SHELBI_HOME", &home);
        write_project(&home, "foo", &root);
        write_project(&home, "ghost", &ghost);

        // Drop a legacy marker into the live project tree.
        let marker = root.join(".shelbi/project");
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "foo\n").unwrap();

        let report = cleanup_legacy_markers().unwrap();
        let foo = report.iter().find(|c| c.name == "foo").unwrap();
        assert!(foo.marker_removed);
        assert!(!foo.work_dir_missing);
        assert!(!marker.exists(), "marker should be gone");

        let ghost_c = report.iter().find(|c| c.name == "ghost").unwrap();
        assert!(ghost_c.work_dir_missing);
        assert!(!ghost_c.marker_removed);

        // Idempotent: a second sweep finds nothing to remove.
        let report2 = cleanup_legacy_markers().unwrap();
        assert!(!report2.iter().any(|c| c.marker_removed));
        std::env::remove_var("SHELBI_HOME");
    }
}
