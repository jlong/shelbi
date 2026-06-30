# Remote Worker → Orchestrator Communication

## Context

Workers — both hub-local (alpha, bravo, charlie) and remote (delta, echo, foxtrot behind SSH) — need to send signals back to the orchestrator: "my pane died," "I finished a task," "the build failed." Today there is no proper channel:

- **Hub workers** append directly to `~/.shelbi/events.log`. This works because the filesystem is shared, but multiple writers race on the same file (today only "happens to work" because hub is largely the sole writer).
- **Remote workers** write `status.yaml` on the remote box; the hub-side poller `ssh`s in and reads it. Read-only, polling, eventually-consistent, no event semantics.

The new `shelbi open <name>` design (`feat-rename-shelbi-workspace-open-shelbi-open-top-level` + its parent task) needs to emit `workspace=<name> pane_alive=false reason=<short>` on exit. We want a single, unified mechanism that works for hub and remote — not two parallel paths.

The requirements: a worker→hub channel that is **not fragile** and **secure**, with no new auth surface, no public listeners, and no extra services beyond what shelbi already manages.

## Design

### 1. Architecture overview

```
                          hub
                        +--------------------------+
                        |  events.log              |
                        |     ^                    |
hub worker (alpha) ---> |   shelbi event          ...|
                        |     |                    |
                        |     v                    |
                        |  ~/.shelbi/hub.sock <-- shelbi daemon
                        |     ^                    |
                        +-----|--------------------+
                              | SSH -R forward
                              v
                        +--------------------------+
                        |  /tmp/shelbi-hub.sock   |
remote worker (delta)-> |     ^                    |
                        |   shelbi event          ...|
                        +--------------------------+
                                  devbox
```

Single daemon (`shelbi daemon`) on hub owns all writes to `events.log`. Workers — hub or remote — send messages to it via `~/.shelbi/hub.sock`. Remote workers reach that socket through an SSH reverse-forward. Same `shelbi event` CLI code path everywhere; the only difference is what `SHELBI_HUB_SOCK` resolves to.

### 2. Hub workers — direct socket

Hub workers set `SHELBI_HUB_SOCK=~/.shelbi/hub.sock` (a local path, no SSH bridge needed). The CLI writes JSON to the socket; daemon validates and appends.

**Fallback**: if the socket is missing (daemon down, not yet installed), `shelbi event` falls back to `O_APPEND` write on `events.log` directly. POSIX guarantees atomicity for writes ≤ `PIPE_BUF` (4096 bytes typically), and event lines are well under that. The system stays usable in a daemon-down scenario; the only loss is the single-writer property until the daemon returns.

This makes the daemon a **soft dependency** for hub: nice to have for ordering and future verbs, not required for basic event emission.

### 3. Remote workers — SSH reverse Unix-socket forward

Hub already maintains a long-lived outbound SSH connection per remote host (to manage tmux). Extend that connection with a reverse Unix-domain socket forward:

```
ssh -o ControlMaster=auto -o ControlPath=~/.shelbi/ssh/%r@%h \
    -R /tmp/shelbi-hub.sock:$HOME/.shelbi/hub.sock \
    devbox
```

On hub, `$HOME/.shelbi/hub.sock` is served by `shelbi daemon`. On devbox, `/tmp/shelbi-hub.sock` is the same socket, surfaced through SSH. Anything that writes to it on devbox lands on hub.

This reuses existing SSH trust, opens **zero new public listeners**, and works through any NAT/firewall configuration that already permits hub→devbox SSH (which it must, for shelbi to function at all).

For remote, the daemon is a **hard dependency**: the SSH forward has nowhere to land without it. No file fallback (the remote can't reach hub's filesystem).

### 4. Hub-side daemon (`shelbi daemon`)

A subcommand of the main `shelbi` binary (not a separate `shelbid`) — one install path, one binary. Listens on `~/.shelbi/hub.sock`. For each newline-delimited JSON message, validate and append to the project's `events.log`.

Message format:

```json
{"verb":"event","project":"shelbi","line":"workspace=delta pane_alive=false reason=signal:SIGHUP"}
```

Initial verbs: just `event`. Future verbs without breaking wire compat: `task-claim`, `request-handoff`, `request-merge`, etc.

One daemon per user, not per project — projects multiplex through the `project` field on each message.

**Stateless between restarts.** `events.log` is the durable record; in-flight messages live in the worker's outbox (see §6). A daemon crash + restart resumes accepting messages, no recovery state to rebuild.

### 5. Daemon supervision — OS-native user services

Don't write our own supervisor. Use the OS:

- **macOS**: `~/Library/LaunchAgents/co.32pixels.shelbi.plist` with `KeepAlive: true`, `RunAtLoad: true`, log paths. launchd handles crash recovery, start-on-login, and process accounting.
- **Linux**: `~/.config/systemd/user/shelbi.service` with `Restart=always`, `RestartSec=1s`, `WantedBy=default.target`. `systemctl --user` manages the lifecycle. On headless boxes the user also needs `loginctl enable-linger <user>` so the daemon survives logout — install script prints this instruction but doesn't run it automatically (it's a system-wide change requiring `sudo`).

`scripts/install.sh` writes the platform-appropriate unit file as part of normal install. Gated by `--no-daemon` for users who want to opt out (CI envs, ephemeral containers, debugging).

