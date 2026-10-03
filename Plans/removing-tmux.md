# Removing tmux

Status: revised 2026-10-02 after adversarial review
([[removing-tmux-and-desktop-app-review]]) and a second review by Codex (eight
findings, all accepted; summarized in that document). Code baseline `7f331c5`.

## Context

Shelbi uses tmux as its runtime. That has turned into the main obstacle to
adoption, for three reasons John named:

1. **People are unfamiliar with it.** A new user lands inside a tmux session
   they did not ask for, with a prefix key, copy mode, and detach semantics they
   have never learned.
2. **Config and keybinding conflicts.** Shelbi binds keys and hooks on the
   user's tmux server and fights their `tmux.conf`.
3. **Fragile control surface.** Shelbi drives and observes agents through
   `send-keys`, `paste-buffer`, `capture-pane`, and pane-title parsing.

This plan removes tmux completely. Shelbi will own its PTYs, render its own
terminals, and need nothing installed beyond the `shelbi` binary. It must still
run comfortably *inside* tmux or Screen as an ordinary full-screen program, but
it will never create a tmux session, window, or pane again.

A desktop app follows ([[desktop-app-gpui]]), so the architecture is chosen so
that the TUI and the desktop app are two clients of the same backend.

### Decisions

| Question | Decision |
|---|---|
| Persistence | Agents survive quitting or crashing the UI. Clients attach and detach. |
| Who owns the PTYs | One small process per session. No shared host. Chosen because it is the most reliable shape: a bug can cost one agent at most, and upgrades never touch running agents. |
| Remote machines | The `shelbi` binary is installed on each remote. Agents survive SSH drops. |
| Remote binary | Use a compatible `shelbi` already on the remote's PATH; otherwise install to `~/.shelbi/bin`. |
| Concurrent clients | Any number of UIs attached at once. |
| Terminal size with several clients | The most recently active client's size, debounced. |
| Full-screen agents | Left in their default mode. The mouse is forwarded to an agent that has asked for it; Shift+wheel and Shift+drag are Shelbi's own. |
| Terminal features | Scrollback, selection and copy, and search, for sessions on the normal screen. `shelbi attach <workspace>` from any terminal, rendered, not raw passthrough. |
| Reserved keys | One chord, Ctrl+Space, opens the palette. Everything else is a palette command or a mouse action. |
| History on disk | A readable text snapshot when a session ends. The raw output log is opt-in per project. |
| Daemon lifecycle | Started on demand. The launchd and systemd units are retired. Session processes restart it if it dies. |
| Mutations | Executed by the daemon, one per issue at a time. The CLI, TUI, and desktop app send commands to it. |
| Rollout | Hard cut at parity. No long-lived tmux fallback. |
| Platforms | macOS and Linux now. New code is Windows-ready: session transport, locking, and PTYs sit behind abstractions (`portable-pty` covers ConPTY; a named pipe can stand in for the socket). Existing Unix-only code is not ported here. |
| Legacy agent commands | `shelbi spawn`, `archive`, `tail`, the old `attach`, and `merge` are removed at cutover. |
| Desktop | Sibling of the TUI at feature parity. The shared app model is built here, in Phase 4. |

## What tmux does for Shelbi today

tmux does five separate jobs, and each needs a replacement.

**1. Keeps processes alive.** The tmux server owns every agent PTY, so agents
outlive the UI. Local workspaces are windows in `shelbi-<project>`; remote
workspaces are their own session `shelbi-w-<ws>` on the remote's tmux server
(`workspace_tmux_addr`, `shelbi-orchestrator/src/workspace.rs:98`).

**2. Is the window manager.** The whole UI is tmux layout:

- `ensure_dashboard` (`shelbi-orchestrator/src/lib.rs:468`) creates the
  session, splits sidebar and orchestrator panes, and creates a hidden stash
  session `_shelbi-<p>` holding the tasks, machines, and activity views, each
  a separate looping process in its own pane. Tasks and activity are ratatui
  programs. Machines is a shell loop that reruns `shelbi workspace list`
  every five seconds (`lib.rs:1758`).
- `show_view` (`lib.rs:368`) changes the right-hand view with `swap-pane`.
  Workspaces are reached with `select-window`. Remote workspaces get a local
  proxy window running `ssh -t host tmux attach`
  (`shelbi-cli/src/commands/open.rs:157`).
- The review interface (`review_ui.rs`, 2,197 lines) is built from
  `split-window`, `swap-pane`, `join-pane`, and `break-pane`.
- Five overlays are **separate `shelbi-cli` processes** launched in
  `display-popup`, each with its own event loop, returning results through
  temp files: palette (2,916 lines), review confirm, reject reason, error
  log, and zen intro. About 5,000 lines in total.
- UI state lives in tmux: session environment (`SHELBI_PANE_*`,
  `SHELBI_CURRENT_VIEW`, `SHELBI_REVIEW_*`, `SHELBI_SIDEBAR*`), window options
  (`@shelbi-user-shell`, `@shelbi-launch-epoch`), a global `bind-key` for the
  palette, a global `session-closed` hook, and per-session resize hooks.
