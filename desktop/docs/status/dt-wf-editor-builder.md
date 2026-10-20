# dt-wf-editor-builder: the workflow editor's Builder

Last of the four Workflows-and-Statuses tasks (`dt-wf-config-model`,
`dt-wf-settings-index`, `dt-wf-editor-form`, `dt-wf-editor-builder`). It
replaces the editor's Builder placeholder with the Workflow Builder frames:
2305:10965 (`workflows/2-builder.png`, nothing selected), 2303:5936 (`3-…`, a
status selected), 2307:10112 (`5-…`, drawing a transition), 2307:10655
(`6-…`, a new transition) and 2307:11276 (`7-…`, a transition just added).
The Builder edits the Form's draft (`EditorState`) through the same `Change`s,
so Save, Discard and the leave prompt are the Form's, and an edit in either
view shows in the other.

## The change

**Where it lives.** `views/workflow_graph.rs` (toolkit-free): the layout,
edge routing, label placement, hit-testing and the canvas's `Viewport`.
`views/workflow_builder_state.rs` (toolkit-free): the selection, the
drawing state machine, the keyboard's node and the hover. `views/workflow_builder.rs`:
the gpui view, as `impl WorkflowEditor` methods (it shares the editor's
draft, menus, tags fields and `change`). The editor now opens on Builder,
the frames' first segment; the Form's tests open it and click Form.

**Toolbar**: Add status (the catalog's statuses the workflow lacks; the new
one is selected), Add transition (draws one from the selected status, else
the keyboard's, else the first), the hint, Fit and the zoom picker (50% to
200%; the zoom runs 25% to 200%).

**Canvas**: one graph frame, centered across the canvas and flush with its
top at 100% (as the frames place it), panned and zoomed. Nodes are the
Workflow Graph Node: the status icon and name, `Owner · agent`, Initial on
the initial status, the focus ring on the selected status, a drawing's two
ends, a transition's source, and the keyboard's node. Edges are painted
paths: transitions solid, status order dashed, 12pt bends and an open
arrowhead, `theme.gray` at 60%; the selected transition, the drawing and a
new transition in the accent (`focus_ring`) with their labels in `link`.
Labels sit on the canvas fill. The legend is under the graph, the hint over
it while nothing is selected (and "Choose a destination…" while drawing).

