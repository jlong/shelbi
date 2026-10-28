# tui-workflow-editor-builder: the workflow Builder in the TUI

## The change

The TUI's workflow editor (`tui-workflow-editor-form`) had the Form only. It
now has the desktop's second view too, the **Builder**: the workflow as a
graph of its statuses and transitions with one details panel for what's
selected. Both views edit the one shared draft (`EditorState`), so every
edit in one shows in the other at once, and Save, Discard, the leave prompt
and the guards stay the editor's. Nothing is written before Save, and the
graph's layout is worked out from the draft each time: it never reaches the
workflow's YAML.

## Shared (`shelbi_app::settings`)

The desktop's toolkit-free Builder modules moved here, the desktop keeping
re-exports under their old paths:

- **`workflow_graph`** (was `desktop/src/views/workflow_graph.rs`): where
  each status sits (statuses in flight on the left, the rest on the right;
  down a column by category, then the catalog's order), routes, hit-testing,
  arrow-key neighbors, the viewport, and the 20-status `big_fixture`. Its
  pure layout tests stay in the module.
- **`workflow_builder`** (was `views/workflow_builder_state.rs`): the
  selection (a status, a transition, a transition being drawn, a new one),
  Back to the status a transition panel came from, hover and hot handles,
  the keyboard's node, and the action rows' reordering.

Added on the way, toolkit-free and covered in
`crates/shelbi-app/tests/settings_workflow_builder.rs` (which also took over
the desktop's state tests and the graph's fixture-loading tests unchanged):

- **The keyboard's transition** (`focus_transition`,
  `step_transition`): `]` / `[` step through the transitions in the
  workflow's order, from the focused status's first leaving one (else
  arriving one); its source becomes the keyboard's node so the arrows carry
  on from there; Enter selects it as a click on its edge does; it's ringed
  in place of its source. The desktop doesn't wire a key to it yet.
