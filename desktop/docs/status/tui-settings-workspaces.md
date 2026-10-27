# tui-settings-workspaces: Settings ▸ Workspaces in the TUI

The TUI's Settings (`tui-settings-shell`) gains the project's Workspaces page,
after the desktop's (`dt-workspace-settings-index`) and its New / Edit
workspace forms and removal (`dt-workspace-settings-forms`), over the shared
configuration and lifecycle services (`dt-workspace-machine-config-services`).
The toolkit-free state both clients use now lives in `shelbi-app`, and the
service's pure checks in `shelbi-core`, so the two clients run one model.

## Shared

- **`shelbi_core::pool`** (new): the requests, results and errors of the pool
  operations (`AddWorkspace`, `UpdateWorkspace`, `RemoveWorkspace`,
  `WorkspaceChange`, `WorkspaceRemoved`, `PoolError`, `PoolField`, ...) and the
  checks a form runs on its draft (`plan_workspace_add`,
  `plan_workspace_update`, `resolve_add_machine`, `normalize_tags`,
  `parse_slot`, the name/slug check), plus `workspace_worktree`. Moved out of
  `shelbi_orchestrator::pool_admin`, which re-exports them (the CLI and its
  callers are unchanged) and keeps everything that touches disk, git, SSH or
  sessions. `shelbi-app` can't depend on the orchestrator (the orchestrator
  depends on it), so this is what lets the form state be shared.
- **`shelbi_app::settings::workspaces`**: the desktop's
  `views/workspaces_state.rs`, moved (`PoolIndex`, `WorkspaceRow`,
  `MachineRow`, `WorkspacesState`, `MachineStatus`, `Intent`, `read`,
  `probe_key`). `LOCAL_LABEL` / `LOCAL_PHRASE` name the local machine "This
  Mac" on macOS and "This computer" elsewhere (the TUI also runs on Linux; the
  desktop's text is unchanged). `fixtures` (behind `test-support`) is the pool
  both clients' tests build from.
- **`shelbi_app::settings::workspace_form`**: the desktop's
  `views/workspace_form_state.rs`, moved (`WorkspaceForm`, `RemoveConfirm`,
  `Job`, `Op`, `Outcome`, `Action`, `Errors`, `MachineChoice`, `PoolService`).
  `PoolService` is now built by `pool_admin::workspace_service()` (live) or
  `pool_admin::workspace_service_with(ops)` (a test's scripted `PoolOps`).
  New: `SLOT_AUTOMATIC_HELP` (what automatic numbering is) and
  `Outcome::notice` (one line for the page after a change).

## The desktop

`views/workspaces_state.rs` and `views/workspace_form_state.rs` are gone; the
page, the dialog, the machine form, the window and the visual harness import
the same types from `shelbi_app::settings::{workspaces, workspace_form}`. The
dialog's live service is `pool_admin::workspace_service()` (was
`PoolService::live()`), its tests' `pool_admin::workspace_service_with(ops)`
(was `PoolService::with_ops`), and its tests' `write_pool(name, pool)` writes
under `SHELBI_HOME` through `shelbi_state::projects_dir()` (was
`write_pool(&home.root, …)`). No behavior changes.

## The TUI

