//! What a task's editable properties may be set to, for every client.
//!
//! The task pane edits Status, Workflow and Priority in place (no edit
//! mode): each value opens a picker of what the issue can be set to now,
//! and a choice becomes the registry command that does it. Which choices
//! are open, and why the others aren't, is decided here from the board's
//! read, with the board's own rules ([`crate::board`]):
//!
//! - **Status**: the statuses the issue's effective workflow declares, in
//!   `statuses.yaml` order, each legal or refused by [`board::drop_target`]
//!   (the same gate a drag uses), the merge edges marked. A choice is a
//!   [`CommandKind::MoveIssue`], so the daemon still runs the transition's
//!   actions and its gated merge.
//! - **Workflow**: every loaded workflow, the project default marked. One
//!   that doesn't declare the issue's stored status is refused with what to
//!   do first, so a change never strands the issue in a status its new
//!   workflow has no column for. A choice is a
//!   [`CommandKind::SetIssueWorkflow`] (the daemon's validated edit).
//! - **Priority**: the issue's place in its status's queue, 1-based for
//!   display (`2 in queue`), over the stored column's canonical order
//!   ([`board::reorder_slot`]). A choice is a [`CommandKind::PrioritizeIssue`]
//!   at the 0-based slot. A done column is ordered by completion, so it has
//!   no queue to reorder.

use shelbi_core::{StatusCategory, Workflow};
use shelbi_state::IssueFile;

use crate::board::{self, BoardInput, DropTarget};
use crate::CommandKind;

/// One status the Status picker lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChoice {
    pub status_id: String,
    /// As the workflow spells it (`In Progress`).
    pub name: String,
    pub category: StatusCategory,
    /// The issue's status now.
    pub current: bool,
    /// The move runs the workflow's gated merge.
    pub merge: bool,
    /// Why the issue can't move there, when it can't.
    pub refusal: Option<String>,
}

/// One workflow the Workflow picker lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowChoice {
    pub name: String,
    /// The project's default workflow (what an issue with no `workflow:`
    /// follows).
    pub default: bool,
    /// The issue's effective workflow now.
    pub current: bool,
    /// Why the issue can't move to it, when it can't.
    pub refusal: Option<String>,
}

/// Where an issue sits in its status's queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuePosition {
    /// 1-based: the top of the queue is 1.
    pub position: u32,
    /// How many issues the queue holds, this one included.
    pub len: u32,
}

impl QueuePosition {
    /// The 0-based slot `PrioritizeIssue` takes for display position
    /// `position` (1 is slot 0).
    pub fn slot_for(position: u32) -> u32 {
        position.saturating_sub(1)
    }
}

/// One place the Priority picker lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityChoice {
    /// 1-based.
    pub position: u32,
    /// Where the issue is now.
    pub current: bool,
    /// The title of the other issue at that place now (the issue takes its
    /// place and it moves along), `None` for the issue's own place.
    pub occupant: Option<String>,
}

fn task<'a>(tasks: &'a [IssueFile], id: &str) -> Option<&'a IssueFile> {
    tasks.iter().find(|tf| tf.task.id == id)
}

/// The Status picker's choices for issue `id`: its effective workflow's
/// statuses in board order. Empty when the issue isn't on the board; every
/// status but the current one refused, when its workflow isn't loaded.
pub fn status_choices(input: &BoardInput, id: &str) -> Vec<StatusChoice> {
    let tasks = input.tasks();
    let Some(tf) = task(&tasks, id) else {
        return Vec::new();
    };
    let rules = input.rules();
    let current = rules.resolved_status_id(&tf.task);
    let workflow = rules.workflow_for(&tf.task);
    input
        .columns(None)
        .into_iter()
        // An unloaded workflow lists every status, each refused with why.
        .filter(|c| workflow.is_none_or(|wf| wf.status(&c.status_id).is_some()))
        .map(|c| {
            let is_current = c.status_id == current;
            let (merge, refusal) = if is_current {
                (false, None)
            } else {
                match board::drop_target(rules, &tf.task, &c.status_id, c.category) {
                    DropTarget::Move { merge } => (merge, None),
                    DropTarget::Reorder => (false, None),
                    DropTarget::Refused(why) => (false, Some(why)),
                }
            };
            StatusChoice {
                status_id: c.status_id,
                name: c.status_name,
                category: c.category,
                current: is_current,
                merge,
                refusal,
            }
        })
        .collect()
}

