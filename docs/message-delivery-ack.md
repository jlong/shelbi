# Message delivery and the ack path

Root-cause finding and the contract `shelbi message` now honors. Filed against
the "shelbi message reports success for messages that are never delivered" bug
(external `website`-project report, reproduced on `main`).

## The symptom

`shelbi message <task> <kind> <body>` used to print `✓ m-… → <task> (<kind>)`
and exit 0 as soon as it had appended the record to
`<worktree>/.shelbi/messages/<task-id>.log`. That `✓` read as "delivered" but
only meant "written to the log file." Whether the worker ever read it was
decided later and recorded only as `ack=timeout` in `~/.shelbi/events.log`,
never returned to the caller. On at least one project the ack never once
succeeded (12 messages, 12 `ack=timeout`, 0 acked), so the success signal was
wrong 100% of the time.

## How the channel is wired

1. `shelbi message` checks that something will read the message: the task isn't
   `done` and its worker session is alive. If not, it appends
   `ack=undeliverable reason=done|no_session` to `events.log` and fails. Nothing
   else happens: the message isn't written, no timer is armed, and the question
   it answers stays open.
2. Otherwise it appends one JSON line to the per-task log and writes `push=ok` to
   `events.log`. It also sends a best-effort `message-pushed` to the hub daemon,
   which arms an in-memory timer.
3. The worker's `SessionStart` hook (`.shelbi/hooks/session-start.sh`) leaves two
   files in `.shelbi/messages/` and starts nothing in the background:
   - `<task>.cursor` is the 1-indexed number of the next unread line, the same
     format the polling contract uses. It is seeded at the log's end only when
     missing, so a resume, clear or compact keeps its place.
   - `<task>.session` is the session's pid and start time. A reused pid has a
     different start time, so a beacon left by a dead session reads as dead and
     needs no cleanup.
4. The worker's `Stop` hook (`.shelbi/hooks/stop.sh`) fires at the end of a turn.
   It reads the complete lines past the cursor, injects them into the agent's
   context as a `<system-reminder>`, sends one `message-ack` per `msg_id` to the
   hub, then advances the cursor atomically. A hook killed before the advance
   delivers the batch again rather than losing it.
5. The daemon writes `ack=worker` when that ack arrives, or `ack=timeout` if its
   timer (default 60s) elapses first.

A polling runner (Codex and anything else without verified hooks) has no hooks to
write a beacon, so dispatch writes `polling` to `<task>.session` and the CLI goes
by whether the workspace's pane is alive. The agent reads the log and advances the
same cursor itself, as its prompt instructs.

## Root cause

This section records the original finding. A later fault in the same channel is
under "Messages lost after the first drain" below.

Two compounding faults, both on the worker side of step 3:

- **The ack was gated on `jq` and `nc` both being present.** The old `stop.sh`
  extracted `msg_id` with `jq` and sent the ack with `nc`, behind a
  `command -v jq && command -v nc` guard. `jq` is not installed on stock macOS
  or minimal Linux images. When it was missing the whole ack block was skipped
  silently: the message was still drained into the agent, so delivery *looked*
  fine, but no `message-ack` was ever sent. Because the daemon writes
  `ack=worker` unconditionally whenever an ack arrives (even after a prior
  `ack=timeout`), a pure timing race would still eventually produce an
  `ack=worker` line. Zero `ack=worker` lines across many messages therefore
  points at this gate, not at timing.

- **The ack is turn-coupled.** Even with the tooling present, the ack is only
  sent when the agent ends a turn (its `Stop` hook). A worker mid-task
  routinely runs a single turn far longer than the daemon's fixed 60s window,
  so `ack=timeout` is written first as a matter of course. The eventual
  `ack=worker` (when the turn ends) is real delivery, but the 60s "timeout" is
  a premature, misleading interim signal.

Hypotheses considered and eliminated: `tail -f > file` output buffering (macOS
BSD `tail` flushes each line promptly when following) and mis-wired settings
(the production `Stop` hook maps to both `pane-idle.sh` and `stop.sh`).

## Fixes

### Ack path (`crates/shelbi-orchestrator/src/workspace.rs`, `stop.sh`)

- Drop the `jq` dependency: `msg_id` is now extracted with `sed`, which is in
  every POSIX toolbox.
- Send the ack over `nc` if present, else `python3` (a more portable fallback
  than `nc` on many hosts). Only the hub address remains a hard precondition.
- Covered by `deployed_stop_hook_drains_and_acks_over_hub_socket`, which runs
  the deployed hook against a real Unix hub socket and asserts both the
  `<system-reminder>` drain and a `message-ack` per `msg_id`.

Delivery confirmation is still fundamentally asynchronous: it cannot land
before the worker's current turn ends. The CLI is therefore made honest rather
than pretending the push is the delivery.

### CLI contract (`crates/shelbi-cli/src/commands/message.rs`)

- The bare push no longer prints `✓`. It prints `queued … — NOT yet confirmed
  delivered` and points at the two ways to learn the real outcome.
- `--wait[=SECS]` blocks (polling `events.log`) until an `ack=worker` lands
  (exit 0) or the window elapses (exit non-zero). A mid-wait `ack=timeout` is
  not treated as terminal, since a late `ack=worker` still upgrades it.
- `shelbi message status <msg-id>` reports `delivered` / `queued` /
  `unconfirmed` / `undeliverable` / unknown from the durable events stream, exiting 0 only when
  the worker acked.
- A message to a `done` task or with no live worker session is reported
  `UNDELIVERABLE`, recorded as `ack=undeliverable`, and not written to the log.

### Messages lost after the first drain

`SessionStart` used to run `tail -f <task>.log > <task>.unread.log`, and `Stop`
drained by renaming `unread.log` away, creating a new one and deleting the old
one. The tail's stdout stayed bound to the original file, so from the first drain
on it wrote into a deleted file. Its pid was still alive, so the CLI reported
`queued` and the message ended in `ack=timeout`. The cursor removes the relay, the
tail, its watchdog and the dispatch sweep that cleaned up after them.

Sessions already running when this shipped keep their old hooks until their next
dispatch. They have no beacon, so the CLI's probe counts a live `tail.d` pid as a
live session for them.

## Delivery states

| State | Meaning | Exit (`status` / `--wait`) |
| --- | --- | --- |
| delivered | `ack=worker` seen — the worker read it into context | 0 |
| queued | `push=ok`, no ack yet — durable, unconfirmed | non-zero |
| unconfirmed | `ack=timeout` and no `ack=worker`: window elapsed, may still land at the next turn end, including after a resume | non-zero |
| undeliverable | `ack=undeliverable`: refused at send time because the task was `done` or no worker session was alive; never written to the log | non-zero |
