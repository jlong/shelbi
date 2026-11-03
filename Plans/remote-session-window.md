# Remote session window: a native desktop window for a remote project

Status: design plan for review (task `remote-desktop-window-plan`, 2026-10-30).
No product code changed. Every file:line reference is against `main` at
`86e6aeb2` (v0.12.0 plus #2028/#2029).

## 1. The problem

Connect to Remote Session, and opening a recent remote session, opens a
window that runs the **far machine's TUI** inside a terminal over `ssh -t`:

- `desktop/src/views/remote_session.rs:1-14`: the window shows "that
  machine's own Shelbi for the project, in a terminal over `ssh -t`
  (`TerminalConnector`), so it always matches the far side's version and
  configuration".
- `crates/shelbi-app/src/launch/ssh.rs:273-286` (`terminal_command`): `ssh -t
  <SSH_OPTIONS> <host> -- sh -c '<find shelbi>; exec "$P" --project <name>'`.
- `crates/shelbi-app/src/launch/terminal.rs:184-200` (`TerminalConnector`):
  that command runs in a local Shelbi session named `remote/<host>/<project>`
  (`terminal.rs:157`), attached like any pane; a drop shows the shared
  "Reconnecting to <host>… (attempt N)" notice (`terminal.rs:4-11`).

The user wants a **real desktop window** for the remote project: sidebar,
Orchestrator, Issues board, task pane, Workspaces, Activity, Machines and
Settings, like a local project's window.

## 2. Why it is not a small change (what the code says)

### 2.1 The desktop reads project state from local disk

The desktop and `shelbi-app` are not socket clients. They call `shelbi_state`
(and `shelbi_orchestrator`) readers that open files under the local
`~/.shelbi`. The full inventory is in section 4.3; the headline facts:

- The sidebar is built by `shelbi_orchestrator::sidebar_model::read_sidebar_model`
  (`crates/shelbi-orchestrator/src/sidebar_model.rs:32`), which reads project
  YAML, `state.yaml`, the board index, the assignment overlay, workspace
  `status.yaml` files, the error log, the session-lost marker, supervision
  status, and probes the daemon version (`sidebar_model.rs:40-122`). For a
  review slot on a remote machine it also runs an **SSH liveness probe**
  (`sidebar_model.rs:14-15`).
- The board is `shelbi_app::board::read_open_board` (`crates/shelbi-app/src/board.rs:683`)
  and `read_closed_page` (`board.rs:740`), over `load_project`, `read_state`,
  `list_workflows`, `load_project_statuses`, `issue_store_for`,
  `read_board_report` (`board.rs:684-756`).
- A GitHub-backed board's store and search use the far machine's `gh`
  credentials (`shelbi_state::issue_store_for`, `search_issues` at
  `crates/shelbi-app/src/board_search.rs:76`).

So the readers must **run on the far machine**. Shipping raw files to the
local app and parsing them here would still leave the SSH probes, the `gh`
calls, and the far machine's own machines/relays unreachable.

### 2.2 The desktop talks to the local daemon's sockets

- Change wakes: `hub.sock` `subscribe` verb (`crates/shelbi-app/src/changes.rs:109-117`).
- Layout events (review slot opened/closed): `hub.sock` again
  (`crates/shelbi-app/src/layout_sub.rs:90`).
- Mutations, review/workspace session ops, quit, client list/detach:
  `control.sock` (`crates/shelbi-proto/src/control.rs:376-430`, client in
  `crates/shelbi-app/src/exec_daemon.rs:78-104,235-237`).
- App-wide change watcher and relaunch watch: `desktop/src/refresh.rs:407-449`,
  `desktop/src/platform/control.rs:1-10,245-252`.
- Session discovery: `shelbi_state::sessions_dir()` at
  `desktop/src/refresh.rs:79`, `desktop/src/elements/terminal/session.rs:157`,
  `crates/shelbi-app/src/session/mod.rs:254`.

### 2.3 Remote sessions already stream through a relay (the hub side)

The hub already reaches sessions on other machines through **one `shelbi relay`
per machine** over a single SSH stdio channel:

- Envelope: `crates/shelbi-proto/src/relay.rs:1-27` (`Hello`, `ListSessions`,
  `SessionList`, `Open{stream, short_id}`, `Opened`, `OpenError`,
  `Data{stream, bytes}`, `Close`, `Ping`, `Pong`; `relay.rs:45-66`). Session
  frames ride inside `Data` unchanged.
- Server: `shelbi_client::serve_relay` (`crates/shelbi-client/src/relay.rs:579-621`),
  run by the hidden `shelbi relay` (`crates/shelbi-cli/src/commands/relay.rs:32`).
- Client: `RelayChannel` (`crates/shelbi-client/src/relay.rs:186-251`:
  `is_reachable`, `list_sessions`, `open`), and every connection goes through
  `Connection::connect(Box<dyn Transport>, ..)`
  (`crates/shelbi-client/src/connect.rs:180-191`), "the single code path both
  local and remote connections share".
- Hub wiring: `shelbi_orchestrator::remote_session` (`remote_session.rs:1-36`,
  relay started at `:134`, cached per host at `:233-292`), and
  `shelbi_orchestrator::session_connect::connect`
  (`crates/shelbi-orchestrator/src/session_connect.rs:25-58`) picks local vs
  relay per target.

This is the transport the plan builds on.

### 2.4 Version skew is already policed, in three different ways

- The Connect probe refuses a host unless `RemoteListing.protocol ==
  REMOTE_PROTOCOL` (1) and the versions share `major.minor`
  (`crates/shelbi-app/src/launch/ssh.rs:39,451-457,543-551`).
- The control protocol "rides the exact-version match" and has no frozen core
  (`crates/shelbi-proto/src/control.rs:14-19`, `CONTROL_PROTOCOL_VERSION = 6`
  at `:55`). The daemon pushes `Reexec` to any subscriber whose hello version
  differs from its own (`crates/shelbi-cli/src/commands/daemon/control.rs:416-429`),
  and the desktop then flips to "relaunch required"
  (`desktop/src/platform/control.rs:1-10`). Mutations from non-CLI paths are
  refused against a mismatched daemon
  (`crates/shelbi-state/src/hub_version.rs:206-214`).
- The desktop checks the local CLI's control protocol at bootstrap
  (`desktop/src/platform/discovery.rs:170-175`, `cli_control_protocol`).
- The session protocol is a frozen core: sessions stay compatible across
  versions (`desktop/src/platform/control.rs:9-10`).

### 2.5 The no-hub-forward rule

`crates/shelbi-app/src/launch/ssh.rs:14-17`: remote-session `ssh` never carries
the `-R` forward of this machine's hub socket, "the far side has its own
Shelbi, and its `hub.sock` must stay its own". The hub's own `ssh` builder
**does** add `-R` (`crates/shelbi-ssh/src/lib.rs:284-306`,
`build_command` at `:347-373`), so the remote window must not reuse
`shelbi_ssh::build_command` or `remote_session`'s relay start
(`remote_session.rs:134`). It also never starts the far daemon implicitly
(`ssh.rs:18-20`; `terminal.rs:8-11`).

## 3. Transport

### Option A: forward the far sockets with `ssh -L` and serve disk reads over them

`ssh -N -L <local>.sock:<far>/control.sock -L <local>.sock:<far>/hub.sock`,
plus a new read API on the daemon.

- Pro: the desktop's existing socket clients work by pointing them at other paths.
- Con: creates socket files on this machine that any process of the user can
  open, with the **entire** control API behind them (including
  `QuitShelbi`, `ReloadClients`, `DetachClients`, `control.rs:396-430`).
  Stale socket files after a drop (the `StreamLocalBindUnlink` dance the hub
  already fights, `crates/shelbi-ssh/src/lib.rs:1029-1052`).
- Con: one forward per socket, and session sockets are per session
  (`~/.shelbi/sessions/<id>/`): we would need the relay anyway for sessions.
- Con: a forwarded `hub.sock` is writable (it is the worker event socket),
  which is exactly what the no-hub-forward rule exists to prevent.

### Option B: a new remote read API on the far daemon (`control.sock` `Query`)

Add `ClientMsg::Query(..)` to the control protocol; reach `control.sock`
however (A or C).

- Pro: one server owns reads and writes.
- Con: reads would need the daemon up and on the same control protocol; the
  daemon process would grow every view reader. Board/sidebar readers already
  read the daemon-published board index from disk (`board.rs:677-682`), so the
  daemon adds no cache advantage.

### Option C (recommended): the relay, extended with named services

One `ssh <host> -- sh -c '<FIND_SHELBI>; exec "$P" relay'` per remote host,
with `SSH_OPTIONS` (`launch/ssh.rs:55-66`: BatchMode, keepalives) and **no
`-R`, no `-L`, no ControlMaster**. The far binary is found exactly the way the
probe finds it (`FIND_SHELBI`, `ssh.rs:236-240`). Relay protocol v2 adds one
frame, `OpenService { stream, service }` (answered by `Opened` / `OpenError`
like `Open`), and three services:

| Service | Far end | Direction | Replaces locally |
|---|---|---|---|
| `query` | handled **inside the far `shelbi relay` process** by a handler the CLI injects into `serve_relay` | request/response, JSON | every disk read in 4.3 |
| `control` | bridged to the far `control.sock`; the relay decodes each client frame and refuses the denylisted messages (section 7) | both | `exec_daemon.rs`, `platform/control.rs`, `refresh.rs:447` |
| `changes` | the relay itself opens the far `hub.sock`, sends only `{"verb":"subscribe","project":..}` (same frame as `changes.rs:117`) and forwards the lines down | far to local only | `changes.rs`, `layout_sub.rs` |

Sessions keep using the existing `ListSessions` / `Open` frames.

Why C:

1. **Nothing new is exposed.** Everything is inside one SSH session's stdio:
   no ports, no socket files on either side. Authorization is the SSH login,
   as today.
2. **The far binary reads its own disk.** `query` runs the far machine's
   readers compiled into the far `shelbi`, so on-disk format skew disappears
   for reads; the only contract is the wire shape of the answers.
3. **It reuses tested machinery**: framing, keepalive/`Unreachable`
   semantics, hub-side demux backpressure and attach-resync replay
   (`relay.rs` docs `:14-19`), and `Connection::connect` over a `RelayStream`.
4. **One channel per host** keeps under sshd's default 10 sessions per
   connection (`relay.rs:5-7`), whatever number of panes the window shows.
5. **The daemon stays the single writer**: writes go through `control`.

Old far binaries: an old relay ends the channel on an unknown frame type
(`serve_relay` returns on a decode error, `relay.rs:606-610`). The client must
read the relay `Hello.version` first and send `OpenService` only to a v2+ relay;
the listing also advertises it (section 6), so the client normally never tries.

### What the far side must expose that it does not today

- Project config and display label, workspaces, machines (`load_project`, `machines_model`).
- Board: open issues, closed pages, search, issue bodies (in `IssueFile`).
- Project state: Zen mode, workspace filter, column overrides (`read_state`, `zen.rs:19-24`).
- Events (`events.log`, for Activity, `crates/shelbi-app/src/activity.rs:795,898`).
- Errors with unread marks (`read_errors_with_unread`, `desktop/src/app.rs:3974`).
- Workflows, statuses, agents, pool/workspaces config, changelog, project keymap overlay.
- Session discovery (`ListSessions` already), the orchestrator title (`info` over a session stream).
- Workspace details, review slots, review open target (orchestrator readers).
- Change and layout notifications (today only on `hub.sock`).
- Far user config: **not** exposed. Theme, desktop and keymap settings are the
  viewer's (`crates/shelbi-state/src/user_config.rs:33-56`); see 4.4.

## 4. The client seam

### 4.1 Shape

- **`ProjectKey { origin, name }`** where `origin` is `Local` or
  `Remote { host }`. Every place that keys by project name keys by this:
  the feed registry (`desktop/src/feeds.rs:36`, `HashMap<String, Shared>`), window
  slots (`desktop/src/window_state.rs:111`) and per-project view state
  (`window_state.rs:79,268`, under `project_dir(project)`; a remote project's
  goes under the local `~/.shelbi/remote/<host>/<project>/`). A remote
  project named like a local one must not share a feed.
- **`trait ProjectSource: Send + Sync`** in `shelbi-app` (toolkit-free),
  one method per query in 4.3, each returning the type the reader returns
  today. It is blocking, like the readers, and runs on the background
  executor where they run now.
- **`LocalSource`** calls today's functions. Since `shelbi-app` must not depend
  on `shelbi-orchestrator` (`sidebar_model.rs:5-8`), `LocalSource` lives in
  `shelbi-orchestrator` (`shelbi_orchestrator::source`), next to the readers
  it wraps; it is also **the handler the far relay's `query` service runs**, so
  local and remote answers are produced by the same code.
- **`RemoteSource`** sends the query over the host's link and decodes the
  answer. Lives in `shelbi-app` (it only needs the wire types and a
  transport).
- **`ProjectLink`**: the window's handle to everything that is not a read:
  `Executor` routing (`crates/shelbi-app/src/exec.rs:204`), session connector,
  change and layout subscriptions. Local = today's sockets; Remote = the
  host's relay channel.
- A window owns `(ProjectKey, Arc<dyn ProjectSource>, ProjectLink)`. Views
  stop calling `shelbi_state`/`shelbi_orchestrator` readers; they ask the
  window's source.

Seams that already exist and only need a remote implementation:
`refresh::Reader` (`desktop/src/refresh.rs:49`), `FeedSpawner`
(`desktop/src/feeds.rs:23`), `SessionConnector`
(`desktop/src/views/session_view.rs:79`), `Executor` (`exec.rs:204`),
`RemoteConnector` (`crates/shelbi-app/src/launch/remote.rs:137`),
`shelbi_app::session::Connector` (`crates/shelbi-app/src/session/mod.rs:240`).

The TUI can adopt the same `ProjectSource`/`ProjectLink` later; its refresher
calls the same `read_sidebar_model`. This plan keeps the TUI on today's
`ssh -t` path (`crates/shelbi-tui/src/shell/remote_session.rs:33`).

### 4.2 Wire types

None of the reader outputs is serializable today: `SidebarModel`
(`crates/shelbi-app/src/view.rs:43`), `BoardRead` (`board.rs:665`), `ClosedPage`
(`board.rs:730`), `MachinesModel` (`view.rs:1095`), `ActivityData`
(`activity.rs:330`), `ChangelogData` (`changelog.rs:189`), `ProjectSummary`
(`settings/summary.rs:53`), `SearchCorpus`
(`overlay/palette_search.rs:215`), `WorkspaceDetails`
(`crates/shelbi-orchestrator/src/workspace_session.rs:93`) and `BoardReport`
(`crates/shelbi-state/src/board_index.rs:749`) derive only `Debug, Clone[, ..]`.
`IssueFile` is already serde (`crates/shelbi-state/src/lib.rs:2983`). Phase 1
adds serde derives with `#[serde(default)]` on new fields so additive fields
don't break decoding. The query enum and framing (`[len u32 BE][json]`, as
`control::encode`, `control.rs:548-581`) live in `shelbi_app::source::wire`
(`shelbi-proto` cannot depend on `shelbi-app`).