Runtime management commands:

- `shelbi daemon install` — write the unit file + load it (idempotent).
- `shelbi daemon uninstall` — unload + remove the unit file.
- `shelbi daemon status` — `launchctl print` / `systemctl --user status` summary.
- `shelbi daemon restart` — for picking up new binaries.
- `shelbi daemon` (no subcommand) — the foreground entry point launchd/systemd invokes.

### 6. Reliability — queue-and-replay on disconnect

For **remote** workers: if the socket is unreachable (SSH dropped, hub restarting, daemon crashed), `shelbi event` appends to `~/.shelbi/outbox/<unix-ts>-<rand>.jsonl` on devbox instead of failing. On every successful socket connect, drains the outbox first (oldest → newest) before sending the new event.

This makes the channel survive short drops without silently losing events. Cost: at most one disk write per event, plus catchup on reconnect.

If the outbox grows beyond a configurable cap (e.g. 1000 entries / 10 MB), drop oldest with a warning event when the channel returns. Workers get backpressure without blocking on a dead hub.

For **hub** workers: outbox is less critical because of the O_APPEND fallback — the worker can always write *something*. But the same outbox mechanism still applies for verbs that the daemon must process (not just append). Phase that in alongside future-verb work.

### 7. Authentication

There is none beyond SSH and Unix-socket file permissions. The reverse forward is created by hub's outbound SSH session, so anything that can write to `/tmp/shelbi-hub.sock` on devbox is already inside the trust boundary the hub explicitly established. On hub, the socket lives in the user's home directory with `0600` permissions. No tokens, no TLS, no PKI to manage or rotate.

Threat model:

- **Devbox compromise** — attacker can forge `events.log` lines. But they can also run any code in the agent's identity, so this is strictly weaker than what they already have. No new attack surface.
- **Network MITM** — SSH is the encryption + integrity layer. Same posture as the existing hub→devbox channel.
- **Multi-tenant devbox** — `/tmp/shelbi-hub.sock` is world-writable by default. If devbox is shared between users, scope the socket to the shelbi user's `$XDG_RUNTIME_DIR` (`/run/user/<uid>/shelbi.sock`) which is per-user.
- **Multi-user hub** — `~/.shelbi/hub.sock` is per-user by virtue of being in the user's home. One daemon per user; no cross-user contamination.

### 8. What this does NOT solve

- **Hub → remote eventing.** Already covered by SSH command invocation (`ssh devbox shelbi ...`). This plan only handles worker → hub.
- **Multi-hub coordination.** One project, one hub. No leader election.
- **Cross-project events at the wire level.** The `project` field routes the message to the correct `events.log`; there is no cross-project subscribe.

## Open questions

1. **Socket location** — `/tmp/shelbi-hub.sock` is convenient; `$XDG_RUNTIME_DIR/shelbi.sock` (linux) / `~/.shelbi/hub.sock` (macOS) is safer. Probably the latter on hub, with documented `SHELBI_HUB_SOCK` override. Remote-side `/tmp/...` is fine because the SSH forward controls who can put it there.
2. **Reverse-forward lifetime** — does shelbi own the SSH ControlMaster, or piggyback on whatever the user has? Owning it is cleaner (we can guarantee the `-R` flag), at the cost of more SSH-config logic.
3. **Outbox drain ordering** — strict FIFO, or "newest first to surface live state faster"? FIFO is simpler and the right default.
4. **Daemon binary identity on launchd** — `co.32pixels.shelbi` vs `co.32pixels.shelbi.daemon`. Picking deliberately matters for log paths and `launchctl print` discoverability.
5. **Install-script opt-out default** — `--no-daemon` opt-out or `--with-daemon` opt-in? Default-on is more useful for the typical user; default-off is safer in ambiguous environments (CI, Docker). Probably default-on with clear messaging.

## Phasing

1. **Phase 1 — `shelbi daemon` + local socket on hub.** Daemon listens, validates, appends to `events.log`. `shelbi event` writes to socket when `SHELBI_HUB_SOCK` is set; falls back to O_APPEND otherwise. Smoke test: `echo '{"verb":"event",...}' | nc -U ~/.shelbi/hub.sock` lands a line.
2. **Phase 2 — daemon supervision.** launchd plist + systemd unit. `shelbi daemon install/uninstall/status/restart`. `scripts/install.sh` integration. Smoke test: kill the daemon, watch it come back in ≤1s.
3. **Phase 3 — hub workers use the socket.** `shelbi open` (post-rename) sets `SHELBI_HUB_SOCK=~/.shelbi/hub.sock` in the agent env. Hub workers now route through daemon. Direct-write fallback still present for safety.
4. **Phase 4 — reverse forward in remote-pane SSH.** Extend hub's outbound SSH to remote workspaces with `-R`. Smoke test: `ssh devbox 'echo {...} | nc -U /tmp/shelbi-hub.sock'` lands a line in hub's `events.log`.
5. **Phase 5 — remote workers use the socket.** `shelbi open` on remote sets `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock`. Remote workers emit events that surface on hub in real time.
6. **Phase 6 — outbox + replay.** Disconnect resilience for remote. Cap + drop-oldest policy.

Phases 1–5 are the minimum viable channel. Phase 6 is the "not fragile" upgrade once the basic shape is proven. Phases can ship independently; each leaves the system in a working state.