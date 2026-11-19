//! Deleting an issue from a client: what it really does on the project's
//! tracker, the words that say so, and when it isn't offered.
//!
//! The delete itself is `shelbi issue rm`'s ([`crate::CommandKind::DeleteIssue`],
//! the daemon's `delete` mutation): the filesystem board removes the issue,
//! and a tracker that can't delete does its nearest durable effect instead
//! (`shelbi_state`'s stores). So the menu row and the confirm describe that
//! effect, never a delete the tracker didn't do ([`DeleteEffect`]).
//!
//! An issue a workspace is working on (an active status) isn't offered:
//! deleting it would strand the worker ([`delete_refusal`]).

use shelbi_core::{IssueTrackerBackend, StatusCategory};

use crate::board::BoardInput;

/// What deleting an issue does on the project's tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteEffect {
    /// The issue is removed (the filesystem board).
    Delete,
    /// GitHub: only a repository admin can delete an issue, so it is closed
    /// as not planned.
    CloseNotPlanned,
    /// Linear: the issue moves to the team's Canceled status.
    Cancel,
}

impl DeleteEffect {
    /// The effect `backend`'s store gives `delete` (`IssueStore::delete`).
    pub fn for_tracker(backend: IssueTrackerBackend) -> Self {
        match backend {
            IssueTrackerBackend::Github => DeleteEffect::CloseNotPlanned,
            IssueTrackerBackend::Linear => DeleteEffect::Cancel,
            IssueTrackerBackend::FileSystem | IssueTrackerBackend::Jira => DeleteEffect::Delete,
        }
    }

    /// The menu row that asks for it (it opens a confirm, so it ends in `…`).
    pub fn menu_label(self) -> &'static str {
        match self {
            DeleteEffect::Delete => "Delete issue…",
            DeleteEffect::CloseNotPlanned => "Close issue…",
            DeleteEffect::Cancel => "Move to Canceled…",
        }
    }

    /// The confirm's question, naming the issue.
    pub fn confirm_title(self, title: &str) -> String {
        match self {
            DeleteEffect::Delete => format!("Delete “{title}”?"),
            DeleteEffect::CloseNotPlanned => format!("Close “{title}”?"),
            DeleteEffect::Cancel => format!("Move “{title}” to Canceled?"),
        }
    }

    /// What the confirm says happens.
    pub fn confirm_body(self) -> &'static str {
        match self {
            DeleteEffect::Delete => "This removes the issue from the board. It can’t be undone.",
            DeleteEffect::CloseNotPlanned => {
                "GitHub only lets repository admins delete issues, so this closes the issue as not planned on GitHub. You can reopen it there."
            }
            DeleteEffect::Cancel => {
                "Shelbi doesn’t delete Linear issues, so this moves the issue to your team’s Canceled status in Linear. You can reopen it there."
            }
        }
    }

    /// The confirm's button.
    pub fn confirm_label(self) -> &'static str {
        match self {
            DeleteEffect::Delete => "Delete",
            DeleteEffect::CloseNotPlanned => "Close issue",
            DeleteEffect::Cancel => "Move to Canceled",
        }
    }

    /// What the card says while it's in flight.
    pub fn progress_label(self) -> &'static str {
        match self {
            DeleteEffect::Delete => "Deleting…",
            DeleteEffect::CloseNotPlanned => "Closing…",
            DeleteEffect::Cancel => "Canceling…",
        }
    }

    /// The status line once it's done.
    pub fn done_line(self, id: &str) -> String {
        match self {
            DeleteEffect::Delete => format!("{id} deleted"),
            DeleteEffect::CloseNotPlanned => format!("{id} closed as not planned"),
            DeleteEffect::Cancel => format!("{id} moved to Canceled"),
        }
    }

    /// The status line's prefix when it fails.
    pub fn failed_line(self) -> &'static str {
        match self {
            DeleteEffect::Delete => "delete failed",
            DeleteEffect::CloseNotPlanned => "close failed",
            DeleteEffect::Cancel => "cancel failed",
        }
    }
}

/// What deleting an issue on `input`'s board does.
pub fn effect(input: &BoardInput) -> DeleteEffect {
    DeleteEffect::for_tracker(input.tracker)
}

