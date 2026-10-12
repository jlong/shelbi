# rt-tui-native-views — In progress

Phase 4c: Issues, Activity, and the new Machines view in the single-process TUI
shell, behind the `session_backend` dev flag. The tmux runtime's standalone view
processes are unchanged and share the same rendering code.

## Approach (confirmed with the orchestrator)

- **Embed the existing `KanbanApp` / `ActivityApp`** and render them through their
  existing `render_full(frame, app, area)` into the shell's main-area Rect — one
  rendering implementation shared with the standalone `run_tasks` / `run_activity`
  processes. No second renderer over the shelbi-app models.
- **Issue moves** route through the shelbi-app executor as the single entry point:
  the shared in-process move fn when `dev.daemon_mutations` is off, the daemon
  control socket when on. Injected into the embedded board via a move-persist seam;
  the standalone process keeps the direct `GitTransitionRunner` path. The existing
  optimistic move + rollback + gated-merge-across-edge behavior is preserved.
- **Refresh** is off the UI thread and change-notification driven: the embedded
  views are fed from snapshots read on a background worker and from daemon change
  notifications; they do not run their own `refresh()` on the UI thread. `refresh()`
  is split into `read_*_data()` (IO) + `apply_*_data()` (fold) so the standalone
  path is byte-identical.
- **Machines** is a new shared view (`machines.rs`, `render_full`) replacing the
  `while true; sleep 5; shelbi workspace list` shell loop; a new `run_machines`
  standalone process drives it for the tmux runtime. It lists machines with their
  workspaces, state, and task, the resolved remote `shelbi` path + version where
  `rt-machine-setup` has landed, and opens a workspace's session on Enter.

## Status

In review. Issues/Activity/Machines render in the shell's main area from the
sidebar; the board moves through the shelbi-app executor (direct when
`dev.daemon_mutations` off, daemon when on) with its optimistic move + rollback +
gated-merge intact; refresh is off-thread (`shell/refresh.rs`) and change-driven
(`shell/changes.rs`); the new `machines.rs` view (shared by `shelbi __machines`)
replaces the `workspace list` shell loop. `cargo build/test/clippy --workspace`
green. Follow-ups left for later phases: live per-machine SSH reachability probe
in the machines view, project-switch UI (4f), overlays (4d).
