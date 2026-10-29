# launch-desktop: the launch window and Connect to Remote Session

Part of the `launch-screen` feature, on the foundation in
`feature/launch-screen` (`shelbi_app::launch`: routing, the model, recent
remote sessions, the remote connection seam).

## The change

**Start.** The app asks the shared route once (`launch::route`, desktop: no
daemon running means the launch screen; the probe never starts one). With
no daemon it opens the **launch window** instead of restoring project
windows; with one running it starts as before (the saved windows, or the
first launch's CLI setup and wizard).

**The launch window** (`views/launch.rs`, Figma "Startup · Projects and
remote sessions" 2539:10949 and "Startup · No projects yet" 2544:10318):
900 × 620, not resizable, the traffic lights over its 320pt left panel
(the sidebar surface): "Welcome to Shelbi", the line under it, Add project
and Connect to remote. The right panel lists Projects on this Mac (32pt
folder, name, `~`-shortened path), most recently opened first, then Recent
remote sessions (name, `user@host:path`, Connect); a section with no rows is
left out. With nothing to list it shows "No projects yet", and Add project
becomes the Primary button.

```
┌────────────────────┬──────────────────────────────────────────┐
│ ● ● ●              │                                          │
│                    │  Projects on this Mac                    │
│                    │ ┌──────────────────────────────────────┐ │
│ Welcome to Shelbi  │ │ 🗀  Shelbi                            │ │  (selected)
│ Open a project on  │ │    ~/Workspaces/shelbi               │ │
│ this Mac or …      │ └──────────────────────────────────────┘ │
│ [ + Add project ]  │   🗀  ContextStore                        │
│ [ ⛁ Connect to … ] │                                          │
│                    │  Recent remote sessions                  │
│                    │   🗀  Shelbi                    [Connect] │
│                    │      jlong@devbox:~/Workspaces/shelbi    │
└────────────────────┴──────────────────────────────────────────┘
```

- **A project** opens through the shared `launch::open_project`: off the UI
  thread, the registration is checked, the daemon starts, the project is
  stamped as opened; then its window opens (or the window already showing
  it comes up). The daemon start is the app's own bootstrap
  (`platform::launch::BootstrapDaemon`: find the CLI, installing it when
  there is none, start the daemon, connect), whose result the new window
  shares (`app::share_ready`) and whose relaunch and detach pushes keep
  reaching the app, as the first launch's setup window's do. Once a window
  has bootstrapped, the shared `SharedDaemon` start is used instead. The
  row shows a spinner meanwhile; a failure (the daemon's, or a project
  gone since the list was read) shows under the lists.
- **The launch window closes** as soon as any project window opens
  (`launch::project_window_opened`, called from `app::open_window`), so a
  project opened from the wizard or a notification closes it too.
- **Add project** opens the existing Add Project wizard (`Origin::Workspace`:
  its Back closes it, leaving the launch window). The project it creates
  opens in a window of its own.
- **Connect to remote** opens the dialog over the launch window; a recent
  session's **Connect** reconnects (`launch::connect_remote`: connect, then
  remember), and the list re-reads so it is on top.
- **Keys**: ↑ / ↓ move through the rows (projects, then remote sessions),
  the pointer over a row selects it too, and the first starts selected, as
  the frame draws it; Return opens the selected row (Add project when there
  are none); ⌘W closes the dialog if open, else the window (the last window
  quits the app, as any window's).
- Frontmost, the launch window installs its own menu bar (no project, so
  only what needs none), with Shelbi ▸ Connect to Remote Session… live.

**Connect to Remote Session** (`views/connect_remote.rs`, Figma "Host"
2539:10950 and "Choose session" 2539:10951) is one dialog, a modal layer on
the scrim over whichever window asked: the launch window (its button, or
the menu with no project window frontmost), a project window (Shelbi ▸
Connect to Remote Session…, under Settings…, per 2544:10362; and the
palette). 580pt wide, 32pt in, 26pt corners, at least the frames' heights
(340 and 456) with the actions at the bottom.

- **Host**: "Connect to remote", "Connect to Shelbi running on another
  machine.", the SSH host Textbox (focused) and its note, Cancel and Find
  Sessions (Primary). While the search runs, Find Sessions is the disabled
  "Finding Sessions…" with a spinner. An error (an invalid host, SSH
  unreachable, no Shelbi there, no sessions there) shows in red under the
  field, the field's edge red; editing the host clears it.
- **Choose session**: "Choose a session on <host>.", the green check and
  "Connected as <user>" with Change host (back, the host kept), the
  sessions as the launch window's cards each with its own Connect (the
  first selected), then Cancel and Connect (Primary) for the selected one,
  "Connecting…" while it runs, an error under the list.
- **Keys**, ahead of the field: Return is the screen's primary action, Esc
  cancels, ↑ / ↓ move through the sessions; ⌘W cancels it.
- Connected: the session is remembered and the dialog closes (the launch
  window re-reads its list; a project window flashes "Connected to …").

The dialog's state is the TUI's (`launch-tui`), shared and toolkit-free:
`shelbi_app::launch::connect::ConnectFlow` (the two steps, the labels, what a
press does, busy while a call runs). The desktop adds what is its own: the
keys (Return, Esc, the arrows), the pointer selecting a row, and whether the
primary button can be pressed.

## The remote connector

The app's connector is `shelbi_app::launch::SshRemote` (the user's own
`ssh`; `launch-remote-connect`, see
[`launch-remote-connect.md`](launch-remote-connect.md)). Tests and
`--visual-figma` run the foundation's `FakeRemote` (`shelbi-app`'s
`test-support` feature, now also enabled by `visual-check`). The connector
is the `LaunchServices` global (`platform::launch`), with the recent list.
A connection opens the session's own window
(`views::remote_session::open_remote_session`), and the launch window
re-reads its list.

