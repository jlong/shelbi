# rt-workspace-sidebar-task-info-agent-diff-editor-when-navigating-into-a-workspace

Status: ready for review.

Opening a dev workspace that has a running task now swaps the nav sidebar for a
**workspace sidebar** (the way a review replaces it): back button + status
(`IN PROGRESS` bold yellow `#dbc300`), task info with a `More` popover, the
worktree line, and a nav block that switches the main area between the
workspace's **agent** session, its **diff**, and an **editor**. Opening an
**idle** workspace keeps the regular sidebar and shows the redesigned idle
placeholder (per John's 2026-10-07 correction).

Key decisions / notes:

- **Shared panel module.** Extracted the back button + status header, task-info
  block, worktree row, nav-block rendering, description preview and openers into
  `crates/shelbi-tui/src/panel.rs`; both `review_panel` and the new
  `workspace_panel` use it (no copy-paste). Review panel output is unchanged
  (its render tests still pass).
- **Content sessions reuse the review machinery, generalized to any workspace.**
  New `shelbi_orchestrator::workspace_session` (twin of `review_session`) spawns
  `<project>/ws/<workspace>/<role>` editor/diff sessions from the same
  `review_ui` command builders; a new `ClientMsg::WorkspaceSession` control op
  (protocol v2 → **v3**) reaches it, mirroring `ReviewSession`. New
  `SessionRef::WorkspaceContent { workspace, role }`. No dev-server to reap, so
  close only ends the editor/diff sessions.
- **Status colour** keyed off the task column: `IN PROGRESS` yellow `#dbc300`
  (the usual case — the sidebar opens only for in-progress workspaces),
  `READY FOR REVIEW` cyan, `DONE` green, else the column name muted.
- **Chat → Orchestrator.** Renamed the first nav item to `Orchestrator` (glyph
  unchanged) at the model source; the palette entry derives from the label so it
  aligned too (the correction allowed either — aligning was the trivial path).
- **Back vs Close.** Back / Esc return to the nav sidebar leaving the content
  sessions loaded; `q` additionally tears the editor/diff sessions down.

No tmux, real `$HOME`, or `~/Library/LaunchAgents` touched by tests. Did not edit
the tracking table in `docs/removing-tmux/README.md`.
