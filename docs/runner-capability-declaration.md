# Runner capability declaration for heterogeneous fleets

**Status: held design, file-only.** This document captures the follow-up filed
under resolved decision Q5a (§8) of the `agent-manifest-model-configuration`
plan (in the Shelbi ContextStore `Shelbi` space, `Plans/`). It is deliberately
not implemented: the manifest milestone assumes a **uniform fleet** (every
machine has every runner kind installed), which is trivially true for today's
all-Claude hub slots. The work described here becomes real only when a fleet
actually goes heterogeneous, for example a machine that has `codex` but not
`claude`, or the reverse. Until then this file is the specification of record so
the implementation can start from a settled design rather than a cold re-think.

## Why there is a gap at all

Runner selection used to be a property of the *slot*. A `Workspace` carried
`runner: String`, and because a slot lived on exactly one machine, that field
doubled as an implicit record of which runner was installed where. The manifest
milestone moved runner/model/effort selection onto the *agent* (role) and its
consuming *project*, and then removed `Workspace.runner` outright (plan §8,
decision Q5). Removing it deleted the only field that recorded per-machine
runner availability.

Under the uniform-fleet assumption that loss costs nothing: dispatch resolves a
runner kind from the agent/project chain
(`shelbi_core::resolve_agent_launch`) and then maps that kind to a concrete
launcher with `Project::runner_for_kind`, and both are guaranteed to succeed
because every machine has every kind. The moment machines differ, the resolver
can pick a kind that the *routed machine* cannot run, and nothing in the current
path notices until launch fails on the remote host.

## What already exists to build on

The design leans on seams that are already in the tree, so the implementation is
additive rather than structural.

- **`RunnerKind`** (`crates/shelbi-core/src/model.rs`) is the canonical
  `claude` / `codex` / `generic` classification every launch decision already
  keys off. A capability declaration is a set of these.

- **Per-kind `CapabilityLadder`** (`RunnerKind::capabilities`,
  `crates/shelbi-core/src/model.rs`). Each kind already declares its transport
  tiers (`context_delivery`, `event_wake`, `state_observation`,
  `message_delivery`, `resume`) as `Conventional` / `Degraded`. This is the
  ladder Q5a says to key off: when more than one available kind can serve an
  agent, the ladder ranks them so degradation prefers the better-integrated
  runner over a merely-present one.

- **Machine-to-workspace tag inheritance** (`Project::effective_tags`,
  `crates/shelbi-core/src/model.rs`). A `Machine` already carries a `tags`
  vector that every workspace on it inherits, unioned with the workspace's own
  tags. Runner-kind availability wants the exact same shape: declared once on
  the machine, inherited by every slot, optionally refined per slot. Reusing
  this pattern keeps the mental model consistent and the code small.

- **Graceful-degradation fallback already in the launch path.**
  `resolve_workspace_launch` (`crates/shelbi-orchestrator/src/workspace.rs`)
  already handles the case where the resolved kind has *no concrete launcher* in
  the project fleet: it falls back to the project baseline runner and, crucially,
  refuses to cross-inject one kind's model/effort onto another kind's launcher
  (`base_is_resolved_kind`). Availability degradation slots in right next to
  this existing branch and reuses the same "did we actually get the resolved
  kind" bookkeeping.

- **`prefers_machine`** (`Task::prefers_machine`,
  `crates/shelbi-core/src/model.rs`) is a persisted routing hint. Its doc string
  is explicit that it is "persisted only; enforcement (or override) is the
  orchestrator's choice", and there is no routing consumer today: `load_task_by_id`
  (`crates/shelbi-orchestrator/src/load.rs`) selects a workspace purely by
  required tags and busy state. So the acceptance criterion "`prefers_machine`
  routing respects runner availability" is forward-looking: whenever
  `prefers_machine` gains a real routing consumer, that consumer must be
  availability-aware from day one.

## The declaration

Add an optional `runner_kinds` capability list to `Machine`, mirroring the
existing `tags` field in both wire shape and inheritance.

```yaml
machines:
  - name: hub
    kind: local
    work_dir: /home/jlong/work
    runner_kinds: [claude, codex]     # both installed here
  - name: codex-box
    kind: ssh
    host: codex-box
    work_dir: /home/jlong/work
    runner_kinds: [codex]             # claude is NOT installed here
```

Rules:

- **Absent means "all kinds available".** An omitted `runner_kinds` on a machine
  declares no constraint, which is exactly the uniform-fleet assumption. Every
  existing project YAML keeps parsing and routing byte-for-byte, so there is no
  regression for uniform fleets (see the no-regression section below). This is
  the single most important semantic: the safe default is permissive, and a
  fleet only becomes constrained when an operator opts in by listing kinds.