- **`aim`**: point a drawing at a node by id (the node under the pointer,
  or the keyboard's node as the arrows move it). `draw_to` goes through it.

## The TUI's Builder

`crates/shelbi-tui/src/shell/settings/workflow_editor/builder.rs` (keys,
panel, drawing) and `canvas.rs` (cells, routing, painting).

**Switching views.** `v` (both views) or the header's `Builder | Form`
control (Enter on it, or a click on either half). The place carries across:
the Form's status or transition row becomes the Builder's selection, and the
Builder's selection the Form's row (a transition's Details stay open). The
editor **opens on the Form** (one line, `workflow_editor::OPENS_ON`).

**Layout** at 80x24 (the panel replaces the canvas below 104 columns):

```
 ← Back   task default  Builder  Form    Save changes   Discard   ⋮ Actions

 ←↑↓→ a status, [ ] a transition, ⏎ to open it; a adds a transition.…

   ╭─ ◌ Backlog ──────────╮              ╭─ ⊘ Canceled ─────────╮
   │ User · orchestrator  │             ▶│ User · None          │
   ╰─ Initial ┬───────────╯             │╰──────────────────────╯
              ┆                         │            ▲
              ▼                         │            │
   ╭─ ○ To Do ────────────╮             │            │
   │ Agent · orchestrator │             │            │ Close PR · Delete…
   ╰────────────────┬─────╯             │            │
                    ┆   ╭───────────────╯            │
                    ▼   │                            │
   ╭─ ◐ In Progress ────┴─╮ Push branc…  ╭─ ◑ Review ┴──────────╮
   │ Agent · developer    ├─────────────▶│ User · review        │
   ╰──────────────────────╯              ╰─────────┬────────────╯
                                                   │ Push branch · Merge
                                                   ▼
                                         ╭─ ● Done ─────────────╮   → ↓ more
 ←↑↓→/[ ] move · ⏎ open · a transition · n status · v form · Tab · Esc back
```

Each status is a box: its category's symbol (`◌` backlog, `○` ready, `◐`
active, `◑` handoff, `●` done, `⊘` archived, colored as the board colors
them) and name in the top border, `Owner · agent` inside, `Initial` in the
bottom border of the initial status. Transitions are solid lines ending in
an arrowhead, their actions over the line's longest run where there's room
(shortened to fit, left off where they can't be); the status order is
dotted. The selected status is double-ruled, the keyboard's node heavy with
its four `◆` handles, a drawing's ends yellow, the selected transition
cyan, the keyboard's transition bold.

**Lines** are routed cell by cell (an A* search over the canvas): each
leaves its source from a free port (a tee in the border) and ends on a free
port of its destination; ports are never shared, nodes never crossed, and a
line pays to turn, more to cross another, and much more to run along one.
So a status to itself, two transitions each way between a pair, cycles and
statuses no transition touches all draw, each line its own, the same way
every time (transitions in the workflow's order, then the dots). The
canvas is laid out again only when its input (names, assignments, edges)
changes.

**Keys** (the canvas focused):

| Key | Does |
|---|---|
| ←↑↓→ | move the keyboard's node (the shared nearest-node rule) |
| `]` / `[` | the next / previous transition |
| Enter | select it: the panel opens and takes the keys |
| `a` | Add transition from the keyboard's node |
| `n` | Add status (the catalog's statuses the workflow lacks) |
| `x` / Delete | remove the selection (else the keyboard's transition, else node) |
| Shift+arrows, Home | scroll the canvas, back to its corner |
| Tab / Shift+Tab | header, canvas, panel |
| Esc | step back: a drawing, the selection, then out of the editor |
| `v`, `m`, `y`, Ctrl+S | Form, ⋮ Actions, Edit YAML, Save |

The canvas scrolls to keep the keyboard's node (or transition) in view, and
`→ ↓ more` says when it's clipped.

**The panel** works as the Form's rows (the same pickers, text fields and
keys: Enter, `x`, `n`, Shift/Alt+↑↓ to reorder), and scrolls by the Form's
rule: the focused row keeps a line under it, so "↓ more" never covers it:

- **Status**: the status picker (it moves the selection), its category,
  Owner, Agent, Workspace tags, Set as initial, its outgoing transitions
  (each opens its transition, ← back to the status), Add transition, Remove
  from workflow (dimmed for the only status).
- **Transition**: From, To, its actions in order, Add action, its Details
  (Target branch, the Run commands, Ready check, Ready timeout, URL), the
  "Transition added" note when it's new, Delete transition.
- **Add transition** (a drawing): From, the destination (or None yet), To
  (a picker: the keyboard-free way to choose), Cancel.
- **New transition**: From, To, its actions, Cancel, Add transition.

Narrow, the panel takes the canvas's place with **← Graph** (and Esc) back
to the canvas, the selection kept; Enter opens it again. A resize keeps the
selection and the cursor's row either way.

**Add transition**: choose the source (`a` on the keyboard's node, the
Status panel's Add transition, or a handle), the destination (arrows, which
preview the line, then Enter; a click; or the panel's To), configure its
actions, then Add transition. Esc or Cancel at any step drops it and
nothing reaches the draft; from a status's panel it goes back to that
status, otherwise to the status it was drawn from (the desktop's rule).
Add transition goes through `Change::InsertTransition`, the draft's check
(a duplicate pair is refused with the Form's message, which shows in the
panel; the transition stays new).

**Mouse** (where the terminal reports it): a click selects a node or a
line, hovering a node shows its handles (the one under the pointer hot), a
press on a handle starts a drawing and a drag drops it on a status; letting
go in place waits for a click on the destination, so no drag is ever
needed. The wheel scrolls (Shift for across).

**Empty and broken states**: a workflow with no statuses says so and points
at `n` and the Form; a transition the canvas can't find room for is counted
over the canvas ("the Form lists every one"). Loading and load errors are
the editor's.

## Intentional differences from the desktop

- **The TUI opens on the Form** (the desktop on the Builder): in a terminal
  the rows are the more usable start (q-001). `OPENS_ON` flips it.
- **Cells, not points**: no zoom or Fit; Shift+arrows, Home and the wheel
  scroll instead. Positions come from the shared layout, the cell sizes and
  routes are the TUI's.
- **Keyboard edges**: `]` / `[` reach transitions directly (the desktop
  reaches them from a status's Outgoing transitions or the canvas).
- **Tab moves between sections** (header, canvas, panel), as in the TUI's
  Form; the desktop's Tab walks the nodes (the arrows do that here).
- **Details in the Transition panel**: the TUI's panel also edits a
  transition's Details (the desktop's Builder leaves them to the Form).
- **No action drag**: actions reorder with Shift/Alt+↑↓, as in the Form.
- **The panel replaces the canvas when narrow**; the desktop's inspector
  always sits beside.

## Tests

- `crates/shelbi-app/tests/settings_workflow_builder.rs`: the moved state
  and catalog-order tests, the keyboard's transition, `aim`, reconcile.
- `crates/shelbi-tui/src/shell/settings/workflow_editor/canvas.rs`: lines
  for two-way, doubled and self-loop transitions with no shared port and no
  node crossed, the same draft drawing the same way with a lone status, the
  20-status / 40-transition fixture routed in well under a second.
- `crates/shelbi-tui/src/shell/settings/workflow_editor/tests/builder.rs`:
  `v` and the header control carrying the place; the canvas at 80x24
  (symbols, names, assignments, Initial, every transition routed, labels,
  clipping); a status edited from its panel by keys (owner, agent, tags,
  initial) showing in the Form and saved; `]` to a transition and its panel
  (reorder, add, remove actions, To, a refused then corrected Details
  field) showing in the Form; Add transition by keys end to end; Esc and
  Cancel at every step leaving the draft untouched; a refused duplicate with
  the Form's message; Form edits in the Builder and Builder removals in the
  Form, with Discard restoring both; the leave prompt; a cyclic workflow
  with a self-loop and lone statuses; resize with the panel open (wide,
  narrow, ← Graph, back); scrolling, and a clipped panel keeping a line
  under every focused row; the mouse (select, hover, handles,
  click-click and drag drawing, the wheel); sizes down to 1x1.
- `crates/shelbi-tui/src/shell/mod.rs`, through the shell's real key and
  mouse path: Add transition by keys at 80x24 then Save off the UI thread,
  a mouse selection, light (contrast-checked) and dark renders at 80x24 and
  160x48 with the panel open.
- The desktop's Builder tests and app tests run unchanged over the moved
  modules.
