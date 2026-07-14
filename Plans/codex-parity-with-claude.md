# Codex parity with Claude, without regressing Claude

Status: proposed  
Date: 2026-07-13  
Code reviewed: `e22fd2d` on `main`, plus the current project configuration and relevant in-flight task bodies

## Executive summary

The board-awareness bug is real and has a precise cause: Shelbi can record and replay board events for Codex, but it cannot wake an idle Codex orchestrator.

Claude works because its runtime turns output from a background `shelbi events tail --follow` process into an asynchronous Monitor callback. That callback creates another agent turn. Codex is instead told to run `shelbi orchestrator events drain` before its next reply. The drain closes replay gaps once a turn exists, but a board transition or heartbeat does not create that turn. When nobody speaks to the orchestrator, the event remains in `events.log` and no scheduling decision happens.

There are two additional material gaps:

1. The current Codex hook integration is not valid for the installed Codex CLI. Shelbi launches workers with `-c core.hooksPath=.shelbi/hooks/codex.toml`, but `codex-cli 0.144.1` rejects `core` as an unknown configuration field under strict validation. The generated flat TOML is also not the current hook schema. At the same time, Shelbi treats Codex as a hook-capable runner and disables its message-polling fallback.
2. The poller emits handoff events with `reason=workspace:ready-marker`, while the orchestrator contract matches `reason=workspace:review-marker`. This mismatch was visible in live project events during this audit. It can delay the freed-workspace reaction for Claude as well as Codex.

The recommended design is a compatibility-preserving runner adapter layer:

- Keep Claude on its current command line, `.claude` deployment, hooks, Monitor behavior, pane grammar, resume behavior, and supervision path. Lock these down with golden and live regression tests before shared refactoring.
- Add a Codex-only event controller that watches the durable Shelbi event stream and can create a Codex turn without human input. Its target backend should be Codex app-server, with a verified tmux submission backend retained as a fallback while app-server remains version-gated.
- Keep scheduling policy in the orchestrator model. The controller only batches, delivers, retries, and acknowledges normalized events.
- Add Codex-native context, lifecycle, state, message, resume, approval, and limit handling instead of passing Codex through Claude-specific files and screen parsers.

This should be shipped in phases, dogfooded with the Shelbi project in Codex-only mode, and kept behind an immediate rollback switch. Existing Claude project YAML must continue to behave exactly as it does today.

## Goals and measurable outcomes

### Orchestrator behavior

- A task moved into a ready-category status while Codex is idle starts an orchestrator turn and is dispatched without a user reminder.
- A workspace handoff immediately makes the freed dev slot eligible for its next ready task.
- A project heartbeat starts the documented bounded sweep when work is in flight, even if no user has spoken.
- Events that arrive while a user turn is active are queued and processed at a safe turn boundary. They must not corrupt the user's input or redirect the answer mid-turn.
- Events are project-scoped, deduplicated, and retried until acknowledged. A controller or orchestrator crash cannot silently consume a batch.
- Zen policy remains model-owned and the existing review-workspace gate remains intact.

### Worker behavior

- Codex receives Shelbi role instructions at developer/system authority, not merely as a user request to read a Claude-namespaced file.
- Initial prompt delivery, resume, hub messages, working/idle/blocked state, approvals, usage limits, and completion markers all have a Codex-native or runner-neutral implementation.
- Local and SSH workspaces have explicit supported behavior. A remote capability gap must show as degraded, not silently pretend hooks are working.

### Compatibility and reliability bar

- Zero changes to the generated Claude orchestrator launch command and Claude worker launch command, except separately approved bug fixes.
- Zero removal or repurposing of `.claude/settings.json`, `.claude/agent-instructions.md`, `.claude/skills`, Claude hook scripts, `--permission-mode`, or `--continue`.
- No overwrite of user-owned `.codex/config.toml`, `.codex/hooks.json`, `AGENTS.md`, or user plugins.
- Ready-event-to-Codex-turn latency: p95 under 2 seconds locally; ready-event-to-dispatch p95 under 10 seconds when an eligible workspace is free.
- No lost event and no duplicate dispatch in crash, reconnect, log-rotation, and repeated-event tests.
- Claude's existing dispatch and message delivery smoke tests stay at least 20/20 successful before and after the change.

