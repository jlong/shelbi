//! A review slot's editor and diff sessions across its tasks, against real
//! `shelbi __session` processes.
//!
//! The session name belongs to the slot and a slot serves one task after
//! another, so what's under test is that a task only ever gets sessions spawned
//! for it: closing a decided review ends them, and one left behind anyway is
//! replaced rather than reused with the previous task's diff still on screen.
//!
//! A cargo test's `current_exe` is the harness, so the sessions spawn through
//! `ensure_content_session_with` and the real binary. Each test has its own
//! `SHELBI_HOME`, so nothing here can reach a hub's sessions.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use shelbi_client::DiscoveredSession;
use shelbi_core::{Column, Host, Issue, Project, WorkspaceSpec};
use shelbi_orchestrator::review_session::{
    close_review, content_session_name, ensure_content_session_with, ReviewContentRole,
};
use shelbi_orchestrator::review_ui::close_review_window;
use shelbi_orchestrator::session_backend::backend;

const PROJECT: &str = "demo";
const SLOT: &str = "review-1";
const ROLES: [ReviewContentRole; 2] = [ReviewContentRole::Editor, ReviewContentRole::Diff];

/// These tests mutate process-global env (`SHELBI_HOME`, `EDITOR`).
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    home: tempfile::TempDir,
    worktree: PathBuf,
    _env: MutexGuard<'static, ()>,
}

