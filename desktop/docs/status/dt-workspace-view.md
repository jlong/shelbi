# dt-workspace-view: the Workspace view

## What landed

- **The Workspace view** (`src/views/workspace.rs`). Selecting a workspace
  row, or `workspace:<name>` from the registry, routes the main area to the
  workspace's agent session (`<project>/ws/<name>`) in the terminal element,
  under a header:
  - **State, name, task.** The live state is the sidebar's own badge (glyph,
    color and words: Working, Awaiting input, Needs permission, Paused, Idle),
    so the two never disagree; then the name (bold), then the task's id and
    title.
  - **Location line.** `machine · branch · Agent`, with a globe icon and
    `(remote)` for a machine reached over SSH.
  - **The TUI workspace panel's affordances**, while the workspace has a task:
    More (the full task description, as markdown in a popover; Esc or a click
    outside closes it), Reveal (opens the worktree in Finder), and the view
    switch: the agent / View Diff / Edit in `<editor>`, plus Close (the TUI's
    `q`), which ends the diff and editor sessions.
  - **Shell**: the user-shell toggle, always offered.
  - **The status line** under the header carries failures ("couldn't open
    view: …", "couldn't open a shell: …"), as the TUI panel's does.
- **`commands.rs`**: `FocusWorkspace` now shows `View::Session(<workspace>)`
  (so a workspace picked in the Activity feed or the Machines view opens here
  too),
  and `view_stub_id(View::Session(_))` is `None`, so workspaces no longer show
  the "Not implemented" stub. `FocusSession` (legacy spawned agents) keeps the
  banner; the TUI doesn't show those either.
- **Idle workspaces** show the TUI's idle placeholder (its wording, from
  `shelbi_app::session::idle_placeholder_lines`, naming the user's palette
  chord) and re-attach as soon as the workspace's sidebar row changes.