- **Machine-level, inherited by slots.** Runner availability is a property of the
  box (what is on its PATH), not of an individual pane, so it belongs on
  `Machine`. Follow the `effective_tags` precedent: expose an
  `available_runner_kinds(workspace)` accessor on `Project` that returns the
  machine's declared set (or "all" when the machine declares none).

- **Optional per-workspace narrowing (only narrowing).** If a future need arises
  to fence a single slot to a subset of what its machine offers, allow an
  optional `runner_kinds` on `WorkspaceSpec` that *intersects* with the
  machine's set. A slot may never widen past its machine, the same
  ceiling-not-override asymmetry the permission clamp uses. Ship this only if a
  concrete need appears; the machine-level field covers the motivating case.

- **Validation.** Extend `Project::validate_workspaces` to reject a
  `runner_kinds` list that names a kind the project's fleet cannot launch (no
  `runners:`/`agent_runners:` entry resolves it via `runner_for_kind`), so a
  typo like `runner_kinds: [claud]` fails the load with a clear message instead
  of silently making every dispatch degrade.

## Dispatch consultation and graceful degradation

The launch path splits cleanly into a pure decision and an orchestrator-side
mapping; availability is consulted at the mapping layer where the target machine
is known.

1. `resolve_agent_launch` stays pure and machine-agnostic. It continues to
   resolve the *preferred* kind from the agent/project chain. It does not learn
   about machines; keeping it pure preserves its unit-testability.

2. `resolve_workspace_launch`
   (`crates/shelbi-orchestrator/src/workspace.rs`) gains the availability check,
   because it already receives the `WorkspaceSpec` and can reach the machine.
   The new step sits between "resolve the preferred kind" and "map kind to
   concrete launcher":

   - Compute the available set for the workspace's machine.
   - If the resolved kind is available, proceed unchanged. This is the uniform-fleet
     path and stays on the existing fast path.
   - If it is absent, **degrade by precedence** rather than failing at launch.
     The degradation candidate order is: the agent manifest's other
     `requires.runner_kinds` (and `runners:` blocks) intersected with the
     machine's available set, ranked by `CapabilityLadder` so a `Conventional`
     runner beats a `Degraded` one, then the project baseline kind if it is
     available. Selecting a different kind must pull that kind's own
     `(model, reasoning_effort)` from the manifest/project, never the absent
     kind's values, exactly as the existing `base_is_resolved_kind` guard
     already enforces for the missing-launcher case.
   - If nothing is available, **refuse with a clear message** naming the agent,
     the machine, the preferred kind, and the machine's declared set, for
     example: "agent `review` prefers runner kind `claude`, but machine
     `codex-box` declares `runner_kinds: [codex]` and no compatible fallback is
     available; install claude on `codex-box`, pin the agent to codex, or route
     this task to a machine that has claude." A clear refusal at dispatch is
     strictly better than an opaque launch failure on the remote host.

The `ResolvedWorkspaceLaunch` result may want to carry a note of *whether* it
degraded and from which kind, so the dispatch event log can record
"degraded claude->codex on codex-box" for observability. This is optional but
cheap and makes a heterogeneous fleet debuggable.

### Where the CapabilityLadder earns its keep

When degradation has more than one available candidate kind, rank them by their
`CapabilityLadder`. The ladder already encodes that Claude runs the fully
`Conventional` contract while Codex is `Degraded` on `message_delivery` and a
generic runner is `Degraded` throughout. A sensible ranking totals the ladder
(count of `Conventional` axes, or a weighted sum if message delivery matters
more for a given agent) and prefers the higher score, so a fleet that has both
`codex` and `generic` available for a claude-preferring agent degrades to
`codex` rather than `generic`. This is also the moment the per-kind manifest
`runners:` blocks (§2 of the plan) start earning their keep: on a uniform fleet
only the preferred block is ever consulted, but on a foreign fleet the fallback
block's `(model, reasoning_effort)` is what actually launches.

## prefers_machine routing respects availability

Two sub-cases, both to be handled when `prefers_machine` gains a real routing
consumer (it has none today):

- **When `prefers_machine` is honored as a routing input.** The selected
  workspace's machine must be able to run the task's resolved agent, or the
  preference is a soft miss: fall through to the next candidate machine rather
  than pinning the task onto a machine that will only degrade or refuse. In
  other words, `prefers_machine` proposes; availability disposes.

