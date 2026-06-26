# Workflows

## Context

Today Shelbi has **exactly five statuses, hardcoded** everywhere — `Backlog`, `Todo`, `InProgress`, `Review`, `Done`. Every project gets the same five. The columns appear in the Kanban TUI, drive the orchestrator's reaction rules, are baked into the events log line shapes, and assumed by Zen Mode's "auto-merge when something hits review" logic. Adding a sixth status (e.g., a "QA" lane between Review and Done) requires a code change.

That model fits a single-track *one user + agents* workflow well, but it can't model:

- A **design + build + review** pipeline where designs land in a "Design Review" column for the user before they hit a worker as buildable Todo.

- A **multi-stakeholder** flow where docs tasks have a "Tech Review" column the user owns and code tasks don't.

- An **agent-only** internal workflow (e.g., a research pipeline) that runs without ever surfacing in the user's primary board.

**Workflows** generalizes the status set per project. The defaults stay, but now they're one workflow definition among many. A workflow is a named YAML file declaring its statuses, who owns each one (user vs agent), and how work moves between them. Tasks are assigned to a workflow (default if unassigned). Multiple workflows can coexist; the TUI and orchestrator respect each task's workflow when rendering and routing.

The five hardcoded statuses plus a new `Canceled` lane (mapping to the `archived` category) become the **default workflow** — one definition file that ships with every new project. Adding a new workflow is a YAML edit. Custom workflows can introduce custom statuses, but every status maps to a **status category** (the underlying semantic) so generic code (Zen Mode auto-merge, activity feed rendering, "what does this column mean?") still works across arbitrary status names.

## Design

### 1. Status categories (the semantic floor)

Every status — default or custom — belongs to one of six categories. Categories are the vocabulary the rest of the system speaks; status *names* are user-facing labels.

