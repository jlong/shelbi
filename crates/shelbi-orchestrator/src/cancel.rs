//! Daemon job cancellation: generations, subprocess deadlines, and a quit
//! barrier (Phase 3 of the remove-tmux effort, `rt-daemon-cancellation`;
//! design: `docs/removing-tmux/phase3-daemon.md`).
//!
//! Two places in today's code abandon a blocked thread on the assumption that
//! the process is about to exit: the launch timeout in `issue start`
//! ([`crate::mutate::start`]) and the poller's shutdown, which leaves per-
//! workspace threads stuck on SSH for the OS to reap ([`crate::poller`]). In a
//! long-lived daemon neither assumption holds — an abandoned launch can wake
//! after its rollback and start an agent on the *wrong* task, and a quit
//! project's stuck poller thread survives while another project keeps the
//! daemon alive.
//!
//! This module is the shared mechanism that brings every daemon job under
//! control. It has three parts, matching the design:
//!
//! - **Generations.** Every job registers a [`JobGuard`] keyed by its project
//!   and (for workspace-scoped work like a dispatch) its workspace. Before it
//!   spawns, kills, or writes state, it checks [`JobGuard::is_current`] and
//!   stops if not. A job's guard carries a cancellation *flag* that is its
//!   generation marker: a [`bump_workspace`] (a launch timeout) trips the flag
//!   of every live job for that workspace, and a job registered *after* the
//!   bump gets a fresh flag — so it belongs to the new generation and survives.
//!   A [`quit_project`] trips every live job's flag for the project.
//!
//! - **Deadlines.** Subprocess calls (SSH, git, `gh`) a job makes run with a
//!   wall-clock deadline and are killed — process group included — on
//!   cancellation rather than left to finish. A job installs a [`Scope`] (its
//!   cancel flag + a per-call deadline) for the duration of its work; the
//!   SSH-backed session backend consults [`current_scope`] and routes its
//!   otherwise-unbounded reads through the deadline-and-cancel runner, so a
//!   wedged host can no longer pin a poll thread for the daemon's lifetime.
//!
//! - **The quit barrier.** [`quit_project`] trips the project's jobs and
//!   returns a [`QuitBarrier`]; [`QuitBarrier::wait`] blocks up to a bound for
//!   those jobs to acknowledge cancellation (drop their guards) before the
//!   caller marks the project closed. That is what makes "closed" mean "no job
//!   for this project is still running," and — because the registry is keyed
//!   per project — quitting one project never touches another's jobs.
//!
//! The registry is process-global (the daemon's jobs run on many threads with
//! no single owner to thread a handle through, matching how the daemon already
//! uses a process-global change bus). It is keyed per project, so tests that
//! use distinct project names are independent and need no shared reset.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default wall-clock bound the quit barrier waits for a project's in-flight
/// jobs to acknowledge cancellation before the project is marked closed.
/// Generous enough to cover an in-flight SSH/git call being killed on its
/// deadline and the thread unwinding, while still bounding a genuinely wedged
/// job so a quit never hangs the daemon.
pub const DEFAULT_QUIT_BARRIER_MS: u64 = 10_000;

/// Env override (milliseconds) for the quit barrier bound, clamped to a sane
/// range so a fat-fingered value can't make a quit hang or cut a legitimate
/// kill short. Tests drive it fast.
const QUIT_BARRIER_ENV: &str = "SHELBI_QUIT_BARRIER_MS";

/// How often [`QuitBarrier::wait`] re-checks whether the project drained.
const BARRIER_POLL: Duration = Duration::from_millis(10);

/// The quit barrier bound, env-overridable via `SHELBI_QUIT_BARRIER_MS`.
pub fn quit_barrier_bound() -> Duration {
    let ms = std::env::var(QUIT_BARRIER_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_QUIT_BARRIER_MS)
        .clamp(50, 120_000);
    Duration::from_millis(ms)
}

/// What kind of daemon job this is — which cancellations apply to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// A dispatch/launch of a workspace. Cancelled by a [`quit_project`] *and*
    /// by a [`bump_workspace`] for its workspace — the launch-timeout case: when
    /// a launch times out and its generation is bumped, the abandoned launch
    /// must stand down while a later redispatch (a fresh generation) runs.
    Launch,
    /// A long-lived poll or supervision thread. Cancelled only by a
    /// [`quit_project`] — never by a single workspace's launch timeout, which
    /// has nothing to do with whether that workspace should still be observed.
    Poll,
}

