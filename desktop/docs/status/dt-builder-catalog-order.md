# dt-builder-catalog-order: Builder statuses in the catalog's order

## The problem

The workflow editor's Builder stacked each column in the workflow's own
`statuses:` order. Adding a status to a workflow appends it to that list, so
in a workflow whose list read `… done, review` (John's `cstore` after adding
Review), Review drew under Done, while the `app` workflow drew it above.
Every workflow should lay its statuses out the same way up.

## The rule

John clarified the ordering key mid-task: **category first, then the
project catalog** (`statuses.yaml`, the order Settings ▸ Workflows ▸
Statuses shows and drag-reorders) within a category.

- Left column: Backlog, Ready, Active.
- Right column: Archived, Handoff, Done (archived still stack from the top,
  handoffs and done from the row of the last left status, as before).
- The workflow's own `statuses:` list no longer affects a node's position.
  Column assignment, the archived-at-top rule, row gaps, routing and labels
  are unchanged.

## The change

All in `desktop/src/views/workflow_graph.rs` and its inputs:

- `layout` sorts its nodes by category rank (stable), so a column's order
  within a category is the order it is given. It owns the category rule, so
  any caller (the tests' `task()`, the bench's big fixture) gets it.
- `Graph::of(workflow, catalog)` now takes the project's catalog and hands
  `layout` the workflow's statuses in catalog order. A status the catalog
  has lost (not expected, the model validates) goes after the rest of its
  category, in workflow order. The callers pass `EditorState::catalog()`,
  the draft's `statuses.yaml` (a new one-line accessor).
- `Graph.nodes` is now in display order (category, then catalog) rather
  than workflow order. Nothing indexes it by position.

## Decisions

- **Status-order (dashed) edges follow the same order.** They join each
  non-terminal status to the next in (category, catalog) order, skipping a
  pair a transition already joins. They used to join consecutive statuses
  in the workflow's list, skipping any pair with a terminal status. With the
  new sort, Archived always falls between the left column and the handoffs,
  so the old rule would have dropped every Active → Handoff dash wherever a
  workflow has an archived status. Filtering terminal statuses out first
  keeps that dash. For the default `task` and `subtask` workflows the
  dashes are identical to before.
- **The each-way parallel offset** (two transitions between the same pair,
  one each way) picks its side by the nodes' order, now the display order.
  It only decides which of the two lines sits on which side.
- **Reordering the catalog re-lays the Builder** with no extra plumbing: the
  editor opens over the Workflows page's model (`Settings::open_editor`),
  and the graph is computed from the draft on every frame. Statuses can't be
  reordered while the editor is open (Manage project statuses leaves it).
- **Tab order** in the canvas (`BuilderState::tab`) still follows the
  workflow's list; the task kept to the layout and its inputs. The arrow
  keys move by geometry, so they follow the new layout.
- The model and the files are untouched: display only.

## Tests

- `workflow_graph::tests::the_workflows_own_list_order_moves_nothing`: Task
  loaded from the fixture equals the frame's `task()` layout, and so does
  Task with its list reversed (Done before Review).
- `categories_rank_before_the_catalogs_order`: the catalog reversed still
  draws Task as the frame does.
- `the_catalog_orders_statuses_within_a_category`: a second handoff status
  (QA) after Review, then moved above it in the catalog, then lost from it;
  node positions and the dashes for each.
- `app::settings_tests::the_builder_lays_statuses_out_in_the_catalogs_order`:
  through the real UI, Review made a Done status on the Workflows page draws
  above Done; dragging Done above Review in the catalog re-lays the Builder;
  moving a status in the workflow's own list (the Form's reorder) moves
  nothing.
- The `--visual-figma` renders are unchanged: all 165 frames (both themes,
  every Builder state including `big`) are byte-identical to the branch's
  base (f712b3f5) rendered the same way.

## Verification

`cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
cargo clippy --all-targets --features visual-check -- -D warnings && cargo
test`: clean, 691 tests pass, on the branch rebased onto `main`.