/// The move to status `status_id` for issue `id`: `Ok(None)` when it is
/// already there, `Err` with the reason when the workflow refuses it.
pub fn status_command(input: &BoardInput, id: &str, status_id: &str) -> Result<Option<CommandKind>, String> {
    let choices = status_choices(input, id);
    let choice = choices
        .iter()
        .find(|c| c.status_id == status_id)
        .ok_or_else(|| format!("{id}: its workflow has no `{status_id}` status"))?;
    if choice.current {
        return Ok(None);
    }
    if let Some(why) = &choice.refusal {
        return Err(format!("{id} can't move to {}: {why}", choice.name));
    }
    Ok(Some(CommandKind::MoveIssue {
        id: id.to_string(),
        to_status: status_id.to_string(),
    }))
}

/// Why issue `tf` can't move to `workflow`: it doesn't declare the status
/// the issue is stored in.
fn workflow_refusal(input: &BoardInput, tf: &IssueFile, workflow: &Workflow) -> Option<String> {
    let stored = tf.task.column.as_str();
    if workflow.status(stored).is_some() {
        return None;
    }
    let name = input
        .project_statuses
        .statuses
        .iter()
        .find(|st| st.id == stored)
        .map_or_else(|| stored.to_string(), |st| st.name.clone());
    Some(format!(
        "`{}` has no {name} status; move the task to a status it has first",
        workflow.name
    ))
}

/// The Workflow picker's choices for issue `id`: every loaded workflow in
/// load order. Empty when the issue isn't on the board.
pub fn workflow_choices(input: &BoardInput, id: &str) -> Vec<WorkflowChoice> {
    let tasks = input.tasks();
    let Some(tf) = task(&tasks, id) else {
        return Vec::new();
    };
    let effective = input.rules().workflow_name(&tf.task).to_string();
    input
        .workflows
        .iter()
        .map(|wf| {
            let current = wf.name == effective;
            WorkflowChoice {
                name: wf.name.clone(),
                default: wf.name == input.default_workflow_name,
                current,
                refusal: if current { None } else { workflow_refusal(input, tf, wf) },
            }
        })
        .collect()
}

/// The change of issue `id` to `workflow`: `Ok(None)` when that is already
/// its effective workflow (an issue with no `workflow:` already follows the
/// default), `Err` with the reason when it would strand the issue.
pub fn workflow_command(input: &BoardInput, id: &str, workflow: &str) -> Result<Option<CommandKind>, String> {
    let choices = workflow_choices(input, id);
    let choice = choices
        .iter()
        .find(|c| c.name == workflow)
        .ok_or_else(|| format!("workflow `{workflow}` isn't loaded"))?;
    if choice.current {
        return Ok(None);
    }
    if let Some(why) = &choice.refusal {
        return Err(format!("{id}: {why}"));
    }
    Ok(Some(CommandKind::SetIssueWorkflow {
        id: id.to_string(),
        workflow: workflow.to_string(),
    }))
}

/// Why issue `id`'s queue can't be reordered: a done column is ordered by
/// completion.
pub fn priority_refusal(input: &BoardInput, id: &str) -> Option<String> {
    let tasks = input.tasks();
    let tf = task(&tasks, id)?;
    let stored = tf.task.column.as_str();
    let category = input
        .project_statuses
        .statuses
        .iter()
        .find(|st| st.id == stored)
        .map_or_else(|| tf.task.column.category(), |st| st.category);
    (category == StatusCategory::Done).then(|| "done is ordered by completion".to_string())
}

/// The stored column `id` sits in, in the queue's order (priority, then id:
/// the order [`board::reorder_slot`] and the stores use).
fn queue<'a>(tasks: &'a [IssueFile], id: &str) -> Vec<&'a IssueFile> {
    let Some(tf) = task(tasks, id) else {
        return Vec::new();
    };
    let mut column: Vec<&IssueFile> = tasks.iter().filter(|o| o.task.column == tf.task.column).collect();
    column.sort_by(|a, b| a.task.priority.cmp(&b.task.priority).then_with(|| a.task.id.cmp(&b.task.id)));
    column
}

