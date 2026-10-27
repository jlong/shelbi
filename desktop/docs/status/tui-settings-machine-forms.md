# tui-settings-machine-forms: the machine forms in the TUI

Settings ▸ Workspaces in the TUI (`tui-settings-workspaces`) gains the Add
machine and Configure machine forms, after the desktop's
(`dt-machine-settings-forms`), over the shared machine service
(`dt-workspace-machine-config-services`). Until now Add machine and a
machine row opened the pool's YAML in the user's editor.

## Shared

- **`shelbi_app::settings::machine_form`** (new): the desktop's draft, moved
  out of `views/machine_form_state.rs`. `MachineForm` holds the fields as
  typed (name, SSH host, project directory, tags), the kind (Add is always
  SSH; Configure keeps the machine's), the field and form errors, the save in
  flight (one at a time, an older run's answer dropped), the focus and the
  keys. `SaveRequest` / `Job` / `MachineService` are the save and the service
  that runs it; `refusal_message` and `notice` say what a refusal or a save
  means. `Controls { browse, checks }` is what a client adds to the Tab order
  (the desktop's Choose… and connection-check buttons); the draft itself
  never checks or records anything.
- `probe_note` is what the page's reachability probe last saw of the saved
  SSH host, always labeled "Last status probe" and never as readiness; it
  goes once the draft's host differs from the saved one. `NOT_CHECKED` is
  what a draft's connection region says before a readiness check runs.
- `shelbi_orchestrator::pool_admin::{machine_service, machine_service_with}`
  build the live service and a test's over scripted `PoolOps`, as
  `workspace_service` does for workspaces.

The readiness check's types (`Readiness`, `validate_target`, the install
outcome) stay in `shelbi_orchestrator::machine_readiness`: `shelbi-app` can't
depend on the orchestrator, and moving them is the readiness task's job
(`tui-settings-machine-readiness`).

## The desktop

`views/machine_form_state.rs` keeps only the connection check region
(`Check`, `CheckOutcome`, `ReadinessRequest`, `Panel`, ...) over the shared
draft: its `MachineForm` wraps `shelbi_app::settings::machine_form::MachineForm`
(read through `Deref`) with `Controls { browse: true, checks: true }`, and
turns the shared form's Test / Install CLI presses into readiness requests.
Its API to the view is unchanged; the draft's own tests moved with it. No
behavior changes.

## The TUI

`crates/shelbi-tui/src/shell/settings/machine_form.rs` (keys, paste, clicks,
drawing) and a `Machine` layer on the Workspaces page
(`shell/settings/workspaces.rs`); the shell runs the save on a thread
through `machine_service` and hands the answer back on Settings' channel
(`SettingsAction::RunMachine`, `SettingsMsg::MachineDone`), as it does for
workspace requests.

**Opening**: Add machine… (Enter, or `m` anywhere on the page) opens Add
machine; Enter on a machine row (or `c` on it) opens Configure machine with
the machine as the page last read it.

**The form** is a modal box over the main area: Kind (read-only: SSH, or
Local with "This Mac" / "This computer"), Name, SSH host (SSH only, with what
it is: a host or an alias from the user's SSH config, connected with the
user's own keys; no password is asked for or stored and SSH config isn't
touched), Project directory (`Machine::work_dir`; a remote's is the remote's
own path, a `~` on it must be written out in full; the local machine's shows
`~`-shortened and its `~` is the user's home), Machine tags (comma
separated, inherited by every workspace on the machine), Forwarding (shown
only when the config sets it, kept as is), and for SSH the Connection region.
Then Cancel and Add machine / Save changes.

**Editing** is a terminal line at the end of the text: type or paste (a
dropped path too) onto the focused field, Backspace, Ctrl+W / Alt+Backspace
back to the last `/`, space or comma, Ctrl+U clears. Tab / Shift-Tab and ↑ /
↓ move between the controls, ← / → between the buttons, Enter submits from a
field (or presses the focused button), Space presses a button, Esc cancels.
Below 40 columns the labels stack over their fields; a short terminal
scrolls to keep the focused control and what's said under it in view.

**Saving**: nothing is written until Add machine / Save changes, and nothing
by opening, closing or cancelling a form. While the save runs the form says
Saving…, a second submit does nothing, edits wait, and Cancel waits for the
answer. A refusal keeps the draft: a field's (a duplicate or empty name, an
unusable host, a relative or remote `~` directory, bad tags) under that field
with the focus moved there; the rest (the machine's workspaces have an
active task or a live session, a worktree would be orphaned, the config
changed since it was read) over the buttons, and the same submit retries. A
rename carries the machine's workspaces along (the service rewrites their
`machine:`). On success the form closes, the page says what happened ("Added
gpu.", "Renamed devbox to gpubox; its workspace moved with it."), the page
rereads with the cursor on the machine, and the sidebar and runtime views
refresh. The save writes where the pool is declared: the global project YAML,
or an in-repo project's machine-local `local.yaml` (the committed
`.shelbi/project.yaml` is never touched).

**Connection**: an SSH form says Not checked, and that nothing (SSH, the
Shelbi CLI, Git, the project directory) is verified until a readiness check
runs. On Configure, the page's last reachability probe of the saved host
shows under it, labeled as the probe's ("Last status probe: devbox.lan
answered over SSH. The probe doesn't check ..."), and goes as soon as the
host is edited. Nothing in the form says ready or successful.

**Stale results**: the form goes when Settings closes, another page shows or
the project switches. A save already sent still lands: for a project left
behind it's dropped (its commit still refreshes nothing in the new project);
for a closed form it says what happened on the page; a form opened since is
never answered by it.

## Intentional differences from the desktop

- No Choose… folder picker: the directory is typed or pasted, with terminal
  line editing.
- No Test connection / Install CLI yet: that's `tui-settings-machine-readiness`
  (since delivered: see [`tui-settings-machine-readiness.md`](tui-settings-machine-readiness.md)).
- Kind is a read-only row (the desktop shows it through the form's title and
  description).
- The page's footnote and the Add machine row no longer mention the editor.

## Tests

- `shelbi-app` `settings::machine_form::tests`: the draft (Add / Configure
  values, local `~` shortening, remote paths as typed, tag splitting, SSH host
  fallback, update expecting the loaded machine, one save at a time, refusals
  on their field or over the actions, the probe note's label and scope, the
  Tab order with and without the client's controls, keys, notices).
- `shelbi-tui` `shell::settings::workspaces::tests` (the page over the shared
  fixture): opening each form, local / SSH field visibility, exactly the
  draft sent once, Cancel, refusals and retries, rename notice, the probe
  label and Not checked, stale results (closed form, newer form, project
  switch), narrow and short terminals, clicks, path editing;
  `shell::settings::machine_form::tests`: word deletion, scrolling.
- `shelbi-tui` `shell::tests` (real key events through the shell, the real
  `pool_admin` over a temp `SHELBI_HOME` and scripted `PoolOps`): adding a
  machine to the global and the in-repo local config, a rename that carries
  its workspace and keeps `forward:`, a directory change refused while a
  workspace has an active task then saved once idle, duplicate and remote-`~`
  refusals beside their field, the local machine's `~` expanded, and a save
  still running at a project switch.
- The desktop's `machine_form_state` tests keep covering the check region
  over the shared draft.