/// One live daemon job's registry entry.
struct LiveJob {
    /// The workspace this job is scoped to, or `None` for project-wide work (a
    /// poll supervisor tick, a supervision restart, the orchestrator).
    workspace: Option<String>,
    /// What kind of job this is — gates which bumps cancel it.
    kind: JobKind,
    /// Tripped when the job is cancelled — superseded by a [`bump_workspace`]
    /// or quit by [`quit_project`]. The guard's lock-free cancellation probe
    /// reads this; a subprocess deadline loop polls it to kill its child early.
    flag: Arc<AtomicBool>,
}

/// Per-project live-job table. No generation counter is stored: a cancellation
/// *trips the flags of the jobs that exist at bump time*, and a job registered
/// afterward gets a fresh (untripped) flag, which is exactly the generation
/// semantics the design calls for.
#[derive(Default)]
struct ProjectState {
    live: HashMap<u64, LiveJob>,
}

#[derive(Default)]
struct Registry {
    projects: HashMap<String, ProjectState>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry::default()))
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry().lock().unwrap_or_else(|p| p.into_inner())
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A job's handle on the cancellation registry, held for the job's duration.
/// Dropping it deregisters the job (so the quit barrier can tell the project
/// has drained). Cloning the [`cancel_flag`](Self::cancel_flag) hands the same
/// lock-free cancellation signal to a subprocess deadline loop.
pub struct JobGuard {
    project: String,
    id: u64,
    flag: Arc<AtomicBool>,
}

impl JobGuard {
    /// Cheap, lock-free: has this job been cancelled — its generation
    /// superseded, or its project quit? A job checks this before it spawns,
    /// kills, or writes state.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// The inverse of [`is_cancelled`](Self::is_cancelled): is this job still
    /// the current generation for its (project, workspace)? Spelled out as its
    /// own method because the call sites read as a positive gate
    /// ("if the job is still current, spawn").
    pub fn is_current(&self) -> bool {
        !self.is_cancelled()
    }

    /// Clone the cancellation flag to hand to a subprocess deadline loop, which
    /// polls it and SIGKILLs its child the moment the job is cancelled — so an
    /// SSH/git/`gh` call is killed on cancellation, not left to finish.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        let mut reg = lock();
        if let Some(ps) = reg.projects.get_mut(&self.project) {
            ps.live.remove(&self.id);
            if ps.live.is_empty() {
                reg.projects.remove(&self.project);
            }
        }
    }
}

/// Register an in-flight daemon job for `project`, scoped to `workspace` (or
/// `None` for project-wide work) and of the given [`JobKind`]. The returned
/// [`JobGuard`] belongs to the current generation (a fresh, untripped flag);
/// hold it for the job's whole lifetime.
pub fn register(project: &str, workspace: Option<&str>, kind: JobKind) -> JobGuard {
    let flag = Arc::new(AtomicBool::new(false));
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut reg = lock();
    let ps = reg.projects.entry(project.to_string()).or_default();
    ps.live.insert(
        id,
        LiveJob {
            workspace: workspace.map(str::to_string),
            kind,
            flag: flag.clone(),
        },
    );
    JobGuard {
        project: project.to_string(),
        id,
        flag,
    }
}

