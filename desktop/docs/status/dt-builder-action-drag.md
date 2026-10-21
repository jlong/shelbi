# dt-builder-action-drag: drag handles on the Builder's action rows

## The change

The workflow editor's Builder listed a selected transition's actions with an
up/down stepper (a ⌃⌄ button whose top half ran the action earlier and bottom
half later), copied from frame 7 (Transition added, 2307:11276). The New
transition panel already drew frame 6's row: the `bars-2` drag handle, the
picker, the trash button. Both panels now use frame 6's row, built once:

```
Actions
Run in this order when a task moves from In Progress to Canceled.
 ═  [Close PR        ▾]  [🗑]
 ═  [Delete branch   ▾]  [🗑]
[+ Add action]
```

- **The row** (frame 6, 2307:11175, from `get_design_context`): a 20pt
  `bars-2` handle in `icon`, 8pt, the action's picker at the Button's Small
  size (28pt, Small medium, 12pt across, a 12pt chevron slot) filling the
  row, 8pt, a 28pt square trash button on the overlay fill (6pt corners). It
  is 28pt tall. The Transition panel's picker was the Base size (32pt) and
  its remove a 24pt × button; both now match New transition.
- **Drag to reorder**: press the handle (grab cursor) and drag. The row
  follows the pointer as a ghost (the popover chip with the handle and the
  action's name), the row under the pointer takes the hover fill, and
  letting go moves the dragged action into that row's place. This is the
  Statuses tables' drag (Settings ▸ Workflows and the Form view): gpui's
  `on_drag` with a ghost view, `drag_over` for the highlight, `on_drop` to
  commit. A drop on its own row, or anywhere that is not a row of the same
  list, changes nothing.
- **Where it lands**: a transition's move is `Change::MoveAction` on the
  draft, the same change the Form view's Run earlier / Run later make, so the
  canvas label and the Form's chips follow at once and Save writes it. A new
  transition's actions are reordered in place until Add transition puts them
  in the draft.
- **Keyboard**: click a handle to focus it (the focus ring, Figma's 1px ring
  and 4px glow, shows on it), then ⌥↑ runs its action earlier and ⌥↓ later.
  The focus follows the action to its new row, so repeated presses keep
  moving it; at either end the key does nothing. A plain arrow is not a
  reorder.
- **The stepper is gone**, with its 18pt `chevron-up-down` drawing
  (`CHEVRON_UP_DOWN_BUTTON`), which nothing else used.

## Decisions

- **Reuse, not a live preview** (q-001, John: option a). The task asked for
  the row to lift while the others make room, "reusing the Statuses tables'
  drag". Those tables show a ghost and highlight the drop row; nothing
  slides. The action rows do the same, so every reorder in the editor works
  one way.
- **Keyboard reach** (agreed with John in the q-001 reply): the app uses no
  gpui tab stops, and the canvas takes Tab for its nodes, so the inspector
  isn't reachable by Tab. A handle is focused by clicking it, then reordered
  from the keyboard. The Form view's chip menus (Run earlier / Run later)
  stay as they were.
- **One focus handle per slot**, created with the editor (six, the number
  of actions there are; a list holds each action at most once). Only one
  action list shows at a time, so the Transition and New transition panels
  share them.
- **One drag type** (`ActionDrag`, carrying its list) replaces
  `NewActionDrag`, so a drag can't drop into another list.
- **Selectors** are shared: `{prefix}-{slot}` (the picker),
  `{prefix}-{slot}-{handle,remove,row}`, `{prefix}-{slot}-{action}` (its menu
  rows), with `builder-new-action` or `builder-action-{index}` as the prefix.
  The New transition rows' old `builder-new-action-handle-N` and
  `builder-new-action-remove-N` are now `builder-new-action-N-handle` and
  `-remove`.

## Code

- `views/workflow_builder_state.rs`: `ActionList` (New, or Transition(n)),
  `action_drop` (a drop's move, or none in place, across lists or for a
  stale row), `action_step` (⌥↑ / ⌥↓), `move_action`.
- `views/workflow_builder.rs`: `action_rows` builds both panels' rows;
  `set_action`, `remove_action`, `move_action` and `actions_of` route an
  edit to the new transition or to the draft; `action_key` takes ⌥↑ / ⌥↓ on
  a focused handle; `ActionDrag` and `ActionGhost`.
- `views/workflow_editor.rs`: `action_focus`, the handles' focus; a
  test-only `builder_graph`.
- `assets.rs`, `assets/icons/heroicons-18/`: the stepper's icon removed.
- `visual_figma.rs`: `state-workflow-builder-action-drag` and
  `state-workflow-builder-action-focused`.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --all-targets --features visual-check -- -D warnings`:
  clean.
- `cargo test` (the full suite): 696 passed, 0 failed (rebased on #1841).
- New state tests (`workflow_builder_state`):
  `an_action_dropped_on_a_row_takes_its_place` (first to last, last to
  first, one later, a drop in place, another list or transition, a stale
  row) and `option_arrows_step_an_action_to_either_end`.
- New app tests (`app_settings_tests`), through the real window with gpui's
  own drag and drop (press the handle, move past the drag threshold, move
  over a row, let go):
  `dragging_an_action_handle_reorders_the_transitions_actions` (no stepper;
  first to last, last to first, a drop in place; the canvas label's lines
  and the Form's chips in the new order; saved to disk),
  `option_arrows_on_a_focused_handle_step_its_action` (a click focuses,
  ⌥↓ ⌥↓ to the end, ⌥↓ at the end does nothing, ⌥↑, a plain ↑ doesn't
  reorder), and `the_new_transition_panel_reorders_its_actions_the_same_way`
  (frame 6's sizes: a 28pt row and picker, 20pt handle, 28pt trash; the
  same drags and ⌥↑; Add transition keeps the order). The existing
  `a_selected_transitions_actions_reorder_change_and_delete` drags where it
  clicked the stepper.
- `--visual-figma` (both themes, checked by eye):
  `settings-workflow-builder-added` (the Transition panel now draws frame 6's
  row; frame 7 still draws the stepper, which this replaces) and
  `settings-workflow-builder-new` (unchanged, frame 6);
  `state-workflow-builder-action-drag` (the ghost over the second row, no
  ring while dragging) and `state-workflow-builder-action-focused` (the
  ring and glow around the handle). The first render of the focused state
  showed the ring as a solid block (a shadow through the translucent
  inspector), which led to the border-drawn ring.