impl Fixture {
    /// A project with one review slot, whose worktree is a repo holding the
    /// branches `task-a` (adds `a.txt`) and `task-b` (adds `b.txt`). The diff
    /// tool writes the files it's shown to `diff.log`, then stays up as a real
    /// one would; the editor is `cat`, which does too.
    fn new() -> Self {
        let env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("SHELBI_HOME", home.path());
        std::env::set_var("EDITOR", "cat");

        let work_dir = home.path().join("repo");
        let worktree = work_dir.join(".shelbi/wt").join(SLOT);
        std::fs::create_dir_all(&worktree).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "test@example.com"],
            &["config", "user.name", "Test"],
            &["commit", "-q", "--allow-empty", "-m", "base"],
        ] {
            git(&worktree, args);
        }
        for (branch, file) in [("task-a", "a.txt"), ("task-b", "b.txt")] {
            git(&worktree, &["checkout", "-q", "-b", branch, "main"]);
            std::fs::write(worktree.join(file), "x\n").unwrap();
            git(&worktree, &["add", "."]);
            git(&worktree, &["commit", "-q", "-m", branch]);
        }

        let log = home.path().join("diff.log");
        let yaml = format!(
            "name: {PROJECT}\n\
             repo: r\n\
             machines:\n  - {{ name: hub, kind: local, work_dir: {work_dir} }}\n\
             orchestrator: {{ runner: claude }}\n\
             agent_runners:\n  claude: {{ command: claude, flags: [] }}\n\
             review:\n  diff_command: git diff --name-only {{base}} {{head}} > {log}; exec cat\n",
            work_dir = work_dir.display(),
            log = log.display(),
        );
        let mut project = Project::from_yaml_str(&yaml).unwrap();
        // The id is the config file's name, which `name:` doesn't set.
        project.name = PROJECT.into();
        project.workspaces = vec![WorkspaceSpec {
            name: SLOT.into(),
            machine: "hub".into(),
            tags: vec!["review".into()],
            slot: None,
        }];
        shelbi_state::save_project(&project).unwrap();

        Self { home, worktree, _env: env }
    }

    /// Put `task` in `column`, on the slot, branch `branch`.
    fn save_task(&self, task: &str, branch: &str, column: Column) {
        let now = chrono::Utc::now();
        let issue = Issue {
            id: task.into(),
            title: task.into(),
            column,
            priority: 0,
            assigned_to: Some(SLOT.into()),
            workflow: None,
            branch: Some(branch.into()),
            pr: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            params: std::collections::BTreeMap::new(),
            created_at: now,
            updated_at: now,
        };
        shelbi_state::save_task(PROJECT, &issue, "body").unwrap();
    }

    /// Load `task` onto the slot: the card and the checkout, as a dispatch does.
    fn load(&self, task: &str, branch: &str) {
        self.save_task(task, branch, Column::review());
        git(&self.worktree, &["checkout", "-q", branch]);
    }

    fn ensure(&self, task: &str, role: ReviewContentRole) {
        ensure_content_session_with(PROJECT, task, role, |spec| {
            shelbi_client::spawn_with_exe(Path::new(env!("CARGO_BIN_EXE_shelbi")), &spec)
                .map(drop)
                .map_err(|e| shelbi_core::Error::Other(e.to_string()))
        })
        .unwrap();
    }

    fn ensure_both(&self, task: &str) {
        for role in ROLES {
            self.ensure(task, role);
        }
    }

    fn live(&self, role: ReviewContentRole) -> Option<DiscoveredSession> {
        let sessions = shelbi_client::list(&shelbi_state::sessions_dir().unwrap()).unwrap();
        shelbi_client::choose_session(&sessions, &content_session_name(PROJECT, SLOT, role))
    }

    /// Wait for the sessions to be up, and return their ids.
    fn wait_live(&self, task: &str) -> Vec<String> {
        ROLES
            .map(|role| {
                let session = wait_for(|| self.live(role)).expect("the session should come up");
                assert_eq!(session.meta.task.as_deref(), Some(task));
                assert_eq!(session.meta.cwd, self.worktree);
                session.short_id
            })
            .to_vec()
    }

    fn wait_gone(&self) {
        for role in ROLES {
            wait_for(|| self.live(role).is_none().then_some(()))
                .expect("the session should be gone");
        }
    }

    /// Wait for the diff tool to report exactly `file`.
    fn wait_diff_shows(&self, file: &str) {
        let log = self.home.path().join("diff.log");
        let shown = wait_for(|| {
            let text = std::fs::read_to_string(&log).ok()?;
            (text.trim() == file).then_some(())
        });
        assert!(
            shown.is_some(),
            "the diff should show only {file}, it showed {:?}",
            std::fs::read_to_string(&log),
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Runs even when an assertion panicked, and only ever sees this test's
        // own sessions: `SHELBI_HOME` is still this fixture's.
        for role in ROLES {
            let name = content_session_name(PROJECT, SLOT, role);
            let _ = backend().kill_window(&Host::Local, &name);
        }
        std::env::remove_var("SHELBI_HOME");
        std::env::remove_var("EDITOR");
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn closing_a_decided_review_ends_its_sessions_and_the_next_task_gets_its_own() {
    let fx = Fixture::new();
    fx.load("task-a", "task-a");
    fx.ensure_both("task-a");
    fx.wait_live("task-a");
    fx.wait_diff_shows("a.txt");

    // Approved: the card has left review by the time the client's Close arrives.
    fx.save_task("task-a", "task-a", Column::done());
    close_review(PROJECT, "task-a").unwrap();
    fx.wait_gone();

    fx.load("task-b", "task-b");
    fx.ensure_both("task-b");
    fx.wait_live("task-b");
    fx.wait_diff_shows("b.txt");
}

#[test]
fn a_session_left_by_another_task_is_replaced_and_its_own_is_reused() {
    let fx = Fixture::new();
    fx.load("task-a", "task-a");
    fx.ensure_both("task-a");
    let first = fx.wait_live("task-a");
    fx.wait_diff_shows("a.txt");

    // Asking again for the same task is a no-op.
    fx.ensure_both("task-a");
    assert_eq!(fx.wait_live("task-a"), first);

    // Decided with no Close ever arriving (the client went away).
    fx.save_task("task-a", "task-a", Column::done());
    fx.load("task-b", "task-b");
    fx.ensure_both("task-b");
    let second = fx.wait_live("task-b");
    for id in &first {
        assert!(!second.contains(id), "task-a's session must not be reused");
    }
    fx.wait_diff_shows("b.txt");
}

#[test]
fn a_late_close_leaves_the_next_tasks_sessions_alone() {
    let fx = Fixture::new();
    fx.save_task("task-a", "task-a", Column::done());
    fx.load("task-b", "task-b");
    fx.ensure_both("task-b");
    let serving = fx.wait_live("task-b");

    // Task A's Close, overtaken by the load of the next task.
    close_review(PROJECT, "task-a").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fx.wait_live("task-b"), serving);

    // Task B's own teardown takes them, the way evicting it from the slot does.
    close_review_window(PROJECT, "task-b").unwrap();
    fx.wait_gone();
}
