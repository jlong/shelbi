# Phase 3 design: the daemon takes over the poller

Status: design. Covers every item under "The daemon takes over the poller" in
the plan (`shelbi/Plans/removing-tmux.md`, revised 2026-10-02). This document is
the shared contract the Phase 3 subtasks build against so they do not each invent
their own lifecycle, cancellation, or open-project model.

Phase 3 moves the poller and supervision out of a UI process and into
`shelbi daemon`. With many clients and no guaranteed sidebar, the poller cannot
live in a TUI pane. The poller file already has no dependency on the rest of the
TUI crate, so the *move* is mechanical; the design work, captured here, is
everything around it.

tmux stays the default runtime on this branch. This phase is worth doing even on
tmux, because it removes the dependency on a sidebar process being alive. While
the daemon poller is on, the old sidebar poller must be disabled so two pollers
never run at once (`rt-daemon-poller`).

## The subtasks and who owns what

| Subtask | Owns |
| --- | --- |
| `rt-daemon-lifecycle` (this branch) | On-demand start, idle exit, the open-project record and its helpers, retiring the launchd/systemd units with an upgrade step, `daemon restart`/`status` without a supervisor, the captured login-shell environment |
| `rt-daemon-poller` | The per-project poller manager, moving the poller file into the daemon, disabling the old sidebar poller |
| `rt-daemon-layout-split` | Splitting layout out of the poller (session half to the daemon, layout half to clients as events) |
| `rt-daemon-cancellation` | Generations, subprocess deadlines, and the quit barrier for every daemon job |
| `rt-session-restarts-daemon` | Session processes restarting a dead daemon |
| `rt-mutations-daemon` | The control socket, per-issue mutation queue, expected state, and recheck before irreversible steps (also the second half of Phase 4a) |

