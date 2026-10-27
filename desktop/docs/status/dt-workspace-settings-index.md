# dt-workspace-settings-index: the Workspaces page in Settings

The project-scoped configuration index for the workspace pool and the
machines that host it, from the revised Settings · Workspaces · Dark frame
(`2523:8879`). It is configuration UI: the runtime **Machines** view
(`views/machines.rs`) and its session behavior are unchanged. The workspace
and machine forms, the mutations, and remote setup are their own tasks; this
page hands them explicit intents.

## The change

**Settings sidebar.** The project section lists **Workspaces** first, before
Workflows and Agents, with the frame's `folder` icon and **⌘5**.

**Workspaces page** (`views/workspaces.rs`, state in
`views/workspaces_state.rs`):

- The frame's page shell: no header band, the **Workspaces** heading and its
  description with **New workspace** centered beside them.
- The workspace table, in declaration order:
  - Workspace: the name, and under it the derived worktree
    (`workspace::workspace_worktree`, `<work_dir>/.shelbi/wt/<name>`), with a
    local home folder shortened to `~`. Both clip to one line, their whole
    text as a tooltip.
  - Machine: its name, and **Local** or **SSH** under it.
  - Tags: the effective tags (`Project::effective_tags`), sorted; a tag
    inherited from the machine has "From <machine>" as its tooltip. None
    reads **No tags**.
  - Slot: `Project::workspace_slot`, explicit or the automatic zero-based
    place on its machine (the tooltip says which). The frame's 1, 2, 3 are
    sample data.
  - An edit (pencil) button and a ⋮ menu: **Edit workspace**, a rule,
    **Remove workspace…**. A row click edits too.
- The footnote, a rule, then **Machines** (**Add machine**) over the machine
  list: the icon (`folder` local, `server-stack` SSH), the name, the SSH
  host, the root project directory and the workspace count; a status badge;
  **Configure**. Then its footnote.

**States.** Loading shows "Loading workspaces…" in the table. A project with
no workspaces says so in the table and still lists its machines. A config
that can't be read shows its error and **Retry** in place of the table, with
no Machines section; the rest of Settings works as usual. A reread that
fails after a good one keeps the rows and shows the error above them.

**Reachability.** Each SSH machine is probed by a
`shelbi_app::reachability::ReachabilityProber` on its own thread, through the
Machines view's probe (`machines::probe`, the shared
`probe_host_reachability`, stubbed under `--visual-figma`). The badge reads
**Checking…**, **Reachable** or **Unreachable** (SSH's reason under the
machine's details, in the error color); a local machine reads **Local** and
is never probed. A local-only project starts no prober. Results are keyed by
machine *and* host (`workspaces_state::probe_key`), so a machine whose host
changes reads Checking until its new host answers, never the old host's
result.

**Intents.** New workspace, a workspace's pencil, row and menu items, Add
machine and Configure each emit `WorkspacesEvent::Open(Intent)`
(`NewWorkspace`, `EditWorkspace(name)`, `RemoveWorkspace(name)`,
`AddMachine`, `ConfigureMachine(name)`), then
`SettingsEvent::OpenPoolForm(intent)`, and the window routes it to
`ShelbiWindow::open_pool_form`. The form tasks replace that body. Until
they do, it calls `SettingsView::open_pool_form`, which opens the file the
pool is declared in (`projects/<p>/local.yaml` for an in-repo project, else
`projects/<p>.yaml`) in the user's editor with a note of what to change, and
rereads it when the window comes back to the front. No control is inert.

**Refresh.** The page rereads the config each time it's shown and on return
from the editor. A reread whose rows differ from the last read's emits
`WorkspacesEvent::Changed` → `SettingsEvent::PoolChanged`, and the window
asks its feed for a refresh now (`FeedSubscription::request`), so the
sidebar's workspace tree, counts and tags (and the workspace shown, through
the feed's usual row update) follow without waiting out the feed's timer.
A form task calls `WorkspacesPage::load` after a save to get the same.

**Project switch.** Switching projects from Settings rebuilds the settings
view for the new project on the same page (the existing `switch_to` path),
so the Workspaces page reads the new project's config. A read that lands for
another project than the page's is dropped (`WorkspacesState::loaded`).

## Decisions

- **⌘5 for Workspaces**, as the task asked: the frame gives it Terminal's ⌘3.
  The sidebar lists it first in the project section all the same, so its
  hotkeys read ⌘5, ⌘4, ⌘6 top to bottom.
- **Reachable, not Connected.** The frame's badge says "Connected". The probe
  is an SSH `true`: it says nothing about the Shelbi CLI, Git, or the
  project there, so the badge says Reachable and its tooltip says what isn't
  checked.
- **Toolkit-free state in the desktop crate**, like `workflows_state` and
  `agents_state`, rather than a new `shelbi_app::view` model: the TUI has no
  Settings pages to share it with, and no public view-model fields change,
  so `parity.md` is untouched.
- **The ⋮ menu holds Edit and Remove.** No frame draws the menu; the edit
  dialog frame (`2526:9184`) carries Remove, so the menu offers it as an
  intent for that form.
- **Interim handling opens the pool file** (with a note naming the key to
  change) rather than a "coming soon" state, as `EditWorkflow` and
  `EditAgent` did before their editors.
- **Rule and row dividers** use the Workflows and Agents pages' roles
  (`divider_strong`, `input_border`); the badge uses the `workflow_badge`
  roles, as the Agents page's Customized badge does.

## Checks

- `views::workspaces_state::tests`: declaration order, derived paths (remote
  paths never `~`-shortened), inherited tags, explicit and automatic slots,
  host fallback, counts, local-only (no probe targets), reachability per
  host (a changed host reads Checking), failed first read and reread,
  another project's read dropped, empty pool, changed-pool detection.
- `app::settings_tests`: the nav order and ⌘5 (⌘3 stays Terminal, Esc
  leaves); the rows and machines; an unreachable remote keeps its
  configuration and Settings carries on; every intent reaches the window and
  opens the pool file; an edit on disk shows on return and refreshes the
  window's feed once; the empty and broken-config states with Retry; a
  project switch reloads the page for the new project; long names and paths
  clip inside their column.
- `settings_nav` tests: every page has its own chord, label and selector.
- `--visual-figma`: `settings-workspaces-{dark,light}` (the fixture
  project's hub and gpu box), `state-workspaces-unreachable` and
  `state-workspaces-menu`; `SHELBI_VISUAL_ONLY=workspaces` renders just
  these. Compared by eye with the frame at 1440 × 1024: the columns sit at
  the frame's x positions (Machine 770, Tags 934, Slot 1226), headings,
  buttons and machine rows match; the content differs only where the frame's
  sample data does.