- **When `prefers_machine` conflicts with availability.** If the *only* machine
  that satisfies the preference cannot run the agent's kind and cannot degrade,
  surface the conflict explicitly (a dispatch-time warning or refusal) instead of
  silently ignoring the preference or silently degrading. The operator asked for
  a specific machine; if that machine cannot serve the role, that is worth
  saying out loud.

The routing helper that consumes `prefers_machine` should therefore take the
resolved agent kind (and its degradation candidates) as an input, so machine
selection and runner availability are decided together rather than in two passes
that can disagree.

## No regression for uniform fleets

The design is safe-by-default at every layer:

- A machine with no `runner_kinds` declares no constraint, so
  `available_runner_kinds` returns "all". Every existing project YAML is exactly
  this case, so parsing, routing, and launch are unchanged.

- The availability check in `resolve_workspace_launch` is a no-op when the
  resolved kind is in the available set, which it always is on a uniform fleet.
  The existing fast path is untouched.

- `prefers_machine` has no routing consumer today, so its availability rule adds
  nothing until that consumer is built.

A targeted test proves this: a project whose machines omit `runner_kinds` must
produce byte-for-byte the same `ResolvedWorkspaceLaunch` as today for every
agent/kind combination. Snapshotting that equivalence is the regression guard.

## Implementation touchpoints

Concrete places the implementation will land, so a future session does not have
to re-discover them:

- `crates/shelbi-core/src/model.rs`
  - Add `runner_kinds: Vec<RunnerKind>` to `Machine` (serde: `default`,
    `skip_serializing_if = "Vec::is_empty"`, and accept the scalar/bare-string
    shorthand via `de_string_or_seq` if a scalar form is wanted, matching
    `tags`).
  - Optionally add the same, narrowing-only, to `WorkspaceSpec` /
    `WorkspaceSpecRaw`.
  - Add `Project::available_runner_kinds(&WorkspaceSpec) -> BTreeSet<RunnerKind>`
    alongside `effective_tags`, returning the full set when the machine declares
    none.
  - Extend `Project::validate_workspaces` to reject a `runner_kinds` entry the
    fleet cannot launch.

- `crates/shelbi-core/src/agent_manifest.rs`
  - The pure resolver may grow a `resolve_agent_launch_with_availability` variant
    (or an `available: Option<&BTreeSet<RunnerKind>>` parameter) that applies the
    degradation-by-precedence and ladder ranking while staying pure and unit
    testable. Keep the machine lookup itself out of this module.

- `crates/shelbi-orchestrator/src/workspace.rs`
  - `resolve_workspace_launch`: consult availability for the workspace's machine,
    degrade or refuse, and reuse the existing `base_is_resolved_kind` discipline
    so a degraded launch never cross-injects the absent kind's model/effort.

- `crates/shelbi-orchestrator/src/load.rs` (and any future `prefers_machine`
  routing consumer)
  - Make machine/workspace selection availability-aware, threading the resolved
    agent kind and its degradation candidates into the choice.

## Acceptance criteria mapping

- *Machine/workspace declares which runner kinds are installed* -> the
  `Machine.runner_kinds` field (optionally narrowed per `WorkspaceSpec`), with
  absent meaning "all".
- *Dispatch consults availability and degrades (or refuses with a clear message)
  when a preferred kind is absent* -> the availability step in
  `resolve_workspace_launch`, degrading by manifest/project precedence ranked by
  `CapabilityLadder`, or refusing with a machine-and-kind-naming message.
- *`prefers_machine` routing respects runner availability* -> the routing
  consumer takes the resolved kind and its candidates as input; preference is a
  soft miss that falls through, and an unsatisfiable preference is surfaced, not
  silently ignored.
- *No regression for uniform fleets* -> absent `runner_kinds` = all kinds; the
  availability check is a no-op on the existing fast path; snapshot-equivalence
  test.

## Open questions to settle at implementation time

- **Refuse vs degrade default.** Should a missing preferred kind degrade silently
  (with an event-log note) or require an explicit `allow_degrade` opt-in per
  agent/project? The plan's language ("fall back per precedence or refuse")
  leaves this open. Recommendation: degrade by default with a logged note, since
  a degraded run is usually better than a stalled task, and reserve refusal for
  the "nothing available at all" case.

- **Ladder ranking function.** A flat count of `Conventional` axes is the simplest
  ranking; whether any axis (notably `message_delivery`) should be weighted for
  specific agent roles is a judgment call best made against a real foreign fleet.

- **Workspace-level narrowing.** Ship the machine-level field first; add
  per-workspace `runner_kinds` only if a concrete need appears, to avoid
  speculative surface area.