| Category   | Semantic                                                                    | Default-workflow status using it | Example custom names                  |
| ---------- | --------------------------------------------------------------------------- | -------------------------------- | ------------------------------------- |
| `backlog`  | Not yet ready for work — triage stage.                                      | `Backlog`                        | `Inbox`, `Triage`                     |
| `ready`    | Ready to be picked up by whoever owns it.                                   | `Todo`                           | `Ready`, `Next Up`                    |
| `active`   | Owner is working on it now.                                                 | `InProgress`                     | `Building`, `Designing`, `Drafting`   |
| `handoff`  | One owner has finished their part; another's input is required next.        | `Review`                         | `QA`, `Awaiting Sign-off`             |
| `done`     | Terminal — accepted, shipped.                                               | `Done`                           | `Shipped`, `Released`                 |
| `archived` | Terminal — closed without shipping (cancelled, won't fix, duplicate, etc.). | _(none)_                         | `Cancelled`, `Won't Fix`, `Duplicate` |

Generic code reasons in categories, never in literal status names:

- Zen Mode's auto-merge trigger: *"when a task transitions to any* *`handoff`* *status whose next owner is the user, evaluate the merge-conditions flow."* Transitions into `archived` never trigger merge.

- Activity feed renderers: *"a* *`ready -> active`* *transition reads as 'started.'"* An `active -> archived` transition reads as *"dropped"* or *"closed without shipping."*

- Orchestrator auto-dispatch: *"a task in a* *`ready`* *status whose owner is* *`agent`* *is dispatchable."* Tasks in `done` or `archived` (both terminal) are never dispatched.

A workflow can repeat a category — e.g., a long pipeline might have three `active` statuses (Design, Build, QA) all in the same category but with different owners.

### 2. Workflow YAML schema

Each workflow lives at `~/.shelbi/projects/<project>/workflows/<workflow-name>.yaml`. Schema:

```yaml
name: default              # filename minus .yaml; used in task frontmatter
description: |             # surfaces in CLI listings + TUI workflow picker
  The standard one-track flow shipped with every project.

statuses:
  - name: Backlog          # display label
    category: backlog
    owner: user            # user | agent
    description: |
      Untriaged work. The user moves things into Todo when ready.

  - name: Todo
    category: ready
    owner: agent

  - name: InProgress
    category: active
    owner: agent

  - name: Review
    category: handoff
    owner: user            # user reviews; next owner is user

  - name: Done
    category: done
    owner: user

  - name: Canceled
    category: archived
    owner: user

# Optional: which status a new task lands in by default. If absent, the
# first status in the list is used.
initial_status: Backlog

# Optional: restrict allowed transitions. If absent, any-to-any is allowed.
# allowed_transitions:
# Backlog: [Todo]
# Todo: [InProgress, Backlog]
# InProgress: [Review, Todo]
# Review: [Done, Todo, InProgress]
# Done: []

# Optional: git-side behavior and per-edge actions. See §12 for the full
# action set, semantics, and worked scenarios. Omitting both blocks gives a
# pure bookkeeping workflow (no git side-effects).
git:
  base_branch: main
  merge_strategy: squash

transitions:
  - from: InProgress
    to: Review
    actions: [push_branch, open_pr]

  - from: Review
    to: Done
    actions: [merge, delete_branch]

  - from: any
    to: Canceled
    actions: [close_pr, delete_branch]
```

### 3. Default workflow

Every new project ships with `workflows/default.yaml` containing the six-status definition above. This makes the default an editable file rather than implicit behavior — the user can immediately customize names (e.g., rename `Todo` to `Ready`) without breaking anything.

If a project has no `workflows/` directory at all (legacy state), shelbi auto-generates `default.yaml` on first load and migrates existing tasks to reference it.

### 4. Multiple workflows per project

A project can have any number of workflow files. Examples:

```
~/.shelbi/projects/myapp/
  workflows/
    default.yaml         # the standard 5-status flow
    research.yaml        # agent-only research pipeline: backlog → reading → drafting → done
    design-review.yaml   # 6-status pipeline with a user-owned QA step before done
```

Workflows are **configuration**, not a UI surface — they're authored as YAML and surfaced only through (a) the `shelbi workflow ...` CLI (§9) and (b) a **workflow filter** on the existing Tasks Kanban board (§7). There is no dedicated "Workflows" view in the TUI.

**By default the filter is** **`All`** **— every task across every workflow is visible.** In `All` mode the column set is the six **status categories** from §1 (Backlog / Ready / Active / Handoff / Done / Archived), since every status in every workflow maps to one of them. Cards from different workflows can sit in the same column; a small workflow label on each card (Phase 2) tells them apart. Selecting a specific workflow from the filter swaps the columns to that workflow's native status names and hides tasks from other workflows.

### 5. Per-task workflow assignment

Tasks declare their workflow in frontmatter:

```yaml
---
id: design-the-thing
title: Design the thing
workflow: design-review        # optional; defaults to "default" if omitted
column: Designing              # uses a status from the assigned workflow
---
```

If `workflow` is absent, the task runs the default workflow. If `workflow` references a missing definition, shelbi falls back to default and logs a warning to the events feed.

### 6. Owner semantics

`owner: user | agent` declares who is *expected* to act when a task is in that status:

- **`agent`** → eligible for auto-dispatch. The orchestrator picks free workers matching any `prefers_machine` constraint.

- **`user`** → the orchestrator does NOT dispatch; the task waits for a user action. Surfaced in the activity feed + sidebar so the user knows their attention is requested.

There is no third "either" value — a task is either work for a worker or work for the user. If the user wants to grab an agent-owned task, they reassign it through the normal CLI/TUI; no schema field needed for that.

In Zen Mode (see §8), the owner determines what counts as "ready for auto-merge" — a transition from an `agent`-owned `active` status to a `user`-owned `handoff` status is the canonical auto-merge trigger.

### 7. TUI rendering

The Tasks Kanban board gains a **workflow filter** at the top:

```
Kanban  ·  [Workflow: All ▾]                                  (q to quit, ? for help)
```

The filter defaults to **`All`** — every task across every workflow is visible, with columns drawn from the six status categories (Backlog / Ready / Active / Handoff / Done / Archived). Selecting a specific workflow switches columns to that workflow's native status names and narrows the cards to only that workflow's tasks. There is no separate "Workflows" view; workflows are configuration (see §4).

**Every column is always shown**, in declaration order — the full structure stays visible at a glance, regardless of where work currently sits. Empty columns aren't hidden; they're collapsed. The same rules apply in both modes: in `All`, the columns are the six status categories; in a workflow-specific filter, they're that workflow's declared statuses.

- Columns with one or more tasks render as **full-width columns** (card list inside).

- Columns with **no tasks collapse to a thin vertical strip** \~3 cells wide, with the column name written top-to-bottom (one character per row). The strip spans the full board height so the column rail stays visually aligned. Cards can still be dropped onto a collapsed strip; moving the cursor onto one expands it temporarily until focus leaves.

- When the combined width of expanded columns exceeds the terminal, the board **scrolls horizontally**. Left/right arrows (or `h`/`l`) move the cursor between columns; the visible window scrolls to keep the cursor in view. The workflow filter, header, and footer stay pinned — only the column track scrolls.

Example of a workflow-specific filter (7-status `design-review`) where only `Build` and `Review` have cards:

```
┌─┐ ┌─┐ ┌────────────┐ ┌─┐ ┌────────────┐ ┌─┐ ┌─┐
│B│ │D│ │   Build    │ │Q│ │   Review   │ │S│ │D│
│a│ │e│ │            │ │A│ │            │ │h│ │o│
│c│ │s│ │ • foo      │ │ │ │ • baz      │ │i│ │n│
│k│ │i│ │ • bar      │ │ │ │ • qux      │ │p│ │e│
│l│ │g│ │            │ │ │ │            │ │ │ │ │
│o│ │n│ │            │ │ │ │            │ │ │ │ │
│g│ │ │ │            │ │ │ │            │ │ │ │ │
└─┘ └─┘ └────────────┘ └─┘ └────────────┘ └─┘ └─┘
```

Switching the filter re-renders with the selected workflow's status set — or back to the six status categories when set to `All`. The sidebar's worker badges and activity feed are workflow-agnostic.

### 8. Orchestrator + Zen mode

**Underlying principle.** Zen Mode grants the orchestrator permission to act *as if the user is absent* — but only as far as it can do so with high confidence its decisions would match the user's. Low confidence → wait for the user. The goal is to let the system blitz through routine work without ever taking a judgment call the user wouldn't have approved.

The category abstraction is what makes this tractable across arbitrary workflows. Instead of bespoke rules per status name, the orchestrator reasons about the *kind* of transition in front of it — a `handoff → done` accept, a `backlog → ready` promotion, an `active → archived` close — and applies the appropriate confidence bar before acting. Each category transition has its own bar:

| Transition                        | Confidence bar in Zen                                                                       |
| --------------------------------- | ------------------------------------------------------------------------------------------- |
| `backlog → ready` (auto-promote)  | High — one of the three judgment categories in the orchestrator prompt must match.          |
| `ready → active` (dispatch)       | Always allowed — purely mechanical.                                                         |
| `active → handoff` (worker done)  | Always allowed — the worker raised it, not the orchestrator.                                |
| `handoff → done` (accept + merge) | Highest — local checks pass, no conflicts, diff under threshold, no danger paths, green CI. |
| `* → archived` (cancel/close)     | Never auto — closing without shipping is always the user's call.                            |

If any check on a high-bar transition fails or is ambiguous, Zen leaves the task where it is and surfaces the reason in the activity feed. Blitzing stops the moment confidence drops; the user picks up where Zen paused.

The orchestrator's reaction rules already key off events like `task=<id> in_progress -> review reason=worker:review-marker`. With Workflows, the events log line shape becomes:

```
<ts> task=<id> workflow=<name> <from-status> -> <to-status> reason=<short> category=<from-cat>->...<to-cat>
```

The `category` annotation lets the orchestrator's reactions match on semantic transitions rather than literal status names:

- *"On any transition into a* *`handoff`-category status whose next owner is* *`user`"* → that's the Zen Mode auto-merge trigger, regardless of whether the user called the status `Review` or `QA` or `Awaiting Sign-off`.

- *"On any* *`active`-owned-by-agent status becoming idle"* → auto-dispatch logic.

The mechanical work for each transition (push, open PR, merge, delete branch, close PR) is declared in the workflow's `transitions:` block — see §12. Zen consults that block to know *what* to execute when its confidence bar is met; the same block drives non-Zen flow via `shelbi merge` and the TUI.

Zen Mode's `## Merge Conditions` prompt section keeps working unchanged — it references "tasks that reach review" semantically, and the orchestrator interprets that against the assigned workflow's category map.

### 9. CLI changes

- `shelbi workflow list` — list all workflows in the project.

- `shelbi workflow show <name>` — print the YAML.

- `shelbi workflow new <name>` — scaffold a starter file.

- `shelbi workflow edit <name>` — open in `$EDITOR`.

- `shelbi task add` — gains an optional `--workflow <name>` flag (defaults to project default).

- `shelbi task move <id> --to <status>` — validates that `<status>` is a member of the task's workflow; errors with a list of valid options on mismatch.

- `shelbi task list --workflow <name>` — filter by workflow.

### 10. Events log shape change

Existing line: `<ts> task=<id> <from> -> <to> reason=<short>`
New line: `<ts> task=<id> workflow=<name> <from> -> <to> reason=<short> category=<from-cat>-><to-cat>`

The orchestrator's events tail parser gains the new fields; old lines (from before the change) parse with defaults (`workflow=default`, computed `category` from the canonical 5-name map).

### 11. Migration of existing projects

Existing projects have no `workflows/` directory. On first load after the upgrade:

1. Shelbi creates `workflows/default.yaml` with the canonical six-status definition (the five historical statuses plus `Canceled`).
2. Existing events log lines are left alone — the parser handles old-shape lines transparently.

Task files are **not** modified. A task with no `workflow:` field is treated as belonging to the default workflow, so there's nothing to backfill. The same rule covers tasks created in the future when the user doesn't specify a workflow.

### 12. Branch + PR semantics

Workflows declare more than statuses and owners — they also declare the **git side-effects that fire on each transition**. The schema extends with a `git:` block (project-level defaults) and a `transitions:` block (per-edge action lists). Status names — not categories — are the anchor: branching policy is most naturally read against the user's actual pipeline labels.

```yaml
# workflows/default.yaml (the git + transitions extension)
git:
  base_branch: main             # default branch tasks branch off
  merge_strategy: squash        # squash | merge | rebase

transitions:
  - from: InProgress
    to: Review
    actions: [push_branch, open_pr]

  - from: Review
    to: Done
    actions: [merge, delete_branch]

  - from: any                   # wildcard — applies regardless of source status
    to: Canceled
    actions: [close_pr, delete_branch]
```

**Action set.** Five primitives, each idempotent and silently no-op when not applicable:

| Action          | Effect                                                                                                          |
| --------------- | --------------------------------------------------------------------------------------------------------------- |
| `push_branch`   | Push the task's branch to `origin`.                                                                             |
| `open_pr`       | Open a PR against `git.base_branch`. No-op if one is already open.                                              |
| `merge`         | Merge the branch using `git.merge_strategy`. If a PR is open, merges via PR; otherwise merges branch-to-branch. |
| `close_pr`      | Close any open PR without merging. No-op if none open.                                                          |
| `delete_branch` | Delete the local + remote branch. Skipped if the branch is checked out in a worker worktree.                    |

Missing transitions are no-ops — a workflow only declares the edges where work needs to happen.

**Wildcards.** `from: any` declares an action set that applies regardless of the source status. Useful for "any path into Canceled deletes the branch." Only one wildcard per `to:` value, to keep resolution unambiguous.

**The `branch:` task field.** Every task carries an optional `branch:` field in its frontmatter — the name of the git branch the task operates on. Its presence (or absence) at task creation controls whether shelbi cuts a fresh branch or uses an existing one.

- **Omitted at creation** → the orchestrator cuts a new branch off the workflow's resolved `git.base_branch` when the task transitions to `InProgress`, names it conventionally (`shelbi/<task-id>`), and writes the name back into the task's frontmatter. The user never types a branch name. This is the common case.
- **Set at creation** → the orchestrator uses that branch as-is. No new branch is cut. This is how *release tasks* work: the user already knows the branch (typically `feature/<name>`) and pre-fills it.

Once populated, `branch:` is the source of truth for every subsequent action — `push_branch`, `open_pr`, `merge`, `delete_branch` all operate on it. Workflows don't need a separate "what branch to ship" field; the task carries that.

**Who executes the actions.**

- **In Zen Mode**, the orchestrator runs the actions for transitions it auto-triggers, gated by §8's confidence bars. A failed precheck (local checks, conflict, diff size, danger paths, CI) short-circuits the transition; the task stays where it is.

- **Outside Zen**, the same `transitions:` block drives the manual CLI and TUI. Moving a card from Review to Done (via `shelbi task move` or the TUI) executes `merge, delete_branch` in sequence.

Single source of truth: the workflow YAML declares behavior; Zen and the user-driven flow share the execution path.

**Two worked scenarios.**

*Gitflow — one PR per task, all merging to* *`main`.* The default block above, no overrides. Every task gets a branch off `main`, PR opens on `InProgress → Review`, squash-merge on `Review → Done`.

*Stack tasks on a long-lived feature branch, then one PR shipping the feature.* Two workflows working together:

```yaml
# workflows/feature-task.yaml — reused by any number of concurrent features
git:
  base_branch: feature/{{feature}}        # {{feature}} is supplied per task
  merge_strategy: squash

transitions:
  - from: InProgress
    to: Review
    actions: [push_branch]                # push so reviewers can see; NO open_pr — work just stacks

  - from: Review
    to: Done
    actions: [merge, delete_branch]       # merge directly into feature/{{feature}}
```

```yaml
# workflows/feature-release.yaml — ships any pre-existing branch into main
git:
  base_branch: main
  merge_strategy: squash

transitions:
  - from: InProgress
    to: Review
    actions: [push_branch, open_pr]       # PR the existing feature branch against main

  - from: Review
    to: Done
    actions: [merge, delete_branch]       # squash-merge into main, then delete the feature branch
```

A task in `feature-task` carries `feature: auth-rewrite` in its frontmatter — `feature-task`'s `base_branch` resolves to `feature/auth-rewrite`, and the orchestrator cuts a fresh task branch off it at dispatch. A second feature's tasks carry `feature: dashboard-v2`. Same workflow file, two concurrent stacks.

When a feature is ready to ship, the user creates a task in `feature-release` with `branch: feature/auth-rewrite` pre-filled in its frontmatter. Because `branch:` is set, the orchestrator skips the "cut a new branch" step and operates directly on the existing feature branch — one PR-and-merge cycle lands the whole feature into `main`.

**Parameterization.** Workflow YAML supports `{{var}}` placeholders, resolved from the task's frontmatter at load time. This is what makes one `feature-task.yaml` reusable across any number of concurrent features.

- Substitution is plain string replacement — no conditionals, no expressions, no defaults. A `{{name}}` token in any `git:` field is replaced by the value of `name:` in the task frontmatter.

- Placeholders are allowed anywhere in `git:` field values. They are not allowed in status names, category names, or the `transitions:` shape itself — only in git-side strings.

- At task load (and `shelbi task add`), shelbi scans the resolved workflow for unresolved `{{...}}` tokens. Any that remain produce a clear error: *"Workflow* *`feature-task`* *requires parameter* *`feature`; add* *`feature: <value>`* *to the task frontmatter."*

- The TUI's per-task workflow indicator (Phase 2) shows the workflow name plus the resolved parameter — e.g., `feature-task[auth-rewrite]` — so the user can tell two parallel feature stacks apart at a glance in the `All` filter.

Example task frontmatter for the `feature-task` workflow above:

```markdown
---
id: build-login-form
title: Build the login form
workflow: feature-task
feature: auth-rewrite                # resolves base_branch → feature/auth-rewrite
---
```

A second feature's task uses the same workflow file with a different value:

```markdown
---
id: wire-dashboard-shell
title: Wire the dashboard shell
workflow: feature-task
feature: dashboard-v2                # resolves base_branch → feature/dashboard-v2
---
```

**Workflows without git.** Research, planning, and other no-code workflows can omit both `git:` and `transitions:` entirely. Tasks move through statuses purely as bookkeeping — no side-effects fire.

**Relationship to categories.** §8's confidence bars speak in *categories* (semantic reasoning: "this is a `handoff → done` accept, apply the highest bar"). The `transitions:` block speaks in *statuses* (concrete actions: "on `Review → Done`, run `merge` and `delete_branch`"). Both layers coexist: categories tell Zen *whether* it has permission to act; transitions tell *anyone acting* what to do.

## Rollout

Two phases.

**Phase 1 — Workflows substrate.**

- Status categories + workflow YAML schema (parser + validator).

- `workflows/` directory scaffold for new projects.

- One-time migration for existing projects (auto-create `default.yaml` if missing; no per-task changes — see §11).

- Per-task `workflow:` frontmatter support.

- CLI: `shelbi workflow list/show/new/edit` + the `--workflow` flag on `task add` + workflow validation on `task move`.

- Events log line shape extended; backward-compatible parser.

- Orchestrator prompt updates: reaction rules speak in categories not status names.

- Zen Mode: confirm `Merge Conditions` works against the category abstraction.

After Phase 1: existing projects keep working; users can author additional workflows by dropping YAML in `workflows/`.

**Phase 2 — TUI polish.**

- Workflow filter in the Tasks Kanban view.

- All-statuses-visible rendering: cards-bearing columns full-width; empty statuses collapsed to vertical-text strips; horizontal scroll when the column track outgrows the terminal.

- Per-task workflow indicator (small label in the task card) so the user can tell at a glance which workflow a card belongs to when the filter is switched away from it.

After Phase 2: rich multi-workflow filtering on the existing Kanban board; single-workflow projects look identical to today.

## Decisions

- **Transitions are any-to-any by default.** Users can move cards freely between any columns. Projects that want a strict state machine declare an optional `allowed_transitions:` map in the workflow YAML (see §2). This preserves today's freedom.
- **No workflow inheritance in v1.** Workflows do not support `extends:` — each YAML stands alone. Revisit only if users author 4+ similar workflows and copy-paste pain becomes real.
- **A task is bound to its workflow at creation.** Cross-workflow moves are not allowed. To switch a task into a different workflow, the user creates a fresh task in the target workflow (and optionally archives the original).
- **The status-category set is closed at six.** `backlog`, `ready`, `active`, `handoff`, `done`, `archived`. Custom workflows pick from these — they can't define new categories. Keeps generic code (Zen Mode, activity feed, sidebar) correct across arbitrary workflows.
- **Kanban workflow filter persists per project.** When the user switches the filter away from `All`, the next session re-opens on the same selection. Reset to `All` only when the previously-chosen workflow's YAML is deleted.
- **`All`-mode card ordering is interleaved by priority.** Cards from multiple workflows mix freely within each category column, sorted by task priority (`shelbi task prio`). The per-card workflow label (Phase 2) provides visual distinction; no grouping or separators.
- **Migration is minimal.** Shelbi creates `workflows/default.yaml` if missing; task files are not modified. Tasks without an explicit `workflow:` field implicitly use the default workflow (see §11).
- **Zen config is per-workflow with project-level fallback.** `zen.checks.local`, `zen.ci_timeout`, and `zen.danger_paths` can be declared in a workflow's YAML; missing values fall back to project-level settings. A `research` workflow can opt out of code-style checks without affecting `default`.
- **Naming stays as "Workflows".** The "Shelbi" prefix is used in prose when disambiguation from CI/GitHub Actions workflows is needed (i.e., "Shelbi Workflows").

## Open questions

_All previously-listed questions are resolved above in Decisions. New questions will land here as the design evolves._
