# dt-shell-sidebar: the app shell

## What landed

- **Sidebar** (`src/views/sidebar.rs`), rendered from `shelbi_app::SidebarModel`
  only. The rows, their order and their labels come from the new shared
  `SidebarModel::rows()` (`crates/shelbi-app/src/view.rs`), which the TUI's
  sidebar now uses too, so the two clients can't drift: the project title and
  search box, the Orchestrator / Issues / Activity nav, the machine-grouped
  workspaces with their state badges (`⏵ ? ⚠ ⏸ · …`), the Ready for Review and
  Queued for Review sections (two-line entries, `✓`/`▶`/`·`, location, `⎇ branch`),
  the config-error and "Loading…" rows, then the footer: version row and board
  banner, the polling indicator, the `ZEN MODE ON` band, the unread-errors
  button, settings and collapse buttons. Machine group headers collapse and
  expand (persisted to `state.json`, shared with the TUI).
- **Resize and collapse.** Drag the right edge; the width is the shared nav
  model's (in TUI columns, 9px each) under `shelbi_app::nav::clamp_sidebar_width`.
  The collapse button, or ⌃⌘S, hides it; a button at the top of the main area
  brings it back.
- **Keyboard** (`src/keys.rs`): gpui keystrokes convert to `KeyChord` and
  dispatch through `shelbi_state::keymap` (the user's `keys.yaml`, read off the
  UI thread): `j`/`k`/arrows (and Tab / Shift+Tab) move, Enter/Space open,
  Ctrl+H / Ctrl+L move focus between sidebar and main, `r` refreshes. ⌘W closes
  the window, ⌘, opens settings.
- **Main area routing**: `ClientState` is the source of truth; every `View`
  routes through it and the main area draws the not-implemented stub for a view
  whose subtask hasn't landed (`commands::view_stub_id`). The Issues row routes
  to `dt-issues-board`'s native board (`views::issues::IssuesView`), built for
  the window's project the first time it is shown (so each project window has
  its own board); Enter on a card opens task detail inside the main area, and
  the board's mutations come back through the window's dispatch.
- **One window per project.** `SwitchProject` opens the target project's
  window, or focuses it if open. The project switcher is the project title's
  dropdown (the list is `shelbi_state::list_projects`, the TUI switcher's
  source). A new window paints the target's chrome on its first frame from the
  prefetched config skeleton (else the bare skeleton) and data cascades in.
  Opening a project brings up its orchestrator and records it open
  (`<cli> --project <name> orchestrate` with the CLI and login environment
  `dt-platform-bootstrap` found, off the UI thread).
