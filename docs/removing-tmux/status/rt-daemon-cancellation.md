# rt-daemon-cancellation — In review

Generations, subprocess deadlines, and a quit barrier for every daemon job
(Phase 3, "The daemon takes over the poller" → Cancellation;
`docs/removing-tmux/phase3-daemon.md`). Brings the two places that abandon a
blocked thread — the launch timeout in `issue start` and the poller's shutdown
stuck on SSH — under a shared cancellation model.

## What landed

- **Cancellation core** `shelbi_orchestrator::cancel` (new module). A
  process-global, per-project registry of in-flight jobs:
  - `register(project, workspace, JobKind)` → a `JobGuard` held for the job's
    life; its lock-free `is_cancelled` / `is_current` is the generation check a
    job makes before it spawns, kills, or writes state. Dropping it deregisters
    the job (what the quit barrier waits on).
  - `bump_workspace(project, ws)` trips every live **Launch** job for that
    workspace (the launch-timeout case); a job registered *after* the bump gets
    a fresh flag — the new generation — so a redispatch survives while the
    abandoned launch stands down. A **Poll** job for the same workspace is
    deliberately untouched.
  - `quit_project(project)` trips every live job for the project and returns a
    `QuitBarrier`; `QuitBarrier::wait()` blocks up to `SHELBI_QUIT_BARRIER_MS`
    (default 10s) for them to acknowledge cancellation. Per-project keying means
    quitting one project never touches another's jobs.
  - `JobKind::{Launch, Poll}` distinguishes a dispatch (also cancelled by its
    workspace's timeout) from a long-lived poll/supervision thread (cancelled
    only by a quit).
- **Subprocess deadlines + kill-on-cancel.** The scope lives one crate down in
  `shelbi_ssh` (`CancelScope` / `enter_cancel_scope` / `current_cancel_scope`),
  re-exported through `cancel` as `Scope` / `enter_scope` / `current_scope`.
  While a scope is installed, the otherwise-unbounded `shelbi_ssh::run` /
  `run_capture` run with the scope's deadline and poll its cancel flag,
  SIGKILLing the whole process group on either — so the poller's previously
  unbounded `snapshot` / `title` / `get_env` reads are now bounded and a quit
  kills a wedged one. New `run_with_deadline_cancellable` is the shared core;
  `run_with_deadline` is a no-cancel wrapper. **No scope installed → byte-for-
  byte the old behavior**, so non-daemon callers and existing tmux behavior are
  unchanged.
- **Launch path** (`workspace::start_workspace_on_task` /
  `resume_workspace_on_task`): register a `Launch` job at entry, check it right
  after the dispatch lock (early bail) and again immediately before
  `deploy_and_spawn` (the irreversible step) — returning `Error::Cancelled`
  rather than starting an agent. `mutate::start`'s idle-timeout branch calls
  `bump_workspace` before rolling back, so the abandoned launch worker thread,
  if it wakes late, does nothing.
- **Poller** (`poller::run_poller_loop` + `run_workspace_poll_loop`): each loop
  registers its job (`Poll`, project-wide for the supervisor / per-workspace for
  the poll thread), installs a scope from the job's cancel flag
  (`SHELBI_POLL_SUBPROC_DEADLINE_MS`, default 60s), and breaks on
  `is_cancelled` alongside the existing shutdown flag.
- **Quit barrier wiring** (`daemon/poller.rs::reconcile`): when a project leaves
  the open set, `quit_project` + drop the poller + `barrier.wait()`, so no
  thread for a closed project survives (a wedged one is logged and left to the
  OS at process exit).
- **New error** `shelbi_core::Error::Cancelled` (+ `is_cancelled`): a job that
  stood down, distinct from a real failure.

## Acceptance criteria → tests

- **Launch times out, task redispatched, original wakes and does nothing** —
  `cancel::tests::an_abandoned_launch_that_wakes_late_does_nothing` and
  `a_job_registered_after_a_bump_belongs_to_the_new_generation`; the real wiring
  (gen check before `deploy_and_spawn`, bump on idle-timeout) is in place and
  clippy/build-verified.
