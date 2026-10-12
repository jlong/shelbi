//! Per-workspace tmux → session-backend migration tracking for the Phase 6
//! cutover (`rt-cutover-migration`, plan "Phase 6: Cutover").
//!
//! The cutover flips the runtime default from tmux to the session-process
//! backend (see [`crate::session_backend_enabled`]). A worktree must be proven
//! *idle* before the new backend may start an agent in it — otherwise a
//! surviving tmux agent and a fresh session-process agent could both edit the
//! same checkout. Each workspace therefore carries a migration state:
//!
//! - [`MigrationState::Pending`] — a legacy tmux session for this workspace may
//!   still be live (a surviving remote `shelbi-w-<ws>` the hub has not yet
//!   reached and confirmed gone, or an unreachable machine). Dispatch to a
//!   pending workspace is refused; the rest of the project keeps working.
//! - [`MigrationState::Migrated`] — the legacy session is confirmed gone (local
//!   `shelbi-<p>` absent or tmux not installed; remote `shelbi-w-<ws>` confirmed
//!   absent on its machine). The new backend may start agents here.
//!
//! The state is persisted inside the project's `state.json`
//! ([`crate::State::workspace_migration`]) so it survives hub restarts and the
//! daemon can read it without re-probing every machine. An **absent** entry
//! means "not yet evaluated"; the dispatch gate treats that as not-yet-migrated
//! (refuse) so the new backend never races an un-checked worktree. The
//! open-time migration pass writes an explicit entry for every declared
//! workspace, so after a project opens on the new runtime `shelbi workspace
//! list` can show which workspaces are still pending.
//!
//! This whole module is cutover scaffolding: once every install is on the new
//! runtime, `rt-cutover-delete` removes it along with the hidden backend
//! setting.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use shelbi_core::Result;

use crate::{read_state, update_state};

/// Where a workspace sits in the tmux → session-backend migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MigrationState {
    /// A legacy tmux session for this workspace may still be live; the new
    /// backend must not start an agent here yet.
    Pending,
    /// The legacy tmux session is confirmed gone; the new backend may start
    /// agents here.
    Migrated,
}

impl MigrationState {
    pub fn as_str(self) -> &'static str {
        match self {
            MigrationState::Pending => "pending",
            MigrationState::Migrated => "migrated",
        }
    }

    /// Whether a worktree in this state is proven idle enough for the new
    /// backend to start an agent in it.
    pub fn is_migrated(self) -> bool {
        matches!(self, MigrationState::Migrated)
    }
}

impl std::fmt::Display for MigrationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The recorded migration state for one workspace, or `None` when the project
/// has no entry for it yet (never evaluated). A missing `state.json` reads as
/// `None` for every workspace.
pub fn workspace_migration_state(project: &str, workspace: &str) -> Result<Option<MigrationState>> {
    Ok(read_state(project)?
        .workspace_migration
        .get(workspace)
        .copied())
}

/// Record `state` for `workspace`, returning the prior value (if any). Routed
/// through [`update_state`] so a concurrent writer to another field of the same
/// project's `state.json` (a heartbeat tick, a filter change) is never lost.
/// Idempotent: writing the same value the workspace already holds is a no-op.
pub fn set_workspace_migration_state(
    project: &str,
    workspace: &str,
    state: MigrationState,
) -> Result<Option<MigrationState>> {
    update_state(project, |s| {
        Ok(s.workspace_migration.insert(workspace.to_string(), state))
    })
}

/// Every recorded workspace migration state for `project`, keyed by workspace
/// name. Empty when the project has no `state.json` or no entries yet.
pub fn all_workspace_migration_states(project: &str) -> Result<BTreeMap<String, MigrationState>> {
    Ok(read_state(project)?.workspace_migration)
}

/// Whether the new session backend is allowed to start an agent in `workspace`.
///
/// True only when the workspace is explicitly [`MigrationState::Migrated`]. An
/// absent entry (never evaluated) or [`MigrationState::Pending`] both read as
/// *not* dispatchable, so the new backend never starts an agent in a worktree a
/// surviving tmux agent might still hold. Callers gate on this only while the
/// session backend is active; on the tmux runtime there is nothing to migrate.
pub fn workspace_migrated(project: &str, workspace: &str) -> Result<bool> {
    Ok(workspace_migration_state(project, workspace)?
        .map(MigrationState::is_migrated)
        .unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-migration-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(p.join("projects").join("p")).unwrap();
        p
    }

    #[test]
    fn absent_entry_reads_as_none_and_not_migrated() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        assert!(workspace_migration_state("p", "alpha").unwrap().is_none());
        assert!(!workspace_migrated("p", "alpha").unwrap());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_and_read_round_trip_and_is_idempotent() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        assert_eq!(
            set_workspace_migration_state("p", "alpha", MigrationState::Pending).unwrap(),
            None
        );
        assert_eq!(
            workspace_migration_state("p", "alpha").unwrap(),
            Some(MigrationState::Pending)
        );
        assert!(!workspace_migrated("p", "alpha").unwrap());

        // Flip to migrated; the prior value is returned.
        assert_eq!(
            set_workspace_migration_state("p", "alpha", MigrationState::Migrated).unwrap(),
            Some(MigrationState::Pending)
        );
        assert!(workspace_migrated("p", "alpha").unwrap());

        // A second workspace is independent.
        set_workspace_migration_state("p", "beta", MigrationState::Pending).unwrap();
        let all = all_workspace_migration_states("p").unwrap();
        assert_eq!(all.get("alpha"), Some(&MigrationState::Migrated));
        assert_eq!(all.get("beta"), Some(&MigrationState::Pending));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn migration_map_omitted_from_state_json_when_empty() {
        // The field must stay out of a fresh `state.json` so an untouched
        // project's file is byte-identical to before this field existed.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let state = crate::State::default();
        crate::write_state("p", &state).unwrap();
        let text = std::fs::read_to_string(home.join("projects/p/state.json")).unwrap();
        assert!(
            !text.contains("workspace_migration"),
            "empty migration map must not serialize: {text}"
        );

        std::env::remove_var("SHELBI_HOME");
    }
}
