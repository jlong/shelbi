# Single-consumer event follower

Root-cause finding and the fix for the bug where a leaked or duplicate
`shelbi orchestrator events next --follow` process starved the orchestrator's
authoritative event drain, so a `mode=zen off -> on` toggle went unseen for
minutes even though the event was in the log the whole time.

## The symptom

The orchestrator observed a real event late (a Zen toggle, but any event class
is equally affected). The event was present in `~/.shelbi/events.log` at its
timestamp, and the durable cursor had not advanced past it, yet the
orchestrator's self-drain kept returning "no batch" for several minutes until a
visibility-timeout redelivery finally surfaced it.

## Root cause: two consuming readers race

The old operating model ran two `shelbi orchestrator events next --follow`
processes against one project:

1. a background follower as a "latency accelerator", and
2. the foreground `--max-lifetime <n>` self-drain as the authoritative catch-up.

Both are *consuming* readers of the same at-least-once delivery queue. A batch
that one follower has claimed is held in lock-step under a stable delivery id
until it is acked, so while it is in flight the other reader sees "no batch."
During that window the authoritative drain is blind and treats the absence as
"nothing happened."

The race was amplified by leaked processes. Only one orchestrator exists per
project, but at diagnosis time three consuming followers were live for one
project: the session's own follower plus two `--max-lifetime` followers left
behind by prior orchestrator restarts (reload and crash-recovery). Each leaked
consumer independently claims batches, so N live consuming followers make any
single reader miss roughly `(N-1)/N` of batches until the visibility timeout
redelivers them. A later diagnosis traced one respawn source to a second
`claude` process launched via a plugin path that ran the orchestrator
instructions and spawned its own `--max-lifetime 4h` follower, so the followers
were not merely leftover, they were actively respawning.

## The fix

Two independent changes, either of which alone narrows the failure, together
close it:

### 1. The accelerator is non-consuming

The documented background accelerator is now `shelbi events tail --follow
--project <you>`, a passive mirror of the event log. It never claims from the
delivery queue, so it can run alongside the authoritative drain forever without
competing. A consuming reader was always the wrong tool for a passive watch.
The authoritative drain remains the single consuming reader:
`shelbi orchestrator events next --follow --max-lifetime 2s`, run once per turn
with claim, react, ack.

The launch bootstrap prompt and the default orchestrator instructions were
updated to match, and `shelbi events tail` gained a `--project <name>` filter so
the accelerator can be scoped to one project.

### 2. One consuming follower per project, enforced

`shelbi orchestrator events next --follow` now claims single-consumer ownership
on start (`shelbi_state::claim_event_follower`). The claim, taken under a
per-project follower lock:

* records the starting process's PID in
  `~/.shelbi/projects/<name>/event-follower`, and
* signals any prior live owner to stop (SIGTERM, which the feed catches and
  turns into a clean `terminated` exit).

Every follower re-reads the recorded owner each tick; one whose PID no longer
matches stands down with a clean `feed=superseded` notice. Ownership is
last-writer-wins, so:

* an orchestrator restart tears down its prior follower rather than accumulating
  one (the new follower's claim supersedes the old), and
* a leaked or duplicate consuming follower cannot co-drain and starve the
  primary: whichever consumer started most recently owns the slot, and the
  others exit.

Because the durable cursor advances only on `events ack`, this takeover never
loses or replays an event: a superseded follower leaves any unacked batch
exactly where it was, and the surviving owner redelivers it verbatim.

## The cursor file was already atomic

The report also flagged a garbled `event-cursor` value (`2833840333762`, which
reads as `2833840` concatenated with `333762`). `write_event_cursor` writes
through `atomic_write`, which writes to a per-process temp file (`create`
truncates) and `rename`s it into place under the event-log lock, so a shorter
value fully replaces a longer one and concurrent writers can never splice their
bytes together. The garbled value could only have come from a non-atomic writer
that has since been retired. The
`write_event_cursor_atomicity_never_concatenates` regression test pins the
property.

## Reproducing the starvation, and the fix

The unit test
`single_owner_sees_every_event_in_a_burst_despite_a_second_follower`
(in `crates/shelbi-cli/src/commands/orchestrator.rs`) is the minimal repro:

1. A primary claims the single consumer slot for a project.
2. A burst of events is appended to the log while a second follower notionally
   co-runs.
3. The owner is never superseded, so its drain from the durable cursor observes
   the entire burst as one batch, not a fraction of it.
4. The second follower, holding any other PID, is reported superseded, so it can
   never claim a batch out from under the primary.

`feed_loop_stands_down_when_superseded_by_a_newer_follower` and the
`event_follower_*` tests in `crates/shelbi-state/src/event_log.rs` cover the
takeover, supersede-exit, and release-safety paths.

## Operational note

If you find stray `orchestrator events next --follow` processes for a project
(for example from a runner that predates this fix), they are now self-correcting:
the next `events next --follow` to start supersedes them, and they exit on their
next tick. The accelerator should be `shelbi events tail --follow`; never run a
second `orchestrator events next --follow` as a passive watch.
