# Remote Worker → Orchestrator Communication

## Context

Workers — both hub-local (alpha, bravo, charlie) and remote (delta, echo, foxtrot behind SSH) — need to send signals back to the orchestrator: "my pane died," "I finished a task," "the build failed." Today there is no proper channel:

- **Hub workers** append directly to `~/.shelbi/events.log`. This works because the filesystem is shared, but multiple writers race on the same file (today only "happens to work" because hub is largely the sole writer).
- **Remote workers** write `status.yaml` on the remote box; the hub-side poller `ssh`s in and reads it. Read-only, polling, eventually-consistent, no event semantics.

Symmetrically, the orchestrator needs a way to push messages *into* a worker — clarification replies, "stop, the spec changed," "you're on the wrong branch." Today this goes through `tmux send-keys`, which is fragile: input can land in a feedback survey, permission prompt, or any UI element claude-code surfaces between turns.

The new `shelbi open <name>` design needs to emit `workspace=<name> pane_alive=false reason=<short>` on exit, and we want orchestrator→worker push to be UI-independent. We want a single, unified mechanism that works for hub and remote — not parallel paths.

The requirements: a worker↔hub channel that is **not fragile** and **secure**, with no new auth surface, no public listeners, no extra services beyond what shelbi already manages, **zero install burden on remote workers** (no shelbi binary, no wrapper script), and works across agent runtimes (claude, codex, future).

## Design

### 1. Architecture overview

```
                          hub
                        +----------------------------+
                        |  events.log                |
                        |     ^                      |
hub worker ----write--> | (nc -U / socat / python)   |
                        |     |                      |
                        |     v                      |
                        |  ~/.shelbi/hub.sock <-- shelbi daemon
                        |     ^                      |
                        +-----|----------------------+
                              | SSH -R forward
                              v
                        +----------------------------+
                        |  /tmp/shelbi-hub.sock     |
remote worker --write-> | (nc -U / socat / python)   |
                        +----------------------------+

  hub or remote worker:
   reads .shelbi/messages/$TASK_ID.log
   (hub writes to it directly on hub;
    via ssh redirect on remote)
```

Two channels:

- **Worker → Hub**: JSON lines into a Unix socket. `shelbi daemon` validates and appends to `events.log`.
- **Hub → Worker**: append-only `.shelbi/messages/$TASK_ID.log` file in the worktree. Worker tails it; orchestrator appends to it.

Both channels are file/socket primitives — no agent-specific protocol, no install on workers.

### 2. Wire protocol — JSON lines on a Unix socket (worker → hub)

One message per line, newline-terminated:

```json
{"verb":"event","project":"shelbi","line":"workspace=delta pane_alive=false reason=signal:SIGHUP"}
{"verb":"request-clarification","project":"shelbi","task_id":"feat-X","question":"...","context":"..."}
```

Initial verbs: `event`. Near-term: `request-clarification`. Future without breaking wire compat: `task-claim`, `request-handoff`, `request-merge`.

Any tool that can write bytes to a Unix-domain socket works:

```sh
echo '{"verb":"event",...}' | nc -U $SHELBI_HUB_SOCK
echo '{"verb":"event",...}' | socat - UNIX-CONNECT:$SHELBI_HUB_SOCK
python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect("'$SHELBI_HUB_SOCK'"); s.sendall(sys.stdin.buffer.read())' < msg.json
```

The agent picks whichever tool is on PATH. Agent instructions list the preferences in order.

### 3. Hub workers

Hub workers set `SHELBI_HUB_SOCK=~/.shelbi/hub.sock` in their agent env. They write to the local socket using the same one-liner as remote workers. There is no shelbi CLI involved — the agent shells out directly.

**Fallback when the daemon is down**: hub workers can `O_APPEND`-write to `events.log` directly (POSIX guarantees atomicity ≤ `PIPE_BUF`). The agent instructions name this as the fallback when the socket connect fails. Loss of the single-writer property is accepted in degraded mode. The daemon is a **soft dependency** for hub.

