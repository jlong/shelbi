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
   acks each read via socket
   (hub writes directly on hub;
    via ssh redirect on remote)
```

Two channels:

- **Worker → Hub**: JSON lines into a Unix socket. `shelbi daemon` validates and appends to `events.log`.
- **Hub → Worker**: append-only `.shelbi/messages/$TASK_ID.log` file in the worktree. Worker tails it; orchestrator appends to it. Worker acks each read via the socket channel so the orchestrator knows whether a message landed.

Both channels are file/socket primitives — no agent-specific protocol, no install on workers.

### 2. Wire protocol — JSON lines on a Unix socket (worker → hub)

One message per line, newline-terminated:

```json
{"verb":"event","project":"shelbi","line":"workspace=delta pane_alive=false reason=signal:SIGHUP"}
{"verb":"request-clarification","project":"shelbi","task_id":"feat-X","question_id":"q-001","question":"...","context":"..."}
{"verb":"message-ack","project":"shelbi","task_id":"feat-X","msg_id":"m-042"}
```

Verbs: `event` (Phase 1), `request-clarification` and `message-ack` (Phase 9). Future without breaking compat: `task-claim`, `request-handoff`, `request-merge`.

Any tool that can write bytes to a Unix-domain socket works:

```sh
echo '{"verb":"event",...}' | nc -U $SHELBI_HUB_SOCK
echo '{"verb":"event",...}' | socat - UNIX-CONNECT:$SHELBI_HUB_SOCK
python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect("'$SHELBI_HUB_SOCK'"); s.sendall(sys.stdin.buffer.read())' < msg.json
```

Agent picks whichever tool is on PATH. Agent instructions list the preferences in order.

### 3. Hub workers

Hub workers set `SHELBI_HUB_SOCK=~/.shelbi/hub.sock` in their agent env. They write to the local socket using the same one-liner as remote workers. No shelbi CLI involved.

**Fallback when the daemon is down**: hub workers can `O_APPEND`-write to `events.log` directly (POSIX guarantees atomicity ≤ `PIPE_BUF`). Agent instructions name this as the fallback. Loss of the single-writer property is accepted in degraded mode. The daemon is a **soft dependency** for hub.

### 4. Remote workers

Remote workers set `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock` (the reverse-forward landing point). Same one-liner pattern. **No shelbi binary, no wrapper script, no install.** When the socket is unreachable the write fails. There is no remote-side outbox. See §7 for loss handling.

### 5. Hub-side daemon (`shelbi daemon`)

A subcommand of the main `shelbi` binary — one install path. Listens on `~/.shelbi/hub.sock`. For each newline-delimited JSON message, validates and dispatches:

- `event` → append to project's `events.log`
- `request-clarification` → surface to orchestrator + emit board-visible event
- `message-ack` → mark the referenced message as delivered (see §9, §13)

One daemon per user, not per project — projects multiplex through the `project` field.

**Stateless between restarts.** `events.log` is the durable record; in-flight messages live in the SSH connection's kernel socket buffer. Crash + restart resumes accepting messages.

### 6. SSH reverse forward and daemon supervision

#### Reverse forward

Hub already maintains a long-lived outbound SSH connection per remote host. Extend it with a reverse Unix-domain socket forward:

```
ssh -o ControlMaster=auto -o ControlPath=~/.shelbi/ssh/%r@%h \
    -R /tmp/shelbi-hub.sock:$HOME/.shelbi/hub.sock \
    devbox