## Non-negotiable design invariants

1. `events.log` remains the durable source of truth. A callback, file notification, socket message, or app-server turn is a wake hint, not the only copy of an event.
2. The Rust controller is transport, not scheduler. It must not choose a workspace, interpret Zen intent, merge a branch, or bypass the orchestrator's reaction rules.
3. Event delivery is at least once. Scheduling operations must remain idempotent or be guarded by current board state.
4. User input wins. Automation that arrives during a user turn waits for a safe boundary unless it is an explicit interrupt-class event with a documented policy.
5. Claude is a frozen compatibility adapter. Codex support is added beside it, not by broadening Claude's screen matchers or routing Claude through app-server.
6. Existing YAML stays valid. Runner type may continue to auto-detect from the executable basename; an optional explicit integration field should exist for wrapper commands.
7. A failed capability probe must be visible. Shelbi should report `structured`, `tmux-fallback`, or `degraded-polling` rather than claiming full Codex integration.

## What exists today

### Event production and the missing wake

The sidebar's poller owns both per-workspace observation and project heartbeats (`crates/shelbi-tui/src/poller.rs:84-210,301-458`). Task transitions, workspace state, pane death, supervision, Zen changes, and heartbeats are appended to `~/.shelbi/events.log`.

The orchestrator bootstrap tells Claude to start `shelbi events tail --follow` and watch the process with Monitor (`crates/shelbi-orchestrator/src/lib.rs:983-999`; `crates/shelbi-state/src/default_orchestrator.md.template:145-183`). `events tail --follow` itself only polls the file and prints new lines every 250 ms (`crates/shelbi-cli/src/commands/events.rs:60-157`). The actual wake is a Claude runtime behavior outside Shelbi.

Codex receives a polling-only contract in its launch prompt (`crates/shelbi-orchestrator/src/lib.rs:1049-1073`). `shelbi orchestrator events drain` and `events next` expose project-scoped facts with cursors, but deliberately do not schedule or create an agent turn (`crates/shelbi-cli/src/commands/orchestrator.rs:1-5,72-83`). The tests explicitly validate delivery at the next turn boundary (`:588-619`) and validate that a heartbeat can be read (`:686-718`); they never prove that an idle Codex gets a turn.

The heartbeat scheduler compounds the problem. When it notices recent `events.log` activity, it skips the pending heartbeat on the explicit assumption that the real event "already woke the orchestrator" (`crates/shelbi-tui/src/poller.rs:346-410`). That assumption is true only for a callback-capable runtime. With Codex, the real event can fail to wake the model and also postpone the next backstop heartbeat. Heartbeat emission is additionally gated on a public-network probe to `1.1.1.1:443` (`poller.rs:564-577`), even though a local board sweep can still be useful while offline. The plan should decouple local scheduler wake from public-network availability or make that gate specific to checks that actually require the network.

The existing drain cursor advances as soon as a batch is read, before the model applies its reactions (`crates/shelbi-cli/src/commands/orchestrator.rs:134-184`). A model or pane crash in that interval creates a loss window. File rotation has another cursor edge: both drain and follow reset only when the saved byte offset is larger than the current file. If a new generation grows beyond the old offset before the next read, the beginning of that generation can be skipped. A reliable controller therefore needs generation-aware delivery ids plus claim/ack semantics, without changing legacy `drain` underneath Claude.

That produces the current dead zone:

```text
board move / heartbeat
          |
          v
      events.log  <---------------- durable and correct
          |
          +---- Claude tail output -> Monitor callback -> Claude turn -> reaction
          |
          +---- Codex drain on next turn ----------------^  no turn creator
```

