//! Behavioral tests for [`apply`] against a real filesystem issue store. The
//! per-piece unit tests live next to their code (`start.rs`, `add_edit.rs`); the
//! daemon concurrency machinery (queue, broadcast, finish-if-client-leaves) is
//! tested end-to-end in `shelbi-cli/tests/control_socket.rs`.

use std::path::PathBuf;

use chrono::Utc;
use shelbi_core::{Column, Issue};
use shelbi_proto::control::{AddSpec, MutationKind, Stream};

use super::*;

/// A short-lived `SHELBI_HOME` with a minimal filesystem project `p`.
fn fresh_home() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "shelbi-mutate-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Route the per-project path through the crate helper rather than
    // hand-building `p.join("projects")` — the callsite-scan guard in
    // `shelbi-state` forbids the raw literal. `projects_dir()` resolves
    // `SHELBI_HOME`, so pin it to this home first (callers re-set it to
    // the same path immediately after).
    std::env::set_var("SHELBI_HOME", &p);
    let projects = shelbi_state::projects_dir().unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join("p.yaml"),
        "name: p\nrepo: /tmp/p\ndefault_branch: main\n\
         orchestrator:\n  runner: claude\n\
         agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
         machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/p\n\
         workspaces:\n  - { name: dev, machine: local, runner: claude }\n",
    )
    .unwrap();
    p
}

fn task(column: Column, id: &str) -> Issue {
    let now = Utc::now();
    Issue {
        id: id.into(),
        title: id.replace('-', " "),
        column,
        priority: 0,
        assigned_to: None,
        workflow: None,
        branch: None,
        depends_on: Vec::new(),
        prefers_machine: None,
        zen: None,
        launch: None,
        created_at: now,
        updated_at: now,
        params: std::collections::BTreeMap::new(),
    }
}

fn events_log() -> String {
    std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap_or_default()
}

#[test]
fn apply_move_writes_status_and_event() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);

    shelbi_state::save_task("p", &task(Column::backlog(), "t"), "body").unwrap();

    let mut sink = RecordingSink::default();
    let mut recheck = no_recheck();
    let note = apply(
        "p",
        "t",
        &MutationKind::Move {
            to: "todo".into(),
            reason: None,
            skip_transition_actions: false,
        },
        &mut sink,
        &mut recheck,
    )
    .unwrap();

    assert_eq!(shelbi_state::load_task("p", "t").unwrap().task.column, Column::todo());
    assert_eq!(note.status, "todo");
    assert!(
        sink.lines
            .iter()
            .any(|(s, t)| *s == Stream::Stdout && t == "✓ t → todo"),
        "stdout lines: {:?}",
        sink.lines
    );
    assert!(events_log().contains(" backlog -> todo "), "{}", events_log());

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn apply_move_aborts_before_writing_when_the_recheck_reports_stale() {
    // Criterion: the state is rechecked immediately before the irreversible
    // step. A recheck that reports the issue moved on must abort the move with
    // no state change.
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);

    shelbi_state::save_task("p", &task(Column::backlog(), "t"), "body").unwrap();

    let mut sink = RecordingSink::default();
    let mut recheck = || {
        Err(MutateError::Stale {
            expected: ExpectedState {
                status: "backlog".into(),
                updated_at: "x".into(),
            },
            actual: ExpectedState {
                status: "todo".into(),
                updated_at: "y".into(),
            },
        })
    };
    let err = apply(
        "p",
        "t",
        &MutationKind::Move {
            to: "todo".into(),
            reason: None,
            skip_transition_actions: false,
        },
        &mut sink,
        &mut recheck,
    )
    .unwrap_err();
    assert!(matches!(err, MutateError::Stale { .. }));
    // Nothing moved.
    assert_eq!(
        shelbi_state::load_task("p", "t").unwrap().task.column,
        Column::backlog()
    );

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn apply_add_into_a_ready_status_wakes_the_orchestrator_but_backlog_stays_quiet() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);

    // Into `todo` (ready): emits a creation/wake event.
    let mut sink = RecordingSink::default();
    let mut recheck = no_recheck();
    apply(
        "p",
        "",
        &MutationKind::Add(Box::new(AddSpec {
            title: "Wake me".into(),
            id: Some("w".into()),
            status: "todo".into(),
            body: None,
            depends_on: vec![],
            prefers_machine: None,
            workflow: None,
            branch: None,
        })),
        &mut sink,
        &mut recheck,
    )
    .unwrap();
    assert!(events_log().contains("task=w"), "todo add must wake: {}", events_log());

    // Into `backlog` (triage): no wake event.
    let before = events_log();
    apply(
        "p",
        "",
        &MutationKind::Add(Box::new(AddSpec {
            title: "Quiet".into(),
            id: Some("q".into()),
            status: "backlog".into(),
            body: None,
            depends_on: vec![],
            prefers_machine: None,
            workflow: None,
            branch: None,
        })),
        &mut sink,
        &mut no_recheck(),
    )
    .unwrap();
    assert!(
        !events_log().replace(&before, "").contains("task=q"),
        "backlog add must stay quiet: {}",
        events_log()
    );
    assert_eq!(shelbi_state::load_task("p", "q").unwrap().task.column, Column::backlog());

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn null_and_recording_sinks_route_streams() {
    let mut rec = RecordingSink::default();
    rec.out("hello");
    rec.warn("warning: oops");
    assert_eq!(rec.lines, vec![
        (Stream::Stdout, "hello".to_string()),
        (Stream::Stderr, "warning: oops".to_string()),
    ]);
    let mut null = NullSink;
    null.out("ignored");
}

