//! Which running agent session belongs to a task: the session a task's
//! detail shows under its properties (the desktop's task pane,
//! `dt-task-pane-session`), and where its Open Workspace goes.
//!
//! The association is the one the sidebar already draws, never the task's
//! status: a dev workspace whose row says it is working the task
//! ([`WorkspaceRow::current_task`]), or the review slot a review task is loaded
//! onto ([`ReviewRow::workspace`]). An In Progress task assigned to a
//! workspace that is busy with another task has no session here, so the
//! pane never shows an unrelated agent; an idle task has none either, and
//! resolving one never starts anything.

use crate::command::CommandKind;
use crate::session::SessionRef;
use crate::view::{ReviewRow, ReviewState, WorkspaceBadge, WorkspaceRow};

/// The live state a task's session header shows: its workspace's badge, or,
/// for a review slot, the review's state (the sidebar's two glyph sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSessionState {
    Workspace(WorkspaceBadge),
    Review(ReviewState),
}

/// A task's running session: the workspace (or review slot) working it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSession {
    /// The task it belongs to.
    pub task: String,
    /// The dev workspace, or the review slot.
    pub workspace: String,
    /// The machine it runs on when that is a remote (SSH) host, `None` when
    /// local or unknown.
    pub remote_machine: Option<String>,
    pub state: TaskSessionState,
}

impl TaskSession {
    /// The session to attach: the workspace's (or slot's) agent.
    pub fn target(&self) -> SessionRef {
        SessionRef::Workspace(self.workspace.clone())
    }

    /// Whether it is a review slot's reviewer rather than a dev workspace.
    pub fn is_review(&self) -> bool {
        matches!(self.state, TaskSessionState::Review(_))
    }

    /// What Open Workspace runs: the workspace's view for a dev workspace,
    /// the review (which reuses the slot already serving it) for a review.
    pub fn open_command(&self) -> CommandKind {
        if self.is_review() {
            CommandKind::LoadReview {
                task_id: self.task.clone(),
            }
        } else {
            CommandKind::FocusWorkspace {
                workspace: self.workspace.clone(),
            }
        }
    }
}

/// The session working `task`, from the sidebar's rows.
///
/// A review task loaded onto a slot resolves to the slot (its reviewer is
/// the live work, even when the dev workspace's row hasn't moved on yet);
/// otherwise the dev workspace whose row is working the task. A queued
/// review, a task assigned to a workspace busy elsewhere, and a task nobody
/// is working resolve to `None`.
pub fn resolve(task: &str, workspaces: &[WorkspaceRow], reviews: &[ReviewRow]) -> Option<TaskSession> {
    let review = reviews
        .iter()
        .filter(|r| r.task_id == task && r.state != ReviewState::Pending)
        .find_map(|r| r.workspace.as_deref().filter(|w| !w.is_empty()).map(|w| (w, r.state)));
    if let Some((slot, state)) = review {
        let row = workspaces.iter().find(|w| w.name == slot);
        return Some(TaskSession {
            task: task.to_string(),
            workspace: slot.to_string(),
            remote_machine: row.and_then(remote_machine),
            state: TaskSessionState::Review(state),
        });
    }
    workspaces
        .iter()
        .find(|w| w.current_task.as_deref() == Some(task))
        .map(|w| TaskSession {
            task: task.to_string(),
            workspace: w.name.clone(),
            remote_machine: remote_machine(w),
            state: TaskSessionState::Workspace(w.badge),
        })
}

fn remote_machine(row: &WorkspaceRow) -> Option<String> {
    (row.is_remote && !row.machine.is_empty()).then(|| row.machine.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str, task: Option<&str>, badge: WorkspaceBadge) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            machine: "hub".into(),
            is_remote: false,
            current_task: task.map(str::to_string),
            agent: None,
            badge,
        }
    }

    fn review(task: &str, slot: Option<&str>, state: ReviewState) -> ReviewRow {
        ReviewRow {
            task_id: task.into(),
            title: String::new(),
            branch: String::new(),
            location: None,
            workspace: slot.map(str::to_string),
            state,
            number: None,
        }
    }

    #[test]
    fn a_task_resolves_to_the_workspace_working_it() {
        let rows = [ws("alpha", Some("T1"), WorkspaceBadge::Working), ws("bravo", Some("T2"), WorkspaceBadge::AwaitingInput)];
        let s = resolve("T2", &rows, &[]).unwrap();
        assert_eq!(s.workspace, "bravo");
        assert_eq!(s.target(), SessionRef::Workspace("bravo".into()));
        assert_eq!(s.state, TaskSessionState::Workspace(WorkspaceBadge::AwaitingInput));
        assert_eq!(s.remote_machine, None);
        assert_eq!(s.open_command(), CommandKind::FocusWorkspace { workspace: "bravo".into() });
    }

    #[test]
    fn a_task_nobody_is_working_has_no_session() {
        // alpha is busy with T1: an assignment or a status alone doesn't
        // attach its agent to T3.
        let rows = [ws("alpha", Some("T1"), WorkspaceBadge::Working), ws("bravo", None, WorkspaceBadge::Idle)];
        assert_eq!(resolve("T3", &rows, &[]), None);
        assert_eq!(resolve("", &rows, &[]), None);
    }

    #[test]
    fn a_remote_workspace_names_its_machine() {
        let mut row = ws("gpu-1", Some("T1"), WorkspaceBadge::Working);
        row.is_remote = true;
        row.machine = "gpu".into();
        assert_eq!(resolve("T1", &[row], &[]).unwrap().remote_machine.as_deref(), Some("gpu"));
    }

    #[test]
    fn a_loaded_review_resolves_to_its_slot_and_opens_the_review() {
        let rows = [ws("alpha", Some("T1"), WorkspaceBadge::Idle)];
        let reviews = [review("T1", Some("review-1"), ReviewState::Serving)];
        let s = resolve("T1", &rows, &reviews).unwrap();
        assert_eq!(s.workspace, "review-1");
        assert_eq!(s.state, TaskSessionState::Review(ReviewState::Serving));
        assert!(s.is_review());
        assert_eq!(s.open_command(), CommandKind::LoadReview { task_id: "T1".into() });
    }

    #[test]
    fn a_queued_review_has_no_session_of_its_own() {
        let reviews = [review("T1", None, ReviewState::Pending), review("T2", Some("review-1"), ReviewState::Pending)];
        assert_eq!(resolve("T1", &[], &reviews), None);
        assert_eq!(resolve("T2", &[], &reviews), None);
        // The dev workspace still working it is the session meanwhile.
        let rows = [ws("alpha", Some("T1"), WorkspaceBadge::Working)];
        assert_eq!(resolve("T1", &rows, &reviews).unwrap().workspace, "alpha");
    }
}
