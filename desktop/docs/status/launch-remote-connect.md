# launch-remote-connect

Connect a Shelbi client on this machine to Shelbi running on another machine
over SSH. Part of the `launch-screen` feature; builds on the foundation
(`shelbi_app::launch`) and plugs into the launch-desktop and launch-tui
dialogs.

## What landed

- **The connector** (`shelbi_app::launch::ssh::SshRemote`). One `ssh <host>`
  per step with `BatchMode=yes` (the user's keys, agent and `~/.ssh/config`;
  never a password prompt; nothing stored but the host) and no `-R` forward
  of this hub's socket. A short `sh` script finds the far `shelbi` the way
  `shelbi machine setup` does and reads `shelbi remote list --json`. Errors:
  unreachable, sign-in refused, host key not trusted, Shelbi not installed,
  version mismatch (same `major.minor`, as for remote machines), daemon not
  running.
- **The far daemon** is never started unasked: `connect` checks it, and the
  dialog's primary button becomes **Start Shelbi** (`ConnectFlow::start` /
  `started`, `RemoteConnector::start_daemon`, `shelbi __ensure-daemon` on the
  host).
- **The Remote Session window** (`views::remote_session`). The far Shelbi's
  own TUI for the project, over `ssh -t`, in a local Shelbi session
  (`remote/<host>/<project>`, `shelbi_app::launch::terminal::TerminalConnector`)
  shown by a `SessionView`. The titlebar row reads "Connected to user@host ·
  Project" with a dot (green connected, yellow reconnecting).
  `SessionView::set_ssh_terminal` makes `ssh`'s exit 255 a lost link, so the
  view shows the shared "Reconnecting to host… (attempt N)" and backs off;
  each attempt re-checks the host first. Quitting the far Shelbi closes the
  window; closing the window ends the local `ssh`.

## Decisions

- A remote session is the far machine's own TUI, not this app's views over
  the wire: both clients read project state straight from the local
  `~/.shelbi`, so a forwarded daemon socket alone can't draw a remote
  project. The TUI over `ssh -t` always matches the far version and
  configuration. A native remote client would be its own project.
- Connecting from the launch window (no bootstrap yet) finds the CLI the way
  bootstrap does, off the UI thread, without starting this machine's
  daemon.

## Verified

- `cargo clippy --all-targets -- -D warnings`, `cargo test` (desktop), and the
  root workspace's clippy and tests.
- gpui tests: the window's titlebar and dot, a dropped `ssh` reconnecting
  with the notice and coming back, exit 0 closing the window and ending the
  local session (`views::remote_session`); `SessionView` treating 255 as a
  lost link only for an ssh terminal; the launch window's dialog offering
  Start Shelbi and then opening the window (`views::launch` tests).