// --- `workspace_busy_with_other` (the #1494 supplant-release guard) ---------
//
// Ported from `shelbi-cli`'s `commands/issue.rs` when the dispatch logic moved
// into this crate. The guard decides whether a dispatch may tear down the
// card's prior assignee: it releases only a workspace still on THIS card (or
// idle), never one re-dispatched to another issue.

/// Write a **GitHub-backed** project `name` into the current `SHELBI_HOME`, so
/// `backend.is_remote()` is true and the guard reads from the local assignment
/// overlay rather than the frontmatter board. Callers set `SHELBI_HOME` under
/// the test lock before calling, so route the per-project path through the
/// crate helper rather than hand-building `home.join("projects")` — the
/// callsite-scan guard in `shelbi-state` forbids the raw literal.
fn write_github_project_yaml(name: &str) {
    let projects = shelbi_state::projects_dir().unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join(format!("{name}.yaml")),
        format!(
            r#"name: {name}
repo: /tmp/{name}
default_branch: main
issue_tracker:
  backend: github
  github:
    repo: owner/repo
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: local
    kind: local
    work_dir: /tmp/{name}
workspaces:
  - {{ name: dev, machine: local, runner: claude }}
"#
        ),
    )
    .unwrap();
}

fn task_assigned(id: &str, column: Column, ws: &str) -> Issue {
    Issue {
        assigned_to: Some(ws.to_string()),
        ..task(column, id)
    }
}

fn issue_file(t: Issue) -> shelbi_state::IssueFile {
    shelbi_state::IssueFile {
        task: t,
        body: String::new(),
        tracker_assignees: Vec::new(),
    }
}

/// Seed a fresh, current daemon index for the `gh` project (repo identity
/// stamped so every reader accepts it).
fn write_gh_index(board: Vec<shelbi_state::IssueFile>) {
    let mut idx = shelbi_state::BoardIndex::fresh(board);
    idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
    shelbi_state::write_board_index("gh", &idx).unwrap();
}

/// Seed a **stale** daemon index for the `gh` project — the identity still
/// matches (so the file serves) but `stale` is flagged, so `read_board` maps it
/// to [`shelbi_state::BoardState::Stale`]. Models a parked/dead daemon whose
/// last-published board has aged out, the shape of the 2026-10-04 incident.
fn write_gh_index_stale(board: Vec<shelbi_state::IssueFile>) {
    let mut idx = shelbi_state::BoardIndex::fresh(board);
    idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
    idx.stale = true;
    shelbi_state::write_board_index("gh", &idx).unwrap();
}

/// The supplant guard's core case: a dispatch must NOT tear down the prior
/// assignee once that workspace has been re-dispatched to a *different* issue.
/// `workspace_busy_with_other` surfaces that other issue so the release block
/// takes the `skipped` branch — leaving the live worker and its issue untouched
/// — instead of killing it mid-task. The decision is read from the local
/// assignment overlay (authoritative routing), with the open board confirming
/// the routed card is still non-terminal.
#[test]
fn workspace_busy_with_other_reports_a_different_active_issue() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");

    // `alpha` was the prior assignee of `handed-off`, but is now routed to
    // `other-task` in the local overlay, and that card is still open.
    shelbi_state::set_task_assignment("gh", "other-task", Some("alpha")).unwrap();
    write_gh_index(vec![issue_file(task_assigned(
        "other-task",
        Column::in_progress(),
        "alpha",
    ))]);

    let busy = workspace_busy_with_other("gh", "alpha", "handed-off")
        .unwrap()
        .expect("alpha is active on a different issue");
    assert_eq!(busy.id, "other-task");

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

