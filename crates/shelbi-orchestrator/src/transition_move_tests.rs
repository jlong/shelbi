//! [`move_issue_with`] against a stub store and a stub [`TransitionRunner`]:
//! no git, no `gh`. Both stubs append to one shared call log, so a test can
//! assert not just *that* an action ran but where it ran relative to the
//! status write — the property the gated merge exists for.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shelbi_core::Issue;
use shelbi_state::issue_store::{
    Cursor, IssueChange, IssueComment, IssueFields, NewIssue, PrioMove, StatusMove,
};

use super::super::tests::{bare_project, bare_task};
use super::*;
use crate::test_lock;

/// `bare_project()`'s name — the project the fixture home holds.
const PROJECT: &str = "fixture";

/// The workflow every test issue runs under: a `review -> done` accept edge
/// with a pre-merge prefix, the merge, and a post-merge cleanup action, plus
/// an `in-progress -> review` edge that declares no merge.
const WORKFLOW: &str = r#"name: mergewf
statuses:
  - { id: backlog,     owner: user                       }
  - { id: todo,        owner: agent, agent: orchestrator  }
  - { id: in-progress, owner: agent, agent: developer     }
  - { id: review,      owner: user                        }
  - { id: done,        owner: user                        }
transitions:
  - { from: in-progress, to: review, actions: [push_branch, open_pr] }
  - { from: review, to: done, actions: [push_branch, merge, delete_branch] }
"#;

type CallLog = Arc<Mutex<Vec<String>>>;

