# dt-sidebar-toggle-animation: the sidebar slides out and back in

## What it was

`ShelbiWindow::toggle_sidebar` flipped `collapsed` and repainted: the
leading column vanished in one frame, the main area jumped to the full
width, and its collapsed titlebar row (room for the traffic lights and the
Show sidebar button) appeared at once, pushing the main area's content down
52pt in the same frame. Showing it was the same jump in reverse.

## What changed

- **A clock-injected reveal** (`views/sidebar_reveal.rs`, `SidebarReveal`),
  the same shape as the column's push (`column_swap.rs`): a target (shown
  or hidden) and the transition under way. `toggle_sidebar` flips
  `collapsed` as before and points the reveal at the new state; the window
  asks it where the sidebar stands each frame and keeps requesting frames
  while it moves. It runs 250ms on Taskpage's `ease_out_expo`
  (`theme/motion.rs`), which is fast off the mark and lands softly, like
  Finder's sidebar. The nav spring was the other candidate; its overshoot
  would push the column past zero width and the main area past the window's
  edge, so the curve it is.
- **Turning around**: a toggle mid-way starts from where the sidebar stands
  and takes only the share of 250ms left to cover, so the position never
  jumps (`a_second_toggle_mid_way_reverses_from_where_it_stands`,
  `toggling_back_and_forth_never_jumps`).
- **Pacing**: like the column's push, a hide or show from rest has its clock
  pinned to its first frames (one 60Hz step in on the first, at most one
  more on the second), so a slow first layout of the main area at its new
  width never skips the curve's fast opening.
- **Clipped, never reflowed** (`sidebar_reveal::place`). The column's width
  in the layout goes from the sidebar's width to 0, and everything it holds
  is laid out at the full width in one slider and moved left, so its right
  edge rides the column's. The slider wraps whatever the column holds (the
  sidebar, a task panel, the Settings sidebar from `dt-settings-sections`),
  so they all slide the same way. The Settings sidebar hides with the app's,
  like the sidebar layer, once the reveal reaches 0. The main area is a flex sibling, so its leading edge follows and
  it grows into the room.
- **The collapsed titlebar row opens with it**: its height follows the
  slide (0 to 52pt) instead of appearing at the end, so the main area's
  content eases down rather than jumping. The Show sidebar button is placed
  in window terms (beside the traffic lights, centered on them, where it has
  always rested) rather than in the row, so it never moves: the main area's
  edge slides past it, uncovering it on a hide and covering it on a show,
  and it fades with the row. Both buttons stay clickable mid-way, and either
  one turns the motion around.
- **Terminals resize once** (`elements/terminal`, `ResizeHold`). A global
  holds the windows whose layout is in motion. While its window is held, a
  terminal pane draws at each frame's size (the session's grid clipped or
  letterboxed, as during any resize) but keeps only the latest size and
  sends nothing to the PTY. On the first frame after the hold lifts it
  reports the size it landed at at once, with no 60ms debounce. The window
  sets the hold from the reveal every frame. A real window-edge drag is
  unchanged (still debounced).
- **Reduce Motion** makes it instant: the reveal lands with no transition
  and nothing is held.
- **Unchanged**: `collapsed` still flips at the toggle, so the saved window
  state, the menus' title, and focus (collapsing moves it to the main area)
  behave as before. A window that opens collapsed starts at rest. Any change
  to `collapsed` other than a toggle snaps the reveal to it with no motion.
  The collapsed task-panel column (a panel keeps the column while the
  sidebar is hidden) still never draws the sidebar. Toggling there only
  moves the column's width between the panel's collapsed width and the
  user's, with no titlebar row. The resize drag and the launch restore are
  untouched.

## Verification

The screen was locked for this task (`CGSSessionScreenIsLocked`), so the
motion was not watched in the real app (`cargo run`). Instead:

- `cargo build`, `cargo clippy --all-targets -- -D warnings` (with and
  without `--features visual-check`), `cargo test` in `desktop/`: 547 tests
  pass on the rebased branch.
- Unit tests (`views/sidebar_reveal.rs`): rest either way, hide and show
  settle, reverse mid-way from where it stands, back-and-forth never jumps,
  Reduce Motion lands at once (including turned on mid-way), first-frame
  pacing, no pacing on a turn, and the placement (full-width content slid
  left; a panel column only changing width).
- Window tests (`app_column_tests.rs`), through the real button and the
  injected clock: the column narrows while `sidebar` keeps its full width
  with its right edge on the column's, the Show button sits at its rest
  spot from the first frame, the saved state and focus change at once, the
  resize hold is on while it moves and off once it settles, and showing
  plays the reverse. Also: a second toggle mid-way draws the same width on
  the turn and then heads back, Reduce Motion is instant, a window opening
  collapsed doesn't animate, and the collapsed panel column widens to the
  user's width and back.
- Terminal test (`session_view.rs`,
  `a_held_layout_resizes_the_pty_once_when_it_settles`): five window sizes
  drawn under a hold (100ms apart, past the debounce) send no resize.
  Releasing it sends exactly one, the size the pane landed at, and nothing
  after.
- `--visual-review` gains frames at a few timestamps (`sidebar-hide-mid`,
  `sidebar-hidden`, `sidebar-show-mid`, `sidebar-reverse-mid`,
  `sidebar-shown`, both themes) with checks that the motion is part way,
  settles, and turns around. In the mid frames the sidebar is slid left at
  its full width (rows cut at the column's edge, not wrapped), the Show
  button is fixed at x 94, uncovered once the main area's edge passes it.
- `--visual-figma` rendered at the base and with the change: 104 of 106
  frames are byte-identical, so the column's new slider wrapper and the
  repositioned Show button change nothing at rest. The two that differ are
  `state-sidebar-hidden` (both themes), and only in the orchestrator
  terminal: its fake session redraws the mockup at whatever size it is told.
  At the base, the 52pt-shorter grid's resize waited on the 60ms debounce
  timer, and gpui timers don't fire offscreen, so the session was never
  told. Now the size is reported as the hold lifts, so the screen is redrawn
  three rows shorter. The live app at the base reaches that same frame 60ms
  later. The Show button sits on the same pixels in both.
- Rebased onto `dt-settings-sections` (#1813). Its
  `toggle_sidebar_collapses_the_settings_sidebar` test now lets the motion
  settle (the `settle` helper moves past the longer of the push and the
  reveal) and checks the Settings sidebar mid-slide: clipped at its full
  width and slid left. 547 tests pass after the rebase, and
  `--visual-review` passes again.
- `app_sidebar_tests`' titlebar-buttons test now runs under Reduce Motion
  (it asserts the end states right after a click). The motion itself is
  covered in `app_column_tests`.
