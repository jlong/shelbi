# dt-chat-view: the orchestrator chat

## What landed

- **The Chat view** (`src/views/chat.rs`). The Orchestrator row, which the
  window opens on, routes the main area to the project's orchestrator session
  (`<project>/orch`, named through `shelbi_app::session::discovery_name`) in
  the terminal element, filling the main area and focused when shown.
  - **Attach on show, detach on hide.** Showing attaches with a full replay,
    so the scrollback is there at once. Another view, or the settings, drops
    the pane, which closes the connection.
  - **Relaunch-tolerant connect.** The attach runs on the background executor
    through `shelbi_app::session::retry_connect`. A socket not bound yet, or an
    orchestrator caught between its old session exiting and the relaunched one
    binding, is retried for 15s rather than settling on a permanent error.
    That is the TUI's `rt-tui-shows-a-permanent-attach-error-...` fix, now
    shared code rather than a copy. When the window's background
    `orchestrate` finishes, it calls `ChatView::reconnect`, which replaces a
    connect still in flight. The TUI's `reconnect` after startup does the
    same.
  - **Restart states.** An exit, or a connection that drops with no exit,
    goes to the shared `RestartController`. The view shows "Session exited —
    restarting (N/5)…" while it reattaches or waits out the backoff, then
    "Stopped after 5 restarts — select it again to relaunch. Last output: …".
    Selecting Orchestrator again after a give-up (or a failure, or
    mid-restart), or leaving and coming back, re-opens it: the budget resets
    and `PersistentRelauncher` drops the daemon's orchestrator relaunch
    marker, as the TUI does. Quitting the project marks the teardown
    deliberate, so that exit is not restarted. The mark is withdrawn if the
    quit fails.
  - **Focus.** The view takes focus when shown and hands it to the pane when
    the pane attaches. A pane that replaces a focused one after a restart
    takes focus too, unless the user moved focus elsewhere meanwhile.
    Clicking the terminal focuses it and moves the window's focus model to
    the main area (`ChatEvent::Focused`).
  - **Keys.** While the chat's terminal holds focus, the window takes the
    focus moves (Ctrl+H / Ctrl+L by default, from `keys.yaml`), the palette
    chord, and Ctrl+Space in the capture phase, so they never reach the
    session. The TUI intercepts the same set ahead of its sessions. Ctrl+Space
    now opens the palette (still the not-implemented banner until
    `dt-palette-overlays`) from the sidebar too. Everything else, including
    Shift+Enter, is the terminal element's.
  - **Sizing** is the element's: it reports its size at once, then debounced.
    The session follows the most recently active client, and the pane
    letterboxes a session sized by another client.
  - Titles and exits go to the window's `SessionSink`, wired here for the
    first time.
- **The terminal element** gained `TerminalPane::attached_to` (a pane over a
  session the caller already attached), `PaneEvent` (title, exited, closed)
  for the view that supervises it, and `session::wire`, which moves an
  attached connection onto the pane's reader and writer threads (`connect`
  uses it too). `Connected::link` is now a boxed `SessionIo`, so tests can
  pass a fake session.
