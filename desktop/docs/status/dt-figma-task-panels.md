# dt-figma-task-panels

The Workspace and Review panels (the task views that stand in the sidebar's
column) restyled to the Figma **Navigation** component's State=Workspace
(`2220:18764`) and State=Review (`2220:17158`) on the sheet `2220:17016`
(fileKey `pu3hxHgWoCDPCzJ4u6zTQK`), in both themes, and the Review screen
matched to its frames (`2165:275` dark, `2220:21066` light). Values came from
the Figma MCP (`get_design_context` on both component states,
`get_variable_defs` on the two Review frames), with the fills Figma draws
through per-frame variable overrides measured from the 1x PNGs.

## What changed

- **The sidebar's chrome stays around a panel** (`views/sidebar.rs`
  `render_panel_column`, used by the window for the `column-review` and
  `column-workspace` layers): the 52pt titlebar (Search, Toggle sidebar) and
  the footer (the unread-errors and Settings buttons, the Zen pill) are the
  default sidebar's own, the pill 32pt as these two states draw it (the
  default state's is 42pt). The titlebar and footer now take their inputs
  directly (`render_titlebar(palette_chord)`, `render_footer(model, link,
  pill_h)`), and the column layers have element ids so two layers' controls
  never share hover state mid-push.
- **Shared panel pieces** (`views/task_panel.rs`), top to bottom:
  - the status row: the back button (28pt square, `theme.background` fill
    deepening on hover and press, 6pt corners, `arrow-left`) and the task's
    column, 16pt in and 12pt apart: the status glyph (16pt) and the label in
    caps, X-Small/Semibold in `theme.foreground`. The glyphs are the board's
    (`style::status_heading_in`, a sibling of the column header's
    `status_heading`); IN PROGRESS is `icon/cyan` here in both themes
    (`style::panel_status_icon`), where the board's column uses `icon/select`
    (pink in Light), as the frames draw it. REVIEW is `icon/purple`.
  - the issue summary, 18pt in, 8pt apart: the title (Base/Medium, loud), the
    description (X-Small on an 18pt line, `theme.foreground`) clamped to five
    lines with an ellipsis, and More (X-Small/Medium, `text/link`) which
    expands it in place and then reads Less. The old popover is gone.
  - the worktree row: `folder` (newly vendored Heroicon), the path's last
    four components after `…/` (`…/shelbi/.shelbi/wt/alpha`, the start
    clipped further only when even that doesn't fit), and Open, the X-Small
    Default button (`style::xsmall_button`), which reveals it in Finder.
    State=Workspace pads the row 8pt above and below, State=Review not at
    all; both are matched.
  - the views, 12pt in, 24pt above and below, 4pt apart: the sidebar's nav
    item (`sidebar::nav_item`, now shared with the default sidebar's nav
    rows) with the sliding chip, each with its ⌘N hint.
  - Review only: the Actions section header (Small/Medium muted) and Approve
    (Success, `check`) and Reject (Danger, `x-mark`) as Small buttons, 28pt,
    the 12pt icon slot (`style::small_icon_button`, which the board's New
    issue now goes through too). Both still run the registry commands, so
    their confirm overlays are unchanged.
- **Workspace panel** (`views/workspace.rs` `render_panel`): Developer
  (named after the agent), View Diff, Edit in <editor>, Shell, ⌘1 to ⌘4.
  The live state label, the workspace name and agent role, the issue id, and
  the branch and machine rows are gone (the component draws none). Shell is a
  toggle: open, it holds the overlay fill and a loud label; "starting…"
  replaces its hint while the daemon starts it. A remote workspace has no
  Open, and its path's tooltip names the machine.
- **Review panel** (`views/review.rs` `render_panel`): Reviewer, View Diff,
  Edit in <editor>, Web Browser (when a URL exists), Shell, ⌘1 to ⌘5, then
  Actions. "Web Browser" is the desktop's label; the shared
  `ReviewSwitch::label` (the TUI's) keeps "Open Browser".
- **The review's user shell is new** (John, 2026-10-18, q-001): Shell opens a
  login shell in the review slot's worktree beside the reviewer, with the
  Workspace view's draggable split. It is the workspace's
  `WorkspaceSessionOp::EnsureShell` / `CloseShell` on the slot (a review slot
  is a workspace; new `ReviewOps::shell_op`), and switching or leaving the
  review closes it. The split moved out of `workspace.rs` into
  `workspace_layout::render_split` / `ShellSplit`, which both views use.
  Shared: `PanelRow::Shell` and `PanelEffect::ToggleShell` in
  `shelbi_app::review`, offered only through `panel_rows_with_shell` /
  `ReviewPanelState::with_shell` (the desktop's); `panel_rows` and
  `ReviewPanelState::new` (the TUI's) never include it, and the TUI maps
  `ToggleShell` to no action, as it already does for the workspace's.
- **⌘1 to ⌘N pick the panel's views** while a panel holds the column
  (`app.rs` `nav_chord` routes `NavChord::Row` to `ReviewView::view_shortcut`
  / `WorkspaceView::view_shortcut`); the sidebar's numbered rows answer them
  otherwise. ⌘O / ⌘I / ⌘L are unchanged. Focus stays where it was, so ⌘2
  from the reviewer's terminal shows the diff without moving the keyboard.
- **Removed**: `style::row_state` (every caller now passes `NAV_RADIUS` to
  `row_state_rounded`), `nav_chip_layer`, `NAV_ROW_H`,
  `TRAFFIC_LIGHT_INSET`, `Hue::Green`, `style::chip` (its last caller was
  the Shell row's on/off chip), and the `chat-bubble-left-right`,
  `folder-open` and `share` Heroicons, all unused after the restyle.

## Departures from the Figma file

- **The Server block is gone, its state isn't** (John, q-001). State=Review
  draws no server: serving, the panel matches the component exactly and Web
  Browser ⌘4's tooltip is the URL. While the slot is pending-load or
  loading, the Web Browser row is inert, its label muted, and a muted
  "loading…" sits where ⌘4 would; its tooltip carries the sub-state, its
  sentence and the health check (and the URL). John also asked for a red
  "failed" when the health check fails, but nothing reports a failed check
  today (`HealthCheck` is NotDeclared / Waiting / Passed, `ReviewState` is
  Pending / Loading / Serving): a failing server simply stays "loading…".
  The sidebar row's `machine:port` location is no longer shown anywhere on
  the desktop.
- **The selection pill.** With the panel focused (the harness renders it so),
  the keyboard's row wears the accent pill at its leading edge, as the
  default sidebar's does. The component draws its unfocused state.
- **The status is the column, not the status name.** IN PROGRESS and REVIEW
  come from `Column::in_progress()` / `Column::review()`'s default names, so
  a project with a custom status name in those columns still reads IN
  PROGRESS / REVIEW (the panel has no workflow in hand).
- **The workspace mockup's "Orchestrator ⌘R"** and its unselected first row
  lose to the component's "Developer ⌘1", selected, as the README decides.

## Pixel check

`cargo run --features visual-check -- --visual-figma <dir>` renders the
Review and Workspace frames; the panel column (280 x 972 below the 52pt
titlebar, the same pixels in both) was compared against the references with
`magick compare -metric AE -fuzz 10%`:

| frame | differing pixels |
|---|---|
| review-dark | 764 (0.28%) |
| review-light | 911 (0.33%) |
| workspace-dark | 861 (0.32%) |
| workspace-light | 1074 (0.39%) |

Every row lands on the reference's y (Reviewer at 329, Developer at 345,
Actions at 539, the buttons at 560 to 588, the Zen pill at 984 to 1016). What
differs is glyph rasterization, the description's fourth line breaking one
word earlier ("Don't…" vs "Don't stop until…", the bundled Geist being
slightly wider than the one the frames were drawn with,
`dt-figma-qa-activity`), the selection pill above, and in the Workspace
frames the mockup's "Orchestrator ⌘R" row.

Out of this task's scope, noted: the main area starts at x = 285 in every
render (the column's 5pt resize handle) where the frames start it at 281, so
whole-frame diffs are shifted by 4pt; and the terminal residuals of
`dt-figma-qa-terminal-screens` (the composer's rules) show in the Review
frame's main area.

Side-by-sides (reference left, render right) and the renders are in
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-task-panels/`,
with `review-states-dark-light.png`: the loading slot's Web Browser row, Less,
merging (light), and the pending placeholder. `--visual-review` and
`--visual-workspace` pass all their checks.

## Verification

- `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings` (and
  with `--features visual-check`), `cargo test`: 491 pass, rebased on
  `7a60ae73` (the numbers above are from that base too). New tests: the
  description's five-line clamp and More / Less, ⌘N in both panels, the Web
  Browser row waiting for the server, the review's shell (ensure, split,
  ⌘5, closed on leave), and the window keeping the sidebar's chrome around a
  panel while ⌘N goes to it (`app_column_tests.rs`).
- Root: `cargo clippy -p shelbi-app -p shelbi-tui --all-targets -- -D
  warnings`, `cargo test -p shelbi-app` (with the new shell-row test and
  `parity_doc`), `cargo test -p shelbi-tui review`, `cargo check --workspace
  --locked` (the root `Cargo.lock` is unchanged). The TUI's panels render as
  before: it never asks for the shell row.
- `desktop/docs/parity.md` describes the new panels (the workspace and
  review sections, the `WorkspaceDetailsInfo`, `WorkspaceHeader`,
  `ReviewRow` and `ReviewPanelModel` rows).
