//! Which OpenCode session Shelbi launched for each task on a workspace.
//!
//! An OpenCode pane runs `opencode --session <id>`, where the id is derived
//! from the project, workspace, task and a *generation*
//! ([`shelbi_core::opencode_session_id`]). This ledger keeps the generation:
//! a resume reuses it, so the pane reopens the same conversation; any other
//! launch (a fresh `issue start`, a supervised restart) bumps it, so the pane
//! starts a new one. It lives at
//! `~/.shelbi/projects/<project>/workspaces/<ws>/opencode-sessions.yaml`,
//! beside `status.yaml` but not in it, because `status.yaml` is deleted when
//! a slot is reaped and a generation that went back to 0 would reopen an old
//! conversation.
//!
//! The slot's current id is mirrored into `status.yaml`
//! ([`crate::WorkspaceStatus::opencode_session`]) on every save, and the
//! poller archives a task's sessions once the task is done (`archived` keeps
//! it from doing that twice).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use shelbi_core::Result;

use crate::{acquire_file_lock, atomic_write, workspaces_dir};

/// One workspace's OpenCode sessions, keyed by task id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCodeSessions {
    /// The task whose session the slot's pane was last launched on, while
    /// that launch was an OpenCode one. `None` once a different runner takes
    /// the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default)]
    pub tasks: BTreeMap<String, OpenCodeTaskSession>,
}

/// The newest OpenCode session Shelbi launched for one task on a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCodeTaskSession {
    /// Bumped on every launch that isn't a resume. Sessions of earlier
    /// generations are the ids for `0..generation`.
    pub generation: u32,
    /// `--session` id of this generation.
    pub id: String,
    /// The OpenCode command the pane ran, which the archive pass runs too.
    pub command: String,
    /// Shelbi archived this task's sessions after the task reached done.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// Archive attempts that failed (the host was unreachable, OpenCode
    /// wasn't found). The pass gives up after a few.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub archive_attempts: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl OpenCodeTaskSession {
    /// Every session id Shelbi has launched for this task here, oldest
    /// first: one per generation.
    pub fn all_ids(&self, project: &str, workspace: &str, task: &str) -> Vec<String> {
        (0..=self.generation)
            .map(|g| shelbi_core::opencode_session_id(project, workspace, task, g))
            .collect()
    }
}

/// `~/.shelbi/projects/<project>/workspaces/<ws>/opencode-sessions.yaml`.
pub fn opencode_sessions_path(project: &str, workspace: &str) -> Result<PathBuf> {
    crate::ensure_flat_path_component("workspace", workspace)?;
    Ok(workspaces_dir(project)?.join(workspace).join("opencode-sessions.yaml"))
}

