# rt-remote-spawn — In review

Remote workspaces on the session backend: spawn over SSH, reach them through the
relay, survive SSH drops (plan "Remote machines" + Phase 5). Behind the hidden
`session_backend` dev flag; the tmux remote path is unchanged.

## What landed

- **`crates/shelbi-orchestrator/src/remote_session.rs`** — the hub side of a
  remote session:
  - **SSH seam** (`RemoteSsh`): `launch` (run `shelbi session new …` on the
    remote, which detaches and returns) + `open_relay` (`ssh <host> <bin> relay`
    with piped stdio wired to a `shelbi_client::RelayChannel`). Production
    `SshSeam` shells out through `shelbi_ssh::build_command` /
    `run_with_deadline`, so the reverse `hub.sock` forward and the
    `SHELBI_HUB_ADDR` env prefix ride along unchanged (AC5). `set_test_seam`
    installs a fake for tests.
  - **Per-machine relay cache** (keyed by SSH host): get-or-create, and a
    one-shot reconnect — a dead channel is dropped and reopened, so the next op
    reaches the still-running session (AC2). The SSH child is reaped on drop.
  - **Relay-backed ops**: `probe` (three-state: Alive/Dead when the machine
    answers, **Unreachable** when it cannot be reached — never Dead, AC3),
    `send_text`/`send_enter`, `snapshot`, `title`, `kill`, `resize`,
    `enumerate`, `live_session_names` — each `list_sessions()` → find by logical
    name → `open(short_id)` → `Connection`.
  - **Binary gate**: `resolve_remote_bin(machine)` reads the `rt-machine-setup`
    record (`shelbi_state::machine_state`); missing or incompatible → an error
    naming `shelbi machine setup <machine>` (AC4). `relay_bin_for_host` resolves
    the relay binary by host for the backend ops (falls back to `shelbi` on PATH).
- **`session_process_backend.rs`** — every `Host::Ssh` branch now delegates to
  `remote_session` instead of returning "Phase 5" errors.
- **`workspace.rs` `deploy_and_spawn`** — the remote arm gained a
  `session_backend_enabled()` branch: resolve+gate the remote binary, then
  `spawn_remote_session` running the same `cd … && … exec <runner>` launch line
  (`remote_cd_launch`) the tmux path builds, under the remote login shell. The
  tmux branch is byte-for-byte the old code.

## Tests

- Unit (`remote_session`): `session new` argv construction; `resolve_remote_bin`
  (missing/incompatible → names `shelbi machine setup`, compatible → path);
  `relay_bin_for_host` fallback.
- Unit (`session_process_backend`): remote ops against an unreachable machine
  (probe Unreachable not Dead; ops error; enumerate None).
- **E2E** (`shelbi-cli/tests/remote_session_e2e.rs`, stubbed SSH seam as the ACs
  permit): a real `shelbi __session` launched "remotely" (locally, real binary)
  and reached through an in-process `serve_relay` — dispatch → Alive → ready
  snapshot → **relay killed mid-task** → reconnect → deliver → handoff
  (`shelbi:review` + marker) (AC1, AC2); then an unreachable seam → probe
  Unreachable, `into_exists()` errors so supervision won't redispatch (AC3).

## Notes / scope

- Dead remote `final_screen` reads the live screen only (a relay bridges live
  sockets; a dead session's `final.txt` is not reachable that way). Crash-record
  callers already tolerate the resulting error.
- `relay_bin_for_host` keys the record by the SSH host string (host == machine
  name in the common config); the authoritative compatibility gate runs at
  dispatch where the machine name is known. Full host↔machine resolution for the
  poller's independent probes is a follow-up.

## Config-upgrade note

No shipped `*.template` / default config / workflow / instructions file changed
(new in-code module + backend/dispatch wiring behind the existing hidden
`session_backend` flag; the agent templates already consume `SHELBI_HUB_ADDR`),
so existing installs need no config-upgrade sniffer — same reasoning as the
`session_backend` flag itself.
