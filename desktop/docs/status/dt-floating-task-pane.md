# dt-floating-task-pane

The card's detail was a centered modal over a dimmed board (80% x 85% of the
main area). It is now the floating task pane from John's revised Task pane
frames: In progress (2172:722) and Ready to start (2573:10737), both Dark,
reviewed live on 2026-10-28. These frames supersede the centered Issue detail
mockup and the earlier docked-pane draft.

## What landed

- **The shell** (`views/task_detail.rs`). An overlay layer inset 8pt from
  the main area's top, right and bottom holds the pane at the right: 640pt
  wide, or narrower in a window that can't fit it (the layer clamps it, so
  it always keeps its 8pt insets), 1008pt tall at the 1440 x 1024 reference
  (x 792, y 8 in the window). It is the `canvas` with 12pt corners, a 1px
  `divider` edge drawn inside it (Figma strokes inside, so the edge takes no
  layout room and the content sits exactly 20pt in), and the frame's drop
  (0 8 32) in the `shadow` role. It paints from `theme.opaque()` like every
  overlay. There is no scrim, no push, and no column resize.
- **The motion.** The pane slides in from past the window's right edge over
  `column_swap::SLIDE` on `ease_out_expo`, through gpui's `with_animation`,
  which renders the end state under Reduce Motion (`sync_reduce_motion`).
  Switching cards keeps the same element, so only an open slides.
- **The Header** follows the Header pattern: 52pt, `toolbar`, the 1px
  canvas edge on its left, `Workflow / id` in Base semibold loud (the
  workflow capitalized, `task` → `Task`), then 28pt More (ellipsis) and
  Close (x-mark) with 20pt icons. There is no pencil or Edit gate.
- **The content** scrolls under the fixed Header (`pane-scroll`, the view's
  `detail_scroll`): 20pt in, 20pt between sections. The title (Desktop/Title
  24 / 32 semibold, loud, wrapping); the properties 6pt apart, each a 28pt
  row with a 108pt Small `muted` label, 12pt, then the value. Status
  carries its status glyph, Workflow and Priority (`N in queue`) are the
  Button at Small in the Quiet style with the dropdown chevron. Branch is
  the branch in the terminal face at 12 / 18, or "Created when work starts"
  in `muted`. Then a divider, the Markdown body, and `Created Oct 26 ·
  Updated Oct 28` (X-Small, `muted`; a year appears when it isn't this
  one). The Workspace row and the Start footer are gone, as John removed
  them. Owner (remote tracker), Depends on / Blocked by, and Pull request
  rows appear only when the task has them.
- **The body** (`views/markdown.rs`) is restyled to the frames: headings
  below h1 in Base semibold with 12pt under them, 20pt between blocks, and
  task lists as 18pt rows 12pt apart. Each row's box is now the design
  system's Check (`controls::check`: `check_fill` when checked, cyan in
  Dark and pink in Light) instead of the old modal's own loud box. The
  checklist count moved out of the header, so `markdown::checklist` is
  test-only now (it still pins that the shipped templates parse).