### 4.3 Inventory of direct state reads to route

Production code only (test modules excluded). "Query" is the `ProjectSource`
method / `query` request it becomes.

**Sidebar and feed**

| Call site | Reads | Query |
|---|---|---|
| `desktop/src/refresh.rs:58` | `read_sidebar_model(project)` (everything in `sidebar_model.rs:32-122`) | `sidebar` |
| `desktop/src/projects.rs:149` | `read_config_skeleton` (`sidebar_model.rs:136`) | `config_skeleton` |
| `desktop/src/refresh.rs:77-92` | `sessions_dir` + session `info` for the orchestrator title | relay `ListSessions` + `Open` + `info` (no new query) |
| `desktop/src/refresh.rs:174-175` | `changes::spawn_observed`, `layout_sub::spawn` (hub.sock) | `changes` service |
| `desktop/src/refresh.rs:447-449` | `control_socket_path` + `ControlClient` (Changed notes) | `control` service |
| `crates/shelbi-app/src/changes.rs:109`, `layout_sub.rs:90` | `hub_socket_path` | `changes` service |

**Issues board, task detail, search, palette**

| Call site | Reads | Query |
|---|---|---|
| `desktop/src/views/issues.rs:271` -> `board.rs:683-735` | `load_project`, `ownership::gate_from_index`, `read_state`, `list_workflows`, `load_project_statuses`, `issue_store_for`, `read_board_report` | `open_board` |
| `desktop/src/views/issues.rs:277` -> `board.rs:740-760` | `issue_store_for`, `read_board_report` | `closed_page` |
| `crates/shelbi-app/src/board_search.rs:76` | `search_issues` (GitHub search on GitHub boards) | `search { query, page }` |
| `desktop/src/views/issues.rs:265`, `views/activity.rs:128`, `app.rs:1359` | `keymap::load_keymaps(Some(project))` (global + project overlay) | local global keys + `project_keymap` (open question 3) |
| `desktop/src/views/issues.rs:1150-1153` | `resolve_editor` (label for "Edit in ...") | local, but the action changes (section 8.4) |
| `desktop/src/app.rs:3742` -> `overlay/palette_search.rs:243-246` | `read_open_board` + `read_changelog` | composed from `open_board` + `changelog` |
| `crates/shelbi-app/src/exec_daemon.rs:274-275` | `issue_store_for` (expected state before a mutation) | `issue_state { id }` |