/// Why issue `id` can't be deleted now, when it can't (the row is drawn
/// dimmed, with this on hover): it is in an active status, so a workspace is
/// working on it. `None` when it can, or when it isn't on the board.
pub fn delete_refusal(input: &BoardInput, id: &str) -> Option<String> {
    let tasks = input.tasks();
    let tf = tasks.iter().find(|tf| tf.task.id == id)?;
    let status_id = input.rules().resolved_status_id(&tf.task);
    let status = input.project_statuses.statuses.iter().find(|st| st.id == status_id);
    let category = status.map_or_else(|| tf.task.column.category(), |st| st.category);
    if category != StatusCategory::Active {
        return None;
    }
    let name = status.map_or_else(|| tf.task.column.default_status_name().to_string(), |st| st.name.clone());
    let verb = match effect(input) {
        DeleteEffect::Delete => "deleting",
        DeleteEffect::CloseNotPlanned => "closing",
        DeleteEffect::Cancel => "canceling",
    };
    Some(format!(
        "A workspace is working on this issue ({name}). Stop it or move the issue out of {name} before {verb} it."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{BoardOutcome, BoardProject, BoardRead};
    use shelbi_core::{default_project_statuses, default_workflow, Column, Issue};
    use shelbi_state::OwnershipGate;
    use shelbi_state::IssueFile;

    fn issue(id: &str, column: Column) -> IssueFile {
        let now = chrono::Utc::now();
        IssueFile {
            task: Issue {
                id: id.into(),
                title: id.into(),
                column,
                priority: 0,
                assigned_to: None,
                workflow: None,
                branch: None,
                pr: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: now,
                updated_at: now,
                params: Default::default(),
            },
            body: String::new(),
            tracker_assignees: Vec::new(),
        }
    }

    fn input(tracker: IssueTrackerBackend, tasks: Vec<IssueFile>) -> BoardInput {
        let mut input = BoardInput::default();
        input.apply_open(BoardRead {
            project: Some(BoardProject {
                default_workflow_name: "default".into(),
                display_name: None,
                workspaces: Vec::new(),
                ownership: OwnershipGate::Unrestricted,
                remote_search: false,
                tracker,
            }),
            state_snapshot: None,
            workflows: Ok(vec![default_workflow()]),
            project_statuses: Ok(default_project_statuses()),
            board: BoardOutcome::Warm {
                banner: None,
                base_tasks: tasks,
            },
            estimates: None,
        });
        input
    }

    #[test]
    fn each_tracker_says_what_its_delete_really_does() {
        let fs = input(IssueTrackerBackend::FileSystem, Vec::new());
        assert_eq!(effect(&fs), DeleteEffect::Delete);
        assert_eq!(effect(&fs).menu_label(), "Delete issue…");
        assert_eq!(effect(&fs).confirm_title("Fix it"), "Delete “Fix it”?");
        assert!(effect(&fs).confirm_body().contains("can’t be undone"));

        let gh = effect(&input(IssueTrackerBackend::Github, Vec::new()));
        assert_eq!(gh, DeleteEffect::CloseNotPlanned);
        assert_eq!(gh.menu_label(), "Close issue…");
        assert_eq!(gh.confirm_label(), "Close issue");
        assert!(gh.confirm_body().contains("closes the issue as not planned on GitHub"));

        let linear = effect(&input(IssueTrackerBackend::Linear, Vec::new()));
        assert_eq!(linear, DeleteEffect::Cancel);
        assert_eq!(linear.menu_label(), "Move to Canceled…");
        assert!(linear.confirm_body().contains("Canceled status in Linear"));
    }

    #[test]
    fn an_active_issue_is_refused_and_says_why() {
        let board = input(
            IssueTrackerBackend::FileSystem,
            vec![issue("busy", Column::in_progress()), issue("idle", Column::todo())],
        );
        let why = delete_refusal(&board, "busy").expect("an active issue is refused");
        assert!(why.contains("A workspace is working on this issue"), "{why}");
        assert!(why.contains("Stop it or move the issue out of"), "{why}");
        assert_eq!(delete_refusal(&board, "idle"), None);
        assert_eq!(delete_refusal(&board, "gone"), None);
    }

    #[test]
    fn handoff_and_done_issues_can_be_deleted() {
        let board = input(
            IssueTrackerBackend::FileSystem,
            vec![issue("r", Column::review()), issue("d", Column::done())],
        );
        assert_eq!(delete_refusal(&board, "r"), None);
        assert_eq!(delete_refusal(&board, "d"), None);
    }
}