- **Project quit while supervision blocked on SSH; quit completes within the
  bound, the blocked call is killed, no thread survives** — end-to-end over the
  real subprocess path in
  `cancel::tests::quitting_a_project_kills_a_job_blocked_on_a_real_subprocess`
  (+ `shelbi_ssh::tests::run_with_deadline_cancellable_kills_a_hung_child_on_cancellation`
  and `run_honors_an_ambient_cancel_scope`).
- **Daemon killed with no client, a session restarts it, supervision resumes on
  its own** — `tests/session_restarts_daemon.rs::supervision_resumes_on_its_own_after_the_daemon_is_restarted`
  (real `__session` + daemon binaries; proves the per-project poller lock is
  held again after the restart).
- **Every job checks its generation before spawn/kill/write-state** — launch and
  poll/supervision loops wired (above).
- **Every SSH/git/gh call has a deadline and is killed on cancellation** — gh
  (45s, already) and git (120s, already) were bounded; the poller's SSH reads
  are now bounded + cancellable via the scope.
- **Quitting one project leaves others untouched** —
  `cancel::tests::quitting_one_project_leaves_another_projects_jobs_untouched`
  and `daemon::poller::tests::closing_one_project_drains_its_jobs_and_leaves_another_untouched`.

## Audit — daemon threads, and how each is brought under the model

(Surface from the `rt-daemon-poller` / `rt-mutations-daemon` baseline.)

- **Per-project poller supervisor + per-workspace poll threads** — under `Poll`
  generations + a cancel scope; cancelled and drained by the quit barrier.
  **Done.**
- **Dispatch / launch** (control-socket `start` → `start_workspace_on_task`) —
  under `Launch` generations with the pre-spawn gen gate + timeout bump.
  **Done.**
- **Supervision restart** (`maybe_supervise_orchestrator`, on the supervisor
  thread) — gated by the supervisor's `Poll` job + scope: its SSH is killed on
  quit and the loop breaks. A restart pass already mid-flight when cancel trips
  can finish that one pass (its SSH is still killed); not a per-step gen check.
  **Mostly done; noted.**
- **Board-refresh manager** (`daemon/board.rs`) — read-only `gh` reads, already
  bounded (45s) and driven off the open set, so a closed project drops out. No
  spawn/kill/state-write that could act on stale intent, so it is left
  bounded-only rather than brought under generations. **Deliberately out of
  scope.**
- **Other mutations** (move/assign/edit over the control socket) — short state
  writes already guarded by `rt-mutations-daemon`'s recheck-before-irreversible;
  not long-running or SSH-blocking, so not wrapped in a generation. **Out of
  scope.**
- **Lifecycle loops** (signal listener, idle-exit monitor, unacked-message
  reaper, accept loops) — not per-project jobs; stop-flag driven. **N/A.**

## Deliberate decisions / limitations

- **The barrier runs in the daemon, not in `shelbi quit`.** The jobs live in the
  daemon process, and the quit trigger is the open flag going false (owned by
  `rt-daemon-lifecycle`). The poller manager observes the close and runs
  `quit_project` + `barrier.wait()`; `quit.rs` / `teardown.rs` are untouched.
- **`run_with_stdin` is not scope-bounded.** It is a send/inject path, not a
  poll read; left as-is to avoid changing injection semantics. Noted for a
  follow-up if a quit must also kill an in-flight injection.
- **Poll-read deadline is generous (60s default).** Bounding the previously
  unbounded reads must not flap a slow-but-alive host to `unreachable`;
  cancellation (a quit) kills promptly regardless of this bound via the flag.

## Notes

- No shipped template / default config changed (new env knobs only:
  `SHELBI_QUIT_BARRIER_MS`, `SHELBI_POLL_SUBPROC_DEADLINE_MS`), so no
  config-upgrade sniffer is needed.
- Builds on [[shelbi-rt-daemon-layout-split]] (daemon poller default-on),
  `rt-daemon-poller` (poller manager + per-project lock), and
  `rt-session-restarts-daemon` (the restart path criterion 3 leans on).