```

Reuses existing SSH trust, zero new public listeners.

#### Daemon supervision — OS-native user services

- **macOS**: `~/Library/LaunchAgents/co.32pixels.shelbi.plist` with `KeepAlive: true`, `RunAtLoad: true`.
- **Linux**: `~/.config/systemd/user/shelbi.service` with `Restart=always`, `RestartSec=1s`. Headless boxes need `loginctl enable-linger <user>` — install script prints the instruction.

`scripts/install.sh` writes the platform-appropriate unit file. `--no-daemon` for opt-out.

Runtime management:

- `shelbi daemon install` / `uninstall` / `status` / `restart`
- `shelbi daemon` (no subcommand) — foreground entry point.

### 7. Loss handling — best-effort + hub-side detection

Remote workers have no outbox. The worker→hub channel is **best-effort**, with three safety nets:

1. **SSH-drop detection is hub-side.** The hub's outbound SSH ControlMaster is monitored; when it drops, the orchestrator marks the workspace unreachable, which is functionally `pane_alive=false`. The most important event is covered even if the worker's own emit never lands.
2. **Agent-instructed retry.** Agent prompt: *"if your socket write fails, retry once after 500ms; if it still fails, continue your work — the orchestrator will reconcile from hub-side state."*
3. **Stale-ControlMaster cleanup on hub start.** When the hub process boots, kill any existing shelbi-owned ControlMaster sockets in `~/.shelbi/ssh/` and re-establish — otherwise remote panes hold a `/tmp/shelbi-hub.sock` that points at a dead socket from the previous run. The cleanup is in shelbi's startup path, gated by a PID-file freshness check so we don't kill *running* shelbi's CMs.

### 8. Worker-initiated clarification

Workers ask via the socket; replies come back via the messages log (§9). Wire-level:

```json
{"verb":"request-clarification","project":"shelbi","task_id":"feat-X","question_id":"q-001","question":"<text>","context":"<excerpt>"}
```

The `question_id` is opaque to the orchestrator but echoed back in the reply's `in_response_to` field — lets the worker correlate multiple in-flight questions.

Agent-instructions paragraph (`agents/developer/instructions.md`):

> When facing ambiguity in a task spec, prefer asking over guessing. Send a clarification request by writing this JSON to `$SHELBI_HUB_SOCK`:
>
>     {"verb":"request-clarification","project":"<project>","task_id":"<your-task-id>","question_id":"<short-unique-id>","question":"<concise question>","context":"<short excerpt>"}
>
> The orchestrator will surface your question to the user and append the reply to your message log. You'll see it via the always-on tail.
>
> **Important safety net**: if you've been waiting more than 5 minutes without a reply, fall back to manually checking `.shelbi/messages/<task-id>.log` yourself — push delivery may have failed silently.
>
> Before emitting a review marker, summarize the key decisions you made in the commit message — helps the orchestrator catch silent wrong-assumption cases at review time.

### 9. Hub → Worker messaging — append-only per-task log

Each task has a message log at `<worktree>/.shelbi/messages/<task-id>.log`. One JSON message per line. The orchestrator appends; the worker reads. Survives worker restarts.

Message format (every message has a `msg_id` for ack correlation):

```json
{"msg_id":"m-042","ts":"2026-06-30T01:55:00Z","kind":"reply","in_response_to":"q-001","body":"<reply text>"}
{"msg_id":"m-043","ts":"2026-06-30T02:10:00Z","kind":"directive","body":"stop what you're doing — the spec changed; re-read the task file before continuing"}
{"msg_id":"m-044","ts":"2026-06-30T02:30:00Z","kind":"context","body":"<additional context>"}
```

Kinds: `reply` (response to a question), `directive` (stop / change course / read X), `context` (background info). Extensible.

**Worker acks each read.** After the worker reads a message, it emits over the socket:

```json
{"verb":"message-ack","project":"shelbi","task_id":"feat-X","msg_id":"m-042"}
```

The daemon writes an `event` line `message=m-042 task=feat-X ack=worker` so the orchestrator can see delivery in the events stream. Unacked messages older than a configurable threshold (default 60s) surface in the orchestrator as a warning.

Hub-side write path:

- Hub workers: `echo '<json>' >> <worktree>/.shelbi/messages/<task-id>.log`
- Remote workers: `ssh <host> 'cat >> <worktree>/.shelbi/messages/<task-id>.log' <<<'<json>'`

Single primitive — file append. Orchestrator CLI: `shelbi message <task-id> <kind> "<body>"` (or `--in-response-to <question-id>` for replies).

Worker read path is agent-specific — see §10.

### 10. Per-agent message delivery — push for claude, pull for codex

The messages log is universal. **How the agent notices a new message** is agent-specific.

#### Claude Code — auto-injection via hooks

Two hooks in `agents/<role>/settings.json`:

**`SessionStart`** — kills any stale tail, then starts a fresh background tail with a lock file:

```sh
LOCKDIR=.shelbi/messages/$TASK_ID.tail.d
mkdir -p .shelbi/messages
if [ -f "$LOCKDIR/pid" ]; then
  kill "$(cat $LOCKDIR/pid)" 2>/dev/null || true
  rm -rf "$LOCKDIR"
