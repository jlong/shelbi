//! The executor seam.
//!
//! A command in the [registry](crate::command) does not *do* anything by
//! itself — invoking it yields an [`Effect`], a piece of plain data that
//! names what should happen. The host (the CLI today, the daemon-backed TUI
//! later) turns an `Effect` into real work through the [`Executor`] trait.
//!
//! This indirection is the point: the mutating effects are carried as
//! [`Effect::Mutate`] with a typed [`Mutation`], so `rt-mutations-daemon`
//! can implement [`Executor`] by routing those to the daemon's control
//! socket, while a local implementation runs the same library / CLI path
//! Shelbi uses today. Nothing in this module performs IO.

use crate::nav::View;

/// A resolved command's side effect — what the host should do. Pure data,
/// no behavior. The navigation effects are client-local view changes; the
/// launch/lifecycle effects are things the host performs (possibly with
/// UI); [`Effect::Mutate`] is the daemon-routable issue/state mutation set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Change the main view (a client-local navigation change). Carries the
    /// target [`View`]; `Session` views name the session to bind.
    ShowView(View),
    /// Focus a workspace, launching its pane lazily if needed.
    FocusWorkspace { project: String, workspace: String },
    /// Load a task into a review slot and show it.
    LoadReview { project: String, task_id: String },
    /// Focus an already-running session (e.g. a legacy spawned agent).
    FocusSession { session: String },
    /// Open an editable config/instructions target in the user's editor.
    OpenEditor { target: EditTarget },
    /// Open the per-project error log.
    OpenErrorLog { project: String },
    /// Switch the client to another project.
    SwitchProject { project: String },
    /// Begin the add-project flow (the host collects the form, then calls
    /// [`Mutation::AddProject`]).
    AddProject,
    /// Quit a project: end its sessions and mark it closed. The host
    /// confirms first.
    QuitProject { project: String },
    /// Quit Shelbi entirely: end all sessions and stop the daemon. The host
    /// confirms first.
    QuitShelbi,
    /// A daemon-routable mutation (issue moves, review decisions, Zen
    /// toggle, project creation). This is the variant `rt-mutations-daemon`
    /// routes to the daemon.
    Mutate(Mutation),
}

/// An editable on-disk target the palette's `edit:*` commands open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    /// The project settings YAML.
    Project,
    /// A named agent's `instructions.md`.
    Agent(String),
    /// The project's `zenmode.md`.
    ZenMode,
    /// The project's `workflows/` directory.
    Workflows,
}

/// A state mutation routed through the [`Executor`]. These are the
/// operations the plan moves behind the daemon's control socket with
/// per-issue queuing, expected state, and recheck before irreversible
/// steps. `rt-mutations-daemon` owns the daemon-side routing; here they are
/// just typed requests.
///
/// The issue mutations (`MoveIssue` … `AddIssue`) are the generic set the
/// plan names. They are not surfaced in the tmux palette today — the
/// registry carries them so the single-process TUI and the desktop app can
/// bind them without a second definition — while [`Mutation::ToggleZen`]
/// and [`Mutation::AddProject`] are the lifecycle mutations the current
/// palette already performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    /// Move an issue to another status, running the workflow's gated merge
    /// when the move crosses a merge edge.
    MoveIssue {
        project: String,
        id: String,
        to_status: String,
    },
    /// Start an issue: dispatch it to a workspace agent.
    StartIssue { project: String, id: String },
    /// Assign an issue to a named workspace.
    AssignIssue {
        project: String,
        id: String,
        workspace: String,
    },
    /// Approve (accept) a review task, running the gated merge.
    ApproveReview { project: String, id: String },
    /// Reject a review task with a reason.
    RejectReview {
        project: String,
        id: String,
        reason: String,
    },
    /// Edit an issue's fields (title/body/priority/etc.).
    EditIssue {
        project: String,
        id: String,
        title: Option<String>,
        body: Option<String>,
    },
    /// Add a new issue.
    AddIssue {
        project: String,
        title: String,
        workflow: Option<String>,
    },
    /// Toggle Zen Mode for a project.
    ToggleZen { project: String },
    /// Scaffold and open a new project (payload collected by the host's
    /// add-project form).
    AddProject {
        name: String,
        root: String,
    },
}

/// The result of running an [`Effect`].
pub type ExecOutcome = Result<(), ExecError>;

/// Why running an effect failed. The host maps real errors (IO, `gh`, git,
/// a daemon round-trip) into these; the variants stay coarse so the app
/// model does not couple to any one backend's error type.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ExecError {
    /// The command referenced something that no longer exists (a task that
    /// moved, a workspace that was removed).
    #[error("not found: {0}")]
    NotFound(String),
    /// The command's expected precondition no longer holds (optimistic
    /// concurrency: another client changed the state first).
    #[error("state changed: {0}")]
    Conflict(String),
    /// The backend refused or failed the operation.
    #[error("{0}")]
    Backend(String),
}

/// The seam the registry calls to perform a command's [`Effect`].
///
/// A host implements this once. The local implementation (CLI / in-process
/// TUI) runs the same library or CLI path used today; `rt-mutations-daemon`
/// will implement it by routing [`Effect::Mutate`] to the daemon while
/// handling the navigation effects client-side.
pub trait Executor {
    /// Perform `effect`. Returns `Ok(())` when the effect was carried out
    /// (or scheduled, for async daemon work the host tracks separately).
    fn run(&mut self, effect: Effect) -> ExecOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recording executor used by the command-registry tests: it captures
    /// the effects it is handed and reports a configurable outcome.
    #[derive(Default)]
    pub struct RecordingExecutor {
        pub seen: Vec<Effect>,
        pub fail_next: Option<ExecError>,
    }

    impl Executor for RecordingExecutor {
        fn run(&mut self, effect: Effect) -> ExecOutcome {
            self.seen.push(effect);
            match self.fail_next.take() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn recording_executor_captures_effects_in_order() {
        let mut ex = RecordingExecutor::default();
        assert!(ex.run(Effect::ShowView(View::Issues)).is_ok());
        assert!(ex
            .run(Effect::Mutate(Mutation::ToggleZen {
                project: "alpha".into()
            }))
            .is_ok());
        assert_eq!(ex.seen.len(), 2);
        assert_eq!(ex.seen[0], Effect::ShowView(View::Issues));
    }

    #[test]
    fn executor_surfaces_a_configured_failure_once() {
        let mut ex = RecordingExecutor {
            fail_next: Some(ExecError::Conflict("moved".into())),
            ..Default::default()
        };
        let out = ex.run(Effect::QuitShelbi);
        assert!(matches!(out, Err(ExecError::Conflict(_))));
        // The failure is one-shot; the next effect succeeds.
        assert!(ex.run(Effect::QuitShelbi).is_ok());
    }
}
