# Generic Review via Workflow Primitives

**Status:** design decisions resolved with jlong 2026-07-03. Ready to break into
implementation tasks. Author: Orchestrator.

## Motivation

Review workspaces currently rely on a bespoke `review:` config block plus
special-cased loading code. That bakes a web-dev-server assumption (TCP ports +
HTTP readiness probe) into Shelbi's core, so review only fits web apps. Review
should instead be **composition of generic workflow primitives** so it works for
any project type and there is no special "review" machinery in the engine.

## The vision (jlong)

> A Review column in a workflow is simply a **Status** with settings to use a
> **workspace with a particular tag** (`review`), and a **transition rule** that
> runs a set of commands (install libraries, start a server), and even a
> **transition for teardown** (if needed) that also runs a set of commands.

## Current state (what exists today)

- **`ReviewConfig`** (`crates/shelbi-core/src/model.rs` ~L580), under the project's
  `review:` key: `base_port` (3000), `port_stride` (10), `setup: Vec<String>`,
  `serve: Option<String>`, `ready_probe: { http, timeout: 90s }`.
- **`WorkspaceSpec.role = review`** — the generic tag already exists on a workspace.
- **Transitions** (`transition.rs::execute_transition` / `run_action`): an edge's
  `actions` is a fixed enum set — `PushBranch, OpenPr, ClosePr, Merge,
  DeleteBranch, Restack`. No arbitrary commands.
- **Review loading** (`review.rs` + `workspace::load_review_workspace`): picks a
  free `role: review` slot, moves the branch onto its worktree, injects `$PORT`
  (`review_workspace_port` from base_port/stride), starts the `review` agent to
  run setup/serve/probe.
- Recently shipped: the **agent transition marker** (`.claude/shelbi-transition`)
  lets an agent trigger a transition, validated against the workflow's
  `transitions`. This plan extends transitions to *run commands* when they fire.

## Design (decisions locked)

### 1. Transitions run command sets, with a `$SLOT` env contract

An edge can run shell commands when it fires. The enter transition (into review)
runs setup + starts the server; the exit transition (out of review) runs teardown:

```yaml
statuses:
  - id: review
    owner: user
    workspace_tag: review          # route to a slot tagged `review`

transitions:
  - from: in-progress
    to: review
    run:                           # commands when the task ENTERS review
      - npm install
      - npm run dev -- --port $((3000 + $SLOT))   # server; holds the pane
    ready: curl -sf localhost:$((3000 + $SLOT))   # readiness: must exit 0
    ready_timeout: 90s
  - from: review
    to: done
    actions: [merge, delete_branch]
    run:                           # commands when the task LEAVES review (teardown)
      - <stop server / cleanup, if needed>
```

- **`$SLOT` env.** Each workspace carries a numeric **`slot`** key. Shelbi exposes
  it to every transition command as **`$SLOT`**; the command does its own math
  (`3000 + $SLOT` → 3001 for slot 1). Shelbi does NOT inject a computed `$PORT` —
  `$SLOT` is generic (ports, dirs, DB names, whatever the command needs).
  Collision-free concurrency without a bespoke serving config.
- Also inject `$SHELBI_TASK`, `$SHELBI_BRANCH`, `$SHELBI_WORKTREE`, `$SHELBI_MACHINE`.
- Commands run in the task's worktree, on the workspace's machine, **host-routed
  through `shelbi_ssh::run`** (works on devbox).
- **`ready` + `ready_timeout`.** After the enter commands, Shelbi polls the `ready`
  command until it exits 0 (or the timeout, default 90s), then marks the review
  serving/ready for the human. Generic — not HTTP-specific.
- **Teardown is not magic.** It is simply the `run:` commands on whatever exit
  transition the workflow declares (`review → done`, `review → in-progress` on a
  bounce, etc.). No auto-on-pane-close hook. (Transitions are the mechanism: a
  status change requires a declared transition; commands run when it fires.
  A `to: "*"` wildcard transition may be worth supporting so one teardown rule
  covers all exits — see Open items.)
- `actions` (git primitives) and `run` (shell) compose on one edge.
- **Serve lifecycle (Phase-1 detail):** the long-running server command holds the
  review agent's pane; Shelbi treats the transition as entered once the enter
  commands are launched and uses `ready` to decide when it's serving. Exact
  background/foreground mechanics to be nailed in Phase 1.

### 2. A Status requires a workspace tag/role

`workspace_tag: review` on a status routes it to a `role: review` pool slot. This
subsumes today's implicit "review status → role:review workspace" routing. `role`
on the workspace stays the generic tag; the status references it. Works alongside
the existing `agent:` field (the `review` agent still runs; the tag decides the slot).

### 3. Remove the bespoke `review:` block

Delete `ReviewConfig` / `ReadyProbe` (`base_port`, `port_stride`, `setup`, `serve`,
`ready_probe`). Its behavior is reconstructed by the workflow (enter `run` +
`ready` + `$SLOT`). `review.rs` / `load_review_workspace` stop reading `ReviewConfig`
and execute the status's enter-transition commands instead.

## Backward compatibility

- Existing `review:` blocks are **silently ignored** (like the removed
  `contextstore_sync` — tolerate the legacy key, don't act on it, don't migrate
  the user's file). Users opt into the new workflow form. No on-disk migration.
- Existing `role: review` workspaces keep working; they gain a `slot` number
  (default by declaration order if unset).

## Open items (smaller, resolve during implementation)

- **`to: "*"` wildcard transitions** — jlong noted a status change requires a
  declared transition. Consider a wildcard target so one exit rule (teardown) can
  cover all exits from a status. Confirm whether unlisted edges stay any-to-any or
  become "must be declared."
- **Serve foreground/background mechanics** (see §1) — how the long-running server
  is held and torn down.
- **Default `slot`** assignment when a workspace omits the key.

## Implementation phases

- **Phase 1 (app):** workflow schema — transition `run` + `ready`/`ready_timeout`,
  status `workspace_tag`, workspace `slot`; execute commands host-routed in
  `execute_transition` with the `$SLOT`/`$SHELBI_*` env; readiness polling. Tests:
  enter/exit ordering, host routing, env injection, ready timeout, failure short-circuit.
- **Phase 2 (app):** reroute review — `shelbi review` / `load_review_workspace`
  use the status tag + transition commands instead of `ReviewConfig`; remove
  `ReviewConfig`/`ReadyProbe`; ignore legacy `review:` blocks. Tests.
- **Phase 3 (docs):** rewrite `concepts/review-workspaces.mdx`,
  `configuration/workflow.mdx`, `guides/getting-started/review-workspaces.mdx` to
  the generic model; remove the `review:` block reference; document `$SLOT`,
  `run`/`ready`/teardown, `workspace_tag`, `slot`.

## Files likely touched

- `crates/shelbi-core/src/model.rs` — Transition (`run`, `ready`, `ready_timeout`),
  Status (`workspace_tag`), WorkspaceSpec (`slot`), remove `ReviewConfig`/`ReadyProbe`.
- `crates/shelbi-orchestrator/src/transition.rs` — run shell commands on an edge.
- `crates/shelbi-orchestrator/src/review.rs`, `workspace.rs` — load via tag +
  commands; `$SLOT`; readiness; remove ReviewConfig reads.
- `crates/shelbi-tui/src/poller.rs` — route a status to its tagged workspace.
- Docs as in Phase 3.

## Relationship to existing plans

- Supersedes the serving-model portion of `Plans/review-workspaces.md`.
- Builds on `Plans/workflow-transition-hooks.md` and the shipped agent send-back /
  transition-marker feature.