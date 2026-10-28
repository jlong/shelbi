# tui-workflow-editor-form: the workflow editor in the TUI

## The change

Settings ▸ Workflows in the TUI (`tui-settings-workflows`) listed the
project's workflows, but Edit only opened the workflow's YAML in the user's
editor. It now opens a dedicated editor for one workflow, the TUI's
counterpart of the desktop's workflow editor (`dt-wf-editor-form`), over the
same draft, the same validation, the same Save / Discard and leave prompt,
and the same mapping onto the workflow YAML. Nothing is written before Save.

## Shared (`shelbi_app::settings::workflow_editor`)

The desktop's toolkit-free `views/workflow_editor_state.rs` moved here whole
(`EditorState`, `Change`, `Field`, `Leave`, `Mode`, `ReviewMode`, the forms,
`ProjectGit`, `run_save` / `run_duplicate` / `run_delete`, the action and
strategy labels). The desktop module is now a re-export of it. This is the
one draft both clients edit, and the one the TUI's Builder follow-up will
edit too (`Mode` stays in the state, so Builder and Form never hold
separate copies).

Added on the way, used by both clients:

- **`TextForm`**: what every text field holds, as typed (Description, Base
  branch, Branch name, Required task fields, the Run the branch fields, each
  status's Workspace tags, the open Details). `text_form(details)` fills the
  fields from the draft, `type_texts(form)` makes each field that differs
  from the draft as it's typed, `text_changes(form)` hands Save its changes.
  The desktop's `sync`, `typed` and `texts` now go through it instead of
  building the changes themselves.
- **`type_texts` retries a refused field once another went in.** A Base
  branch like `feature/{{feature}}` is refused until `feature` is a required
  task field; the form applies Git before Required task fields, so typing the
  field used to leave Git refused until the next keystroke in it. Both
  clients had this; a test covers it.
- **`conflict()`**: the last save was refused because the files changed on
  disk since they were loaded. Save stays refused until Discard reads them
  again.
- `has_errors()` is public (it was test-only).
- The editor's copy (help lines, placeholders, Details and Run the branch
  labels) is shared as constants.

