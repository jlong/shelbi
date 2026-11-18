# Orchestrator awareness system (v2)

Status: approved 2026-11-10; filed as umbrella `awareness-v2` + subtasks. Requested by John: replace the periodic heartbeat + Monitor feed with event-driven notifications, behind a settings toggle so the old system stays available.

## Problem

The orchestrator learns about in-flight work from a periodic heartbeat (`poller.rs` `maybe_emit_heartbeat`, 60s standard, backs off to 5m when the board is quiet) streamed through `shelbi events tail --follow` under Claude Code's Monitor tool, plus a consuming drain (`shelbi orchestrator events next/ack`).

In practice this is noise:

- A quiet board still produces a heartbeat every 5 minutes, each one a conversation turn ("drain, ack, nothing to do").
- The Monitor tool expires every 30 minutes and must be re-armed. Each re-arm replays the recent tail into the conversation.
- The heartbeat exists only so the orchestrator can notice work it is waiting on (missed review markers, missed dispatches, CI, Zen backlog). It is a polling substitute for notifications Shelbi could send directly.

## Goal

Notify the orchestrator only when something it cares about happens:

1. **Reminders.** The orchestrator sets "remind me to do X in 1 hour". When the time passes, it gets a notification.
2. **Job completion.** The orchestrator registers a job (a background command, a CI run, a PR's checks) and is notified when it finishes, with the result.
3. **Estimates and overdue alerts.** When assigning work, the orchestrator records an estimate. If the task is still active past the estimate, it gets an "overdue" notification.
4. **Heartbeats only on change.** In v2 a heartbeat (the board digest: zen_eligible, idle_workspaces, Zen cue) is sent only alongside a board transition or a notification, never on a timer.

All of this sits behind a setting with two choices:

- **Simple heartbeat system (v1).** Today's behavior, unchanged.
- **Awareness system (v2).**

## Design

### 1. The awareness store (daemon-owned)

A new durable store at `~/.shelbi/projects/<project>/awareness.json`. Shelbi state never lives under `.claude/`. The poller evaluates it on its existing 5s supervisor tick and owns all timing, so reminders survive orchestrator restarts, reloads and runner switches. Claude's CronCreate and Codex automations are session-scoped or desktop-only, so neither is a fit.

Entries:

- `reminder { id, due_at, text, created_by }`. One-shot. Optional `--every` for recurring, capped.
- `job { id, kind, target, started_at, label }`, where `kind` is one of:
  - `command`: Shelbi runs the command detached under the daemon and records exit code + output tail.
  - `pid`: an existing process.
  - `ci`: a commit's CI run, reusing `zen::scoped_ci_poll`.
  - `pr`: a PR's checks / merge state.
- `estimate { task_id, estimate, started_at, overdue_at, alerted }`. Created on dispatch. Cleared when the task leaves the `active` category.

When an entry fires, the poller writes one event line to `events.log` and removes or advances the entry.

### 2. New event kinds

```
<ts> project=P reminder id=<id> text="check the tap PR merged"
<ts> project=P job id=<id> kind=command label="cargo test" status=ok|failed exit=<n> detail=<tail>
<ts> project=P task=<id> overdue estimate=45m elapsed=1h10m workspace=<ws>
```

These get kinds `reminder`, `job_done` and `task_overdue` in `event_kind` (`commands/orchestrator.rs:1113`) and in the Codex `wake.rs` priority table. Overdue alerts at the estimate, then repeats every half-estimate (floor 10m) until the task leaves active.

### 3. CLI for the orchestrator

```
shelbi remind in 1h "check the release tap PR merged"
shelbi remind at 15:00 "..."     shelbi remind list     shelbi remind cancel <id>
shelbi job run --label "desktop tests" -- cargo test     # detached, notify on exit
shelbi job watch --pid <pid> | --ci <sha> | --pr <n>
shelbi job list / cancel <id>
shelbi issue start <id> --workspace <ws> --estimate 45m
shelbi issue estimate <id> 2h                             # set or revise
```

`issue start` without `--estimate` falls back to a per-workflow default (for example `app: 90m`). With no default, there is no overdue alert.

### 4. Heartbeats in v2

The periodic emitter is off. The board digest (`zen_eligible`, `idle_workspaces`, disk, Zen cue) is attached to the delivery batch that carries a transition or a notification, so it arrives only when something happened.

What the old heartbeat sweep did, and where it goes in v2:

- **Missed review markers:** a task overdue against its estimate triggers the same four-condition check. Separately, the poller already sees the marker file directly.
- **Missed dispatches:** every `idle` / `handoff` transition already triggers dispatch. The digest on each transition carries `idle_workspaces` and `zen_eligible`, so nothing is lost.
- **Zen policy re-injection (summary every 3rd heartbeat, re-read hourly):** moves to the delivery digest (every Nth delivered batch) and to the SessionStart / UserPromptSubmit hook context.
- **Liveness ("silent tail means the follower died"):** with push delivery there is no tail to die. Shelbi tracks channel or bridge connection state and surfaces a disconnect in the sidebar instead.

### 5. Delivery to the orchestrator

**Claude Code: use the Shelbi system plugin we already ship.** We already stage it and pass it with `--plugin-dir` (`system_plugin.rs`, `with_orchestrator_plugin_dir`). It gains three pieces, in order of preference:

1. **Channel (preferred, true push).** An MCP server (`shelbi awareness channel`, stdio) declares `claude/channel`. It subscribes to the hub's delivery queue (claim, then ack after emit) and pushes each v2 batch as a `notifications/claude/channel` event. This is the only documented way to push into a running session, and the docs' webhook example implies it wakes an idle session.
   - Requires launching with `--dangerously-load-development-channels`, because custom channels aren't on the allowlist. That shows a warning dialog at launch.
   - Needs claude.ai or API-key auth. Team/Enterprise orgs must have `channelsEnabled`.
   - Research preview.
2. **Plugin monitor (fallback).** `monitors/monitors.json` runs `shelbi orchestrator events next --follow --kinds v2`. It auto-starts with the session and needs no re-arm by the agent. Docs don't say whether plugin monitors have the 30-minute deadline (verify). It also wakes the session, since each line is a notification.
3. **Hooks (always on, both modes).**
   - UserPromptSubmit: drains pending v2 events into `additionalContext`, so the orchestrator never answers the user with stale state. This replaces the "drain before every reply" instruction.
   - Stop: guarded by `stop_hook_active`, blocks stopping only if an urgent item (reminder, job_done, overdue) arrived mid-turn.

**Codex: reuse the bridge we already have.** `wake.rs` already delivers queued events into the Codex thread with `turn/start` (idle) and `turn/steer` (mid-turn). For v2:

- Drop the `HeartbeatKeepAlive` / `Heartbeat` priorities.
- Add the three new kinds.
- Use `thread/inject_items` for the low-priority digest so it doesn't start a turn.

Codex hooks (SessionStart / UserPromptSubmit `additionalContext`) can carry the Zen cue. The `.codex-plugin` manifest can bundle them in the same system plugin. Codex has no channel equivalent and needs none, since the app-server is already the push path.

### 6. The setting

- **Key:** project setting `orchestrator.awareness: heartbeat | awareness`, default `heartbeat` until v2 is proven. It belongs with `heartbeat` in `SHARED_PROJECT_FIELDS`, and a global default could live in `~/.shelbi/config.yaml` later.
- **UI:** new "Orchestrator" group on the Project scope of Settings. Two radio options with labels exactly as requested: "Simple heartbeat system (v1)" and "Awareness system (v2)". There's no radio widget in Settings today, but add_project and new_task have radios, and the theme segmented control is the model for the shared `shelbi-app` settings state. TUI and desktop both get it.
- **Applying a change:** changing the setting needs an orchestrator relaunch, because launch args, plugin pieces and bootstrap prompt differ. The control says "Applies when the orchestrator restarts" and offers Restart.
- **Instructions:** the orchestrator template gets a v1 and a v2 variant of the event, bootstrap, heartbeat and Zen sections. Shelbi composes the right one at launch, so the agent never reads rules for the other mode.
  - v2 instructions tell the orchestrator to estimate every dispatch.
  - They tell it to set a reminder for anything it is waiting on with no event.
  - They tell it to wrap long local commands in `shelbi job run`.
- **Config upgrade:** add a sniffer so existing projects' instructions pick up the v2 sections, as Shelbi's existing config upgrades do when a shipped default changes.

## Phasing (umbrella + subtasks)

1. **Store + CLI + events.** Awareness store, `remind` / `job` / `--estimate` / `issue estimate`, poller evaluation, the three new event lines and kinds. Works in both modes (v1 orchestrators can use reminders too).
2. **Setting + mode switch.** `orchestrator.awareness` key, Settings radios (TUI + desktop), heartbeat emitter gated off in v2, digest attached to delivery batches.
3. **Claude delivery.** System plugin gains the UserPromptSubmit/Stop hooks and the plugin monitor; launch wiring. Then the channel server behind a sub-flag once verified.
4. **Codex delivery.** wake.rs priorities, `inject_items` digest, Codex hooks.
5. **Instructions + config upgrade.** v2 template sections, composition by mode, sniffer.
6. **Dogfood.** Flip this project to v2 and run a real batch. Ship with v1 as default; consider flipping the default after it holds up.

## Decisions (John, 2026-11-10)

1. **Claude delivery:** ship v2 on the auto-starting plugin monitor plus the UserPromptSubmit/Stop hooks. No channel yet (it needs `--dangerously-load-development-channels`, a warning dialog at every launch). Revisit when channels leave research preview or allow plugin channels.
2. **Scope:** per-project setting `orchestrator.awareness`, next to `heartbeat`.
3. **Estimates:** the orchestrator passes `--estimate` when it can; otherwise a per-workflow default applies (`default_estimate:` in the workflow YAML), so every active task can go overdue.
4. **Overdue alerts:** repeat until the task leaves the active category, every half the estimate (floor 10m). A 90m task alerts at 90m, 135m, 180m, ...

## To verify before building phase 3

- Does a channel event wake an idle session? The docs imply it but don't state it.
- Do plugin monitors have the 30-minute deadline?
- Does `--dangerously-load-development-channels plugin:<name>@inline` accept a `--plugin-dir` plugin?
- Does the Codex `--remote` TUI render turns that Shelbi starts? This applies to Codex only if we ever change the bridge.