- **The user shell.** The toggle asks the daemon for the workspace's shell
  (new `WorkspaceSessionOp::EnsureShell`), a login shell in the worktree named
  `<project>/ws/<name>/shell`, and attaches it in a second terminal beside the
  agent. A draggable divider splits them (`src/views/workspace_layout.rs`,
  both panes at least 240px, the agent's share kept across window resizes).
  Toggling it off, leaving the view, switching workspace, or exiting the shell
  closes it; the first three send `CloseShell` (SIGHUP, as a closing terminal
  does). The shell never occupies the workspace's slot, so dispatch ignores it.
- **Remote workspaces.** All of a workspace's sessions attach through
  `shelbi_orchestrator::session_connect::connect`. It takes the local path for
  a local workspace and the machine's relay for one on an SSH machine (new
  `remote_session::attach`, which streams through the cached per-machine relay
  and replaces a dead one). When the SSH connection drops, the pane's stream
  ends with no exit. A remote pane then **reconnects** ("Reconnecting to
  gpu-box… (attempt N) `gpu-box` is unreachable"): immediately, then backing
  off 0.5s doubling to 8s, without giving up and without spending a restart.
  Only an attempt that reaches the machine and finds the session gone counts as
  a death. The user shell runs on the remote too (spawned over SSH, reached
  through the same relay).
- **Restart states** are the Chat view's. Its slot machine moved out of
  `chat.rs` into `src/views/session_view.rs` (`SessionView`), now
  target-agnostic. `chat.rs` is a thin alias (`ChatView =
  SessionView::orchestrator`). The Workspace view holds two: the agent (or its
  diff or editor), restarted on the shared `RestartController`, and the shell,
  set to `OnDeath::Close`.
- **Drop to paste.** Files dropped on any terminal pane (chat, agent, shell)
  paste their paths, backslash-escaped and space-joined as Terminal.app and
  Ghostty paste a drop (`shelbi_term::input::dropped_paths_text`). The paste is
  bracketed when the program asked for it, and the pane takes focus.

### Moved to or added in the shared crates

- `shelbi-proto`: `WorkspaceSessionOp::{EnsureShell, CloseShell}`;
  `CONTROL_PROTOCOL_VERSION` 5 → 6 (`dt-board-dnd` took 5 for `Prioritize`).
- `shelbi-orchestrator`:
  - `workspace_session::{workspace_details, ensure_user_shell,
    close_user_shell, shell_session_name}`. `workspace_open_info` (the TUI's)
    is now `workspace_details` plus its local-only check, with unchanged
    behavior.
  - `remote_session::{attach, signal}`.
  - `session_backend`'s `signal_by_name`.
  - New `session_connect` (local or relay attach, with the failure
    classification).
- `shelbi-app`:
  - `session::reconnect` (`ReconnectController`, `ReconnectPolicy`), plus
    `ConnectFailure::Unreachable`, `SessionRef::{workspace_shell, workspace}`,
    `reconnecting_notice`, and `idle_placeholder_lines`, moved out of the
    TUI, which now delegates to it.
  - `view::{WorkspaceHeader, WorkspaceDetailsInfo}` and
    `WorkspaceBadge::label`.
  - `WorkspaceContentRelauncher` re-ensures the shell role too.
- `shelbi-term`: `input::dropped_paths_text`.
- `shelbi-cli` daemon: handles the two new ops.
- `shelbi-tui`: delegates its idle-placeholder wording to the shared function,
  and treats `Unreachable` as a failure (its connector is local-only, so it
  never sees one). No TUI behavior changed.

## Decisions

The orchestrator's ruling (q-001): where the spec assumes behavior the TUI
doesn't have, match the TUI; add something new only where the spec asks for it
(the shell toggle, the divider, drop-to-paste). Each default, with the code
that justifies it:

1. **Selecting an idle workspace launches nothing.** The spec says it
   "launches its pane lazily … through `WorkspaceSessionOp`". The TUI doesn't:
   `rt-tui-idle-workspace-open` shows an idle placeholder and retries the
   attach when the shown workspace changes. See `crates/shelbi-tui/src/shell/mod.rs`:
   `show` → `RowTarget::Session`, `retry_shown_workspace`, and
   `render_idle_workspace`. `WorkspaceSessionOp` only had `Ensure{editor|diff}`
   and `Close`, nothing that starts an agent. The desktop matches the TUI:
   placeholder, then re-attach when the sidebar row changes (`update_row` →
   `SessionView::retry_if_idle`; the TUI retries on a change event for that
   workspace). Agents are the daemon's to dispatch.
2. **The user shell is new, and so is its daemon op.** The spec asks for a
   shell beside the agent "as the TUI's `@shelbi-user-shell` replacement
   does". There is no such replacement: `@shelbi-user-shell`
   (`crates/shelbi-orchestrator/src/workspace.rs`, `open_user_shell`, used by
   `shelbi open` on an idle workspace) puts a shell *in* the idle slot and
   blocks dispatch while it is open. So `EnsureShell`/`CloseShell` are new: a
   separate `<project>/ws/<name>/shell` session beside the agent that never
   touches the slot, works on a busy workspace, and is the workspace content
   role `shell` (it relaunches through `WorkspaceContentRelauncher`, like the
   editor and diff).
3. **"Closes with the view."** Hiding the view, switching workspace, or
   toggling the shell off ends the shell session (`CloseShell`), not just
   detaches it. Exiting the shell closes the split and is not restarted.
   SIGHUP rather than the session's default SIGTERM: an interactive shell
   ignores SIGTERM, which the daemon end-to-end test caught.
4. **The panel's affordances live in a header, not a sidebar.** The TUI swaps
   the nav sidebar for its workspace panel (`crates/shelbi-tui/src/workspace_panel.rs`).
   The spec asks for a header, so the same affordances (More, the worktree
   folder, Agent / View Diff / Edit in `<editor>`, and `q` Close as a Close
   button) sit in the header, and the window keeps its sidebar. As in the TUI,
   the panel's affordances appear only while the workspace has a task
   (`ShellState::show` opens the panel only when `workspace_has_task`).
5. **Diff and editor are local workspaces only**, as in the TUI
   (`workspace_session::ensure_content_session` refuses a remote workspace,
   and the TUI's `workspace_open_info` errors). The switch and Reveal are
   hidden for a remote workspace; More and Shell are not.
6. **Back leaves the diff/editor loaded.** Leaving the view or switching back
   to the agent detaches them. Only Close ends them, as only `q` does in the
   TUI (`crates/shelbi-tui/src/shell/workspace.rs`, `WorkspaceAction::Back` vs
   `Close`).
7. **The header's state is the sidebar badge**, not the TUI panel's column
   status (`IN PROGRESS`, from `ws_status_from_model`). The spec asks for a
   "live state badge", and the badge is the live one (working, awaiting input,
   permission, paused). The TUI derives both from the same row, and the panel
   only opens for an in-progress task anyway.
8. **A remote pane's lost connection is a reconnect, not a restart.** This is
   new; the spec asks for it, and the TUI can't attach a remote session at all
   (`shelbi_app::session::connect` discovers local sessions only). An attempt
   that reaches the machine and finds the session gone is a death and goes to
   the restart controller; an unreachable machine is retried with no limit,
   because a machine coming back is the normal end of an outage. A local pane's
   dropped connection stays a death (the Chat view's rule).
9. **Header title freshness.** Details (title, branch, body) are read off the
   UI thread once per task id. A title read for a previous task is never shown
   under a new id (`WorkspaceHeader::build`), and a row whose task changed
   triggers a re-read.
10. **Drop format.** Backslash escaping, matching what a terminal-hosted TUI
    receives from Terminal.app or Ghostty
    (`crates/shelbi-tui/src/shell/mod.rs`, `deliver_paste`), with no trailing
    space.
11. **One `SessionView` for chat and workspace.** `chat.rs`'s doc (from
    `dt-chat-view`) anticipated it. The chat's 13 tests run unchanged against
    `SessionView::orchestrator`.

## Verification

Commands and results (load average 55–75 throughout):

From `desktop/`:

- `cargo build`: ok
- `cargo clippy --all-targets -- -D warnings`: ok
- `cargo clippy --all-targets --features visual-check -- -D warnings`: ok
- `cargo test`: **206 passed** after rebasing onto `dt-board-dnd`,
  `dt-activity-machines`, `dt-packaging` and `dt-palette-overlays` (158
  before the rebases). Registry `workspace:<name>` palette entries open this
  view. New
  tests:
  - Session view: remote reconnect with backoff, unreachable at open, a
    reconnect that finds the session gone, hide while reconnecting, local
    drop still a death, close-on-death, `show_target`, idle placeholder and
    retry, and a file drop pasting into the session.
  - Workspace view (10): header from `WorkspaceRow` fixtures, the header
    following the row, idle (nothing launched, re-attach on change), the shell
    split (open, divider drag, close with the view), shell exit, the daemon
    refusing a shell, the diff switch and Close, a refused view falling back,
    switching workspace, and a remote workspace recovering from an SSH drop.
  - Split math (6), and the window routing a workspace row.
- `cargo run --features visual-check -- --visual-workspace <dir>`: 5 checks
  ok. Frames show the working workspace, the shell split, the description, the
  idle placeholder, and a remote workspace reconnecting.
- `cargo run --features visual-check -- --visual-check <dir>`: not re-run
  (the terminal element changed only by adding its drop listener).

From the repository root:

- `cargo build --workspace`: ok
- `cargo clippy --workspace --all-targets -- -D warnings`: ok
- `cargo check --workspace --locked`: ok. The root `Cargo.lock` is unchanged.
- `cargo +1.88 check --workspace --all-targets --locked`: ok.
- Targeted, all passing:
  - `cargo test -p shelbi-app -p shelbi-term -p shelbi-proto`: reconnect
    controller (4), header model (5), drop text.
  - `cargo test -p shelbi-orchestrator --lib` for `workspace_session`,
    `session_connect`, and `remote_session`.
  - `cargo test -p shelbi-tui --lib -- idle workspace`: 82.
  - `cargo test -p shelbi --test control_socket`: 7, including the new
    **daemon user-shell e2e**. A real sandboxed daemon starts
    `p/ws/dev/shell`, an ensure is idempotent, `pwd` in it is the worktree,
    and `CloseShell` ends it.
  - `cargo test -p shelbi --test remote_session_e2e`: 2, including the new
    **relay SSH-drop e2e**. `session_connect::connect` attaches a "remote"
    agent through the real `serve_relay` (only SSH is stubbed) and streams its
    screen and input. Shutting the relay's socket (the SSH connection dying)
    ends the client's stream. An unreachable machine reads as `Unreachable`.
    Reattaching replays the exchange from before the drop, and input works.
- `cargo test --workspace --no-fail-fast` (on the rebased tree, load average
  50–70): **3939 passed, 1 failed**. The failure was
  `shelbi-tui shell::pty_input_tests::a_dropped_file_pastes_into_the_open_workspace_content_session`:
  `observer connects: ConnectionRefused`. The test helper connects the moment
  the session's socket *file* exists, before the session listens: a load race
  in the test, not this change (the test builds its session directly and goes
  through none of the code changed here). It passed 3 of 3 when rerun alone.
  An earlier full run, from before the SIGHUP fix, also failed this branch's
  own shell e2e at its old assertion; that test passes now.

## What's left

- **The real app was not driven by hand.** I posted no synthetic input
  (other workers' apps and the user share this machine), so selecting a
  workspace in a live window, dragging the divider with a real mouse, and
  dropping a Finder file were exercised in gpui test windows (real dispatch,
  real element tree) and the offscreen harness, not on screen. A real-SSH
  ControlMaster kill was not run (no remote machine here). The relay e2e kills
  the relay's transport, which is what an SSH drop does to it.
- **The TUI still can't attach remote sessions.** Wiring its connector to
  `session_connect::connect` would let it, but it would also need the
  reconnect controller (its `SessionManager` only reacts to an exit).
  Follow-up.
- **Text drags from other apps** don't paste: gpui-pre 0.3.9 only delivers
  file drops (`docs/gpui.md`).
- **The split ratio isn't persisted** across launches (it lives in the view).
  `WindowState` could carry it.
- **Description headings** render in gpui-kit's markdown style (dark
  headings), the same as the Issues task detail.