Tests: `crates/shelbi-app/tests/settings_workflow_editor.rs` (its own
binary and `SHELBI_HOME` lock) took over the desktop's 16 state tests
unchanged and adds the text form round trip (refused text kept out of the
draft, Save refused until corrected, a removed status's tags skipped), an
unchanged `branch_prefix` surviving an unrelated save, the conflict flag
(Save refused twice, the hand edit standing, Discard + reload), rename and
delete refused while an open issue uses the workflow, Set as default only
through the draft, and the dependent-field retry.

## The TUI editor (`crates/shelbi-tui/src/shell/settings/workflow_editor.rs`)

**Opening.** Enter or `e` on a workflow (or Edit in its menu) on Settings ▸
Workflows; `SettingsAction::EditWorkflow { name, status }` from anywhere
else. With the Workflows page's model loaded the draft shows at once; a
read starts either way, bringing the project's `git:` values (the Git
fields' placeholders) and the files as they are now. `status` puts the
cursor on that status's row: the route an agent's workflow-status
assignment takes (`tui-settings-agents` lists assignments; its agent editor
follow-up links them here). Like the desktop's editor, it hides the settings
sidebar and takes both columns; the shell paints no divider then.

**Layout** (80x24 and up; nothing panics down to 1x1):

```
 ← Back   task default ●              Save changes  Discard  ⋮ Actions
 
 Workflow
 › Name               task
   Description        Branch work with a review gate.
   Initial status     Backlog ▾
 Statuses (6)
   ● Backlog          User   orchestrator   Any workspace
   ● Review           User   review         [review]
   + Add status
   Manage project statuses
 Transitions (4)
   In Progress → Review
       1. Push branch
       2. Open PR
       + Add action
       ▸ Details
   + Add transition
 Git · Review · Automation · Delete workflow…            ↓ more
 ↑↓ · ⏎ edit · Tab section · m menu · ^S save · Esc back
```

The header never scrolls: Back, the name with its `default` badge and a `●`
while there are unsaved edits, then Save changes (dimmed without edits,
`Saving…` while a save runs), Discard (dimmed without edits) and `⋮ Actions`
(Save, Discard, Edit YAML, Duplicate, Set as default, Manage project
statuses, Delete workflow…; an entry the guards refuse is dimmed with the
reason under the list). The Form scrolls under it in the desktop's sections:
Workflow; Statuses (each with its owner, agent and tags; Add status; Manage
project statuses); Transitions (each `from → to`, its actions numbered in
order, Add action, Details and, open, Target branch, the Run commands one
row each with Add command, Ready check, Ready timeout, URL; Add transition);
Git (Base branch and Branch name with the project's values as placeholders,
Merge strategy); Review (Review mode with its help; Serve command, Setup,
Ready check, URL, Working directory under Run the branch); Automation (Zen
settings with what Custom sets, Required task fields); Delete workflow… (dimmed,
with the reason, for the default or only workflow). Each field's error shows
in red under its row; the Save error (or the comment-loss notice) shows at
the top of the Form.

**Keys** (all terminal-safe; no mouse needed):

| Key | Does |
|---|---|
| the user's up / down, arrows | move through the rows |
| PgUp / PgDn / Home / End | jump |
| Tab / Shift+Tab | next / previous section (header, Workflow, Statuses, Transitions, Git, Review, Automation, Delete) |
| Enter | the row's main thing: type in a field, open a picker, a status's or a transition's menu, toggle Details, press a header button |
| `m` / `.` | the row's menu (a status, a transition), else ⋮ Actions |
| `o` / `a` / `t` on a status | owner / agent picker; type its tags |
| `f` / `t` on a transition | from / to picker |
| `d` on a transition's rows | open or close its Details |
| `n` | add in the section: a status (picker), an action (picker), a Run command, a transition |
| `x` / Delete | remove a status, a transition, an action, a Run command |
| Shift+↑↓ / Alt+↑↓ | move a status, an action or a Run command (the cursor follows) |
| `y` | Edit YAML |
| Ctrl+S | Save |
| Esc | Back |

**Typing** happens in the row. Each key goes to the draft at once through
`type_texts` (the Name waits for Save, since a rename reads the board), as
the desktop's fields do; Enter, Esc or ↑↓ stop typing (↑↓ then move), Tab
stops and goes to the next section; Left/Right, Home/End, Ctrl+A/E, Ctrl+U
and Backspace edit; a paste types into the field. A field keeps what was
typed, refused text included, with its error under the row, until it's
corrected, saved, discarded or reloaded: moving away or editing elsewhere
never puts the old text back.

**Pickers** list the choices with the current one marked (`current`) and the
selected one's help under the list (Review mode, Zen settings): Initial
status and From / To over the workflow's statuses, Add status over the
catalog statuses the workflow doesn't have (or says every one is in it),
Owner, Agent (None and the project's agents), an action (those the
transition doesn't hold yet), Merge strategy, Review mode, Zen settings
(Use project settings; Custom only explains, as on the desktop). A menu
entry's own key runs it from the menu; Esc closes.

**Save** (Ctrl+S, the header, ⋮ Actions) applies the text fields, refuses
when a field's text is wrong or Run the branch has no serve command, and
otherwise runs `run_save` off the input loop (`EditorWork::Save`, through
the shell's Settings channel under the editor's `Job` ticket). A failed save
keeps the draft and shows why. A save that worked refreshes the board
(`request_refresh`) and reads the Workflows index under the editor again.
**Discard** puts the loaded model back.

**Leaving.** Back, Esc, Manage project statuses, Duplicate and Edit YAML go
through the shared `Leave` guard: with unsaved edits a Save / Discard /
Cancel prompt asks first (`s` / `d` / `c` or Esc, ←→ and Enter), and Save
goes on once its save lands. A project switch (the palette, while the
editor shows) asks the same through Settings' own prompt; its Save runs the
editor's save and switches once it lands, and a failed save stays. Back
lands on the Workflows page with the workflow's row selected (Manage project
statuses: the first status), once its fresh read lands.

**Guards** come from the shared model: rename and delete are refused while
open issues use the workflow (the model reads the board when the save or
delete runs), Delete is dimmed for the default or only workflow, Set as
default is a draft change written at Save. Delete asks first (Cancel
focused; `y` deletes).

**Conflicts.** A save the model refuses because the files changed on disk
since they were loaded opens **Changed on disk**: Reload (`r`: the draft
goes, the files are read again) or Keep editing (`k`, Esc, Enter: it's
focused). Keep editing leaves the draft, and Save stays refused, so a hand
edit is never overwritten.

**Back from the user's editor** (Edit YAML): the run loop's editor return
reloads Settings, and the editor reads its files again for its own project
only. A clean draft takes them; one with edits keeps them (its save then
meets the conflict check); a workflow gone from the files closes the editor
with a note on the Workflows page. Every result carries the editor's ticket,
so a read, save, duplicate or delete that lands after a project switch (the
editor goes with the switch) or a newer request is dropped.

**Mouse.** A click selects a row, a click on a field starts typing, a second
click does Enter, a header button presses; menu entries and prompt buttons
are clickable; the wheel scrolls without moving the focus.

## Mappings (as the desktop's)

- Branch name is `git.branch`, the full template; a workflow naming its
  branches by `git.branch_prefix` shows `<prefix>/{{id}}`, which keeps the
  prefix when saved unchanged. Empty fields and the project's merge
  strategy mean no `git:` block: the workflow inherits the project's.
- Review mode is `review:`: Diff only is no block, Run the branch a recipe
  whose `serve` is required.
- Zen settings is `zen:`: Use project settings is no block; Custom shows
  what the workflow's own block sets and isn't edited here.
- Required task fields is `required_params:`; a transition's Details are its
  `target`, `run`, `ready`, `ready_timeout` and `url`.
- Keys the form has no control for (and unknown ones) are preserved by
  `WorkflowConfig`'s comment-preserving patcher.

## Intentional differences from the desktop

- **No Builder yet.** The TUI offers Form only (`workflow_editor::VIEWS`);
  the Builder | Form control joins the header when the Builder follow-up
  lands, over the same `EditorState`.
- **Rows instead of a table.** A status's owner, agent and tags, and a
  transition's endpoints, actions and details, are reached through the row's
  keys or menu (the desktop has a picker per cell and drags). Run commands
  are one row each rather than a multi-line text area.
- **Delete asks first** (the desktop deletes at once, its guard disabling
  the button up front), as the TUI's Workflows page does.
- **A conflict asks.** The desktop shows the error and points at Discard;
  the TUI opens Reload / Keep editing.
- **A project switch asks** while the editor holds edits (the desktop's
  editor hides the switcher).

## Tests

- `crates/shelbi-tui/src/shell/settings/workflow_editor/tests.rs`: an edit
  in every section through keys alone (nothing written before Save, then
  every value on disk), refused text kept in view with its error until
  corrected (Details timeout, a Git placeholder needing a required field),
  Run the branch without a serve command, Zen back to the project's with
  unknown keys kept, an unchanged `branch_prefix` and the project's values
  as placeholders, the leave prompt for Back / Manage project statuses /
  Duplicate / Edit YAML with Save / Discard / Cancel, a failed save keeping
  the draft, the delete and rename guards, a concurrent hand edit (Reload or
  Keep editing), reload on return (clean, dirty, the workflow gone),
  Duplicate, a stale result dropped, scrolling and focus at 80x24 (every
  row's focus in view, Tab sections, the wheel), an Agents link's status
  row, tiny sizes.
- `crates/shelbi-tui/src/shell/mod.rs`: through the shell, the editor at
  80x24 over both columns, a save off the UI thread that refreshes the board
  and Back to its row with the index current, Edit YAML's return reloading
  the editor for the same project, a project switch with unsaved edits
  (Cancel, then Save and switch), an assignment link, light theme
  readability.
- The desktop's editor tests and app tests run unchanged over the moved
  state.
