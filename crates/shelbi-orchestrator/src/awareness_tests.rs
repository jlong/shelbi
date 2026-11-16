use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use shelbi_core::{Project, Result};
use shelbi_state::awareness::{self, JobKind, JobResult, JobStarter, NewJob};

use super::*;
use crate::test_lock::{isolated_home, IsolatedHome};

const P: &str = "demo";

/// An isolated home holding `demo`'s state dir: the awareness store writes
/// only into a project dir that already exists.
fn home() -> IsolatedHome {
    let home = isolated_home();
    std::fs::create_dir_all(shelbi_state::project_dir(P).unwrap()).unwrap();
    home
}

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
    /// Workspaces whose session still serves the task its jobs started on.
    live_workspaces: RefCell<HashSet<String>>,
    /// Reports handed to a worker: (workspace, prompt).
    delivered: RefCell<Vec<(String, String)>>,
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
    fn starter_live(&self, _: &Project, starter: &JobStarter) -> bool {
        self.live_workspaces.borrow().contains(&starter.workspace)
    }
    fn deliver_to_starter(&self, _: &Project, starter: &JobStarter, notice: JobNotice) {
        self.delivered.borrow_mut().push((starter.workspace.clone(), notice.prompt()));
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
    let _home = home();
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
    // The firing is remembered in the history.
    let history = awareness::read_reminder_history(P).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!((history[0].id.as_str(), history[0].text.as_str()), ("r1", "check the tap PR merged"));
    assert_eq!((history[0].due_at, history[0].fired_at), (at(2), at(2)));
}

#[test]
fn a_reminder_missed_while_the_daemon_was_down_fires_once_and_a_recurring_one_reschedules() {
    let _home = home();
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
    // Each firing has a record, the recurring one's two slots apart, newest
    // first; the recurring reminder is still pending as well.
    let history: Vec<(String, DateTime<Utc>)> = awareness::read_reminder_history(P)
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.due_at))
        .collect();
    assert_eq!(history, [("r2".to_string(), at(40)), ("r2".to_string(), at(0)), ("r1".to_string(), at(0))]);
    assert_eq!(store().reminders.len(), 1);
}

