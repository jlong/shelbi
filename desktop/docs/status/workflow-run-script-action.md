# workflow-run-script-action: Run script in a transition's Actions

## The change

A transition's `run` commands, `ready` check and `ready_timeout` were three
fields in its Details panel (the commands as one text area on the desktop,
one row each in the TUI), away from the actions they run after. They are now
a **Run script** action in the transition's Actions, in both clients and in
both views (Form and Builder). Details keep Target branch and URL.

`run` is not a `TransitionAction` in `shelbi_core`, and the engine needs no
change: Run script is an action of the editor's, backed by the transition's
existing `run` / `ready` / `ready_timeout` keys. Nothing new is written to the
YAML's `actions`.

## Shared (`shelbi_app::settings::workflow_editor`)

- `ScriptForm { run: Vec<String>, ready, ready_timeout }` (its own form, a
  command per entry; blank entries are dropped on apply) and
  `DetailsForm { target, url }`.
- `Change::Script { index, form }` and `Change::RemoveScript(index)` (clears
  all three keys). A bad timeout is refused at the transition's row, as
  before.
- `EditorState::has_script(index)`: any of the three keys set. The row shows
  Run script then; the add menu offers it otherwise. `script_form(index)`.
- `TextForm::script` (the open editor's fields) beside `details`;
  `text_form(details, script)` takes both. `differs` compares the applied
  transition, so a blank command row, or the editor filled as loaded, is no
  change.
- Copy: `RUN_SCRIPT`, `RUN_HELP` (now says the task must be assigned to a
  workspace), `RUN_PLACEHOLDER`, `ADD_COMMAND`, `SCRIPT_READY`,
  `SCRIPT_TIMEOUT` (were `DETAILS_READY` / `DETAILS_TIMEOUT`).

## Desktop

- Form: a `Run script ⌄` chip after the action chips (Select style while its
  editor is open), its menu Edit (Done while open) and Remove. The + menu
  lists Run script after the git actions while the transition has none;
  picking it opens the editor on one empty, focused command.
- The editor opens under the row in the Details panel's style: title and
  help, a row per command (drag handle, number, field, ✕), Add command,
  Ready check and Ready timeout, Done. Commands reorder by dragging their
  handle onto another row (the Builder actions' ghost). It isn't draggable
  among the git actions: it always runs after them.
- Builder: the transition panel lists a Run script row after the action rows
  (no handle; its button opens the same editor in the panel, trash removes),
  and Add action offers Run script. New transition doesn't (add it once the
  transition exists). The desktop Builder panel still has no Details.

## TUI

- Form and the Builder's transition panel: `N. Run script · K commands ▸`
  after the numbered actions; Add action lists Run script. Enter opens its
  menu (Edit / Done, Remove); `e` toggles the editor, `x` removes, `d`
  Details, `n` Add action.
- The editor opens inline under the actions (the Form) or in the panel (the
  Builder), as the TUI's other inline editors do, rather than as a dialog:
  help, Commands (a box per command; `n` adds, `x` removes, Shift/Alt+↑↓
  reorders), Ready check, Ready timeout, Done. In the Builder, selecting
  another transition closes it.

## Tests

- `crates/shelbi-app/tests/settings_workflow_editor.rs`: Run script writes
  `run` in order (blank rows dropped), `ready`, `ready_timeout`; refused
  timeout; reopened workflow shows it and an unedited save changes nothing;
  reorder; Remove clears all three keys. Details write only target / url.
- TUI (`workflow_editor/tests.rs`, `tests/builder.rs`): added from Add
  action by keys, commands typed / added / reordered, Done lands on its row,
  its menu, the add menu stops offering it, refused timeout, Builder panel
  rows, `x` removes.
- Desktop (`app_settings_tests.rs`): chip + editor round trip with a drag
  reorder and a remove, save to disk, reopen, Remove; the Builder panel's
  row; Details show no Run fields.
