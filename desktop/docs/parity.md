# TUI parity

The TUI (`crates/shelbi-tui`) and the desktop app (`desktop/`) are two clients
over one model (`crates/shelbi-app`): the same command registry, the same
view models, the same daemon. This page is the parity record: the hand walk of
every command in both clients, the differences that are platform-specific by
design, the known gaps still open, and where each client displays every
field of every `shelbi_app::view` model.

How parity is kept is in [`../README.md`](../README.md#parity-with-the-tui):
exhaustive matches with no wildcard arm, a headless scenario test per command
(`crates/shelbi-app/tests/command_scenarios.rs`), a smoke test per front end
(the TUI's `every_registry_command_runs_from_the_palette`, the desktop's
`src/app_smoke.rs`), and the field tables below, which
`crates/shelbi-app/tests/parity_doc.rs` checks against the source: a new
public field fails that test until it has a row here.

## Command walk

Walked by hand on 2026-10-14 (`dt-parity-audit`) in both clients built from
this branch, against a sandboxed Shelbi root (its own daemon and sessions, the
hub's socket variables unset): projects `pa` and `pb`, a fake orchestrator
agent, a `dev` workspace and a `review-1` slot, a task in review on a real
branch, and the `editor` setting pointed at a script that records the path it
was handed. The TUI ran in a private tmux server and was driven through its
palette (Ctrl+Space, type, Enter); the desktop app was driven through its
native menus (AX presses), with the palette checked by hand too (Ctrl+Space,
"Activity", Enter). The smoke tests run every registry command through both
paths on every `cargo test`.

| Command | Registry id | TUI | Desktop |
|---|---|---|---|
| Orchestrator | `view:orch` | Chat attaches to the orchestrator session | Same (Go ▸ Orchestrator) |
| Issues | `view:tasks` | Board | Board (Go ▸ Issues) |
| Activity | `view:activity` | Feed | Feed (Go ▸ Changelog, ⌘L, the sidebar's Changelog; also via the palette) |
| Machines | `view:machines` | Machines view, palette-only | Machines view (Go ▸ Machines) |
| Workspace | `workspace:<name>` | Idle placeholder for an idle workspace; the agent when one runs | Same (Go ▸ Workspaces ▸ dev) |
| Open review | `review:<id>` | Serving task: the review interface (panel, live server line, reviewer session) | Queued task: the "Load for review" slot dialog (Go ▸ Reviews ▸ … ▸ Open Review); a serving one opens the Review view |
| Approve | `action:approve-review:<id>` | Reached the daemon; the sandbox's gated merge refused (no `origin` to push to) and the status line said so | Reached the daemon; refused (the slot never loaded the task) and the banner said so |
| Reject | `action:reject-review:<id>` | Reason prompt; Ctrl+D sent it; the task went back to `todo` | Same (Reviews ▸ … ▸ Reject…) |
| Zen toggle | `action:toggle-zen` | First off-to-on shows the intro; confirm turned Zen on; again turned it off | Same; the View menu item relabels on/off |
| Error log | `action:error-log` | Overlay ("No errors logged.") | Same (View ▸ Error Log) |
| Settings | `action:settings` | Added after this walk (`tui-settings-shell`), covered by both smoke tests: Settings over the view, its sidebar in the sidebar's column (also `,` in the sidebar and the ⚙ beside the project title) | The existing Settings, on the page last used (Shelbi ▸ Settings…, ⌘,) |
| Switch Project | `action:switch-project` | Fixed here: was a switch to an empty project name; now the palette's Projects column, focused | The sidebar's project switcher (File ▸ Switch Project…) |
| Switch to a project | `action:switch-project:<slug>` | The client moves to `pb`, and back | The window switches to `pb` in place, and back to where `pa` was left (File ▸ Open Project ▸ pb; `dt-switch-project-in-window`) |
| New Window | `action:new-window` | Not offered: the registry lists it only for a multi-window client (`CommandModel::multi_window`); the TUI's arm says to run `shelbi` in another terminal | A second window on the current project, cascaded (File ▸ New Window, ⌘⇧N, and the palette) |
| Add project | `action:add-project` | Form with detection and the derived id; cancelled before creating (submit would launch a real agent) | The Add Project wizard's own window (`dt-add-project-wizard`): folder, then where settings live, then Create Project through the same `project_create::add_project`, its checklist, and the created workflows, whose Okay opens the project on its Orchestrator; also File ▸ Add Project…, the switcher and the chooser. The TUI keeps its one-form overlay over the same engine |
| Edit Project Settings | `edit:project` | Made real here: the editor ran on `projects/pa.yaml` with the terminal handed over (checked with `vi`), then the shell repainted | Made real here: the editor ran on the same path ("Opened pa.yaml in …") |
| Edit an agent | `edit:agent:<name>` | `agents/developer/instructions.md` | Same path |
| Edit Zen Mode | `edit:zenmode` | `zenmode.md` | Same path |
| Edit Workflows | `edit:workflows` | The `workflows/` directory | Same path |
| Detach | `action:detach` | The TUI exits; every session keeps running | The app quits; every session keeps running |
| Detach Other Clients | `action:detach-others` | Detached the desktop app ("Detached 1 other window.") | Detached the TUI |
| Quit Project | `action:quit-project` | The TUI exits and the daemon closes the project (see Known gaps for the handoff) | Confirmation sheet, then the orchestrator's handoff, its session ends; the window stays and switches to another open project, or shows the empty-state chooser; `pb` kept running |
| Quit Shelbi | `action:quit-shelbi` | Every sandbox session ended and the sandbox daemon stopped | Not pressed in this walk: the TUI's Quit Shelbi had already stopped the sandbox. The smoke test runs it through the palette and the menu; its confirmation sheet is the one Quit Project showed (`confirm_quit_shelbi`) |
| Move a card | (board) `MoveIssue` | ⇧L / ⇧H moved `backlog` ↔ `todo` | Same keys (and drag and drop) |
| Reorder a card | (board) `PrioritizeIssue` | ⇧K moved a card to the top | Same keys (and drag within a column) |
| Assign | (board) `AssignIssue` | Not on the TUI board (see Known gaps) | `a`: workspace picker; `@dev` written |
| Start | (board) `StartIssue` | Not on the TUI board | `s`: dispatched to `dev`; the sandbox agent never confirmed, so the daemon rolled it back |
| Edit | (board) `EditIssue` | Not on the TUI board | `e`: in-place editor; ⌘S saved the body |
| New issue | (board) `AddIssue` | Not on the TUI board | `n`: form; the issue was created in `backlog` |

## Platform differences (by design)

- **Drag and drop** on the board: desktop only. The TUI moves and reorders
  with the same ⇧H/⇧L/⇧K/⇧J keys the desktop also has.
- **Board search** (`dt-issues-toolbar`): desktop only. The toolbar's
  Search field (`/` focuses it, Esc clears it) narrows the cards by title,
  id and branch, on top of the workflow and workspace filters; the TUI board
  has the two filters only.
- **Native menus** with `keys.yaml` key equivalents, and **system
  notifications**: desktop only. Every menu item runs a registry command
  through the same dispatch as the palette.
- **Windows** (`dt-switch-project-in-window`): both clients switch projects
  in place: the TUI its one screen, the desktop the current window (its
  frame, size and sidebar width stay; the outgoing project's views and
  sessions are torn down and the target's sidebar skeleton paints on the next
  frame, its main view attaching after that frame). The desktop also opens
  more windows (File ▸ New Window, ⌘⇧N), each switching on its own; two may
  show one project and share its feed (`crate::feeds`), the first of them
  posting its notifications. A notification goes to a window already showing
  its project, else switches the frontmost. Window frames persist per window
  slot (`~/.shelbi/desktop-windows.json`), and a launch reopens the windows
  that were open, each on its last project (the first launch after the change
  migrates the old per-project frame to one window). Where a project was left
  (view, focus, sidebar selection and scroll, open review, board workflow
  filter) persists per project (`projects/<name>/desktop-view.json`) and is
  restored by a switch back in either window; the TUI's `ClientState`
  remembers only the view, in memory. Quitting the shown project keeps the
  window: it moves to another open project or the empty-state chooser. A
  bare "Switch Project" opens each client's switcher (the TUI's palette
  Projects column, the desktop's sidebar switcher).
- **The sidebar** (`dt-figma-sidebar`): the desktop draws Figma's
  Navigation component over the same rows (`SidebarModel::rows`, laid out by
  `views/sidebar.rs` `layout` in the shared order: Workspaces, then the review
  sections, as in the TUI), each section collapsible (remembered per project
  in `desktop-view.json`), the Activity view labeled "Changelog" (the TUI and
  the shared model keep "Activity"), fixed chords (⌘O Orchestrator, ⌘I Issues,
  ⌘L Changelog, ⌘1 to ⌘9 the working workspaces then the reviews, each
  hinted on its row and taken even from a focused terminal), a review's
  tracker number on its second line, the version line only on a mismatch (it
  is otherwise in the Settings button's tooltip), and a clickable Zen pill
  ("Zen mode on" / "off") in place of the band. Machines is in neither
  client's sidebar (the palette, and the desktop's Go menu).
- **Settings** (`tui-settings-shell`): both clients draw it in place of the
  sidebar over one model (`shelbi_app::settings`: the pages and numbers, the
  pages each client offers, the navigation, the leave prompt, the stale-load
  tickets). The desktop offers every page, under ⌘, and ⌘1 to ⌘6; the TUI
  offers a General summary of where the settings live, with the Open in
  editor paths, and Keybindings, Terminal, Workflows and Agents, under the palette,
  the sidebar's `,` and the page's digit, and adds pages as their follow-ups
  land. The TUI's switcher is the palette's Projects column, and a switch
  keeps Settings up for the new project. See [`status/tui-settings-shell.md`](status/tui-settings-shell.md).
- **Settings ▸ General** (`tui-settings-general`): both clients edit the
  shared `theme:` (Auto, Dark, Light, from `shelbi_app::settings::general`'s
  `THEMES` and labels) as an immediate-save control with the shared
  `SaveStatus` (Saving…, Saved, or why it failed). The TUI saves one narrow
  change (`general::Change`) through `update_user_config` and applies the
  theme once the write lands; the desktop applies it at once and writes its
  whole form. Auto follows the system in the desktop and is dark in the TUI
  (a terminal doesn't report its appearance). The TUI's General also edits
  `editor:` (Enter commits, clearing restores `$EDITOR` then `vim`) and keeps
  its summary of where the settings live; the desktop's General has no editor
  field and instead edits the CLI path, notifications and window effects,
  which are the desktop's own.
- **Settings ▸ Terminal**: the desktop edits its own terminals' font, size,
  Option key, scrollbar and ANSI colors (`desktop:` in `config.yaml`) and
  previews them. In the TUI the host terminal owns all of that, so its
  Terminal page explains so, shows the sixteen ANSI colors as sessions draw
  them (the host's palette) and the shared preview session
  (`shelbi_app::settings::terminal_preview`) drawn through the sessions' own
  cell mapping. It has no controls, never writes `desktop:` and never sends
  palette escape sequences to the host. See
  [`status/tui-settings-general.md`](status/tui-settings-general.md).
- **Settings ▸ Workflows** (`tui-settings-workflows`): both clients run one
  state (`shelbi_app::settings::workflows`: the edits, the one-at-a-time save
  queue, the guards, per-row errors). The TUI asks before a delete (the
  desktop's guards disable it up front and it deletes at once), reorders with
  Shift/Alt+arrows or its row menu (the desktop drags), keeps a new workflow
  selected rather than opening its editor, and draws a status as `●` in the
  board's category color. See
  [`status/tui-settings-workflows.md`](status/tui-settings-workflows.md).
- **Settings ▸ Keybindings** (`tui-settings-keybindings`): one editor state
  (`shelbi_app::settings::keybindings`: rows, search, context filter,
  capture, conflicts, the edits and their save), the same `keys.yaml`, and a
  change applies only once written. The desktop draws keycaps with hover
  buttons and a context picker; the TUI draws its display style with row
  keys (`a` add, `u` reset, Backspace remove), a stepping context control,
  and a `⚠` on chords the terminal can't send. See
  [`status/tui-settings-keybindings.md`](status/tui-settings-keybindings.md).
- **Settings ▸ Agents** (`tui-settings-agents`): both clients draw the page
  over one state (`shelbi_app::settings::agents`: the rows, the guards on
  Reset and Delete, the serialized save queue, the confirm copy, the
  preview), and both edit the shared instructions in place. A row's click
  or pencil (desktop) and Enter (TUI) open the agent details editor, and a
  new or duplicated agent opens in it in both. The desktop's ⋮ menu is the
  TUI's `m` (and the row's `⋮`); its table stacks under 72 columns. See
  [`status/tui-settings-agents.md`](status/tui-settings-agents.md).
- **Agent details** (`dt-agent-details`, `tui-agent-editor`): both editors
  run one draft (`shelbi_app::settings::agent_editor`: the changes and
  their fields, the pickers' choices, Save / Discard, the leave prompt, the
  rename / delete / reset guards, the conflict flag, what the project sets
  over the agent) and open the same files in the user's editor
  (`Leave::Open`: `agent.yaml`, `instructions.md`, a skill's `SKILL.md`).
  The desktop picks an import with the macOS chooser, the TUI types its
  path; the TUI also takes a custom model name, inspects a skill in place,
  confirms a skill's removal and shows each runner setting's value in
  effect under its picker (the desktop says it in the Runner preferences
  footnote). See [`status/tui-agent-editor.md`](status/tui-agent-editor.md).
- **Settings ▸ Workspaces** (`tui-settings-workspaces`): both clients show
  the same rows (`shelbi_app::settings::workspaces`: each workspace's name,
  derived worktree, machine and Local or SSH, own and inherited tags, and
  effective slot; each machine's host or Local, root directory, workspace
  count and SSH-only reachability) and run the same New / Edit workspace
  form and removal confirmation (`shelbi_app::settings::workspace_form`)
  through the same service (`pool_admin::workspace_service`). The desktop
  draws the forms as a 600pt dialog over the window with a toolkit text
  input; the TUI draws them as a modal box over the main area and edits each
  field at its end (type, Backspace, Ctrl+U), and adds `n` / `e` / `d` on the
  page. The TUI says the automatic slot rule in full under the field
  (`SLOT_AUTOMATIC_HELP`); the desktop shows its number in the placeholder
  only. Add machine and Configure open the machine form in both
  (`tui-settings-machine-forms`): the same draft
  (`shelbi_app::settings::machine_form`) saved through the same service
  (`pool_admin::machine_service`). Both run the same connection check
  (`tui-settings-machine-readiness`: `shelbi_app::settings::machine_check`
  over `machine_readiness::service`): Test connection / Test again, Install
  CLI when it can help, the same panel titles and messages, the same
  binding of a result to the draft's host and directory, and the same
  records. The desktop adds Choose… for this Mac's directory and shows the
  check as the Figma region (icon, title, detail); the TUI types the
  directory as a terminal line, adds `m` / `c` on the page, lists the
  check's stages (`ReadinessForm::stages`: SSH, Platform, Shelbi CLI, Git,
  Project directory, each with its result) with the elapsed time while it
  runs, offers Stop check (the desktop's Test button is disabled while a
  check runs, and a check stops when its dialog closes), and says on the
  page what an install did to the machine when the form it ran for has
  closed. Both keep the status probe's last word on the saved host labeled
  as the probe's. See
  [`status/tui-settings-workspaces.md`](status/tui-settings-workspaces.md),
  [`status/tui-settings-machine-forms.md`](status/tui-settings-machine-forms.md)
  and [`status/tui-settings-machine-readiness.md`](status/tui-settings-machine-readiness.md).
- **The workflow editor** (`tui-workflow-editor-form`): both clients edit
  one draft (`shelbi_app::settings::workflow_editor`: the changes, the text
  form, Save, Discard, the leave prompt, the guards). The TUI's Form
  reaches a row's cells through its keys or menu (the desktop has a picker
  per cell and drags), lists Run commands one per row, asks before Delete,
  asks Reload or Keep editing on an on-disk conflict (the desktop points at
  Discard), and asks before a project switch. See
  [`status/tui-workflow-editor-form.md`](status/tui-workflow-editor-form.md).
- **The workflow Builder** (`tui-workflow-editor-builder`): both clients
  draw the same graph (`shelbi_app::settings::workflow_graph`'s columns and
  rows) and select, draw and step back the same way
  (`settings::workflow_builder`). The TUI's editor **opens on the Form**
  (`v` or the header's Builder | Form for the Builder) where the desktop's
  opens on the Builder: in a terminal the rows are the more usable start.
  The TUI lays the graph out in cells (no zoom or Fit; Shift+arrows, Home
  and the wheel scroll), reaches transitions with `]` / `[`, uses Tab for
  header / canvas / panel (the desktop's Tab walks nodes), edits a
  transition's Details in its panel too, reorders actions with
  Shift/Alt+↑↓ (no drag), and puts the panel in the canvas's place below
  104 columns (← Graph goes back). See
  [`status/tui-workflow-editor-builder.md`](status/tui-workflow-editor-builder.md).
- **Editors**: the TUI hands its terminal to the editor and waits; the desktop
  opens a terminal editor in Terminal.app and a GUI editor directly. Both
  open the same path (`shelbi_app::command_model::edit_target_path`) with the
  same `editor` setting.
- **Quit confirmation**: the desktop confirms Quit Project and Quit Shelbi
  with a sheet (⌘Q only closes the client); the TUI runs them from the
  palette directly, as it always has.
- **Color**: the desktop has its own palette, after the Figma design
  system ([`theme.md`](theme.md)), in dark and light; the TUI keeps the
  shared tokens (`shelbi_app::theme`) and its colors are unchanged. Colors
  that carry meaning in both clients (a workspace's state glyph, a review's
  sub-state, the Zen band, the activity feed's tones) map onto the Figma
  color for the same meaning (`Theme::tone`, `Theme::decoration`). The TUI's
  other color-only cues (a column header in its category color, a task
  status colored by category, a dependency in green or red) have no color on
  the desktop; where the color carries information, the desktop says it
  another way (a done dependency gets a `✓`).
- **The first-run hint** ("Ctrl+P palette · type E to edit settings") is the
  TUI's onboarding copy in `SidebarModel.status_line`; it is claimed once per
  machine, so the desktop leaves it to the TUI and shows its palette chord in
  the sidebar's search box instead.
- **Links**: the desktop's review URLs are clickable; the TUI prints them.
- **The review panel takes the sidebar's place** in both clients: with a
  review open the window is two columns, the panel then the session. The TUI
  swaps them in place. The desktop pushes between them like two levels of a
  navigation stack (`views/column_swap.rs`): going in, the sidebar slides out
  to the left and fades while the panel slides in from the right; Back,
  Close, a decision landing, or the task leaving review play the reverse. It
  takes about 220ms, ease-out, clipped to the column, which keeps its width
  so the session never moves; a second toggle mid-way turns it around from
  where it stands. Under macOS Reduce Motion it is a crossfade with no
  slide. The panel uses the user's sidebar width and the divider beside it
  resizes it; with the sidebar collapsed it still opens in that column, at
  the default width, and Back returns to the collapsed state. Leaving gives
  the sidebar back with its selection, scroll and keyboard focus where they
  were.
- **The workspace panel takes the sidebar's place** in both clients, as the
  review panel does, and both navigate it by the same rows and keys
  (`shelbi_app::workspace_panel`: Back, More, the folder, the view switch;
  ↑/↓ or j/k, Enter, `m`, Esc for Back, `q` to close the diff and editor and
  go back, Tab to the agent). The desktop's (`views/workspace.rs`
  `render_panel`, the Navigation component's State=Workspace,
  `dt-figma-task-panels`) pushes in with the review's motion, keeps the
  sidebar's titlebar and footer around it, and differs in what it shows:
  - **An idle workspace keeps the sidebar** and shows its page in the main
    area (the state, the name, where it lives, a Shell button), as the TUI
    keeps its sidebar for one.
  - **The status is the task's column** (IN PROGRESS), as in the TUI; the
    workspace name, its live state, the task's id and branch, and the
    machine are not in the panel (the sidebar row carries the live state).
  - **The description clamps to five lines** and More opens the task
    detail (the board's modal) over the screen, which stays as it is under
    it (`dt-more-opens-detail`); the TUI's More opens a description
    popover.
  - **The worktree row has an Open button** that reveals it in Finder.
  - **The views carry ⌘1 to ⌘N hints**, and those chords pick them while
    the panel holds the column (the sidebar's rows otherwise).
  - **A Shell row** opens the user shell beside the agent (the TUI has no
    user shell), and the active Diff or Editor row has a × that ends those
    sessions without leaving.
  - **A remote workspace** keeps the agent and Shell rows and shows its
    worktree path without Open, its machine in the path's tooltip; the TUI
    opens remote workspaces with the same panel as local ones.
  - **Back returns to the view the workspace was opened over**, as the TUI's
    does; going from a workspace to a review (or back) pops one panel to the
    sidebar and pushes the other.
  There is no header above the agent's terminal any more: it starts at the
  top of the main area.
- **Dropping files and images on a terminal pane** pastes the escaped paths
  followed by a space, in both clients, so Claude Code attaches a dropped
  image. The TUI gets that paste from its own terminal (Ghostty and
  Terminal.app turn a Finder file or a screenshot thumbnail into its path).
  The desktop app builds it itself (`shelbi_term::input::dropped_paths_text`):
  a Finder drop through gpui, and a screenshot thumbnail, browser, Preview or
  other image-data or file-promise drop through `src/drops/`, which first
  writes it to a file (`~/Library/Caches/<bundle id>/drops/`, PNG for raw
  image data) and pastes that path. Every terminal pane takes drops (chat,
  workspace agent and user shell, review).

## Known gaps

Found by this audit and not closed by it; each is a follow-up.

- **The TUI board has no Start, Assign, Edit, or New issue.** The desktop
  board does (`s`, `a`, `e`, `n`). The TUI's users do these through the
  orchestrator or `shelbi issue`. The commands exist in the registry; the TUI
  needs the board UI for them.
- **The TUI board sends no expected state** with a move or reorder, so it has
  no protection against overwriting a change another client just made; the
  desktop sends `(status, revision)` on every mutation.
- **Neither workspace panel shows the live state** (both read IN PROGRESS
  for a workspace waiting for input); the sidebar row shows it.
  `WorkspaceHeader` is unused by the TUI.
- **The TUI review panel** shows the live server state and location (added
  here) but not the health check or the sub-state detail, and always shows
  Diff and Editor (it has no remote flag for a review slot). The desktop's
  (State=Review, `dt-figma-task-panels`) has no server block: the Web
  Browser row stays inert with "loading…" until the slot serves, the
  sub-state, detail and health check in its tooltip, and the URL in its
  tooltip once serving. It also offers a user shell in the slot's worktree
  (Shell ⌘5), which the TUI does not.
- **Board details shown in one client only**: the desktop shows a card's
  excerpt, workflow badge (even on a one-workflow board) and dependencies on
  the card, "Loading history…" in the done
  column, and the task's workflow and PR link in the detail; the TUI shows
  a board total ("N total") and resolves the detail's status and branch
  differently (stored column and raw branch, against the desktop's resolved
  status and templated branch).
- **Daemon (not a client gap, found during the walk):** quitting the last
  open project from the TUI marked it closed, but the sandbox daemon exited
  during the orchestrator handoff wait and left the orchestrator session
  running. Quit Shelbi cleaned it up. The daemon's idle exit should wait for
  an in-flight quit.

## View-model fields

One table per `shelbi_app::view` model (`crates/shelbi-app/src/view.rs`, plus
`ReviewPanelModel`, which it re-exports from `review.rs`). Cells name where the
field is displayed, as a place in the UI plus the file and function (TUI paths
relative to `crates/shelbi-tui/src/`, desktop paths relative to
`desktop/src/`). "not shown" says what the field is for instead. When a client
renders from its own type rather than the shared model, the cell names the
equivalent datum.

### `SidebarModel`
| Field | TUI | Desktop |
|---|---|---|
| `project_label` | sidebar header title (`shell/sidebar.rs` `render_header`) | sidebar header title (`views/sidebar.rs` `render_header`); window title, and so the Window menu's list (`app.rs` `set_window_title`, at window build and on every switch); "Quit X?" prompt (`app.rs` `confirm_quit_project`); notification bodies (`notify.rs` `observe_sidebar`, `observe_orchestrator`) |
| `nav` | nav block rows, glyph picked from the label (`shell/sidebar.rs` `render_nav`, `nav_glyph`); palette view entries | nav rows, Heroicon picked from the view, with their ⌘O / ⌘I / ⌘L hints (`views/sidebar.rs` `render_nav_row`, `nav_icon`); palette view entries; Go menu with the same chords (`menus/tree.rs`) |
| `workspaces` | Workspaces section rows (`shell/sidebar.rs` `render_row`); palette workspace entries | Workspaces section rows, above the review sections, collapsible (`views/sidebar.rs` `layout`, `render_row`); palette; Go > Workspaces menu (`menus/tree.rs`); blocked-workspace notifications (`notify.rs` `observe_sidebar`) |
| `reviews` | Ready / Queued for Review sections (`shell/sidebar.rs` `render_row`); palette review + approve/reject entries | Ready / Queued for review sections with their count badges, collapsible, below Workspaces (`views/sidebar.rs` `layout`, `render_row`); palette; Go > Reviews menu (`menus/tree.rs`); "Ready for review" notifications (`notify.rs` `observe_sidebar`); review view live server state (`app.rs` `update_review_row`) |
| `config_error` | red wrapped `! message` row under Workspaces (`shell/sidebar.rs` `render_row`, `config_error_lines`) | red `! message` row under Workspaces (`views/sidebar.rs` `render_row`); also gates review-closed reconcile (`app.rs` `update_review_row`) |
| `board_loading` | italic "Loading…" placeholder row (`shell/sidebar.rs` `render_row`) | italic "Loading…" placeholder row (`views/sidebar.rs` `render_row`); also suppresses notifications on a cold skeleton (`notify.rs` `observe_sidebar`) and the review-closed reconcile (`app.rs` `update_review_row`) |
| `collapsed_machines` | machine group `▸`/`▾` glyph, `(N, M active)` suffix, hidden rows (via `rows()`, `shell/sidebar.rs` `render_row`); toggled in `shell/mod.rs` | same glyph, suffix and hidden rows (via `rows()`, `views/sidebar.rs` `row_text`); toggled in `app.rs` |
| `board_banner` | not shown in the sidebar; the board's status is the Issues title row's (see the board model's `banner`) | footer notes above the buttons, only when stale or cached with no daemon, in yellow; a warm board's age (`board 12s`) is dropped as a refresh timer (`views/sidebar.rs` `board_warning`, `render_footer`) |
| `daemon_version_line` | the Settings button's hover name beside the toolbar; above the toolbar only on a mismatch (`shell/sidebar.rs` `FooterButton::tooltip`, `render_footer`) | the Settings button's tooltip; in the footer notes only on a mismatch (`views/sidebar.rs` `render_footer`) |
| `daemon_version_mismatch` | shows the version line, red, above the toolbar, the fix (`shelbi daemon restart`) on its own line (`shell/sidebar.rs` `render_footer`) | shows the version line, red, in the footer notes (`views/sidebar.rs` `render_footer`) |
| `status_line` | footer status row, muted for the first-run hint, else yellow (`shell/sidebar.rs` `render_footer`); stamped from the startup status in `shell/mod.rs` `apply_snapshot` | not shown: the footer would draw it, but the desktop never stamps it (the first-run hint is TUI onboarding copy; see Platform differences) |
| `zen_mode` | green "ZEN MODE ON" band (`shell/sidebar.rs` `render_zen_row`); palette Zen toggle label | the footer's Zen pill, green "Zen mode on" or neutral "Zen mode off", a click toggles Zen (`views/sidebar.rs` `render_zen_pill`); palette Zen toggle label; Zen menu item title (`menus/tree.rs`) |
| `unread_errors` | red `!` button in the footer when > 0 (`shell/sidebar.rs` `render_footer`, `render_error_button`) | red `!` button in the footer when > 0 (`views/sidebar.rs` `render_footer`) |
| `session_lost` | full-width red top banner in every view (`shell/mod.rs` `render_session_lost_banner`) | red notice at the top of the main column (`app.rs` render, `notice`) |
| `orchestrator_status` | red `Orchestrator: <status>` footer status row, 3 rows reserved so it wraps, taking the row over `status_line` (`shell/sidebar.rs` `render_footer`, text from `SidebarModel::footer_status`); Orchestrator view placeholder "Orchestrator is down. <status>. Select Orchestrator again to relaunch it." while its session is not live (`shell/mod.rs` `orchestrator_down_notice`) | red `Orchestrator: <status>` first of the footer notes, above the buttons (`views/sidebar.rs` `render_footer`, `footer_status`); the same placeholder in the Chat view while the orchestrator is not live (`views/session_view.rs` `notice`, fed by `app.rs` `update_chat_supervisor_status`) |

### `NavItem`
| Field | TUI | Desktop |
|---|---|---|
| `label` | nav row text, and it picks the glyph (`shell/sidebar.rs` `nav_item_line`, `nav_glyph`); palette entry title | nav row text, "Changelog" for the Activity view (`views/sidebar.rs` `render_nav_row`, `nav_label`); palette entry title; Go menu item (also through `nav_label`) |
| `view` | not shown: routing only, mapped to a session or native view (`shell/sidebar.rs` `target_from_view`) | picks the nav row's icon (`views/sidebar.rs` `nav_icon`); routing via `SidebarRow::target` |

### `WorkspaceRow`
| Field | TUI | Desktop |
|---|---|---|
| `name` | row label (`shell/sidebar.rs` `render_row`); palette entry title | row label (`views/sidebar.rs` `row_text`); palette; Go menu; workspace panel name in bold (`views/workspace.rs` `render_panel`); notification titles (`notify.rs`) |
| `machine` | machine group header text when the project spans several machines (via `rows()`, `shell/sidebar.rs` `render_row`) | machine group header (via `rows()`, `views/sidebar.rs` `row_text`); panel machine row (`views/workspace.rs` `render_panel`); remote attach target (`views/workspace.rs` `set_row`) |
| `is_remote` | not used (remote attach comes from session discovery, `shell/session.rs`) | panel machine row with a globe icon and `(remote)` (`views/workspace.rs` `render_panel`); routes the session through the remote relay and drops the Diff, Editor and folder rows (`views/workspace.rs` `set_row`, `panel_shape`) |
| `current_task` | not shown as text in the sidebar; counts toward `M active` on a collapsed group (via `rows()`); palette workspace subtitle; gates opening the workspace panel (`shell/mod.rs` `workspace_has_task`) and the panel's IN PROGRESS/IDLE status (`shell/mod.rs` `ws_status_from_model`) | `M active` count (via `rows()`); palette subtitle; panel task id and the full panel vs the reduced idle one (`views/workspace.rs` `render_panel`, `panel_shape` via `has_task`); notification body (`notify.rs`) |
| `agent` | right-aligned row label, title-cased, or `idle` (`right_label`, `shell/sidebar.rs` `render_row`) | right-aligned row label, X-Small semibold, then the row's ⌘N; "Idle", muted, without one (`views/sidebar.rs` `row_text`); panel `· Agent` role after the name and the agent switch row's label (`views/workspace.rs` `render_panel`) |
| `badge` | colored state glyph in the row (`shell/sidebar.rs` `render_row`) | the Navigation component's state glyph (green play, yellow `?`, quiet dot; the shared glyph for the states it doesn't draw) (`views/sidebar.rs` `workspace_glyph`); panel status glyph plus words beside the back button, e.g. `⏵ Working` (`views/workspace.rs` `render_panel`); drives blocked/needs-permission notifications (`notify.rs` `observe_sidebar`) |

### `ReviewRow`
| Field | TUI | Desktop |
|---|---|---|
| `task_id` | not shown: routing key for opening the review (`shell/sidebar.rs` `build_rows`, `RowTarget::Review`); palette command id | not shown in the row: routing key (via `SidebarRow::target`) and debug selector (`views/sidebar.rs` `row_selector`); shown in the "reached review" notification body (`notify.rs`) |
| `title` | row first line, truncated with `…` (`shell/sidebar.rs` `render_row`); palette entry and Approve/Reject titles | row first line (`views/sidebar.rs` `row_text`); palette; Go > Reviews menu; notification title (`notify.rs`) |
| `branch` | dim `⎇ branch` second line (`shell/sidebar.rs` `render_row`) | muted second line after the number (`views/sidebar.rs` `row_text`, `render_row`) |
| `location` | right-aligned `machine:port` on the first line (`shell/sidebar.rs` `render_row`) | not shown: State=Review dropped the server block and the sidebar row's right side is its ⌘N hint (the Navigation component); the review URL stands in (the Web Browser row's tooltip) |
| `workspace` | not used | not used |
| `number` | not shown | `#401` before the branch on the row's second line (`views/sidebar.rs` `row_text`); filled from the board index for a GitHub-backed project (`shelbi-orchestrator` `sidebar_model`) |
| `state` | section split (Ready vs Queued, via `rows()`) and the badge glyph, bold cyan ✓ when serving (`shell/sidebar.rs` `render_row`) | section split (via `rows()`) and the component's glyph: the cyan check when serving, a quiet dot when queued (`views/sidebar.rs` `review_glyph`); the review panel's Web Browser row, inert with "loading…" until serving, the sub-state and detail in its tooltip (`views/review.rs` `render_waiting_browser`) |

### `WorkspaceDetailsInfo`
The TUI never constructs this struct; its workspace panel reads the same facts from `workspace_session::workspace_open_info` into its own `WorkspacePanel` (`shell/mod.rs` `build_workspace_interface`). The TUI column describes that equivalent display.

| Field | TUI | Desktop |
|---|---|---|
| `task` | struct not used; the task title shows bold in the panel (`workspace_panel.rs` `render_full`, `render_task_info`); id only in the More popover meta (`kanban.rs` `render_task_popover_into`) | panel title, wrapped (`views/workspace.rs` `render_panel` via `WorkspaceHeader`); the id only as the title's fallback |
| `body` | struct not used; inline preview lines plus the More popover (`workspace_panel.rs` `render_task_info`; `shell/overlays.rs` `TaskDescription`) | description clamped to five lines, only when non-empty, More opening the task detail over the screen (`views/workspace.rs` `render_panel`, via `shelbi_app::review::description_text`; the window draws the board's detail, `app.rs` `open_task_over`) |
| `branch` | struct not used; the task's branch only in the More popover meta (`kanban.rs` `render_task_popover_into`); no worktree-branch fallback | the idle page's place line when there is no worktree (`views/workspace.rs` `render_toolbar` via `WorkspaceHeader::location_line`); not in the panel |
| `worktree` | struct not used; worktree path shown as the folder row, click reveals (`workspace_panel.rs` `render_folder`) | panel worktree row, its last four components (`…/shelbi/.shelbi/wt/alpha`), Open reveals it in Finder when local (`views/workspace.rs` `render_panel`, `task_panel::worktree_row`, `reveal_folder`); the idle page's place line |
| `editor_name` | struct not used; "Edit in X" switch label (`workspace_panel.rs` `switch_entry`) | "Edit in X" switch row label (`views/workspace.rs` `render_panel`) |

### `WorkspaceHeader`
The TUI does not use `WorkspaceHeader` at all; its workspace panel header is the back button plus a column status (`IN PROGRESS` / `IDLE`), the title, a preview, the folder row and the switcher.

| Field | TUI | Desktop |
|---|---|---|
| `name` | not used; the workspace panel never shows the workspace name | the idle page's toolbar (`views/workspace.rs` `render_toolbar`); not in the panel (State=Workspace draws none) |
| `task_id` | not used; id only in the More popover | not shown: the panel title's fallback when the title is empty (`views/workspace.rs` `render_panel`); gates the status and summary |
| `title` | not used; the panel shows the task title from its own `IssueFile` (`workspace_panel.rs` `render_task_info`) | panel title, wrapped (`views/workspace.rs` `render_panel`) |
| `branch` | not used; branch only in the More popover | the idle page's place line without a worktree (`views/workspace.rs` `render_toolbar` via `location_line`) |
| `machine` | not used | the idle page's place line (`views/workspace.rs` `render_toolbar`); a remote panel's worktree tooltip (`render_panel`) |
| `is_remote` | not used | `(remote)` on the idle page's place line (`views/workspace.rs` `render_toolbar`); the worktree tooltip names the machine (`render_panel`) |
| `agent` | not used; Agent switch label comes from the task's `agent:` frontmatter (`shell/mod.rs` `agent_display_name`) | the agent view's label, `Developer` (`views/workspace.rs` `render_panel`) |
| `state` | not used; panel status is the task column (`IN PROGRESS`/`IDLE`, `shell/mod.rs` `status_for_column`, `ws_status_from_model`), not the live badge | the idle page's state, glyph and lowercase words (`views/workspace.rs` `render_toolbar` via `state_decoration`); the panel shows the task's column (IN PROGRESS), as the TUI's |

#

### `IssuesModel`
| Field | TUI | Desktop |
|---|---|---|
| `columns` | equivalent `app.all_columns` + `column_tasks()`, drawn as lanes (`kanban.rs` `render_columns`) | lanes (`views/issues.rs` `render_board`) |
| `title` | equivalent `display_label()` (display_name else slug) in title row "Issues · name" (`kanban.rs` `render_title`) | not shown: the Figma Issues frame has no board title (the sidebar's project switcher names the project) |
| `loading` | equivalent `board_loading`: centered "Loading board…" overlay while no tasks (`kanban.rs` `render_full` / `render_loading_overlay`) | "Loading board…" row in each empty column (`views/issues.rs` `render_column`) |
| `banner` | equivalent `board_banner` in title row, muted while warm, yellow when stale or cached with no daemon (`kanban.rs` `render_title`) | muted text in header (`views/issues.rs` `render_header`) |
| `error` | equivalent: refresh error written to `status_line` (`kanban.rs` `fail_refresh`), shown in footer (`render_footer`) | footer status when no newer status (`views/issues.rs` `render_footer`) |
| `done_loading` | not shown: no equivalent; done column shows "(empty)" until the closed page lands | "Loading history…" row in done column (`views/issues.rs` `render_column`) |
| `has_more_done` | equivalent `has_more_done()`: "↓ load more (m)" row in done column (`kanban.rs` `render_column`) | "Load more (m)" clickable row in done column (`views/issues.rs` `render_column`) |
| `show_workflow_badges` | equivalent computed locally via `show_card_workflow_label`, gates card badge (`kanban.rs` `render_columns` / `card_meta_line`) | not shown: the desktop card always carries its workflow badge, as the Figma card draws it (`views/issues.rs` `card_meta`) |
| `workflow_filter_label` | equivalent own text " Workflow: All ▾" chip, cyan when set, hidden unless more than one workflow (`kanban.rs` `workflow_chip_text` / `render_title`) | the workflow chip's label while a workflow filter is set, in the Button's Select style; unfiltered the chip reads `Workflows` (the Figma Header), always shown (`views/issues.rs` `render_header`) |
| `workspace_filter_label` | equivalent own text " Workspace: All ▾" chip, cyan when set (`kanban.rs` `filter_chip_text` / `render_title`) | the workspace chip's label while a workspace filter is set, in the Select style; unfiltered the chip reads `Workspaces` (`views/issues.rs` `render_header`) |

### `ColumnDef`
| Field | TUI | Desktop |
|---|---|---|
| `status_id` | not shown: bucketing key, move target, collapse override key (`kanban.rs` `task_belongs_to`, `override_key_for`) | not shown: desktop renders `IssueColumn`; `ColumnDef` only used for move targets (`views/board_state.rs` `move_target`) |
| `status_name` | header label via `column_label` (`kanban.rs` `render_column`, `render_collapsed_column`) | not shown directly (header uses `IssueColumn.label`, built from it) |
| `category` | header color via `category_color`; done sort + load-more row (`kanban.rs` `render_column`) | not shown: shared board logic only (`board.rs` `sort_column_cards`) |

### `IssueColumn`
| Field | TUI | Desktop |
|---|---|---|
| `status_id` | n/a (uses `ColumnDef.status_id`, see above) | not shown: toggle/drop/move key (`views/board_state.rs` `toggle_column`, `drop_target`) |
| `status_name` | n/a (equivalent `ColumnDef.status_name` is the header source) | not shown: not used (header uses `label`) |
| `label` | equivalent `column_label(status_name)` header, rotated in collapsed strip (`kanban.rs` `render_column`, `render_collapsed_column`) | column header; letters of collapsed strip (`views/issues.rs` `render_column`, `render_collapsed`) |
| `category` | equivalent `ColumnDef.category`: header color (`kanban.rs` `category_color`) | the header's status glyph and its color (`theme/style.rs` `status_icon`, `status_heading`); also picks the done column for history/load-more and done-insertion order (`views/issues.rs` `render_column`, `render_collapsed`, `views/board_state.rs`) |
| `collapsed` | equivalent `is_column_collapsed()` picks the collapsed strip (`kanban.rs` `render_columns`) | picks the collapsed strip (`views/issues.rs` `render_board`) |
| `cards` | equivalent `column_tasks()`: cards + "(N)" count (`kanban.rs` `render_column`) | cards + count, "(empty)" (`views/issues.rs` `render_column`) |

### `IssueCard`
| Field | TUI | Desktop |
|---|---|---|
| `id` | not shown on card (popover/hit key; shown in popover) | not shown on card: identity/key; shown in drag ghost (`views/issues.rs` `CardGhost`) |
| `title` | card title, wraps to two lines (`kanban.rs` `render_column`) | card title, two-line clamp (`views/issues.rs` `render_card`) |
| `summary` | not shown: the TUI card has no room for an excerpt; the body shows in the popover | excerpt under the title, four-line clamp (`views/issues.rs` `render_card`) |
| `blocked` | equivalent `task.is_blocked`: lock prefix + dim title (`kanban.rs` `render_column`) | lock icon + muted title; "blocked by" deps label (`views/issues.rs` `render_card`, `card_meta`) |
| `branch` | equivalent `card_branch()` "⎇ branch" on meta line (`kanban.rs` `card_meta_line`) | branch after the workflow badge, muted (`views/issues.rs` `card_meta`) |
| `workflow` | not shown: raw `task.workflow` only feeds workflow-name resolution | not shown: not used anywhere |
| `workflow_name` | equivalent `task_workflow_name()` badge (`kanban.rs` `card_meta_line`) | workflow badge on every card (`views/issues.rs` `card_meta`, `workflow_badge`) |
| `assignee` | not shown on card: `assigned_to` drives workspace filter and the footer "workspace" hint (`kanban.rs` `render_footer`) | not shown on card (the Figma card has no assignee): detail's workspace row (`views/task_detail.rs` `render`); assign picker mark |
| `tracker_owner` | remote tracker only: a card that isn't the user's leads its meta line with `@login` / `unassigned` in yellow (`kanban.rs` `card_meta_line`); the TUI board has no Start to withhold | remote tracker only: a card that isn't the user's leads its meta line with `@login` / `unassigned` (`views/issues.rs` `card_meta`); `s` shows the `--take` hint instead of starting (`views/board_state.rs` `start`, via `IssueCard::start_refusal`) |
| `priority` | not shown: ordering and reorder slots only | not shown: ordering only (sort in `board.rs`) |
| `depends_on` | not shown on card (popover only) | "depends on / blocked by ids" meta line (`views/issues.rs` `card_meta`) |
| `status_id` | n/a (bucketing via `resolved_status_id`) | not shown: written on optimistic move only (`views/board_state.rs` `apply_open`) |
| `stored_status` | n/a: TUI moves send no expected state (`kanban.rs` `ExecutorMovePersister`) | not shown: mutation expected state (`IssueCard::expected`, `views/board_state.rs`) |
| `updated_at` | n/a (raw `task.updated_at` used for done ordering only) | not shown: mutation expected state + revised check (`views/board_state.rs`) |

### `DepBadge`
| Field | TUI | Desktop |
|---|---|---|
| `id` | equivalent dep id in popover deps row (`kanban.rs` `popover_header`) | card meta deps list (`views/issues.rs` `card_meta`); detail deps row (`views/task_detail.rs` `render`) |
| `status` | equivalent `columns.get(dep)` as "[status]" or "[missing]" in popover (`kanban.rs` `popover_header`) | "[status]" or "[missing]" in detail (`views/task_detail.rs` `render`); not on card |
| `done` | equivalent: dep text colored by its column category, green when done, red when missing (`kanban.rs` `popover_header`) | `✓` after a done dependency in the detail, monochrome (`views/task_detail.rs` `dep_text`) |

### `BoardFilters`
| Field | TUI | Desktop |
|---|---|---|
| `workflow` | equivalent `app.workflow_filter`: narrows columns/cards, chip text + cyan highlight (`kanban.rs` `task_belongs_to`, `workflow_chip_text`) | narrows model in `IssuesModel::build`; surfaces as chip label and dropdown cursor (`views/board_state.rs`) |
| `workspace` | equivalent `app.workspace_filter`: filters cards, chip text + cyan highlight (`kanban.rs` `task_matches_filter`, `filter_chip_text`) | filters model in `IssuesModel::build`; chip label, dropdown cursor, persisted (`views/board_state.rs`) |
| `search` | not shown: the TUI board has no text search (desktop only, see Platform differences) | the toolbar's Search field; narrows cards in `IssuesModel::build` by title, id and branch (`board.rs` `card_matches_search`), `/` focuses it, Esc clears it (`views/issues.rs` `render_search`, `views/board_state.rs` `set_search`) |

### `TaskDetailModel`
| Field | TUI | Desktop |
|---|---|---|
| `id` | equivalent `task.id` "id:" row (`kanban.rs` `popover_header`) | "id" field (`views/task_detail.rs` `render`) |
| `title` | equivalent `task.title` in popover border title (`kanban.rs` `render_task_popover_into`) | panel header (`views/task_detail.rs` `render`) |
| `status_id` | not shown (popover keyed by task id) | not shown: not used |
| `status_label` | equivalent but different source: `column_label(task.column.default_status_name())`, stored column not resolved status (`kanban.rs` `popover_header`) | not shown: `status_name` instead (the Figma frame spells the status as the workflow does) |
| `status_name` | not shown: the popover shows the column label (`status_label`) | "status" field (`views/task_detail.rs` `render`) |
| `category` | equivalent: status text colored by `column_color(task.column)` (`kanban.rs` `popover_header`) | not shown: not used |
| `priority` | "priority:" on column row (`kanban.rs` `popover_header`) | "priority N" in status field (`views/task_detail.rs` `render`) |
| `assignee` | "workspace: @ws" when set; footer workspace hint (`kanban.rs` `popover_header`, `render_task_popover_into`) | "workspace" field, "unassigned" when none (`views/task_detail.rs` `render`) |
| `tracker_owner` | equivalent `OwnershipGate::owner(tracker_assignees)`: "owner:" row, `me` or `@login` plus the `--take` hint, remote tracker only; not in the review popover (`kanban.rs` `popover_header`) | "owner" field, `me` or `@login` plus the `--take` hint, remote tracker only; hides Start when not the user's (`views/task_detail.rs` `render`, `TaskDetailModel::startable`) |
| `branch` | equivalent but raw `task.branch` only, no templated base branch (`kanban.rs` `popover_header`) | "branch" field, resolved `card_branch` (`views/task_detail.rs` `render`) |
| `workflow_name` | not shown | "workflow" field (`views/task_detail.rs` `render`) |
| `depends_on` | deps row with "[status]" (`kanban.rs` `popover_header`) | deps field with "[status]" (`views/task_detail.rs` `render`) |
| `blocked` | "blocked by" vs "depends on" label (`kanban.rs` `popover_header`) | same label choice (`views/task_detail.rs` `render`) |
| `pr_link` | not shown | clickable "PR" link (`views/task_detail.rs` `render`) |
| `created` | "created:" row (`kanban.rs` `popover_header`) | "created" field (`views/task_detail.rs` `render`) |
| `updated` | "updated:" on created row (`kanban.rs` `popover_header`) | "updated" in created field (`views/task_detail.rs` `render`) |
| `body` | markdown body, scrollable (`kanban.rs` `render_task_popover_into` via `markdown::render_note`) | markdown `TextView`, "_No description._" fallback, checklist count, in-place editor (`views/task_detail.rs` `render`) |

### `MachinesModel`
| Field | TUI | Desktop |
|---|---|---|
| `display_name` | title bar after "Machines", falls back to project slug (`machines.rs` `display_label` / `render_title`) | header after "Machines", falls back to slug (`views/machines_state.rs` `title`, `views/machines.rs` `render`) |
| `machines` | one header line plus workspace lines per machine; "No machines declared" when empty (`machines.rs` `render_body`) | one group per machine; same empty text; footer also shows a total "N workspaces" count via `workspace_count` (`views/machines.rs` `render_body` / `render`) |

### `MachineRow`
| Field | TUI | Desktop |
|---|---|---|
| `name` | bold machine heading (`machines.rs` `machine_header_line`) | machine heading, Base Medium, loud (`views/machines.rs` `machine_header`) |
| `kind` | not shown: `is_local` is used instead | not shown: `is_local` is used instead |
| `host` | `ssh <host>` location label (`machines.rs` `machine_header_line`); also feeds prober targets (`probe_targets`) | `ssh <host>` location label (`views/machines.rs` `machine_header`); also feeds prober targets |
| `is_local` | "local" vs `ssh <host>`; gates reachability and binary badges (`machines.rs` `machine_header_line`) | same (`views/machines.rs` `machine_header`) |
| `tags` | `[a, b]` badge in workflow-badge color (`machines.rs` `machine_header_line`) | `[a, b]` in the workflow badge's text color on the line under the heading (`views/machines.rs` `machine_metadata`) |
| `remote` | remote binary `path vX` / incompatible / "not set up" badge, remotes only (`machines.rs` `machine_header_line`) | same text on the line under the heading, muted when resolved, remotes only (`views/machines.rs` `machine_metadata`) |
| `reachability` | reachable / checking / unreachable badge, remotes only, error truncated at 40 (`machines.rs` `reachability_span`) | same label after the ssh host, remotes only, error truncated at 80 with the whole reason on hover (`views/machines.rs` `machine_header`) |
| `workspaces` | one line per workspace, "(no workspaces)" when empty (`machines.rs` `render_body`) | one clickable row per workspace, "(no workspaces)" when empty (`views/machines.rs` `render_body`) |

### `MachineWorkspace`
| Field | TUI | Desktop |
|---|---|---|
| `name` | 18-char name column; also what Enter opens (`machines.rs` `workspace_line`, `selected_workspace`) | 150px name column; also what click/Enter focuses (`views/machines.rs` `workspace_row`, `views/machines_state.rs` `focus_selected`) |
| `is_review_slot` | not shown: never read by the view | not shown: never read by the view (review rows are recognized via `review` / `assignment`) |
| `state` | observed-state badge (`machines.rs` `workspace_line`) | 76px observed-state column; a serving review slot reads `review` (the Figma frame) (`views/machines.rs` `state_cell`) |
| `current_task` | not shown: the task cell uses `assignment`, as on the desktop (`dt-parity-audit`) | not shown: the task cell uses `assignment` instead |
| `assignment` | `in_progress: <ids>` / `review: <id>` / "idle" cell (`machines.rs` `workspace_line`) | `<status>: <title>` via `MachineWorkspace::task_cell`, the ids cell on hover (`views/machines.rs` `workspace_row`) |
| `agent` | agent column, `-` when idle (`machines.rs` `workspace_line`) | not shown: the Figma frame's third column is the runner (`dt-figma-machines`) |
| `runner` | not shown: the TUI keeps its agent column | 96px runner column (`Codex`, `Claude`), `—` when idle (`views/machines.rs` `workspace_row`) |
| `task_title` | task title after the assignment cell (`machines.rs` `workspace_line`) | the title in the task cell, `<status>: <title>` (`MachineWorkspace::task_cell`, `views/machines.rs` `workspace_row`) |
| `branch` | `⎇ branch` (`machines.rs` `workspace_line`) | `⎇ branch`, right-aligned (`views/machines.rs` `workspace_row`) |
| `review` | review server state and URL (`machines.rs` `workspace_line`) | `starting` while the server isn't up, and the clickable URL (`views/machines.rs` `workspace_row`) |

### `ReviewSlot`
| Field | TUI | Desktop |
|---|---|---|
| `task_id` | not shown directly: the same id appears via `assignment` (`review: <id>`) | not shown directly: the same id appears via `assignment` (`review: <id>`), which `MachinesModel::build` derives from this field (`views/machines.rs` `workspace_row`) |
| `state` | "serving" (cyan) / "starting" (gray) via `ReviewSlot::label` (`machines.rs` `workspace_line`) | "starting" (muted) via `ReviewSlot::label`; serving shows only as the state column's `review` (`views/machines.rs` `workspace_row`) |
| `url` | URL after the state (`machines.rs` `workspace_line`); not clickable in a terminal | clickable URL with external-link icon, opens in browser (`views/machines.rs` `workspace_row`) |

### `MachinesInput`
Neither client builds or reads `MachinesInput`. Both call the shared reader `shelbi_orchestrator::machines_model::read_machines_model` (`crates/shelbi-orchestrator/src/machines_model.rs`), which fills it and maps it through `MachinesModel::build`; the clients only see the resulting `MachinesModel`. So each field's parity is the parity of the output field it feeds.

| Field | TUI | Desktop |
|---|---|---|
| `display_name` | via `MachinesModel.display_name`: title (`machines.rs` `render_title`) | via `MachinesModel.display_name`: header (`views/machines.rs` `render`) |
| `machines` | via `MachineRow` name/kind/host/is_local/tags: machine header (`machines.rs` `machine_header_line`) | via same: machine header (`views/machines.rs` `machine_header`) |
| `workspaces` | via `MachineWorkspace.name` (+ `is_review_slot`, unshown): workspace lines (`machines.rs` `workspace_line`) | via same: workspace rows (`views/machines.rs` `workspace_row`) |
| `records` | via `MachineRow.remote`: binary badge (`machines.rs` `machine_header_line`) | via same: binary badge (`views/machines.rs` `machine_header`) |
| `statuses` | via `state` only (badge); `current_task` unshown (`machines.rs` `workspace_line`) | via `state` only (badge); `current_task` unshown (`views/machines.rs` `workspace_row`) |
| `in_progress` | via `assignment`, `agent`, `task_title`, `branch` (`machines.rs` `workspace_line`) | via `assignment`, `agent`, `task_title`, `branch` (`views/machines.rs` `workspace_row`) |
| `review_slots` | via `review` (state + URL) and `assignment` (`machines.rs` `workspace_line`) | via `review` (state + URL) and `assignment` (`views/machines.rs` `workspace_row`) |
| `branches` | via `branch` (`machines.rs` `workspace_line`) | via `branch` (`views/machines.rs` `workspace_row`) |
| `titles` | via `task_title` (`machines.rs` `workspace_line`) | via `task_title` (`views/machines.rs` `workspace_row`) |
| `runners` | not shown (via `runner`, which the TUI doesn't draw) | via `runner` (`views/machines.rs` `workspace_row`) |

### `ReviewPanelModel`
The desktop renders `ReviewPanelModel` directly (`views/review.rs` builds it with `ReviewPanelModel::build`). The TUI does not consume this model: its `ReviewPanel` (`review_panel.rs`) is built from `ReviewOpenParams` (task id, slot, worktree, editor name, a `has_review_url` bool, and the `IssueFile`) in `shell/mod.rs` `build_review_interface`. The TUI column maps each field to the equivalent datum.

| Field | TUI | Desktop |
|---|---|---|
| `task_id` | `id` row in the More popover header, from the `IssueFile` (`kanban.rs` `popover_header`, opened by `shell/overlays.rs` `task_description`); also keys approve/reject | title fallback when the title is empty (`views/review.rs` `render_panel`); also keys approve/reject |
| `slot` | not shown: binds the content sessions (`shell/review.rs` `show_chat` / `show_role`) | not shown: binds the content sessions and the user shell (`views/review.rs` `content_target`, `open_shell`) |
| `title` | bold task-info title (`review_panel.rs` `render_full` via `panel.rs` `render_task_info`); More popover title (`kanban.rs` `render_task_popover_into`) | Base/Medium title (`views/review.rs` `render_panel` via `task_panel::summary`) |
| `body` | More popover markdown body (`kanban.rs` `render_task_popover_into`) | not shown as markdown: gates More via `has_body` (`views/review.rs` `render_panel`) |
| `description` | 3-line preview, computed from the raw body with the same `description_text` (`panel.rs` `description_preview`) | clamped to five lines, More opening the task detail over the screen (`views/review.rs` `render_panel` via `task_panel::summary`; the window draws the board's detail, `app.rs` `open_task_over`) |
| `worktree` | left-truncated folder row, always shown (`review_panel.rs` `render_full` via `panel.rs` `render_folder`) | worktree row, its last four components, Open reveals it in Finder, no Open on a remote slot (`views/review.rs` `render_panel` via `task_panel::worktree_row`) |
| `status_label` | hardcoded "Ready for review" beside back button (`review_panel.rs` `render_full`) | not shown: the panel shows the task's column (REVIEW, the purple glyph) beside the back button, as State=Review draws it (`views/review.rs` `render_panel`) |
| `editor_name` | "Edit in <editor>" switch label (`review_panel.rs` `switch_entry`) | same label (`views/review.rs` `render_panel`) |
| `review_url` | not shown: only a `has_review_url` bool gating the Open Browser switch (`review_panel.rs` `rows`); URL resolved at open time (`review_panel.rs` `review_url`) | the Web Browser row's tooltip (`views/review.rs` `render_panel`, `render_waiting_browser`); gates the row |
| `remote_machine` | not shown: the TUI has no remote info for a review | not shown as text: `is_remote` drops Open from the worktree row and Diff/Editor from `switch_items` (`views/review.rs` `render_panel`) |
| `switch_items` | own fixed list Reviewer / View Diff / Edit in / (Open Browser) (`review_panel.rs` `rows` / `switch_entry`) | nav rows iterated from the model with ⌘N hints, the browser labelled "Web Browser", then Shell (`views/review.rs` `render_panel`) |
| `server` | state glyph + sub-state name and location under the folder row, live from the sidebar row (`review_panel.rs` `render_server_line`, fed by `shell/mod.rs` `apply_snapshot`); no health check or sub-state detail (the TUI has no `ready:` facts for the slot) | the Web Browser row: inert with a muted "loading…" in place of its ⌘4 hint until serving, the sub-state, detail and health check in its tooltip (`views/review.rs` `render_waiting_browser`); State=Review draws no server block |

### `ErrorLogModel`
Neither client consumes `ErrorLogModel`. Both render `shelbi_state::read_errors_with_unread` rows (`(ErrorLogEntry, bool)`) through the shared `shelbi_app::overlay::error_log::Viewer`. Mapped to that datum:

| Field | TUI | Desktop |
|---|---|---|
| `entries` | `viewer.rows`, newest first, "No errors logged." when empty (`overlay/error_log.rs` `build_lines` / `render`) | `viewer.rows`, same empty text, "Loading…" before the read lands (`views/overlay/error_log.rs` `render`) |
| `unread` | not shown in the overlay; the sidebar's unread button count comes from `SidebarModel.unread_errors` instead (`shell/sidebar.rs`) | not shown in the overlay; sidebar count comes from `SidebarModel.unread_errors` (`views/sidebar.rs`) |

### `ErrorRow`
| Field | TUI | Desktop |
|---|---|---|
| `timestamp` | local `YYYY-MM-DD HH:MM` via `format_ts` (`overlay/error_log.rs` `build_lines`) | same via `format_ts` (`views/overlay/error_log.rs` `render`); also in Copy text (`Viewer::text`) |
| `message` | message, word-wrapped under its column (`overlay/error_log.rs` `build_lines`) | message in a flex column (`views/overlay/error_log.rs` `render`); also in Copy text |
| `source` | not shown: stored on the entry, never rendered | not shown: never rendered (nor in Copy text) |
| `unread` | bold `●` marker (`overlay/error_log.rs` `build_lines`) | bold `●` marker (`views/overlay/error_log.rs` `render`) |
