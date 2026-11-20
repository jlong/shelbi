# pane-responsive-props

The task pane's properties sat in a stack under the title at every width.
Dragged wide, the pane now moves them into a column beside the title and
body, the way Linear's issue view has them. At the default 640pt nothing
changes.

## What landed

- **The breakpoint** (`views/pane_width.rs`, `PaneLayout`). The width the
  pane draws at picks the layout: under 860pt (`SPLIT_W`) the properties stay
  stacked; at it or past it they move to a 280pt column (`SIDE_W`). The title
  and body keep 580pt at the breakpoint, a little under the 600pt they have
  at the default, so the split never makes the body read narrower. The width
  shown decides, not the one chosen: a window that shrinks a wide pane under
  the breakpoint stacks it. `PaneFrame::shown` reads it from the measured
  layer, and the layer's probe now repaints when its size changes, so a
  window resize picks the layout on the next frame.
- **The split layout** (`task_detail::details`). Only the details split. A
  `pane-details` row holds the title, status lines, body and dates in the
  `pane-scroll` (still on `detail_scroll`, so the editors keep their caret in
  view and the keys still page it), and the `pane-side` column, its own
  scroll with a 1pt divider on its left, holding the same property table
  in the same order with the same pickers. Each label sits over its value
  there, both the column's width
  ([`pane-side-props-stacked`](pane-side-props-stacked.md); it first had
  84pt labels beside 144pt values, which cut them off). The
  divider between the properties and the body goes (nothing is over the
  body to set off), and the dates stay under the body in both layouts. The
  Header and the session area under the resize handle stay full width.
- **Pickers and keys.** The pickers are the same `dropdown` on the same
  anchor, so each opens under its value in the column (a menu wider than
  the room right of its value is pulled in from the window's edge, as
  anywhere). Tab order and `pane_focus` are state, not layout: title, Status,
  Workflow, Priority, body, in both.
- **The switch is live.** A drag of the edge re-renders on every move, so
  the layout changes the moment the width crosses the breakpoint, both ways.
  A reader scrolled into the body keeps their line across the switch
  (`task_detail::body_probe`, `pane_width::keep_body_line`): a probe in the
  body records where it starts in the scroll and, when the layout changed
  since the last paint, moves the offset by as much as the body moved.
  Someone who can still see the body's top keeps the offset, so the
  properties come back into view rather than scrolling away above.
- **Everywhere the pane shows**, over Review and Workspace too: it is the
  same `IssuesView` render.

## Tests

- `pane_width::tests`: the breakpoint (the default stacks, one point short
  stacks, the breakpoint splits, a shrunk pane stacks) and `keep_body_line`
  (a reader in the body follows it both ways and within the new scroll; a
  reader over it keeps the offset; junk input).
- `issues::tests::the_default_width_keeps_the_properties_under_the_title`.
- `issues::tests::a_wide_pane_puts_the_properties_beside_the_body_live`: a
  drag one point short of the breakpoint, at it, past it and back, mid-drag;
  every row in the column in order; the dates under the body; a window
  shrinking and growing.
- `issues::tests::the_split_layout_keeps_the_pickers_and_the_keys`: each
  picker under its value, a choice sent, Tab through title, the three
  properties and the body, Shift+Tab back.
- `issues::tests::a_reader_in_the_body_keeps_their_line_as_the_layout_switches`.
- `pane_session::tests::a_wide_pane_splits_only_its_details_over_a_full_width_session`:
  the column stops at the resize handle; the handle, session header and
  session span the pane.
- The visual check's `state-task-pane-wide` shot.
