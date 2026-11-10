use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use shelbi_core::{Project, Result};
use shelbi_state::awareness::{self, JobKind, JobResult, NewJob};

use super::*;
use crate::test_lock::isolated_home;

const P: &str = "demo";

fn project() -> Project {
    let mut p = Project::from_yaml_str(
        "repo: /tmp/p\nmachines:\n  - { name: local, kind: local, work_dir: /tmp/p }\n\
         orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n",
    )
    .unwrap();
    p.name = P.into();
    p
}

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-10T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn at(mins: i64) -> DateTime<Utc> {
    t0() + chrono::Duration::minutes(mins)
}

fn mins(n: u64) -> Duration {
    Duration::from_secs(n * 60)
}

#[derive(Default)]
struct FakeProbe {
    alive: RefCell<HashSet<u32>>,
    ci: RefCell<Option<WatchVerdict>>,
    pr: RefCell<Option<WatchVerdict>>,
    remote_calls: Cell<u32>,
    offline: Cell<bool>,
    /// `None` = board unreadable.
    active: RefCell<Option<HashMap<String, Option<String>>>>,
}

impl AwarenessProbe for FakeProbe {
    fn pid_alive(&self, pid: u32) -> bool {
        self.alive.borrow().contains(&pid)
    }
    fn commit_ci(&self, _: &Project, _: &str, _: bool) -> Result<Option<WatchVerdict>> {
        self.remote_calls.set(self.remote_calls.get() + 1);
        Ok(self.ci.borrow().clone())
    }
    fn pull_request(&self, _: &Project, _: u64, _: bool) -> Result<Option<WatchVerdict>> {
        self.remote_calls.set(self.remote_calls.get() + 1);
        Ok(self.pr.borrow().clone())
    }
    fn active_tasks(&self, _: &Project) -> Option<HashMap<String, Option<String>>> {
        self.active.borrow().clone()
    }
    fn online(&self) -> bool {
        !self.offline.get()
    }
}

/// Event bodies (timestamp stripped) written so far.
fn events() -> Vec<String> {
    let path = shelbi_state::events_log_path().unwrap();
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split_once(' ').map(|(_, b)| b.to_string()).unwrap_or_default())
        .collect()
}

fn store() -> awareness::AwarenessStore {
    awareness::read_awareness(P).unwrap()
}

fn run(state: &mut AwarenessTickState, now: DateTime<Utc>, probe: &FakeProbe) {
    tick(&project(), state, now, probe);
}

#[test]
fn a_reminder_fires_once_when_due_and_leaves_the_store() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    awareness::add_reminder(P, at(2), "check the tap PR merged", None).unwrap();

    run(&mut state, at(1), &probe);
    assert!(events().is_empty());
    run(&mut state, at(2), &probe);
    run(&mut state, at(3), &probe);
    assert_eq!(
        events(),
        [r#"project=demo reminder id=r1 text="check the tap PR merged""#]
    );
    assert!(store().reminders.is_empty());
}

#[test]
fn a_reminder_missed_while_the_daemon_was_down_fires_once_and_a_recurring_one_reschedules() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    awareness::add_reminder(P, at(0), "one-shot", None).unwrap();
    awareness::add_reminder(P, at(0), "every 10m", Some(mins(10))).unwrap();

    // Back up 35 minutes later: each fires once, the recurring one lands on
    // its next slot on the original grid.
    run(&mut state, at(35), &probe);
    assert_eq!(events().len(), 2);
    let s = store();
    assert_eq!(s.reminders.len(), 1);
    assert_eq!(s.reminders[0].due_at, at(40));
    run(&mut state, at(39), &probe);
    assert_eq!(events().len(), 2);
    run(&mut state, at(40), &probe);
    assert_eq!(events().len(), 3);
}

fn command_job() -> String {
    awareness::add_job(
        P,
        NewJob {
            kind: JobKind::Command,
            target: "cargo test".into(),
            label: Some("desktop tests".into()),
        },
    )
    .unwrap()
    .id
}

fn finish(id: &str, exit: i32, tail: &[&str]) {
    let dir = awareness::job_dir(P, id).unwrap();
    std::fs::create_dir_all(dir).unwrap();
    awareness::write_job_result(
        P,
        id,
        &JobResult {
            exit_code: Some(exit),
            signal: None,
            finished_at: Utc::now(),
            tail: tail.iter().map(|s| s.to_string()).collect(),
            error: None,
        },
    )
    .unwrap();
}