/// A workspace serving a `review`-column task on a different issue is just as
/// busy as one holding an `in_progress` card — the guard counts any open routed
/// issue regardless of column, so a dispatch never tears down a review slot
/// serving an unrelated card.
#[test]
fn workspace_busy_with_other_counts_a_review_slot() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");
    shelbi_state::set_task_assignment("gh", "under-review", Some("alpha")).unwrap();
    write_gh_index(vec![issue_file(task_assigned(
        "under-review",
        Column::review(),
        "alpha",
    ))]);

    let busy = workspace_busy_with_other("gh", "alpha", "handed-off")
        .unwrap()
        .expect("alpha is serving a review on a different issue");
    assert_eq!(busy.id, "under-review");

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

/// The two release-fires cases: the prior assignee is still on THIS card (the
/// original gate-to-gate move the release was written for) or has gone idle.
/// Both yield `None`, so the release proceeds — a real teardown when a pane is
/// up, a no-op when idle. The card being dispatched is excluded by id, so an
/// overlay marker that still points at it never reads as "busy elsewhere".
#[test]
fn workspace_busy_with_other_none_when_on_this_card_or_idle() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");

    // Still on this card: the overlay routes `handed-off` to alpha and nothing
    // else. (The gate-to-gate case — alpha is released.)
    shelbi_state::set_task_assignment("gh", "handed-off", Some("alpha")).unwrap();
    write_gh_index(vec![issue_file(task_assigned(
        "handed-off",
        Column::in_progress(),
        "alpha",
    ))]);
    assert!(
        workspace_busy_with_other("gh", "alpha", "handed-off")
            .unwrap()
            .is_none(),
        "the card being dispatched is excluded, so the prior assignee reads as free"
    );

    // Idle: the overlay routes nothing to alpha at all.
    shelbi_state::set_task_assignment("gh", "handed-off", None).unwrap();
    write_gh_index(Vec::new());
    assert!(
        workspace_busy_with_other("gh", "alpha", "handed-off")
            .unwrap()
            .is_none(),
        "an idle prior assignee has no other active issue"
    );

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

/// The incident shape (2026-10-04), the reason the first pass was reworked: the
/// prior assignee A was re-dispatched to another card *after* the last board
/// refresh, and the daemon then died, so the published index is **stale** and
/// still lists that card in `todo`. Reading occupancy from the index alone (its
/// old `in_progress`/`review` filter) read A as idle and killed its live worker.
/// Deciding from the local overlay — authoritative and never lagged — surfaces
/// the routed card from a stale index all the same, so the release is skipped
/// and A's pane is left alone.
#[test]
fn workspace_busy_with_other_reads_the_overlay_past_a_stale_index() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");

    // Local overlay (authoritative) routes `other-task` to alpha.
    shelbi_state::set_task_assignment("gh", "other-task", Some("alpha")).unwrap();
    // The published index is stale and still shows `other-task` in `todo` — the
    // exact state the old guard misread as "alpha is idle".
    write_gh_index_stale(vec![issue_file(task_assigned(
        "other-task",
        Column::todo(),
        "alpha",
    ))]);

    let busy = workspace_busy_with_other("gh", "alpha", "handed-off")
        .unwrap()
        .expect("alpha is routed to another open issue, even from a stale index");
    assert_eq!(
        busy.id, "other-task",
        "a todo card from a stale index still counts — the release must be skipped"
    );

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

/// The overlay can retain a marker for a card that has since gone terminal (its
/// merge cleared the column but not the marker). A `Warm` board is the current
/// open-only set, so a routed id absent from it is confirmed terminal and the
/// prior assignee reads as free — the release proceeds rather than being blocked
/// forever by a lingering marker.
#[test]
fn workspace_busy_with_other_frees_a_terminal_marker_against_a_warm_board() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");

    // Overlay still routes `done-task` to alpha, but the card has closed and the
    // warm index no longer lists it.
    shelbi_state::set_task_assignment("gh", "done-task", Some("alpha")).unwrap();
    write_gh_index(Vec::new());

    assert!(
        workspace_busy_with_other("gh", "alpha", "handed-off")
            .unwrap()
            .is_none(),
        "a routed id absent from a warm (open-only) board is terminal — alpha is free"
    );

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

/// When the overlay routes another issue to A but the board **isn't warm**
/// (stale/cold/unreachable) and doesn't list that id, we can't prove whether it
/// is still active. The guard returns `Err` so the caller errs toward NOT
/// releasing — a possibly-orphaned pane is recoverable; killing an unrelated
/// live worker on a false negative is not.
#[test]
fn workspace_busy_with_other_cant_confirm_on_a_stale_board_missing_the_routed_id() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");

    // Overlay routes `other-task` to alpha, but the stale index doesn't carry it
    // (so presence can't confirm it, and staleness can't deny it either).
    shelbi_state::set_task_assignment("gh", "other-task", Some("alpha")).unwrap();
    write_gh_index_stale(Vec::new());

    assert!(
        workspace_busy_with_other("gh", "alpha", "handed-off").is_err(),
        "an unconfirmable stale board must not authorize a release"
    );

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}