- `run_main` (`shelbi-tui/src/lib.rs:150`) ends in `exec tmux attach` or
  `exec tmux switch-client`.

**3. Is the input channel.** `send_text` (`shelbi-tmux/src/lib.rs:402`) uses
`send-keys -l` or `load-buffer` plus `paste-buffer -p`. `submit.rs` wraps this
in `send_verified`, which captures the pane before and after to confirm the
text landed, mainly to survive the race between a paste and the Enter that
follows it. Remote agents are launched by pasting the launch command into an
interactive login shell (`workspace.rs:2556`).

**4. Is the observation channel.** The poller (`shelbi-tui/src/poller.rs`,
5,390 lines of code and 6,072 of tests) polls each workspace for liveness,
pane contents (usage-limit, dialog, and stall detection), and the pane title.
The `shelbi:<state>` title is set by an OSC 2 escape from hook scripts
(`workspace.rs:3833`), with no tmux command involved, so that signal carries
over unchanged.

**5. Is the remote runtime.** Remotes need only tmux and an agent CLI; there is
no shelbi binary there.

Structural facts that shape the redesign:

- **The poller runs inside the sidebar process** and also drives layout: it
  calls `ensure_dashboard` to relaunch a crashed orchestrator
  (`poller.rs:3962`) and several `review_ui` functions that build and tear
  down panes.
- **The daemon is small, hub-global, and thread-based.** It is started by a
  launchd or systemd unit with `KeepAlive` and a one-second throttle
  (`daemon/supervise.rs:699`). Its environment is a minimal baked PATH that
  already needs healing to find `gh`.
- **tmux-specific code is spread wider than the Cargo graph suggests.**
  Beyond `shelbi-tmux` and its three dependents, it lives in `shelbi-state`
  (`to_tmux_key`, `tmux_palette_key`, three instruction templates),
  `shelbi-core` (`TmuxAddr`), and `shelbi-agent` (launch strings).
- **About 78 call sites are session operations** through `shelbi-tmux`.
  **About 200 are raw layout calls** (`lib.rs` 66, `review_ui.rs` 51,
  `workspace.rs` 20, and others).
- **Shipped agent instructions tell agents to run tmux**
  (`default_orchestrator.md.template:588`, `default_review.md.template:85`,
  `skills/load_run_detection.SKILL.md:62`), and review events carry a tmux
  target.
- **The Codex orchestrator is three processes in one pane** (app-server,
  bridge, remote TUI) that depend on duplicated pane stdin and `$TMUX_PANE`
  (`lib.rs:1521`, `:1697`).

## Target architecture

```
 clients (any number, attach and detach freely)
 +-------------+  +----------------+  +-----------------+  +--------------+
 | shelbi TUI  |  | shelbi attach  |  | desktop app     |  | shelbi CLI   |
 | (ratatui)   |  | (one session)  |  | (gpui, later)   |  | send, etc.   |
 +------+------+  +-------+--------+  +--------+--------+  +------+-------+
        |                 |                    |                  |
        +-----------------+----- shelbi-client +------------------+
                                   |
            one socket per session under ~/.shelbi/sessions/
                                   |
   +-------------------+  +-------------------+  +-------------------+
   | session process   |  | session process   |  | session process   |
   |  PTY + emulator   |  |  PTY + emulator   |  |  PTY + emulator   |
   |  agent: alpha     |  |  agent: bravo     |  |  orchestrator     |
   +-------------------+  +-------------------+  +-------------------+
                                   ^
                                   |  same client crate
 +---------------------------------+----------------------------------------+
 | shelbi daemon  (hub only, restartable at any time)                       |
 |   existing hub.sock verbs, board refresh                                 |
 |   NEW: per-project poller and supervision (moved out of the TUI)         |
 +--------------------------------------------------------------------------+

 remote machine:  session processes  <-- ssh host shelbi relay (stdio) -- hub
```

### One process per session

Each agent, shell, editor, or orchestrator runs under its own small session
process. It owns that one PTY, runs that one emulator, and listens on its own
socket. There is no shared host and no server to keep alive.

This is the shape dtach, abduco, and shpool use, and it was chosen over a
single host for reliability:

- **Smallest blast radius.** The worst any bug can do is end one session, and
  supervision already redispatches a dead workspace.
- **Upgrades never touch running agents.** A running session keeps the code
  it started with. New sessions use the new binary. Nothing waits for an idle
  moment.
- **Everything else can restart freely.** The daemon, the relay, and every
  client hold no PTYs.
- **The emulator sits beside its PTY**, so it can answer the agent's startup
  queries with nothing else running.

The costs, accepted: no central registry (sessions are discovered by scanning
a directory), the daemon holds one connection per session, and an emulator
fix reaches only sessions started after it.

