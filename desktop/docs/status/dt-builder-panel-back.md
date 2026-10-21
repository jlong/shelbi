# dt-builder-panel-back: a back button in the Builder's transition panels

## The change

The workflow editor's Builder inspector can drill from a Status panel into a
transition (an Outgoing transitions row, or Add transition), but once there
the only exits were × and Esc, both of which dropped the selection. The
transition panels now lead their heading with a back button when they were
reached from a status:

```
┌ [←] Transition                  [×] ┐
│ From  [Review ▾]                    │
│ To    [Done ▾]                      │
│ ...                                 │
```

- **Where it shows**: Transition (opened from an Outgoing transitions row),
  Draw transition and New transition (started by Add transition, the Status
  panel's or the toolbar's with a status selected), and the Transition panel
  a new transition becomes once added. A Status panel never shows it.
- **What it does**: returns to the status the panel came from, selected,
  with the Status panel at the scroll offset it had. A pending drawing or new
  transition is dropped (as Cancel does, its duplicate error cleared with
  it); an added transition stays in the draft. The status gone from the
  draft (removed in the Form) forgets the back target: no ←, and Esc
  deselects.
- **Esc** takes the same path: back to that status first, then to nothing
  selected, then the editor's own Back. Cancel connection and New
  transition's Cancel go the same way.
- **The status it goes back to** is the one whose panel was open, even when
  the drawing's From picker has since moved the source elsewhere.
- **Tooltip** "Back to <status name>", the status's display name.
- **Styling**: `task_panel::back_button`, the app's back button (the task
  panels', Settings'): a 28pt square on the overlay fill deepening on hover
  and press, 6pt corners, the 20pt `arrow-left` in `button_icon`, from the
  same roles in both themes. It sits left of the title, 8pt apart; a heading
  with it is centered on its row (it is 4pt taller than the 24pt close), as
  the Status panel's heading is.

## Decisions

- **A transition picked on the canvas** (its edge or its label) has nothing
  to go back to: no ←, and Esc deselects, as before. Going back to its From
  status was the alternative; it would invent a step the user never took,
  and the From status is one click away on the canvas, ringed.
- **A drawing begun by dragging a node's handle** goes back to the status
  whose panel was open, else to the node it was drawn from. That keeps the
  Builder's existing Esc (a drawing steps back to its status), so the ←
  shows exactly when Esc would return to a status.
- **Keyboard**: the Builder has no Tab traversal through the inspector (the
  canvas takes Tab for its nodes, and nothing else in the app uses gpui's
  tab stops), so the keyboard's way back is Esc, which takes the button's
  path wherever the focus is in the editor. The button itself is
  mouse-only, as the inspector's other buttons are.
- **Scroll**: the inspector now tracks a `ScrollHandle`. Leaving a Status
  panel for a transition panel keeps its offset and starts the new panel at
  the top; going back restores it.

## Code

- `views/workflow_builder_state.rs`: `BuilderState::back`, the status a
  transition panel goes back to, set only while one shows.
  `open_transition` (an Outgoing transitions row) records the selected
  status; `select_transition` (the canvas), `select_status` and `deselect`
  clear it; `begin_draw` records the selected status, else the source;
  `release`, `pick`, `set_from` and `added` keep it; `escape` returns to it,
  else deselects; `reconcile` drops it with the selection or its status.
- `views/workflow_builder.rs`: `open_transition`, `keep_status_scroll` and
  `step_back` (Esc, ←, Cancel), the button in `render_inspector`.
- `views/workflow_editor.rs`: the editor's Back (Esc) steps back through
  `step_back`; `inspector_scroll`, `back_scroll`, and a test-only
  `inspector_offset`.
- `visual_figma.rs`: `state-workflow-builder-back`.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --all-targets --features visual-check -- -D warnings`:
  clean.
- `cargo test` (the full suite): 678 passed, 0 failed.
- New state tests (`workflow_builder_state`):
  `a_transition_opened_from_a_status_goes_back_to_it` (status → transition →
  back → nothing), `add_transition_goes_back_to_its_status_dropping_the_drawing`
  (status → add → back cancels, through a moved source and once added),
  `a_transition_selected_on_the_canvas_has_nothing_to_go_back_to` (canvas →
  transition, an edge clicked from a drilled-in transition, a handle drag),
  `reconcile_forgets_a_back_status_the_draft_lost`.
- New app test (`app_settings_tests`):
  `a_transition_panel_goes_back_to_the_status_it_came_from` clicks through
  the real window: the Status panel scrolled, an outgoing row (the panel at
  the top, ← left of the title at 28pt), ← back to the status at its offset,
  Esc twice, Add transition's drawing and new transition dropped by ← with
  the draft untouched, and an edge label with no ← and Esc deselecting.
- `--visual-figma`: `state-workflow-builder-back` (Review's Done transition
  opened from Review) checked by eye in dark and light. The
  `settings-workflow-builder-drawing` render (In Progress selected, then its
  handle dragged) now shows the ← too, which frame 5 predates; the other
  Builder renders are unchanged.
