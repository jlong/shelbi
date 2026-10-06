# rt-machines-reachability — In review

Phase 5 follow-up: the native machines view (from `rt-tui-native-views`) showed
each remote machine's recorded `shelbi` path/version from `rt-machine-setup` but
not whether the host was answering *right now*. The old tmux `while true; shelbi
workspace list; sleep 5` loop probed remotes over SSH, so a box going down was
visible there and wasn't in the native view. This restores that live signal
without ever putting SSH on the render path.

## What landed

- **`Reachability`** (`shelbi-orchestrator::machine`) — a three-state
  `Unknown` / `Reachable` / `Unreachable { error }`, plus `probe_reachability`,
  a lightweight "is this host answering" probe: a trivial `true` over the
  existing `RemoteExec` seam (same `shelbi_ssh` ControlMaster + reverse-forward
  transport the poller uses), classified with the module's existing
  unreachable/auth markers. Deliberately much cheaper than `probe_machine` (no
  login shell, no binary resolution). `SshExec::with_deadline` lets the probe
  use a short (10s) fail-fast bound instead of the install-sized default.
- **`ReachabilityProber`** (`shelbi-tui/src/reachability.rs`) — a background
  thread that probes each declared remote on a 30s cadence, backs off
  geometrically on failure up to 5 min, and publishes the latest per-machine
  `Reachability` into a shared map. The probe runs only on this thread; it is
  injected as a closure so both runtimes drive real SSH while tests stub it.
  `set_targets` / `snapshot` touch only in-memory state, so the UI thread never
  waits on SSH.
- **`MachinesApp` wiring** — `MachineEntry` gains a `reachability` field;
  `read_data` stays pure (no SSH), seeding locals `Reachable` and remotes
  `Unknown`. `enable_reachability()` (called by both runtimes right after
  construction) spawns the prober; `apply_data` pushes the current remote
  targets (locals excluded → never probed → no SSH for them) and folds the
  latest snapshot into remote headers.
- **Shared renderer** — `machine_header_line` (the one `render_full` both the
  tmux `__machines` pane and the in-process shell view use) draws a badge next
  to the `ssh <host>` label: green `● reachable`, dark-grey `○ checking…`, or
  red `● unreachable: <error>`. Locals get no badge.

## Tests

- Orchestrator: `probe_reachability` classification (reachable / connection
  refused / auth / timeout) via the `FakeExec` seam.
- Prober (`reachability.rs`): down→recovers with a stubbed probe; `set_targets`
  / `snapshot` don't block on a 150ms probe; only declared targets are probed;
  pure backoff shape.
- `MachinesApp`: remote reachability folds in and recovers (stub); locals are
  never probed (call-count 0); the shared header renders each state next to the
  ssh host label; local header has no badge.

## Config-upgrade note

No shipped `*.template` / default config / workflow / instructions file changed
(this is TUI + orchestrator code, no new config surface), so existing installs
need no config-upgrade sniffer.

## Notes

- Each runtime runs its own prober in its own process; "same reachability via
  the one shared renderer" is structural — both fold a `Reachability` into the
  same `machine_header_line`.
- `Cargo.lock` is unchanged (no new dependencies), so the MSRV job is unaffected.
