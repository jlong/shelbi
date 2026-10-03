# rt-daemon-layout-split — In review

Split layout out of the daemon poller (Phase 3, "Layout leaves the poller";
`docs/removing-tmux/phase3-daemon.md`). The poller now drives only the session
half of each split and publishes a typed layout event; a client arranges its own
view in response.

## What landed

- **Typed layout events.** `shelbi_state::LayoutEvent`
  (`OrchestratorRestarted`, `ReviewOpened`, `ReviewClosed`,
  `ReviewAgentRecovered`) carried on `ChangeNotification::Layout` over the
  existing `rt-daemon-poller` change bus. No tmux details on the wire.
  `publish_layout` + `ChangeNotification::from_line` helpers.
- **Session half in the orchestrator.** `supervise_restart_orchestrator<B:
  SessionBackend>` restarts a crashed orchestrator in place through the backend
  seam — generic so it is testable with a stub backend, no tmux and no client.
- **Poller is layout-free.** The four call sites
  (`ensure_dashboard`, `close_review_window`, `build_review_panel_no_focus`,
  `recover_parked_review_agent`) now do session/state work + publish a layout
  event. The parked-agent decision is a read-only probe over the `SessionBackend`
  seam (`get_env` + pane `probe`), not a `review_ui` pane call.
- **tmux client reacts.** The sidebar subscribes to layout events — over the hub
  socket when the daemon runs the poller (the default), and over its own
  in-process bus when the setting is off — and performs today's pane/window work
  (`ensure_dashboard` / `review_ui::*`), so visible behavior is unchanged. It is
  the always-present dashboard pane, so headless restart is preserved.
- **State query for late clients.** `review_ui::review_layout_state` derives the
  review slots a connecting client should lay out from the board alone (no tmux);
  the sidebar lays out from it on start.
- **Default flip.** `daemon_poller_enabled()` now defaults on; only an explicit
  falsy `SHELBI_DAEMON_POLLER` (`0`/`false`/`no`/`off`) restores the in-sidebar
  poller.

## Notes

- No shipped template / default config changed, so no config-upgrade sniffer is
  needed (the setting is an env var, not persisted project config).
- Pane-level behavior (the `review_ui` splits/swaps, `ensure_dashboard` layout)
  is unchanged and remains CI/integration-validated; the new seams are
  unit-tested (event round-trip, wire parse, stub-backend restart, state-query
  derivation).
- Known limitation: the late-client state query reconciles only *opens*
  (`review_layout_state` adds the panels a client should show). A `ReviewClosed`
  event missed while no client was connected is not reconciled on reconnect —
  the done task's review window lingers until the next load reaps the slot. On
  tmux the sidebar is the dashboard's own pane and is effectively always up, so
  the live event delivers; full close-reconciliation lands with the TUI cutover.
