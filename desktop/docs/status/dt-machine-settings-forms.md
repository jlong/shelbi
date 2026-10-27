# dt-machine-settings-forms: Add machine and Configure machine

Settings ▸ Workspaces' **Add machine** and a machine's **Configure** open a
600pt dialog over the window (Figma "Settings · Workspaces · Add machine"
`2526:9557` and "Configure machine" `2528:9870`, over the revised overview
`2523:8879`). It edits a draft of one machine and saves it through the shared
machine service (`shelbi_orchestrator::pool_admin`, from
`dt-workspace-machine-config-services`). Remote readiness checking and setup
are `dt-machine-readiness-setup`'s; this task shows the connection-check
region and hands it structured requests.

## The change

**Where it lives.** `views/machine_form_state.rs` holds the draft, the keys
and the check region's state, toolkit-free; `views/machine_form.rs` paints
it and runs its blocking work. `SettingsView::open_pool_form` opens it for
`Intent::AddMachine` and `Intent::ConfigureMachine(name)` (the workspace
intents still open the pool file until their forms land) and owns it while
it's open; the window draws it as its top layer (`machine_form::render_layer`):
the scrim over the whole window, the dialog centered over the main area, as
the frames place it. An overlay (the palette) still opens above it.

**The forms.**

- Title, then what the form is about: Add machine says it connects another
  machine over SSH; Configure names the machine and how many of the
  project's workspaces it runs ("Build server runs 2 workspaces for this
  project.", "runs 1 workspace", "runs no workspaces").
- **Name** and **SSH host** side by side, the SSH help under them; then
  **Project directory**, then **Machine tags** with their helpers. Add
  machine starts empty (descriptive placeholders, no sample values);
  Configure starts from the machine as the page last read it.
