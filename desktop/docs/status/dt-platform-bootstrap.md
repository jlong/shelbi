# dt-platform-bootstrap: finding Shelbi from a bare launch

## What landed

The app now reaches Shelbi before it shows the shell. On launch the window
shows the wordmark and the current step, then either the shell (sidebar | main
split, unchanged) or a full-window screen naming the fix.

`src/platform/`:

- `env.rs`: the login-shell environment for the app's children, captured by
  the shared `shelbi_core::login_env::login_shell_env` (the same
  `$SHELL -l -i -c env` capture, deadline and fallback the daemon and the
  session process use; nothing duplicated). The app's own `SHELBI_HOME`,
  `SHELBI_HUB_SOCK`, `SHELBI_CONTROL_SOCK` replace the login shell's, so a
  daemon the app starts listens where the app looks.
- `discovery.rs`: finds `shelbi`. Order: `desktop.cli_path` in
  `~/.shelbi/config.yaml` (a leading `~/` expands; when set it is the only
  candidate), then the login `PATH`, then `/opt/homebrew/bin`,
  `/usr/local/bin`, `~/.cargo/bin`, deduplicated. The result records its
  source (setting, login PATH, standard location). `shelbi --version` gives
  the installed CLI's version (5s deadline).
- `daemon.rs`: starts the daemon by running `<cli> daemon` through the shared
  on-demand start, with a 20s deadline. The daemon's stderr goes to
  `<SHELBI_HOME>/logs/desktop-daemon-start.log`; a failure carries its last 20
  lines.
- `version.rs`: the exact-match policy plus which way a mismatch goes (below).
- `control.rs`: `ControlHandle`, the `Ready` payload's control connection.
  `connect()` opens a command connection, and refuses once a relaunch is
  required. `watch()` subscribes as an attached `desktop` client (so
  `shelbi detach --list` shows the app). A `Reexec` push, or a reconnect hello
  with another version after the daemon restarted, makes a relaunch required.
  Hellos have a 5s deadline: `ControlClient::connect` has none, and a wedged
  daemon must not wedge bootstrap.
- `bootstrap.rs`: `Bootstrap::spawn` runs
  `ResolvingEnv → FindingCli → StartingDaemon → Connecting → Ready | Failed(kind)`
  on its own thread over the `Platform` seam (`SystemPlatform` in the app) and
  publishes `BootstrapEvent`s on an `async-channel`. After `Ready` it publishes
  at most one `RelaunchRequired`.
- `relaunch.rs`: `open -n` on the enclosing `.app`, or re-running the bare
  binary; the window then quits.

`src/views/bootstrap.rs`: progress, the failure screens, and the relaunch bar.
Each failure's text is a pure `Screen` (title, what happened, fix steps with
copyable commands, a monospace detail block, one button), unit-tested
without gpui:

| Failure | Screen | Fix shown | Button |
|---|---|---|---|
| CLI not found | "Shelbi can't find the shelbi command" + **Searched:** every path | `brew install jlong/shelbi/shelbi`, or the `desktop.cli_path` snippet; a bad setting is named instead | Try Again |
| Daemon failed to start | reason + `<cli> daemon` it ran + the stderr tail and log path | run `<cli> daemon` in a terminal; `shelbi daemon status` | Try Again |
| Control socket unreachable | the socket path and error | `shelbi daemon restart` | Try Again |
| App newer than CLI | both versions | `brew upgrade shelbi` | Try Again |
| Daemon stale (CLI on disk = app, daemon older) | both versions | `shelbi daemon restart` | Try Again |
| App outdated (daemon newer at first hello) | both versions | relaunch; else `brew upgrade --cask shelbi-desktop` | Relaunch Shelbi |

