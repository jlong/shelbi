# Review: the Codex weekend changes, and a path to one harness

Status: review and proposal
Date: 2026-07-15
Author: Claude (orchestrator), reviewing main at `b670346`
Companion doc: `Plans/codex-parity-with-claude.md` (the plan Codex wrote for itself on 2026-07-13)

## What actually landed

Over the weekend and the two days after, Codex shipped a substantial body of work to make itself a first-class Shelbi runner:

- **#374** `submit.rs` (1,705 lines): verified pane submission for all worker text delivery. Paste without Enter, settle, Enter as a separate key event, verify via pane re-capture. Runner-specific screen parsers behind a `SubmitProfile` enum (`ClaudeUi`, `CodexUi`).
- **#376** `wake.rs` (initial ~300 lines): tmux text-injection wakes so board events could reach an idle Codex orchestrator.
- **#378**: stable tmux pane-id targets (runner-neutral, genuinely good).
- **#379**: draft-collision guards on the injection path (don't type over a half-composed user message).
- **#380**: the big pivot. This squash-merged the `fix-codex-wake-draft-collision-and-quiescent-heartbeats` branch, which **replaced** the tmux injection wake with a native event bridge: a project-scoped `codex app-server` subprocess on a private Unix socket, a JSON-RPC client (`codex_rpc.rs`, 710 lines), a persisted thread id (`codex-thread.json`), a durable acked event queue (`codex-event-queue.json`), and ~900 lines of rejection/batch tests. `wake.rs` is now 4,010 lines.
- A 454-line plan (`Plans/codex-parity-with-claude.md`) that is honest, careful about not regressing Claude, and mostly good. Phases 2 and 3 of it are effectively implemented; Phase 1 (adapter extraction) was skipped.

Loose ends: one unmerged test commit (`55062e3`, "normalize empty tmux globals") is all that remains on the branch; everything else was squashed into #380. A dangling commit `f767b54` ("greet new projects contextually") is on no branch.

## Assessment

### What is genuinely good

1. **The native bridge is the right architecture.** Structured `turn/start`/`turn/steer` RPC with definitive `turn/started`/`turn/completed` events beats screen scraping in every dimension: no composer races, no regex grammar that breaks on a UI update, durable at-least-once delivery with explicit acks, real crash recovery (the queue survives; in-flight batches reconcile on reconnect). The test suites (`wake_rejection_tests.rs`, `wake_batch_tests.rs`) cover the failure modes that actually happen: non-steerable turns, stale threads, protocol incompatibility, reconnect reconciliation.