**Compatibility with old sessions.** Because a session keeps the binary it
started with, newer clients, relays, and daemons must keep controlling
sessions that may be weeks old. A version number in the hello detects a
difference; it does not solve it. The policy:

- The session protocol has a **frozen core**: hello, attach with replay,
  output, input, resize, snapshot, kill, and the exit event. These frames
  never change meaning and are never removed.
- Everything else is an **additive capability** announced in the hello.
  Clients use a capability only if the session announced it, and fall back
  to the core otherwise.
- There is therefore no "session too old" state. An old session is always
  attachable and killable; it may lack newer conveniences.
- A test in CI runs the current client against session binaries built from
  each previous release.

This is a different policy from the daemon and apps, which must match
exactly (see "Versions and upgrades").

**Sessions restart the daemon.** With the service units retired, something
must bring the daemon back if it crashes while no UI is attached. Each session
process checks periodically whether the daemon's lock is held; if it is not
and the session's project is marked open, it starts the daemon. The daemon's
existing single-instance lock makes the race between sessions harmless. An
open project always has an orchestrator session, so there is always a
watcher. This is a few lines in the session process and keeps the chosen
lifecycle: no installed service, nothing running when nothing is open.

**Layout on disk.** `~/.shelbi/sessions/<short-id>/` holds `sock`, `lock`,
`meta.json` (name, argv, cwd, task, launch time, protocol version), and after
exit `exit.json` and `final.txt`. The directory name is a short hash, not the
session name, to stay under the 104-byte socket path limit on macOS;
`meta.json` carries the readable name (`<project>/orch`,
`<project>/ws/<workspace>`, `<project>/review/<slot>/<role>`,
`<project>/shell/<workspace>`). A session whose lock is not held is dead, and
its directory is stale state to be reaped.

**Spawn.** A client runs `shelbi __session` detached, passing argv, cwd,
initial size, metadata, and **an explicit environment**. The session process
never inherits the environment of whatever happened to launch it.

- The environment comes from the user's interactive login shell, captured
  once (`$SHELL -l -i -c env`) and cached, because `.zshrc` is where nvm,
  fnm, and Homebrew PATH setup usually live and a plain `-l -c` skips it.
- Terminal-identity variables are scrubbed (`TMUX`, `TMUX_PANE`,
  `TERM_PROGRAM`, `STY`). The session sets `TERM=xterm-256color`,
  `COLORTERM=truecolor`, and its own `TERM_PROGRAM=shelbi`. No custom
  terminfo, so nothing to install on remotes.
- On Linux the process is started with `systemd-run --user --scope` where
  available, with lingering enabled, because logind's `KillUserProcesses`
  otherwise kills it at logout. Falls back to `setsid`. All stdio is
  redirected so a launching `ssh` does not hang.

**Emulation.** One headless emulator per session process. The crate is chosen
in Phase 0 between `alacritty_terminal` (vendored, because replay needs state
it keeps private, and Zed itself pins a fork) and a `vt100`-family crate that
can already serialize its screen. The deciding requirement is full access to
both screen buffers, saved cursors, modes, and history, since replay
serializes all of it. Whichever is chosen:

- The kitty keyboard protocol is enabled, since Claude Code's Shift+Enter
  depends on it.
- **The session process is the only thing that answers terminal queries**
  (cursor position, device attributes, colors). It answers at once, with no
  clients attached. Codex exits if the cursor-position reply is late. Clients
  report their foreground and background colors on connect so the session has
  real values for color queries; before any client has connected it answers
  with a dark default.

**Exit.** When the child exits, the session process writes `exit.json`
(status, time, reason) and `final.txt` (the last screen and recent history as
readable text), then exits itself. That replaces liveness polling, the
`--as-pane` wrapper's exit event, tmux's `remain-on-exit`, and the
`capture-pane` tail used for crash records.

**History.** 10,000 lines of scrollback in memory per session, plus a bounded
ring of recent raw bytes. The full raw output log exists but is off unless a
project enables it, because for full-screen agents it is repaint noise that
includes anything pasted.

**PTY.** `portable-pty`. Phase 0 checks controlling terminal and process
group handling, that `kill` reaches the whole group, and that no descriptors
leak into children.

### The protocol

Length-prefixed frames, one byte of frame type, a versioned hello on connect.
Control frames carry JSON; output and input frames carry raw bytes.

| Request | Purpose |
|---|---|
| `hello` | Protocol version, client colors, client capabilities |
| `info` | Title, size, mode flags, metadata, child state |
| `attach`, `detach` | Subscribe to output; attach returns a replay first |
| `input` | Raw bytes to the PTY |
| `paste` | Text delivered with bracketed paste if the program enabled it |
| `resize` | This client's viewport |
| `snapshot` | Visible screen as text, optionally with N lines of history |
| `set-meta` | Update metadata |
| `kill` | Signal the child's process group |
| events | Pushed: title changed, bell, resized, child exited |

Requirements the first draft missed:

