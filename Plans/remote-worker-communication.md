# Remote Worker → Orchestrator Communication

## Context

Today devbox workspaces (delta, echo, foxtrot) live behind SSH. Hub manages them outbound: opening remote tmux sessions, polling `status.yaml` files. When a remote worker needs to *send* a signal back — "my pane died," "I finished a task," "the build failed" — there's no clean channel. Workers write `status.yaml` on the remote box; the hub-side poller `ssh`s in and reads it. That's read-only, polling, eventually-consistent, and doesn't carry events.

The new `shelbi workspace open <name>` design (filed as a backlog task) needs to emit `workspace=<name> pane_alive=false reason=<short>` on exit. For hub workspaces that's a local file append. For remote workspaces it needs to land in hub's `~/.shelbi/events.log` somehow — soon enough that the orchestrator's reaction rules can fire.

The requirement: a remote→hub channel that is **not fragile** and **secure**, without adding new auth surface, public listeners, or services that need lifecycle management beyond what shelbi already has.

## Design

### 1. Reverse Unix-socket forward on the existing SSH connection

Hub already maintains a long-lived outbound SSH connection per remote host (to manage tmux). Extend that connection with a reverse Unix-domain socket forward:

```
ssh -o ControlMaster=auto -o ControlPath=~/.shelbi/ssh/%r@%h \
    -R /tmp/shelbi-hub.sock:$HOME/.shelbi/hub.sock \
    devbox
```

On hub, `$HOME/.shelbi/hub.sock` is a Unix socket served by a hub-side daemon. On devbox, `/tmp/shelbi-hub.sock` is the same socket, surfaced through SSH. Anything that writes to it on devbox lands on hub.

This reuses the existing SSH trust, opens **zero new public listeners**, and works through any NAT/firewall configuration that already permits hub→devbox SSH (which it must, for shelbi to function at all).

### 2. Hub-side daemon (`shelbid`)

A tiny daemon listens on `~/.shelbi/hub.sock`. For each newline-delimited JSON message, validate and append to `~/.shelbi/events.log`. Single-process, single-file writer — eliminates concurrent-write races that today only "happen to work" because hub is the sole writer.

Message format:

```json
{"verb":"event","project":"shelbi","line":"workspace=delta pane_alive=false reason=signal:SIGHUP"}
```

Initial verbs: just `event`. Future verbs without breaking wire compat: `task-claim`, `request-handoff`, `request-merge`, etc.

The daemon is started by `shelbi init` as a launchd/systemd user service. One per user, not per project — projects multiplex through the `project` field on each message.

### 3. Remote-side CLI

The `shelbi` binary on devbox already has an `event` command path. Make it dual-mode:

- If `$SHELBI_HUB_SOCK` is set and the socket exists → write JSON to the socket.
- Otherwise → fall back to local file append (today's behavior; still useful for hub workspaces).

`shelbi workspace open` (on remote) sets `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock` before exec'ing the agent, so every child process inherits the env. No agent code changes — it shells out to `shelbi event` as it would on hub.

### 4. Reliability: queue-and-replay on disconnect

If the socket is unreachable (SSH dropped, hub restarting, daemon crashed), `shelbi event` appends to `~/.shelbi/outbox/<unix-ts>-<rand>.jsonl` on devbox instead of failing. On every successful socket connect, `shelbi event` drains the outbox first (oldest → newest) before sending the new event.

This makes the channel survive short drops without silently losing events. Cost: at most one disk write per event, plus catchup on reconnect.

If the outbox grows beyond a configurable cap (e.g. 1000 entries / 10 MB), drop oldest with a warning event when the channel returns. Workers get backpressure without blocking on a dead hub.

### 5. Authentication

There is none beyond SSH. The reverse forward is created by hub's outbound SSH session, so anything that can write to `/tmp/shelbi-hub.sock` on devbox is already inside the trust boundary the hub explicitly established. No tokens, no TLS, no PKI to manage or rotate.

Threat model:

- **Devbox compromise** — attacker can forge `events.log` lines. But they can also run any code in the agent's identity, so this is strictly weaker than what they already have. No new attack surface.
- **Network MITM** — SSH is the encryption + integrity layer. Same posture as the existing hub→devbox channel.
- **Multi-tenant devbox** — `/tmp/shelbi-hub.sock` is world-writable by default. If devbox is shared between users, scope the socket to the shelbi user's `XDG_RUNTIME_DIR` (`/run/user/<uid>/shelbi.sock`) which is per-user.

### 6. What this does NOT solve

- **Hub → remote eventing.** Already covered by SSH command invocation (`ssh devbox shelbi ...`). This plan only handles remote → hub.
- **Multi-hub coordination.** One project, one hub. No leader election.
- **Cross-project events at the wire level.** The `project` field on messages is what `shelbid` uses to route to the correct `events.log`; there is no cross-project subscribe.

## Open questions

1. **Daemon shape** — separate `shelbid` binary, or a `shelbi daemon` subcommand started by launchd that runs in the foreground? Preference: subcommand. One binary, one install path.
2. **Socket location** — `/tmp/shelbi-hub.sock` is convenient; `$XDG_RUNTIME_DIR/shelbi.sock` (linux) / `~/.shelbi/hub.sock` (macOS) is safer. Probably the latter, with documented `SHELBI_HUB_SOCK` override.
3. **Reverse-forward lifetime** — does shelbi own the SSH ControlMaster, or piggyback on whatever the user has? Owning it is cleaner (we can guarantee the `-R` flag), at the cost of more SSH-config logic.
4. **Outbox drain ordering** — strict FIFO, or "newest first to surface live state faster"? FIFO is simpler and the right default.

## Phasing

1. **Phase 1 — `shelbi daemon` + local socket.** Hub-side only. Daemon listens, validates, appends to `events.log`. Smoke test: `echo '{"verb":"event",...}' | nc -U ~/.shelbi/hub.sock` lands a line.
2. **Phase 2 — reverse forward in remote-pane SSH.** Extend hub's outbound SSH to remote workspaces with `-R`. Smoke test: `ssh devbox 'echo {...} | nc -U /tmp/shelbi-hub.sock'` lands a line in hub's `events.log`.
3. **Phase 3 — `shelbi event` socket mode.** Teach the CLI to prefer `$SHELBI_HUB_SOCK` when set. Wire `shelbi workspace open` to set the env on remote. Remote workers can now emit events that surface on hub.
4. **Phase 4 — outbox + replay.** Disconnect resilience. Cap + drop-oldest policy.

Phase 1–3 are the minimum viable channel. Phase 4 is the "not fragile" upgrade once the basic shape is proven.