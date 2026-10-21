# dt-builder-handle-hit: easy-to-grab connection handles in the Builder

## The bug

The workflow editor's Builder shows four connection handles (dots on the
middles of a node's sides) on the hovered node; dragging one draws a
transition. They were hard to grab, and often could not be dragged at all.

## Root cause (confirmed)

The handles only exist on the hovered node, and the hover was
`Graph::node_at(p)`: the pointer strictly inside the node's rect. Each dot is
centered on the node's edge, so half of it, and most of its 9pt hit area, lay
outside the rect. Moving the pointer onto the outer half of a dot cleared the
hover, the handles disappeared, and `press` (which checks `handle_at` only for
the hovered node) never saw the handle. The press fell through to edge
selection (a transition leaving that side starts at the same midpoint, so its
line ran right under the dot) or to panning.

Reproduced first in `a_handle_stays_hovered_past_the_nodes_edge_and_beats_the_edge_there`
(the handles were gone 4pt past In Progress's right edge: "still hovered past
the node's edge") and `a_handles_hit_area_is_large_at_every_zoom` (a press 11pt
out from the bottom dot at 50% drew nothing), both failing on `main`.

## The change

- **Hover model** (`Graph::hover_at`, used by `BuilderState::hover_to`): the
  node under the pointer, else the node already hovered while the pointer is
  within 6pt (on screen) of its rect or within its handles' hit areas plus
  the same 6pt of slack. Moving from a node onto any part of a dot, and a
  little beyond, never drops the hover. A node is only *kept* this way:
  arriving at a dot from outside without crossing the node doesn't hover it,
  so a transition's line ending there stays clickable.
- **Hit area**: 12pt (on screen) around the dot's center, a 24pt target,
  Apple's minimum for a pointer. It is in screen points at every zoom, so it
  never shrinks when zoomed out. `handle_at` now picks the nearest side when
  two targets overlap (a small node at a low zoom) rather than the first
  listed. The dot itself is still Figma's 10pt.
- **Priority**: `press` refreshes the hover at the press point, then checks
  the hovered node's handles before nodes, edges and panning, so a handle
  wins over the transition leaving from under it.
- **Feedback**: each handle is a 24pt target (`builder-handle-<id>-<side>`)
  that sets the crosshair cursor, holding the dot. The handle under the
  pointer (`BuilderState::hot`), and the one held while drawing from it, is
  drawn hot: 14pt, edged in the accent (`focus_ring`) over the accent at 35%,
  the drawing line's color (`builder-handle-hot-<id>-<side>`).

## Decisions

- **Crosshair** over the grab hand: the gesture draws a line, and the grab
  hand already means "reorder" in the inspector's action rows.
- **The held handle stays hot** while drawing, so the line visibly leaves
  from it. `settings-workflow-builder-drawing` now shows In Progress's bottom
  handle in the accent, which frame 5 doesn't draw; every other Builder
  render is unchanged.
- **The target is a square** for the cursor (a gpui hitbox ignores rounded
  corners) while the press hit-test is the 12pt circle; the corners differ
  by under 4pt.
- **Slack is 6pt** on screen: enough to absorb a pointer overshooting the hit
  area's edge, small enough that the hover never reaches another node (rows
  are 88pt apart; at the 25% minimum zoom the reach is 72 graph points).

## Code

- `views/workflow_graph.rs`: `HANDLE_HIT` 9 → 12, `HANDLE_HOT_D`,
  `HOVER_SLACK`, `Rect::grown`, `Graph::hover_at`, nearest-side `handle_at`.
- `views/workflow_builder_state.rs`: `BuilderState::hot`, `hover_to`,
  `unhover`, `hot_handle`.
- `views/workflow_builder.rs`: `pointer_moved` and `press` use `hover_to`;
  the canvas's leave uses `unhover`; `handle_element` draws the target, the
  crosshair and the hot dot.
- `visual_figma.rs`: `state-workflow-builder-handle-hot`.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --all-targets --features visual-check -- -D warnings`:
  clean.
- `cargo test` (the full suite): 683 passed, 0 failed.
- New graph tests: `a_handle_is_a_24pt_target_at_every_zoom` (11pt out hits,
  8pt in hits, 13pt out misses at 25/50/100/200%; the nearest side on a small
  node), `a_hovered_node_stays_hovered_over_its_handles` (1 to 17pt past the
  edge at 50/100/200%, gone at 30pt, not gained from outside, the node under
  the pointer wins, and the handle and the edge both hit at the same point).
- New state test:
  `hovering_keeps_the_node_over_its_handles_and_heats_the_one_under_the_pointer`.
- New app tests (the real window): the two above, the first also walking
  every side from 4pt inside to 11pt outside without the handles flickering,
  and the second pressing the bottom, right and top handles 11pt out at 50%,
  100% and 200%.
- `--visual-figma`: `state-workflow-builder-handle-hot` and
  `settings-workflow-builder-drawing` checked by eye (dark).