Relaunch bar: once `Ready`, a CLI change shows a yellow bar over the shell
("Shelbi was updated to X. Relaunch to keep working: changes are paused until
then; open sessions keep streaming.") with a Relaunch button. While it shows,
`ShelbiWindow::run` refuses every command `commands::sends_to_daemon` marks
(no wildcard arm; issue mutations, review loads, project switch and quit, Zen,
focusing a workspace that may launch an agent) with a banner. Views,
`FocusSession`, the error log, editors, and Detach still run.
`ControlHandle::connect` refuses as well, so a future command path is blocked
even if it skips that check.

Shared crates (both additive):

- `shelbi-state::daemon_lifecycle`: `DaemonLauncher`, `set_daemon_launcher`,
  `ensure_daemon_running_with` returning a typed `DaemonStartError`
  (`Spawn` / `Exited` / `Timeout`). `ensure_daemon_running` behaves as
  before (it now also reaps the daemon child) unless a launcher is installed.
  A spawned daemon that dies while no daemon holds the lock now fails fast
  instead of waiting out the deadline.
- `shelbi-state::user_config`: `UserConfig.desktop: DesktopConfig { cli_path }`,
  omitted from the file when unset.

## Decisions

- **The launcher is installed process-wide.** `shelbi_state::ensure_daemon_running`
  re-runs `current_exe()`, which in this process is the app. Shared code the
  shell will call (`shelbi_app::execute_mutation`, `review_session`,
  `workspace_session`) starts the daemon that way, so without the override
  it would launch `shelbi-desktop daemon`, a second app window. Bootstrap
  installs `<cli> daemon` with the login env before its own start, so every
  later on-demand start in the process uses the CLI.
- **A set `desktop.cli_path` is the only candidate.** A typo is reported on
  the CLI-not-found screen and not silently replaced by some other `shelbi`
  on the PATH. An unreadable `config.yaml` is reported there too, and
  discovery continues without it.
- **The version policy uses three versions, not two.** The task names two
  mismatch cases; a third is common in practice. When `brew upgrade` swaps the
  CLI but the long-lived daemon keeps running the old binary, the app and CLI
  agree and only the daemon is stale. "brew upgrade shelbi" would be wrong
  advice there, so that case gets `shelbi daemon restart`. A daemon *newer*
  than the app at the first hello is the "CLI upgraded, this process is the
  old app" case at launch, so it gets the relaunch screen. An unparseable
  version falls back to "upgrade the CLI".
- **Any `Reexec` means relaunch.** This includes `shelbi reload` with matching
  versions. The daemon's protocol defines `Reexec` as "re-exec (TUI) /
  prompt for relaunch (desktop)", so the app follows it. The bar names a
  version only when it differs.
- **`Detach` pushes are logged, not acted on.** Detaching the window is
  `dt-shell-sidebar`'s.
- **The integration test lives in `src/platform/integration_tests.rs`**, not
  `tests/`: `shelbi-desktop` is a binary crate, and `tests/` can't import from
  a binary. The real control server lives in the `shelbi` binary crate (its
  `serve_bg` isn't importable), so the test runs a stand-in daemon speaking
  `shelbi_proto::control` (hello, subscribe, `Reexec` pushes, restart onto a
  new version) and the hub hello. Only the login-shell capture is replaced;
  discovery, the daemon start through the fake `shelbi` script, the hello, the
  version check, and the watch are the real code.
- **No `config.yaml` scaffold change.** Documenting `desktop.cli_path` in the
  shipped scaffold would be a template edit needing a config-upgrade sniffer
  for existing installs; the key is optional and documented here and on the
  CLI-not-found screen instead.
- `async-channel` (already in gpui's tree) carries events from the bootstrap
  thread to the view's gpui task; no new crates entered `desktop/Cargo.lock`.

## What's left

- **`SHELBI_HOME` set only in the login shell** (for example in `.zshrc`) is
  not adopted by the app's own process. Children get the app's view (see
  `env.rs`), so the app is consistent but looks in `~/.shelbi`. Adopting it
  means `setenv` from a background thread while gpui runs, which isn't safe.
  Revisit if it comes up, for example by capturing before the window opens.
- **Fonts.** The screens use the window's default font; Geist and Geist Mono
  for the command boxes arrive with `dt-theme-tokens`.
- **The shell doesn't use `Ready` yet** beyond a one-line connection summary
  in the placeholder main area. `Ready.env` and `ControlHandle::connect` are
  marked `allow(dead_code)` until `dt-shell-sidebar` picks them up.
- **Visual check by a human.** This session had no Screen Recording
  permission, so I could not screenshot the window (captures came back black).
  The states and transitions were verified through the app's log (below), but
  the layout of the failure screens and the relaunch bar has not been looked
  at.
- **Manual upgrade test not run end to end** (`brew upgrade shelbi` with the
  app and an old worker session running). It needs two installable versions.
  Its parts are covered: the real daemon's `Reexec` path (below), the
  restart-onto-a-new-version path (integration test), and the command gate
  (unit + integration). The session view half can't be exercised until
  `dt-chat-view` / `dt-workspace-view` attach sessions. To run it:
  1. Install version N, start a project, open the app, open a worker session
     view.
  2. Install N+1 (`brew upgrade shelbi`), then `shelbi daemon restart` (or
     `shelbi reload`).
  3. Expect the relaunch bar, a refused mutation (banner), and the session
     view still streaming.

## Verification

From `desktop/` (Rust 1.99.0):

- `cargo build`: ok, no warnings.
- `cargo clippy --all-targets -- -D warnings`: ok.
- `cargo test`: 45 passed. New: discovery order, setting override, PATH
  dedup, relative entries, exec bit, `--version` parsing; version policy and
  semver precedence; env composition; stderr tail; bundle detection; the
  state machine (every path, scripted `Platform`); screens; the command gate.
  Integration, against real processes and sockets in a `/tmp` `SHELBI_HOME`:
  reaches `Ready` after starting a stopped daemon through a fake `shelbi` on a
  fake `PATH`; the `desktop.cli_path` override wins; a daemon dying at startup
  fails with its stderr tail; a `Reexec` push requires a relaunch and
  `connect()` refuses; a daemon restarted on another version requires a
  relaunch.

From the repo root (`shelbi-state` changed):

- `cargo check --workspace --locked`: ok; root `Cargo.lock` unchanged.
- `cargo build --workspace`: ok.
- `cargo clippy --workspace --all-targets -- -D warnings`: ok.
- `cargo test --workspace`: 3814 passed, 0 failed (36 test binaries). New in
  `shelbi-state`: a daemon dying at startup fails fast with its stderr kept;
  the installed launcher runs `<exe> daemon`; `desktop.cli_path` parses,
  round-trips, and is omitted when unset.
- `cargo +1.88 check --workspace --all-targets --locked`: ok (the CLI's MSRV).

By hand, the debug binary wrapped in a throwaway `.app` and launched with
`open` under a scrubbed launchd-like environment
(`PATH=/usr/bin:/bin:/usr/sbin:/sbin`) and a sandbox `SHELBI_HOME`:

- With no daemon running and `shelbi` only on the login shell's PATH
  (`~/bin`, not in any standard location), the app found it ("your login
  shell's PATH"), ran `~/bin/shelbi daemon`, and logged `bootstrap ready`
  with daemon 0.10.0 about 2s after launch.
- After the sandbox daemon idle-exited and was started again, the app's watch
  reconnected and registered (`ListClients` showed a `desktop` client with
  the app's pid). Sending the real daemon `ReloadClients` (what
  `shelbi reload` sends) logged "relaunch required, reason=shelbi reload".
- `desktop.cli_path: ~/nope/shelbi` in the sandbox `config.yaml` failed with
  `CliNotFound { searched: ["/Users/jlong/nope/shelbi"], setting: Some(..) }`.
- Note for the next person testing this way: `open` passes the calling
  shell's environment to the app. Launched from a Shelbi pane that includes
  `SHELBI_HUB_SOCK`, which points the app at the live hub, so scrub the
  environment (`env -i ... open`) as above.