The interfaces each subtask consumes are named inline below and collected under
[Interfaces](#interfaces-the-subtasks-build-against).

## Lifecycle

The daemon owns no PTYs, so nothing it holds is lost when it restarts. That is
what lets the lifecycle be on-demand instead of supervised.

### On-demand start

The first client that needs the daemon starts it. One shared helper,
`shelbi_state::ensure_daemon_running()`, is called by any client before it talks
to the daemon:

1. If the hub socket already answers a hello probe, return at once.
2. Otherwise, if the daemon's single-instance lock (`hub.sock.lock`) is not
   held, spawn `shelbi daemon` detached (its own session via `setsid`, all stdio
   redirected so a launching `ssh` or terminal does not hang).
3. Wait for the socket to answer, up to a deadline.

The helper is safe when several clients race. The daemon takes the single
`hub.sock.lock` flock for its whole lifetime at startup (`acquire_bind_lock`), so
if two clients both spawn a daemon, exactly one wins the bind and the losers exit
without touching the live socket. Racing clients therefore converge on exactly
one daemon regardless of interleaving.

Opening a project calls `ensure_daemon_running()`. It runs inside
`shelbi_orchestrator::ensure_dashboard`, so every open path (the CLI, the TUI
launcher, a supervision restart, a reload) brings the daemon up if it is down.

### Idle exit

The daemon exits when no project is open. A monitor thread polls the
open-project set (see [What "open" means](#what-open-means)); when it is empty
the thread flips the same stop flag the signal handler uses and wakes the accept
loop, and the daemon drains and exits through its normal shutdown path.

A short minimum-lifetime debounce covers two cases: it keeps a just-started
daemon alive long enough for the opener to record the open flag and for a
restart to verify the new version, and it avoids thrash when a user closes one
project and opens another a moment later. The debounce and poll interval have
environment overrides so tests can drive them fast.

Because the daemon owns no PTYs, an idle exit followed by a later on-demand start
is invisible to running agents: their session processes are untouched.

### Restart and status without a supervisor

With no launchd or systemd unit in the picture, `shelbi daemon restart` and
`shelbi daemon status` operate directly:

- **restart** retires any leftover supervisor unit (below), stops the running
  daemon (SIGTERM to the recorded PID, waited until the lock is released), and
  starts a fresh one on the current binary through `ensure_daemon_running()`.
  This is the path the version-mismatch flow already uses: after an upgrade a new
  CLI calls `shelbi daemon restart` so the daemon runs the current binary
  (`shelbi-state/src/hub_version.rs`).
- **status** reports from the lock and the socket, not from a supervisor: whether
  the single-instance lock is held, whether the socket answers, and the PID and
  version from the daemon PID record.

If the daemon crashes with no client attached, a session process restarts it
(`rt-session-restarts-daemon`): each session periodically checks whether the lock
is held and, if its project is still marked open, calls the same
`ensure_daemon_running()`. An open project always has an orchestrator session, so
there is always a watcher.

### Retiring the units

`shelbi daemon install` and `shelbi daemon uninstall` are removed, along with the
launchd plist and systemd unit templates and all the supervisor plumbing
(bootstrap, kickstart, unit rendering, PATH baking and self-heal).

Existing installs still have a unit on disk whose `KeepAlive` (launchd) or
`Restart=always` (systemd) loop would fight an on-demand daemon: it would respawn
the old binary after a restart stops it, and keep a daemon alive when no project
is open. So an upgrade step, run on hub start, stops and removes any installed
unit:

- macOS: `launchctl bootout` the agent and delete
  `~/Library/LaunchAgents/dev.shelbi.daemon.plist`.
- Linux: `systemctl --user disable --now` the unit and delete
  `~/.config/systemd/user/dev.shelbi.daemon.service`.

It is idempotent (a missing unit is a clean no-op) and discloses what it removed
with a `daemon-unit-retired` line on `events.log`. It runs in
`daemon serve`'s startup, the same "reconcile before serving" window that the
config-upgrade pass and the housekeeping sweeps already use, so the first time a
new daemon runs it clears the old supervisor for good.

## Environment

The daemon runs git, `gh`, workflow actions, and SSH. It must find those tools
and the user's real configuration, not the minimal environment of whatever
launched it (a detached spawn inherits the launcher's environment, and the old
launchd/systemd units handed the daemon a bare `PATH` that had to be healed to
find `gh`).

So at startup the daemon overlays the captured interactive login-shell
environment onto its own: the user's login shell is run once as
`$SHELL -l -i -c env`, the output parsed into variables, and each applied to the
daemon's process environment. The interactive (`-i`) form is required because
`.zshrc` is where nvm, fnm, and Homebrew PATH setup usually live and a plain
`-l -c` skips it. The result is cached for the process lifetime.

This is the same capture the session processes use for their explicit
environment (plan: "One process per session"). The capture helper lives in a
shared crate, `shelbi-core` (`login_env`), so `rt-session-process` reuses it for
session spawn rather than writing a second copy; see the README row. Sessions
additionally scrub terminal-identity variables and set their own `TERM`; the
daemon has no terminal and only needs the login PATH and config, so it overlays
the captured variables as-is.

The baked-PATH plumbing and its self-heal are deleted with the units: the login
environment supersedes them.

## What "open" means

A project is open when its `state.json` says so. The record is a boolean on the
per-project `State` struct, set when the project is opened and cleared when it is
quit. It is written with the forward-compatible `State` IO so a mixed-version
fleet round-trips it.

Open is deliberately *not* derived from the orchestrator session existing:
supervision must be able to restart a dead orchestrator in a project that is
still open, so "open" has to outlive the orchestrator process. It is set in
`ensure_dashboard` (the open path, idempotent on every bring-up and on a
supervision restart) and cleared on the quit/teardown path alongside the existing
`closed` event.

`rt-daemon-lifecycle` adds the field and three helpers that the rest of Phase 3
consumes:

- `set_project_open(project, open)` records or clears it.
- `is_project_open(project)` reads it.
- `list_open_projects()` returns every open project (a scan of registered
  projects filtered by the flag).

The idle-exit monitor and the per-project poller manager both key off
`list_open_projects()`, so the daemon supervises exactly the open set and exits
when it empties. This replaces today's tmux-session-derived "open project"
heuristic in the board refresher; `rt-daemon-poller` moves the board and poller
onto the record.

## Per-project pollers

The daemon is hub-global, but a sidebar was per-project. The daemon gains a
manager that runs one poller per open project, following the precedent of the
board refresh manager (`daemon/board.rs`, `spawn_refresh_manager`): on an
interval it reconciles the running pollers against `list_open_projects()`,
starting a poller for a newly opened project and stopping the one for a project
that has closed. `rt-daemon-poller` owns this manager and the move of the poller
file; it builds on the open-project record and the idle-exit stop flag defined
here.

## Layout leaves the poller

`ensure_dashboard`, `close_review_window`, `build_review_panel_no_focus`, and
`recover_parked_review_agent` mix two concerns: driving a session (start, stop,
restart) and arranging the UI (splitting panes, building the review panel). In a
daemon with many clients and no guaranteed UI, only the session half belongs in
the daemon.

`rt-daemon-layout-split` splits them: the session half stays with the daemon, and
the layout half becomes a change notification the daemon pushes so clients react
and arrange their own view. The split uses the pushed-notification channel below;
the session operations it keeps go through the `SessionBackend` seam from Phase 2.

## Cancellation

Two places in today's code abandon a blocked thread on the assumption that the
process is about to exit: the launch timeout in `issue start`
(`shelbi-cli/src/commands/issue.rs`) and the poller shutdown, which leaves
threads stuck on SSH for the OS to reap (`poller.rs`). In a long-lived daemon
neither assumption holds. An abandoned launch can wake after its rollback and
start an agent on the wrong task; a quit project's stuck poller thread survives
while another project keeps the daemon alive. `rt-daemon-cancellation` owns the
fix, which has three parts:

- **Generations.** Every job (a dispatch, a supervision restart, a poll cycle)
  carries a generation for its workspace and project. Before it spawns, kills, or
  writes state, it checks that its generation is still current and stops if not. A
  timeout or a project quit bumps the generation. This is what makes an abandoned
  launch that wakes late do nothing.
- **Deadlines.** Subprocess calls (SSH, git, `gh`) run with a deadline and are
  killed on cancellation rather than left to finish on their own. The pattern is
  the child-deadline wrapper already used in `github_store` (kill the whole
  process group on timeout).
- **The quit barrier.** Quitting a project bumps the generation and then waits,
  up to a bound, for that project's in-flight jobs to acknowledge cancellation
  before the project is marked closed. Clearing the open flag (above) is the
  signal that gates idle exit; the quit barrier is what makes "closed" mean "no
  job for this project is still running."

Tests that must pass (from the plan): a launch times out, the task is
redispatched, and the original job wakes up and does nothing; a project is quit
while its supervision is blocked on SSH, and the quit still completes; the daemon
is killed with no client attached and supervision resumes on its own.

## Probes

Supervision's liveness probe keeps its current shape; only its transport changes
as sessions move off tmux.

- **Pushed.** Title and exit become pushed events from the session process, not
  periodic `capture-pane` / liveness polls.
- **Periodic.** Screen sampling for stall, usage-limit, and dialog detection
  stays periodic and calls `snapshot`.
- **Three states preserved.** A session is dead, alive, or *unreachable*.
  Unreachable (a remote that cannot be contacted before a deadline) is never
  treated as dead, so a network blip never triggers a redispatch. This is the
  same three-state probe the tmux poller has today
  (`shelbi-poll-one-pane-sample-detectors`).

## Pushed change notifications

The `hub.sock` NDJSON protocol is unchanged, and every existing verb keeps its
exact behavior: workers write to it from shell hooks, and that contract stays
stable (tested by the existing worker-hook tests). On top of it, the daemon
pushes change notifications to connected clients so they do not have to poll:
board changes, workspace-status changes, and the layout events from the
layout split. Clients subscribe on connect and react; a client that is not
connected simply reads state on its next open, exactly as today. A `subscribe`
frame may carry a `project`: the daemon is hub-global but a UI client is
per-project, so a subscriber that names its project is streamed only that
project's changes, not a sibling's; omitting `project` streams every project's
changes. The mutation control socket (`rt-mutations-daemon`) is a separate
channel from this event socket.

## Interfaces the subtasks build against

- `shelbi_state::ensure_daemon_running()` — on-demand start; called by every
  client before it needs the daemon, and by `rt-session-restarts-daemon`.
- `shelbi_state::stop_daemon()` — stop the running daemon (SIGTERM to the PID,
  waited until the lock releases); used by `daemon restart`.
- `shelbi_state::set_project_open` / `is_project_open` / `list_open_projects` —
  the open-project record, consumed by the idle monitor, `rt-daemon-poller`'s
  manager, and the board refresher.
- `shelbi_state::hub_lock_path()` / `daemon_lock_held()` — the single-instance
  lock path and a non-blocking "is a daemon running or starting" probe, shared by
  the client start helper and the daemon's own `acquire_bind_lock`.
- `shelbi_core::login_env::login_shell_env()` — the cached login-shell
  environment, reused by `rt-session-process` for session spawn.
- The idle-exit stop flag and the startup "reconcile before serving" window in
  `daemon/serve.rs` — where `rt-daemon-poller` attaches its manager and the
  retire/env steps already run.
- The `hub.sock` NDJSON event socket and its pushed notifications — the channel
  `rt-daemon-layout-split` publishes layout events on; distinct from the mutation
  control socket `rt-mutations-daemon` adds.

## Versions and upgrades

Two policies, deliberately different (plan: "Versions and upgrades"):

- **Sessions** keep long-lived compatibility through the frozen protocol core: a
  session keeps the binary it started with, and newer clients, relays, and
  daemons must keep controlling weeks-old sessions. There is no "session too old"
  state.
- **Daemon, CLI, TUI, and desktop app** match exactly, as the existing mutation
  guard requires (`shelbi-state/src/hub_version.rs`). After an upgrade a new CLI
  restarts the daemon (the restart path above). Clients still on the old version
  are told by the daemon that they are out of date: a TUI re-execs itself, and
  the desktop app shows a relaunch prompt and sends no commands until relaunched.
  Read-only viewing of sessions keeps working meanwhile, because that goes
  through the session protocol.
- **Relays** are started fresh per connection from the installed binary, so they
  are always current and speak the frozen core to old sessions.

The upgrade test: upgrade the CLI with an old desktop app, an old TUI, and an old
worker session all running, and confirm the session stays usable, the daemon
restarts once, and both clients are prompted or re-exec'd.

Within `rt-daemon-lifecycle` the relevant slice is that the retire step and the
on-demand restart together guarantee the post-upgrade daemon runs the current
binary with no supervisor respawning the old one.

## Phase 3 order of work

Design first (this document). Then, in parallel on this branch:

1. On-demand start; retire the units with an upgrade step; the open-project
   record; the captured login environment; restart/status without a supervisor
   (`rt-daemon-lifecycle`).
2. Per-project poller manager; move the poller file; disable the old sidebar
   poller (`rt-daemon-poller`).
3. Split layout out of the poller (`rt-daemon-layout-split`).
4. Generations, deadlines, and the quit barrier (`rt-daemon-cancellation`).
5. Sessions restart a dead daemon (`rt-session-restarts-daemon`).

The mutation control socket (`rt-mutations-daemon`) lands with Phase 4a but
shares the daemon lifecycle defined here.
