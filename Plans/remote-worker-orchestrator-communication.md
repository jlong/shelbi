# Remote Worker → Orchestrator Communication

## Context

Workers — both hub-local (alpha, bravo, charlie) and remote (delta, echo, foxtrot behind SSH) — need to send signals back to the orchestrator: "my pane died," "I finished a task," "the build failed." Today there is no proper channel:

- **Hub workers** append directly to `~/.shelbi/events.log`. This works because the filesystem is shared, but multiple writers race on the same file (today only "happens to work" because hub is largely the sole writer).
- **Remote workers** write `status.yaml` on the remote box; the hub-side poller `ssh`s in and reads it. Read-only, polling, eventually-consistent, no event semantics.

The new `shelbi open <name>` design (`feat-rename-shelbi-workspace-open-shelbi-open-top-level` + its parent task) needs to emit `workspace=<name> pane_alive=false reason=<short>` on exit. We want a single, unified mechanism that works for hub and remote — not two parallel paths.

The requirements: a worker→hub channel that is **not fragile** and **secure**, with no new auth surface, no public listeners, no extra services beyond what shelbi already manages, and — critically — **zero install burden on remote workers**. Remote machines don't necessarily have shelbi or any helper script; the wire protocol must be reachable with stock Unix tools.

## Design

### 1. Architecture overview

```
                          hub
                        +----------------------------+
                        |  events.log                |
                        |     ^                      |
hub worker (alpha) ---> | (nc -U / socat / python)   |
                        |     |                      |
                        |     v                      |
                        |  ~/.shelbi/hub.sock <-- shelbi daemon
                        |     ^                      |
                        +-----|----------------------+
                              | SSH -R forward
                              v
                        +----------------------------+
                        |  /tmp/shelbi-hub.sock     |
remote worker (delta)-> | (nc -U / socat / python)   |
                        +----------------------------+
                                  devbox
                                (no shelbi binary,
                                 no wrapper script)
```

Single daemon (`shelbi daemon`) on hub owns all writes to `events.log`. Workers — hub or remote — send messages to it by writing newline-delimited JSON directly to `~/.shelbi/hub.sock` (hub) or `/tmp/shelbi-hub.sock` (remote, surfaced via SSH reverse-forward). No CLI involved on the worker side. The agent uses whatever socket-writing tool is on PATH.

### 2. Wire protocol — JSON lines on a Unix socket

One message per line, newline-terminated:

```json
{"verb":"event","project":"shelbi","line":"workspace=delta pane_alive=false reason=signal:SIGHUP"}
{"verb":"request-clarification","project":"shelbi","task_id":"feat-X","question":"...","context":"..."}
```

Initial verbs: `event`. Near-term: `request-clarification` (see §7). Future without breaking wire compat: `task-claim`, `request-handoff`, `request-merge`.

Any tool that can write bytes to a Unix-domain socket works:

```sh
# nc / ncat (BSD or OpenBSD flavor with -U)
echo '{"verb":"event",...}' | nc -U $SHELBI_HUB_SOCK

# socat
echo '{"verb":"event",...}' | socat - UNIX-CONNECT:$SHELBI_HUB_SOCK

# Python (always present on macOS and most Linux)
python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect("'$SHELBI_HUB_SOCK'"); s.sendall(sys.stdin.buffer.read())' < msg.json
```

The agent picks whichever tool is on PATH. Agent instructions list the preferences in order.

### 3. Hub workers

Hub workers set `SHELBI_HUB_SOCK=~/.shelbi/hub.sock` in their agent env. They write to the local socket using the same one-liner as remote workers. There is no shelbi CLI involved — the agent shells out directly.

**Fallback when the daemon is down**: hub workers can also `O_APPEND`-write to `events.log` directly (POSIX guarantees atomicity ≤ `PIPE_BUF`). The agent instructions name this as the fallback when the socket connect fails. Loss of the single-writer property is accepted in degraded mode.

The daemon is therefore a **soft dependency** for hub: nice for ordering and future verbs, not required for basic event emission.

### 4. Remote workers

Remote workers set `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock` (the reverse-forward landing point). Same one-liner pattern. **No shelbi binary, no wrapper script, no install.** The agent instructions say *what to write* and *where*; the agent figures out *how* with the tools at hand.

