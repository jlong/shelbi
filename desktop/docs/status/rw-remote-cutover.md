# rw-remote-cutover

Phase P10 of the remote-window plan (ContextStore `Shelbi`,
`Plans/remote-session-window.md`, sections 6, 9 and 11): the native window
is the default for a remote project. The P6 dev setting
(`desktop.native_remote`, `SHELBI_NATIVE_REMOTE`) is gone.

## What landed

- **The decision, always.** Connect to Remote Session and a recent remote
  session read the host's listing and decide with the P4 rule
  (`RemoteListing::window_decision`: the far version equals this build's
  exactly and its relay, control and query protocol numbers match). A match
  opens the native window (`app::open_remote_window`); anything else (another
  patch release, a Shelbi that predates `native` in its listing) opens the
  terminal window with the notice "Shelbi on devbox is 0.12.1 and this Mac
  has 0.13.0, so it opens in its terminal view. Update both to get the full
  window." `remote_window::choose` no longer takes a flag; `decide_with` no
  longer takes one either.
- **Every reconnect decides again**
  (`shelbi_app::remote_window::reconnect_check`). The check a `HostLink`
  runs before it reopens a channel is now the daemon check
  (`RemoteConnector::connect`, never starting it) plus a fresh listing and
  the decision. A host that no longer matches refuses with
  `RemoteError::NoLongerNative` and no channel opens, so nothing reaches a
  daemon that may reject it. The pane connector
  (`RemoteSessionConnector::channel`), the window's link watch
  (`RemoteWindow::keep_up`) and Start Shelbi all use it.
- **Reopen.** A window whose watch or pane reconnect gets `NoLongerNative`
  (`ShelbiWindow::host_changed`) closes its panes and views
  (`close_remote_panes`, the switch teardown) and draws, in place of its
  shell, "Shelbi on devbox changed" with the line ("Shelbi on devbox is now
  0.14.0 and this Mac has 0.13.0. Reopen to continue in its terminal view.",
  `remote_window::changed_notice`) and a Reopen button
  (`bootstrap::render_remote_changed`, `REOPEN_LABEL`). Reopen closes the
  window and runs `open_remote_session` again, which decides afresh.
- **Open in Terminal View** (`CommandKind::OpenTerminalView`,
  `action:open-terminal-view`, `Effect::OpenTerminalView`). Listed only when
  the model has a `remote_host` and a project, hidden until typed in the
  palette; View ▸ Open in Terminal View is a fixed slot, gray in a window on
  this machine. The desktop opens the far TUI over `ssh -t` in its own
  window beside the native one (`remote_session::open_terminal_view`, no
  notice). The TUI's model never lists it; its `dispatch_effect` arm focuses
  the main area and says it is already the terminal view (a remote project
  opens in the TUI over `ssh -t`). Scenario:
  `open_in_terminal_view_reaches_the_host_only_in_a_remote_window`.
- **Two hops, until P9.** A workspace pane whose far session is missing asks
  the far source for the workspace's details; when the far project declares
  it on another of its machines (`is_remote`), the pane showed a placeholder
  naming the machine instead of idle. P9 (`rw-remote-two-hop`) replaced it
  with the real stream.
- **New task over the link.** The New task dialog's write-up hand-off goes
  through the window's `ProjectLink` (`new_task::create_linked`, already
  wired by the merge of #2106); now covered end to end
  (`remote_sessions::new_task_hands_the_write_up_to_the_far_orchestrator`:
  the add on the far daemon, the write-up typed into the far
  orchestrator).

## Not in this phase

- Two-hop sessions (P9, `rw-remote-two-hop`, since landed).
- An existing `desktop.native_remote:` line in a user's `config.yaml` is
  ignored on read (the desktop block doesn't deny unknown keys) and dropped
  the next time Settings saves the desktop block. No config-upgrade sniffer:
  it was a dev setting no shipped default ever wrote.