- **Moved to `shelbi-app` (toolkit-free) and shared with the TUI:**
  - `shelbi_app::supervision`: the crash-loop core (`SupervisionState`, cap,
    backoff, window, recovery), moved from `shelbi-orchestrator`, which
    re-exports it so the poller's and load path's
    `shelbi_orchestrator::supervision` paths are unchanged. It had to move:
    `shelbi-app` can't depend on the orchestrator.
  - `shelbi_app::session`: `SessionRef` and `discovery_name`; `connect`, the
    TUI's `LiveConnector` body, with the idle-workspace resolver injected;
    `retry_connect`, the TUI's connect-worker retry loop; `ConnectFailure`,
    `RetryPolicy`, `IdleInfo` and `Connected` (with a small `GridSize`, so the
    crate needn't depend on the emulator); and the notices
    (`connecting_notice`, `failed_notice`, `restarting_notice`,
    `gave_up_notice`, `exit_line`).
  - `shelbi_app::session::RestartController`: the decision half of the TUI
    `SessionManager`'s `drive_supervision` / `act_on_death`, returning a
    `DeathAction` (restart N, give up, backoff, stand down, leave). The TUI's
    `SessionManager` now delegates to it.
  - `shelbi_app::session::relaunch`: the `Relauncher` seam and the TUI's
    `PersistentRelauncher`, `ReviewRelauncher` and
    `WorkspaceContentRelauncher`, moved from `shelbi-tui/src/shell/relaunch.rs`.
  - The TUI's `shell::session` re-exports these, so its call sites are
    unchanged. The TUI's behavior and wording are unchanged: its 187 shell
    tests pass, and its main / review / workspace placeholders render through
    the shared notice functions.

## Decisions

- **Move the decision core, not the whole `SessionManager`.** The TUI's slot
  holds a ratatui pane, a blocking connection and a worker thread. The
  desktop's holds a gpui entity and executor tasks. What both must agree on is
  toolkit-free: naming, the connect classification, the retry loop, the
  restart decisions and the wording. Those moved; each client keeps a small
  slot machine over them. The task asks for exactly this split: attach-state
  tests in a gpui context, restart-decision tests in `shelbi-app`.
- **A dropped connection counts as a death.** The TUI only reacts to an
  `Exited` event. The desktop also treats the stream ending with no exit (the
  session process killed outright, or a session that exits while we attach
  to it) as a death and restarts. Otherwise the pane would sit live but dead.
- **Alive ticks.** The TUI feeds the controller an alive observation every
  event-loop turn. The desktop is event-driven, so it feeds one when the pane
  attaches and one at the moment the pane dies, just before the death. That
  is the same observation the TUI's last tick makes, so the recovery window
  (60s up resets the history) behaves the same. A 250ms timer runs only while
  waiting out a backoff.
- **Hide drops the connection** (the task's "detach on hide"), and the next
  show is a fresh attach with a fresh budget. A view hidden while dead
  re-opens on the next show, like the TUI re-selecting a dead pane.
- **The chord interception is limited to a focused chat terminal**, so the
  Issues board's text inputs keep their own Ctrl chords.
- **No root `Cargo.lock` change.** The shared tests use `std::env::temp_dir`
  rather than adding `tempfile` to `shelbi-app`.

## Verification

From `desktop/`:

- `cargo build`: ok
- `cargo clippy --all-targets -- -D warnings`: ok
- `cargo clippy --all-targets --features visual-check -- -D warnings`: ok
- `cargo test`: 132 passed. The 13 chat tests run in a gpui test context with
  a fake session: attach with replay and detach on hide; typing reaches the
  session (Shift+Enter is `CSI 13;2u` once the program enables the kitty
  protocol); an exit reattaches; a dropped connection waits out the backoff
  and reattaches; a relaunch gap with failed attempts reattaches; a crash loop
  gives up and re-selecting relaunches (a reopen); a dead view hidden and
  shown re-opens; a deliberate close isn't restarted; `reconnect` replaces an
  in-flight connect and leaves live and hidden views alone; a stale result for
  a hidden view is dropped; a replacement pane keeps keyboard focus; the pane
  reports its size. Two window tests cover the Orchestrator route and which
  chords the window takes from a focused terminal.
- `cargo run --features visual-check -- --visual-check <dir>`: 0 failed.

From the repository root (`shelbi-app`, `shelbi-orchestrator` and `shelbi-tui`
changed):

- `cargo build --workspace`: ok
- `cargo clippy --workspace --all-targets -- -D warnings`: ok
- `cargo check --workspace --locked`: ok; the root `Cargo.lock` is unchanged
- `cargo test -p shelbi-app`: ok (the moved supervision tests, plus the
  session and restart-controller tests)
- `cargo test -p shelbi-tui --lib shell::`: 187 passed
- `cargo test --workspace --no-fail-fast`: 3896 passed, 5 failed (load-bound
  session e2e timeouts, see "Full suite" below)

### By hand (sandboxed hub)

The real app ran against an isolated hub: `SHELBI_HOME=/tmp/shg`, the hub and
control socket variables unset, its own daemon, and a `/bin/sh -c '…; exec
cat'` orchestrator. The real hub was never touched.

- **Opening the project** shows the orchestrator with its output replayed,
  the cursor focused.
- **Found and fixed:** on a loaded machine the first attach started before
  the session existed and gave up ("not reachable after 15s") while the
  background `orchestrate` was still bringing it up. The window had only
  retried a settled failure. It now `reconnect`s as the TUI does, and that is
  tested.
- **Crash** (killing the sandbox orchestrator's child): "Session exited —
  restarting (N/5)…", then the give-up notice after five. **The sandbox daemon
  logged `supervision=restart` but never spawned a new session.** That is the
  daemon bug fixed on `main` by #1690 ("relaunch a dead orchestrator for
  real"), which isn't on `jlong/desktop-app` yet. Standing in for the daemon
  (spawning a fresh `alpha/orch` by hand), a newly opened app attached to it.
- **Found and fixed:** the give-up notice ran past the edge (now wraps), and
  a reattached pane lost keyboard focus (now keeps it, and that is tested).
- **`shelbi reload`** in the sandbox failed at its own daemon-restart step
  ("hub daemon did not start within 10s", load average around 80) before
  reaching the orchestrator swap. The app showed the existing "relaunch
  required" bar. Reproducing the swap by hand (kill `alpha/orch`, spawn a new
  one) made the view show the restart and reattach to the new session on its
  own.
- **Two clients:** the TUI ran in a separate sandboxed tmux server on the same
  session. Typing in the TUI resized the session to the TUI's 62x26, and the
  desktop showed that grid letterboxed and correct. Typing in the desktop to
  take the size back wasn't driven by hand: no synthetic input while the
  machine is in use. The input path is covered by the chat tests, and the
  size switch is the session's `on_input` rule, the same for every client.

## What's left

- **Full root suite under load.** Five session-process e2e tests timed out on
  this machine (see below). CI is the authority for them.
- **A reload costs two restarts.** When the old session exits, the first
  reattach can land on the still-exiting session (it holds its lock for a
  moment). That attach then drops and counts as a second death, with a 5s
  backoff. The TUI would sit on such a pane instead. A connect that skips
  sessions that have already pushed `exited` would fix both; it belongs in
  `shelbi_client::choose_session`.
- **The relaunch bar overlaps the traffic lights** when shown. This is
  `dt-platform-bootstrap`'s bar, not this task's.
- Workspace sessions (`View::Session(<workspace>)`) still show the
  not-implemented stub. `ChatView` keeps its target as a `SessionRef`, so
  `dt-workspace-view` can reuse it.

### Full suite

`cargo test --workspace --no-fail-fast` (load average 55 to 80 throughout):
**3896 passed, 5 failed**. All five are `shelbi-cli` session-process e2e tests
that wait a fixed 10s for a freshly spawned `shelbi __session` to bind its
socket: `old_session_compat`, `remote_session_e2e`, `session_backend_e2e` (2)
and `session_survival`. They fail the same way when rerun alone ("session
socket never appeared"). By hand, a plain `shelbi session new` in a fresh
`SHELBI_HOME` took more than 25s to bind on this machine at the time; the
sandbox daemon's log also shows its login-shell env capture timing out at 10s.
This branch changes nothing in `shelbi-session`, `shelbi-client` or
`shelbi-cli` (`git diff --stat origin/jlong/desktop-app -- crates/shelbi-session
crates/shelbi-client crates/shelbi-cli` is empty). Rely on CI for these.
