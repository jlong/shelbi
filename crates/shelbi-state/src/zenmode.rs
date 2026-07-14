//! Per-project `zenmode.md` — the user-owned source of truth for what Zen
//! Mode means for this project.
//!
//! The file lives in the project's config half (`<config_root>/zenmode.md`,
//! alongside `workflows/` and `agents/`) and is materialized on `shelbi
//! init` and self-healed on `shelbi reload`. Its **first line is a one-line
//! Zen summary** the heartbeat re-injects on a short cadence so the running
//! orchestrator keeps applying Zen behavior even as its static instructions
//! fade from attention over a long session; the rest of the file is the
//! fuller auto-promote + merge policy.
//!
//! Like `agents/*/instructions.md`, the file is user-editable and
//! self-heal-preserving: once written it is never clobbered, so edits
//! (including edits to the first-line summary) survive a reload. Only a
//! genuinely missing file is (re)created.

use std::path::PathBuf;

use shelbi_core::{Error, Result};

use crate::{atomic_write, config_project_dir};

/// Bundled default `zenmode.md` content. The first line is the one-line Zen
/// summary the heartbeat echoes back; the rest is the auto-promote
/// categories and merge conditions the orchestrator applies.
pub const DEFAULT_ZENMODE: &str = include_str!("default_zenmode.md.template");

/// File name of the per-project Zen policy definition.
pub const ZENMODE_FILE: &str = "zenmode.md";

/// `<config_root>/zenmode.md` — config-mode-aware, so an in-repo project
/// resolves it to `<repo>/.shelbi/zenmode.md` and a global project to
/// `~/.shelbi/projects/<name>/zenmode.md`.
pub fn zenmode_path(project: &str) -> Result<PathBuf> {
    Ok(config_project_dir(project)?.join(ZENMODE_FILE))
}

/// Outcome of a scaffold / self-heal pass over `zenmode.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZenmodeOutcome {
    /// The file was missing and has just been written from the bundled
    /// default.
    Created,
    /// The file already existed and was left untouched — user edits
    /// (including the first-line summary) are preserved byte-for-byte.
    Unchanged,
}

/// Write the default `zenmode.md` when absent, preserving any existing file
/// (and its user edits) byte-for-byte. Used by both `shelbi init` (fresh
/// project) and the `shelbi reload` self-heal path (adds the file to an
/// existing project that predates it), mirroring how `instructions.md` is
/// preserved on reload.
pub fn scaffold_zenmode(project: &str) -> Result<ZenmodeOutcome> {
    let path = zenmode_path(project)?;
    if path.exists() {
        return Ok(ZenmodeOutcome::Unchanged);
    }
    atomic_write(&path, DEFAULT_ZENMODE.as_bytes())?;
    Ok(ZenmodeOutcome::Created)
}

/// First line of the project's `zenmode.md`, read fresh and trimmed of
/// surrounding whitespace. This is what the heartbeat re-injects on its
/// short cadence — reading fresh each time means a user's edit to the first
/// line takes effect on the next heartbeat without a reload.
///
/// `Ok(None)` when the file is missing (the heartbeat then emits a plain
/// `zen=on` cue with no summary until the next reload materializes the file)
/// or when the first line is blank. Non-`NotFound` IO errors propagate so a
/// transiently unreadable file isn't silently treated as absent.
pub fn read_zenmode_summary(project: &str) -> Result<Option<String>> {
    let path = zenmode_path(project)?;
    let text = match crate::read_to_string_at(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    let first = text.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return Ok(None);
    }
    Ok(Some(first.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use std::fs;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-zenmode-test-{}-{}",
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
    fn default_zenmode_first_line_is_a_one_line_summary() {
        // The bundled default's first line is a single, non-empty summary
        // line (no markdown heading) the heartbeat can echo verbatim.
        let mut lines = DEFAULT_ZENMODE.lines();
        let first = lines.next().unwrap();
        assert!(!first.trim().is_empty(), "first line must be the summary");
        assert!(
            !first.starts_with('#'),
            "first line is the summary sentence, not a markdown heading"
        );
        assert!(
            first.contains("Zen"),
            "summary should describe Zen: {first:?}"
        );
        // The fuller policy follows below the summary.
        assert!(DEFAULT_ZENMODE.contains("Auto-promote judgment categories"));
        assert!(DEFAULT_ZENMODE.contains("Merge conditions"));
        assert!(DEFAULT_ZENMODE
            .contains("shelbi zen pr-create <task-id> --match-head-commit <head_sha>"));
        assert!(DEFAULT_ZENMODE
            .contains("shelbi zen pr-merge <pr-number> --match-head-commit <head_sha>"));
    }

    #[test]
    fn scaffold_writes_when_absent_and_preserves_user_edits() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Missing → written from the default.
        assert_eq!(scaffold_zenmode("p").unwrap(), ZenmodeOutcome::Created);
        let path = zenmode_path("p").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_ZENMODE);

        // A user edit (including the first-line summary) survives a re-run.
        let edited = "Zen: my own policy, promote nothing.\n\nDetail here.\n";
        fs::write(&path, edited).unwrap();
        assert_eq!(scaffold_zenmode("p").unwrap(), ZenmodeOutcome::Unchanged);
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn summary_reads_first_line_fresh() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Missing file → None (heartbeat falls back to a plain cue).
        assert_eq!(read_zenmode_summary("p").unwrap(), None);

        scaffold_zenmode("p").unwrap();
        let default_first = DEFAULT_ZENMODE.lines().next().unwrap().to_string();
        assert_eq!(read_zenmode_summary("p").unwrap(), Some(default_first));

        // A first-line edit is picked up fresh on the next read (no reload).
        let path = zenmode_path("p").unwrap();
        fs::write(&path, "Zen: edited summary line.\n\nrest\n").unwrap();
        assert_eq!(
            read_zenmode_summary("p").unwrap(),
            Some("Zen: edited summary line.".to_string())
        );

        // A blank first line reads as None rather than an empty reminder.
        fs::write(&path, "\n\nbody\n").unwrap();
        assert_eq!(read_zenmode_summary("p").unwrap(), None);

        std::env::remove_var("SHELBI_HOME");
    }
}
