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

fn column_ids(column: Column) -> Vec<String> {
    issue_store("p")
        .unwrap()
        .list_in_status(&column)
        .unwrap()
        .into_iter()
        .map(|tf| tf.task.id)
        .collect()
}

#[test]
fn apply_prioritize_lands_the_issue_at_the_slot_and_shifts_the_rest() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    for (i, id) in ["a", "b", "c", "d"].iter().enumerate() {
        let mut t = task(Column::todo(), id);
        t.priority = i as u32;
        shelbi_state::save_task("p", &t, "").unwrap();
    }

    let mut sink = RecordingSink::default();
    let mut recheck = no_recheck();
    let note = apply(
        "p",
        "d",
        &MutationKind::Prioritize { position: 1 },
        &mut sink,
        &mut recheck,
    )
    .unwrap();

    assert_eq!(column_ids(Column::todo()), ["a", "d", "b", "c"]);
    assert_eq!(note.verb, "prioritize");
    assert_eq!(note.status, "todo");
    assert!(
        sink.lines
            .iter()
            .any(|(s, t)| *s == Stream::Stdout && t == "✓ d now at slot 1 in todo"),
        "stdout lines: {:?}",
        sink.lines
    );

    // A slot past the end clamps to the bottom, like `issue prio --set`.
    apply(
        "p",
        "a",
        &MutationKind::Prioritize { position: 99 },
        &mut sink,
        &mut recheck,
    )
    .unwrap();
    assert_eq!(column_ids(Column::todo()), ["d", "b", "c", "a"]);

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn apply_prioritize_aborts_without_reordering_when_the_recheck_reports_stale() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    for (i, id) in ["a", "b"].iter().enumerate() {
        let mut t = task(Column::todo(), id);
        t.priority = i as u32;
        shelbi_state::save_task("p", &t, "").unwrap();
    }

    let mut sink = RecordingSink::default();
    let stale = ExpectedState {
        status: "todo".into(),
        updated_at: "x".into(),
    };
    let mut recheck = || {
        Err(MutateError::Stale {
            expected: stale.clone(),
            actual: stale.clone(),
        })
    };
    let err = apply(
        "p",
        "b",
        &MutationKind::Prioritize { position: 0 },
        &mut sink,
        &mut recheck,
    )
    .unwrap_err();
    assert!(matches!(err, MutateError::Stale { .. }));
    assert_eq!(column_ids(Column::todo()), ["a", "b"]);

    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

/// Write a second project `g` into the current `SHELBI_HOME` whose board is the
/// GitHub (remote) backend, so a test can exercise the remote-vs-local branch of
/// [`is_stale_change`]. The YAML otherwise mirrors `fresh_home`'s project `p`.
fn write_github_project() {
    let projects = shelbi_state::projects_dir().unwrap();
    std::fs::write(
        projects.join("g.yaml"),
        "name: g\nrepo: /tmp/g\ndefault_branch: main\n\
         orchestrator:\n  runner: claude\n\
         agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
         machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/g\n\
         workspaces:\n  - { name: dev, machine: local, runner: claude }\n\
         issue_tracker:\n  backend: github\n  github:\n    repo: owner/repo\n",
    )
    .unwrap();
}

fn at(status: &str, updated_at: &str) -> ExpectedState {
    ExpectedState {
        status: status.into(),
        updated_at: updated_at.into(),
    }
}

