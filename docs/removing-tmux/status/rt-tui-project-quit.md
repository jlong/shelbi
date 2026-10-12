# rt-tui-project-quit (Phase 4f)

**Status:** ready for review.

Project switching, the three quit actions, the redefined `shelbi reload`, and
out-of-date client handling for the single-process (session-backend) TUI. All
new behavior sits behind the `session_backend` dev flag; the tmux runtime
(`quit.rs`, `quit_project.rs`, `quit_shelbi.rs`, `teardown.rs`, pane-based
`reload`) is untouched until cutover.

## What landed

- **Control protocol** (`shelbi-proto`): `ClientMsg::{QuitProject, QuitShelbi,
  ReloadClients}` and `ServerMsg::Reexec`. Client methods + `Notice` enum in
  `shelbi-client` (`Subscription::recv` now yields `Changed`/`Reexec`).
- **Quit composition** (`shelbi-orchestrator::quit`): `quit_project` /
  `quit_shelbi` — mark closed first (watchdog contract), request the
  orchestrator handoff, end sessions (behind the `Sessions` seam), drain the
  quit barrier. Project-scoped; other projects untouched.
- **Daemon** (`daemon/control.rs`): handles the lifecycle commands behind a
  `LifecycleOps` seam; `QuitShelbi` acks then stops the daemon; a
  version-mismatched subscriber is told to re-exec on subscribe; `ReloadClients`
  broadcasts `Reexec`.
- **`shelbi reload`** (`commands/reload_session.rs`): session-runtime reload —
  handoff → signal clients re-exec → restart daemon → replace the orchestrator
  session; workers untouched. Behind a `ReloadOps` seam.
- **Out-of-date gate** (`shelbi-app::exec_daemon`): a version-mismatched client
  refuses to send a mutation (no change made) before it re-execs.
- **Shell** (`shelbi-tui::shell`): `q` = close UI (sessions survive);
  `switch_project` restores each project's last view; a background re-exec
  listener re-execs the TUI (restoring the project via argv + the view via
  `SHELBI_REEXEC_VIEW`) on a `Reexec` push or a post-restart version mismatch.
  The command palette's `dispatch_effect` routes `SwitchProject` →
  `switch_project`, `QuitProject`/`QuitShelbi` → the daemon lifecycle seam, and
  `AddProject` to an honest status note (creating a new project needs an
  in-process add-project form + a shared creation path — the CLI's `add_project`
  lives in shelbi-cli, out of this crate — so it ports in its own subtask).

## Acceptance criteria → tests

- Switch restores last view — `shell::tests::switching_projects_restores_each_projects_last_view`.
- `q` closes UI, sessions survive — `shell::tests::{close_ui_quits_without_touching_sessions, q_in_the_sidebar_closes_the_ui}`.
- Quit project ends only that project's sessions, waits for jobs, marks closed — `quit::tests::quit_project_{ends_only_that_projects_sessions_and_marks_it_closed, waits_for_the_quit_barrier_to_drain}`, `daemon::control::tests::quit_project_command_runs_quit_project_and_acks`.
- Quit Shelbi ends all + stops daemon, nothing restarts — `quit::tests::quit_shelbi_closes_every_project_before_ending_sessions` (close-before-end is the no-restart contract), `daemon::control::tests::quit_shelbi_command_acks_then_stops_the_daemon`.
- `shelbi reload` restarts daemon, replaces orch with handoff, re-execs TUIs, leaves workers — `reload_session::tests::reload_runs_handoff_signal_restart_then_replace_orchestrator`, `daemon::control::tests::reload_broadcasts_reexec_to_a_subscriber`.
- Upgrade: out-of-date client told to re-exec — `daemon::control::tests::an_out_of_date_subscriber_is_told_to_reexec_immediately`.
- Out-of-date client sends no mutations — `exec_daemon::tests::out_of_date_gate_refuses_a_version_mismatch_and_allows_a_match`.
- Palette Quit Project reaches the daemon command — `shell::tests::palette_quit_project_reaches_the_daemon_command` (drives `run_entry` → `dispatch_effect` → the lifecycle seam).

## Notes

- Rebased onto `jlong/remove-tmux` after `rt-tui-overlays` (#1488) and
  `rt-tui-native-views` (#1489) merged: `shell/mod.rs` keeps the overlays
  (palette, `dispatch_effect`, `spawn_job`) and the native views (`render_full`
  for Issues/Activity/Machines) while this subtask adds the re-exec listener,
  `switch_project`, and the quit routing. The obsolete `native_placeholder`
  helper was dropped (native views now render for real).

- A full multi-process upgrade e2e (spawn old daemon + old TUI, new CLI restarts,
  TUI re-execs) is left to CI / manual — it needs several real processes and is
  the flaky-under-load kind the worker instructions say not to loop on locally.
  The constituent mechanisms are unit-tested above.
- Per-client state across re-exec currently restores the project (argv) and the
  active view (`SHELBI_REEXEC_VIEW`); sidebar width / full per-project last-view
  map are in-process and reset on re-exec (a follow-up can serialize them).
