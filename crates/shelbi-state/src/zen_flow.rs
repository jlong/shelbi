//! One record per task that Zen's pinned PR flow has probed, at
//! `<project>/zen-flow/<task-id>.json`.
//!
//! Each step of the flow (`zen probe`, `pr-create`, `ci-watch`, the `done`
//! move) is a separate process, and the orchestrator only ever carried the
//! probe's identity between them as flags in its own prose. This record is the
//! durable copy: what the probe pinned, and how far the flow has got with it.
//! The probe writes it, each later step advances it, and the workflow's `merge`
//! action reads it, so a landing can tell "CI graded this exact commit" from
//! "the card was moved".
//!
//! Unlike the PR status cache, this file authorizes a merge. An unreadable
//! record is an error for the caller to fail closed on, never an empty read.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shelbi_core::{Error, Result};

const DIR: &str = "zen-flow";

/// How far a task's flow has got with the commit its probe pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ZenFlowPhase {
    /// A probe pinned the identity. Nothing has been published under it.
    Probed,
    /// PR `pr`'s head is the pinned integration commit. CI has no verdict yet,
    /// or its last one wasn't green.
    Published { pr: u64 },
    /// `ci-watch` reported green for PR `pr` at the pinned integration commit.
    CiGreen { pr: u64 },
}

/// What one probe pinned for a task, and the phase the flow has reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZenFlowRecord {
    pub task_id: String,
    pub repository: String,
    pub repository_id: String,
    pub base_branch: String,
    pub base_sha: String,
    /// The commit CI grades and the PR head a landing requires. `None` when the
    /// probe found conflicts, so nothing under this record can land.
    pub integration_sha: Option<String>,
    pub head_sha: String,
    pub published_head_sha: String,
    pub phase: ZenFlowPhase,
}

fn dir(project: &str) -> Result<PathBuf> {
    Ok(crate::project_dir(project)?.join(DIR))
}

fn path(project: &str, task_id: &str) -> Result<PathBuf> {
    // A task id becomes a file name here, so it must be exactly one component.
    if Path::new(task_id).file_name() != Some(std::ffi::OsStr::new(task_id)) {
        return Err(Error::Other(format!(
            "task id `{task_id}` can't name a Zen flow record"
        )));
    }
    Ok(dir(project)?.join(format!("{task_id}.json")))
}

fn read(path: &Path) -> Result<Option<ZenFlowRecord>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::Other(format!("Zen flow record {}: {e}", path.display())))
}

/// `task_id`'s record. `None` means Zen hasn't probed this task.
pub fn load(project: &str, task_id: &str) -> Result<Option<ZenFlowRecord>> {
    read(&path(project, task_id)?)
}

/// Write `record` as its task's record, replacing any earlier one.
pub fn save(project: &str, record: &ZenFlowRecord) -> Result<()> {
    crate::ensure_dir(&dir(project)?)?;
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|e| Error::Other(format!("serialize Zen flow record: {e}")))?;
    crate::atomic_write(&path(project, &record.task_id)?, &bytes)
}

/// Drop `task_id`'s record. Dropping one that doesn't exist is fine.
pub fn forget(project: &str, task_id: &str) -> Result<()> {
    match std::fs::remove_file(path(project, task_id)?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::Io(e)),
        _ => Ok(()),
    }
}

/// The record pinning `integration_sha`. `ci-watch` is told a PR and an
/// identity but no task, and every probe builds its own integration commit, so
/// the commit names the record. An unreadable record is skipped, which leaves
/// that flow short of green.
pub fn find_by_integration(project: &str, integration_sha: &str) -> Result<Option<ZenFlowRecord>> {
    let entries = match std::fs::read_dir(dir(project)?) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    for entry in entries {
        let path = entry.map_err(Error::Io)?.path();
        match read(&path) {
            Ok(Some(record)) if record.integration_sha.as_deref() == Some(integration_sha) => {
                return Ok(Some(record));
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "skipping unreadable Zen flow record"),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;

    fn with_home(f: impl FnOnce()) {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "shelbi-zen-flow-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("SHELBI_HOME", &home);
        f();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn record(task_id: &str, integration: &str) -> ZenFlowRecord {
        ZenFlowRecord {
            task_id: task_id.into(),
            repository: "github.com/example/repo".into(),
            repository_id: "R_1".into(),
            base_branch: "main".into(),
            base_sha: "base".into(),
            integration_sha: Some(integration.into()),
            head_sha: "head".into(),
            published_head_sha: "head".into(),
            phase: ZenFlowPhase::CiGreen { pr: 7 },
        }
    }

    #[test]
    fn a_record_round_trips_and_a_missing_one_reads_as_none() {
        with_home(|| {
            assert_eq!(load("p", "t").unwrap(), None);
            save("p", &record("t", "x")).unwrap();
            assert_eq!(load("p", "t").unwrap(), Some(record("t", "x")));
            forget("p", "t").unwrap();
            assert_eq!(load("p", "t").unwrap(), None);
            forget("p", "t").unwrap();
        });
    }

    #[test]
    fn a_corrupt_record_is_an_error_not_an_absent_one() {
        with_home(|| {
            save("p", &record("t", "x")).unwrap();
            std::fs::write(path("p", "t").unwrap(), "{not json").unwrap();
            assert!(load("p", "t").is_err());
        });
    }

    #[test]
    fn the_integration_commit_finds_its_record_past_a_corrupt_one() {
        with_home(|| {
            assert_eq!(find_by_integration("p", "x").unwrap(), None);
            save("p", &record("a", "x")).unwrap();
            save("p", &record("b", "y")).unwrap();
            save("p", &record("c", "z")).unwrap();
            std::fs::write(path("p", "c").unwrap(), "{not json").unwrap();
            assert_eq!(find_by_integration("p", "y").unwrap().unwrap().task_id, "b");
            assert_eq!(find_by_integration("p", "z").unwrap(), None);
        });
    }

    #[test]
    fn a_task_id_cannot_escape_the_record_directory() {
        with_home(|| {
            assert!(load("p", "../evil").is_err());
            assert!(load("p", "a/b").is_err());
            assert!(load("p", "..").is_err());
        });
    }
}