#[test]
fn a_finished_command_job_reports_its_exit_and_last_line() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    probe.alive.borrow_mut().insert(4242);
    let mut state = AwarenessTickState::default();
    let failing = command_job();
    awareness::set_job_runner_pid(P, &failing, 4242).unwrap();
    let passing = command_job();
    awareness::set_job_runner_pid(P, &passing, 4242).unwrap();

    run(&mut state, Utc::now(), &probe);
    assert!(events().is_empty(), "still running");

    finish(&failing, 3, &["running 4 tests", "test result: FAILED", ""]);
    finish(&passing, 0, &["ok"]);
    run(&mut state, Utc::now(), &probe);
    run(&mut state, Utc::now(), &probe);
    let ev = events();
    assert_eq!(ev.len(), 2, "{ev:?}");
    let log = awareness::job_output_path(P, &failing).unwrap();
    assert_eq!(
        ev[0],
        format!(
            r#"project=demo job id={failing} kind=command label="desktop tests" status=failed exit=3 detail="test result: FAILED" log={}"#,
            log.display()
        )
    );
    assert!(ev[1].contains(&format!("job id={passing} ")), "{}", ev[1]);
    assert!(ev[1].contains(" status=ok exit=0 "), "{}", ev[1]);
    assert!(store().jobs.is_empty());
}

#[test]
fn a_command_job_whose_runner_died_without_a_result_is_reported_failed() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    let id = command_job();
    awareness::set_job_runner_pid(P, &id, 999_999).unwrap();
    run(&mut state, Utc::now(), &probe);
    let ev = events();
    assert_eq!(ev.len(), 1);
    assert!(ev[0].contains(" status=failed "), "{}", ev[0]);
    assert!(ev[0].contains("without recording a result"), "{}", ev[0]);
}

#[test]
fn a_pid_job_fires_when_the_process_exits() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    probe.alive.borrow_mut().insert(77);
    let mut state = AwarenessTickState::default();
    awareness::add_job(P, NewJob { kind: JobKind::Pid, target: "77".into(), label: None }).unwrap();
    run(&mut state, Utc::now(), &probe);
    assert!(events().is_empty());
    probe.alive.borrow_mut().clear();
    run(&mut state, Utc::now(), &probe);
    assert_eq!(
        events(),
        [r#"project=demo job id=j1 kind=pid label="pid 77" status=exited"#]
    );
}

#[test]
fn ci_and_pr_watches_sample_on_their_cadence_and_only_online() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    awareness::add_job(P, NewJob { kind: JobKind::Ci, target: "abc123".into(), label: None }).unwrap();
    awareness::add_job(P, NewJob { kind: JobKind::Pr, target: "42".into(), label: None }).unwrap();

    probe.offline.set(true);
    run(&mut state, Utc::now(), &probe);
    assert_eq!(probe.remote_calls.get(), 0, "no GitHub round-trip offline");

    probe.offline.set(false);
    run(&mut state, Utc::now(), &probe);
    assert_eq!(probe.remote_calls.get(), 2);
    run(&mut state, Utc::now(), &probe);
    assert_eq!(probe.remote_calls.get(), 2, "within the cadence: no new sample");
    assert!(events().is_empty(), "both still pending");

    // Settle both; force the next sample by forgetting the clock.
    *probe.ci.borrow_mut() = Some(WatchVerdict::Failed { check: "app-ci".into() });
    *probe.pr.borrow_mut() = Some(WatchVerdict::Merged);
    state.last_remote_probe.clear();
    run(&mut state, Utc::now(), &probe);
    assert_eq!(
        events(),
        [
            r#"project=demo job id=j1 kind=ci label="ci abc123" status=failed detail="failing check: app-ci""#,
            r#"project=demo job id=j2 kind=pr label="PR #42" status=merged"#,
        ]
    );
    assert!(store().jobs.is_empty());
}

#[test]
fn an_active_task_alerts_at_its_estimate_then_every_half_estimate_until_it_leaves_active() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    *probe.active.borrow_mut() =
        Some(HashMap::from([("t-1".to_string(), Some("alpha".to_string()))]));
    awareness::set_estimate(P, "t-1", mins(30), t0(), Some("alpha")).unwrap();

    for m in [0, 29, 30, 31, 44, 45, 50, 60] {
        run(&mut state, at(m), &probe);
    }
    assert_eq!(
        events(),
        [
            "project=demo task=t-1 overdue estimate=30m elapsed=30m workspace=alpha",
            "project=demo task=t-1 overdue estimate=30m elapsed=45m workspace=alpha",
            "project=demo task=t-1 overdue estimate=30m elapsed=1h workspace=alpha",
        ]
    );

    // Handoff: the task leaves the active set; the estimate is dropped quietly.
    *probe.active.borrow_mut() = Some(HashMap::new());
    run(&mut state, at(200), &probe);
    assert_eq!(events().len(), 3);
    assert!(store().estimates.is_empty());
}

