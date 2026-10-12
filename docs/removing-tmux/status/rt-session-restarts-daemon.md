# rt-session-restarts-daemon — In review

Sessions restart the daemon (Phase 3, "Sessions restart the daemon" + the
"Daemon lifecycle" decision; `Plans/removing-tmux.md`). With the launchd/systemd
units retired (`rt-daemon-lifecycle`), nothing brings a crashed hub daemon back
while no UI is attached. An open project always has at least an orchestrator
session, so each `shelbi __session` process is now the watcher.

## What landed

- **`shelbi-session/src/daemon_watchdog.rs`** — a background thread spawned from
  `session::run`. On each tick it checks the daemon's single-instance lock and
  this session's open-project record; if the lock is **free** and the project is
  **open**, it starts the daemon via the installed binary's `__ensure-daemon`
  seam (the same on-demand helper clients use, `ensure_daemon_running`).
- **Installed binary, not this image.** The daemon is resolved from `$SHELBI_BIN`
  or the first executable `shelbi` on `$PATH` — never the session's own
  `current_exe`, so an old session brings up the current daemon rather than
  resurrecting a stale image. Resolution is a pure, unit-tested function.
- **Explicit captured environment.** The `__ensure-daemon` helper is handed the
  captured login-shell environment (`shelbi_core::login_shell_env`) plus this
  home's `SHELBI_HOME`/`SHELBI_ROOT`, with `env_clear` first — never the
  session's own child environment — and is `setsid`-detached.
- **Races are harmless.** Many sessions may start at once; the daemon's bind
  lock converges on exactly one, the losers exit. The watchdog's lock check is
  only an optimization.
- **Not when closed.** The quit/teardown paths clear the open flag before their
  sessions exit, so a session shutting down for a quit sees `open == false` and
  never restarts the daemon.

## Interval and jitter (load)

The check adds no measurable load: the default period is **15 s base + a random
0–10 s jitter** per session (`DEFAULT_INTERVAL` / `DEFAULT_JITTER` in
`daemon_watchdog.rs`). The jitter spreads many sessions' probes so they do not
all wake — or race to spawn — on the same tick. Tests drive the loop fast via
`SHELBI_DAEMON_WATCH_INTERVAL_MS` / `SHELBI_DAEMON_WATCH_JITTER_MS` (ms).

## Tests

- Unit (`shelbi-session`): the start decision matrix, project-name parsing,
  interval parsing, jitter bounds, and installed-binary resolution preferring a
  PATH/`$SHELBI_BIN` entry over this image.
- Integration (`crates/shelbi-cli/tests/session_restarts_daemon.rs`, real
  `__session` binary): an open project's session restarts a SIGKILLed daemon
  within a few intervals; several sessions converge on exactly one daemon; a
  closed project's session never starts one.

## Notes

- No config-upgrade sniffer: the behavior lives in the session binary and is
  gated only on env-var overrides, not persisted/template config.
- Builds on [[shelbi-rt-daemon-lifecycle]] (open-project record, lock helpers,
  `__ensure-daemon` seam) and the `rt-session-process` session binary.
