# dt-workspace-settings-forms: the New and Edit workspace dialogs

The forms behind Settings ▸ Workspaces' workspace intents
(`dt-workspace-settings-index`), from the revised Figma frames "Settings ·
Workspaces · New workspace" (`2528:9683`) and "Edit workspace" (`2526:9184`),
over the shared configuration and lifecycle operations
(`shelbi_orchestrator::pool_admin`, `dt-workspace-machine-config-services`).
Add machine and Configure open the machine form (`dt-machine-settings-forms`);
nothing on the page opens the pool file any more.

## The change

**Opening.** New workspace opens New; a row's pencil, the row and its menu's
Edit open Edit for that row's workspace (the page's last read, not a
fixture); the menu's Remove workspace… opens the removal confirmation on its
own. The window holds the open dialog (`ShelbiWindow::pool_dialog`) and draws
it over everything: the `scrim` (`surface/overlay`, the frames' "Modal
scrim") over the whole window, the 600pt panel centered over the main area,
as the frames place it at x 560.

**The form** (`views/workspace_dialog.rs`, state in
`views/workspace_form_state.rs`): the title and its line, then

- **Name** and **Machine** side by side. New suggests the first NATO name the
  project doesn't use (the frame's `echo` after alpha to delta), selected so
  typing replaces it, on the first local machine. Machine is the Picker (the
  Button at Base with a chevron); its list is the configured machines, each
  with `This Mac` or `SSH · <host> · <reachability>` (Unreachable in the error
  color), and follows the page's rereads, so a machine added meanwhile is
  offered.
- **Tags**: the workspace's own tags (commas or spaces between them), never
  its machine's. When the machine has tags, a second helper says the
  workspace also gets them from the machine and where they're changed, so a
  save neither copies nor drops them.
- **Worktree location**: `workspace::workspace_worktree` for the draft's name
  and machine (`~`-shortened on this Mac), live as either changes, in the
  revised disabled Textbox (the Textbox at half opacity) in both dialogs.
  It's a read-only input, so the path can be selected and copied and Tab
  stops on it. After a rename or a move, its helper says what Save does to
  the worktree and what Shelbi checks first.
- **Slot (optional)**: empty is `None`, shown as `Automatic (n)` where `n` is
  `Project::workspace_slot` for the draft; any whole number, zero included, is
  explicit.
- A rule, then **Remove workspace** at the left (Edit only), **Cancel** and
  **Create workspace** / **Save changes** at the right (Save is off until
  something changed).

**Saving.** Submit checks the draft with the service's own planners
(`plan_workspace_add`, the new `plan_workspace_update`, `parse_slot`) against
the page's read; a problem shows beside its field (Name, Machine, Tags, Slot)
and focus goes to the first one. A clean draft becomes a `Job` naming the
project the dialog was opened for, run off the UI thread through
`PoolService` (`add_workspace`, `update_workspace`, `remove_workspace`).
While it runs a spinner shows beside the actions, and Return, the buttons,
Cancel and Esc do nothing, so it's sent once. Success closes the dialog, the
page rereads the pool (its `Changed` refreshes the window's feed, so the
sidebar's tree and counts follow), and any warning the service returned (an
old worktree left in place) shows as the page's notice. An error keeps the
dialog and every value as typed: a field's beside it, any other (an active
task, work at risk, a conflict with an edit made elsewhere, an unreachable
machine, a failed write) under the fields.

**Removal.** "Remove <name>?" says what happens: the session stops, the
worktree at its path on its machine is deleted, the workspace leaves the
project, its branches stay, and an active task or uncommitted or unpushed
work stops it. Remove workspace sends `remove_workspace` with `force: false`
and the workspace as loaded; the service re-checks the task and the worktree,
and a refusal shows in the confirmation ("… Nothing was removed."). From
Edit, the confirmation stacks over the form and its Cancel returns there.