### 4. Remote workers

Remote workers set `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock` (the reverse-forward landing point). Same one-liner pattern. **No shelbi binary, no wrapper script, no install.** When the socket is unreachable (SSH dropped, hub down, daemon crashed) the write fails. There is no remote-side outbox — that would require a persistent process or wrapper, both of which we are explicitly avoiding. See §7 for loss handling.

### 5. Hub-side daemon (`shelbi daemon`)

A subcommand of the main `shelbi` binary (not a separate `shelbid`) — one install path, one binary on hub. Listens on `~/.shelbi/hub.sock`. For each newline-delimited JSON message, validate and append to the project's `events.log`. One daemon per user, not per project — projects multiplex through the `project` field on each message.

**Stateless between restarts.** `events.log` is the durable record; in-flight messages live in the SSH connection's kernel socket buffer. A daemon crash + restart resumes accepting messages, no recovery state to rebuild.

### 6. SSH reverse forward and daemon supervision

#### Reverse forward

Hub already maintains a long-lived outbound SSH connection per remote host. Extend that connection with a reverse Unix-domain socket forward:

```
ssh -o ControlMaster=auto -o ControlPath=~/.shelbi/ssh/%r@%h \
    -R /tmp/shelbi-hub.sock:$HOME/.shelbi/hub.sock \
    devbox
```

Reuses existing SSH trust, opens **zero new public listeners**, works through any NAT/firewall configuration that already permits hub→devbox SSH.

#### Daemon supervision — OS-native user services

Don't write our own supervisor. Use the OS:

- **macOS**: `~/Library/LaunchAgents/co.32pixels.shelbi.plist` with `KeepAlive: true`, `RunAtLoad: true`. launchd handles crash recovery, start-on-login, process accounting.
- **Linux**: `~/.config/systemd/user/shelbi.service` with `Restart=always`, `RestartSec=1s`, `WantedBy=default.target`. `systemctl --user` manages lifecycle. On headless boxes the user also needs `loginctl enable-linger <user>` so the daemon survives logout — install script prints this instruction but doesn't run it automatically.

`scripts/install.sh` writes the platform-appropriate unit file as part of normal install. Gated by `--no-daemon` for opt-out (CI envs, ephemeral containers).

Runtime management:

- `shelbi daemon install` / `uninstall` / `status` / `restart`
- `shelbi daemon` (no subcommand) — foreground entry point launchd/systemd invokes.

### 7. Loss handling — best-effort + hub-side detection

Because remote workers have no outbox and no retry daemon, the worker→hub channel is **best-effort**. Two safety nets:

1. **SSH-drop detection is hub-side.** The hub's outbound SSH ControlMaster is already monitored. When it drops, the orchestrator knows the workspace is unreachable, which is functionally `pane_alive=false`. The most important event is covered by hub-side connection monitoring even if the worker's own emit never lands.
2. **Agent-instructed retry.** Agent prompt: *"if your socket write fails, retry once after 500ms; if it still fails, continue with your work and the orchestrator will reconcile from hub-side state."* No persistent retry queue, just a single immediate retry.

This trades a small probability of lost low-stakes events for a much simpler remote-side architecture.

### 8. Worker-initiated clarification

Workers ask via the socket; replies come back via the messages log (§9). Wire-level:

```json
{"verb":"request-clarification","project":"shelbi","task_id":"feat-X","question":"<text>","context":"<excerpt>"}
```

Agent-instructions paragraph (`agents/developer/instructions.md`):