#[test]
fn is_stale_change_ignores_a_self_write_rev_bump_on_a_remote_board() {
    // The bug: `issue move <id> --to todo` then `issue start <id>` on a
    // GitHub-backed board. The move's write-back bumps `updated_at` ~15-30s
    // after the command returns, so `start` sees the same status at a newer
    // revision. That self-write must NOT read as a conflict.
    //
    // But on a LOCAL board the revision is a reliable token, so the same
    // same-status bump there IS stale; and a real status change is stale on
    // either backend.
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project();

    let rev_a = "2026-10-13T01:48:25+00:00";
    let rev_b = "2026-10-13T01:48:53+00:00";

    // Remote (GitHub) board: a same-status revision bump is a self-write, not
    // a conflict — `issue start` right after `issue move` succeeds.
    assert!(
        !is_stale_change("g", &at("todo", rev_a), &at("todo", rev_b)),
        "a same-status rev bump on a remote board must not read as stale"
    );
    // Identical state is never stale.
    assert!(!is_stale_change("g", &at("todo", rev_a), &at("todo", rev_a)));
    // A real concurrent status change still raises the conflict on a remote
    // board (someone else moved the card).
    assert!(
        is_stale_change("g", &at("todo", rev_a), &at("in_progress", rev_b)),
        "a concurrent status change on a remote board must read as stale"
    );

    // Local (filesystem) board: the revision is authoritative, so a
    // same-status bump IS a real change and reads as stale (no regression).
    assert!(
        is_stale_change("p", &at("todo", rev_a), &at("todo", rev_b)),
        "a rev bump on a local board must still read as stale"
    );
    assert!(!is_stale_change("p", &at("todo", rev_a), &at("todo", rev_a)));
    assert!(is_stale_change("p", &at("todo", rev_a), &at("in_progress", rev_a)));

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

// ---- tracker ownership: `start` on a shared remote tracker -------------------

/// A fake GitHub for project `gh` serving one issue `t` (number 7) assigned to
/// `assignees`, authenticated as `octocat`. The add-assignees POST answers the
/// issue with `octocat` added. Every call is recorded.
fn fake_github_issue(assignees: &[&str]) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    let issue = |logins: &[String]| {
        let assignees: Vec<String> =
            logins.iter().map(|l| format!(r#"{{"login":"{l}"}}"#)).collect();
        format!(
            r#"{{"number":7,"title":"T","body":"","state":"open","assignees":[{}],"labels":[{{"name":"shelbi:id/t"}},{{"name":"shelbi:status/todo"}}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}}"#,
            assignees.join(",")
        )
    };
    // The same issue as a GraphQL node (the single-issue query `get_raw` uses).
    let node = |logins: &[String]| {
        let assignees: Vec<String> =
            logins.iter().map(|l| format!(r#"{{"login":"{l}"}}"#)).collect();
        format!(
            r#"{{"data":{{"rateLimit":{{"remaining":4999}},"repository":{{"issue":{{"number":7,"title":"T","state":"OPEN","stateReason":null,"createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-08-02T00:00:00Z","body":"","assignees":{{"nodes":[{}]}},"labels":{{"nodes":[{{"name":"shelbi:id/t"}},{{"name":"shelbi:status/todo"}}]}}}}}}}}}}"#,
            assignees.join(",")
        )
    };
    let before: Vec<String> = assignees.iter().map(|s| s.to_string()).collect();
    let mut after = before.clone();
    after.push("octocat".into());
    let before_node = node(&before);
    let (before, after) = (issue(&before), issue(&after));
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let rec = calls.clone();
    shelbi_state::set_test_gh_runner(move |args: &[&str]| {
        let joined = args.join(" ");
        rec.lock().unwrap().push(joined.clone());
        if args.contains(&"user") && !joined.contains("repos/") {
            return Ok("octocat\n".to_string());
        }
        if args.contains(&"graphql") && joined.contains("IdSearch") {
            return Ok(
                r#"{"data":{"rateLimit":{"remaining":4999},"search":{"nodes":[{"number":7}]}}}"#
                    .to_string(),
            );
        }
        if args.contains(&"graphql") {
            return Ok(before_node.clone());
        }
        if joined.contains("/assignees") {
            return Ok(after.clone());
        }
        Ok(before.clone())
    });
    calls
}

fn start_t(take: bool) -> (Result<ChangeNote, MutateError>, RecordingSink) {
    let mut sink = RecordingSink::default();
    let mut recheck = no_recheck();
    let out = apply(
        "gh",
        "t",
        &MutationKind::Start {
            workspace: Some("dev".into()),
            branch: None,
            reason: None,
            force: false,
            take,
        },
        &mut sink,
        &mut recheck,
    );
    (out, sink)
}

fn claimed(calls: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> bool {
    calls.lock().unwrap().iter().any(|c| c.contains("/assignees"))
}

#[test]
fn start_refuses_an_issue_assigned_to_someone_else() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");
    let calls = fake_github_issue(&["alice"]);

    let err = start_t(false).0.unwrap_err().to_string();
    assert!(err.contains("assigned to @alice"), "{err}");
    assert!(err.contains("--take"), "{err}");
    assert!(!claimed(&calls), "start never claims on its own");
    assert!(!events_log().contains("task=t "), "a refused start moves nothing: {}", events_log());

    shelbi_state::clear_test_gh_runner();
    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn start_refuses_an_unassigned_issue() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");
    let calls = fake_github_issue(&[]);

    let err = start_t(false).0.unwrap_err().to_string();
    assert!(err.contains("not assigned to anyone"), "{err}");
    assert!(!claimed(&calls));

    shelbi_state::clear_test_gh_runner();
    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn start_passes_the_ownership_gate_for_the_users_own_issue() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");
    let calls = fake_github_issue(&["octocat"]);

    // The launch itself can't run here (no repo, no session), so the start may
    // still fail later; what matters is that it isn't the ownership gate.
    if let Err(e) = start_t(false).0 {
        let e = e.to_string();
        assert!(!e.contains("Shelbi only starts issues assigned to you"), "{e}");
        assert!(!e.contains("login could not be resolved"), "{e}");
    }
    assert!(!claimed(&calls), "an own issue needs no claim");

    shelbi_state::clear_test_gh_runner();
    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn start_take_claims_someone_elses_issue_before_starting_it() {
    let _g = crate::test_lock::acquire();
    let home = fresh_home();
    std::env::set_var("SHELBI_HOME", &home);
    write_github_project_yaml("gh");
    let calls = fake_github_issue(&["alice"]);

    let (out, sink) = start_t(true);
    if let Err(e) = &out {
        assert!(!e.to_string().contains("Shelbi only starts issues assigned to you"), "{e}");
    }
    assert!(claimed(&calls), "--take assigns the issue to the user on the tracker: {out:?}");
    assert!(
        sink.lines.iter().any(|(_, l)| l == "✓ took t: assigned to @octocat on the issue tracker"),
        "{:?}",
        sink.lines
    );

    shelbi_state::clear_test_gh_runner();
    std::env::remove_var("SHELBI_HOME");
    let _ = std::fs::remove_dir_all(&home);
}
