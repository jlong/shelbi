# dt-sidebar-hover-stuck: one hovered row, no blink

## The bug

Hovering the sidebar sometimes blinked the hover fills and left several rows
lit at once. Switching to another app and back cleared it.

## Root cause (confirmed)

Every hover and press fade (`theme::motion::fill_layer`) kept its state in a
thread-local `POINTERS` map keyed by the control's key (for a sidebar row,
its selector, `sidebar-ws-alpha`), and only the element's gpui `on_hover`
listener wrote it. That listener fires when *its element's* remembered hover
(`hover_listener_state`, in gpui's element state, keyed by the element id)
changes. Two things broke the link between the two:

1. **The sidebar's rows had positional element ids**
   (`("sidebar-row", index)`). When a row arrived, left or reordered above
   others (a model refresh, a section or machine group folding), each row
   below took over the element state of the row that used to sit at its
   index. Hover `alpha` (index 5), then let a review row arrive above it:
   index 5 is now the new row, which inherits "was hovered" and so never
   reports an enter; `alpha` is at index 6, which inherits "was not
   hovered" and so never reports a leave. `POINTERS["sidebar-ws-alpha"]`
   stays `hovered: true` with the pointer elsewhere. Repeat while moving
   through the list and several rows stay lit.
2. **An element that stops painting never reports a leave.** A row selected
   by a click drops its fill layer (the chip paints it), a folded row stops
   painting, a row scrolled out of a list stops painting. Its entry stays as
   it was, and when the element comes back it renders the stale hover first.

The blink is the same id mismatch seen through the fade: the spring that
animates the fill keeps its state under the element id, so a row arriving
at an index started from whatever value the previous occupant had reached
(a full hover fill on a row the pointer isn't over, fading out; or a dip on
the row that is), and a row remounted with a stale hover started at the
hover fill without fading in.

Reproduced in `theme::motion::pointer_tests` (a test view whose rows are
keyed by name but have positional ids, as the sidebar's did): five of the
six tests fail on the old code (`mv-a moved out from under the pointer`,
`um-b is gone`, `two_rows_are_never_both_hovered` with two keys lit, the
press surviving its row moving away, and the hover coming back after the
pointer left the window).

Why switching apps cleared it was not pinned down: the app-switch path
redraws and moves the pointer over the rows again, which re-runs enter and
leave for the elements involved; the fix doesn't depend on it.

## The change

- **The hit test is the truth, every frame** (`motion::paint_pointer`).
  Each fill layer carries an invisible probe filling its element that
  inserts its own hitbox (normal behavior, so it blocks nothing) and, at
  paint, asks gpui whether that hitbox is under the pointer, exactly as gpui's
  hover listener counts hover (not while dragging, not after a key press, not
  under an occluding overlay). gpui computes that hit test after prepaint, so
  it reflects the frame being painted. The answer overrules the stored hover
  and clears a press the pointer no longer covers. A row that moves out from
  under a still pointer loses its hover the frame it moves; the row that
  moves in gains it.
- **Unpainted keys are forgotten** (`motion::close_frame`). The first fade a
  window paints in a frame opens the frame and defers its close until the
  frame has painted; the close drops every key that window painted before
  but not in this frame (unmounted, folded, selected, scrolled out), and
  redraws if any hit test changed what the frame's render showed (gpui
  ignores `refresh` while drawing). A row that comes back starts at rest and
  fades in if it lands under the pointer.
- **The pointer leaving the window** is tracked per window: gpui keeps its
  last position inside, so the hit test alone would re-light the row at the
  edge on the next redraw. A `MouseExitEvent` marks the window left (no
  element hovered) until the next mouse move.
- **Sidebar rows are keyed by row** (`render_row`): the element id is the
  row's selector (the blanks between sections, which repeat, keep their
  index). A row's fade spring now stays with it as rows move, which is what
  removes the blink.
- The `on_hover`, mouse-down and mouse-up listeners stay: they give the
  immediate response between frames, and the per-frame hit test settles
  them. All `fill_layer` users get the fix (buttons, palette rows, settings
  rows, the builder, the task panels), not only the sidebar.

## Decisions

- **Reconcile in paint over resetting on window (de)activation.** A reset on
  activation only clears the symptom when the user happens to switch apps;
  the hit test keeps the state right while the window stays in front.
- **Two elements sharing a key in one frame are hovered if either is.**
  Letting the last painter win would flip the state every frame and redraw
  forever; sharing a key is a bug in the caller anyway, and its elements
  already read one state.
- **`hold_pointer` entries are held**: the hit test and the sweep skip them,
  so the components sheet's hover and press renders stay as before. They
  clear with `reset_pointers`, which also clears the frame bookkeeping.
- **A press dragged off its control fades back** (it used to stay pressed
  until the next mouse move after the button came up), which matches macOS
  buttons.
- The sweep relies on every painted element running its paint closures each
  frame. The desktop has no `.cached()` views; a cached view's fades would be
  swept (forgotten, so unhovered) on frames that reuse its paint.

## Verified

- `theme::motion::pointer_tests`: a row moving and reordering under a still
  pointer, a row unmounted while hovered and remounted away from the pointer,
  two rows never both hovered across five rows under four orders (rows
  changing under the still pointer after each hover), a press fading in and
  dropping when its row moves away, the pointer leaving the window, and a
  held pointer surviving the hit test and an unmount.
- Desktop `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --features visual-check -- -D warnings` and `cargo test`
  (889 passed).