> When facing ambiguity in a task spec — missing requirements, conflicting constraints, surprising existing code — prefer asking over guessing. Send a clarification request by writing this JSON to `$SHELBI_HUB_SOCK`:
>
>     {"verb":"request-clarification","project":"<project>","task_id":"<your-task-id>","question":"<concise question>","context":"<short excerpt of the relevant code or spec>"}
>
> The orchestrator will surface your question to the user and append the reply to your message log. You'll see it via the always-on tail (see §9–§10).
>
> Before emitting a review marker, briefly summarize the key decisions you made in the commit message — this helps the orchestrator catch silent wrong-assumption cases at review time.

### 9. Hub → Worker messaging — append-only per-task log

Each task has a message log at `<worktree>/.shelbi/messages/<task-id>.log` in the worker's worktree. One JSON message per line. The orchestrator appends; the worker reads. The log is durable on disk and survives worker restarts.

Message format:

```json
{"ts":"2026-06-30T01:55:00Z","kind":"reply","in_response_to":"<question-id>","body":"<reply text>"}
{"ts":"2026-06-30T02:10:00Z","kind":"directive","body":"stop what you're doing — the spec changed; re-read the task file before continuing"}
{"ts":"2026-06-30T02:30:00Z","kind":"context","body":"<additional context the orchestrator wants the worker to have>"}
```

Kinds: `reply` (response to a question), `directive` (stop / change course / read X), `context` (background info). Extensible without breaking compat.

Hub-side write path (orchestrator code):

- For hub workers: `echo '<json>' >> <worktree>/.shelbi/messages/<task-id>.log`
- For remote workers: `ssh <host> 'cat >> <worktree>/.shelbi/messages/<task-id>.log' <<<'<json>'`

Single primitive — file append. Works the same for both worker locations. The orchestrator's CLI surface for this is `shelbi message <task-id> <kind> "<body>"` (or similar); the existing broken `shelbi send` can be either fixed to wrap this, or replaced.

Worker read path is agent-specific — see §10.

### 10. Per-agent message delivery — push for claude, pull for codex

The messages log is universal. **How the agent notices a new message** is agent-specific.

#### Claude Code — auto-injection via hooks

Claude Code supports lifecycle hooks. Two hooks deliver messages:

- **`SessionStart`** hook (registered in `agents/<role>/settings.json`) starts a background tail of the message log and creates an "unread" marker file:

      tail -f -n 0 .shelbi/messages/$TASK_ID.log > .shelbi/messages/$TASK_ID.unread.log &

- **`Stop`** (or `PostToolUse`) hook checks `.unread.log` after every agent turn. If there are unread lines, the hook outputs them — Claude Code injects hook output as a system reminder in the next turn:

      if [ -s .shelbi/messages/$TASK_ID.unread.log ]; then
        echo "<system-reminder>New orchestrator messages:"
        cat .shelbi/messages/$TASK_ID.unread.log
        echo "</system-reminder>"
        : > .shelbi/messages/$TASK_ID.unread.log
      fi

Agent receives orchestrator pushes between turns without ever having to remember to check.

#### Codex / future agents — pull-style polling in the agent prompt

Codex and other agents without rich hooks fall back to agent-instructed polling. Their task prompt includes:

> Your message log is at `.shelbi/messages/<your-task-id>.log`. Between significant steps (after each file edit, build, or test run), check it with:
>
>     tail -n +$(cat .shelbi/messages/<your-task-id>.cursor 2>/dev/null || echo 1) .shelbi/messages/<your-task-id>.log
>
> Then update the cursor with the new line count. Act on any messages you find before continuing your current work.

Higher latency (between polls vs between turns) but functional. Same orchestrator-side mechanism; only the delivery half is agent-specific.

#### Future agents

The contract is: "agent must observe new lines in `.shelbi/messages/<task-id>.log` and act on them." If the agent supports hooks, use push. Otherwise, instruct it to poll. Hub-side write API is unchanged.

### 11. Authentication

