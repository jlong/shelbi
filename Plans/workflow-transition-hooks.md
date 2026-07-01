# Workflow Transition Hooks

## Context

Today the `transitions:` block in a workflow YAML (see [[workflows]] §12) accepts a fixed vocabulary of built-in git actions — `open_pr`, `merge`, `delete_branch`. That covers the canonical code-review pipeline, but not:

- **Local gates.** "Run `cargo test` before I let this task leave `in-progress`" is a common ask that today lives in Zen Mode's per-workflow `checks:` block — a Zen-only mechanism that fires only during auto-merge, not on manual transitions.

- **Worker-side skill invocations.** "Before you write the review marker, invoke the `polish-pass` skill" or "on entering `research`, run the `deep-research` skill on the task description" — steps that require Claude-side reasoning, not a shell exec.

- **Custom shell steps.** Building a docs preview on entering `staging`, rendering a Figma snapshot, uploading a build artifact — none of that fits the built-in enum.

**Transition hooks** generalize the `transitions:` block with two new primitives — **shell hooks** (executed by the Rust runner, default on the worker's machine) and **skill hooks** (injected as prose into the worker's instructions at dispatch, invoked by the worker itself). Both sit alongside the existing built-in `actions:` and gate the transition — failure blocks. Zen Mode's existing `checks:` block collapses into shell hooks so there's one source of truth for "things that happen during a transition."

## Design

### 1. Two primitives with different execution models

The core insight: shell and skill hooks look similar in YAML but run in fundamentally different places.

- A **shell hook** is a command the Rust runner execs. Default locus is `on: worker` (SSH into the assigned workspace's worktree). Non-zero exit blocks the transition. Runner has full control and enforcement.

- A **skill hook** is a Claude Code skill name. The Rust runner cannot invoke skills — skills live in the Claude session. Instead, at dispatch time, the runner appends a fenced instruction block to the worker's task-body / prompt saying: *"Before signaling completion, invoke skill* *`<name>`. If it reports failure, do not write the review marker."* The worker is trusted to comply. There is no runtime enforcement, only convention plus the fact that a skill that reports failure normally emits a signal the worker can see and act on.

Because they run in different places, they're only nominally "the same list" — the runner branches on the step type when planning the transition.

### 2. Schema extension

The current schema:

```yaml
transitions:
  - from: in-progress
    to: review
    actions: [push, open_pr]
```

Becomes:

```yaml
transitions:
  - from: in-progress
    to: review
    run:
      - name: cargo test
        cmd: cargo test --workspace

      - name: cargo clippy
        cmd: cargo clippy --workspace --all-targets -- -D warnings

      - name: polish pass
        skill: polish-pass

    actions: [push, open_pr]
```

One `run:` list, ordered, every entry a gate. Steps execute in the order listed; the first non-zero exit (or worker-reported skill failure) aborts the transition. `actions:` fire only if every step in `run:` succeeded.

If you want a step that shouldn't gate (a notification you don't want to block on), wrap it in shell semantics: `cmd: ./scripts/slack-notify.sh || true`. Explicit, and keeps the schema strict.

### 3. Step schema

Every step in `run:` accepts:

| Field      | Type     | Default    | Notes                                                                                                                                                              |
| ---------- | -------- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `name`     | string   | *required* | Human label; surfaces in events (`reason=hook:failed step=<name>`), CLI output, TUI activity feed.                                                                 |
| `cmd`      | string   | see below  | Shell command. Mutually exclusive with `skill`. Runs in `sh -c` (POSIX; not the user's login shell) for portability. `$VAR` expansion only against injected env.   |
| `skill`    | string   | see below  | Name of a Claude Code skill (matches the skill name in `~/.claude/skills/` or a project skill). Mutually exclusive with `cmd`.                                     |
| `on`       | enum     | `worker`   | For shell steps only: `worker` (SSH into the assigned workspace's machine, cwd = worktree) or `hub` (runs where the orchestrator lives, cwd = project root).       |
| `parallel` | bool     | `false`    | For shell steps only: if two consecutive shell steps both set `parallel: true`, they run concurrently. Skill steps cannot be parallelized (they're prose). See §5. |
| `timeout`  | duration | `10m`      | For shell steps only. Kill and treat as failure after this duration.                                                                                               |

Exactly one of `cmd` or `skill` must be set on each step. Only `name` and the `cmd`/`skill` field are required.

### 4. Execution context

**Shell steps.** Every step gets these env vars, populated by the runner before spawning:

| Variable                | Value                                                      |
| ----------------------- | ---------------------------------------------------------- |
| `SHELBI_TASK_ID`        | Task's stable id                                           |
| `SHELBI_TASK_TITLE`     | Task title                                                 |
| `SHELBI_TASK_PATH`      | Absolute path to the task's markdown file (on the hub)     |
| `SHELBI_BRANCH`         | Task's `branch:` frontmatter (may be empty if not yet cut) |
| `SHELBI_WORKSPACE`      | Assigned workspace name (may be empty)                     |
| `SHELBI_WORKSPACE_HOST` | The workspace's machine name (may be empty)                |
| `SHELBI_WORKFLOW`       | Workflow name (e.g. `default`, `app-feature`)              |
| `SHELBI_FROM_STATUS`    | Source status id                                           |
| `SHELBI_TO_STATUS`      | Target status id                                           |
| `SHELBI_FROM_CATEGORY`  | Source category (`backlog`, `ready`, `active`, etc.)       |
| `SHELBI_TO_CATEGORY`    | Target category                                            |
| `SHELBI_TRIGGER`        | `user`, `orchestrator`, `zen`, `worker`                    |
| `SHELBI_PROJECT`        | Project name                                               |

For `on: worker` steps, `SHELBI_TASK_PATH` points to the hub path — steps that want the task body inside the worktree should read it locally from the checked-out branch or fall back to `ssh hub cat "$SHELBI_TASK_PATH"`. Env vars are set on the remote side via the SSH invocation.

**Skill steps.** Injected as prose at dispatch time. The exact text lives in `agents/_shared/hooks-instructions.md` (a new file, template-substituted per project) so it's editable. Rough shape:

```
### Transition-hook skills for this task

Before you signal completion (write the review marker), invoke each of the
following skills in order. If a skill reports failure, address the failure
and re-run it before proceeding. Do not write the review marker until all
transition-hook skills pass.

1. `polish-pass`
2. `docs-check`
```

The instruction block is appended to the task body the worker sees, gated by the transition being entered. Which skills fire on which transition is determined at dispatch time (dispatch = `todo → in-progress`, so the skills declared on the `in-progress → review` transition are the ones the worker needs to do *before* writing the review marker).

### 5. Ordering and parallelism (shell only)

Shell steps run sequentially by default. To run a batch concurrently, mark each step in the batch `parallel: true`; adjacent parallel steps form a group. A non-parallel step (or a skill step) ends the group and acts as a barrier.

```yaml
run:
  - name: build
    cmd: cargo build --workspace

  - name: test
    cmd: cargo test --workspace
    parallel: true

  - name: clippy
    cmd: cargo clippy --workspace --all-targets -- -D warnings
    parallel: true             # test + clippy run concurrently

  - name: polish
    skill: polish-pass         # barrier: skill steps never parallelize
```

Skill steps are always sequential and are executed by the worker in order, one after the next.

Within a parallel shell group: all steps start together, the group succeeds only if all succeed, and any failure fails the whole transition (other in-flight steps run to their `timeout` and their results are captured; no cancel-on-first-failure in v1).

### 6. Failure semantics

Every step is a gate. There is no `gate:` field.

- **Shell step**, non-zero exit or timeout → transition aborted. Task stays in source status. Emit `task=<id> hook=failed step=<name> exit=<code> reason=hook:failed`. Any already-completed steps' side effects (files touched, commits landed) are **not** rolled back. The runner cannot undo work that already happened; the plan structure (gates before `actions:`) minimizes the damage.

- **Skill step**, worker reports failure → this is a soft contract. The worker sees the instruction to invoke the skill and to hold off on the review marker if it fails. If the worker misbehaves and writes the review marker anyway, the transition proceeds — the runner has no way to know. Emit `task=<id> hook=skill-dispatched step=<name> reason=hook:skill-issued` at dispatch time as an audit trail; a follow-up plan could add a worker-emits-skill-result marker for enforcement.

Output capture (shell): stdout + stderr stream to the orchestrator's activity feed prefixed with `[<step-name>] ` ; the final \~4KB tail is included in the failure event so the orchestrator can surface the reason to the user without re-running.

**Feedback loop on worker-initiated transitions.** If a shell gate fails on a transition triggered by a worker's review marker, the runner emits the failure event and additionally sends a follow-up message to the worker's tmux pane summarizing the failure (which step, exit code, tail of output). The worker is expected to address the failure and re-signal. Under Zen, the orchestrator's reaction rule re-dispatches with the failure context automatically; outside Zen, the user sees it in the activity feed and decides.

### 7. Zen Mode migration (one-shot refactor)

Zen's current per-workflow `checks:` block (see `shelbi zen status`) becomes shell hooks in the workflow YAML — no separate code path.

Today:

```yaml
# zen config (per-workflow override)
checks:
  - cargo build --workspace
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
```

After (as workflow YAML):

```yaml
# workflows/app.yaml
transitions:
  - from: in-progress
    to: review
    run:
      - name: build
        cmd: cargo build --workspace

      - name: test
        cmd: cargo test --workspace

      - name: clippy
        cmd: cargo clippy --workspace --all-targets -- -D warnings

    actions: [push, open_pr]
```

Zen's merge-conditions probe (`shelbi zen probe`) reads the resolved workflow's `run:` gates for the target transition instead of a separate list. What Zen still owns:

- The **judgment layer** — deciding when to *run* the transition (auto-promote / auto-merge policy).

- The **cross-cutting probes** — merge-conflict check, diff-size limit, danger-path check, CI watch. These aren't per-transition hooks; they're global merge guards. They stay as Rust code invoked by Zen only.

The `zen.yaml` schema drops `checks:` entirely. This is a breaking config change; the migration path is documented in the release notes with a one-line warning from `shelbi zen status` on first run pointing to the new location. No compat shim.

### 8. Manual transitions vs Zen

Hooks run on **every** transition matching a declared edge, not just Zen-driven ones. This is deliberate:

- `shelbi task move <id> --to review` from the CLI runs `run:` gates and blocks on failure.

- The TUI moving a card via drag-and-drop hits the same code path.

- The poller moving a card to `handoff` on a worker's review marker runs the same gates.

- Zen's auto-merge is just another caller — no special treatment.

The only difference under Zen: it makes the *decision* to invoke a transition automatically. The execution is the same code.

For manual transitions where the user wants to bypass a gate (rare, but possible — the gate is broken, or the code review is out-of-band): `shelbi task move --skip-hooks` emits `reason=hook:skipped` and lands the transition. Zen never accepts `--skip-hooks`; when Zen sees a broken gate it leaves the task where it is and surfaces the reason.

### 9. `shelbi status --full` interaction

Independent of this plan, [[shelbi-status-full-single-bootstrap-snapshot-for-the-orchestrator]] adds `shelbi status --full` for orchestrator bootstrap. Once hooks land, that command renders the resolved hooks per workflow — same command, richer output. Sequencing: `shelbi status --full` ships first and then absorbs hook rendering when hooks land; nothing about hooks blocks it.

### 10. Rollout

Small enough to land in one sequence of tasks:

1. **Schema + parser.** Extend the workflow loader to accept `run:` under transitions with the field set from §3. Validation: required fields, exactly-one-of `cmd`/`skill`, no `on:`/`parallel:`/`timeout:` on skill steps (or accept-and-ignore with a warning — pick when implementing).

2. **Shell runner.** New module (or extend `shelbi-orchestrator`) that takes a resolved transition + task + trigger context and executes shell steps in order. `on: worker` via `shelbi-ssh`. Streams output to the activity feed. Emits `hook:started`, `hook:passed`, `hook:failed` events. Barrier + parallel-group semantics.

3. **Skill instruction injection.** At dispatch (`todo → in-progress`), the runner reads the *next* transition (`in-progress → review` by convention) and appends the skill-instructions block to the task body the worker sees. Template lives in `agents/_shared/hooks-instructions.md`.

4. **Wire into every mover.** `shelbi task move`, TUI card moves, poller-driven marker moves, Zen auto-moves — all route through the runner. This is the invasive step; needs a careful audit of every code path that mutates a task's status.

5. **Feedback loop.** When a shell gate fails on a worker-initiated transition, the runner sends a follow-up message to the worker's tmux pane. New helper in `shelbi-tmux`.

6. **Migrate Zen.** Drop `checks:` from `zen.yaml`. Zen's `probe` reads the resolved workflow's `run:` for the target transition. Update `shelbi zen status` output. Update docs. Add a one-shot migration warning that fires when a legacy `checks:` block is detected in `zen.yaml`.

7. **Docs + example.** Update `Plans/workflows.md` §12 with the extended schema. Worked example in the docs site.

Each of these is a filable task.

### 11. Open questions (unresolved)

- **Should shell hooks be primarily worker-executed (via skills) or runner-executed?** This is an unresolved axis that could invert the design of §1–§7. The plan as written treats runner-executed shell (`on: worker` via SSH) and worker-executed skills as peers, both first-class. The counter-argument: any gate that requires *iteration* (fix → re-run → fix) should belong to the worker, because the worker has the state in-context and the judgment to decide whether a failure is real or spurious. A runner-executed gate that fires after the worker signals done creates an adversarial loop — reject the marker, re-dispatch with a stale worker who has to re-hydrate before fixing. If we accept that, the primitive picture shifts:

  - **Skill hooks become the primary tool** for every "before you signal done, make sure X" — tests, lints, builds, type-checks, docs coverage. Injected into the worker's task instructions at dispatch. Iteration happens in-context.

  - **Runner-executed shell hooks shrink to a narrow niche**: non-iterative, credentials-required, hub-side. Notifications on transition (Slack, Linear), analytics writes, cross-project pings. Things the worker legitimately can't or shouldn't do.

  - **`on: worker`** **for shell disappears.** Shell is always `on: hub`. Anything that needs the worktree becomes a skill.

  - **Zen's** **`checks:`** **do** ***not*** **collapse into workflow hooks.** They serve a different purpose: runner-verified gates specifically because Zen auto-merges without user review, where trusting the worker's judgment isn't enough. They stay Zen-only, runner-executed shell — a distinct layer with a different trust model, not duplication.

  - **Trade-off.** Skill hooks are a soft contract. If a worker misbehaves and signals done without actually running the tests, we don't catch it at the workflow layer — only at the Zen layer (which won't merge without its own runner-verified checks). Outside Zen, the user reviews the branch and catches it there. We'd be choosing "trust + review" over "enforcement" at the workflow layer, on the theory that Shelbi's architecture already has the user in the loop for non-Zen work.

  If we invert: §1's "two primitives with different execution models" stays, but the emphasis flips — skills are primary, shell is secondary and hub-only. §2's schema example loses `on: worker` from most steps. §5 (ordering/parallelism) simplifies — skills sequential by nature, shell only runs concurrently on the hub. §6's feedback-loop-to-worker-on-gate-failure goes away entirely (no worker-affecting gates to fail). §7's Zen migration reverses — Zen keeps `checks:` because they're a different trust layer. Rollout in §10 loses the "wire shell runner + SSH" step for the primary path.

  **Decision needed** before implementing §1–§10.

### 12. Deferred follow-ups (out of scope for v1)

- **Named reusable blocks.** Explicitly out of scope for v1 per the design discussion. Revisit once we've seen ≥2 workflows duplicate the same 3+ step block.

- **Skill-result enforcement.** In v1, skill hooks are a soft contract with the worker. A follow-up could add a worker-emitted marker for each skill invocation with a pass/fail signal that the runner reads before completing the transition. Needs cooperation from the worker's runtime.

- **Per-status hooks (`on_enter`** **/** **`on_exit`).** Out of scope in v1 — per-transition covers the cases we know about. If we see many edges converging on one status with duplicated hook lists, revisit.

- **Conditional hooks.** No `if:` predicate in v1. Users can gate steps by writing `sh -c "test <cond> && <cmd>"` (with `|| true` if they don't want a false condition to gate). If a common pattern emerges (e.g. "only run when the task has label X"), add first-class support then.

- **Retry.** No built-in retry on gate failure. If a flaky test is the reason, the fix is to stabilize the test.

- **Cross-transition state.** Steps can't share output with later steps except via the filesystem (worktree files). Keeps the model simple; punt to filesystem when needed.

## Acceptance signals

- A workflow YAML with `run:` under a transition parses, validates (exactly-one-of `cmd`/`skill`), and executes in order before the built-in `actions:`.

- A failing shell gate blocks the transition; task stays in source status; event surfaces the failing step + stderr tail.

- On a worker-initiated transition, a failed gate results in a tmux follow-up message to the worker.

- `on: worker` shell steps execute in the worktree via SSH.

- `parallel: true` runs adjacent shell steps concurrently with a barrier on the next non-parallel step (or on any skill step).

- At dispatch, the worker's instructions include the skill-hooks for the *next* transition, in order, with clear language about withholding completion on failure.

- Zen's `probe` reads workflow `run:` gates instead of the legacy `checks:` block; `zen.yaml` no longer accepts `checks:`; a one-shot migration warning fires when the legacy block is detected.

- `shelbi status --full` renders the resolved hooks for each workflow (partial dependency on the status-full task landing first).