When the socket is unreachable (SSH dropped, hub down, daemon crashed) the write fails. There is no remote-side outbox — that would require a persistent process or a wrapper, both of which we are explicitly avoiding. See §6 for how loss is handled.

### 5. Hub-side daemon (`shelbi daemon`)

A subcommand of the main `shelbi` binary (not a separate `shelbid`) — one install path, one binary on hub. Listens on `~/.shelbi/hub.sock`. For each newline-delimited JSON message, validate and append to the project's `events.log`. One daemon per user, not per project — projects multiplex through the `project` field on each message.

**Stateless between restarts.** `events.log` is the durable record; in-flight messages live in the SSH connection's kernel socket buffer. A daemon crash + restart resumes accepting messages, no recovery state to rebuild.

### 6. SSH reverse forward and daemon supervision

#### Reverse forward

Hub already maintains a long-lived outbound SSH connection per remote host (to manage tmux). Extend that connection with a reverse Unix-domain socket forward:

```
ssh -o ControlMaster=auto -o ControlPath=~/.shelbi/ssh/%r@%h \
    -R /tmp/shelbi-hub.sock:$HOME/.shelbi/hub.sock \
    devbox
```

Reuses existing SSH trust, opens **zero new public listeners**, works through any NAT/firewall configuration that already permits hub→devbox SSH.

#### Daemon supervision — OS-native user services

Don't write our own supervisor. Use the OS:

- **macOS**: `~/Library/LaunchAgents/co.32pixels.shelbi.plist` with `KeepAlive: true`, `RunAtLoad: true`, log paths. launchd handles crash recovery, start-on-login, and process accounting.
- **Linux**: `~/.config/systemd/user/shelbi.service` with `Restart=always`, `RestartSec=1s`, `WantedBy=default.target`. `systemctl --user` manages the lifecycle. On headless boxes the user also needs `loginctl enable-linger <user>` so the daemon survives logout — install script prints this instruction but doesn't run it automatically (it's a system-wide change requiring sudo).

`scripts/install.sh` writes the platform-appropriate unit file as part of normal install. Gated by `--no-daemon` for users who want to opt out (CI envs, ephemeral containers, debugging).

Runtime management:

- `shelbi daemon install` — write unit file + load it (idempotent).
- `shelbi daemon uninstall` — unload + remove unit file.
- `shelbi daemon status` — `launchctl print` / `systemctl --user status` summary.
- `shelbi daemon restart` — pick up new binaries.
- `shelbi daemon` (no subcommand) — foreground entry point launchd/systemd invokes.

### 7. Loss handling — best-effort + hub-side detection

Because remote workers have no outbox and no retry daemon, the channel is **best-effort**. The design accepts occasional loss, with two safety nets:

1. **SSH-drop detection is hub-side.** The hub's outbound SSH ControlMaster is already monitored (it has to be — it carries pane management). When it drops, the orchestrator knows the workspace is unreachable, which is functionally `pane_alive=false`. The most important event (worker death) is therefore covered by hub-side connection monitoring even if the worker's own emit never lands.
2. **Agent-instructed retry.** The agent prompt says: *"if your socket write fails, retry once after 500ms; if it still fails, continue with your work and the orchestrator will reconcile from hub-side state."* No persistent retry queue, just a single immediate retry.

This trades a small probability of lost low-stakes events (chatter, intermediate progress) for a much simpler remote-side architecture.

### 8. Worker-initiated clarification

Workers should ask when ambiguous rather than guess. Wire-level support is a verb:

```json
{"verb":"request-clarification","project":"shelbi","task_id":"feat-X","question":"<text>","context":"<excerpt>"}
```

Agent-instructions paragraph (`agents/developer/instructions.md`):

> When facing ambiguity in a task spec — missing requirements, conflicting constraints, surprising existing code — prefer asking over guessing. Send a clarification request by writing this JSON to `$SHELBI_HUB_SOCK`:
>
>     {"verb":"request-clarification","project":"<project>","task_id":"<your-task-id>","question":"<concise question>","context":"<short excerpt of the relevant code or spec>"}
>
> Then pause and wait. The orchestrator will surface your question to the user and route their reply back to you via your tmux pane.
>
> Before emitting a review marker, briefly summarize the key decisions you made in the commit message — this helps the orchestrator catch silent wrong-assumption cases at review time.