2. **Verified submission (#374) is runner-neutral value.** The paste/settle/Enter/verify discipline fixed real Claude-side failures too (we live-tested it at review). This is the shared harness getting better, not Codex-specific accretion.

3. **The plan doc's invariants are correct** and worth keeping as policy: events.log stays the durable source of truth, the controller is transport not scheduler, delivery is at-least-once with idempotent mutations, user input wins, degradation must be visible.

### What is concerning

1. **Codex built itself a second harness instead of extending the shared one.** The tally on main: ~2,900 lines of Codex-specific machinery (bridge + RPC + parsers) next to ~1,300 lines of Claude-specific machinery (readiness probes, input-box grammar, usage-limit modals), coordinated by ~24 scattered `is_claude_runner()` / `is_codex_runner()` call sites. The two runners now solve the same five problems (context delivery, wake, state observation, message delivery, resume) with two unrelated mechanisms each. The plan doc prescribed extracting runner adapters *first* (its Phase 1); the implementation skipped that and grew the branch count instead.

2. **The asymmetry is backwards.** Codex, the guest, got the structured path: persistent thread identity, durable acked event delivery, real turn boundaries. Claude, the home team, still runs on the fragile path: OSC title hooks, tmux footer regex, `--continue`, and a Monitor callback on a background `tail`. The durable queue with delivery ids exists but only Codex reads it. If the bridge design is right (it is), Claude deserves it too, not as app-server RPC but as the same queue/ack contract delivered over Claude's own wake mechanism.

3. **The flagship path is not actually running.** In the deployed Shelbi project right now, `codex-thread.json` says `native_active: false` (the bridge fell back to standalone) and `codex-event-queue.json` holds an undelivered pending batch. Nothing in `shelbi status` surfaces this. The plan's own invariant 7 ("a failed capability probe must be visible: structured, tmux-fallback, or degraded-polling") was never implemented. We are one silent fallback away from the exact idle-wake bug this whole effort was meant to fix, and we would not know.

4. **A known-broken hook wiring still ships.** `with_codex_hooks()` (shelbi-agent/src/lib.rs:117) still injects `-c core.hooksPath=...` plus `--dangerously-bypass-hook-trust`. The plan doc itself demonstrated that codex-cli 0.144.1 rejects `core` as an unknown config field under strict validation, and that the generated flat TOML is not the current hooks schema. Meanwhile `polls_for_messages()` counts Codex as hook-capable and disables the polling fallback. Net effect: Codex *workers* have neither working hooks nor pull polling for hub messages. The orchestrator got the bridge; workers got left with the broken wiring.

5. **Event vocabulary drift is live.** The poller emits `reason=workspace:ready-marker`; the orchestrator contract (my own instructions, and the Zen rules) matches `reason=workspace:review-marker`. I watched real events under this drift during this review. It works today only because reactions also key on `to_category=handoff`. The plan called for normalized causes plus historical aliases; that fix should not wait.

6. **Sharp edge: the one-way door.** `handoff.rs` hard-errors on switching a project from Codex-native back to Claude or a custom runner while `native_active` is true. Defensible as a data-safety guard, but it is a hard error where a guided migration ("archive the thread, then switch") belongs. Given that `native_active` is false in practice right now, this guard currently protects a path that is not even active.

7. **Layer bleed in the event-log core.** `workspace_status.rs` grew ~1,167 lines of rotation recovery, logical cursors, and migration markers to serve the bridge's durability needs. That is generic infrastructure motivated by one consumer, living in a file whose job was pane-state observation. It belongs in its own event-log module with the queue as one consumer and the legacy `drain` path as another.

## The generic model: capability ladders, not runner branches

The ideal is not "Claude and Codex behave identically." They cannot: prompt authority, resume, and hooks are genuinely different products. The ideal is that Shelbi has **one harness with five runner-neutral contracts**, and each runner declares which rung of each ladder it can climb. Runner-specific code then shrinks to thin adapters at the bottom of each ladder.

| Contract | Structured (best) | Conventional | Degraded (worst) |
| --- | --- | --- | --- |
| Context delivery | System/developer authority (`--append-system-prompt` / `developerInstructions`) | Positional startup prompt | Prompt file the agent is told to read |
| Event wake | Push a turn (app-server `turn/start`; Claude Monitor on a queue feed) | Verified tmux injection | Polling contract (drain before reply) |
| State observation | Structured events (`turn/started`/`turn/completed`, hook callbacks) | OSC title markers from hooks | Screen parsing |
| Message delivery | Active-turn injection / queued turn | Hook-driven drain (Stop/SessionStart) | Prompt-level polling of `messages/<task>.log` |
| Resume | Persistent thread id (`thread/resume`) | Transcript resume (`--continue`) | Cold start with a context banner |

Three design rules fall out of this:

1. **Every rung transition must be observable.** `shelbi status --full` and the workspace list should show, per agent: `structured | conventional | degraded` for wake and messages at minimum. A fallback is fine; a silent fallback is the bug class we just lived through.
2. **The durable layer is the lingua franca.** `events.log`, the event queue with delivery ids and acks, ready/transition markers, `messages/*.log`, `handoff.md`. All runner-neutral, all file-based, all already exist. Adapters translate between this layer and each runner's transport. Nothing above this layer may mention a runner.
3. **Scheduling stays in the model.** The bridge got this right: it batches, delivers, retries, acks. It never picks a workspace. Keep it that way as the queue generalizes.

### What this means concretely for each runner

**Claude** moves up, not sideways: keep the launch command, hooks, and `--continue` frozen (the plan's compatibility bar is right), but point the Monitor wake at a queue-feed command (`shelbi orchestrator events next --follow` or similar) instead of raw `events tail`, so Claude inherits delivery ids, ack semantics, and crash-safe replay from the same queue the bridge uses. Claude Code's hook surface already gives structured-ish state; that is its "structured" rung without any app-server equivalent.

**Codex** keeps the bridge as its structured rung, but the fallback story needs honesty: fix or remove the invalid `core.hooksPath` wiring, and make `polls_for_messages()` depend on a hook health handshake instead of the basename, so a Codex worker without working hooks gets the degraded polling contract rather than silence.

**Unknown runners** (aider, future CLIs) get the degraded rung of every ladder by default and work correctly, just less pleasantly. This is the real payoff of the ladder model: adding a runner means writing adapters only for the rungs where it can do better.

## Recommended actions, ranked

1. **Surface integration health now** (small, high value). Add wake/message mode per agent to `shelbi status --full` and emit an event on any fallback transition. Then investigate why `native_active` is false in this project and drain the stuck pending batch.
2. **Fix the Codex worker message path** (bug). Either ship a valid hooks config for the installed CLI behind a live handshake, or flip Codex to the polling fallback until hooks are proven. Today it is silently neither.
3. **Normalize the handoff cause** (small). Emit one typed cause, accept `workspace:ready-marker` / `workspace:review-marker` / `worker:review-marker` as aliases, add the emitter-to-contract test the plan specified. Update the orchestrator template to match what the poller actually emits.
4. **Extract the event-log/queue core out of `workspace_status.rs`** (refactor). Rotation recovery, logical cursors, the durable queue, and delivery ids become a shared module with two consumers: the Codex bridge and a new Claude queue feed. This is the plan's Phase 1 adapter work, done in the order that pays down the newest debt first.
5. **Give Claude the queue** (medium). Swap the raw `events tail --follow` bootstrap for a queue-feed with acks. Claude's wake mechanism (Monitor) stays; only the feed underneath changes. This deletes the "Claude loses events on crash between drain and reaction" window without touching Claude's launch surface.
6. **Collapse the runner branches into adapters** (larger, do after 4–5 prove the seams). One `RunnerAdapter` per runner owning: launch flags, prompt authority, hook deployment, submit profile, resume, and ladder capabilities. Target: `is_claude_runner()` / `is_codex_runner()` appear only inside adapter construction. The plan's `integration: claude|codex|generic` YAML field comes along for wrapper commands.
7. **Dedupe the hook scripts** (small, anytime). `claude.*` and `codex.*` hook bodies are near-identical copies; deploy one runner-neutral set plus per-runner config shims.
8. **Soften the one-way door** (small). Replace the hard error on Codex-to-legacy runner switch with a guided path: archive `codex-thread.json`, mark the queue drained, proceed.
9. **Decide the branch remnants** (trivial). Merge or discard `55062e3`; delete the stale branch either way. Triage the dangling `f767b54`.

Items 1–3 are independently shippable this week and none of them touch Claude's frozen surface. Items 4–6 are the actual convergence and should ride the plan doc's golden-fixture discipline (freeze Claude artifacts byte-for-byte before moving code).

## What stays runner-specific, permanently

Being honest about the floor so we stop aspiring past it:

- Launch flags and prompt authority (`--append-system-prompt` vs `developerInstructions` vs positional).
- Resume mechanics (`--continue` vs `thread/resume` vs cold banner).
- Hook config formats and trust flows (`.claude/settings.json` vs whatever Codex's supported layer turns out to be).
- Screen grammar, for as long as any rung still scrapes (Claude's bordered box vs Codex's `› ` composer).
- Usage-limit and approval-dialog signatures.

Everything else that is runner-specific today (event queueing, batching, ack, cursors, rotation recovery, message files, markers, submission verbs, scheduling) is shared harness wearing a runner costume, and the work above takes the costume off.

## Bottom line

The weekend work is better than the situation it responded to: the idle-wake gap was real, the bridge is the right long-term shape, and the tests are the best-covered corner of the orchestrator. The cost is that Shelbi now contains two philosophies of agent integration, the newer and better one is attached to the guest runner and currently disengaged, and the shared core absorbed ~1,200 lines of one consumer's infrastructure. The unification path is not to make Codex more like Claude. It is to promote the bridge's queue/ack/observability model into the shared harness, let Claude consume it through its own wake mechanism, and shrink both runners to thin adapters over five explicit capability ladders.