/// An isolated `SHELBI_HOME` holding the fixture project and its workflow —
/// the config `move_issue_with` reads through `shelbi_state`. The issue itself
/// lives only in the [`StubStore`].
struct Fixture {
    home: PathBuf,
    log: CallLog,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let lock = test_lock::acquire();
        let home = std::env::temp_dir().join(format!(
            "shelbi-move-issue-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&bare_project()).unwrap();
        let wf_dir = shelbi_state::workflows_dir(PROJECT).unwrap();
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(wf_dir.join("mergewf.yaml"), WORKFLOW).unwrap();
        Self {
            home,
            log: CallLog::default(),
            _lock: lock,
        }
    }

    fn store(&self, column: Column) -> StubStore {
        let mut task = bare_task("t");
        task.column = column;
        task.workflow = Some("mergewf".into());
        StubStore {
            issue: Mutex::new(task),
            log: self.log.clone(),
        }
    }

    fn runner(&self) -> StubRunner {
        StubRunner {
            log: self.log.clone(),
            fail: None,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// The lines written to `events.log` (empty when it was never created).
    fn events(&self) -> Vec<String> {
        std::fs::read_to_string(shelbi_state::events_log_path().unwrap())
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// A one-issue in-memory board. Only the two calls a move makes are live;
/// anything else is a test bug.
struct StubStore {
    issue: Mutex<Issue>,
    log: CallLog,
}

impl StubStore {
    fn column(&self) -> Column {
        self.issue.lock().unwrap().column.clone()
    }
}

impl IssueStore for StubStore {
    fn get(&self, id: &str) -> Result<Option<IssueFile>> {
        let issue = self.issue.lock().unwrap();
        Ok((issue.id == id).then(|| IssueFile {
            task: issue.clone(),
            body: "body".into(),
            tracker_assignees: Vec::new(),
        }))
    }
    fn move_status(&self, id: &str, to: &Column, reason: &str) -> Result<Option<StatusMove>> {
        self.log
            .lock()
            .unwrap()
            .push(format!("move_status {id} -> {} reason={reason}", to.as_str()));
        let mut issue = self.issue.lock().unwrap();
        if issue.column == *to {
            return Ok(None);
        }
        let from = std::mem::replace(&mut issue.column, to.clone());
        Ok(Some(StatusMove {
            from,
            to: to.clone(),
            workflow: issue.workflow_or_default().to_string(),
        }))
    }

    fn list(&self) -> Result<Vec<IssueFile>> {
        unreachable!()
    }
    fn list_in_status(&self, _status: &Column) -> Result<Vec<IssueFile>> {
        unreachable!()
    }
    fn add(&self, _spec: NewIssue) -> Result<Issue> {
        unreachable!()
    }
    fn set_priority(&self, _id: &str, _pos: PrioMove) -> Result<()> {
        unreachable!()
    }
    fn set_fields(&self, _id: &str, _fields: IssueFields) -> Result<()> {
        unreachable!()
    }
    fn cancel(&self, _id: &str, _reason: &str) -> Result<Option<StatusMove>> {
        unreachable!()
    }
    fn move_status_and_unassign(
        &self,
        _id: &str,
        _to: &Column,
        _reason: &str,
    ) -> Result<Option<StatusMove>> {
        unreachable!()
    }
    fn delete(&self, _id: &str) -> Result<()> {
        unreachable!()
    }
    fn renumber(&self, _status: &Column) -> Result<()> {
        unreachable!()
    }
    fn park_review(&self, _id: &str) -> Result<Option<String>> {
        unreachable!()
    }
    fn clear_parked(&self, _id: &str) -> Result<()> {
        unreachable!()
    }
    fn reject_review(
        &self,
        _id: &str,
        _ready: &Column,
        _reason: &str,
        _date: &str,
    ) -> Result<Option<StatusMove>> {
        unreachable!()
    }
    fn poll_changes(&self, _since: &Cursor) -> Result<(Vec<IssueChange>, Cursor)> {
        unreachable!()
    }
    fn list_comments(&self, _id: &str) -> Result<Vec<IssueComment>> {
        unreachable!()
    }
    fn add_comment(&self, _id: &str, _body: &str) -> Result<IssueComment> {
        unreachable!()
    }
}

/// Records each transition call instead of touching git. `fail` names the one
/// call (`cut_branch` / `gated_merge` / `remaining_actions`) that errors.
struct StubRunner {
    log: CallLog,
    fail: Option<&'static str>,
}

impl StubRunner {
    fn failing(mut self, call: &'static str) -> Self {
        self.fail = Some(call);
        self
    }

    fn record(&self, call: &'static str, line: String) -> Result<()> {
        self.log.lock().unwrap().push(line);
        match self.fail {
            Some(f) if f == call => Err(Error::Other(format!("{call} exploded"))),
            _ => Ok(()),
        }
    }
}

impl TransitionRunner for StubRunner {
    fn cut_branch(&self, _project: &Project, task_id: &str) -> Result<()> {
        self.record("cut_branch", format!("cut_branch {task_id}"))
    }

    fn gated_merge(
        &self,
        edge: &TransitionEdge<'_>,
        workspace_label: &str,
    ) -> Result<Option<GatedMerge>> {
        self.record(
            "gated_merge",
            format!(
                "gated_merge {} {} -> {} workspace={workspace_label}",
                edge.issue.task.id, edge.from, edge.to
            ),
        )?;
        // What the live gate reports: the pre-merge prefix plus `merge`.
        let actions = edge.workflow.actions_for_transition(edge.from, edge.to);
        let merge_pos = actions
            .iter()
            .position(|a| *a == TransitionAction::Merge)
            .expect("gated_merge is only called for a merge edge");
        Ok(Some(GatedMerge {
            detail: "pr:42:abc123".into(),
            ran: actions[..=merge_pos].to_vec(),
        }))
    }

    fn remaining_actions(
        &self,
        edge: &TransitionEdge<'_>,
        skip: &[TransitionAction],
    ) -> Result<Vec<ActionOutcome>> {
        let remaining: Vec<String> = edge
            .workflow
            .actions_for_transition(edge.from, edge.to)
            .iter()
            .filter(|a| !skip.contains(a))
            .map(|a| a.to_string())
            .collect();
        self.record(
            "remaining_actions",
            format!(
                "remaining_actions {} {} -> {} run=[{}]",
                edge.issue.task.id,
                edge.from,
                edge.to,
                remaining.join(", ")
            ),
        )?;
        Ok(Vec::new())
    }
}

/// How `shelbi issue move` calls in.
fn cli_request(to: &str) -> MoveRequest<'_> {
    MoveRequest {
        project: PROJECT,
        id: "t",
        to,
        reason: "user:cli",
        workspace_fallback: "cli",
        skip_transition_actions: false,
    }
}

/// How the TUI board's background persistence chain calls in.
fn board_request(to: &str) -> MoveRequest<'_> {
    MoveRequest {
        project: PROJECT,
        id: "t",
        to,
        reason: "user:tui",
        workspace_fallback: "board",
        skip_transition_actions: false,
    }
}

fn no_warnings(w: MoveWarning) {
    panic!("unexpected warning: {w}");
}

#[test]
fn accept_edge_merges_before_the_status_write_then_deletes_the_branch() {
    // The headline: a card dragged `review -> done` on the board runs the
    // edge's gated merge BEFORE the status is written, and the edge's
    // remaining action (`delete_branch`) after it.
    let fx = Fixture::new("accept");
    let store = fx.store(Column::review());

    let outcome =
        move_issue_with(&store, &fx.runner(), &board_request("done"), &mut no_warnings).unwrap();

    assert_eq!(
        fx.calls(),
        vec![
            "gated_merge t review -> done workspace=board",
            "move_status t -> done reason=user:tui",
            "remaining_actions t review -> done run=[delete_branch]",
        ],
    );
    assert_eq!(outcome.column, Column::done());
    assert!(outcome.moved);
    assert_eq!(
        outcome.merge.map(|gm| gm.detail).as_deref(),
        Some("pr:42:abc123"),
        "the merge result is handed back for the caller to report",
    );
    assert_eq!(store.column(), Column::done());
    let events = fx.events();
    assert_eq!(events.len(), 1, "events: {events:?}");
    assert!(events[0].contains(" review -> done "), "line: {}", events[0]);
    assert!(events[0].contains(" reason=user:tui "), "line: {}", events[0]);
}

#[test]
fn board_and_cli_moves_run_the_same_transition_actions() {
    // Same edge, driven the way each surface drives it. Apart from the
    // caller's own labels (`reason=`, the merge event's fallback `workspace=`)
    // the calls are identical, in the same order.
    fn normalized(calls: Vec<String>) -> Vec<String> {
        calls
            .into_iter()
            .map(|c| {
                c.replace("user:cli", "<reason>")
                    .replace("user:tui", "<reason>")
                    .replace("workspace=cli", "workspace=<fallback>")
                    .replace("workspace=board", "workspace=<fallback>")
            })
            .collect()
    }

    let cli = {
        let fx = Fixture::new("same-cli");
        let store = fx.store(Column::review());
        move_issue_with(&store, &fx.runner(), &cli_request("done"), &mut no_warnings).unwrap();
        fx.calls()
    };
    let board = {
        let fx = Fixture::new("same-board");
        let store = fx.store(Column::review());
        move_issue_with(&store, &fx.runner(), &board_request("done"), &mut no_warnings).unwrap();
        fx.calls()
    };

    assert_eq!(cli.len(), 3, "calls: {cli:?}");
    assert_eq!(normalized(cli), normalized(board));
}

#[test]
fn failed_gated_merge_leaves_the_issue_where_it_was() {
    let fx = Fixture::new("merge-fails");
    let store = fx.store(Column::review());
    let runner = fx.runner().failing("gated_merge");

    let err = move_issue_with(&store, &runner, &board_request("done"), &mut no_warnings)
        .unwrap_err();

    // The gate ran and nothing after it did: no status write, no cleanup.
    assert_eq!(fx.calls(), vec!["gated_merge t review -> done workspace=board"]);
    assert_eq!(store.column(), Column::review());
    assert!(fx.events().is_empty(), "no move event for a move that didn't happen");
    assert!(matches!(err, MoveError::Merge { .. }), "err: {err:?}");
    assert_eq!(
        err.to_string(),
        "merge for `t` failed; leaving it in `review` \
         (NOT advancing to `done`): gated_merge exploded",
    );
}

#[test]
fn failed_post_merge_cleanup_warns_but_the_move_stands() {
    let fx = Fixture::new("cleanup-fails");
    let store = fx.store(Column::review());
    let runner = fx.runner().failing("remaining_actions");

    let mut warnings = Vec::new();
    let outcome = move_issue_with(&store, &runner, &cli_request("done"), &mut |w| {
        warnings.push(w.to_string())
    })
    .unwrap();

    assert!(outcome.moved);
    assert_eq!(store.column(), Column::done());
    assert_eq!(
        warnings,
        vec![
            "post-merge cleanup for `t` failed (merge already landed): \
             remaining_actions exploded"
        ],
    );
}

#[test]
fn skip_transition_actions_crosses_a_merge_edge_without_the_runner() {
    let fx = Fixture::new("skip");
    let store = fx.store(Column::review());
    let req = MoveRequest {
        skip_transition_actions: true,
        ..cli_request("done")
    };

    let outcome = move_issue_with(&store, &fx.runner(), &req, &mut no_warnings).unwrap();

    assert_eq!(fx.calls(), vec!["move_status t -> done reason=user:cli"]);
    assert_eq!(outcome.merge, None);
    let events = fx.events();
    assert_eq!(events.len(), 1, "events: {events:?}");
    assert!(events[0].ends_with(" actions=skipped"), "line: {}", events[0]);
}

#[test]
fn edge_without_a_merge_fires_no_actions_from_either_surface() {
    // Pins `shelbi issue move`'s existing contract, now shared by the board: a
    // hand move only runs the actions of an edge that declares `merge`. The
    // `push_branch` / `open_pr` of `in-progress -> review` belong to the
    // poller's ready-marker handoff, not to a hand move.
    for req in [cli_request("review"), board_request("review")] {
        let fx = Fixture::new("no-merge");
        let store = fx.store(Column::in_progress());

        let outcome = move_issue_with(&store, &fx.runner(), &req, &mut no_warnings).unwrap();

        assert_eq!(
            fx.calls(),
            vec![format!("move_status t -> review reason={}", req.reason)],
        );
        assert_eq!(outcome.merge, None);
        assert_eq!(store.column(), Column::review());
    }
}

#[test]
fn move_into_in_progress_cuts_the_branch_before_the_status_write() {
    let fx = Fixture::new("cut");
    let store = fx.store(Column::todo());

    move_issue_with(&store, &fx.runner(), &board_request("in-progress"), &mut no_warnings)
        .unwrap();

    assert_eq!(
        fx.calls(),
        vec!["cut_branch t", "move_status t -> in-progress reason=user:tui"],
    );
}

#[test]
fn failed_branch_cut_aborts_the_move() {
    let fx = Fixture::new("cut-fails");
    let store = fx.store(Column::todo());
    let runner = fx.runner().failing("cut_branch");

    // `in_progress` is the CLI's friendly spelling of `in-progress`.
    let err = move_issue_with(&store, &runner, &cli_request("in_progress"), &mut no_warnings)
        .unwrap_err();

    assert!(matches!(err, MoveError::BranchCut(_)), "err: {err:?}");
    assert_eq!(fx.calls(), vec!["cut_branch t"]);
    assert_eq!(store.column(), Column::todo());
}

#[test]
fn undeclared_target_errors_before_any_side_effect() {
    let fx = Fixture::new("undeclared");
    let store = fx.store(Column::review());

    let err = move_issue_with(&store, &fx.runner(), &cli_request("qa"), &mut no_warnings)
        .unwrap_err();

    assert_eq!(
        err.to_string(),
        "`qa` is not a status in workflow `mergewf` \
         (valid: backlog, todo, in-progress, review, done)",
    );
    assert!(fx.calls().is_empty(), "calls: {:?}", fx.calls());
}

#[test]
fn move_to_the_current_status_is_a_quiet_no_op() {
    let fx = Fixture::new("noop");
    let store = fx.store(Column::done());

    let outcome =
        move_issue_with(&store, &fx.runner(), &cli_request("done"), &mut no_warnings).unwrap();

    assert!(!outcome.moved);
    assert_eq!(fx.calls(), vec!["move_status t -> done reason=user:cli"]);
    assert!(fx.events().is_empty());
}
