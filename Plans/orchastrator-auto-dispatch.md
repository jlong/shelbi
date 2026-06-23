# Orchastrator Auto-Dispatch

## Context

Today the orchestrator (the Claude pane in `shelbi-shelbi:dashboard`) is reactive: the user moves tasks through the Kanban board manually, and explicitly asks the orchestrator to start a worker on each one. The orchestrator's prompt template even says *"Don't start tasks without explicit user direction."* That's why, after a round of work, the same ceremony repeats: "promote to todo and start a worker" four or five times in a row.

The user wants the orchestrator to *be* the scheduler — actively keeping workers busy without being asked. The dispatch decision belongs in the orchestrator's reasoning loop (so it can apply judgment: routing, priority, retries, holding off when something looks wrong), not in deterministic Rust code. To do that the orchestrator needs a live view of the board.

Shelbi already has the plumbing for a live view: `~/.shelbi/events.log` is an append-only transition log, written by the poller for worker state changes, and consumed by `shelbi events tail --follow`. Today it only carries worker events. The minimal change is to extend the same log to also carry **task** column transitions, then point the orchestrator at it as its live feed. The initial snapshot comes from `shelbi task list` and `shelbi worker list` — both already exist. No new CLI subcommand.

## Design

### 1. Extend `events.log` to include task transitions

Today the log carries lines like `<rfc3339> worker=alpha working -> awaiting_input`, written from `crates/shelbi-tui/src/poller.rs:163-173` via `shelbi_state::append_worker_event`. Add a sibling:

```rust
// crates/shelbi-state/src/lib.rs
pub fn append_task_event(
    task_id: &str,
    from: Column,
    to: Column,
    reason: &str,
) -> Result<()>;
```

Disk format (same file): `<rfc3339> task=<id> <from> -> <to> reason=<short>`. `shelbi events tail` streams everything in the file in arrival order; consumers distinguish line kinds by the `worker=` vs `task=` prefix.

Call from every site that mutates a task's column:

- `crates/shelbi-cli/src/commands/task.rs::move_to` (line ~330) — reason `user:cli`

- `crates/shelbi-cli/src/commands/task.rs::start` (line ~396, writes InProgress) — reason `user:cli:start` (or `orchestrator:auto-dispatch worker=<name>` when the orchestrator runs it)

- `crates/shelbi-cli/src/commands/worker.rs::stop` (line ~91, releases tasks back to Todo) — reason `worker:stop`

- `crates/shelbi-tui/src/kanban.rs::move_card` (line ~244) — reason `user:tui`

- `crates/shelbi-tui/src/poller.rs::maybe_promote_to_review` (line ~213) — reason `worker:review-marker`

The reason string is short, structured-ish, and human-readable. The orchestrator parses by prefix.

### 2. New task field `prefers_machine`

Add to `Task` at `crates/shelbi-core/src/model.rs:309-326`:

```rust
# [serde(default, skip_serializing_if = "Option::is_none")]
pub prefers_machine: Option<String>,
```

`shelbi task add` gains an optional `--prefers-machine <name>` flag. The orchestrator reads this field via `shelbi task list` / `task show` and honors it when picking workers. Enforcement is in the prompt, not in code.

### 3. Orchestrator setup: snapshot + tail

The orchestrator's three information sources at session start:

- **Initial task snapshot** — `shelbi task list` (already exists). Gives all columns + priorities + assigned\_to.

- **Initial worker snapshot** — `shelbi worker list` (already exists). Gives pane-alive + idle/working + current task.

- **Live transitions** — `shelbi events tail --follow` (already exists). Run as a background process via the Bash tool's `run_in_background: true`, then watched line-by-line with the `Monitor` tool. Each emitted line is a notification the orchestrator can react to.

No new CLI subcommand. The single change to existing CLIs is that `events.log` now carries task lines, so `shelbi events tail` will surface both worker and task transitions.

### 4. Orchestrator prompt — this is where the dispatch logic lives

Rewrite the relevant section of `crates/shelbi-orchestrator/src/default_orchestrator.md.template` and mirror into `/Users/jlong/.shelbi/projects/shelbi/CLAUDE.md`.

Sections to add:

**Auto-dispatch contract** (replaces the "don't start tasks without explicit direction" rule):

> Moving a task into `todo` is the start signal — your job is to assign it to a free worker and launch them. When a worker finishes (the poller moves their task to `review`), give them the next ready task immediately. The user is the priority-setter and reviewer; you are the scheduler.

**Bootstrap on session start:**

> First reply of the session — or after a reload — do this:
>
> 1. `shelbi task list` to get the board snapshot.
> 2. `shelbi worker list` to get the worker pool snapshot.
> 3. Start `shelbi events tail --follow` in the background. Watch it with `Monitor`. Each line is your trigger.
>
> If the tail dies (Monitor reports the task ended), restart it.

