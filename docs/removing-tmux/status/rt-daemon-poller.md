# rt-daemon-poller — Landed (on `jlong/remove-tmux`)

Per-project poller manager; moved the poller file into the daemon's domain;
disabled the sidebar poller when the daemon runs it.

## What landed

- **Moved the poller** `shelbi-tui/src/poller.rs` → `shelbi-orchestrator/src/poller.rs`
  (code + its 104 tests). It lives in `shelbi-orchestrator`, not `shelbi-cli`,
  because both the daemon (`shelbi-cli`) and the sidebar (`shelbi-tui`) must be
  able to run it and `shelbi-orchestrator` is the only crate both depend on; it
  also already held the poller's `supervision` and `session_backend` neighbors.
  Re-exported as `shelbi_tui::WorkspacePoller` at the old path so callers don't churn.
- **Per-project poller manager** `shelbi-cli/src/commands/daemon/poller.rs`
  (`spawn_poller_manager`), modeled on `board::spawn_refresh_manager`. One poller
  per open project, reconciled every 2s against `list_open_projects()`; starts a
  poller when a project opens, stops it when it closes — no daemon restart. Wired
  into `daemon/serve.rs` startup next to the refresh manager and idle monitor.
- **The setting** `SHELBI_DAEMON_POLLER` (`shelbi_state::daemon_poller_enabled`,
  default off, dev-only). On → daemon polls, sidebar doesn't; off → sidebar polls,
  daemon doesn't. Documented in the README "Dev settings" section.
- **Per-project lock** `shelbi_state::acquire_poller_lock` (flock on
  `<project_dir>/poller.lock`), taken inside `WorkspacePoller::start`. Guarantees
  exactly one poller per project even if a stale sidebar overlaps the switch; an
  inert start (lock held) is retried on the manager's next tick.
- **Pushed change notifications** in-process `shelbi_state::change_bus`
  (`subscribe_changes` / `publish_change` / `ChangeNotification`) + a `subscribe`
  verb on `hub.sock` that streams NDJSON change lines to a connected client. The
  board refresher publishes `Board` changes; `append_workspace_event` publishes
  `Workspace` changes. The existing `hub.sock` NDJSON verbs are unchanged.

## Preserved (no behavior change)

- Probes still go through `SessionBackend` (routed in `rt-backend-callers`).
- The three-state probe (dead / alive / unreachable; unreachable never treated
  as dead) is unchanged — it moved verbatim with the poller.
- With `SHELBI_DAEMON_POLLER` off (the default), behavior is exactly as before.

## Round 1 review fix (flaky change-bus test)

The change bus is process-global, so a concurrent test's publish reached a
subscriber under test before its own change did (the failure the review hit:
`project:"proj"` arriving at the `project:"p"` subscriber). Fixed the isolation,
not the one assertion:

- **`ChangeNotification::project()`** accessor added, so a subscriber can filter
  the shared bus down to the one project it cares about.
- **Server-side project filter on `subscribe`.** A `subscribe` frame may carry
  `project`; `stream_changes` then streams only that project's changes and drops
  the rest. `subscribe_project_filter` parses it (`Some(None)` = all projects,
  `Some(Some(p))` = only `p`). This makes the `serve.rs` test deterministic for a
  real client, not just the test — a per-project sidebar now subscribes with its
  own project and isn't woken by a sibling's churn.
- **All three change-bus tests now filter on a project name unique to the test.**
  The two `shelbi-state` bus tests use a `recv_for(sub, project, within)` helper
  that drains foreign-project changes until the deadline; the `serve.rs` test
  subscribes with a unique project and relies on the server-side filter.
- **New tests:** `a_project_scoped_subscriber_ignores_other_projects_changes`
  (filter drops a sibling project, publishing the foreign change first each loop
  so a leak would fail) and `subscribe_project_filter_parses_optional_project`.
- Ran `cargo test -p shelbi-state` (full suite, 857 tests) and the `serve`
  tests three times each under parallel execution — stable.

**Decision on client-side project filtering (review asked):** yes, a real
`subscribe` client should filter by project, and now does so **server-side** —
the daemon is hub-global but a UI client is per-project, so filtering at the
source avoids waking a sidebar for a board it isn't showing and keeps the push
channel cheap. The `project` field is optional: a client that wants everything
(a future global monitor) omits it. The notification still carries `project`, so
a client can additionally filter client-side if it ever multiplexes projects on
one subscription.

## Out of scope (other subtasks)

Layout split (`rt-daemon-layout-split`), generations/cancellation/quit barrier
(`rt-daemon-cancellation`), the mutation control socket (`rt-mutations-daemon`).