There is a dormant callback producer in `crates/shelbi-state/src/workspace_status.rs:30-127,1092-1152`. If `SHELBI_ORCH_EVENT_CALLBACK_SOCK` is set in the process that appends a line, it sends a best-effort `EventEnvelope` to a Unix socket. There is no production listener or registration path, and no production setter outside the implementation. Setting the variable only in the orchestrator pane would not reach the sidebar poller or unrelated CLI processes that write the events. The callback envelope and drain response also use different `kind` vocabularies. This is useful prior art, not a working wake path.

### Claude connection points that must be preserved

| Connection point | Claude behavior today | Evidence |
| --- | --- | --- |
| Runner identification | Executable basename `claude` selects first-class behavior | `crates/shelbi-agent/src/lib.rs:29-46` |
| Orchestrator launch | Existing flags, then `--append-system-prompt "$(cat .claude/agent-instructions.md)"`, then positional bootstrap | `crates/shelbi-orchestrator/src/lib.rs:1001-1033`; golden tests `:1807-1873` |
| System context | Shared preamble plus role instructions staged in `.claude/agent-instructions.md` | `crates/shelbi-state/src/agent_workspaces.rs:263-363`; `workspace.rs:1968-1980,2161-2268` |
| Skills | Role skills mirrored to `.claude/skills` | `workspace.rs:2316-2340` |
| Hooks | `.claude/settings.json` maps SessionStart, Stop, Notification, UserPromptSubmit, and PreToolUse to Shelbi-owned scripts | `crates/shelbi-state/src/default_workspace_settings.json.template` |
| Hub messages | SessionStart tails the task log; Stop injects unread messages and acks them | `workspace.rs:1982-2023` |
| Pane state | Hook scripts emit `shelbi:working`, `shelbi:idle`, and `shelbi:blocked` OSC titles | `workspace.rs:2025-2027`; `poller.rs:637-713` |
| Permissions | `--permission-mode` is injected only for Claude | `crates/shelbi-agent/src/lib.rs:48-76` |
| Resume | `shelbi task resume` adds Claude `--continue` and verifies a pasted resume prompt | `crates/shelbi-agent/src/lib.rs:78-105`; `workspace.rs:1180-1426` |
| Readiness and submission | Claude input box, trust dialog, spinner, footer, paste-chip, and title-marker grammar | `crates/shelbi-orchestrator/src/ready.rs:17-94`; `workspace.rs:1439-1769` |
| Dialogs and usage limit | Built-in Claude dialog signatures and structural Claude usage-limit parser | `crates/shelbi-core/src/model.rs:200-231`; `ready.rs:116-248` |
| Handoff | Agent writes `.claude/shelbi-ready`; poller rebases, transitions, detaches, and closes dev pane | `workspace.rs:124-185`; `poller.rs:895-1075` |
| Crash/reload | Runner-neutral pane supervision plus a one-shot handoff file; Claude workspace transcripts resume with `--continue` | `poller.rs:1708-1755`; `crates/shelbi-orchestrator/src/handoff.rs`; `workspace.rs:1050-1147` |

These are not merely incidental Claude references. Several are operational protocols refined around real failures. They need regression fixtures before common code is moved.

### Codex connection points and current gaps

| Connection point | Current Codex behavior | Gap |
| --- | --- | --- |
| Orchestrator context | Entire Shelbi contract is embedded in the initial positional user prompt | Lower authority than Claude's appended system prompt; no persistent structured thread identity |
| Orchestrator wake | Drain before user-facing replies | Cannot wake idle Codex; heartbeat is inert without a turn |
| Worker context | Startup prompt says to read `.claude/agent-instructions.md` | Claude namespace and user-prompt authority; no Codex-native skill mounting |
| Hooks | Adds `-c core.hooksPath=.shelbi/hooks/codex.toml --dangerously-bypass-hook-trust` | Installed Codex rejects `core`; generated flat TOML is not current hooks schema |
| Message fallback | `polls_for_messages(Codex)` returns false | With invalid hooks, Codex has neither hooks nor pull polling |
| Prompt confirmation | Shared confirmation code falls through to Claude pane grammar | Codex launch success can false-fail or false-confirm when title hooks do not fire |
| Resume | Cold start with a banner pointing at preserved worktree | Codex CLI supports `resume`, and app-server supports `thread/resume`, but Shelbi persists no Codex session/thread id |
| State/dialogs/limits | Generic pane title plus Claude-only fallback parsers; no built-in Codex dialog signatures | Codex approvals, busy state, and limit states are not reliably observed |
| Orchestrator reload request | Blind tmux paste plus Enter | No Codex idle/busy/submit verification; can strand the handoff request |

