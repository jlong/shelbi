# Default Install Experience: Workflows and Agents

## Context

A fresh `shelbi init` today scaffolds a single `default` workflow (defined by
`default_workflow()` in `crates/shelbi-core/src/workflow.rs`, serialized by
`crates/shelbi-core/src/scaffold.rs`), a pool that includes one `review`-tagged
slot, and three agent instruction templates in `crates/shelbi-state/src/`:
`default_orchestrator.md.template`, `default_developer.md.template`, and
`default_review.md.template` (the review-workspace loader). That is a thin
starting point. We want the out-of-box experience to be opinionated and
immediately useful: two clear workflows, a working review gate on a review
workspace, and a set of ready-to-use specialized review agents the user can opt
into without writing prompts from scratch.

This plan defines the new shipped defaults. It does not add any ContextStore or
other tool-specific coupling to Shelbi (Shelbi stays plain-file, runner-agnostic).

## Decision Summary

1. Ship two default workflows: **`task`** (the default) and **`subtask`**.
2. `task` is normal branch work with a review gate: backlog to done through a
   `review` status served on a `review`-tagged workspace; branch off the default
   branch; squash-merge on accept.
3. `subtask` is the equivalent of the current `app-feature-subtask`: a piece
   of a parent `task`, done on its own branch, squash-merged into the parent
   task's branch with no review. It names its parent via frontmatter and
   resolves `base_branch: task/{{task}}` to the parent's branch.
4. `default_workflow: task` in the scaffolded project config.
5. The default workflows wire only three agents: **Orchestrator**, **Developer**,
   and **Reviewer**. Reviewer is the agent that stands up the review workspace
   (installs/builds, boots the dev server for a human to inspect).
6. Ship three additional specialized agent presets that are available but NOT
   assigned by the default workflows: **QA**, **Security**, and **Adversarial
   Review**. A user opts one in by assigning it to a column via per-status
   `agent:` (or `shelbi agent`), without having to author the prompt.
7. The default project scaffold provisions a `review`-tagged workspace so the
   `task` review status has somewhere to run.
8. Mapping to today's repo workflows: `task` is the equivalent of `app` and
   `subtask` is the equivalent of `app-feature-subtask`. There is NO separate
   umbrella workflow (no `app-feature` analog): a `task`'s own branch is the
   long-lived parent that subtasks merge into, and the `task`'s review is the
   single gate that merges the whole thing to main.

## The two workflows

Both use the fixed five-category vocabulary (backlog, ready, active, handoff,
done) with the default status labels. Only the review gate differs.

### `task` (default) — branch work with review

- **statuses**: `backlog` (owner user), `todo` (ready), `in-progress`
  (developer), `review` (handoff; requires the `review` tag, boots a server via
  the review agent), `done`, `canceled`.
- **git**: `base_branch: main`, `branch_prefix: task` (a task's branch is
  `task/<id>`, which is what subtasks target), `merge_strategy: squash`.
- **transitions**:
  - `in-progress -> review` : `[push_branch, open_pr]`
  - `review -> done` : `[merge, delete_branch]`
  - `in-progress -> canceled` / `review -> canceled` : `[close_pr, delete_branch]`
- This is the current shipped `default` workflow (equivalent to the repo's
  `app`), renamed and kept as the default. The `review` status routes to a
  `review`-tagged workspace where the Reviewer agent serves the branch for
  human accept/bounce.
- A `task` doubles as the parent branch when it has subtasks: subtasks merge
  into `task/<id>`, and the task's review merges the accumulated work to main.
  No umbrella workflow is needed.
- **One PR.** A `task` opens exactly one pull request, via `push_branch` +
  `open_pr` on `in-progress -> review`. That PR is the review surface and, when
  accepted (`review -> done` : `merge`), lands on main carrying the task's own
  work plus every subtask that merged into its branch. Subtasks add to this PR;
  they never open their own.

### `subtask` — piece of a parent task, no review (equivalent of `app-feature-subtask`)

- **statuses**: `backlog`, `todo`, `in-progress`, `done`, `canceled` (no
  `review`).
