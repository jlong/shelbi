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