**Keys and focus.** The dialog takes its keys ahead of the text fields
(`intercept_keystrokes`) and the window passes nothing behind it (no sidebar
chords, palette or menu commands; ⌘W closes the dialog). Tab / ⇧Tab cycle
Name, Machine, Tags, Worktree location, Slot, Remove (Edit), Cancel, the
default action, and wrap. Return is the default action (on Cancel or Remove
workspace, that button); Space presses the focused button or opens the
picker; ↑ / ↓ open it and move in it, Return or Space picks, Esc closes it.
Esc or Cancel closes the dialog and discards the draft. Focus rings show
once the keyboard is used. On close, focus returns to the control that
opened it: New workspace, the row's pencil (the renamed row's after a
rename) or ⋮; after a removal, the next row's pencil (else the previous
row's, else New workspace). New workspace and the rows' pencil and ⋮ now
take the keyboard: Return or Space presses them, Esc hands the keys back to
the settings sidebar.

**Accessibility.** The panels are `Dialog` nodes named by their titles with
their line as the description; each field is a group named by its label;
buttons carry their labels; the picker is a `ComboBox` (expanded state, the
machine's detail as its description) over a `ListBox` of options.

**Project switch and leaving.** The window drops the dialog when it switches
projects, changes route, or leaves Settings: the draft is discarded, as the
workflow and agent editors' drafts are. A job already sent finishes against
the project it names.

## Decisions

- **The window holds the dialog**, not the page: the frames' scrim covers
  the sidebar too, which only a window-level layer can draw.
- **One dialog at a time**: the window opens no workspace dialog while the
  machine form is open, and the settings view opens no machine form while a
  workspace dialog is (both are modal over the whole window). With both
  forms in, the page's open-the-YAML fallback (`edit_in_yaml` and its
  reread-on-return) is gone.
- **`plan_workspace_update` in `pool_admin`**: `update_workspace`'s checks
  moved into a public planner (it calls it), so the dialog checks an edit
  with exactly the service's rules instead of a copy.
- **Suggested name**: New prefills the first unused NATO name, as the frame's
  `echo` reads; a project past `zulu` gets `workspace-<n>`.
- **Tags split on commas and spaces**: `normalize_tags` refuses a tag with a
  space in it, so `gpu linux` is two tags rather than an error.
- **The slot placeholder names the automatic slot** (`Automatic (1)`, the
  frame says `Automatic`), since the number is what workflows use.
- **Save changes is off with nothing changed**, as in the agent editor.
- **Removal's default is Cancel**: Return on the confirmation presses the
  focused button, which starts on Cancel (Apple's guidance for destructive
  confirmations).
- **Busy dialogs don't close**: Cancel and Esc wait for the request, so a
  closed dialog never hides a change still being made.
- **Focus after removal** goes to the neighbouring row's pencil, so the
  keyboard stays in the table.

## Checks

- `views::workspace_form_state::tests` (16): New and Edit initialization
  (own tags, explicit slot), live location and slot placeholder, inherited
  note, the requests sent (empty slot `None`, zero kept, expected entry,
  tags not copied from the machine), field errors keeping the draft,
  single submission, Tab order, the picker from the keyboard, Return and
  Space, a machine added after a reread, the removal confirmation's default,
  refusal and success.
- `app::settings_tests` (dialogs, over the real `pool_admin` with a scripted
  `PoolOps` for a local and an SSH machine in a throwaway home): create on the
  remote (`materialize Build server Ssh echo`), the page rereading and the
  feed refresh, focus back on New; Edit from the row saving only its own
  tags; a rename moving the worktree and focus following it; an active task
  refusing a rename with every value kept, a field error beside Name, Esc
  writing nothing; two Returns and an Esc before the job runs sending one
  request, with the spinner drawn; Tab containment, the sidebar chords held,
  the picker by keyboard; removal from the menu (Return on Cancel, a refusal
  for unpushed work, then success with the next row focused); removal from
  Edit over a remote; a project switch dropping the draft unsaved.
- `--visual-figma` (`SHELBI_VISUAL_ONLY=workspaces`):
  `settings-workspaces-new` and `settings-workspaces-edit` in both themes,
  compared by eye with 2528:9683 and 2526:9184 at 1440 × 1024: the panel at
  x 560 and 600 wide, the fields, helpers, rule and actions where the frames
  put them; the content differs where the frames' sample data does. And
  `state-workspaces-new-picker` and `state-workspaces-remove`, checked by eye.