`crates/shelbi-tui/src/shell/settings/workspaces.rs`, wired in
`shell/settings.rs` (Workspaces joins `OFFERED`) and `shell/mod.rs` (the
reader, the service, and their results on Settings' one channel).

**The page** (5, in the project section before Workflows): the heading and
what the page is (configuration; the Machines view stays where they're
watched at work), New workspace, the workspace table (name, machine with
Local or SSH, own tags in cyan and the machine's as muted `+tag`, or No tags,
and the effective slot, `N auto` when automatic) with each worktree on the
line under its name, a footnote on slots and `+tag`, then Machines: Add
machine and each machine (its name, Local or `SSH · host` with its
reachability, then its root directory and workspace count, and SSH's reason
when it's unreachable), and a footnote saying reachability is only SSH.
Loading, a failed read (its error, `r` or Try again), no workspaces, and no
project each say so.

**Keys on the page**: ↑ / ↓ (and the sidebar's nav keys) move the selection,
Enter runs it (a workspace row opens Edit), `n` New workspace, `e` Edit, `d` /
Delete Remove, `r` reads again, Tab back to the sidebar, Esc leaves Settings.
Clicks open rows.

**The form** is a modal box over the main area that takes every key and
click until it closes: Name, Machine (the picker), Tags, Worktree (derived,
read-only, follows the name and machine), Slot (empty shows `Automatic (n)`
with the rule spelled out), Remove workspace… (Edit), Cancel and Create
workspace / Save changes. Fields edit at the end of the text: type,
Backspace, Ctrl+U clears, and a paste (or a dropped file's path) lands on
the focused text field as one line; New's suggested name is replaced by the
first key typed or the paste. Tab / Shift-Tab, and ↑ / ↓ between text fields, move; on Machine ↑ / ↓
open the picker and walk it, Enter picks, Esc closes it; ← / → walk the
buttons; Enter is the default action (on Cancel or Remove, that button); Esc
cancels. The machine's inherited tags are said under Tags, never copied into
the draft. Errors from the shared checks show under their field with focus
on the first; refusals and failures (an active task, work at risk, a hook
merge, a conflict, a provisioning failure) show under the fields with the
draft kept, and the same submit retries.

**Removal** asks first ("Remove alpha?", what it deletes and what stops it),
starts on Cancel, and sends `force: false`; a refusal stays in the
confirmation saying nothing was removed.

**Running.** A submit hands the shell a `Job` and a ticket; the shell runs it
on a thread through `PoolService` and the result comes back on Settings'
channel. While it's out the form is busy: Enter, Esc, Cancel and typing do
nothing, so it's sent once. Nothing is written before an explicit Create,
Save or Remove, and Cancel / Esc write nothing.

**Refresh and stale results.** Success closes the form, shows a notice
(`Created zeta.`, `Renamed alpha to alpha2.`, any warning), reads the page
again, and asks the shell's refresher for a new snapshot (the sidebar's tree
and the machines view read the config again; dispatch and routing read it on
every use). A read that finds the pool changed since the last one (an edit
elsewhere) refreshes the sidebar too. The shown page reads again every 5s and
when the editor returns. A project switch drops the form and the page's data;
a read or result still in flight for the old project is dropped when it
lands. A committed change refreshes the project it was for only while that
project is shown; Settings closed (the form goes, as the desktop's dialog
does) still lands the result on the page's notice.

**Reachability** comes from the shared `ReachabilityProber` over
`machine::probe_host_reachability`, started while the page shows and stopped
when it doesn't; never spawned under test.

**Machines.** Add machine and Configure open the file the pool is declared
in (`projects/<p>/local.yaml` in-repo, else `projects/<p>.yaml`) in the
user's editor and say so on the page; the TUI's machine form is its own task.

## Decisions

- **The planners moved to `shelbi-core`, not `shelbi-state`**: they're pure
  functions of a `Project` and `shelbi-core` already owns `Project`'s pool
  rules (`validate_workspaces`, `workspace_slot`, `effective_tags`).
- **`pool_admin::workspace_service`** builds the shared `PoolService`, so the
  live wiring is one function both clients call.
- **End-of-text editing** in the TUI form, like its Add project form; no
  caret movement inside a field.
- **The TUI drops an open form when Settings closes**, like the desktop's
  dialog; a request it already sent still lands.
- **Removal from the page's `d`**, alongside Edit's Remove workspace…, as the
  desktop's row menu offers both.

## Platform differences

See `parity.md` (Settings ▸ Workspaces): the form's frame and text editing,
the TUI's page keys, the spelled-out slot rule, and machine actions (the
desktop's dialog against the TUI's editor hand-off).

## Checks

- `shelbi_app::settings::workspaces::tests` and
  `shelbi_app::settings::workspace_form::tests`: the desktop's tests, moved
  with the state.
- `shell::settings::workspaces::tests` (19): rows (derived paths, `+tag`,
  explicit and automatic slots, counts), honest reachability with the
  configuration kept, loading / failed / empty / no-project states, a stale
  read dropped, New from the keyboard sending exactly the draft (suggestion
  typed over, case-kept tags, read-only location, slot 0), the slot's
  automatic / overflow / `u32::MAX` / non-digit cases, the shared name rules
  (duplicate, uppercase, slug collision), Edit with own and inherited tags
  apart and the loaded entry as `expected`, the machine picker by keyboard,
  Cancel and Esc, removal (Cancel first, never forced, active-task and
  unpushed refusals, then success), hook-merge errors keeping the draft and
  retrying the same request, a paste onto the focused field, results after
  a project switch and after Settings closed, machine actions opening the
  pool file, a reread offering a new machine, drawing at any size, and
  clicks.
- `shell::tests` (6, through the real event path with the real reader and
  `pool_admin` over a scripted `PoolOps` in a throwaway `SHELBI_HOME`):
  create from the keyboard in the global and the in-repo config (written once
  through the service, comments kept, the shared in-repo file untouched, the
  page and sidebar refreshed); edit + Esc writing nothing, then a rename
  moving the worktree; removal refused for an active task, then done once
  idle; a hook-merge refusal writing nothing; a request in flight at a
  project switch landing without touching the new project; and a source
  check that no TUI settings code serializes a project.
