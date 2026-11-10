//! The daemon's half of a client's project-state write
//! ([`shelbi_proto::control::StateWrite`], control protocol v7).
//!
//! The board's workspace filter and column overrides, Zen Mode, the error log
//! and its read mark, and a pane's supervision reopen markers used to be
//! written by whichever client changed them, straight to this machine's disk.
//! A remote window can't do that, so every client asks the daemon instead and
//! the daemon writes here: one writer per machine, and one path for a window
//! on this machine or on another one. The daemon's control socket calls
//! [`apply`] and then announces the change to its other subscribers.
//!
//! Each write is the same `shelbi_state` call the client used to make, so the
//! files keep their shape and every reader is unchanged.

use shelbi_proto::control::{ColumnOverride, RelaunchPane, StateWrite};
use shelbi_state::supervision_relaunch as sr;

/// Make `write` to `project`'s state on this machine. `Err` carries the
/// message a client shows (or logs) verbatim.
pub fn apply(project: &str, write: &StateWrite) -> Result<(), String> {
    let result = match write {
        StateWrite::SetWorkspaceFilter { filter } => {
            shelbi_state::set_workspace_filter(project, filter.as_deref())
        }
        StateWrite::SetColumnOverride {
            workflow,
            status,
            value,
        } => shelbi_state::set_kanban_column_override(project, workflow, status, value.map(column_override)),
        // `toggle_zen_mode` keeps its own daemon-version guard; here it asks
        // this daemon about itself, which always matches.
        StateWrite::ToggleZen { source } => shelbi_state::toggle_zen_mode(project, source).map(|_| ()),
        StateWrite::AppendError { message, source } => {
            shelbi_state::append_error(project, message, source.as_deref())
        }
        StateWrite::ClearErrors => shelbi_state::clear_errors(project),
        StateWrite::MarkErrorsRead => shelbi_state::mark_errors_read(project),
        StateWrite::RequestRelaunch { pane } => request_relaunch(project, pane),
        StateWrite::RestartOrchestrator => crate::restart_orchestrator(project).map(|_| ()),
    };
    result.map_err(|e| e.to_string())
}

fn column_override(value: ColumnOverride) -> shelbi_state::KanbanColumnOverride {
    match value {
        ColumnOverride::Collapsed => shelbi_state::KanbanColumnOverride::Collapsed,
        ColumnOverride::Expanded => shelbi_state::KanbanColumnOverride::Expanded,
    }
}

/// Drop the reopen markers that reset `pane`'s supervision budget. A
/// workspace pane can be governed by more than one supervision pass (the
/// per-workspace supervisor and the stranded-dev / stranded-review resume
/// passes), and the client can't tell which gave up, so every key that could
/// apply is written; each pass consumes only its own.
fn request_relaunch(project: &str, pane: &RelaunchPane) -> shelbi_core::Result<()> {
    match pane {
        RelaunchPane::Orchestrator => sr::request_supervision_relaunch(project, &sr::orchestrator_relaunch_key()),
        RelaunchPane::Workspace { workspace: w } => {
            sr::request_supervision_relaunch(project, &sr::workspace_supervise_relaunch_key(w))?;
            sr::request_supervision_relaunch(project, &sr::workspace_resume_relaunch_key(w))?;
            sr::request_supervision_relaunch(project, &sr::review_resume_relaunch_key(w))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = "demo";

    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev_home: Option<std::ffi::OsString>,
        home: std::path::PathBuf,
    }

    impl HomeGuard {
        fn new() -> Self {
            let lock = crate::test_lock::acquire();
            let home = std::env::temp_dir().join(format!(
                "shelbi-orch-state-write-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(home.join("projects").join(PROJECT)).unwrap();
            let prev_home = std::env::var_os("SHELBI_HOME");
            std::env::set_var("SHELBI_HOME", &home);
            HomeGuard {
                _lock: lock,
                prev_home,
                home,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn the_board_writes_land_in_state_json() {
        let _home = HomeGuard::new();
        apply(
            PROJECT,
            &StateWrite::SetWorkspaceFilter {
                filter: Some("ws:alpha".into()),
            },
        )
        .unwrap();
        apply(
            PROJECT,
            &StateWrite::SetColumnOverride {
                workflow: "task".into(),
                status: "done".into(),
                value: Some(ColumnOverride::Expanded),
            },
        )
        .unwrap();
        let state = shelbi_state::read_state(PROJECT).unwrap();
        assert_eq!(state.workspace_filter.as_deref(), Some("ws:alpha"));
        let key = shelbi_state::kanban_column_override_key("task", "done");
        assert_eq!(
            state.kanban_column_overrides.get(&key),
            Some(&shelbi_state::KanbanColumnOverride::Expanded)
        );

        apply(PROJECT, &StateWrite::SetWorkspaceFilter { filter: None }).unwrap();
        apply(
            PROJECT,
            &StateWrite::SetColumnOverride {
                workflow: "task".into(),
                status: "done".into(),
                value: None,
            },
        )
        .unwrap();
        let state = shelbi_state::read_state(PROJECT).unwrap();
        assert_eq!(state.workspace_filter, None);
        assert!(state.kanban_column_overrides.is_empty());
    }

    #[test]
    fn zen_flips_and_the_error_log_appends_marks_and_clears() {
        let _home = HomeGuard::new();
        let zen = || shelbi_state::read_state(PROJECT).unwrap().zen_mode;
        let toggle = StateWrite::ToggleZen {
            source: "user:palette".into(),
        };
        apply(PROJECT, &toggle).unwrap();
        assert_eq!(zen(), shelbi_state::ZenModeState::On);
        apply(PROJECT, &toggle).unwrap();
        assert_eq!(zen(), shelbi_state::ZenModeState::Off);

        apply(
            PROJECT,
            &StateWrite::AppendError {
                message: "Zen toggle failed: x".into(),
                source: Some("zen".into()),
            },
        )
        .unwrap();
        let errors = shelbi_state::read_errors(PROJECT).unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].source.as_deref(), Some("zen"));
        assert_eq!(shelbi_state::unread_error_count(PROJECT).unwrap(), 1);
        apply(PROJECT, &StateWrite::MarkErrorsRead).unwrap();
        assert_eq!(shelbi_state::unread_error_count(PROJECT).unwrap(), 0);
        apply(PROJECT, &StateWrite::ClearErrors).unwrap();
        assert!(shelbi_state::read_errors(PROJECT).unwrap().is_empty());
    }

    #[test]
    fn a_relaunch_request_writes_every_key_a_pass_could_consume() {
        let _home = HomeGuard::new();
        apply(
            PROJECT,
            &StateWrite::RequestRelaunch {
                pane: RelaunchPane::Orchestrator,
            },
        )
        .unwrap();
        assert!(sr::consume_supervision_relaunch(PROJECT, &sr::orchestrator_relaunch_key()).unwrap());

        apply(
            PROJECT,
            &StateWrite::RequestRelaunch {
                pane: RelaunchPane::Workspace {
                    workspace: "alpha".into(),
                },
            },
        )
        .unwrap();
        for key in [
            sr::workspace_supervise_relaunch_key("alpha"),
            sr::workspace_resume_relaunch_key("alpha"),
            sr::review_resume_relaunch_key("alpha"),
        ] {
            assert!(sr::consume_supervision_relaunch(PROJECT, &key).unwrap(), "{key}");
        }
        assert!(!sr::consume_supervision_relaunch(PROJECT, &sr::orchestrator_relaunch_key()).unwrap());
    }
}