There is none beyond SSH and Unix-socket / filesystem file permissions. The reverse forward is created by hub's outbound SSH session, so anything that can write to `/tmp/shelbi-hub.sock` on devbox is already inside the trust boundary the hub explicitly established. On hub, the socket lives in the user's home directory with `0600` permissions. The message log lives in the worktree, owned by the worker. No tokens, no TLS, no PKI.

Threat model:

- **Devbox compromise** — attacker can forge `events.log` lines and tamper with their own message log. Both are strictly weaker than what the agent's identity already grants.
- **Network MITM** — SSH is the encryption + integrity layer.
- **Multi-tenant devbox** — scope to `$XDG_RUNTIME_DIR/shelbi.sock` if shared.
- **Multi-user hub** — `~/.shelbi/hub.sock` and worktrees are per-user.

### 12. What this does NOT solve

- **Hub → remote eventing for non-worker concerns.** Already covered by SSH command invocation. This plan only handles worker ↔ hub.
- **Multi-hub coordination.** One project, one hub. No leader election.
- **Cross-project events at the wire level.** The `project` field routes the message; no cross-project subscribe.
- **Real-time interrupts on codex workers.** Codex sees messages on its next poll, not immediately. Acceptable for clarification/directive use cases; would need different mechanism for true interrupts.

## Open questions

1. **Socket location on hub** — `~/.shelbi/hub.sock` (cross-platform) vs `$XDG_RUNTIME_DIR/shelbi.sock` (linux-native). Lean toward `~/.shelbi/hub.sock` with `SHELBI_HUB_SOCK` override.
2. **Reverse-forward lifetime** — does shelbi own the SSH ControlMaster or piggyback on the user's? Owning is cleaner; costs SSH-config logic.
3. **Daemon binary identity on launchd** — `co.32pixels.shelbi` vs `co.32pixels.shelbi.daemon`.
4. **Install-script daemon default** — opt-out (`--no-daemon`) or opt-in (`--with-daemon`)? Lean default-on with clear messaging.
5. **Message log retention** — truncate / rotate when? Per-task means it bounds with the task lifecycle; archive on `done`?
6. **Codex polling cadence** — every-step is conservative but high-latency. Worth letting the user tune?
7. **CLI for orchestrator message push** — extend `shelbi send` or new `shelbi message`? `shelbi send` semantics (tmux send-keys) are different; probably separate command.

## Phasing

1. **Phase 1 — `shelbi daemon` + local socket on hub.** Daemon listens, validates, appends to `events.log`. `shelbi event` writes to socket when `SHELBI_HUB_SOCK` is set; falls back to O_APPEND.
2. **Phase 2 — daemon supervision.** launchd plist + systemd unit. `shelbi daemon install/uninstall/status/restart`. `scripts/install.sh` integration.
3. **Phase 3 — hub workers use the socket.** Agent instructions updated; `shelbi open` sets `SHELBI_HUB_SOCK=~/.shelbi/hub.sock` in the agent env.
4. **Phase 4 — reverse forward in remote-pane SSH.** Extend hub's outbound SSH to remote workspaces with `-R`. Smoke test from devbox lands a line in hub's `events.log`.
5. **Phase 5 — remote workers use the socket.** `shelbi open` on remote sets `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock`. Agent instructions for remote workers updated.
6. **Phase 6 — message log + hub-side write API.** Define the messages format, `<worktree>/.shelbi/messages/<task-id>.log` convention, and the orchestrator's append primitive (`shelbi message` CLI or equivalent).
7. **Phase 7 — Claude Code hook integration.** SessionStart hook starts the background tail; Stop hook injects unread lines. Ship as part of the default `agents/<role>/settings.json` template.
8. **Phase 8 — Codex polling fallback.** Agent-instructed polling pattern documented + included in the codex agent template.
9. **Phase 9 — clarification verb end-to-end.** `request-clarification` over the socket → orchestrator UI surface → reply written to messages log. Closes the loop.

Phases 1–5 are the worker→hub channel. Phases 6–8 are the hub→worker channel. Phase 9 wires them together.