**Activity**

| Call site | Reads | Query |
|---|---|---|
| `desktop/src/views/activity_reader.rs:67` -> `activity.rs:858-900` | `events_log_path` (the hub-wide `events.log`), staged read | `activity_window { window, filter }`, two replies (early + full) |
| `crates/shelbi-app/src/activity.rs:519-533` | `load_project`, `read_board_with_cfg`, `fold_assignment_overlay` (titles) | inside `activity_window` |
| `crates/shelbi-app/src/activity.rs:676` | `issue_store_for` | inside `activity_window` |
| `crates/shelbi-app/src/activity.rs:2443` | `task_path` | inside `activity_window` |
| `desktop/src/views/activity_state.rs:52` | `load_project` | inside `activity_window` |

**Workspaces, review, machines**

| Call site | Reads | Query |
|---|---|---|
| `desktop/src/views/workspace.rs:113` | `workspace_session::workspace_details` (`workspace_session.rs:113`) | `workspace_details { ws }` |
| `desktop/src/views/review.rs:109` | `review_session::review_open_target` (`review_session.rs:229`) | `review_open_target { task }` |
| `desktop/src/views/review.rs:131` | `load::review_slots` (`load.rs:188`) | `review_slots` |
| `desktop/src/views/machines.rs:98` | `machines_model::read_machines_model` (`machines_model.rs:30`) | `machines` |
| `desktop/src/views/machines.rs:61` | `machine::probe_host_reachability` (SSH from this machine) | `machine_reachability { host }` (must run far side) |
| `crates/shelbi-orchestrator/src/session_connect.rs:35` | `load_project` (where a workspace lives) | resolved far side (section 5) |
| `crates/shelbi-app/src/session/mod.rs:409` | `load_project(..).is_ok()` | `project_exists` |