The hook break is reproducible with the installed CLI:

```text
$ codex app-server --strict-config \
    -c core.hooksPath=.shelbi/hooks/codex.toml --listen off
Error: unknown configuration field `core` in -c/--config override
```

Current official Codex hook discovery looks for `hooks.json` or inline `[hooks]` next to an active config layer, and hook definitions use event -> matcher group -> handler nesting. Project hooks also participate in a trust flow. See [OpenAI's Codex hooks reference](https://developers.openai.com/codex/hooks). In addition, Codex Stop hooks require JSON output; Shelbi's shared message-drain script emits plain text, which is valid developer context for some events but invalid for Stop in the current protocol.

### Event protocol drift

The poller emits `reason=workspace:ready-marker` in `crates/shelbi-tui/src/poller.rs:992-1002`. The normal and Zen orchestrator rules match `reason=workspace:review-marker` in `crates/shelbi-state/src/default_orchestrator.md.template:232,550`, while site documentation still names `worker:review-marker`.

This is not cosmetic. The poller closes the finished dev pane after the transition, so the task transition may be the only immediate signal that the dev slot should be reloaded. The fix should accept historical aliases, emit one normalized typed cause going forward, and add a contract test that feeds the real emitter output into the real reaction classifier or prompt fixture.

## Target architecture

```text
                     durable facts
poller / CLI / daemon -------------> events.log
                                      /       \
                                     /         \
             existing Claude path  /           \  Codex-only controller
              tail + Monitor wake  /             \ batch + scope + retry + ack
                                  v               v
                       Claude adapter         Codex adapter
                         unchanged        app-server turn/start
                                           or verified tmux fallback
                                  \               /
                                   \             /
                                  orchestrator model turn
                             existing reaction + Zen judgment
                                           |
                                           v
                                  Shelbi CLI mutations
```

### 1. Introduce explicit runner adapters

Replace scattered basename branches with a small capability-oriented adapter surface while preserving basename detection as the default for existing YAML.

The adapter needs to own these behaviors:

- launch command and prompt authority;
- context and skill deployment;
- initial prompt delivery and verified submission;
- asynchronous event delivery;
- thread/session resume;
- lifecycle hook deployment and health;
- working, idle, blocked, approval, and usage-limit observation;
- hub-message delivery;
- local and SSH transport constraints.

An optional YAML field such as `integration: claude|codex|generic` should allow wrapper commands to select an adapter without renaming the executable. Existing `command: claude` and `command: codex` continue to auto-detect, and absent fields serialize exactly as before.

The Claude adapter should initially delegate to the existing functions without changing their output. Do not "generalize" by adding Codex strings to `wait_for_claude_ready`, `claude_is_processing`, Claude input-box parsing, or Claude limit detection. Move code only after golden tests exist, and compare generated artifacts byte for byte.

### 2. Add a project event controller with acknowledged delivery

The controller should be supervised with the orchestrator pane and should watch the project-scoped durable log, not depend on the environment variable of every event writer.

Required behavior:

- Maintain its own wake/delivery state. It must not advance the current model `event-cursor` merely to discover that work exists.
- Coalesce bursts into a small batch, retaining raw lines and normalized fields.
- If the orchestrator is idle, submit one scheduler turn.
- If a user or scheduler turn is active, queue the batch. Process it at the next safe boundary; do not type into an active input box.
- Give each batch a stable delivery id derived from project plus cursor range. Codex app-server can also receive that id as `clientUserMessageId` for tracing.
- Retry an unacknowledged batch after controller, pane, or app-server restart.
- Let the orchestrator explicitly acknowledge a batch only after it has applied all required reactions. Keep the existing `events drain` command for compatibility; add a non-consuming peek/claim and explicit ack path for the controller instead of silently changing Claude's cursor semantics.
- On duplicates, inject the same delivery id and require the model to reconcile against current board state. `shelbi task start` and other mutations should reject stale transitions safely.

The controller injects facts and an instruction to apply the existing reaction rules. It does not choose a workspace or run Zen operations itself.

### 3. Make Codex app-server the structured target backend

Codex app-server is the official deep-integration surface for conversation history, approvals, and streamed events. It supports persistent threads, `thread/start`/`thread/resume`, `turn/start`, `turn/steer`, and definitive `turn/started`/`turn/completed` notifications. It also supports attaching the Codex TUI over a local Unix endpoint. See [OpenAI's Codex app-server documentation](https://developers.openai.com/codex/app-server).

Recommended orchestrator topology:

1. Shelbi starts `codex app-server` on a private Unix socket in a Shelbi-owned, mode-0700 runtime directory.
2. A Shelbi app-server client initializes the connection and starts or resumes the persisted orchestrator thread.
3. The thread is created with the composed Shelbi orchestrator prompt as `developerInstructions`, the project config directory as `cwd`, and explicit approval/sandbox values translated by the Codex adapter.
4. Shelbi records the returned thread id in project runtime state.
5. The human-facing TUI attaches to the same server/thread with `codex resume --remote unix://... <thread-id>`.
6. The event controller uses `turn/start` when the thread is idle. It normally queues during an active user turn. `turn/steer` is reserved for an explicitly defined interrupt class or for coalescing new scheduler facts into a scheduler-owned turn.
7. `turn/started`, `turn/completed`, approval requests, and item events become the authoritative Codex state signals, replacing tmux spinner and input-box guesses.

The installed `codex-cli 0.144.1` exposes these methods and fields, including `developerInstructions` and `clientUserMessageId`, in its generated app-server schema. However, the CLI still labels app-server experimental and the public docs label WebSocket transport experimental. Therefore this backend needs a startup capability handshake and a supported-version range. Failure falls back to the verified tmux backend and emits a visible degraded-status event; it must never leave the orchestrator unwakeable.

### 4. Retain a verified tmux fallback

The current `comms-verified-submit-for-all-text-injected-into-worker-panes-messages-and-nudges-not-just-dispatch` task is directly relevant. Once accepted, its shared verified-submit primitive should be the only tmux text-delivery path used by the Codex fallback.

The fallback controller should:

- wait for a Codex-specific idle/readiness signal;
- paste a short wake request, settle, send Enter as a separate event, and verify a Codex turn began;
- retry Enter a bounded number of times;
- emit submitted/stuck events and retain the batch until acknowledged;
- never reuse Claude input-box grammar as proof for Codex;
- queue while the pane is busy.

A Codex Stop hook can provide a secondary turn-boundary rail: if events arrived during the current turn, return valid Stop-hook JSON with `decision: "block"` and a reason instructing Codex to drain and react. This closes the "arrived before final answer" window. It cannot wake an already idle Codex when a future event arrives, so it is not a substitute for the controller.

### 5. Normalize the event protocol

Create one shared normalized event model for log parsing, callback envelopes, controller batches, and CLI drain output.

- Use typed event kind and category fields for routing.
- Introduce a normalized cause for workspace handoff and accept `workspace:ready-marker`, `workspace:review-marker`, and `worker:review-marker` as historical aliases.
- Keep raw lines for diagnostics and forward compatibility.
- Preserve project scoping before any reaction.
- Generate prompt examples or contract fixtures from the same constants used by emitters where practical.
- Decide and test log-rotation cursor behavior. A cursor past the new file length must not cause an unbounded replay storm or a lost unacknowledged batch.

Do this without moving scheduler judgment into Rust. Normalization answers "what happened," not "what should we do."

### 6. Add Codex-native hooks only through a supported, non-destructive layer

The current `core.hooksPath` and flat `codex.toml` wiring should be removed from the Codex adapter after a compatibility migration. It must not be removed from Claude or change `.claude/settings.json`.

Phase 0 must spike which supported Codex configuration layer can be supplied without overwriting repository or user configuration:

1. a Shelbi-owned Codex plugin/profile layer enabled for the session;
2. a validated inline hooks config passed as an active CLI config layer; or
3. a non-destructive project hook merge only if Codex exposes a safe, reversible mechanism.

The chosen path must pass strict config validation and a live hook handshake. Do not declare hooks healthy because a config file was generated.

Codex hook adapters need Codex-specific output rules:

- `SessionStart`: start or verify the message channel and emit a hook-version/health handshake;
- `UserPromptSubmit`: mark working and optionally supply Shelbi developer context;
- `PreToolUse`: mark working;
- `PermissionRequest`: mark blocked with approval detail;
- `PostToolUse`: inject unread hub messages using valid JSON additional context when appropriate;
- `Stop`: mark idle, drain messages with valid Stop JSON, and request a continuation only when a pending event/message exists.

With app-server, structured turn and approval events should be primary and hooks become fallback/compatibility. Either way, `polls_for_messages(Codex)` must depend on a successful hook/controller health handshake. If health is absent, restore pull polling rather than silently dropping messages.

### 7. Bring Codex workers to parity

After the orchestrator is reliable, extend the same Codex adapter to worker panes.

- Start or resume a persisted thread per workspace/task. Use `codex resume` or app-server `thread/resume`, not a cold banner, when the session is available.
- Pass shared preamble plus role instructions as Codex developer instructions. Keep `.claude/agent-instructions.md` and `.claude/skills` deployed for Claude; add a Shelbi-owned runner-neutral artifact or Codex-native mount alongside them.
- Verify the instruction sources returned by app-server and expose them in diagnostics.
- Translate project permission intent into explicit Codex approval and sandbox settings. Do not reinterpret Claude `workspace_permissions_mode` without a documented mapping and tests.
- Deliver hub messages through the structured active-turn/queued-turn channel where possible. Fall back to a healthy Codex hook or explicit pull polling.
- Add Codex-specific approval, busy, idle, and usage-limit detection. Prefer app-server events; use separately versioned pane signatures only in tmux fallback.
- Keep the file-based ready and transition markers. They are runner-neutral in behavior even though their current path is under `.claude`; changing that path is not necessary for parity and risks Claude regressions.
- For SSH workspaces, initially keep the verified tmux/hook path. Add remote app-server only after a private socket/SSH-forward design passes disconnect and reconnect tests.

## Implementation phases

### Phase 0: characterize, freeze Claude, and fix the protocol typo

Deliverables:

- Capture golden fixtures for Claude orchestrator argv, Claude worker argv, settings JSON, deployed hook scripts, instruction and skill paths, resume flags, reload handoff, local/SSH pane wrappers, and relevant pane captures.
- Add a live Claude smoke harness that proves background Monitor wake, ready dispatch, handoff reload, heartbeat sweep, messages, resume, and crash supervision.
- Add a live Codex characterization harness that proves the current idle-wake failure and records the installed Codex version/capabilities.
- Replace the handoff reason drift with a normalized cause plus backward-compatible aliases. Update prompt and docs to accept the actual emitted reason before changing the emitter.
- Add strict validation and a live handshake test for any Codex hooks config.

Exit criteria:

- The current Codex failure is reproducible without a human.
- All Claude fixtures pass unchanged.
- A real poller-produced handoff event is recognized by the orchestrator contract.

### Phase 1: extract runner adapters with no behavioral change

Deliverables:

- Add `Claude`, `Codex`, and `Generic` adapters/capability records.
- Preserve basename auto-detection and all current YAML.
- Move existing Claude code behind the Claude adapter without changing output.
- Add explicit degraded-state reporting for missing Codex capabilities.

Exit criteria:

- Claude golden artifacts are byte-for-byte identical.
- Existing unit and integration suites pass.
- Selecting a wrapper executable with `integration: codex` reaches the Codex adapter.

### Phase 2: ship the Codex event controller and tmux wake fallback

Deliverables:

- Project-scoped watcher, batching, queueing, delivery ids, retry, and explicit acknowledgment.
- Codex-specific idle/busy state for fallback.
- Verified-submit integration for orchestrator nudges.
- Valid Stop-hook catch-up behavior when a batch arrives during a turn.
- Status/events for queued, delivered, acknowledged, retried, stuck, and degraded batches.

Exit criteria:

- Idle Codex reacts to ready moves and heartbeats without a user message.
- User input preempts queued automation cleanly.
- Killing the controller between detect, deliver, and ack loses nothing.
- Claude does not start or depend on this controller backend.

### Phase 3: add structured Codex app-server orchestration

Deliverables:

- Private Unix app-server lifecycle and capability handshake.
- Persisted orchestrator thread id and structured resume.
- Developer-instruction authority for the Shelbi orchestrator contract.
- TUI attachment to the same thread.
- Structured turn, approval, state, and error observation.
- Automatic fallback to Phase 2 on unsupported or unhealthy versions.

Exit criteria:

- No tmux scraping is needed for Codex orchestrator idle/busy/submit/resume in structured mode.
- Reload and supervisor restart return to the same thread and replay unacknowledged events.
- App-server loss degrades visibly to verified tmux wake.

### Phase 4: bring Codex workers to structured parity

Deliverables:

- Codex developer context and skill mounting.
- Thread/session persistence per workspace task.
- Structured messages, state, approvals, and limits, with hook/poll fallbacks.
- Local first; SSH app-server only after a separate transport spike.

Exit criteria:

- Codex worker dispatch, messages, resume, and handoff match the Claude behavior contract.
- Mixed Claude/Codex projects pass the matrix below.

### Phase 5: dogfood, document, and roll out

Deliverables:

- Run the Shelbi project with Codex structured orchestration for at least one week of normal board use.
- Publish runtime mode and event-delivery health in `shelbi status --full` and the activity feed.
- Update architecture, runner configuration, events, hooks, troubleshooting, and migration docs.
- Keep an opt-in switch through dogfood; move `transport: auto` to the default only after metrics pass.
- Retain a one-field rollback to legacy Codex TUI behavior and leave Claude configuration untouched.

## Verification matrix

### Runner combinations

| Orchestrator | Worker | Local | SSH | Required |
| --- | --- | --- | --- | --- |
| Claude | Claude | yes | yes | frozen baseline |
| Claude | Codex | yes | yes | worker parity and mixed-runner safety |
| Codex | Claude | yes | yes | primary scheduler fix without worker changes |
| Codex | Codex | yes | yes, fallback first | full parity |

### End-to-end scenarios

1. Idle orchestrator + `backlog -> ready` task: one wake, one dispatch, no user input.
2. Three ready events in a burst: one coalesced turn, priority order honored, no duplicate task starts.
3. `active -> handoff` from the real ready marker: freed dev workspace receives the next ready task; review workspace remains human-loaded.
4. Quiet active task + heartbeat: bounded missed-marker and missed-dispatch sweep runs.
5. Quiet board heartbeat: a scheduler turn performs the bounded sweep, then produces no noisy user-facing response when the reaction is a documented silent acknowledgment.
6. Event during a user turn: user answer completes first, then queued scheduler turn runs; no text lands in the live input box.
7. Event during a scheduler turn: safely coalesce or queue according to the active-turn policy.
8. Cross-project event with the same workspace name: ignored before delivery/reaction.
9. Duplicate line and duplicate batch delivery: same delivery id, current board reconciled, no duplicate dispatch.
10. Controller crash before delivery, after accepted delivery, after model drain, and before ack: batch eventually completes once.
11. App-server crash and unsupported Codex version: visible fallback, no lost wake.
12. Log rotation and cursor past EOF: bounded replay, no missing unacknowledged event.
13. Orchestrator reload and supervisor restart: same Codex thread, one-shot handoff retained, pending event replayed.
14. Approval/trust dialog and usage limit: correct blocked/paused status, no Claude-parser false positive.
15. Hub message to idle and busy worker: delivered or queued with ack; no manual Enter.
16. Ready marker and arbitrary transition marker: unchanged for both runners.
17. Zen on/off/paused and review-workspace gate: no auto-merge regression.
18. Existing customized Claude settings, instructions, and skills: preserved through reload.

### Failure injection

- corrupt or rejected hook config;
- hook trust not granted;
- app-server socket unavailable or server version outside the supported range;
- SSH disconnect while a message or wake is pending;
- pane at approval UI, input UI, busy turn, and dead process;
- daemon/sidebar restart;
- event writer that does not inherit any callback environment variable;
- model turn failure after event delivery;
- CLI mutation failure after the model chose an action.

## Likely code and documentation surface

- `crates/shelbi-core/src/model.rs`: optional explicit integration/transport config and capability serialization.
- `crates/shelbi-agent`: runner adapter interface plus frozen Claude, structured Codex, and generic implementations.
- `crates/shelbi-orchestrator/src/lib.rs`: adapter-selected orchestrator launch and supervised controller lifecycle.
- `crates/shelbi-orchestrator/src/workspace.rs`: adapter-selected context, prompt, hook, resume, state, and submit behavior; retain shared marker and lifecycle pieces.
- New Codex app-server client/controller module, preferably isolated from Claude code.
- `crates/shelbi-cli/src/commands/orchestrator.rs`: non-consuming batch claim/peek, explicit ack, and health/status diagnostics while keeping `drain` compatible.
- `crates/shelbi-state/src/workspace_status.rs`: one normalized event envelope/cause model; either re-home or deprecate the dormant callback socket.
- `crates/shelbi-tui/src/poller.rs`: runner-selected state/limit observation and controller health in status; remove the assumption that any recent event already woke every runner, and do not suppress local scheduler recovery solely because a public-network probe failed. Keep scheduler judgment out of the poller.
- `crates/shelbi-state/src/default_orchestrator.md.template`: transport-neutral batch/ack contract and corrected handoff cause, while retaining Claude Monitor bootstrap.
- Hook templates: leave Claude files unchanged; add Codex-native adapters only through a supported configuration layer.
- Site docs and ContextStore architecture docs: correct the claim that every runner is woken by `events tail --follow`.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| App-server protocol changes | Startup version/capability handshake, generated-schema contract tests, supported-version range, verified tmux fallback |
| Shared refactor breaks Claude | Freeze artifacts first, isolate Claude adapter, byte-for-byte tests, live canary, Codex-only feature flag |
| Event wake races with user input | Structured turn state, queue by default, explicit interrupt policy, verified submit only when idle |
| Controller consumes events before model acts | Separate wake state, non-consuming claim, explicit ack, retry unacknowledged batches |
| Hook discovery/trust changes | Strict validation plus live health handshake; never assume a generated file ran |
| Shelbi overwrites user Codex config | Use a Shelbi-owned plugin/profile/runtime layer; fail closed if no supported non-destructive layer exists |
| Remote app-server adds fragile forwarding | Ship remote verified-tmux/hook fallback first; gate structured SSH on a separate reconnect test suite |
| Prompt rules drift from emitted protocol | Typed causes, historical aliases, emitter-to-reaction contract test |
| Model receives automation as ordinary user intent | Use developer instructions for durable policy and clearly tagged internal event batches with stable delivery ids |

## Definition of done

This effort is complete when the user can leave a Codex orchestrator idle, change the board from the TUI, and watch the correct scheduling action happen without speaking to it; the same is true for heartbeat recovery. Codex worker lifecycle and communication pass the same observable contract as Claude. Claude's current integration remains intact under golden, live local, live SSH, reload, message, resume, handoff, and Zen tests. Runtime diagnostics make any fallback or degradation explicit, and one configuration change can roll Codex back without touching Claude.

## Reference material

- [OpenAI Codex hooks reference](https://developers.openai.com/codex/hooks)
- [OpenAI Codex app-server documentation](https://developers.openai.com/codex/app-server)
- Existing ContextStore plan: `Plans/orchestrator-auto-dispatch.md`
- Existing ContextStore plan: `Plans/worker-orchestrator-communication.md`
- Existing ContextStore architecture: `Engineering/architecture.md`