**Input**: scroll pans, ⌘-scroll and pinch zoom about the pointer, a drag on
the background pans and a click there deselects. Hovering a node shows its
four handles; pressing one starts a drawing, the line following the pointer
(an L along the handle's axis), snapping to the route a transition would
take once it's over another node. Letting go over a node opens New
transition; anywhere else keeps the drawing until a node is clicked. A click
on a node selects it (or, drawing, picks it); a click on an edge or its
label selects its transition. With the canvas focused (any press on it
focuses it): Tab and shift-Tab step through the statuses in order, the arrow
keys go to the nearest node that way, Enter selects (or picks the
destination while drawing), Delete or Backspace removes the selected status
(not the last one) or transition, and Esc steps back: a drawing or a new
transition to its status, a selection to nothing, then the editor's Back.

**Inspector** (320pt, from the frames):
- *Status*: the Status picker, Category, Owner, Agent, Workspace tags (the
  Form row's own field, so typing shows in both), Initial status with Set as
  initial (disabled on the initial one), Outgoing transitions (`Done · Push
  branch, Merge`, each selecting its transition) with Add transition, and
  Remove from workflow (disabled for the only status).
- *Draw transition*: From (changes the source), the hint, Destination (the
  node under the pointer, or "None yet"), Cancel connection.
- *New transition*: From, To, Actions (drag handle to reorder, a picker to
  swap, delete) with Add action, Cancel and Add transition. A refused
  transition (a duplicate) shows the model's message above the buttons.
- *Transition*: From, To, Actions (a stepper whose top half runs the action
  earlier and bottom half later, a picker, remove), Add action, the
  "Transition added…" note right after adding, and Delete transition.

## Decisions (the frames against the real config)

- **The Status picker navigates** (the answer to q-001): it lists the
  workflow's statuses and moves the selection; nothing is edited. A swap
  (replacing the status with another catalog status, retargeting its
  transitions) would need a model op that doesn't exist; say if that was the
  intent.
- **Automatic layout, no stored positions.** The workflow YAML is user
  config shared with the CLI and the TUI, and nothing else holds per-workflow
  UI state, so there is no sensible place to keep dragged positions. Nodes
  aren't draggable; the layout follows the frame: backlog, ready and active
  statuses down the left in workflow order; archived from the top of the
  right column; handoff and done from the row of the last left status. A
  row with a labeled transition straight down to the next gets the 120pt gap
  (Review to Done), others 88pt.
- **Routing**: a straight run when two nodes share a row or column and
  nothing is in between, else the first of L (vertical then horizontal), L
  (horizontal then vertical), Z through the column gap, Z through the row
  gap that crosses no other node, else a bracket around the side. This
  reproduces all four of the frame's routes and its new In Progress → Done.
  Transitions one each way between two statuses run 6pt apart, and routes
  sharing a lane (several Zs through one gap) spread up to 10pt apart.
- **Status order edges** join consecutive statuses where neither is
  terminal and no transition already does (the frame's Backlog → To Do →
  In Progress, and none after).
- **Labels** go over a route's longest segment: one line joined with ` · `
  above a horizontal run, an action a line beside a vertical one. The frame
  nudges In Progress → Canceled's label 36pt above its run's middle; the
  rule keeps it centered.
- **Dropping a drawing opens New transition** rather than adding at once:
  frame 6 is the step between the drop and frame 7, and its Add transition
  is what puts it in the draft (still unsaved; Save writes it).
- **Self-transitions**: dropping on the source isn't a destination; the
  model allows one (From = To from the pickers), drawn as a loop.
- **Names as files and agents are**, as in the Form (`task`, `developer`,
  `review`), where the frames title-case them.
- **Hint and legend**: the legend shows in every state (frames 3 and 6 drop
  it, 2, 5 and 7 draw it); the hint shows as frames 2 and 5 draw it.
- **Nothing animates.** Pan, zoom and Fit move the canvas at once, so Reduce
  Motion has nothing to change. The buttons' hover fades are the shared
  `fill_layer`, which gpui already settles under Reduce Motion.
- Colors: Figma's `theme.muted` is `form_hint`, `theme.divider` is
  `divider`, `theme.search` is `search` (nodes and the inspector), the edges'
  `theme.gray` at 60% is the `icon` role (gray at 90%) at two thirds,
  `icon/cyan` / `focus/default/ring` is `focus_ring`, `text/link` is `link`.
  Light uses the same roles.

## Assets

`heroicons-18/x-mark.svg` (`X_MARK_BUTTON`: the inspector's close, an
action's remove) and `heroicons-18/chevron-up-down.svg`
(`CHEVRON_UP_DOWN_BUTTON`: an action's stepper), the 24/outline drawings at
the 18pt stroke, per that folder's README.

## figma-diff

`--visual-figma` renders `settings-workflow-builder{,-status,-drawing,-new,-added}`
in dark and light (1440x1024, the frames' size) over the Workflows fixture's
Task, plus `state-workflow-builder-fit`, `-zoomed` (50%, the keyboard on
Review) and `-big` (the 20-status, 40-transition workflow, fitted), checked
by eye. The references, copied as `<screen>-dark.png`:

| Screen | First render | After |
|---|---|---|
| builder (2) | 1.09% | 1.02% |
| builder-status (3) | 1.45% | 1.39% |
| builder-drawing (5) | 1.41% | 1.24% |
| builder-new (6) | 1.33% | 1.22% |
| builder-added (7) | 1.39% | 1.27% |

Fixed between them: quiet text in `muted` instead of `form_hint`, the edges
at full `icon` gray, the transition inspectors' headings centered (the frames
set them at the top), and the legend's dashes. What's left: Figma's Geist
v1.2 sets text wider than the bundled Geist (the toolbar, every label), the
frames' header dirty (Discard and an enabled Save) where the renders are
clean, the lowercase title and agent names, the archived glyph (a slashed
circle where the frames draw an x), the In Progress → Canceled label's
nudge, the legend in frames 3 and 6, and the drawn and new transitions'
labels (frames 5 to 7), which the frames set 20pt right of their run's
middle.

## Performance

`--bench-builder` (feature `visual-check`) opens the `big` workflow (20
statuses, 40 transitions) in the real window offscreen at 1440x1024 and
times each step's input dispatch plus the whole window's render, layout and
paint. Release build, on John's laptop:

| Sweep | p50 ms | p90 ms | p99 ms | max ms | > 16.7 ms |
|---|---|---|---|---|---|
| idle | 2.59 | 4.39 | 7.09 | 8.13 | 0 of 120 |
| pan (scroll) | 5.96 | 10.22 | 15.53 | 15.61 | 0 |
| zoom (⌘-scroll) | 7.34 | 12.68 | 19.03 | 19.20 | 5 |
| hover | 2.39 | 5.73 | 16.61 | 18.42 | 1 |
| draw (a handle dragged) | 2.78 | 5.55 | 10.20 | 14.82 | 0 |

Measured on a busy hub (load average about 41), so the tails carry other
workers' noise. Zoom is the costly sweep: every step sets the text at a
new size. Zoomed text sizes step by half points so a gesture reuses sizes
gpui has already shaped. Before that change the same sweep had a p99 of
37 ms, 6 frames over 16.7 ms and 101 over 8.3 ms; after it, 44 over
8.3 ms.

The layout is redone each frame (it's under a millisecond: the unit test
holds it to 4ms in a debug build), and a gpui test draws the big workflow
in the debug build under a loose 250ms a frame bound, to catch a layout gone
quadratic.

## Tests

- `views::workflow_graph::tests`: Task laid out at the frame's points and
  frame size, the status-order rule, all four routes plus the new L, labels
  over the longest segment, a blocked straight run bracketing, transitions
  each way apart, lanes spreading, hit-testing nodes, handles (reach on
  screen) and edges or labels, arrow-key neighbors, the viewport's centering,
  zoom about the pointer, fit and pan clamp, and the 20/40 layout's time.
- `views::workflow_builder_state::tests`: the drawing state machine (handle,
  over nothing, over its own node, over a destination, release, waiting for
  a click, Esc stepping back), Add transition with a changed source and
  adding selecting the new transition, reconcile after the draft loses
  things, and the keyboard (Tab, arrows, Enter, Delete's target).
- `views::workflow_editor_state::tests`: unchanged, plus `InsertTransition`
  through the model.
- `app::settings_tests`: the Builder drawing Task as the frame does; a status
  edited from the inspector with the Form seeing it (and the reverse), the
  Status picker, Remove from workflow, Add status, Close, and Save writing
  it; a handle dragged onto Done through New transition and Add transition
  to the draft and disk; Add transition then a click, Cancel, a duplicate
  refused in the panel, and Esc stepping back; a selected transition's
  stepper, picker, Add action, remove, a refused To, and Delete transition;
  the keyboard (Tab, arrows, Enter, Backspace on a status, Delete on a
  transition, Esc to the leave prompt); pan by scroll and by drag, ⌘-scroll
  zoom about the pointer, the zoom picker and Fit; and the 20/40 workflow
  drawn in a frame.

## Verification

`cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
cargo clippy --all-targets --features visual-check -- -D warnings && cargo
test`. No `crates/` changes.