Orchestrator reaction rule: surface clarification requests as a sidebar badge or popup; route the user's reply back via `shelbi send <workspace>` (depends on the send bug being fixed).

The "summarize key decisions" instruction is the safety net for the common failure mode where the agent doesn't realize it's making an assumption.

### 9. Authentication

There is none beyond SSH and Unix-socket file permissions. The reverse forward is created by hub's outbound SSH session, so anything that can write to `/tmp/shelbi-hub.sock` on devbox is already inside the trust boundary the hub explicitly established. On hub, the socket lives in the user's home directory with `0600` permissions. No tokens, no TLS, no PKI.

Threat model:

- **Devbox compromise** — attacker can forge `events.log` lines. But they can already run any code in the agent's identity, so this is strictly weaker than what they have. No new attack surface.
- **Network MITM** — SSH is the encryption + integrity layer. Same posture as the existing hub→devbox channel.
- **Multi-tenant devbox** — `/tmp/shelbi-hub.sock` is world-writable by default. If devbox is shared between users, scope to `$XDG_RUNTIME_DIR/shelbi.sock` (`/run/user/<uid>/shelbi.sock`).
- **Multi-user hub** — `~/.shelbi/hub.sock` is per-user by virtue of being in the user's home. One daemon per user; no cross-user contamination.

### 10. What this does NOT solve

- **Hub → remote eventing.** Already covered by SSH command invocation (`ssh devbox tmux send-keys ...`). This plan only handles worker → hub.
- **Multi-hub coordination.** One project, one hub. No leader election.
- **Cross-project events at the wire level.** The `project` field routes to the correct `events.log`; no cross-project subscribe.

## Open questions

1. **Socket location on hub** — `~/.shelbi/hub.sock` (cross-platform, in user home) vs `$XDG_RUNTIME_DIR/shelbi.sock` (linux-native, tmpfs). Lean toward `~/.shelbi/hub.sock` with documented `SHELBI_HUB_SOCK` override.
2. **Reverse-forward lifetime** — does shelbi own the SSH ControlMaster, or piggyback on whatever the user has? Owning it is cleaner (we can guarantee the `-R` flag), at the cost of more SSH-config logic.
3. **Daemon binary identity on launchd** — `co.32pixels.shelbi` vs `co.32pixels.shelbi.daemon`. Matters for log paths and `launchctl print` discoverability.
4. **Install-script opt-out default** — `--no-daemon` (opt out, daemon default-on) or `--with-daemon` (opt in)? Lean default-on with clear messaging.
5. **Agent tool preference order** — should the prompt suggest `nc -U` first, or `python3` for portability? `nc -U` is shorter; `python3` is more universally present. Probably list both, agent picks.

## Phasing

1. **Phase 1 — `shelbi daemon` + local socket on hub.** Daemon listens, validates, appends to `events.log`. Smoke test: `echo '{"verb":"event",...}' | nc -U ~/.shelbi/hub.sock` lands a line.
2. **Phase 2 — daemon supervision.** launchd plist + systemd unit. `shelbi daemon install/uninstall/status/restart`. `scripts/install.sh` integration. Smoke test: kill the daemon, watch it come back in ≤1s.
3. **Phase 3 — hub workers use the socket.** Agent instructions updated; `shelbi open` sets `SHELBI_HUB_SOCK=~/.shelbi/hub.sock` in the agent env. Hub workers emit via direct socket writes; O_APPEND fallback documented for daemon-down.
4. **Phase 4 — reverse forward in remote-pane SSH.** Extend hub's outbound SSH to remote workspaces with `-R`. Smoke test: `ssh devbox 'echo {...} | nc -U /tmp/shelbi-hub.sock'` lands a line in hub's `events.log`.
5. **Phase 5 — remote workers use the socket.** `shelbi open` on remote sets `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock` in the agent env. Agent instructions updated with the JSON shape and socket-write tool preferences. Remote workers emit events that surface on hub in real time.
6. **Phase 6 — clarification verb.** Add `request-clarification` handling in `shelbi daemon`. Orchestrator reaction rule surfaces a popup/badge and routes user reply back via `shelbi send` (requires `bug-shelbi-send-does-not-work-with-workspace-based-agents-...` first).

Phases 1–5 are the minimum viable channel. Phase 6 layers on conversational support once `shelbi send` is fixed.