fi
mkdir -p "$LOCKDIR"
touch .shelbi/messages/$TASK_ID.log
tail -f -n 0 .shelbi/messages/$TASK_ID.log > .shelbi/messages/$TASK_ID.unread.log &
echo $! > "$LOCKDIR/pid"
```

The lock+kill prevents duplicate tails when a session restarts. Touching the log first ensures `tail -f` doesn't fail before the orchestrator's first write.

**`Stop`** (or `PostToolUse`) — atomic-rename pattern, no race with concurrent appends:

```sh
UNREAD=.shelbi/messages/$TASK_ID.unread.log
PROC=$UNREAD.processing
if [ -s "$UNREAD" ]; then
  mv "$UNREAD" "$PROC"          # atomic; new writes go to a fresh unread.log
  touch "$UNREAD"               # re-create empty for tail to keep writing
  echo "<system-reminder>New orchestrator messages:"
  cat "$PROC"
  echo "</system-reminder>"
  # ack each message back over the socket
  jq -c '.msg_id' "$PROC" | while read MSG_ID; do
    echo '{"verb":"message-ack","project":"'$PROJECT'","task_id":"'$TASK_ID'","msg_id":'$MSG_ID'}' \
      | nc -U "$SHELBI_HUB_SOCK"
  done
  rm "$PROC"