fn command_job() -> String {
    awareness::add_job(
        P,
        NewJob {
            kind: JobKind::Command,
            target: "cargo test".into(),
            label: Some("desktop tests".into()),
            estimate: None,
            started_by: None,
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
    let _home = home();
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
    let _home = home();
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
    let _home = home();
    let probe = FakeProbe::default();
    probe.alive.borrow_mut().insert(77);
    let mut state = AwarenessTickState::default();
    awareness::add_job(P, NewJob { kind: JobKind::Pid, target: "77".into(), label: None, estimate: None, started_by: None }).unwrap();
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
    let _home = home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    awareness::add_job(P, NewJob { kind: JobKind::Ci, target: "abc123".into(), label: None, estimate: None, started_by: None }).unwrap();
    awareness::add_job(P, NewJob { kind: JobKind::Pr, target: "42".into(), label: None, estimate: None, started_by: None }).unwrap();

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
    let _home = home();
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
    let _home = home();
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
    let _home = home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    awareness::set_estimate(P, "t", mins(10), t0(), None).unwrap();
    run(&mut state, at(60), &probe);
    assert!(events().is_empty());
    assert_eq!(store().estimates.len(), 1);
}

#[test]
fn the_job_runner_records_exit_code_and_output_tail() {
    let _home = home();
    let argv: Vec<String> = ["sh", "-c", "echo one; printf '\\033[31mtwo\\033[0m\\n' >&2; [ -t 1 ] && echo tty; exit 3"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    crate::job_exec::run_job_command(P, "j9", &argv, None).unwrap();
    let result = awareness::read_job_result(P, "j9").unwrap().unwrap();
    assert_eq!(result.exit_code, Some(3));
    // Both streams, through a terminal, written as plain text.
    assert_eq!(result.tail, ["one", "two", "tty"]);
    let log = std::fs::read_to_string(awareness::job_output_path(P, "j9").unwrap()).unwrap();
    assert_eq!(log, "one\ntwo\ntty\n");

    let missing = vec!["/nonexistent/shelbi-test-binary".to_string()];
    crate::job_exec::run_job_command(P, "j10", &missing, None).unwrap();
    let result = awareness::read_job_result(P, "j10").unwrap().unwrap();
    assert_eq!(result.exit_code, Some(127));
    assert!(result.detail().unwrap().contains("could not start"));
}

#[test]
fn the_job_runner_runs_with_the_environment_it_is_given() {
    let _home = home();
    let argv: Vec<String> = ["sh", "-c", "echo \"$JOB_SHELL_TEST\""].iter().map(|s| s.to_string()).collect();
    let env: std::collections::BTreeMap<String, String> = [
        ("JOB_SHELL_TEST".to_string(), "from-the-caller".to_string()),
        ("PATH".to_string(), std::env::var("PATH").unwrap_or_default()),
    ]
    .into();
    crate::job_exec::run_job_command(P, "j11", &argv, Some(&env)).unwrap();
    let result = awareness::read_job_result(P, "j11").unwrap().unwrap();
    assert_eq!(result.tail, ["from-the-caller"]);
}

#[test]
fn a_job_session_is_removed_once_the_job_has_left_the_store() {
    let _home = home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    let job = awareness::add_job(P, NewJob { kind: JobKind::Command, target: "true".into(), label: None, estimate: None, started_by: None }).unwrap();
    // A dead session directory for the job, and one for a workspace.
    let sessions = shelbi_state::sessions_dir().unwrap();
    let dead_session = |dir: &str, name: &str| {
        let path = sessions.join(dir);
        std::fs::create_dir_all(&path).unwrap();
        let meta = serde_json::json!({
            "id": dir, "name": name, "pid": 0, "launched_at": "2026-11-10T10:00:00Z",
            "cwd": "/", "argv": ["true"], "protocol_version": shelbi_proto::PROTOCOL_VERSION,
        });
        std::fs::write(path.join("meta.json"), meta.to_string()).unwrap();
        path
    };
    let job_dir = dead_session("jobsess", &job_session_name(P, &job.id));
    let ws_dir = dead_session("wssess", "demo/ws/job");
    assert!(
        shelbi_client::list(&sessions).unwrap().iter().any(|s| s.dir == job_dir),
        "fixture session is discoverable"
    );

    run(&mut state, at(0), &probe);
    assert!(job_dir.exists(), "a running job keeps its session");

    awareness::remove_job(P, &job.id).unwrap();
    run(&mut state, at(1), &probe);
    assert!(!job_dir.exists(), "an ended job's session is removed");
    assert!(ws_dir.exists(), "a workspace's session is never touched");
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

#[test]
fn a_finished_job_moves_into_the_history_with_how_it_ended() {
    let _home = home();
    let probe = FakeProbe::default();
    probe.alive.borrow_mut().insert(77);
    let mut state = AwarenessTickState::default();
    let failing = command_job();
    awareness::set_job_runner_pid(P, &failing, 4242).unwrap();
    awareness::add_job(P, NewJob { kind: JobKind::Pid, target: "77".into(), label: None, estimate: None, started_by: None }).unwrap();
    probe.alive.borrow_mut().insert(4242);
    finish(&failing, 3, &["running 4 tests", "test result: FAILED"]);
    run(&mut state, Utc::now(), &probe);

    // A fresh read, as after a hub restart: the record is on disk.
    let history = awareness::read_job_history(P).unwrap();
    assert_eq!(history.len(), 1, "{history:?}");
    let rec = &history[0];
    assert_eq!(rec.id, failing);
    assert_eq!(rec.kind, JobKind::Command);
    assert_eq!(rec.status, JobStatus::Failed);
    assert_eq!(rec.exit_code, Some(3));
    assert_eq!(rec.detail.as_deref(), Some("test result: FAILED"));
    assert_eq!(rec.log, Some(awareness::job_output_path(P, &failing).unwrap()));
    assert_eq!(rec.display_label(), "desktop tests");
    assert!(rec.finished_at >= rec.started_at);

    // The pid watch exits: recorded too, newest first.
    probe.alive.borrow_mut().remove(&77);
    run(&mut state, Utc::now() + chrono::Duration::seconds(1), &probe);
    let history = awareness::read_job_history(P).unwrap();
    let ids: Vec<(&str, JobStatus)> = history.iter().map(|r| (r.id.as_str(), r.status)).collect();
    assert_eq!(ids, [("j2", JobStatus::Exited), (failing.as_str(), JobStatus::Failed)]);
    assert_eq!(history[0].log, None, "a watch has no output");
    assert!(store().jobs.is_empty());
}

#[test]
fn a_watch_verdict_is_recorded_with_its_status() {
    let _home = home();
    let probe = FakeProbe::default();
    let mut state = AwarenessTickState::default();
    awareness::add_job(P, NewJob { kind: JobKind::Pr, target: "42".into(), label: None, estimate: None, started_by: None }).unwrap();
    *probe.pr.borrow_mut() = Some(WatchVerdict::Closed);
    run(&mut state, Utc::now(), &probe);
    let history = awareness::read_job_history(P).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!((history[0].status, history[0].display_label().as_str()), (JobStatus::Closed, "PR #42"));
}

#[test]
fn a_canceled_job_is_recorded_as_canceled() {
    let _home = home();
    let pid = awareness::add_job(P, NewJob { kind: JobKind::Pid, target: "1".into(), label: Some("init".into()), estimate: None, started_by: None })
        .unwrap()
        .id;
    let cmd = command_job();
    finish(&cmd, 0, &["done"]);
    cancel_job(P, &pid).unwrap().unwrap();
    // A command that finished before the poller reported it keeps its result.
    cancel_job(P, &cmd).unwrap().unwrap();
    let history = awareness::read_job_history(P).unwrap();
    let by_id = |id: &str| history.iter().find(|r| r.id == id).unwrap();
    assert_eq!(by_id(&pid).status, JobStatus::Canceled);
    assert_eq!(by_id(&cmd).status, JobStatus::Ok);
    assert_eq!(by_id(&cmd).exit_code, Some(0));
    assert!(by_id(&cmd).log.is_some());
    assert!(store().jobs.is_empty());
    assert!(cancel_job(P, &pid).unwrap().is_none(), "canceled once");
}

/// A pid watch on `pid` that started at [`t0`], with `estimate`.
fn watch_since_t0(pid: u32, label: Option<&str>, estimate: Option<Duration>) -> String {
    let id = awareness::add_job(
        P,
        NewJob { kind: JobKind::Pid, target: pid.to_string(), label: label.map(str::to_string), estimate, started_by: None },
    )
    .unwrap()
    .id;
    awareness::update_awareness(P, |s| {
        let job = s.jobs.iter_mut().find(|j| j.id == id).unwrap();
        job.started_at = t0();
        if let Some(estimate) = estimate {
            job.set_estimate(estimate);
        }
        Ok(())
    })
    .unwrap();
    id
}

#[test]
fn a_job_past_its_estimate_alerts_then_every_half_estimate_until_it_finishes() {
    let _home = home();
    let probe = FakeProbe::default();
    probe.alive.borrow_mut().extend([77, 78]);
    let mut state = AwarenessTickState::default();
    watch_since_t0(77, Some("desktop tests"), Some(mins(30)));
    // No estimate: never overdue, however long it runs.
    watch_since_t0(78, None, None);

    for m in [0, 29, 30, 31, 44, 45, 50, 600] {
        run(&mut state, at(m), &probe);
    }
    let overdue: Vec<String> = events().into_iter().filter(|e| e.contains(" overdue ")).collect();
    assert_eq!(
        overdue[..2],
        [
            r#"project=demo job id=j1 overdue estimate=30m elapsed=30m label="desktop tests""#,
            r#"project=demo job id=j1 overdue estimate=30m elapsed=45m label="desktop tests""#,
        ]
    );
    // A late tick (the daemon was down) fires once, on the original grid.
    assert_eq!(overdue[2], r#"project=demo job id=j1 overdue estimate=30m elapsed=10h label="desktop tests""#);
    assert_eq!(overdue.len(), 3);
    assert!(!events().iter().any(|e| e.contains("id=j2")), "{:?}", events());
    assert_eq!(store().jobs[0].next_alert_at, Some(at(615)));
    let kinds: Vec<_> = std::fs::read_to_string(shelbi_state::events_log_path().unwrap())
        .unwrap()
        .lines()
        .map(|l| shelbi_state::EventEnvelope::from_log_line(l).kind)
        .collect();
    assert!(kinds.iter().all(|k| *k == shelbi_state::EventKind::JobOverdue), "{kinds:?}");

    // It finishes on a tick that is also an alert slot: its end is reported,
    // not that it ran long, and the history keeps the estimate.
    probe.alive.borrow_mut().remove(&77);
    run(&mut state, at(615), &probe);
    let after: Vec<String> = events().into_iter().skip(3).collect();
    assert_eq!(after.len(), 1, "{after:?}");
    assert!(after[0].starts_with("project=demo job id=j1 kind=pid"), "{after:?}");
    let record = awareness::find_job_record(P, "j1").unwrap().unwrap();
    assert_eq!(record.estimate, Some(mins(30)));
    run(&mut state, at(700), &probe);
    assert_eq!(events().len(), 4);
}

#[test]
fn a_revised_estimate_counts_from_the_start_and_alerts_next_tick_when_exceeded() {
    let _home = home();
    let probe = FakeProbe::default();
    probe.alive.borrow_mut().insert(77);
    let mut state = AwarenessTickState::default();
    let id = watch_since_t0(77, None, Some(mins(60)));
    run(&mut state, at(40), &probe);
    assert!(events().is_empty());
    // Revised down to 30m at 40m in: already past it.
    let revised = awareness::set_job_estimate(P, &id, mins(30)).unwrap().unwrap();
    assert_eq!(revised.next_alert_at, Some(at(30)));
    run(&mut state, at(41), &probe);
    assert_eq!(events(), [r#"project=demo job id=j1 overdue estimate=30m elapsed=41m label="pid 77""#]);
    assert_eq!(store().jobs[0].next_alert_at, Some(at(45)));
    // Revised up past now: quiet until the new estimate runs out.
    awareness::set_job_estimate(P, &id, mins(120)).unwrap();
    run(&mut state, at(119), &probe);
    assert_eq!(events().len(), 1);
    run(&mut state, at(120), &probe);
    assert_eq!(events().len(), 2);
    assert!(awareness::set_job_estimate(P, "j99", mins(5)).unwrap().is_none());
    assert!(awareness::set_job_estimate(P, &id, Duration::ZERO).is_err());
}

// ---------------------------------------------------------------------------
// Routing a worker's job back to it

fn alpha() -> JobStarter {
    JobStarter { workspace: "alpha".into(), agent: Some("developer".into()), task: Some("t1".into()) }
}

/// A pid watch on `pid` that `alpha` started at [`t0`], with `estimate`.
fn worker_watch(pid: u32, estimate: Option<Duration>) -> String {
    let id = watch_since_t0(pid, Some("desktop tests"), estimate);
    awareness::update_awareness(P, |s| {
        s.jobs.iter_mut().find(|j| j.id == id).unwrap().started_by = Some(alpha());
        Ok(())
    })
    .unwrap();
    id
}

#[test]
fn a_worker_started_job_reports_to_its_worker_and_not_the_feed() {
    let _home = home();
    let probe = FakeProbe::default();
    probe.live_workspaces.borrow_mut().insert("alpha".into());
    let mut state = AwarenessTickState::default();
    let id = awareness::add_job(
        P,
        NewJob { kind: JobKind::Command, target: "cargo test".into(), label: Some("desktop tests".into()), estimate: None, started_by: Some(alpha()) },
    )
    .unwrap()
    .id;
    finish(&id, 3, &["test result: FAILED"]);
    run(&mut state, Utc::now(), &probe);
    run(&mut state, Utc::now(), &probe);

    assert!(events().is_empty(), "the orchestrator's feed gets nothing: {:?}", events());
    let delivered = probe.delivered.borrow().clone();
    assert_eq!(
        delivered,
        [(
            "alpha".to_string(),
            format!(
                "Shelbi job {id} \"desktop tests\" finished: failed, exit 3. Last line: \"test result: FAILED\". \
                 Read the output: shelbi job log {id} -n 50"
            )
        )]
    );
    assert!(store().jobs.is_empty());
    let record = awareness::find_job_record(P, &id).unwrap().unwrap();
    assert_eq!(record.started_by, Some(alpha()), "the history keeps who started it");
}

#[test]
fn a_job_whose_worker_is_gone_reports_to_the_feed() {
    let _home = home();
    // `alpha`'s session ended, or now serves another task.
    let probe = FakeProbe::default();
    probe.alive.borrow_mut().insert(77);
    let mut state = AwarenessTickState::default();
    let id = worker_watch(77, None);
    probe.alive.borrow_mut().remove(&77);
    run(&mut state, at(5), &probe);

    assert!(probe.delivered.borrow().is_empty());
    let ev = events();
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert!(ev[0].starts_with(&format!("project=demo job id={id} kind=pid")), "{ev:?}");
}

#[test]
fn a_worker_started_jobs_overdue_alert_goes_to_the_worker() {
    let _home = home();
    let probe = FakeProbe::default();
    probe.alive.borrow_mut().insert(77);
    probe.live_workspaces.borrow_mut().insert("alpha".into());
    let mut state = AwarenessTickState::default();
    let id = worker_watch(77, Some(mins(30)));

    for m in [29, 30, 31, 45] {
        run(&mut state, at(m), &probe);
    }
    assert!(events().is_empty(), "{:?}", events());
    let delivered = probe.delivered.borrow().clone();
    assert_eq!(delivered.len(), 2, "{delivered:?}");
    assert!(delivered[0].1.starts_with(&format!("Shelbi job {id} \"desktop tests\" is past its estimate: running 30m, estimate 30m.")), "{delivered:?}");
    assert!(delivered[1].1.contains("running 45m"), "{delivered:?}");
    assert_eq!(store().jobs[0].next_alert_at, Some(at(60)), "the alert advanced as a feed one would");

    // The worker moves on: the next alert, and the end, go to the feed.
    probe.live_workspaces.borrow_mut().clear();
    run(&mut state, at(60), &probe);
    probe.alive.borrow_mut().remove(&77);
    run(&mut state, at(61), &probe);
    let ev = events();
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0], format!(r#"project=demo job id={id} overdue estimate=30m elapsed=1h label="desktop tests""#));
    assert!(ev[1].starts_with(&format!("project=demo job id={id} kind=pid")), "{ev:?}");
    assert_eq!(probe.delivered.borrow().len(), 2);
}
