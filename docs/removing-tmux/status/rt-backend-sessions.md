# rt-backend-sessions — In review

Phase 2, second half: a [`SessionBackend`] implementation over detached
`shelbi __session` processes, selected by a hidden dev flag. The tmux backend
stays the default and is unchanged. Plan section: Phase 2 ("Implement over
session processes second").

## What landed

- **`SessionProcessBackend`** (`shelbi-orchestrator`,
  `session_process_backend.rs`) — the full `SessionBackend` trait over the
  shared `shelbi-client` / `shelbi-session` stack:
  - `probe` → scan `sessions_dir()` + the lifetime-lock liveness (`is_held`);
    a local scan always answers, so it is never `Unreachable`.
  - `snapshot` / `history` / `final_screen` → `shelbi_client::snapshot` (live
    over the socket, or a dead session's `final.txt`), already in
    `capture-pane -p -J` shape, so the ready.rs / submit.rs detectors read it
    unchanged.
  - `send_text` → `Connection::paste` (no Enter), `send_enter` →
    `Connection::input(b"\r")`, `send_line` → both; the submit layer's
    injection lock serializes the sequence.
  - `title` → `Connection::info().title` (the OSC 2 `shelbi:<state>` worker
    marker rides the title event through unchanged).
  - `kill` → `Connection::kill(None)` (signals the child's process group).
  - `injection_lock` → the **same** process-global registry the tmux backend
    uses (shared `session_backend::injection_guard`, keyed on
    `SessionTarget::label()`), so a paste never interleaves regardless of which
    backend is active.
- **Addressing.** `session_process_backend::session_name` derives the plan's
  readable name from the (still tmux-shaped) target, as a pure function so spawn
  and every lookup agree: a local slot `shelbi-<proj>` / `<workspace>` →
  `<proj>/ws/<workspace>`; a whole session → `<proj>/orch`; a pane → `pane/<id>`.
  Native plan-shaped targets land when the callers are rewritten at cutover.
- **Backend selection.** `session_backend::backend()` now returns a `Backend`
  enum (`Tmux` | `Session`) that implements `SessionBackend` by delegation and
  carries the four tmux-topology-only inherent methods (`kill_window`,
  `kill_pane`, `live_pane_ids`, `spawn_local_pane`). Off (the default) it is
  byte-identical to `TmuxBackend`; on, every call site transparently drives
  session processes. The local dispatch `spawn_local_pane` maps
  `LocalPaneTmuxArgs` to a `SpawnSpec` (`to_session_spawn_spec`), carrying the
  per-dispatch env (`TASK_ID` / `PROJECT` / `SHELBI_HUB_SOCK` / `SHELBI_AGENT` /
  `PORT` / review pgid) as an `exec`-prefix in a login shell — the same POSIX
  idiom the remote path uses.
- **The hidden flag.** `DevConfig.session_backend` in `~/.shelbi/shelbi.yaml`
  plus `shelbi_state::session_backend_enabled()`, mirroring
  `daemon_mutations`: `$SHELBI_SESSION_BACKEND` (`1`/`true`/`0`/`false`) wins,
  else the config flag, else off. Dev-only; not surfaced in the wizard.

## Tests

- Unit: name derivation (slot/session/pane), remote-host rejection (never
  silently "dead"), metadata/env defaulting to absent, respawn → `Failed`
  (`session_process_backend`); `session_backend_enabled` default + env override
  (`shelbi-state`).
- **End-to-end** (`shelbi-cli` `tests/session_backend_e2e.rs`): with the flag
  on, a real `shelbi __session` runs a stub agent; the backend probes it Alive,
  `ready::is_input_ready` fires on its snapshot, the `shelbi:idle` title marker
  parses, `submit::deliver_text` delivers a message through the seam, and the
  agent writes its review-ready marker and flips to `shelbi:review` — the full
  dispatch-to-handoff cycle on the session backend. (Spawned via
  `spawn_with_exe` so the real binary runs; production `spawn_local_pane` uses
  `current_exe`, covered by the `to_session_spawn_spec` unit test.)

## AC4 — the Codex orchestrator runs in a session (rework 2026-10-04)

The orchestrator launch path now has a session-backend branch behind the same
hidden flag, so AC4 is met rather than deferred.

- **`orchestrator_session_spec`** (`lib.rs`, the session analogue of
  `orchestrator_pane_cmd`) builds the `SpawnSpec` that runs the orchestrator as
  the session PTY's **foreground child**: `cd <workdir> && SHELBI_PROJECT=… \
  SHELBI_TMUX_SESSION=… SHELBI_MANAGED_CONTEXT=1 exec <launch>` under `$SHELL
  -lc`. Both tmux artifacts the Codex three-process unit depended on
  (`docs/removing-tmux/phase0/agents.md`, item 4) are gone:
  - **No `exec 3<&0` stdin dup.** The pane wrapper dups fd 0 only because it
    backgrounds the orchestrator as a shell job; a session owns the PTY and
    `exec`s the launch directly, so the PTY slave *is* the only stdin. The Codex
    bridge's inherited remote TUI (`Stdio::inherit`) draws straight to the
    session PTY, and `app-server` (`Stdio::null`) is unchanged — `wake.rs` needs
    no edit.
  - **No `$TMUX_PANE`.** The crash-record tail and the zen heartbeat / signal
    traps are not reproduced; they are Phase 3 daemon/session supervision
    (`rt-daemon-poller`). The orchestrator's identity is the session target
    `<project>/orch`, not a tmux pane id. Belt and braces: the session child env
    already scrubs `TMUX` / `TMUX_PANE` / `STY` (`SCRUBBED_TERMINAL_VARS`), so
    even a stray reference could not leak the launcher's.
- **`ensure_dashboard`** gets a flag branch (after the backend-agnostic setup —
  project-open, commit-guard refresh, agent-context deploy) that calls
  `ensure_orchestrator_session` and returns, instead of building the tmux
  dashboard. `ensure_orchestrator_session` probes `<project>/orch` for
  idempotency (the session-backend equivalent of the "2+ panes" early return),
  claims/re-arms the first-launch greeting exactly as the tmux path does, and
  spawns through `Backend::spawn_orchestrator_session`.
- **Tmux default is byte-identical with the flag off.** The branch is a single
  `if session_backend_enabled()` guard; every existing `ensure_dashboard` test
  runs the unchanged tmux path.

### What is *not* in this branch (and why), deferred to the named phase

- **The visual dashboard layout** — the sidebar pane, the hidden
  task/review/activity views, and the `swap-pane` arrangement — is tmux topology
  with no session analogue. Standing up a session-backend view layout is the
  Phase 4 TUI shell's job (`rt-tui-shell`), so the session branch brings up the
  **orchestrator process** (the AC4 scope) and leaves the view layout to that
  phase. A developer exercising the flag today gets a running session-process
  orchestrator reachable through the backend (`shelbi session attach`), not a
  tmux dashboard.
- **Orchestrator supervision** (crash record, heartbeat, restart) is Phase 3
  (`rt-daemon-poller`); `supervise_restart_orchestrator` is already generic over
  the backend and the session backend's `respawn → Failed` routes it to a
  rebuild, matching the session model.

### Tests for AC4

- Unit (`lib.rs`): `orchestrator_session_spec` execs the launch dup-free with no
  `$TMUX_PANE` / heartbeat / trap, keeps `SHELBI_MANAGED_CONTEXT=1`, names the
  session `<project>/orch`, and shell-escapes a spaced workdir.
- E2e (`shelbi-cli` `tests/session_backend_e2e.rs`,
  `orchestrator_runs_as_a_session_without_tmux_pane_and_delivers_input_once`):
  spawns a real `shelbi __session` through `orchestrator_session_spec` with a
  stub Codex bridge, and asserts the process's `$TMUX_PANE` is unset and a steer
  sent through the backend arrives **exactly once**.

## Manual check — real agents in a session (carried over from review)

Confirmed on macOS (Darwin 25.6, arm64) with the built binary, flag on, no
client attached:

- **Claude Code 2.1.289** (`shelbi session new … -- claude`): booted fully and
  reached its trust-folder prompt / input box.
- **Codex 0.160.0** (`… -- codex`): booted fully to its `› Ask Codex to do
  anything` composer with the model line and `? for shortcuts` footer.

Both reached their prompt with the session process answering startup queries
alone — the Phase 0 `agents.md` responder requirement, now exercised through the
production session binary. The three-process Codex orchestrator's shape (no
`$TMUX_PANE`, no duplicated pane stdin) is now wired behind the flag (above) and
proven by the AC4 e2e, over the Phase 0 spike's process-shape finding.

## Scope / deferred (behind the flag, local-only in Phase 2)

- **Remote (`Host::Ssh`) is Phase 5** (`rt-remote-spawn`): remote operations
  report `Unreachable` / an error rather than silently succeeding, so a remote
  workspace is never mistaken for dead. A developer exercising the flag uses a
  local project.
- **Metadata / session env are not persisted** (`get_metadata` / `get_env`
  return `Ok(None)`, `set_metadata` no-op): the `@shelbi-user-shell` mark and
  the `SHELBI_PANE_orch` / review-interface keys have no session-process store
  yet. Every caller treats the absent value as a safe default (no pinned pane,
  no user-shell mark, no parked interface → rebuild from scratch). Persisting
  these moves with the daemon/TUI in Phases 3–4.
- **`respawn`** has no in-place analogue (a session keeps its binary); it
  reports `Failed`, which the orchestrator-restart caller already treats as
  "rebuild", matching the session model (a crashed session is replaced by a
  fresh spawn under Phase 3 supervision).

## Config-upgrade note

`DevConfig.session_backend` is a new **optional** config key that defaults to
off when absent (the `dev:` block is omitted from the file entirely at
defaults). No shipped `*.template` / default config / workflow / instructions
file changed, so existing installs need no config-upgrade sniffer — same
reasoning as the `daemon_mutations` flag that preceded it.
