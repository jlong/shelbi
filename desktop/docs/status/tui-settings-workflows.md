# tui-settings-workflows: Settings ▸ Workflows in the TUI

## The change

The TUI's Settings (`tui-settings-shell`) had General and Terminal
(`tui-settings-general`) and no project page. It now has **Workflows**, the index of the shown project's
workflows and its status catalog, edited in place and saved as each edit is
made, the way the desktop app's Settings ▸ Workflows page
(`dt-wf-settings-index`) works. Both clients now run the same state, edits,
save queue and guards.

## Shared (`shelbi_app::settings::workflows`)

The desktop's toolkit-free `views/workflows_state.rs` moved here whole
(`Edit`, `Target`, `commit`, `Draft`, `WorkflowsState`, the category labels
and legend text, the Workflows frames' fixture), and the desktop module is
now a re-export of it plus its tests' throwaway home. All reads and writes
still go through `shelbi_state::workflow_config` (validation,
comment-preserving edits, the on-disk conflict check, unreadable files left
alone). Changes made on the way:

- **Errors per row.** The state kept one error; it now keeps one per
  `Target`. An edit clears only its own row's error (and the header's), and
  a save that lands clears only its own. So a queued edit that saves never
  hides an earlier one that failed, and two rows can show their failures at
  once. `errors()` lists them all (a client shows one whose row is gone
  under the header).
- **`Edit::StepStatus { id, down }`**: the keyboard's reorder. It finds the
  status's position when it is applied, so moves queued behind a save each
  take one step (an absolute `MoveStatus` computed on the shown model would
  repeat the same move). A step past the end changes nothing.
  `move_status_by` hands it out, `None` at the end.
- **`loaded` ignores a read that lands while a save runs** (the save brings
  its own fresh model; the desktop view did this check itself) and returns
  whether it took it. A failed reload keeps the last good model and says why
  under the header.
- **`committed` ignores a completion with no save in flight.**
- New helpers both clients use: `confirmation(edit)` (the delete question,
  naming the file or the status and id), `Edit::is_destructive`,
  `new_status_id(name)` (the id a new status will get), `unreadable()`,
  `queued()`, `clear_error`, `load(project)`, and `prepare_statuses_yaml` /
  `prepare_workflow_yaml` (Edit YAML's file check; `statuses.yaml` is
  written from the shipped catalog first when a project has none).
- The page's copy (`WORKFLOWS_HELP`, `STATUSES_HELP`, `RENAME_NOTE`) is
  shared; the desktop page reads it from here.

Tests: `crates/shelbi-app/tests/settings_workflows.rs` (its own binary and
`SHELBI_HOME` lock) took over the desktop state tests and adds the step
queue, terminal-status refusal, per-row errors under a queue, late loads and
completions, unreadable files, a failed reload, a save that drops comments,
and Edit YAML's scaffolding.

## The TUI page (`crates/shelbi-tui/src/shell/settings/workflows.rs`)

`settings::OFFERED` now has Workflows beside General and Terminal, under the
project switcher with its number, 4 (`📑`). Edit YAML is its own shell action
(`SettingsAction::EditYaml`, which scaffolds a missing file first) next to
General's `EditFile(config.yaml)`; a paste goes to the Workflows name field
when one is open, else to General's editor command.