- **Bootstrap integration.** The first window runs `platform::Bootstrap` and
  shows its progress / failure screens; its `Ready` is kept app-wide, so a
  project switch's new window skips straight to the shell. The relaunch bar
  shows in every window, and commands that reach the daemon are refused until
  relaunch (`commands::sends_to_daemon`). Bootstrap's control watch is the app's
  one attached-client registration (under the app's client id, so "Detach Other
  Clients" spares this app), and a detach push now quits the app
  (`BootstrapEvent::Detach`, `ControlHandle::watch_as`).
- **Refresh pipeline** (`src/refresh.rs`): per window, `shelbi_app::spawn_refresher`
  over the shared sidebar reader, woken by the hub change subscription
  (`shelbi_app::changes`, moved from the TUI), with a polling fallback (750ms,
  the TUI's cadence) while the daemon is unreachable and a "Daemon unreachable ·
  polling" indicator; it recovers on its own. App-wide, one control-socket
  subscription registered as the attached desktop client wakes the right feed on
  `ServerMsg::Changed` (an unregistered subscription; detach and relaunch
  arrive through bootstrap's registered watch). Session title / exit
  events have a `SessionSink` for the terminal views. Events cross to gpui over a
  futures channel; spawn, request and drop never block the window.
- **Settings** (`src/views/settings.rs`), from the sidebar footer or ⌘,: terminal
  font family and size, theme (Auto / Dark / Light, the `theme:` key), the
  `desktop.cli_path` override, and "Open keys.yaml". A theme choice applies at
  once (`theme::apply_preference`). Saved through the new
  `shelbi_state::update_user_config`, which keeps every key it doesn't own.
- **Window state** (`src/window_state.rs`): frame, sidebar width and collapse per
  project in `~/.shelbi/projects/<name>/desktop-window.json`, saved debounced and
  off the UI thread.
- **Commands** (`src/commands.rs`): `ShowView` (all four views; Issues shows the
  board), the board's issue mutations (`HostCall::IssueCommand`, to the board),
  `SwitchProject`, `Detach` (quits the app, agents keep running),
  `DetachOtherClients`, `QuitProject` and `QuitShelbi` (both behind a native
  confirmation) now do real work.
- UI font: Geist, vendored (OFL) under `assets/fonts/geist/`. New Heroicons:
  magnifying-glass, cog-6-tooth, chevron-up-down, chevron-double-left, bars-3,
  check, signal-slash.

### Shared crates

- `shelbi-app::view`: `SidebarModel::rows()` / `SidebarRow` / `SidebarTarget`,
  `SidebarModel::skeleton`, `default_nav_items`, `WorkspaceRow::right_label`,
  `title_case`. The TUI's `build_rows` is now a map over `rows()`.
- `shelbi-app::changes`: the hub change subscriber, moved from
  `shelbi-tui/src/shell/changes.rs`, plus `spawn_observed` (a connected flag).
- `shelbi-app::nav::ClientState::select` (click selection).
- `shelbi-orchestrator::sidebar_model`: the IO-backed `read_sidebar_model` /
  `read_config_skeleton`, moved from `shelbi-tui/src/shell/sidebar_model.rs` so
  both clients read the sidebar the same way. `shelbi-orchestrator` now depends
  on `shelbi-app` (the root `Cargo.lock` gains only that internal edge).
- `shelbi-state::user_config`: `desktop.terminal_font_family` /
  `desktop.terminal_font_size` (beside the landed `theme` and
  `desktop.cli_path`) and `update_user_config`.

## Rebase onto dt-issues-board

`dt-issues-board` (#1691) landed on the umbrella after this branch was handed
off; the branch was rebased onto it (`bba32b70`) and both subtasks' behavior
kept:

- `commands.rs`: `CommandHost::show_issues` folded into `show_view` (Issues is a
  routed view like the rest, with no stub: `view_stub_id(Issues) = None`); the
  board's `issue_command` kept, so `MoveIssue` / `StartIssue` / `AssignIssue` /
  `EditIssue` / `AddIssue` still run on the board.
- `app.rs`: this branch's window, plus the board: `ensure_issues` builds an
  `IssuesView` for the window's project on first show, subscribes to its
  `IssuesEvent::Command`s (they go through `run`, so the relaunch gate and the
  board's "bootstrap not Ready" gate apply to sidebar and board alike), and the
  main area renders it when the routed view is Issues. Keyboard focus follows
  `ClientState`: showing the board or Ctrl+L hands focus to the board (its own
  `kanban:` / `popover:` keys), Ctrl+H takes it back to the sidebar. Keys the
  board doesn't bind bubble to the window, so the global chords keep working.
- `src/project.rs` (the board's stopgap "which project does the window show") is
  gone: its launch order (argument / `$SHELBI_PROJECT`, the project containing
  the working directory, the first open project, then the most recent) now lives
  in `projects::launch_project`, and the board gets the window's project.
- `keys.rs`: both branches had written the same gpui→`KeyChord` conversion; this
  branch's (shifted punctuation as the typed character, F1–F12 only) is kept,
  with the board's `H` / `shift-up` cases added to its tests. Both the sidebar
  and the board call it.
- `assets.rs`, `views/mod.rs`, `shelbi_app` module list, `Cargo.toml` /
  `Cargo.lock`: unions of both sides (`board` and `changes`; `chrono`,
  `markdown`, `futures`, `serde`, `serde_json`).

## What's left

- `AddProject` stays a stub: it is an overlay form in the TUI, so it belongs with
  `dt-palette-overlays`, as do the search box and the palette chord (both show
  the stub banner today).
- The font settings are stored; the terminal element reads them once it lands.
  A changed CLI path takes effect on the next launch (bootstrap reads it).
- A launch with no registered project logs and quits; a first-run screen (with
  Add Project) belongs with the overlays.
- The app menu's "Settings…" item belongs to `dt-menus-notifications` (⌘, is
  bound already).

## Decisions

- **Shared layout, not a port.** The TUI computed its rows inside its renderer;
  that layout moved to `shelbi_app::view` and both clients map over it. The
  IO-backed reader needs `shelbi_orchestrator` (review-slot serving, branch
  names), which `shelbi-app` must not depend on, so it moved to
  `shelbi-orchestrator` instead; the desktop depends on it by path.
- **Nav icons are Heroicons**, not the TUI's emoji (the desktop's icon rule);
  every other glyph (state badges, `▾ ▸ ⎇`) is the TUI's character. Section
  headers drop the TUI's `— … —` decoration; the labels are the same. There is
  no Machines nav row, matching the TUI (it is a palette entry there).
- **Opening a project runs the CLI** (`<cli> --project <name> orchestrate`):
  spawning a session in process re-executes `current_exe()`, which here is the
  app. The CLI and environment are bootstrap's.
- **Bootstrap once, not per window.** Rerunning it in each project window would
  put progress screens where the target's chrome must paint on the first frame.
- **Config shapes** mirror `dt-theme-tokens` / `dt-platform-bootstrap`, which
  landed while this was in flight; the rebase only added the font fields.
- **Saves merge** (`update_user_config`) so a desktop save never drops a key this
  build doesn't model. YAML comments in `config.yaml` are not preserved (true of
  every existing save path too).
- **Widths in TUI columns** (9px): the stored width, the default (28), the
  minimum (24) and the clamp are the shared model's, so the rule is literally the
  TUI's. A drag moves in 9px steps.
- **Daemon link** follows the hub change subscription: while it is down the feed
  polls every 750ms and the sidebar says so; while up it refreshes on each change
  plus a 5s safety refresh.
- **Detach quits the app** (one client, every window); closing the last window
  quits too. Agents keep running either way.
- **`q` / Ctrl+C in the sidebar do nothing**: they close the TUI, but a window
  closes with ⌘W.
- Window state is a small JSON read before the window opens (it decides the
  frame); every other read and every write is off the UI thread.

## Verification

### After the rebase onto dt-issues-board

Run locally on an arm64 Mac (Apple M4), 2026-10-14, on the rebased tree.

From `desktop/`:

- `cargo build --locked`: clean (only the upstream `block 0.1.6` note).
- `cargo clippy --locked --all-targets -- -D warnings`: clean.
- `cargo test --locked`: 92 passed. That is this branch's 73, the board's 19
  new tests (the board's two `keys` tests were folded into this branch's `keys`
  tests, since both branches wrote the same converter), and 2 new `app` tests
  (the Issues row routes to the board with no stub; the board's mutations reach
  the board).

From the repository root:

- `cargo check --workspace --locked`: resolves, no lockfile change.
- `cargo build --workspace`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo +1.88 check --workspace --all-targets --locked`: clean.
- `cargo test --workspace`: 3863 passed, 0 failed.

By hand, against a contained `SHELBI_HOME=/tmp/shbh` (one scratch project, the
runner `sleep 3600`, `desktop.cli_path` at this worktree's `shelbi`, sockets
unset), driving the app with CGEvent keys: bootstrap reached Ready and the shell
painted; Ctrl+H, `j`, Enter in the sidebar showed the native board in the main
area (three backlog cards, the board's footer keys); `j`, Enter opened the task
detail inside the shell (Markdown body rendered); Esc, Ctrl+H, `k` moved the
*sidebar* selection to Orchestrator while the board's card selection stayed put
(focus handed back). Torn down with `shelbi -y quit --all` against that home.

### Before the rebase

Run locally on an arm64 Mac (Apple M4), 2026-10-14.

From `desktop/`:

- `cargo build`: clean (only the known upstream `block 0.1.6` future-incompat note).
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test`: 73 passed (after rebasing onto the landed bootstrap and theme
  subtasks), including the gpui test-context render tests
  (`views::sidebar::render_tests`: every row of a fixture model is drawn in
  model order, the selection marks the right row, a skeleton shows "Loading…",
  polling shows the indicator, clicks activate the right ordinal), the
  gpui-key-to-`KeyChord` conversion (`keys`), settings round-trip
  (`views::settings`), window-state persistence (`window_state`), and the feed
  (`refresh`: a 400ms reader never blocks spawn / request / drop, a lost daemon
  degrades to polling and recovers on its own, change wakes refresh).

From the repository root (shared crates changed):

- `cargo check --workspace --locked`: resolves; the root `Cargo.lock` gains only
  the internal `shelbi-orchestrator → shelbi-app` edge, no version changes.
- `cargo build --workspace`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo +1.88 check --workspace --all-targets --locked`: clean.
- `cargo test --workspace --no-fail-fast`: 3813 passed, 4 failed. The four are
  `shelbi-cli` integration tests that wait 10s for a freshly spawned
  `shelbi __session` to come up (`old_session_compat`, `remote_session_e2e`,
  `session_cli`, `session_survival`: "session socket never appeared"). They ran
  at a load average of ~78 on 10 cores with sibling workers building, touch
  nothing this branch changes, and flip between runs (`session_survival` and
  `session_cli` each passed on a rerun). Left to CI. The new and moved tests
  (`shelbi-app` view / nav / changes, `shelbi-orchestrator::sidebar_model`,
  `shelbi-state::user_config`, every `shelbi-tui` sidebar test) pass.

By hand in the running app (`cargo run`, screenshots via `screencapture -l`,
input via CGEvents). Before the rebase:

- The real `shelbi` project: live badges for five working workspaces, the Queued
  for Review row with its branch, the version row, the Zen band; clicking Issues
  routed to the `view:tasks` stub (before the board landed; it now shows the
  board); the project title opens the full project list.
- An isolated `SHELBI_HOME` with two scratch projects (one with workspaces on an
  SSH machine at `nohost.invalid`) and `desktop.cli_path: /usr/bin/true`:
  switching opened the second project's window with its name and machine-grouped
  rows on the first frame, the window stayed responsive, and switching back
  focused the existing window instead of opening another.
- Dragging the divider to 38 columns persisted to `desktop-window.json` and was
  restored on relaunch. Ctrl+H, `j`, `j`, Down, Enter walked the sidebar and
  dispatched `FocusWorkspace` for `alpha` (its stub banner).
- Settings: picking Light and entering font size 14 wrote `theme: light` and
  `desktop.terminal_font_size: 14`, keeping `keymap:` and `desktop.cli_path`.
- With `SHELBI_HUB_SOCK` at a socket nobody listens on: "Daemon unreachable ·
  polling" showed; a stand-in listener appearing cleared it on its own; the
  listener dying brought it back.

After the rebase, against a contained `SHELBI_HOME=/tmp/shbg` (two scratch
projects, the orchestrator runner `sleep 3600`, `desktop.cli_path` at this
worktree's `shelbi`): bootstrap found the CLI, started a daemon for that home and
reached Ready; the shell painted in the light theme (`auto`, system light) and
connected live; switching to the second project opened its window 0.4s after
the click with its chrome and machine-grouped rows (no bootstrap screens) while
"Starting the orchestrator…" ran; both projects' `state.json` read `open: true`.
Torn down with `shelbi quit --all` against that home.