#[test]
fn a_short_estimate_repeats_on_the_ten_minute_floor() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    *probe.active.borrow_mut() = Some(HashMap::from([("t".to_string(), None)]));
    awareness::set_estimate(P, "t", mins(10), t0(), None).unwrap();
    for m in [10, 15, 20, 25, 30] {
        run(&mut state, at(m), &probe);
    }
    let elapsed: Vec<_> = events()
        .iter()
        .map(|e| e.split("elapsed=").nth(1).unwrap().to_string())
        .collect();
    assert_eq!(elapsed, ["10m", "20m", "30m"]);
}

#[test]
fn estimates_are_left_alone_while_the_board_is_unreadable() {
    let _home = isolated_home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    awareness::set_estimate(P, "t", mins(10), t0(), None).unwrap();
    run(&mut state, at(60), &probe);
    assert!(events().is_empty());
    assert_eq!(store().estimates.len(), 1);
}

#[test]
fn the_job_runner_records_exit_code_and_output_tail() {
    let _home = isolated_home();
    let argv: Vec<String> = ["sh", "-c", "echo one; echo two >&2; exit 3"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    run_job_exec(P, "j9", &argv).unwrap();
    let result = awareness::read_job_result(P, "j9").unwrap().unwrap();
    assert_eq!(result.exit_code, Some(3));
    assert_eq!(result.tail, ["one", "two"]);

    let missing = vec!["/nonexistent/shelbi-test-binary".to_string()];
    run_job_exec(P, "j10", &missing).unwrap();
    let result = awareness::read_job_result(P, "j10").unwrap().unwrap();
    assert_eq!(result.exit_code, Some(127));
    assert!(result.detail().unwrap().contains("could not start"));
}

#[test]
fn commit_and_pr_verdicts_grade_the_rollup() {
    let check = |state: &str| {
        serde_json::json!({
            "__typename": "CheckRun", "name": "app-ci", "status": "COMPLETED",
            "conclusion": state, "detailsUrl": "", "checkSuite": {"app": {"slug": "github-actions"}}
        })
    };
    let commit = |nodes: Vec<serde_json::Value>| {
        serde_json::json!({"oid": "abc", "statusCheckRollup": {"contexts": {
            "totalCount": nodes.len(), "pageInfo": {"hasNextPage": false}, "nodes": nodes
        }}})
    };
    assert_eq!(
        commit_ci_verdict("abc", &commit(vec![check("SUCCESS")]), false).unwrap(),
        Some(WatchVerdict::Passed)
    );
    assert_eq!(
        commit_ci_verdict("abc", &commit(vec![check("FAILURE")]), false).unwrap(),
        Some(WatchVerdict::Failed { check: "app-ci".into() })
    );
    let pending = serde_json::json!({
        "__typename": "CheckRun", "name": "app-ci", "status": "IN_PROGRESS",
        "conclusion": null, "detailsUrl": "", "checkSuite": {"app": {"slug": "github-actions"}}
    });
    assert_eq!(commit_ci_verdict("abc", &commit(vec![pending]), true).unwrap(), None);
    // No checks yet: pending until the settle grace, then nothing to wait for.
    let empty = serde_json::json!({"oid": "abc", "statusCheckRollup": null});
    assert_eq!(commit_ci_verdict("abc", &empty, false).unwrap(), None);
    assert_eq!(commit_ci_verdict("abc", &empty, true).unwrap(), Some(WatchVerdict::Passed));

    let merged = serde_json::json!({"state": "MERGED"});
    assert_eq!(pull_request_verdict(1, &merged, false).unwrap(), Some(WatchVerdict::Merged));
    let closed = serde_json::json!({"state": "CLOSED"});
    assert_eq!(pull_request_verdict(1, &closed, false).unwrap(), Some(WatchVerdict::Closed));
    let mut required = check("FAILURE");
    required["isRequired"] = serde_json::Value::Bool(true);
    let open = serde_json::json!({"state": "OPEN", "mergeStateStatus": "BLOCKED",
        "commits": {"nodes": [{"commit": commit(vec![required])}]}});
    assert_eq!(
        pull_request_verdict(1, &open, false).unwrap(),
        Some(WatchVerdict::Failed { check: "app-ci".into() })
    );
}