- **git**: `base_branch: task/{{task}}`, `merge_strategy: squash`. The
  `{{task}}` var is interpolated from the subtask's `task:` frontmatter field
  (the parent task's id), so the subtask branches from and squash-merges into
  the parent task's branch, never into main directly.
- **transitions**:
  - `in-progress -> done` : `[merge, delete_branch]` (a direct local squash-merge
    into the parent task's branch)
  - `in-progress -> canceled` : `[delete_branch]`
- **No PR.** A subtask never opens a pull request. Its actions are `merge` +
  `delete_branch` only (no `push_branch`/`open_pr`), so its work merges straight
  into the parent task's branch and surfaces only on the parent task's PR. There
  is exactly one PR per `task` (opened when the task enters `review`), and it
  carries the accumulated work of all its subtasks.
- Handoff-less: the hub poller already advances a finished worker on a
  workflow with no handoff status straight to done (shipped), firing the merge
  into the parent branch.
- Convention: file a subtask with `task: <parent-task-id>` in its frontmatter.
  Only one subtask per parent branch runs at a time (git allows a branch in a
  single worktree); the orchestrator queues them.
- Requires the templated `base_branch` (`{{var}}`) resolution to be honored at
  dispatch. NOTE: there is a known open bug where dispatch ignores a workflow's
  templated `base_branch` and cuts from the default branch instead
  (bug-dispatch-ignores-templated-base-branch). That MUST be fixed for `subtask`
  to work as a shipped default.

## Agents

Ship six agent instruction presets. Three are wired into the default workflows;
three are available for the user to opt in.

### Wired by default

- **Orchestrator** — the scheduler/lead (`default_orchestrator.md.template`).
- **Developer** — executes tasks on a dev workspace
  (`default_developer.md.template`).
- **Reviewer** — the review-workspace loader: on a task entering `review`, it
  checks out the branch, installs/builds, and boots the dev server on a real
  port for a human to click through (`default_review.md.template`). This is the
  agent that "sets up the review workspace." It does not scrutinize the diff;
  it stands up the running app.

### Shipped but not wired (opt-in specialized reviewers)

These scrutinize a task's diff and can govern a column the user adds. Each ships
as `agents/<id>/instructions.md` in the scaffold so it is present and
discoverable, but no default-workflow status references it.

- **QA** (`qa`) — exercises the change against acceptance criteria, looks for
  regressions, missing tests, broken edge cases; reports pass/fail with repro.
- **Security** (`security`) — reviews the diff for security issues (injection,
  authz, secrets, unsafe deserialization, supply-chain), scoped to defensive
  review.
- **Adversarial Review** (`adversarial`) — tries to refute the change: hunts
  for correctness bugs and unhandled cases, defaulting to skeptical.

A user wires one in by adding a status (e.g. a `qa` column between in-progress
and review) with `agent: qa`, or by pointing an existing status at it. Document
the one-liner in the getting-started docs.

## Default project scaffold

`shelbi init` / first run should produce:

- **Pool**: N dev workspaces plus at least one `review`-tagged workspace on the
  hub (so `task`'s review status can run). Keep the current `review`-tag slot
  from `scaffold.rs`.
- **Workflows**: write `workflows/task.yaml` and `workflows/subtask.yaml`; set
  `default_workflow: task` in the project config.
- **Agents**: scaffold all six `agents/<id>/instructions.md` files (three wired,
  three available). The three specialized presets ship as static templates
  alongside the existing three in `crates/shelbi-state/src/`.
- Keep the self-documenting commented-optional config style already used by the
  scaffolder.

## Positioning / docs impact (must-do follow-up)

Shipping QA, Security, and Adversarial Review as presets CHANGES the current
honesty caveat in `Product/positioning.md` (and the live site), which says those
agents are "built with shipped primitives, not shipped presets." Once this
lands, that is no longer true: they come in the box. Update:

- `Product/positioning.md` fact-check note and the value-prop framing.
- Site `ValueProps` triad item 2 ("Agents provide specialization") to say the
  reviewers ship, not just that you can add them.
- Getting-started / install docs: show the default `task`/`subtask` split and
  the one-liner to wire in QA/Security/Adversarial.

## Migration for existing projects

Projects created before this change carry a `default` workflow and
`default_workflow` may be unset or `default`. Options (pick one in
implementation):

- Ship `task` as the new default and treat `default` as an alias that resolves
  to `task`, so existing tasks keep loading; or
- Add a one-time migration (like `migrate.rs`) that renames `default` to `task`
  and writes `subtask.yaml`, leaving task frontmatter that says `workflow: default`
  working via the alias.

Do not break existing boards. Prefer the alias plus a migration that adds the
new files without rewriting user task frontmatter.

## Implementation tasks

1. Define `task` (rename/replace the current `default_workflow()`) and add
   `subtask` as shipped workflow definitions; update `scaffold.rs` to write both
   and set `default_workflow: task`.
2. Add the three specialized agent templates
   (`default_qa.md.template`, `default_security.md.template`,
   `default_adversarial.md.template`) and constants in `agent_workspaces.rs`;
   scaffold writes all six agent instruction files.
3. Keep Reviewer wired to the `review` status; leave QA/Security/Adversarial
   unassigned.
4. Onboarding wizard: reflect the new defaults (two workflows, review workspace,
   six agents) in prompts and summary.
5. `default`-workflow alias + migration for existing projects.
6. Docs + site copy: new default flow, opt-in reviewers, updated positioning
   caveat.
7. Tests: scaffold produces both workflows, `default_workflow: task`, six agent
   files, a review-tagged slot; `task` review gate and `subtask` handoff-less
   auto-advance both work.

## Open decisions

- **`subtask` base branch: RESOLVED.** `subtask` is the equivalent of
  `app-feature-subtask`: it branches from and merges into the parent `task`'s
  branch (`base_branch: task/{{task}}`), not main, with no review. A `task`
  plays the parent/umbrella role; there is no separate umbrella workflow.
- **Prerequisite bug:** dispatch must honor templated `base_branch`
  (bug-dispatch-ignores-templated-base-branch) before `subtask` can ship as a
  default, otherwise subtask branches get cut from main instead of the parent.
- **Number of default review workspaces** (recommend 1 on the hub).
- **Agent prompt content** for QA/Security/Adversarial (needs authoring; keep
  each tight and single-purpose).
- **Naming**: `task`/`subtask` as the shipped names (vs the repo's current
  `app`/`app-feature-subtask`), and whether `adversarial` or `adversarial-review`
  is the agent id.
- **Runner defaults** for the specialized agents (inherit project runner).