# dt-workflow-edit-yaml: Edit YAML in the workflow ⋮ menus

## The change

Both workflow ⋮ menus (a row's on Settings ▸ Workflows, and the workflow
editor's titlebar) now lead with **Edit YAML**, which opens the workflow's
file (`~/.shelbi/projects/<project>/workflows/<name>.yaml`) in the user's
configured editor:

```
⋮ ┌──────────────────┐
  │ Edit YAML        │
  │ Duplicate        │
  │ Set as default   │
  │ ──────────────── │
  │ Delete           │
  └──────────────────┘
```

- **The path** comes from the config model
  (`WorkflowConfig::workflow_path`; the editor's `EditorState::file`, the
  workflow as saved), never built by hand.
- **The launch** is the one the Statuses section's Edit YAML button and
  `edit:*` (`edit:workflows`) use, now one function:
  `external_edit::open_file` (`resolve_editor`, then `launch_for` and
  `spawn`: a terminal editor in Terminal.app, a GUI editor directly). All
  three callers go through it. A launch that fails shows its message: under
  the row on the page, at the editor's Save error line in the editor.
- **No icons**: neither ⋮ menu draws icons on its items, so Edit YAML has
  none either.
- **In the editor with unsaved changes** Edit YAML is a fourth `Leave`
  (`Leave::EditYaml`): the same Save / Discard / Cancel prompt Back uses.
  Save writes the draft, then opens the file (a refused save, e.g. a
  conflict, opens nothing and shows its error); Discard reverts, then opens
  it; Cancel does nothing. The editor stays open either way.
- **Picking up the hand edit**: launching Edit YAML (either menu, and the
  Statuses button) marks the view to read its files again the next time the
  window becomes active (`observe_window_activation`). The page re-reads
  them as it does when shown. The editor reads them into its draft only if
  the draft is still clean when the read lands (no edits, no save running);
  a dirty draft is kept, and its save is refused by the model's on-disk
  conflict check ("Changed on disk ... Discard to reload them."), so it
  can't overwrite the hand edit. A hand edit that removed the workflow
  closes the editor, as Discard's re-read does.
- **A file that can't be read** (listed on the page with its error) now has
  a ⋮ button too, whose menu holds Edit YAML only: that's how the file gets
  fixed. Once fixed and the window is back in front, it lists as a normal
  workflow.

## Decisions

- **The broken file's menu holds only Edit YAML**, not the other three
  disabled. Duplicate, Set as default and Delete all go through the model,
  which can't load that file, so they would only ever show disabled.
- **Re-read on return only after an Edit YAML**, not on every activation.
  It's what the task asks, and it keeps an editor's clean draft (and the
  page) from re-reading each time the user switches apps.
- **The Statuses Edit YAML button re-reads on return too**, since it now
  shares the page's launch path; a hand edit to `statuses.yaml` shows the
  same way.
- **Discard's stale re-read and the return re-read share `read_again`**,
  which now skips a draft that became dirty while the files were read
  (before, a Discard's re-read could land over edits made in between).
- **Tests open no editor**: under `cfg(test)`, `open_file` records the path
  in `external_edit::tests::OPENED` (it never resolves or spawns an
  editor), and the tests count the opens of their own home's files.

## Code

- `views/external_edit.rs`: `open_file`, and the test recorder
  (`tests::OPENED`, `tests::opened`).
- `app.rs`: `open_in_editor` launches through `open_file`.
- `views/workflows.rs`: `edit_workflow_yaml`, `open_yaml` (shared with the
  Statuses `edit_yaml`), `reload_on_return` and its activation observer;
  `more_button`, `render_unreadable_row`; `render_workflow_menu` takes the
  name and an optional summary (none for a broken file).
- `views/workflow_editor_state.rs`: `Leave::EditYaml`, `EditorState::file`,
  `EditorState::open_failed`.
- `views/workflow_editor.rs`: the menu item, `edit_yaml`, `read_again`,
  `reload_on_return` and its activation observer. The titlebar's layout is
  untouched (`dt-editor-back-spacing` is changing it).

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --all-targets --features visual-check -- -D warnings`:
  clean.
- `cargo test` (the full suite): 687 passed, 0 failed.
- New app tests (`app_settings_tests`), through the real window:
  - `edit_yaml_on_a_workflow_row_opens_its_file_and_the_page_rereads_it`:
    Edit YAML sits above Duplicate, opens `task.yaml` once, closes the menu,
    leaves the editor shut; a hand edit to the description shows on the
    page only after the window is deactivated and activated again.
  - `a_broken_workflow_file_still_offers_edit_yaml`: an unparseable
    `broken.yaml` gets a ⋮ with Edit YAML only, which opens it; fixed by
    hand, it lists as a workflow (with its edit button) on return.
  - `edit_yaml_in_the_editor_asks_about_unsaved_edits_first`: first in the
    editor's menu; a clean draft opens at once; with edits, the prompt
    shows, Cancel opens nothing and keeps them, Discard reverts and opens,
    Save writes and opens.
  - `the_editor_rereads_a_clean_draft_on_return_and_keeps_a_dirty_one`: a
    clean draft takes the hand edit on return; a draft edited while away is
    kept on return, and its Save is refused with "Changed on disk", the
    second hand edit standing.
- Not checked by eye in `--visual-figma`: the change is one more text row in
  two existing menus and a ⋮ button on the broken-file row, both built from
  the existing menu and row-button helpers.