## Design

Sizes, type, colors and icons are the nodes' (`get_design_context`), as
roles: the left panel `sidebar`, the right `canvas`, a selected card
`background` (12pt corners, 12pt in, 12pt gap), titles `text_loud`, paths
and notes `muted`, the dialog's lines `foreground`, the folder `icon`, the
check `icon_green`; buttons are the Button component (Base 32pt; a row's
Connect Small 28pt). Light, contrast and translucency follow from the
roles (the dialog's panel is opaque, as Contact Us's).

- **32pt icons** are new: the `@large` icon list
  (`assets/icons/heroicons-32/`, `ICON_SIZE_LARGE`), the frames' two
  folders, a card's (the outline drawing scaled, a 2pt stroke) and the
  empty state's (1.25pt).
- **Copy** follows the frames: the dialog's heading is "Connect to remote"
  (`remote::DIALOG_HEADING`; `DIALOG_TITLE`, "Connect to Remote Session",
  stays the command's and the dialog's accessible name), and the
  foundation's "Change Host" became "Change host".
- **Not matched on purpose**: Figma's hovered card (Hover=true, 2565:13581)
  has an 8pt gap where the resting card has 12, so its title sits 4pt to
  the left; the cards keep 12 so nothing shifts as the selection moves.
  The Choose session frame lists `jlong@devbox:…` rows under "Choose a
  session on build-server."; the app's rows name the host searched.

## Menus and the registry

`CommandModel::remote_connect` and `launch_screen` are on for the desktop's
windows (and the launch window's model), so the palette lists Connect to
Remote Session… and Show Launch Screen, Shelbi ▸ Connect to Remote
Session… sits under Settings…, and File ▸ Show Launch Screen under New
Window. With no project window frontmost, both (and Add Project…) run
windowless (`menus::run_windowless_command`). The TUI's flags stay off until
`launch-tui`.

## Verification

- `cargo test` (desktop): 818 passed. New: `views::launch::tests` (both
  states, keys, open with the daemon started first through `FakeDaemon`, a
  failed start, a project gone, Add project, reconnect and its failure, the
  dialog's two screens end to end, inline SSH errors, Change host, Esc, ⌘W,
  the window closing once a project window opens, the menu item),
  `app::connect_tests` (the menu's item opens the one dialog over a project
  window, ⌘W cancels it, a connection closes it) and `platform::launch`. The smoke tests now run
  the two commands through the palette and the menus.
- `cargo clippy --all-targets -- -D warnings` (with and without
  `visual-check`); root `cargo clippy --workspace --all-targets`,
  `cargo check --workspace --locked`, `cargo test -p shelbi-app`,
  `cargo test -p shelbi-tui launch`.
- `SHELBI_VISUAL_ONLY=launch cargo run --features visual-check --
  --visual-figma <dir>`: `launch-projects`, `launch-empty`,
  `launch-connect`, `connect-host`, `connect-host-finding`,
  `connect-host-error`, `connect-choose`, `connect-choose-error`, dark and
  light, compared with the frames.
- The real app in a sandboxed `SHELBI_HOME` with the hub's socket variables
  unset logged "no daemon running: the launch window" and opened it.