**Reaction rules** (one bullet per event prefix):

- `task=<id> backlog -> todo reason=user:*` → scan free workers. Honor `prefers_machine` if set on the task. Run `shelbi task start <id> --worker <name>`. If no eligible free worker, leave it in todo; mention it in your next reply so the user knows.

- `task=<id> in_progress -> review reason=worker:review-marker` → the assigned worker is now free. Look up the next ready task (`shelbi task list --ready` or scan from your snapshot), dispatch as above.

- `worker=<name> working -> awaiting_input` or `idle` → same as above: worker just became free, find them work.

- `worker=<name> pane_alive=false` (or any pane-death indicator) → don't auto-restart. Surface to the user in the next reply.

**Free-worker selection:**

> A worker is free when no task in `in_progress` is assigned to it (parse from snapshot, update from events). Pick free workers in the order they're declared in the project YAML. If a task has `prefers_machine`, only consider workers on that machine.

**When NOT to dispatch:**

> - If a previous dispatch for this task failed within the last minute, pause it and ask the user.
>
> - If you see two `task -> todo` events for the same task within seconds, treat as deduplication (the user is probably correcting a misclick); only dispatch once.
>
> - If the user has spoken to you in the last 30 seconds, finish answering them before reacting to events.

**Reporting:**

> Lead each user-facing reply with a one-line activity summary if anything changed since your last reply. Example: `alpha → palette task, bravo finished worker-auto → review, charlie idle no ready tasks`. Don't repeat unchanged status.

### 5. Files to touch

- `crates/shelbi-core/src/model.rs` — `prefers_machine: Option<String>` on `Task`, round-trip test.

- `crates/shelbi-state/src/lib.rs` — `append_task_event`.

- `crates/shelbi-cli/src/commands/task.rs` — `append_task_event` calls on `move_to` + `start`; optional `--prefers-machine` flag on `add`; `--reason` flag pass-through on `move_to` so the orchestrator can tag its dispatches as `orchestrator:auto-dispatch`.

- `crates/shelbi-cli/src/commands/worker.rs` — `append_task_event` calls in `stop`.

- `crates/shelbi-tui/src/kanban.rs` — `append_task_event` calls in `move_card`.

- `crates/shelbi-tui/src/poller.rs` — `append_task_event` calls in `maybe_promote_to_review`.

- `crates/shelbi-orchestrator/src/default_orchestrator.md.template` — rewrite the orchestrator contract per section 4.

- `/Users/jlong/.shelbi/projects/shelbi/CLAUDE.md` — mirror the new orchestrator contract so the project's orchestrator picks it up on reload.

Nothing in `shelbi-orchestrator` Rust code changes for dispatch — the orchestrator is the Claude pane and its instructions live in the prompt template.

## Verification

**Unit-test slice:**

- `append_task_event` writes parseable lines; concurrent appends from CLI + poller don't tear (test the lock or atomic-append semantics that `append_worker_event` already uses).

- `Task` with `prefers_machine: foo` round-trips through YAML correctly, and absent field defaults to `None`.

**Manual end-to-end:**

1. Reload sidebar (`shelbi reload`) so the orchestrator's new prompt loads.
2. In the orchestrator pane, type `start`. The orchestrator should run the three bootstrap commands (`task list`, `worker list`, `events tail --follow` in background), then say "scheduler ready, N tasks in todo, M workers free."
3. From the Kanban TUI, create three backlog tasks. Set `prefers_machine: devbox` on one (hand-edit the frontmatter for MVP).
4. Promote all three to todo via the TUI. Within a few seconds the orchestrator should:

   - Notice three `task=... backlog -> todo` events on its stream.

   - Run `shelbi task start` for the first two (one on a hub worker, one on devbox to honor the preference).

   - Leave the third in todo (no free worker) and surface a status line on its next reply.
5. When the devbox worker finishes (touches its review marker), the orchestrator should see `task=... in_progress -> review` followed by `worker=... -> awaiting_input` and dispatch the third task.
6. `tail -f ~/.shelbi/events.log` should show clear `task=` and `worker=` lines interleaved, with `reason=orchestrator:auto-dispatch worker=<name>` distinguishing the orchestrator's own moves.
7. Manually move a task backlog → in\_progress (skip todo) via the TUI. The orchestrator should ignore it — manual-override path stays open.
8. Kill the `shelbi events tail` background process. On its next turn, the orchestrator should notice (via Monitor) and restart it.

**Regression smell-test:**

- `shelbi task start <id> --worker <name>` (the explicit manual path) still works unchanged.

- `shelbi events tail` (without `--follow`) prints both worker and task lines in chronological order.

## MVP cut

If a smaller first commit is needed: ship `append_task_event` + the prompt rewrite without `prefers_machine`. The orchestrator just walks workers in declaration order; devbox-only tasks get manually routed during the gap. Add `prefers_machine` in the next commit.
