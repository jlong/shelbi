# rt-redesign-the-review-panel-with-task-info-and-a-more-popover-figma

Status: ready for review.

Redesigned the review panel (`crates/shelbi-tui/src/review_panel.rs`) to the
Figma: back button + bold-cyan status on one row, new task-info block (title +
3-line markdown-stripped description preview + cyan `More`), worktree line, nav
switches in the sidebar slot style, and `✅ Approve` / `❌ Reject` on one row
with no brackets in the design colours (#5acd25 / #e04f52).

`More` / the `m` key opens the board's task-detail popover over the main area by
reusing the kanban renderer: `kanban::render_task_popover_into` +
`popover_header` were extracted (`pub(crate)`) so the board and the review show
the identical box. The review opens it as a new `ActiveOverlay::TaskDescription`
variant (overlay and review are independent shell fields, so the review content
session keeps running underneath; Esc closes, j/k scroll).

Task title/body reach the panel via a new `task` field threaded through
`ReviewOpenInfo` → `ReviewOpenParams` → `ReviewInterface::new` (fetched from the
`IssueFile` the orchestrator's `review_open_info_for_slot` already reads). The
`IssueFile` is boxed in the enum-carrying types to keep clippy's
`large_enum_variant` quiet; `ReviewOpenInfo`/`ReviewOpenTarget` dropped their
`PartialEq`/`Eq` derives (one orchestrator test switched to a `match`).

New theme tokens: `ACCENT_CYAN` (#00a6b2), `FG_SECONDARY` (#bababa),
`ACTION_RED` (#e04f52); reused `PALETTE_FG` (#c6c6c6) and `PALETTE_GREEN`
(#5acd25).

Not done here: the right-edge `▏` #414141 divider is the shell's column (owned by
the parallel `rt-sidebar-spacing-colors-…` sidebar restyle), so it's left as-is.

No shipped-default/template changes, so no config-upgrade sniffer is needed.
`cargo build`, `cargo clippy --all-targets` clean for shelbi-tui + shelbi-orchestrator;
review_panel / shell / overlays / review / kanban-popover / review_session tests pass.