/// Bump the generation of one workspace in a project: trip the cancellation
/// flag of every live **launch** job scoped to that workspace. Called when a
/// launch times out, so the abandoned launch — which may wake much later — sees
/// itself stale and does nothing, while a *later* redispatch (a job registered
/// after this bump) gets a fresh flag and runs. A poll thread for the same
/// workspace is deliberately untouched: a launch timeout says nothing about
/// whether the workspace should still be observed.
pub fn bump_workspace(project: &str, workspace: &str) {
    let reg = lock();
    if let Some(ps) = reg.projects.get(project) {
        for job in ps.live.values() {
            if job.kind == JobKind::Launch && job.workspace.as_deref() == Some(workspace) {
                job.flag.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// Quit a project: trip the cancellation flag of every live job for it
/// (workspace-scoped and project-wide alike) and return a [`QuitBarrier`] the
/// caller waits on before marking the project closed. Jobs for *other*
/// projects are untouched.
pub fn quit_project(project: &str) -> QuitBarrier {
    let reg = lock();
    if let Some(ps) = reg.projects.get(project) {
        for job in ps.live.values() {
            job.flag.store(true, Ordering::SeqCst);
        }
    }
    QuitBarrier {
        project: project.to_string(),
    }
}

/// How many live jobs a project currently has registered. Used by the quit
/// barrier and by tests.
pub fn live_job_count(project: &str) -> usize {
    lock()
        .projects
        .get(project)
        .map(|ps| ps.live.len())
        .unwrap_or(0)
}

/// Returned by [`quit_project`]: a handle to wait for the project's cancelled
/// jobs to acknowledge cancellation (drop their guards) before the project is
/// marked closed.
pub struct QuitBarrier {
    project: String,
}

impl QuitBarrier {
    /// Wait up to the configured [`quit_barrier_bound`] for every job in the
    /// project to finish. Returns `true` if the project fully drained, `false`
    /// if the bound elapsed first (a genuinely wedged job the caller logs and
    /// leaves to the OS at process exit).
    pub fn wait(&self) -> bool {
        self.wait_for(quit_barrier_bound())
    }

    /// [`wait`](Self::wait) with an explicit bound, so the timing is unit
    /// testable without the env knob.
    pub fn wait_for(&self, bound: Duration) -> bool {
        let start = Instant::now();
        loop {
            if live_job_count(&self.project) == 0 {
                return true;
            }
            if start.elapsed() >= bound {
                return false;
            }
            std::thread::sleep(BARRIER_POLL);
        }
    }
}

// ---------------------------------------------------------------------------
// Subprocess cancellation scope
//
// A job installs a scope (its cancel flag + a per-call deadline) on its own
// thread for the duration of its work. The scope lives one crate down, in
// `shelbi_ssh`, so the *unbounded* SSH entry points (`run` / `run_capture`,
// which the poller's snapshot / title / get_env reads go through) can honor it
// without a dependency on this crate and without threading a deadline through
// every call site. We re-export it here so a daemon job in this crate installs
// one through the cancellation module it already uses. With no scope installed
// (every non-daemon caller) SSH behavior is byte-identical to before, so
// existing tmux behavior is unchanged.
// ---------------------------------------------------------------------------

pub use shelbi_ssh::{CancelScope as Scope, CancelScopeGuard as ScopeGuard};

/// Install `deadline` + `cancel` as the current thread's cancellation scope for
/// as long as the returned guard lives. A poll/supervision job wraps its
/// subprocess-making work in this with its own [`JobGuard::cancel_flag`], so a
/// quit (or a timeout) kills the in-flight SSH call instead of waiting it out.
pub fn enter_scope(deadline: Duration, cancel: Arc<AtomicBool>) -> ScopeGuard {
    shelbi_ssh::enter_cancel_scope(deadline, cancel)
}

/// The current thread's cancellation scope, if a job installed one.
pub fn current_scope() -> Option<Scope> {
    shelbi_ssh::current_cancel_scope()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Each test uses a project name unique to it (the registry is keyed per
    /// project), so tests are independent without a shared reset. A short
    /// suffix keeps names readable in failure output.
    fn proj(tag: &str) -> String {
        format!(
            "cancel-test-{tag}-{}",
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn a_fresh_job_is_current_and_counted() {
        let p = proj("fresh");
        let g = register(&p, Some("alpha"), JobKind::Launch);
        assert!(g.is_current());
        assert!(!g.is_cancelled());
        assert_eq!(live_job_count(&p), 1);
        drop(g);
        assert_eq!(live_job_count(&p), 0, "dropping the guard deregisters");
    }

    #[test]
    fn bumping_a_workspace_cancels_only_that_workspaces_live_jobs() {
        let p = proj("bump");
        let a = register(&p, Some("alpha"), JobKind::Launch);
        let b = register(&p, Some("beta"), JobKind::Launch);
        let wide = register(&p, None, JobKind::Poll);

        bump_workspace(&p, "alpha");

        assert!(a.is_cancelled(), "alpha's job is superseded");
        assert!(!b.is_cancelled(), "beta's job is untouched");
        assert!(
            !wide.is_cancelled(),
            "a project-wide job is not a workspace job"
        );
    }

    #[test]
    fn a_launch_timeout_does_not_cancel_the_workspaces_poll_thread() {
        // A launch timing out bumps the workspace generation to stand down the
        // abandoned launch — but it must NOT cancel the poll thread for the same
        // workspace, which has its own reason to keep observing the slot.
        let p = proj("bump-vs-poll");
        let launch = register(&p, Some("alpha"), JobKind::Launch);
        let poll = register(&p, Some("alpha"), JobKind::Poll);

        bump_workspace(&p, "alpha");

        assert!(launch.is_cancelled(), "the launch is superseded");
        assert!(
            poll.is_current(),
            "the poll thread keeps observing the workspace"
        );
    }

    #[test]
    fn a_job_registered_after_a_bump_belongs_to_the_new_generation() {
        // The crux of acceptance criterion 1: a redispatch to the SAME
        // workspace after a launch timeout must survive, while the abandoned
        // original stays cancelled.
        let p = proj("redispatch");
        let original = register(&p, Some("alpha"), JobKind::Launch);
        bump_workspace(&p, "alpha"); // the launch timed out
        let redispatch = register(&p, Some("alpha"), JobKind::Launch); // the task is redispatched

        assert!(original.is_cancelled(), "the abandoned launch is stale");
        assert!(
            redispatch.is_current(),
            "the redispatch is the current generation and runs"
        );
    }

    #[test]
    fn an_abandoned_launch_that_wakes_late_does_nothing() {
        // Model the launch worker: it blocks (on "git/ssh"), then — before its
        // irreversible spawn — checks its generation. The parent times out and
        // bumps the workspace while the worker is still blocked. When the
        // worker finally proceeds it must not spawn.
        let p = proj("late-wake");
        let spawned = Arc::new(AtomicU64::new(0));

        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let worker = {
            let p = p.clone();
            let spawned = spawned.clone();
            std::thread::spawn(move || {
                let guard = register(&p, Some("alpha"), JobKind::Launch);
                // Signal that the job is registered and now "blocked on git".
                parked_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                // Pre-spawn generation check — the irreversible step.
                if guard.is_current() {
                    spawned.fetch_add(1, Ordering::SeqCst);
                }
            })
        };

        // Wait for the worker to register and park, then time out + redispatch.
        parked_rx.recv().unwrap();
        bump_workspace(&p, "alpha");
        release_tx.send(()).unwrap();
        worker.join().unwrap();

        assert_eq!(
            spawned.load(Ordering::SeqCst),
            0,
            "the abandoned launch woke after the timeout and must not spawn"
        );
    }

    #[test]
    fn quitting_a_project_cancels_all_its_jobs_and_the_barrier_drains() {
        let p = proj("quit");
        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let handles: Vec<_> = ["alpha", "beta"]
            .iter()
            .map(|w| {
                let p = p.clone();
                let w = w.to_string();
                let parked_tx = parked_tx.clone();
                std::thread::spawn(move || {
                    let guard = register(&p, Some(&w), JobKind::Poll);
                    let scope = enter_scope(Duration::from_secs(60), guard.cancel_flag());
                    parked_tx.send(()).unwrap();
                    // "Blocked on SSH": spin until the job is cancelled, then
                    // unwind (as a killed subprocess call would let the thread).
                    while !guard.is_cancelled() {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    drop(scope);
                })
            })
            .collect();
        drop(parked_tx);
        // Both jobs are registered and parked.
        parked_rx.recv().unwrap();
        parked_rx.recv().unwrap();
        assert_eq!(live_job_count(&p), 2);

        let barrier = quit_project(&p);
        assert!(
            barrier.wait_for(Duration::from_secs(5)),
            "the quit barrier drained within the bound"
        );
        assert_eq!(live_job_count(&p), 0, "no job for the quit project survives");
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn quitting_one_project_leaves_another_projects_jobs_untouched() {
        // Acceptance criterion 6.
        let quitting = proj("isolate-quit");
        let other = proj("isolate-other");
        let doomed = register(&quitting, Some("alpha"), JobKind::Poll);
        let survivor = register(&other, Some("alpha"), JobKind::Poll);

        let barrier = quit_project(&quitting);
        assert!(doomed.is_cancelled(), "the quit project's job is cancelled");
        assert!(
            survivor.is_current(),
            "another open project's job keeps running"
        );
        // The barrier can't drain while the doomed job's guard is still held…
        assert!(!barrier.wait_for(Duration::from_millis(30)));
        drop(doomed);
        assert!(barrier.wait_for(Duration::from_secs(1)), "drains once released");
        // The survivor and its project are wholly untouched by the quit.
        assert!(survivor.is_current());
        assert_eq!(live_job_count(&other), 1);
    }

    #[test]
    fn quitting_a_project_kills_a_job_blocked_on_a_real_subprocess() {
        // Acceptance criterion 2, end-to-end over the real subprocess path: a
        // job registers, installs its cancel scope, and blocks in
        // `shelbi_ssh::run` on a child that would outlive the test. Quitting the
        // project trips the job; the scope kills the child; the job returns and
        // the barrier drains well within its bound — the blocked call is killed
        // and no thread for the project survives.
        let p = proj("ssh-kill");
        let (started_tx, started_rx) = mpsc::channel();
        let worker = {
            let p = p.clone();
            std::thread::spawn(move || {
                let job = register(&p, None, JobKind::Poll);
                let _scope = enter_scope(Duration::from_secs(120), job.cancel_flag());
                started_tx.send(()).unwrap();
                // Blocks until the scope kills it on cancellation.
                shelbi_ssh::run(&shelbi_core::Host::Local, ["sleep", "30"])
            })
        };
        started_rx.recv().unwrap();
        // Let the child actually start before we quit.
        std::thread::sleep(Duration::from_millis(100));

        let start = Instant::now();
        let barrier = quit_project(&p);
        assert!(
            barrier.wait_for(Duration::from_secs(5)),
            "the barrier drained after the blocked call was killed"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "quit completed within the bound ({:?})",
            start.elapsed()
        );
        assert_eq!(live_job_count(&p), 0, "no thread for the quit project survives");

        let res = worker.join().unwrap();
        let err = res.expect_err("the scoped run was killed, not completed");
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted, "err: {err}");
    }

    #[test]
    fn the_barrier_times_out_on_a_wedged_job() {
        let p = proj("wedged");
        let _wedged = register(&p, None, JobKind::Poll); // never dropped → never acknowledges
        let barrier = quit_project(&p);
        assert!(
            !barrier.wait_for(Duration::from_millis(40)),
            "a job that never finishes trips the bound rather than hanging"
        );
    }

    #[test]
    fn a_scope_is_installed_for_the_jobs_duration_and_then_cleared() {
        assert!(current_scope().is_none(), "no ambient scope by default");
        let flag = Arc::new(AtomicBool::new(false));
        {
            let _s = enter_scope(Duration::from_secs(3), flag.clone());
            let scope = current_scope().expect("scope installed");
            assert_eq!(scope.deadline, Duration::from_secs(3));
            assert!(!scope.cancel.load(Ordering::SeqCst));
        }
        assert!(current_scope().is_none(), "scope cleared on guard drop");
    }

    #[test]
    fn scopes_nest_and_restore() {
        let outer = Arc::new(AtomicBool::new(false));
        let inner = Arc::new(AtomicBool::new(false));
        let _o = enter_scope(Duration::from_secs(1), outer.clone());
        {
            let _i = enter_scope(Duration::from_secs(2), inner.clone());
            assert_eq!(current_scope().unwrap().deadline, Duration::from_secs(2));
        }
        assert_eq!(
            current_scope().unwrap().deadline,
            Duration::from_secs(1),
            "dropping the inner scope restores the outer"
        );
    }

    #[test]
    fn quit_barrier_bound_is_env_overridable_and_clamped() {
        let _g = crate::test_lock::acquire();
        let prev = std::env::var_os(QUIT_BARRIER_ENV);
        std::env::set_var(QUIT_BARRIER_ENV, "250");
        assert_eq!(quit_barrier_bound(), Duration::from_millis(250));
        // Clamped: an absurd value can't make a quit hang.
        std::env::set_var(QUIT_BARRIER_ENV, "99999999");
        assert_eq!(quit_barrier_bound(), Duration::from_millis(120_000));
        std::env::set_var(QUIT_BARRIER_ENV, "0");
        assert_eq!(quit_barrier_bound(), Duration::from_millis(50));
        match prev {
            Some(v) => std::env::set_var(QUIT_BARRIER_ENV, v),
            None => std::env::remove_var(QUIT_BARRIER_ENV),
        }
    }
}
