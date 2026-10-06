# rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched-as-the-tui-opens

Status: ready for review.

Fixed the single-process TUI settling on a permanent "no live session" error
when it connects during an orchestrator relaunch (daemon restart SIGTERMs the
old `<project>/orch` and spawns a new one a beat later).

- New retryable `ConnectFailure::Awaiting(msg)`: no live session yet but one is
  expected. `LiveConnector` raises it for the orchestrator when an exited
  session is on disk or the project declares one (`should_await_session` /
  `orchestrator_declared`). The connect worker retries it on the existing
  `RetryPolicy` cadence and attaches as soon as a live session binds; on
  deadline it surfaces the carried message (exited session's last line) as-is.
- `SessionManager::show` no longer no-ops on a re-selected *failed*/idle target,
  so re-selecting Chat re-attempts without a quit/reopen.
- All crate-local behavior; no new deps (no Cargo.lock change, no MSRV concern).
- Tests: 5 new (retry-then-live, give-up message, `should_await_session`
  predicate incl. lazy declared-check, re-show re-attempt). `shelbi-tui` build +
  clippy clean; all 118 shell tests pass.
