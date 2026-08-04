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

1. `shelbi message` appends one JSON line to the per-task log and writes
   `push=ok` to `events.log`. It also sends a best-effort `message-pushed` to
   the hub daemon, which arms an in-memory timer.
2. The worker's `SessionStart` hook (`.shelbi/hooks/session-start.sh`) starts
   `tail -f -n 0 <task>.log > <task>.unread.log`. The tail only appends new
   lines to `unread.log`; it never acks.
3. The worker's `Stop` hook (`.shelbi/hooks/stop.sh`) fires at the end of a
   turn. If `unread.log` is non-empty it drains it into the agent's context as
   a `<system-reminder>` and sends one `message-ack` per `msg_id` to the hub.
4. The daemon writes `ack=worker` when that ack arrives, or `ack=timeout` if
   its timer (default 60s) elapses first.

## Root cause

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
  `unconfirmed` / unknown from the durable events stream, exiting 0 only when
  the worker acked.
- A message to a `done` task is reported `UNDELIVERABLE` (the workspace has
  moved to a different task and will not read this task's log again) rather than
  claiming a future `SessionStart` pickup. No live tail is reported as queued
  but not delivered.

## Delivery states

| State | Meaning | Exit (`status` / `--wait`) |
| --- | --- | --- |
| delivered | `ack=worker` seen — the worker read it into context | 0 |
| queued | `push=ok`, no ack yet — durable, unconfirmed | non-zero |
| unconfirmed | `ack=timeout` and no `ack=worker` — window elapsed, may still land | non-zero |
| undeliverable | task `done`, or no live reader | non-zero |
