# dt-task-pane-resize

The floating task pane had a fixed 640pt width. Its left edge now resizes
it, the way the sidebar's right edge resizes the sidebar.

## What landed

- **The model** (`views/pane_width.rs`, `PaneWidth`). The width is kept as
  the user chose it, 640pt (`DEFAULT_W`) until they drag. A drag moves the
  edge as far as the pointer moves from where it pressed, so a press never
  jumps the edge to the pointer. It is clamped to at least 400pt (`MIN_W`:
  the 108pt property labels, their values and the insets) and at most what
  leaves 240pt (`BOARD_KEEP`) of the main area beside the pane. A saved
  width that is junk (not finite, under the minimum) loads as the default.
- **The handle** (`task_detail::resize_handle`). The sidebar edge's 5pt grab
  room (`app::RESIZE_GRAB`, now shared) and `CursorStyle::ResizeLeftRight`,
  laid over the pane's first points, on top of its content. A press starts
  the drag without moving the keys (an editor keeps its caret). While the
  drag is on, a 1pt rule in `icon` lights the edge between the corners, as
  the sidebar's edge lights.
- **The drag** (`task_detail::resize_probe`). A canvas on the pane's layer
  measures the layer, and while a drag is on registers window-wide mouse
  move and up listeners (the `Slider` pattern), so the drag follows the
  pointer over the board, the sidebar, anywhere, until the button comes up.
  The pane re-renders on each move: text reflows live. The right edge stays
  8pt in from the main area.
- **A narrow window.** The pane asks for the chosen width and the layer's
  flex row shrinks it to fit, as before, so the 8pt insets always hold. The
  chosen width is untouched and comes back when the window grows.
- **Persistence.** The drag's end emits `IssuesEvent::PaneWidth` when the
  width changed; the window stores it in `WindowState::task_pane_width` and
  saves its slot (debounced, as the sidebar width is). A new board entity
  (first use, a project switch) takes the window's width; File ▸ New Window
  copies it from the frontmost window, as it copies the sidebar. Older
  session files without the field load the default.
- **Everywhere the pane shows.** The board's pane and the pane over Review
  and Workspace are the same `IssuesView`, so they share one width.
- **Motion.** The slide-in starts past the chosen width plus the inset, so a
  wide pane still enters from off screen; Reduce Motion shows it in place.

Double-click to reset is not added: the sidebar's edge doesn't do it.

## Tests

- `pane_width::tests`: the default, both directions, both clamps, a narrow
  layer, junk input and saved values.
- `issues::tests`: the handle's place and size; a live drag (mid-drag
  width, fixed right edge, title reflow, nothing saved until release), the
  minimum, the maximum with the pointer far over the board, no jump on a
  press in place; the width across close and reopen and a window that
  shrinks and grows; the slide-in from a chosen width.
- `app::column_tests::the_task_pane_resizes_over_the_review_and_the_workspace_and_the_window_keeps_it`:
  the saved width applied, drags over Review and Workspace, and
  `WindowState` updated.
- `app::window_tests::a_launch_reopens_the_open_windows_and_a_close_forgets_one`:
  a launch slot's width reaches its window and survives in a closed slot.
- `window_state` tests: round trip, default 640, a pre-resize file.