- **This Mac** (a local machine's Configure): Name alone, the directory with
  **Choose…** (the native folder chooser), and tags. No SSH host, no
  connection check, no CLI footnote. Its kind stays local.
- For an SSH machine, the **connection check** region and its footnote.
- A rule, then **Cancel** and **Add machine** / **Save changes**.

**Fields.**

- *Project directory* is `Machine::work_dir`, the project's root on that
  machine (workspace worktrees go under it). This Mac's shows `~`-shortened
  and the service expands `~` against this Mac's home. A remote's is shown
  and handed on exactly as stored or typed: the service refuses a remote `~`
  ("write the remote home directory out in full") rather than expand it
  here, and nothing checks a remote path with local filesystem calls.
- *SSH host* is handed to the service as typed (trimmed). An SSH config
  alias works because every SSH call goes through the user's `ssh` and its
  config and keys; nothing collects a password or stores credentials. Empty
  means SSH connects to the name, as before.
- *Machine tags* are typed comma-separated and normalized by the service
  (`normalize_tags`). Every workspace on the machine inherits them; the page
  rereads after a save, so the workspace rows show the new effective tags.

**Saving.** Add machine / Save changes runs once at a time (the button
disables, a spinner shows) and calls `pool_admin::add_machine` or
`update_machine` off the UI thread. Configure sends the machine as loaded
as `expected`, so an edit made elsewhere in between is a conflict, not
overwritten. The service validates against the config as it is then
(names unique ignoring case and punctuation, a usable host, an absolute
directory, tags), refuses moving or re-hosting a machine whose workspaces
are active or have worktrees the change would orphan, carries its
workspaces along on a rename, and writes only the changed keys
(comments, `forward:` and other keys the form doesn't own survive; global
projects write `projects/<p>.yaml`, in-repo projects `projects/<p>/local.yaml`,
never the committed half). A refusal keeps the draft: one about a field marks
it (red edge, the message in place of its helper, the keyboard moved there);
anything else shows over the actions, with what to do next for a conflict, a
machine gone since, or a version mismatch. Editing a field clears its error.

**After a save** the dialog closes, the keyboard goes back to what had it,
and the page rereads the config. Its change reaches the window
(`WorkspacesEvent::Changed` → `SettingsEvent::PoolChanged` → feed refresh),
so the machine rows and counts, the workspace rows' tags, the sidebar and
anything that reads the pool next (the workspace forms' machine pickers)
follow.

**Cancel / Esc** close the dialog and discard the draft; nothing was
written. A project switch rebuilds the settings view and drops the form
with it. Answers land through a weak handle: a closed form takes no late
answer, and a save already running finishes for the project it started
for, never the one switched to.

**Connection check.** The region shows what's known for the draft's host
and directory:

| State | Shows |
| --- | --- |
| not checked | "Not checked yet", what a test checks, **Test connection** |
| checking | a spinner, "Checking…", the host, the button disabled |
| `Ready` | the check, "Connection successful", the service's detail, **Test again** |
| `Reachable` | "SSH connected", the CLI, Git and directory not checked yet, **Test again** |
| `NeedsCli` | a warning, "Shelbi CLI needed", the message, **Install CLI** and **Test again** |
| `Failed` | a warning in red, "Couldn’t connect", why, **Test again** |

Each button makes a `ReadinessRequest { run, intent: Test | TestAgain |
InstallCli, target }`, where the target is the draft as it stands (project,
the configured machine's name or none, name, SSH destination, directory),
and runs it through `machine_form::Engine::readiness` off the UI thread. A
result counts only for the host and directory it was checked with: editing
either drops the region back to not checked, and an answer for an older
draft is dropped. Checks never write the project's config.

Until `dt-machine-readiness-setup` replaces `Engine::real`'s readiness
function, it asks only whether SSH reaches the host (the page's own probe,
`machines::probe`), so it answers `Reachable` or `Failed` and never claims
the successful state; asked to install the CLI (which it never offers), it
says to install it by hand.

**Keys.** Tab and ⇧Tab move through the fields and buttons (the buttons'
focus ring shows once the keyboard has moved), Return submits from a field
or presses the focused button, Space presses the focused button, Esc
cancels. The keys are the dialog's only while the keyboard is in it, so a
palette opened over it keeps its own. The window's sidebar chords don't
reach the page under it.

## Decisions

- **The latest helper copy in both forms**: SSH keys and settings are
  "observed", and Project directory is "The root project directory on this
  machine." (the Configure frame still has the older "used" and worktree
  wording).
- **The dialog is the window's layer, owned by the settings view**, so its
  scrim covers the sidebar as the frames draw it, while closing Settings or
  switching projects drops it with the view.
- **No client-side validation.** The service validates under its lock
  against the config as it is then; a second, local copy of its rules could
  only disagree with it.
- **An interim SSH-only readiness check** rather than an inert Test button:
  it reuses the page's probe and says only what it checked. The readiness
  task swaps in the real check through `Engine::readiness`.
- **A press on the scrim does nothing**; Cancel and Esc dismiss, so a stray
  click never loses a draft.
- **`MachineRow::config`** carries the whole `Machine` the page read, so
  Configure opens instantly from it and sends it as `expected`.
- **The connection-check icons** are the 20pt Heroicons drawn at 16pt (the
  frame's size), as the spinner is scaled; no 16pt drawings were vendored.

## Checks

- `views::machine_form_state::tests`: Add is an empty SSH draft; Configure
  fills the machine and its workspace count (0, 1, n); This Mac's form has
  no SSH, a `~`-shortened directory and a local kind, and nothing to test; a
  remote directory is never shortened or expanded; tags split on commas and
  normalize through the service; host fallback and aliases; Configure saves
  as an update expecting the loaded machine, one at a time, another run's
  answer dropped; refusals mark their field or the form and keep the draft;
  checks count only for their host and directory; Install CLI only when
  asked for; no host to test marks the host; Tab, ⇧Tab, Return, Space, Esc.
- `app::settings_tests` (the real service over a quiet `PoolOps`, in a test
  home): add a machine with an alias host, a spaced path and tags, the page
  and sidebar refreshed and focus restored; Configure's values and count,
  a tag change that keeps `forward:` and its comment and reaches the
  workspace's inherited tags; This Mac's form with Choose… staying local; a
  local `~` expanded and a remote one refused; a duplicate name; moving a
  machine with an active workspace refused, then a rename carrying its
  workspace; a concurrent edit is a conflict that overwrites nothing;
  Cancel and Esc write nothing; Tab order and the sidebar chords held; a
  check (and Install CLI) writes nothing and goes stale with the host; a
  project switch drops the form; an in-repo project writes `local.yaml` only.
- `--visual-figma`: `settings-machine-add` (the frame's sample values typed,
  a passing check landed) and `settings-machine-configure`, plus
  `state-machine-add-empty`, `-configure-local`, `-check-failed` and
  `-needs-cli`. The Add dialog differs from the frame's in 0.74% of its
  pixels at 10% fuzz: its 600 × 693 box, position (x 560, y 166), fields,
  region and actions line up; the differences are glyph rendering and the
  sample directory.