/// Where issue `id` sits in its status's queue, 1-based.
pub fn queue_position(tasks: &[IssueFile], id: &str) -> Option<QueuePosition> {
    let column = queue(tasks, id);
    let at = column.iter().position(|tf| tf.task.id == id)?;
    Some(QueuePosition {
        position: u32::try_from(at + 1).ok()?,
        len: u32::try_from(column.len()).ok()?,
    })
}

/// The Priority picker's choices for issue `id`: every place in its queue.
pub fn priority_choices(tasks: &[IssueFile], id: &str) -> Vec<PriorityChoice> {
    queue(tasks, id)
        .into_iter()
        .enumerate()
        .map(|(i, tf)| PriorityChoice {
            position: u32::try_from(i + 1).unwrap_or(u32::MAX),
            current: tf.task.id == id,
            occupant: (tf.task.id != id).then(|| tf.task.title.clone()),
        })
        .collect()
}

/// The reorder landing issue `id` at display position `position` (1-based)
/// of its queue: `Ok(None)` when it is already there, `Err` with the reason
/// when its queue can't be reordered or the place is past its end.
pub fn priority_command(input: &BoardInput, id: &str, position: u32) -> Result<Option<CommandKind>, String> {
    if let Some(why) = priority_refusal(input, id) {
        return Err(format!("{id} can't be reordered: {why}"));
    }
    let tasks = input.tasks();
    let at = queue_position(&tasks, id).ok_or_else(|| format!("{id} is no longer on the board"))?;
    if position == 0 || position > at.len {
        return Err(format!("{id}: its queue has {} places", at.len));
    }
    if position == at.position {
        return Ok(None);
    }
    Ok(Some(CommandKind::PrioritizeIssue {
        id: id.to_string(),
        position: QueuePosition::slot_for(position),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{BoardOutcome, BoardProject, BoardRead};
    use shelbi_core::{default_project_statuses, default_workflow, Column, Issue};
    use shelbi_state::OwnershipGate;

    fn issue(id: &str, status: &str, priority: u32) -> IssueFile {
        let now = chrono::Utc::now();
        IssueFile {
            task: Issue {
                id: id.into(),
                title: format!("Title {id}"),
                column: Column::from_status_id(status),
                priority,
                assigned_to: None,
                workflow: None,
                branch: None,
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

    /// A `subtask` workflow without review (todo, in-progress, done).
    fn subtask() -> Workflow {
        let mut wf = default_workflow();
        wf.name = "subtask".into();
        wf.statuses.retain(|s| s.id != "review" && s.id != "backlog");
        wf
    }

    fn input(tasks: Vec<IssueFile>) -> BoardInput {
        let mut input = BoardInput::default();
        input.apply_open(BoardRead {
            project: Some(BoardProject {
                default_workflow_name: "default".into(),
                display_name: None,
                workspaces: Vec::new(),
                ownership: OwnershipGate::Unrestricted,
                remote_search: false,
            }),
            state_snapshot: None,
            workflows: Ok(vec![default_workflow(), subtask()]),
            project_statuses: Ok(default_project_statuses()),
            board: BoardOutcome::Warm {
                banner: None,
                base_tasks: tasks,
            },
        });
        input
    }

    #[test]
    fn status_choices_are_the_workflows_statuses_with_the_current_marked() {
        let mut t = issue("a", "todo", 0);
        t.task.workflow = Some("subtask".into());
        let input = input(vec![t]);
        let choices = status_choices(&input, "a");
        let ids: Vec<&str> = choices.iter().map(|c| c.status_id.as_str()).collect();
        assert_eq!(ids, ["todo", "in-progress", "done", "canceled"]);
        assert!(choices[0].current);
        assert!(choices.iter().all(|c| c.refusal.is_none()));
        assert_eq!(status_command(&input, "a", "todo"), Ok(None));
        assert_eq!(
            status_command(&input, "a", "done"),
            Ok(Some(CommandKind::MoveIssue { id: "a".into(), to_status: "done".into() }))
        );
        // Review isn't its workflow's: not offered, and refused if asked for.
        assert!(status_command(&input, "a", "review").is_err());
    }

    #[test]
    fn an_unloaded_workflow_refuses_every_move_and_says_why() {
        let mut t = issue("a", "todo", 0);
        t.task.workflow = Some("gone".into());
        let input = input(vec![t]);
        let choices = status_choices(&input, "a");
        assert!(choices.len() > 1);
        for c in &choices {
            assert_eq!(c.current, c.status_id == "todo");
            assert_eq!(c.refusal.is_some(), !c.current, "{}", c.status_id);
        }
        assert!(choices[0].refusal.as_deref().unwrap().contains("isn't loaded"));
    }

    #[test]
    fn a_merge_edge_is_marked() {
        let input = input(vec![issue("a", "review", 0)]);
        let done = status_choices(&input, "a").into_iter().find(|c| c.status_id == "done").unwrap();
        assert_eq!(done.merge, default_workflow().is_merge_transition("review", "done"));
    }

    #[test]
    fn a_workflow_without_the_issues_status_is_refused() {
        let input = input(vec![issue("a", "review", 0), issue("b", "todo", 0)]);
        let a = workflow_choices(&input, "a");
        assert_eq!(a.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["default", "subtask"]);
        // No `workflow:`: it follows the project default.
        assert!(a[0].current && a[0].default);
        let why = a[1].refusal.as_deref().unwrap();
        assert!(why.contains("no Review status"), "{why}");
        assert!(workflow_command(&input, "a", "subtask").is_err());
        assert_eq!(workflow_command(&input, "a", "default"), Ok(None));
        // `todo` is in both: the change is the daemon's validated edit.
        assert_eq!(
            workflow_command(&input, "b", "subtask"),
            Ok(Some(CommandKind::SetIssueWorkflow { id: "b".into(), workflow: "subtask".into() }))
        );
        assert!(workflow_command(&input, "b", "nope").is_err());
    }

    #[test]
    fn an_explicit_workflow_is_current_and_the_default_is_a_change_back() {
        let mut t = issue("a", "todo", 0);
        t.task.workflow = Some("subtask".into());
        let input = input(vec![t]);
        let choices = workflow_choices(&input, "a");
        assert!(!choices[0].current && choices[0].default);
        assert!(choices[1].current && !choices[1].default);
        assert_eq!(
            workflow_command(&input, "a", "default"),
            Ok(Some(CommandKind::SetIssueWorkflow { id: "a".into(), workflow: "default".into() }))
        );
    }

    #[test]
    fn priority_is_the_1_based_place_in_the_stored_queue() {
        // Stored priorities need not be contiguous; ties go by id.
        let tasks = vec![issue("c", "todo", 7), issue("a", "todo", 2), issue("b", "todo", 7), issue("x", "backlog", 0)];
        let input = input(tasks.clone());
        assert_eq!(queue_position(&tasks, "a"), Some(QueuePosition { position: 1, len: 3 }));
        assert_eq!(queue_position(&tasks, "c"), Some(QueuePosition { position: 3, len: 3 }));
        assert_eq!(queue_position(&tasks, "x"), Some(QueuePosition { position: 1, len: 1 }));
        let choices = priority_choices(&tasks, "b");
        assert_eq!(choices.len(), 3);
        assert!(choices[1].current && choices[1].occupant.is_none());
        assert_eq!(choices[0].occupant.as_deref(), Some("Title a"));
        // Position 1 is slot 0; the boundaries hold.
        assert_eq!(
            priority_command(&input, "c", 1),
            Ok(Some(CommandKind::PrioritizeIssue { id: "c".into(), position: 0 }))
        );
        assert_eq!(
            priority_command(&input, "a", 3),
            Ok(Some(CommandKind::PrioritizeIssue { id: "a".into(), position: 2 }))
        );
        assert_eq!(priority_command(&input, "b", 2), Ok(None));
        assert!(priority_command(&input, "b", 0).is_err());
        assert!(priority_command(&input, "b", 4).is_err());
        // The command lands where the board's own reorder would.
        assert_eq!(board::reorder_slot(&tasks, "c", Some("a")), Some(0));
    }

    #[test]
    fn a_done_queue_is_not_reordered() {
        let input = input(vec![issue("a", "done", 0), issue("b", "done", 1)]);
        assert!(priority_refusal(&input, "a").is_some());
        assert!(priority_command(&input, "a", 2).is_err());
    }

    #[test]
    fn an_issue_not_on_the_board_has_no_choices() {
        let input = input(Vec::new());
        assert!(status_choices(&input, "a").is_empty());
        assert!(workflow_choices(&input, "a").is_empty());
        assert!(priority_choices(&input.tasks(), "a").is_empty());
    }
}