fi
```

The `mv` then `touch` pattern ensures a message written between read and clear ends up in the *new* unread.log, not lost.

#### Codex / future agents — pull-style polling

Codex's task prompt includes:

> Your message log is at `.shelbi/messages/<your-task-id>.log`. **After every Bash, Edit, or Read tool call**, check it with:
>
>     CURSOR=$(cat .shelbi/messages/<task-id>.cursor 2>/dev/null || echo 1)
>     tail -n +$CURSOR .shelbi/messages/<task-id>.log
>     wc -l < .shelbi/messages/<task-id>.log > .shelbi/messages/<task-id>.cursor
>
> Act on any messages you find before continuing. For each new message, ack via:
>
>     echo '{"verb":"message-ack","project":"<project>","task_id":"<task-id>","msg_id":"<msg-id>"}' | nc -U $SHELBI_HUB_SOCK

Tying the poll to specific concrete tool calls (Bash, Edit, Read) avoids the "the agent ran one giant tool call and never polled" failure mode.

#### Future agents

The contract: "agent must observe new lines in `.shelbi/messages/<task-id>.log` and ack each via socket." If the agent supports hooks, use push. Otherwise, instruct it to poll on a concrete trigger.

### 11. Authentication

None beyond SSH and Unix-socket / filesystem file permissions. The reverse forward is created by hub's outbound SSH session; anything that can write to `/tmp/shelbi-hub.sock` on devbox is already inside the trust boundary. Hub socket is `0600` in user home. Message log lives in the worktree, owned by the worker.

Threat model:

- **Devbox compromise** — attacker can forge `events.log` lines and tamper with their own message log. Both strictly weaker than what the agent's identity already grants.
- **Network MITM** — SSH is the encryption + integrity layer.
- **Multi-tenant devbox** — scope to `$XDG_RUNTIME_DIR/shelbi.sock` if shared.
- **Multi-user hub** — socket and worktrees are per-user.

### 12. What this does NOT solve

- **Hub → remote eventing for non-worker concerns.** Already covered by SSH command invocation.
- **Multi-hub coordination.** One project, one hub.
- **Cross-project events at the wire level.** Per-message `project` field; no cross-project subscribe.
- **Real-time interrupts on codex workers.** Codex sees messages on its next poll. Acceptable for clarification/directive use cases.

### 13. Robustness — known weak spots and mitigations

Honest catalogue of where the design is fragile, baked-in mitigations, and accepted residual risk.

| Weak spot | Where it surfaces | Mitigation in the design |
|---|---|---|
| Worker dies / hangs without reading message | Orchestrator pushes a directive; worker never processes it | Worker acks each read via socket (§9). Unacked > 60s surfaces in the orchestrator as a warning. |
| Claude Code hook misconfigured / disabled | Push delivery silently fails | Agent prompt (§8) includes "if no reply in 5 min, manually tail the log." Pull path always works. |
| Tail process duplication on session restart | Two tails write to the same unread.log → duplicate injections | SessionStart hook (§10) kills any prior tail by PID lock file before spawning. |
| Race between Stop hook clearing unread.log and orchestrator append | Message written during the clear window is lost | Atomic `mv unread.log unread.processing` then `touch unread.log` — new writes go to the fresh file (§10). |
| Stale SSH ControlMaster after hub restart | Remote panes hold a `/tmp/shelbi-hub.sock` pointing at a dead socket | Hub startup kills stale shelbi-owned CMs in `~/.shelbi/ssh/` and re-establishes (§7). |
| Codex polls in giant tool call | Worker never checks the message log mid-step | Prompt ties polling to concrete triggers (after every Bash/Edit/Read), not the vague "between significant steps" (§10). |
| Daemon throughput under fan-out | Many remote workers fill kernel socket buffer; workers block on write | Daemon does JSON parse + file append only — no synchronous IO bottleneck. Realistic risk only with dozens of workers; today's pool is 3. Worth revisiting if scale grows. |
| Best-effort remote events lost in seconds before SSH drop | Pre-drop chatter never lands | Accepted residual risk. Pane_alive specifically is recovered hub-side via SSH-drop detection (§7); other events are low-stakes. |
| Old "stop" directive read by worker minutes later | Time-sensitive directive no longer applies | Messages include `ts`. Agent prompt: "treat directives older than 5 min as informational, not as live interrupts." |
| Hook output is trusted as a system reminder | If attacker writes to unread.log, they inject prompts | Same trust boundary as the worktree filesystem — attacker with filesystem write already has agent-level execution. No new attack surface. |
| Message log location not derivable for arbitrary host | Orchestrator needs `<worktree>` path for remote write | Orchestrator already knows each workspace's worktree (it spawned them). Resolution lives in the workspace state. |

**Accepted residual risk**: best-effort delivery of low-stakes events in the seconds-before-SSH-drop window. Everything else has a mitigation in the design.

**Observability**: every push, ack, and ack-timeout produces an event in `events.log`, so the user can audit the conversation channel from the same tail they already watch.

## Open questions

1. **Socket location on hub** — `~/.shelbi/hub.sock` vs `$XDG_RUNTIME_DIR/shelbi.sock`. Lean toward `~/.shelbi/hub.sock` with override env.
2. **Reverse-forward lifetime** — does shelbi own the SSH ControlMaster or piggyback? Owning is cleaner; costs SSH-config logic.
3. **Daemon binary identity on launchd** — `co.32pixels.shelbi` vs `co.32pixels.shelbi.daemon`.
4. **Install-script daemon default** — opt-out (`--no-daemon`) or opt-in (`--with-daemon`)? Lean default-on.
5. **Message log retention** — truncate / rotate when? Archive on task `done`?
6. **Ack-timeout threshold** — 60s is a guess. Should it be configurable per project or per message kind (replies stricter than context)?
7. **CLI for orchestrator message push** — new `shelbi message` command, or extend something existing? Likely new — `shelbi send` semantics (tmux send-keys) are different.

## Phasing

1. **Phase 1 — `shelbi daemon` + local socket on hub.** Daemon listens, validates, appends `events.log`. Socket write with O_APPEND fallback.
2. **Phase 2 — daemon supervision.** launchd plist + systemd unit. `shelbi daemon install/uninstall/status/restart`. `scripts/install.sh` integration.
3. **Phase 3 — hub workers use the socket.** Agent instructions updated; `shelbi open` sets `SHELBI_HUB_SOCK=~/.shelbi/hub.sock`.
4. **Phase 4 — reverse forward in remote-pane SSH.** Extend hub's outbound SSH with `-R`. Stale-CM cleanup on hub start.
5. **Phase 5 — remote workers use the socket.** `shelbi open` on remote sets `SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock`. Agent instructions for remote workers.
6. **Phase 6 — message log + hub-side write API.** Define format, `<worktree>/.shelbi/messages/<task-id>.log` convention, `shelbi message` CLI primitive.
7. **Phase 7 — Claude Code hook integration.** SessionStart + Stop hooks with lock-file tail management and atomic-rename unread cursor. Default `agents/<role>/settings.json` template.
8. **Phase 8 — Codex polling fallback.** Concrete-trigger polling pattern in the codex agent template.
9. **Phase 9 — clarification verb + ack loop end-to-end.** `request-clarification` over socket → orchestrator surface → reply to messages log → worker reads → ack over socket → orchestrator sees delivery in events stream. Unacked-timeout warning.

Phases 1–5 are the worker→hub channel. Phases 6–8 are the hub→worker channel. Phase 9 wires them together with the ack loop that closes the robustness gaps.