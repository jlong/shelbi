//! Per-project `pr-template.md` — the user-owned instructions the developer
//! worker follows when authoring a pull-request body for a finished task.
//!
//! The file lives in the project's config half
//! (`<config_root>/pr-template.md`, alongside `workflows/`, `agents/`, and
//! `zenmode.md`) and is materialized on `shelbi init` and self-healed on
//! `shelbi reload` (and by the version-agnostic config-upgrade pass on hub
//! start). It is **instructions to the worker** on how to write the PR body —
//! summary grounded in the real problem, technical details, a conditional
//! Mermaid diagram for schema changes, screenshots or an ASCII wireframe for
//! UI changes, and a QA checklist — not a variable-substitution template.
//!
//! Like `zenmode.md` and `agents/*/instructions.md`, the file is
//! user-editable and self-heal-preserving: custom prose survives a reload.
//! Editing it changes the guidance the worker follows for subsequent PRs.

use std::path::PathBuf;

use shelbi_core::Result;

use crate::{atomic_write, config_project_dir};

/// Bundled default `pr-template.md` content — the shipped guidance a worker
/// follows when it has no project-specific override.
pub const DEFAULT_PR_TEMPLATE: &str = include_str!("default_pr_template.md.template");

/// File name of the per-project PR-description template.
pub const PR_TEMPLATE_FILE: &str = "pr-template.md";

/// `<config_root>/pr-template.md` — config-mode-aware, so an in-repo project
/// resolves it to `<repo>/.shelbi/pr-template.md` and a global project to
/// `~/.shelbi/projects/<name>/pr-template.md`.
pub fn pr_template_path(project: &str) -> Result<PathBuf> {
    Ok(config_project_dir(project)?.join(PR_TEMPLATE_FILE))
}

/// Outcome of a scaffold / self-heal pass over `pr-template.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrTemplateOutcome {
    /// The file was missing and has just been written from the bundled
    /// default.
    Created,
    /// The file already existed and was left untouched — user edits are
    /// preserved byte-for-byte.
    Unchanged,
}

/// Write the default `pr-template.md` when absent, preserving any existing
/// custom content byte-for-byte. Used by both `shelbi init` and `shelbi
/// reload`; the config-upgrade pass materializes it the same way on hub start
/// for projects that predate this file.
pub fn scaffold_pr_template(project: &str) -> Result<PrTemplateOutcome> {
    let path = pr_template_path(project)?;
    if path.exists() {
        return Ok(PrTemplateOutcome::Unchanged);
    }
    atomic_write(&path, DEFAULT_PR_TEMPLATE.as_bytes())?;
    Ok(PrTemplateOutcome::Created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use std::fs;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-pr-template-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn default_template_covers_every_required_section() {
        // The shipped guidance must name each of the five sections so a worker
        // reading it produces the intended structure.
        for needle in [
            "## 1. Summary",
            "## 2. Technical details",
            "Mermaid",
            "ASCII wireframe",
            "## 5. QA checklist",
            ".shelbi/pr-body.md",
        ] {
            assert!(
                DEFAULT_PR_TEMPLATE.contains(needle),
                "default template missing {needle:?}"
            );
        }
    }

    #[test]
    fn scaffold_writes_when_absent_and_preserves_user_edits() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Missing → written from the default.
        assert_eq!(
            scaffold_pr_template("p").unwrap(),
            PrTemplateOutcome::Created
        );
        let path = pr_template_path("p").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_PR_TEMPLATE);

        // A user edit survives a re-run byte-for-byte.
        let edited = "# My project PR rules\n\nAlways include a rollback plan.\n";
        fs::write(&path, edited).unwrap();
        assert_eq!(
            scaffold_pr_template("p").unwrap(),
            PrTemplateOutcome::Unchanged
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);

        std::env::remove_var("SHELBI_HOME");
    }
}