/// The ledger for `workspace`; empty when there is none yet.
pub fn load_opencode_sessions(project: &str, workspace: &str) -> Result<OpenCodeSessions> {
    let path = opencode_sessions_path(project, workspace)?;
    match fs::read_to_string(&path) {
        Ok(text) => Ok(yaml_serde::from_str(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(OpenCodeSessions::default()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

/// Read-modify-write the ledger under its lock, so a dispatch and the
/// poller's archive pass can't drop each other's change. `f` returns whether
/// it changed anything; an unchanged ledger isn't written (and a missing one
/// isn't created).
pub fn update_opencode_sessions<T>(
    project: &str,
    workspace: &str,
    f: impl FnOnce(&mut OpenCodeSessions) -> (bool, T),
) -> Result<T> {
    let path = opencode_sessions_path(project, workspace)?;
    let _lock = acquire_file_lock(&path.with_extension("lock"))?;
    let mut ledger = load_opencode_sessions(project, workspace)?;
    let (changed, out) = f(&mut ledger);
    if changed {
        atomic_write(&path, yaml_serde::to_string(&ledger)?.as_bytes())?;
    }
    Ok(out)
}

/// Record an OpenCode launch of `task` on `workspace` and return the session
/// id the pane runs: this generation's id on a `resume`, the next
/// generation's otherwise (0 for the task's first launch here). The slot's
/// `status.yaml`, when there is one, is rewritten to show it.
pub fn begin_opencode_session(
    project: &str,
    workspace: &str,
    task: &str,
    resume: bool,
    command: &str,
) -> Result<String> {
    let id = update_opencode_sessions(project, workspace, |ledger| {
        let generation = match ledger.tasks.get(task) {
            Some(prior) if resume => prior.generation,
            Some(prior) => prior.generation.saturating_add(1),
            None => 0,
        };
        let id = shelbi_core::opencode_session_id(project, workspace, task, generation);
        ledger.tasks.insert(
            task.to_string(),
            OpenCodeTaskSession {
                generation,
                id: id.clone(),
                command: command.to_string(),
                archived: false,
                archive_attempts: 0,
            },
        );
        ledger.current = Some(task.to_string());
        (true, id)
    })?;
    refresh_status_mirror(project, workspace);
    Ok(id)
}

/// Record that `workspace` was launched on something other than OpenCode,
/// so `status.yaml` stops naming the last OpenCode session. The ledger keeps
/// the task's generation for a later OpenCode launch.
pub fn end_opencode_session(project: &str, workspace: &str) -> Result<()> {
    let path = opencode_sessions_path(project, workspace)?;
    if !path.exists() {
        return Ok(());
    }
    let changed = update_opencode_sessions(project, workspace, |ledger| {
        let changed = ledger.current.take().is_some();
        (changed, changed)
    })?;
    if changed {
        refresh_status_mirror(project, workspace);
    }
    Ok(())
}

/// The session id the slot's pane runs for `task`, when its last launch was
/// an OpenCode one on that task.
pub fn current_opencode_session(project: &str, workspace: &str, task: Option<&str>) -> Option<String> {
    let task = task?;
    let ledger = load_opencode_sessions(project, workspace).ok()?;
    if ledger.current.as_deref() != Some(task) {
        return None;
    }
    ledger.tasks.get(task).map(|t| t.id.clone())
}

/// Re-save an existing `status.yaml` so it picks up the ledger's current id
/// now rather than on the next poll. Best-effort: the poll writes it anyway.
fn refresh_status_mirror(project: &str, workspace: &str) {
    if let Ok(Some(status)) = crate::load_workspace_status(project, workspace) {
        let _ = crate::save_workspace_status(project, &status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use crate::{load_workspace_status, save_workspace_status, WorkspaceState, WorkspaceStatus};

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-opencode-sessions-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn status(task: &str) -> WorkspaceStatus {
        let now = chrono::Utc::now();
        WorkspaceStatus {
            workspace: "bravo".into(),
            current_task: Some(task.into()),
            state: WorkspaceState::Working,
            last_transition: now,
            last_seen: now,
            opencode_session: None,
        }
    }

    #[test]
    fn a_resume_reopens_the_session_and_a_fresh_start_gets_a_new_one() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let first = begin_opencode_session("demo", "bravo", "fix", false, "opencode").unwrap();
        assert_eq!(first, shelbi_core::opencode_session_id("demo", "bravo", "fix", 0));
        // Resumed (pane killed, `issue resume`): the same conversation.
        assert_eq!(begin_opencode_session("demo", "bravo", "fix", true, "opencode").unwrap(), first);
        // A fresh start: the next generation, a new session.
        let fresh = begin_opencode_session("demo", "bravo", "fix", false, "opencode").unwrap();
        assert_eq!(fresh, shelbi_core::opencode_session_id("demo", "bravo", "fix", 1));
        assert_eq!(begin_opencode_session("demo", "bravo", "fix", true, "opencode").unwrap(), fresh);

        // Another task on the slot, then back: the first task's generation
        // was kept, so its fresh start still doesn't reuse an old session.
        begin_opencode_session("demo", "bravo", "other", false, "opencode").unwrap();
        let again = begin_opencode_session("demo", "bravo", "fix", false, "opencode").unwrap();
        assert_eq!(again, shelbi_core::opencode_session_id("demo", "bravo", "fix", 2));

        // A resume with nothing recorded starts at generation 0.
        assert_eq!(
            begin_opencode_session("demo", "alpha", "new", true, "opencode").unwrap(),
            shelbi_core::opencode_session_id("demo", "alpha", "new", 0)
        );

        let ledger = load_opencode_sessions("demo", "bravo").unwrap();
        assert_eq!(ledger.current.as_deref(), Some("fix"));
        assert_eq!(
            ledger.tasks["fix"].all_ids("demo", "bravo", "fix"),
            vec![first, fresh, again]
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn status_yaml_shows_the_slots_current_session() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Before any OpenCode launch, the field is absent from the file.
        save_workspace_status("demo", &status("fix")).unwrap();
        let path = crate::workspace_status_path("demo", "bravo").unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("opencode_session"));

        // The launch rewrites the existing status.yaml at once.
        let id = begin_opencode_session("demo", "bravo", "fix", false, "opencode").unwrap();
        let back = load_workspace_status("demo", "bravo").unwrap().unwrap();
        assert_eq!(back.opencode_session.as_deref(), Some(id.as_str()));
        assert!(std::fs::read_to_string(&path).unwrap().contains(&format!("opencode_session: {id}")));

        // A poll that rebuilds the status from scratch keeps it.
        save_workspace_status("demo", &status("fix")).unwrap();
        let back = load_workspace_status("demo", "bravo").unwrap().unwrap();
        assert_eq!(back.opencode_session.as_deref(), Some(id.as_str()));

        // A status for another task doesn't borrow this task's session.
        save_workspace_status("demo", &status("other")).unwrap();
        assert_eq!(load_workspace_status("demo", "bravo").unwrap().unwrap().opencode_session, None);

        // A slot relaunched on another runner drops it.
        save_workspace_status("demo", &status("fix")).unwrap();
        end_opencode_session("demo", "bravo").unwrap();
        assert_eq!(load_workspace_status("demo", "bravo").unwrap().unwrap().opencode_session, None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn ending_a_session_on_a_slot_that_never_ran_opencode_writes_nothing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        end_opencode_session("demo", "bravo").unwrap();
        assert!(!opencode_sessions_path("demo", "bravo").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }
}
