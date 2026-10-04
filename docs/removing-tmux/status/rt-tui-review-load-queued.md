# rt-tui-review-load-queued

Status: **ready for review**

Load a *queued* (handed-off, not-yet-on-a-slot) review from the single-process
TUI — the gap `rt-tui-review` left (it only opened reviews already serving on a
slot).

- Enter on a queued review in the sidebar, or the palette's load-review action,
  now resolves serving-vs-queued off the UI thread
  (`shelbi_orchestrator::review_session::review_open_target`). Serving → opens
  the native interface directly (unchanged); queued → raises the existing
  `review_confirm` overlay over the **free** review slots.
- Confirming loads the task onto the chosen slot through the daemon, off the UI
  thread, via a new `ReviewSessionOp::Load { workspace }` control op (daemon runs
  the same `load::load_review_task` the tmux Enter runs: checkout + boot +
  health-check + agent). On success the daemon publishes `ReviewOpened`; the
  initiating client opens the native interface then (a background `ReviewOpened`
  never steals another client's view, matching tmux's no-focus resume).
- Every review slot busy → the overlay reports it (a no-slots informational
  dialog) and loads nothing. Non-evicting by design; evicting an occupied slot
  is out of scope here (the tmux picker still evicts).
- The tmux review-load flow (`shelbi_tui::app::open_review_load_prompt` →
  `load_review_task_evicting`) is untouched.

Tests: overlay informational variant; shell queued→picker, all-busy→report,
confirm→load→ReviewOpened→interface (stubbed daemon), background-ReviewOpened
no-steal; orchestrator `review_open_target` serving-vs-queued.

`cargo build`/`clippy --all-targets -D warnings` green across the workspace.
Full `cargo test --workspace` not run locally end-to-end (busy-hub parallel
flakiness in the daemon-control fixtures: `create_dir_all`/socket-bind under
contention — they pass single-threaded); relying on CI for the authoritative
suite.