- **The old action row moved into More** (`BoardState::pane_actions`, a
  `Mode::PaneMenu`): Move left, Move right, Start (left out for someone
  else's issue on a remote tracker), Assign…, Edit description, Open in
  editor, and Open workspace once the task is assigned. Each row shows its
  key, and the keys still work on the pane (`H` / `L`, `s`, `a`, `e`, `E`,
  `o`). The menu has its own keys (j / k / Enter / Esc).
- **Not modal** (`BoardState::pane`). The pane's task is kept apart from
  which overlay has the keys. A board menu, picker or prompt opened over it
  (a filter chip, the assign picker, New issue) closes back to the pane
  (`rest()`). Only Close, Esc on the pane, Open workspace, or the screen
  going away closes it. A click on another visible card shows that card in
  the one pane. The pane occludes the board under it and nothing else. On
  the board a press in the pane takes the keys back (from the search field,
  say), unless something inside it took them first.
- **Every entry uses this pane**: board cards and Enter, Activity rows and
  notifications (`open_task`), and More from Review and Workspace
  (`open_task_over`). Over those screens the floating view's layer no
  longer tracks focus or swallows clicks, so the screen around the pane
  stays usable. The pane itself is the focus target, and its Close or Esc
  hands the keys back through `sync_detail_over` → `focus_main_view`.
- **Loading, missing, and failed** (the board's read error) states draw in
  the same shell (`PaneContent`), with the Header and Close, never the old
  centered note over a scrim.
- **Esc order.** A menu or picker over the pane takes Esc first. The body
  editor takes it next: Esc leaves the editor and saves a changed draft
  through `EditIssue` instead of dropping it (`IssuesView::leave_editor`),
  and Close does the same. A focused terminal beside the pane keeps its own
  Esc, because the pane doesn't have the keys then. Esc on the pane closes
  it.
- The open card keeps its selection ring while the pane is up, as both
  frames draw it (the modal used to drop it under the scrim).

## Decisions

- The **property values are faces, not controls yet**. Choosing a status,
  workflow, or priority belongs to the property-editor task, so they have
  no hover or press, which keeps them from promising a click that does
  nothing. Moves stay reachable through More and `H` / `L`.
- I **kept the in-place body editor reachable** (More ▸ Edit description,
  `e`) so editing doesn't regress before the WYSIWYG task replaces it. It
  is not a new gate: no pencil, and nothing is read-only until you enter
  it. Its Cancel is now Discard, the one explicit way to drop a draft.
- Priority reads `N in queue`, the frames' copy, from `TaskDetailModel.priority`.
- **The session area** (In progress frame, `dt-task-pane-session`) is not
  here. The scroll is `flex_1` under the Header, so that task adds its
  resize handle and session below it, and an idle task's details fill the
  pane as the Ready to start frame shows.
- No new `CommandKind` or `shelbi_app::view` field, so the parity table only
  changed where the desktop shows each `TaskDetailModel` field
  (`parity.md`). No shared crate changed.
- The old modal's icon sizes were retired with it: the `@close` 16pt class
  (`ICON_SIZE_CLOSE`, `assets/icons/heroicons-16/`) and the 18pt Start /
  Assign / Edit / Editor drawings. `ARROW_LEFT_BUTTON` stays for the
  component sheets.
- The Implementation notes heading in Figma sits 8pt over its paragraph
  where the other two sit 12pt. The pane uses 12pt for every heading.

## Verification

- `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings`
  (also `--features visual-check`), and `cargo test`.
- New tests. `views::issues`: the pane at 512,8 640 x 1008 in a 1160 x 1024
  main area, with the board's bounds, scroll offset, and selection unchanged
  by open and close; a 600 x 500 window narrowing it to 584 x 484 inside its
  insets; the slide in from the right; another card switching the one pane;
  a filter menu over it closing back to it; More's rows and keys; a missing
  task in the shell; Esc in the editor saving the draft. `board_state`: the
  pane through pickers and menus, and More's actions. `app::column_tests`:
  More over Workspace places the pane 8pt in with no scrim, and More over
  Review stays up through a click beside it, then Close hands the keys back
  to the review.
- `--visual-figma` (`SHELBI_VISUAL_ONLY=issues`): `task-pane` and
  `task-pane-idle` against the two frames, exported to
  `design-refs/desktop-figma/task-pane{,-idle}-dark.png`. The pane matches
  to within a pixel. What `figma-diff.sh` still reports is the board and
  sidebar fixture data under it (excerpts, dates, the footer's hint list,
  the sidebar's section order) and the In progress frame's session area.
  Undrawn states, checked by eye in both themes: `state-task-pane-menu`,
  `-switched`, `-scrolled`, `-empty`, and `-missing`.
