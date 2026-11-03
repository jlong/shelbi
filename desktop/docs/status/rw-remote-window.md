# rw-remote-window

Phase P6 of the remote-window plan (ContextStore `Shelbi`,
`Plans/remote-session-window.md`, sections 4.3, 8.1 and 11): the full
native main window for a project on another machine, behind a developer
setting. With the setting off, nothing changes.

> Since P10 (`rw-remote-cutover`) there is no setting: the native window is
> the default, and the section below is history.

## Turning it on

`desktop.native_remote: true` in the viewer's `~/.shelbi/config.yaml`, or
`SHELBI_NATIVE_REMOTE=1` for one launch (`1`/`true`/`yes` on, `0`/`false`/`no`
off; anything else leaves the setting). Settings doesn't show it, and saving
Settings keeps it. The far machine's config never decides it.

## What landed

- **The decision** (`shelbi_app::remote_window::choose`, desktop
  `views::remote_session::decide`). Connect to Remote Session and a recent
  remote session read the host's listing (`RemoteConnector::listing`) only
  with the flag on. A host on exactly this version, speaking this build's
  relay, control and query protocols, opens natively; any other opens
  today's terminal window with one line under its titlebar: "Shelbi on
  devbox is 0.12.1 and this Mac has 0.13.1, so it opens in its terminal view.
  Update both to get the full window." With the flag off the host is never
  asked and the terminal window opens exactly as before.
- **The window** (`app::open_remote_window`, `RemoteWindow`). The same
  `ShelbiWindow` over a remote `ProjectHandle`
  (`remote_window::remote_handle`: `RemoteSource`, `ProjectLink::remote` and
  `RemoteSessionConnector`, all over the host's one shared `HostLink`). The
  title and the sidebar name `jlong@devbox` and the project; the sidebar's
  host row carries a dot, green while the far change stream is live, yellow
  while it reconnects. The first frame paints the listing's name for the
  project. Its project switcher and the palette's projects list only the far
  host's projects (from the listing). Opening the same remote project again
  brings its window up.
- **Commands.** Issue moves, starts, assigns, adds, edits, approvals,
  rejections and reordering go to the far daemon through the relay's
  `control` service, as do review and workspace content sessions and Quit
  Project. The window never brings the far orchestrator up and never runs
  this machine's `shelbi open` for it.
- **Project-state writes** (P7, `rw-remote-state-writes`): Toggle Zen, the
  board's workspace filter and column overrides, the error log's read marks
  and Clear, a failed toggle's error entry, and a reopened pane's relaunch
  marker go to the far daemon as `ClientMsg::WriteState` (control protocol
  v7) over the same `control` service. A local window sends the same message
  to this machine's daemon (`shelbi_app::state_write`), so the daemon is the
  one writer either way and tells its other clients with a `Changed` note.
  The relay refuses a write to any project but the window's own. The Zen
  intro's "don't show again" stays the viewer's own, on this machine.
- **Not yet available** (P8, plan 8.4; P8 since landed, see
  [`rw-remote-config-writes.md`](rw-remote-config-writes.md)), each disabled with its reason
  through the command path: the registry lists the command with the reason
  as its subtitle (`CommandModel::remote_host`, `CommandModel::unavailable`),
  the menu bar draws it disabled, and the window refuses it with that line
  before any dispatch (no dispatch arm changes). Open in editor
  (edit targets), Add Project, Quit Shelbi (denylisted by the relay), Detach
  Other Clients and New Window. Settings is read-only: the Workflows and
  Agents pages refuse an edit or Edit YAML with the reason, the workflow and
  agent editors and the workspace and machine forms (and so machine checks)
  don't open. A panel's open-in-app and reveal-folder say the folder is on
  the host. Files dropped on a far terminal aren't pasted: the pane shows
  why for a few seconds.
- **Notifications** are on: a remote window's notices are tagged
  `host:project`, and a click opens only that window.
- **Keybindings**: the viewer's keys plus the far project's overlay
  (`project_keymap` over the relay).
- **Far daemon stopped.** Every 3s a window whose link is down asks the host
  (`RemoteConnector::connect`, which never starts anything) and reopens the
  link once it checks out, so a window with no pane (the board) recovers too.
  A stopped far daemon, found there or by a pane's reconnect, shows a bar:
  "Shelbi isn't running on devbox." with Start Shelbi; only that button
  starts it.
- **Reads.** A remote board re-reads every 2s (750ms locally), since each
  read is a round trip.

## Tests

- `app::remote_tests` (desktop): a fake far host
  (`remote_fixture::FarHost`: a fixture project read by the far side's real
  `QueryAnswers` over an in-process relay, a fake far daemon on its
  `control.sock`, a fake far hub). Every view opens and reads the far
  project; a card moved in the window lands on the far board, and a far move
  reaches the window; Toggle Zen and the error log's read marks and Clear
  are made by the far daemon (`StateWrite`s it records); refused commands
  give their reason; the switcher and title name the host.
- `refresh::tests::a_far_changed_note_wakes_a_remote_feed`: a far `Changed`
  wakes a remote feed at once.
- `views::remote_session::tests`: the flag off opens the terminal window
  without asking the host; another far version opens it with the notice; the
  same version opens natively over the link; a stopped far daemon is never
  started by deciding.
- `shelbi_app::remote_window::tests`, `command::tests`,
  `menus::tree::tests::a_remote_windows_unavailable_commands_are_disabled`.

## Left for later phases

A far reload (`ChangeWake::Reload`) doesn't yet close the window's panes and
offer Reopen (plan 6); sessions on the far
project's own machines (P9).