**Errors, Zen, changelog**

| Call site | Reads | Query |
|---|---|---|
| `desktop/src/app.rs:3974` | `read_errors_with_unread` | `errors` |
| `crates/shelbi-app/src/zen.rs:19-24` | `read_state`, `read_global_state` | in `sidebar` (already) / `zen_state` |
| `desktop/src/views/changelog.rs:188` -> `changelog.rs:204-208` | `changelog::read_entries` | `changelog` |

**Settings**

| Call site | Reads | Query |
|---|---|---|
| `crates/shelbi-app/src/settings/summary.rs:131-138` | `load_project`, `config_project_dir`, `WorkflowConfig::load`, `list_agents` | `project_summary` |
| `crates/shelbi-app/src/settings/summary.rs:87-90` | `shelbi_home`, `load_user_config` (global section) | stays local (viewer's config) |
| `desktop/src/views/workflows.rs:184`, `workflow_editor.rs:333,570`, `settings/workflows.rs:192,202` | `WorkflowConfig::load` | `workflow_config` |
| `crates/shelbi-app/src/settings/workflow_editor.rs:334` | `load_project` | in `workflow_config` |
| `desktop/src/views/agents.rs:130`, `agent_editor.rs:192,404`, `settings/agents.rs:154` | `AgentsConfig::load` | `agents_config` |
| `crates/shelbi-app/src/settings/workspaces.rs:186,197` | `load_project`, `projects_dir` | `workspaces_config` |
| `crates/shelbi-app/src/settings/keybindings.rs:519-520` | `load_keymaps(project)` | `project_keymap` |
| `crates/shelbi-app/src/command_model.rs:196,234-254` | `agents_dir`, `zenmode_path`, `workflows_dir`, `changelog_template_path`, `config_project_dir`, `projects_dir` (Edit targets) | `edit_targets` (far paths; section 8.4) |

**Project list and launch**

| Call site | Reads | Remote window |
|---|---|---|
| `desktop/src/projects.rs:134-135,213-238`, `desktop/src/app.rs:642-643,1840`, `crates/shelbi-app/src/command_model.rs:51,142-146` | `list_projects`, `list_open_projects`, `resolve_project_for_cwd` | the switcher lists the far host's projects from the listing (`RemoteListing.projects`, `ssh.rs:83-94`); open question 6 |
| `crates/shelbi-app/src/launch.rs:120,186,291` | `list_projects`, `ensure_daemon_running`, `touch_project_launched` | not used by a remote window; the far daemon is never started implicitly |

**Stays local in a remote window (the viewer's machine)**

`load_user_config` / `update_user_config` (`desktop/src/app_icon.rs:32`,
`theme/mod.rs:354`, `elements/terminal/settings.rs:138,200`,
`views/settings.rs:856,1199`, `app.rs:1156`, `settings/general.rs:95`),
`toggle_sidebar_machine_collapsed` (`app.rs:2065`, keyed by host for a remote
window), window state (`window_state.rs`), recent remotes
(`launch/recent.rs:47`), update check (`platform/update.rs:91`), feedback
(`feedback.rs:286`), bootstrap/discovery (`platform/*`).

### 4.4 Guarding the inventory

Add a seam-gate test (the pattern the settings work already uses) that scans
`desktop/src/views/**` and the desktop feed for the reader names in 4.3 and
fails if a view calls one directly instead of the window's `ProjectSource`.
That keeps a later view from silently reading the local disk in a remote
window.

## 5. Sessions

### 5.1 Sessions on the far host itself (orchestrator, local workspaces, review content, shells)

A `RemoteSessionConnector` implements `SessionConnector`
(`session_view.rs:79-88`): resolve `discovery_name(project, target)`
(`crates/shelbi-app/src/session/mod.rs:135`), `ListSessions` on the host's
channel, `open(short_id)` (`relay.rs:251`), `Connection::connect(Box<dyn
Transport>)` (`connect.rs:186`). Classification matches
`session_connect::classify_remote` (`session_connect.rs:63-90`): channel down
is `ConnectFailure::Unreachable` (keep reconnecting), no session is `Idle`.
Review/workspace content sessions are requested through the `control` service
(`ReviewSession`/`WorkspaceSession`, `control.rs:240-300`) exactly as locally.

### 5.2 Sessions on the far project's own remote machines (two hops)

A far project may run workspaces on a third machine; the far hub reaches them
through **its** relay (`remote_session.rs:267`). The local app has no route
(and possibly no credentials) to that machine. Recommended: relay v2 also
answers `OpenService { service: "session", target }` by running
`session_connect::connect` far side (`session_connect.rs:25`) and bridging the
resulting connection's bytes. The far hub's own relay cache and `-R` forward
stay on the far side, where they already are. This is its own phase (P8);
until then those panes show a placeholder naming the machine.

### 5.3 Latency

- Keystrokes and output take the same path the `ssh -t` window takes today,
  one SSH hop, with session frames forwarded unchanged; no extra encode.
- Many panes share one channel; the client-side demux already has per-stream
  bounded queues that drop to resync on overflow (rt-relay design), so a busy
  agent can't starve the orchestrator pane.
- Reads are change-driven: the `changes` service wakes a `sidebar` query; the
  polling fallback (`desktop/src/refresh.rs:16-19`) runs at a slower cadence
  for a remote feed, since each poll is a round trip plus the far read.
- The window paints the config skeleton first (the cold-switch pattern,
  `sidebar_model.rs:127-135`) from the listing so the first frame doesn't wait
  on a sidebar round trip.

### 5.4 Reconnect

- One `RemoteLink` per host (like the hub's relay cache,
  `remote_session.rs:233-292`) shared by every window and pane on that host.
- On channel loss (`is_reachable()` false via Ping/Pong, or EOF), every pane
  goes through its own `ReconnectController`
  (`crates/shelbi-app/src/session/reconnect.rs:63`) and shows
  `reconnecting_notice(host, attempt, reason)` (`session/mod.rs:479`),
  "Reconnecting to <host>… (attempt N)", as remote workspace panes and the
  current remote window do (`remote_session.rs` view, `:9-12`).
- The feed reports a link state like `DaemonLink::Polling`
  (`desktop/src/refresh.rs:97-102`) and the titlebar dot turns yellow (current
  remote window behavior, `remote_session.rs:8-9`).
- Each reconnect attempt first runs `RemoteConnector::connect`
  (`launch/ssh.rs:371-383`), which checks the project and that the far daemon
  is running, so a reconnect **never starts the far daemon**; a stopped daemon
  ends the loop with `DaemonStopped` and the window offers Start, as today
  (`terminal.rs:144-149`).
- Sessions resume by attach and resync; the agents never stopped (far side
  keeps running, `ssh.rs:21-24`).

## 6. Version compatibility

Rule: **a native window opens only when both sides speak the same query,
control and relay protocols and share `major.minor`; otherwise Shelbi opens
today's TUI-over-SSH window**, with one line saying why.

- **Advertise** in `RemoteListing` (`ssh.rs:83-94`) an optional
  `native: { relay, control, query }` (the three protocol numbers). It is
  additive: the struct does not deny unknown fields, so an old client reads a
  new listing, and a new client reading an old listing sees `None` and falls
  back. `REMOTE_PROTOCOL` stays 1.
- **Decide** in the Connect flow after the probe (`ssh.rs:355-362`): match ->
  native window; mismatch or `None` -> TUI window plus a notice such as
  "Shelbi on devbox is 0.12.1 and this Mac has 0.13.0, so it opens in its
  terminal view. Update both to get the full window."
- **Re-check on every reconnect**: the far side may have been upgraded while
  the window was away. A changed protocol closes the native window's panes
  and offers "Reopen" (which re-decides), rather than sending commands to a
  daemon that may reject them.
- **Control `Reexec`**: the far daemon pushes `Reexec` to a subscriber whose
  version differs (`daemon/control.rs:420-429`). The remote link subscribes
  with `ClientKind::Remote`, which the protocol already reserves
  (`control.rs:313-320`), and the daemon skips the version `Reexec` for that
  kind (a deliberate `ReloadClients` still reaches it and is treated as
  "reconnect and re-decide"). The local app's own `ControlHandle` relaunch
  watch (`platform/control.rs`) is never fed by a remote link.
- **Tolerant decoding**: query answers use `#[serde(default)]`, so an additive
  field doesn't need a protocol bump; a removed or retyped field does. A
  round-trip snapshot test per wire type pins the shape so a change without a
  bump fails CI.
- **Sessions**: the session protocol is frozen, so terminal panes would work
  across versions; the rule above is about the data and command planes.

Open question 1 asks whether to require exact version equality instead.

## 7. Security

- **SSH only, no new ports, no socket files**: one `ssh` child per host, with
  `BatchMode=yes` and keepalives (`launch/ssh.rs:55-66`), stdio only. No `-L`,
  no `-R`, no ControlMaster; it does not go through `shelbi_ssh::build_command`
  (which adds `-R`, `crates/shelbi-ssh/src/lib.rs:293-300`).
- **The no-hub-forward rule holds** (`ssh.rs:14-17`): this machine's `hub.sock`
  is never exposed to the far host, and the far `hub.sock` is never exposed
  here. The `changes` service is one-way: the relay writes only the
  `subscribe` verb to the far `hub.sock`; nothing from the client is ever
  written to it, so a client cannot inject worker events.
- **Control denylist**: the relay decodes client frames on the `control`
  service (the far binary knows its own protocol) and refuses `QuitShelbi`,
  `ReloadClients` and `DetachClients { All }` from a remote window by default
  (open question 2). `QuitProject` is allowed only for the window's project.
- **Credentials stay put**: `gh`, git, and the far project's machine SSH keys
  are used only on the far side, since reads and two-hop sessions run there.
  Shelbi still stores nothing but the host as typed (`ssh.rs:10-13`).
- **Authorization = the SSH login**: everything runs as the far user, as the
  TUI-over-SSH window does today.
- **Far paths are data**: query answers may contain far paths (Edit targets);
  the local app never opens them as local files (section 8.4).

## 8. Writes

### 8.1 Already daemon-owned: bridge `control`

These already go through `control.sock` and need only the transport switch
(plus not calling `ensure_daemon_running()` for a remote project,
`exec_daemon.rs:78,102,235`):

- Issue mutations: Move, Start, Assign, Unassign, Add, Edit, Approve, Reject,
  Prioritize (`control.rs:147-180`; `Mutation` in `exec.rs:113-150`), with the
  per-issue queue, expected-state check and `Changed` broadcast. The far TUI
  and other far clients see a remote window's moves immediately, and vice
  versa.
- Review and workspace content sessions (`ReviewSession`, `WorkspaceSession`).
- Quit project, list/detach clients (subject to the denylist).

The expected state a mutation sends (`exec_daemon.rs:274`) comes from the
`issue_state` query, or from the board snapshot the window already has.

### 8.2 Direct file writes to move behind the daemon

Today these write the local disk directly. New control messages (daemon-owned,
so a single writer and a broadcast), used by local windows too so both paths
stay one:

| Call site | Write |
|---|---|
| `desktop/src/views/issues.rs:881` | `set_workspace_filter` |
| `desktop/src/views/issues.rs:892` | `set_kanban_column_override` |
| `crates/shelbi-app/src/zen.rs:30-31` | `toggle_zen_mode` (guarded by `ensure_daemon_matches_for_mutation`) |
| `crates/shelbi-app/src/zen.rs:39` | `mark_zen_intro_seen` (global; stays local, it's the viewer's intro) |
| `crates/shelbi-app/src/zen.rs:54` | `append_error` |
| `desktop/src/app.rs:3869` | `clear_errors` |
| `desktop/src/app.rs:3975` | `mark_errors_read` |
| `crates/shelbi-app/src/session/relaunch.rs:68-76` | `request_supervision_relaunch` (far supervision state) |

### 8.3 Config saves

Settings pages save project config through comment-preserving patchers
(`WorkflowConfig` save in `settings/workflow_editor.rs`, `AgentsConfig` in
`settings/agents.rs`, pool/machine changes through `pool_admin`
(`desktop/src/views/machine_form.rs:101`), `scaffold_project_statuses`
(`settings/workflows.rs:209`), `materialize_default_agents`). Recommended:
one control message `SaveConfig { surface, expected_hash, text }`: the client
builds the new text with its own patcher (same release line), the daemon
re-parses it with the far binary's loader, refuses on a hash mismatch (someone
else edited the file) and writes atomically. Machine readiness checks
(`settings/machine_check.rs:11`) and add/update machine run SSH **from the far
hub**, so they become control requests too, not local calls.

### 8.4 Actions that touch the viewer's machine

- **Open in editor** (`desktop/src/views/external_edit.rs:155,205`,
  `elements/terminal/open.rs:38`, Edit targets in `command_model.rs:234-254`):
  the files are on the far machine. Proposal: open them in a far-side terminal
  session running the far `$EDITOR` (a `WorkspaceSession`-style request).
  Open question 4.
- **Image drops into a terminal** (`desktop/src/drops/pasteboard.rs:170-172`
  writes a local file and pastes its path): the path doesn't exist far side.
  Proposal: upload through a relay `put` service to a far temp dir and paste
  the far path. Open question 5.
- **Add Project** (`desktop/src/commands.rs:217-221`, local
  `project_create`): not offered from a remote window in v1.

## 9. Phasing into tasks

Every phase merges on its own. P1 to P9 keep today's behavior for users:
Connect still opens the TUI-over-SSH window, and the native window is reachable
only behind a hidden dev setting (proposed `desktop.native_remote: true` in
`config.yaml`, env override `SHELBI_NATIVE_REMOTE=1`). P10 flips the
default.

### P1 `rw-serde-models`: serializable read models

- Serde derives (with `#[serde(default)]`) on every output in 4.2 and what they
  contain; round-trip tests per type.
- Acceptance: each type round-trips through JSON; `cargo test --workspace` and
  desktop tests pass; no behavior change; `shelbi-app`'s `no_ui_deps` holds.

### P2 `rw-project-source`: the seam, local only

- `ProjectKey`, `ProjectSource`, `LocalSource` (in `shelbi-orchestrator`),
  `ProjectLink` with the local implementation; every row of 4.3 routed through
  them; feeds and view state keyed by `ProjectKey`; the seam-gate test of 4.4.
- Acceptance: no view in `desktop/src/views` calls a 4.3 reader directly (gate
  test); TUI and desktop smoke tests (`every_registry_command_runs_from_the_palette`,
  `desktop/src/app_smoke.rs`) pass; local behavior unchanged; root
  `Cargo.lock` unchanged and `cargo check --workspace --locked` on 1.88.

### P3 `rw-relay-services`: relay v2

- `RELAY_PROTOCOL_VERSION = 2`; `OpenService`; services `query` (handler
  injected by `commands/relay.rs` from `LocalSource`), `control` (bridge plus
  denylist), `changes` (one-way subscribe). Client checks the relay hello
  before using services. `RemoteListing.native` capability field. `RemoteLink`
  (one `ssh` per host, `SSH_OPTIONS`, `FIND_SHELBI`, no `-R`).
- Acceptance: in-process e2e tests in the style of
  `crates/shelbi-client/tests/relay_e2e.rs` for each service; a v1 relay is
  detected and never sent `OpenService`; denylisted control messages are
  refused; the `changes` service never forwards client bytes to `hub.sock`; a
  test asserts the built `ssh` argv has no `-R`/`-L`/ControlMaster.

### P4 `rw-remote-source`: reads over the link

- `RemoteSource` and the remote `ProjectLink` subscriptions (`changes`, layout,
  Changed notes over `control` with `ClientKind::Remote`); daemon skips the
  version `Reexec` for `Remote`.
- Acceptance: a test drives a `RemoteSource` against an in-process relay over a
  fixture project and gets answers equal to `LocalSource` on the same fixture
  for every query; a change on the far side wakes a remote feed; a version
  mismatch in the listing makes the decision "fallback".

### P5 `rw-remote-sessions`: panes over the link

- `RemoteSessionConnector` for orchestrator, far-local workspaces, review
  content and shells; reconnect with the shared notice; the orchestrator title
  probe over the link.
- Acceptance: a fake-relay test attaches, drops the channel, shows
  "Reconnecting to <host>… (attempt N)", and resumes the same session; a
  reconnect never starts the far daemon (`FakeRemote::started()` stays empty,
  `launch/remote.rs:225`).

### P6 `rw-remote-window`: the window, behind the dev flag

- Opening a remote session with the flag on opens the full main window for a
  remote `ProjectKey`: sidebar, Orchestrator, Issues board, task pane,
  Workspaces, Activity, Machines, Settings (read), with issue mutations via
  `control` (8.1). Titlebar names the host; project switcher lists the host's
  projects. Actions in 8.2 to 8.4 not yet available are disabled through the
  command enablement path with a reason (no "not implemented" dispatch arm, per
  the parity rule).
- Acceptance: with the flag on, a fake-host smoke test opens every view; moving
  a card on a remote window lands on the far board and a far `Changed` updates
  the window; with the flag off the TUI window opens exactly as today.

### P7 `rw-remote-state-writes`: daemon-owned project-state writes

- Control messages for every row of 8.2, used by local windows as well.
- Acceptance: each write works locally and remotely through the same message;
  a scenario per new message in `crates/shelbi-app/tests/command_scenarios.rs`
  if it is a command; control protocol bumped with a doc line in `control.rs`.

### P8 `rw-remote-config-writes`: settings saves and machine checks

- `SaveConfig` with expected hash; readiness checks and machine/workspace
  changes as control requests run far side.
- Acceptance: editing a workflow in a remote window writes the far file with
  comments preserved; a concurrent far edit is refused with a "changed on
  <host>, reload" message; machine checks run from the far hub.

### P9 `rw-remote-two-hop`: sessions on the far project's machines

- Relay `session` service resolving via `session_connect::connect` far side.
- Acceptance: a far-project workspace on a third (fake) machine streams into
  the window and reconnects when either hop drops.

### P10 `rw-remote-cutover`: native by default

- Connect and recent remotes open the native window when the capabilities
  match, the TUI window otherwise (with the notice); a menu/palette command
  "Open in Terminal View" keeps the TUI window reachable (a new `CommandKind`
  with its scenario and an arm in both clients). Editor and image-drop
  behaviors per the answers to open questions 4 and 5. `desktop/docs/parity.md`
  rows for any new view-model fields; site docs updated (no em dashes).
- Acceptance: matched host -> native window; mismatched or old host -> TUI
  window plus notice; both smoke tests pass; the dev flag is removed.

Dependencies: P1 -> P2 -> P4; P3 -> P4 -> P5 -> P6 -> P10; P7, P8 and P9
after P6, in any order; P10 needs P7 and P8 (P9 optional for cutover, open
question 8).

## 10. Open questions for the maintainer

1. **Version rule**: protocol-number match plus same `major.minor` (proposed),
   or exact version equality like mutations today?
2. **Destructive control from a remote window**: should Quit Shelbi, Reload
   clients and Detach all be reachable on the far host from a remote window, or
   denylisted (proposed)?
3. **Keybindings**: the viewer's global keys plus the far project's overlay
   (proposed), or the viewer's keys only?
4. **Open in editor** for far files: a far terminal session with the far
   `$EDITOR` (proposed), disabled, or a local editor over SFTP?
5. **Image drops** into a remote terminal: upload through the relay to a far
   temp dir (proposed), or disable?
6. **Project switcher** in a remote window: only the far host's projects
   (proposed), or local and remote mixed in one list?
7. **Notifications** (`desktop/src/notify.rs`) for remote projects: on by
   default?
8. **Two-hop workspaces** (P9): required before cutover, or after?
9. **The TUI**: keep `ssh -t` for remote (proposed), or plan a native remote
   TUI on the same `ProjectSource` later?
10. **Dev flag**: `desktop.native_remote` in `config.yaml` plus env override,
    or reuse the hub's `DevConfig` block?
11. **Far daemon stopped**: keep "never start implicitly; offer Start"
    (proposed, as today)?

## 11. Decisions (maintainer, 2026-11-02)

Approved for implementation as umbrella `remote-window` (app-feature, branch
`feature/remote-window`; subtasks on the `remote-window-subtask` workflow).
Every open question in section 10 takes the proposed answer, except question 1:

1. **Version rule: exact version equality.** A native window opens only when
   the far Shelbi's version equals this Mac's exactly (and the relay, control
   and query protocol numbers match). Any other version opens today's
   TUI-over-SSH window with the one-line notice naming both versions. This
   replaces the `major.minor` rule in section 6; the `native` capability in
   `RemoteListing` still carries the protocol numbers, and the decision also
   compares the full version string.
2. Quit Shelbi, Reload clients and Detach all are denylisted from a remote window.
3. Keybindings: the viewer's global keys plus the far project's overlay.
4. Open in editor: a far terminal session running the far `$EDITOR`.
5. Image drops: upload through the relay to a far temp dir and paste the far path.
6. Project switcher: only the far host's projects.
7. Notifications for remote projects: on by default.
8. Two-hop workspaces (P9): after cutover; P10 does not wait for P9.
9. The TUI keeps `ssh -t` for remote.
10. Dev flag: `desktop.native_remote: true` in `config.yaml` plus
    `SHELBI_NATIVE_REMOTE=1`.
11. Far daemon stopped: never start it implicitly; offer Start.