- **Sequenced output.** Every output frame carries a sequence number, and a
  `resized(cols, rows)` frame travels in the same stream. Clients lock their
  emulator to the session's size, so every emulator reflows at the same point
  in the byte stream. Sequence numbers also make reconnect exact.
- **Backpressure.** Each client has a bounded buffer. A client that falls
  behind is dropped back to a fresh replay; it never blocks the PTY reader
  and never grows a queue without bound.
- **Input arbitration.** Input frames from different clients are written
  whole and in arrival order, never interleaved mid-frame.
- **Sizing.** The PTY takes the size of the most recently active client
  (last to send input), debounced. Other viewers clip or letterbox.
- **`snapshot` text matches `capture-pane -p -J`** in shape (joined wrapped
  lines, trailing whitespace handling), because about 80 detector and
  baseline tests in `ready.rs` and `submit.rs` are anchored on it.

**Attach replay.** The session's emulator is the authority, and replay
reconstructs its full state for the new client: scrollback, the normal
screen, the alternate screen if one is active, both saved cursors, the scroll
region, tab stops, character sets, and all modes including the keyboard
protocol stack. The client ends up with an emulator in the same state as the
session's, on both buffers.

An earlier draft relied on making full-screen programs repaint by sending
them a resize signal. That is not reliable: at unchanged dimensions a program
may redraw only differences or nothing (ratatui's documented behavior), and
it never rebuilds the normal screen underneath, so attaching while `vim` or
`less` is open and then quitting it would leave the new client on an empty
screen. A forced repaint may still be sent after replay as a harmless nudge,
but nothing depends on it.

The stream is only split where the parser is at rest, never in the middle of
an escape sequence or a multi-byte character. The serializer is the largest
single piece of Phase 1 and is the reason the emulator choice is a spike
question.

`send_verified` stays, built on `snapshot`. The host removes guesswork about
*how* to paste; it does not remove the paste-then-Enter race.

### Shared client crates

- **`shelbi-proto`:** frame and message types. No I/O.
- **`shelbi-client`:** discover sessions, spawn, connect, blocking request
  API, and a reader thread that delivers output and events over channels.
  Runtime-agnostic so gpui can consume it without tokio.
- **`shelbi-term`:** everything about showing a session that is not tied to a
  UI toolkit: the client-side emulator, scrollback navigation, selection,
  search, and input encoding. Input encoding uses termwiz's key encoder (MIT)
  and adds mouse coordinate translation, paste, and focus events. Replies the
  client emulator generates to terminal queries are discarded.

### The daemon takes over the poller

With many clients and no guaranteed sidebar, the poller and supervision
cannot live in a UI process. They move into `shelbi daemon`. The poller file
has no dependencies on the rest of the TUI crate, so the move itself is
mechanical and needs no new crate. The design work is around it:

- **Lifecycle.** The daemon is started on demand by the first client that
  opens a project and exits when no project is open. The launchd and systemd
  units are retired: `shelbi daemon install` is removed, and an upgrade step
  uninstalls existing units so the `KeepAlive` respawn loop cannot fight an
  on-demand daemon. Because the daemon owns no PTYs, `shelbi daemon restart`
  and the version-mismatch flow restart it directly. If it crashes with no
  client attached, a session process restarts it (above).
- **Cancellation.** Two places in today's code abandon a blocked thread on
  the assumption that the process is about to exit: the launch timeout in
  `issue start` (`shelbi-cli/src/commands/issue.rs:1536`) and the poller's
  shutdown, which leaves threads stuck on SSH for the OS to reap
  (`poller.rs:346`). In a long-lived daemon neither is true. An abandoned
  launch can wake after its rollback and start an agent on the wrong task; a
  quit project's stuck poller thread survives while another project keeps the
  daemon alive. So:
  - Every job (a dispatch, a supervision restart, a poll cycle) carries a
    **generation** for its workspace and project. Before it spawns, kills, or
    writes state, it checks that its generation is still current and stops
    if not. A timeout or a project quit bumps the generation.
  - Subprocess calls (SSH, git, `gh`) run with a deadline and are killed on
    cancellation; they are not left to finish on their own.
  - Quitting a project waits for its jobs to acknowledge cancellation, up to
    a bound, before the project is marked closed.
- **Environment.** The daemon runs git, `gh`, workflow actions, and SSH. It
  uses the same captured login-shell environment as sessions, not whatever
  the launching process had.
- **Per-project pollers.** The daemon is hub-global; a sidebar was
  per-project. The daemon gains a manager that runs a poller per open
  project, following the precedent of `spawn_refresh_manager`
  (`daemon/serve.rs:278`).
- **What "open" means.** A project is open when its `state.json` says so,
  set by opening it and cleared by quitting it. It is not derived from the
  orchestrator session existing, because supervision must be able to restart
  a dead orchestrator in a project that is still open.
- **Layout leaves the poller.** `ensure_dashboard`, `close_review_window`,
  `build_review_panel_no_focus`, and `recover_parked_review_agent` are split:
  the session half (start, stop, restart a session) stays with the daemon;
  the layout half becomes an event that clients react to.
- **Probes.** Title and exit become pushed events. Screen sampling for
  stall, usage-limit, and dialog detection stays periodic and calls
  `snapshot`. The three-state probe is preserved: a session is dead, alive,
  or *unreachable*, and unreachable (a remote that cannot be contacted before
  a deadline) is never treated as dead.
- The `hub.sock` NDJSON protocol is unchanged. Workers write to it from shell
  hooks, and that contract stays stable. The daemon additionally pushes
  change notifications to connected clients so they need not poll.

### The daemon executes mutations

With several clients, sharing a library function is not enough. Today's
`move_to` reads the issue, runs the gated merge, then writes the status
(`issue.rs:858`), and the file lock it takes later covers only the write. Two
clients can interleave: one approves and merges while another rejects or
redispatches, and the approval lands stale work and overwrites the newer
status.

So mutations have one owner. Move, start, assign, approve, reject, edit, and
add are commands sent to the daemon over a new control socket. The daemon:

- runs **one mutation per issue at a time**, queued;
- takes an **expected state** with each command (the status and revision the
  client was looking at) and rejects the command with a clear message if the
  issue has moved on;
- **rechecks** that state immediately before any irreversible step (a merge,
  a push, a dispatch);
- **finishes the job if the client goes away**, so closing the app mid-merge
  does not abandon it half-done;
- reports progress and the result back to the requesting client and
  announces the change to all others.

The mutation logic itself still moves out of `shelbi-cli` into a library
(`shelbi-orchestrator`); the daemon is what calls it. `shelbi issue move` and
the rest become thin clients, which starts the daemon if needed. The
existing guard that refuses mutations when the daemon's version differs
already makes the daemon a precondition for them.

### Versions and upgrades

Two policies, deliberately different:

- **Sessions:** long-lived compatibility through the frozen core (above).
- **Daemon, CLI, TUI, desktop app:** exact version match, as the existing
  mutation guard requires (`shelbi-state/src/hub_version.rs:177`). After an
  upgrade, a new CLI restarts the daemon. Clients still running the old
  version are told by the daemon that they are out of date: a TUI re-execs
  itself, and the desktop app shows a relaunch prompt and sends no commands
  until relaunched. Read-only viewing of sessions keeps working in the
  meantime, since that goes through the session protocol.
- **Relays** are started fresh per connection from the installed binary, so
  they are always current, and they speak the frozen core to old sessions.

The upgrade test: upgrade the CLI with an old desktop app, an old TUI, and
an old worker session all running, and confirm the session stays usable, the
daemon restarts once, and both clients are prompted or re-exec'd.

### The TUI becomes one process

`shelbi` starts a single ratatui program that owns the whole screen.

- **Layout:** sidebar on the left, main area on the right. The main area
  shows either a native view (issues board, machines, activity) or a
  terminal view bound to a session. The issues and activity views are
  existing ratatui code that stops being a separate looping process. The
  machines view is new; today it is a shell loop.
- **One event loop.** Four independent loops merge into one. Any call that
  can block (SSH, git, `gh`, opening a review, approving a review, cutting a
  branch) runs off the UI thread and reports back, because in one process a
  blocked call freezes every terminal.
- **Overlays.** Palette, review confirm, reject reason, error log, and zen
  intro are ported from separate CLI processes into in-process overlays.
  `shelbi popup`, the tmux `bind-key`, `to_tmux_key`, and `tmux_palette_key`
  are deleted.
- **Review interface:** the panel is a native view; the editor and the diff
  are sessions shown in terminal views; the split is in-process layout.
- **Per-client state.** Current view, sidebar width, and focus are state of
  each client. Each client remembers its own last view per project. One-shot
  flags such as the zen intro stay global.

**The shared app model.** Navigation, the command registry, view models, and
data refresh are written as a new crate, `shelbi-app`, with no ratatui,
crossterm, or gpui types in it. The TUI is a renderer over it. This is done
here and not later because the desktop app needs the same model, and writing
it inside ratatui first would mean writing it twice. It requires moving the
key-chord type in `shelbi-state` off crossterm. See Phase 4.

**Focus, keys, and mouse.**

- No prefix key and no modes.
- With a terminal view focused, every key goes to the agent except
  **Ctrl+Space, which opens the palette**. From the palette, Escape returns
  to the agent, Tab moves focus to the sidebar, and typing runs any command.
- **The agent keeps its mouse.** When the program has turned on mouse
  reporting (Claude Code's default full-screen mode does), wheel, click, and
  drag are forwarded to it with coordinates translated to the pane. The agent
  provides its own scrolling and selection in that mode.
- **Shift+wheel and Shift+drag are always Shelbi's**: scroll Shelbi's
  scrollback and select text for copy. When the program has not asked for
  the mouse, plain wheel and drag do the same.
- Shelbi's scrollback and search exist for sessions on the normal screen. A
  full-screen program has no scrollback to show; that is the program's
  design, not something Shelbi overrides.
- Copy uses OSC 52 so it works over SSH and through an outer tmux or Screen,
  with a native clipboard fallback locally.

**Quit semantics** replace `quit.rs`, `quit_project.rs`, `quit_shelbi.rs`,
and `teardown.rs`:

- Close the UI: agents keep running. Default for `q`.
- Quit project: end that project's sessions and mark it closed.
- Quit Shelbi: end all sessions and stop the daemon.

**`shelbi reload`** is redefined: restart the daemon, signal attached TUI
clients to re-exec and desktop apps to prompt for relaunch, and hand off the
orchestrator as today by replacing its session. Worker sessions are not
touched.

### `shelbi attach <workspace>`

A full-screen client showing one session with no sidebar or chrome, for using
an agent from any terminal, including a pane of the user's own tmux or Screen.

It renders through `shelbi-term`, the same way the TUI's terminal view does.
An earlier draft made it a raw passthrough of the session's bytes. That
cannot work with several clients: a session sized for a 200-column desktop
window, streamed raw into an 80-column terminal, wraps and positions its
cursor by the receiving terminal's geometry, and no filtering fixes that.
Rendering also removes two other passthrough problems. The user's terminal
never sees the agent's terminal queries, so it cannot answer them into the
agent, and the agent's modes are never pushed onto the user's terminal, so
there is nothing to restore on a crash beyond the client's own screen.

- When attach is the most recently active client the session takes its size
  and fills the terminal. Otherwise it shows a clipped or letterboxed view.
- Detach is Ctrl+] by default, configurable, shown in a one-line hint.

The name replaces the legacy `shelbi attach`, which is removed.

### Running inside tmux or Screen

The TUI is an ordinary full-screen program, so nesting works by construction.
All `$TMUX` branching is deleted. What needs care is capability detection:

- The kitty keyboard protocol is not passed through by Screen or by tmux
  without `extended-keys`, and crossterm does not request the older
  modifyOtherKeys form. Inside those, Shift+Enter may be unavailable to the
  agent. Detect it and say so once, with the one-line tmux setting.
- Truecolor falls back to 256 colors.
- OSC 52 copy depends on the outer multiplexer's clipboard setting.

### Remote machines

- Remote sessions are the same session processes, started over SSH and
  detached as described under Spawn, so they outlive the connection.
- **One relay per machine.** The hub runs `ssh <host> shelbi relay`, a
  short-lived process that bridges a single stdio channel to all session
  sockets on that machine. One channel, not one per session, because sshd
  allows ten sessions per multiplexed connection by default. The relay holds
  nothing; if it or the connection dies, the hub starts another and each
  client reconnects by sequence number.
- The protocol carries keepalives so a dead connection is noticed quickly.
- **Finding the binary:** the hub probes through the remote's interactive
  login shell (`$SHELL -l -i -c 'command -v shelbi'`), since a plain SSH
  command does not load the user's PATH. It uses a compatible `shelbi` found
  there, otherwise `~/.shelbi/bin/shelbi`. The resolved path is recorded per
  machine and shown by `shelbi machine status`.
- **Install:** `shelbi machine setup <name>` detects OS and architecture,
  fetches the matching release, and installs it to `~/.shelbi/bin/shelbi`. A
  package-manager install that is too old is never overwritten; Shelbi
  installs its own copy alongside and says so.
- The reverse forward that carries worker events to `hub.sock` is unchanged.
- The local proxy window (`ssh -t host tmux attach`) is deleted.

## What gets deleted

- The `shelbi-tmux` crate, `TmuxAddr`, and the tmux launch strings in
  `shelbi-agent`.
- `ensure_dashboard`, the stash session, `show_view`'s `swap-pane` logic, the
  sidebar clamp hooks and script, `apply_palette_binding`, and the
  `session-closed` hook.
- The pane plumbing in `review_ui.rs`.
- `shelbi popup` and the standalone overlay processes, `shelbi open
  --as-pane`, and the tmux teardown scripts.
- The legacy agent record and everything built on it: `spawn.rs`,
  `archive.rs`, `tail.rs`, the old `attach.rs`, all of `merge.rs`, and the
  legacy fallbacks in `list.rs`, `diff.rs`, `send.rs`, and
  `shelbi-tui/src/app.rs:1935`.
- `shelbi daemon install | uninstall` and the launchd and systemd unit
  templates.
- `tmux_test_support.rs` and the 47 `tmux_available()` guards.
- The tmux dependency in `.goreleaser.yaml`, the Homebrew formula script, the
  wizard preflight, and `README.md`.

The roughly 200 raw layout calls are not ported behind any abstraction. They
are deleted when the single-process TUI replaces what they did.

## Phases

Each phase lands on `main` and leaves Shelbi working. tmux remains the runtime
until Phase 6.

One item is pulled out ahead of the plan because it is a bug today:

**Before anything else: the board skips gated merges.** `shelbi issue move`
runs the workflow's gated merge before crossing a merge edge
(`shelbi-cli/src/commands/issue.rs:908`). The TUI board's `move_card`
(`shelbi-tui/src/kanban.rs:1485`) only changes status, so a card dragged to
done is marked done with nothing merged. Fix: move the CLI's move logic into
a library function in `shelbi-orchestrator` and have both call it. This is a
separate task, and it is also the first slice of Phase 4a.

### Phase 0: Spikes (small to medium)

Throwaway prototypes to retire the real risks.

1. **Agent fidelity.** Claude Code in its default full-screen mode and in
   inline mode, and Codex, in a PTY rendered by a ratatui widget: keys
   including Shift+Enter, mouse forwarding, paste, wide characters, redraw
   cost under heavy output.
2. **Query responder.** Start each agent with no client attached and confirm
   it comes up, which proves the session process answers startup queries.
3. **Replay.** Serialize full emulator state and rebuild it in a second
   emulator. Cases: reattach at unchanged size to a full-screen agent; attach
   while `vim` is open, then quit `vim` and confirm the shell screen
   underneath is intact. This decides the emulator crate.
4. **Codex orchestrator unit.** The three-process orchestrator, which relies
   on duplicated pane stdin and `$TMUX_PANE`, running in a session.
5. **Process survival.** A session process outliving its launcher on macOS
   and on Linux with logind, and the child group dying when it should.
6. **Nesting.** The prototype inside tmux and inside Screen.

Exit criteria: a written list of what does not work and whether each item is
fixable, and the emulator decision.

### Phase 1: Session process, protocol, attach (large)

- `shelbi-proto`, `shelbi-client`, `shelbi-term`, and the `shelbi __session`
  process.
- Debug surface: `shelbi session ls | new | kill | send | snapshot`.
- The new `shelbi attach <session>`, rendered.
- The frozen core is written down and marked as such in `shelbi-proto`.
- Tests run real PTYs in-process; no external binary. They include two
  clients attached at different widths, and replay in the cases from the
  spike.

Nothing in the product uses this yet.

### Phase 2: A session seam in the orchestrator (large)

A `SessionBackend` trait covering **session operations only**: spawn, kill
(process group), three-state probe with deadline, send text, send enter,
snapshot, history, title, get and set metadata, enumerate, respawn in place,
read a dead session's final screen, resize, and a per-target injection lock.

- Implement over `shelbi-tmux` first with no behavior change, moving
  `workspace.rs`, `submit.rs`, `ready.rs`, `handoff.rs`, `load.rs`,
  `issue.rs`, `send.rs`, `open.rs`, `open/pane.rs`, `wake.rs`, and the
  poller's probes onto it. About 78 uses plus about 40 raw calls.
- Implement over session processes second.
- A hidden setting selects the backend, for development only.

### Phase 3: Poller and supervision move to the daemon (medium move, large to make correct)

Design first, covering every item under "The daemon takes over the poller".
Then:

- On-demand daemon start; retire the units with an upgrade step.
- Per-project poller manager and the open-project record.
- Split layout out of the poller.
- Captured login environment for the daemon.
- Generations, deadlines, and the quit barrier for every daemon job.
- Sessions restart a dead daemon.

Tests that must pass: a launch times out, the task is redispatched, and the
original job wakes up and does nothing; a project is quit while its
supervision is blocked on SSH; the daemon is killed with no client attached
and supervision resumes on its own.

This phase is worth doing even on tmux, because it removes the dependency on
a sidebar process being alive. During this phase the old sidebar poller must
be disabled when the daemon poller is on, so two pollers never run at once.

### Phase 4: The single-process TUI (very large, in parts)

Built behind the same hidden setting.

- **4a. Mutations and the app model.** Finish moving `move_to`, `start`,
  `assign`, `edit`, and `add` out of `shelbi-cli` into a library, and put
  them behind the daemon's control socket with per-issue queuing, expected
  state, and recheck before irreversible steps. The CLI commands become
  clients of it. Tests: approve against reject from two clients at once;
  the same issue dispatched twice at once; a client that disconnects
  mid-merge. Create
  `shelbi-app`: navigation, a real command registry (typed arguments,
  availability, executor) replacing the palette's string-prefix dispatch,
  view models, and background refresh that publishes snapshots. Move the
  chord type off crossterm. Exit criterion: no ratatui or crossterm types in
  `shelbi-app` or `shelbi-state`.
- **4b. Shell and terminal view.** One event loop, sidebar, terminal view
  widget, focus and mouse model, scrollback, selection, search. Redraws
  capped at about 60 per second and wrapped in synchronized-output mode.
- **4c. Native views.** Issues and activity in-process; the new machines
  view.
- **4d. Overlays.** Port the five popup processes.
- **4e. Review interface.** Panel, editor and diff sessions, layout.
- **4f. Project switching and quit actions.**

### Phase 5: Remote hosts (medium to large)

- `shelbi relay` and the remote transport in `shelbi-client`.
- `shelbi machine setup`, the PATH probe, and the version check.
- Remote spawn through session processes; delete the paste-buffer launch.
- Reconnect after an SSH drop, tested by killing the control connection
  mid-task.

### Phase 6: Cutover (large)

One release.

- Flip the default and delete everything under "What gets deleted".
- **Migration is tracked per workspace, and dispatch waits for it.** A
  worktree must be proven idle before the new backend may start an agent in
  it, or two agents edit the same checkout. Closing the local tmux session
  proves nothing about remotes: each remote workspace has its own
  `shelbi-w-<ws>` session on its own machine, and today's teardown reports a
  remote kill as done even when it failed (`teardown.rs:376`).
  - Each workspace carries a migration state: pending or migrated.
  - A local workspace is migrated once its project's `shelbi-<p>` tmux
    session is confirmed gone (or tmux is not installed).
  - A remote workspace is migrated only after the hub has reached the
    machine and confirmed that its `shelbi-w-<ws>` session does not exist,
    killing it first if the user agrees. A failed or unverified kill leaves
    it pending.
  - **An unreachable remote stays pending.** Dispatch to a pending workspace
    is refused with a message saying why. The rest of the project works.
  - A project with a surviving local `shelbi-<p>` session refuses to open
    until it is closed, which also prevents an old sidebar's poller running
    beside the new daemon's.
  - In-flight tasks are redispatched after migration; there is no live
    migration of running agents.
- **State.** Records carrying a `tmux` address load without error (unknown
  fields are already preserved) and are ignored.
- **Agent instructions.** Rewrite the three shipped templates to use
  `shelbi session snapshot` and session commands instead of tmux, replace the
  tmux target in review events with a session name, and add a config-upgrade
  rule that rewrites the same passages in user-edited instructions, since
  `reload` preserves those.
- **Packaging.** Drop the tmux dependency from the deb and the formula.
- **Docs.** `Engineering/architecture.md`, `Product/vision.md`,
  `Product/principles.md`, `Product/positioning.md`, and in the code
  repository `README.md`, `AGENTS.md`, `docs/release/homebrew-tap.md`, and
  about 100 mentions under `site/` (install, comparison pages, the feature
  grid and hero, CLI reference pages, `public/install.sh`).
- **CI.** A smoke job that launches the TUI inside tmux and inside Screen.

## Effect on other plans

- **`customizable-keybindings.md`:** section 5 and Phase 2 (the tmux-level
  palette chord, `chord.to_tmux_key()`) become unnecessary. The chord type
  moving off crossterm affects its Phase 1.
- **`review-workspaces.md`:** section 10 (the tmux pane model for a running
  server) needs rewriting in terms of sessions.
- **`configurable-review-nav-items.md`:** the `popup` display mode and the
  `respawn-pane` path map to overlays and sessions.
- **`worker-orchestrator-communication.md`** and
  **`workflow-transition-hooks.md`:** references to `tmux send-keys` and a
  "new helper in `shelbi-tmux`" map to `paste`.
- **`getting-started-experience-the-60-second-wizard.md`:** the tmux
  preflight row and the "No tmux" stop are removed.
- **`release-distribution-homebrew-apt.md`:** the `Depends:` line loses tmux,
  and the daemon unit installation in `scripts/install.sh` is removed.

## Risks

- **Terminal fidelity is now Shelbi's problem.** Every rendering, query, or
  key-encoding bug that tmux absorbed becomes a Shelbi bug. Phase 0 sizes it.
- **Phase 4 is most of the work.** It merges four event loops, ports about
  5,000 lines of overlay processes, rewrites the review interface, and
  introduces the shared model. It is split into six parts so each lands on
  its own.
- **The emulator dependency.** `alacritty_terminal` is 0.x with breaking
  releases and would need vendoring; a `vt100`-family crate is simpler to
  serialize from but less complete. Each session keeps the emulator it
  started with, so fixes do not reach long-running agents.
- **Remote install is a new failure point** that zero-install never had.
  Unusual architectures, read-only home directories, and locked-down hosts
  need clear errors and a manual path.
- **Losing tmux's escape hatch.** A stuck session used to be inspectable
  with plain `tmux attach`. `shelbi attach` and `shelbi session snapshot`
  must be solid from Phase 1.
- **The daemon becomes a required command server.** Every mutation now
  depends on it, including from the CLI. Its restart path (by clients and by
  session processes) and its job cancellation have to be solid, and Phase 3's
  tests exist for that.
- **Memory.** Full scrollback is tens of megabytes per session at wide
  sizes. Clients hold scrollback only for sessions being viewed.

## Out of scope

- The desktop app ([[desktop-app-gpui]]).
- A Windows port. The new session code is written to be portable, but
  workers reach `hub.sock` from shell hooks using `nc -U`, socat, or python,
  and 33 existing files use Unix-only APIs. Porting those is separate work.
- User-arranged splits and tiling beyond the fixed sidebar, main area, and
  review layout.
- Changing the `hub.sock` event protocol or the plain-file state model.
- Forcing agents out of their default full-screen mode.