**Layout.** The title (with `saving…` and the queue length while a save
runs), the helper, then: any comment-loss notice (yellow) and the header's
error (red). **Workflows**: Name (with a `default` badge), Description,
Statuses and Transitions counts; files the model can't read as
`⚠ <name>  Can't be edited here: <error>`; then `+ New workflow`.
**Statuses**: `●` and the name in the board's category color
(`kanban::category_color`, the TUI's existing status color), ID, Category as
text, Used in; then `+ New status`, `Edit statuses.yaml in your editor`, and
the rename note. **Status categories**: each category's name (in its color)
and meaning. Errors show in red under the row they are about. Columns drop
as the page narrows (a description needs 12 columns to itself, ids show from
60 columns, counts from 44, Used in while 8 columns are left); nothing
panics down to 1x1.

**Keys** (page focused; Tab or Ctrl+L moves there):

| Key | On a workflow | On a status | Elsewhere |
|---|---|---|---|
| up / down (the user's sidebar nav, arrows) | move | move | move |
| Enter | Edit (the workflow editor, see below) | Rename | the `+` row's form, Edit YAML |
| `m` or `.` | actions menu | actions menu | actions menu |
| `n` | New workflow | New status | by section |
| `c` | Duplicate | Change category | |
| `s` | Set as default | | |
| `e` | Edit | Rename | |
| `y` | Edit YAML (its file) | Edit `statuses.yaml` | `statuses.yaml` |
| `x`, Delete | Delete… | Delete… | |
| Shift+↑ / Shift+↓, Alt+↑ / Alt+↓ | | move the status | |
| PageUp / PageDown / Home / End, `r` | jump; reload | | |

The **actions menu** lists every action the row has with its key (a blocked
Delete dimmed), so nothing depends on remembering a letter or on a terminal
sending Shift+arrows: Move up / Move down are there too. Inside it the
arrows and Enter choose, an entry's own key runs it, Esc closes. The hint
band names the focused row's keys.

**Typing a name** happens in the row: the status's own row for Rename, the
`+` row for New workflow, Duplicate (prefilled `<name>-copy`, `-copy-2`, …
to be unique) and New status (which shows the id it will get, from
`new_status_id`, and its category, cycled with Tab / Shift+Tab). Enter
saves, Esc cancels; Left/Right, Home/End, Ctrl+A/E, Ctrl+U and Backspace
edit; a paste types into the field. While a field, menu, picker or confirm
is open it holds every key: digits type, and Esc closes it before it closes
Settings. A new workflow or status ends selected; a refused name keeps its
row open with the model's reason.

**Change category** opens a picker of the six categories as text (the
current one marked `current`, the chosen one's meaning under the list).

**Delete** asks first (`y` or the Delete button deletes; Enter alone presses
Cancel, which is focused; `n` / Esc cancel). A delete the guards refuse up
front (the default workflow, the only workflow, a status a workflow uses)
opens no question: the row shows why. A workflow open issues use is refused
by the model when chosen (it reads the board), and the row shows its
message with the issue ids.

**Saving.** Every edit goes through the shared queue. The shell takes the
next save (`SettingsShell::begin_commit`), runs `commit` on a thread, and
the result comes back on Settings' one channel under a `Job` ticket keyed by
project; the next queued save starts when it lands. A save that lands after
a project switch is dropped (its project's queue goes with the switch). A
save that worked asks the shell's refreshers for fresh data
(`request_refresh`), so the board's columns follow the catalog at once.

**Fresh data.** The page reads the files every time it is shown (opening
Settings on it, a page switch to it, a project switch), on `r`, and when the
user's editor returns from Edit YAML (the run loop's editor handoff already
reloads Settings). A read that lands while a save runs is left to the save.

**Edit, and Edit YAML.** Edit (Enter, `e`, the menu) is routed through
`SettingsAction::EditWorkflow` to `ShellState::edit_workflow`, which now
opens the workflow editor (`tui-workflow-editor-form`, see
[`tui-workflow-editor-form.md`](tui-workflow-editor-form.md)). Edit YAML
stays an explicit action. A file that's gone says so at its row instead of
opening.

**Mouse.** A click selects a row; a click on the selected row opens its
menu; a click on a `+` row, Edit YAML or an unreadable file runs it. Menu,
picker and confirm entries are clickable; a click outside a menu or picker
closes it.

## The desktop

No behavior change. `views/workflows_state.rs` re-exports the shared module;
the page's Edit YAML uses the shared `prepare_*` helpers and its copy the
shared constants. With per-row errors, a desktop edit no longer clears an
unrelated row's error. The fixture is now `shelbi_app`'s (`#[doc(hidden)]`).

## Platform differences (by design)

- **Confirming a delete.** The TUI asks before deleting a workflow or a
  status; the desktop deletes from the ⋮ menu or the row's button at once,
  as `dt-wf-settings-index` built it (its guards disable the button up
  front instead).
- **A new workflow** opens its editor at once in the desktop (its form); in
  the TUI it is selected and Enter opens it, since the fallback editor is
  the user's terminal editor and launching it unasked would take over the
  screen.
- **Reorder.** Desktop: drag a status by its handle. TUI: Shift/Alt+arrows
  or the menu's Move up / Move down, one step at a time.
- **Status icons.** The desktop draws the Figma glyphs per category; the
  TUI draws `●` in the board's category color, as its board colors columns.
  Categories are text in both (no category icons).
- **Fresh data.** Desktop: on show and when the window comes back to the
  front after Edit YAML. TUI: on show, on `r`, and when the editor exits.

## Not done

- The workflow editor itself (its own task).
- Workflow rename and description editing from the index (neither client's
  index offers them; the editor does).
- No palette entry per page ("Settings: Workflows").
- Not hand-walked in a live TUI; the tests drive `handle_event` and
  `draw_frame` on `TestBackend` against real files in a throwaway home.

## Tests

- `shelbi-app`: `tests/settings_workflows.rs` (above).
- `shelbi-tui` `shell::settings::workflows::tests`: the tables and legend;
  every row reached by keys and Enter's action per row; create (digits type,
  Backspace, Enter, lands selected); a taken name; duplicate and set
  default; delete refused for the default, confirm with Cancel focused,
  then delete; open issues block a delete; rename in place keeps the id and
  comments, Esc cancels, unchanged saves nothing; the category picker as
  text; Shift/Alt reorder with the cursor following, the menu's moves; a new
  status's id preview and Tab category; a status in use, then an unused one
  deleted after confirming; the menu's entries and keys; three edits queued
  behind a save; a save and a read for a project left are dropped; an
  on-disk conflict; an unreadable file listed, opened, and untouched while
  other edits save; the comment-loss notice; paste; a failed read and no
  project; narrow and tiny sizes; light readability of the page, menu and
  picker.
- `shelbi-tui` `shell::tests`: Workflows by its number at 80x24 and 160x48
  (sidebar row, tables, scrolling to the last status, row hints); a workflow
  created from the keys through the real commit thread (a digit typed into
  the name, a paste, Esc closing the field before Settings); Edit YAML
  queuing the editor and its return reloading the page, a missing file's
  error at its row; a project switch reading the new project's files; light
  readability through `draw_frame`. The existing stop-walk tests now include
  the Workflows row.
- Desktop: `cargo test` (the moved state tests run in `shelbi-app`; the
  page, settings and editor tests run unchanged over the re-export).

## Verified

Root: `cargo build --workspace`, `cargo clippy --workspace --all-targets --
-D warnings`, `cargo check --workspace --locked`, `cargo test --workspace`.
Desktop: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`. `Cargo.lock` unchanged.
