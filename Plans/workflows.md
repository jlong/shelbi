# Workflows

## Context

Today Shelbi has **exactly five statuses, hardcoded** everywhere — `Backlog`, `Todo`, `InProgress`, `Review`, `Done`. Every project gets the same five. The columns appear in the Kanban TUI, drive the orchestrator's reaction rules, are baked into the events log line shapes, and assumed by Zen Mode's "auto-merge when something hits review" logic. Adding a sixth status (e.g., a "QA" lane between Review and Done) requires a code change.

That model fits a single-track *one user + agents* workflow well, but it can't model:

- A **design + build + review** pipeline where designs land in a "Design Review" column for the user before they hit a worker as buildable Todo.
- A **multi-stakeholder** flow where docs tasks have a "Tech Review" column the user owns and code tasks don't.
- An **agent-only** internal workflow (e.g., a research pipeline) that runs without ever surfacing in the user's primary board.

**Workflows** generalizes the status set per project. The defaults stay, but now they're one workflow definition among many. A workflow is a named YAML file declaring its statuses, who owns each one (user vs agent), and how work moves between them. Tasks are assigned to a workflow (default if unassigned). Multiple workflows can coexist; the TUI and orchestrator respect each task's workflow when rendering and routing.

The five hardcoded statuses become the **default workflow** — one definition file that ships with every new project. Adding a new workflow is a YAML edit. Custom workflows can introduce custom statuses, but every status maps to a **status category** (the underlying semantic) so generic code (Zen Mode auto-merge, activity feed rendering, "what does this column mean?") still works across arbitrary status names.

## Design

### 1. Status categories (the semantic floor)

Every status — default or custom — belongs to one of five categories. Categories are the vocabulary the rest of the system speaks; status *names* are user-facing labels.

| Category    | Semantic                                                                  | Default-workflow status using it |
|-------------|---------------------------------------------------------------------------|----------------------------------|
| `backlog`   | Not yet ready for work — triage stage.                                    | `Backlog`                        |
| `ready`     | Ready to be picked up by whoever owns it.                                 | `Todo`                           |
| `active`    | Owner is working on it now.                                               | `InProgress`                     |
| `handoff`   | One owner has finished their part; another's input is required next.      | `Review`                         |
| `done`      | Terminal state — accepted, shipped.                                       | `Done`                           |

Generic code reasons in categories, never in literal status names:

- Zen Mode's auto-merge trigger: *"when a task transitions to any `handoff` status whose next owner is the user, evaluate the merge-conditions flow."*
- Activity feed renderers: *"a `ready -> active` transition reads as 'started.'"*
- Orchestrator auto-dispatch: *"a task in a `ready` status whose owner is `agent` is dispatchable."*

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

# Optional: which status a new task lands in by default. If absent, the
# first status in the list is used.
initial_status: Backlog

# Optional: restrict allowed transitions. If absent, any-to-any is allowed.
# transitions:
#   Backlog: [Todo]
#   Todo: [InProgress, Backlog]
#   InProgress: [Review, Todo]
#   Review: [Done, Todo, InProgress]
#   Done: []
```

### 3. Default workflow

Every new project ships with `workflows/default.yaml` containing the five-status definition above. This makes the default an editable file rather than implicit behavior — the user can immediately customize names (e.g., rename `Todo` to `Ready`) without breaking anything.

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

**By default the filter is `All` — every task across every workflow is visible.** In `All` mode the column set is the five **status categories** from §1 (Backlog / Ready / Active / Handoff / Done), since every status in every workflow maps to one of them. Cards from different workflows can sit in the same column; a small workflow label on each card (Phase 2) tells them apart. Selecting a specific workflow from the filter swaps the columns to that workflow's native status names and hides tasks from other workflows.

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

The filter defaults to **`All`** — every task across every workflow is visible, with columns drawn from the five status categories (Backlog / Ready / Active / Handoff / Done). Selecting a specific workflow switches columns to that workflow's native status names and narrows the cards to only that workflow's tasks. There is no separate "Workflows" view; workflows are configuration (see §4).

**Every column is always shown**, in declaration order — the full structure stays visible at a glance, regardless of where work currently sits. Empty columns aren't hidden; they're collapsed. The same rules apply in both modes: in `All`, the columns are the five status categories; in a workflow-specific filter, they're that workflow's declared statuses.

- Columns with one or more tasks render as **full-width columns** (card list inside).
- Columns with **no tasks collapse to a thin vertical strip** ~3 cells wide, with the column name written top-to-bottom (one character per row). The strip spans the full board height so the column rail stays visually aligned. Cards can still be dropped onto a collapsed strip; moving the cursor onto one expands it temporarily until focus leaves.
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

Switching the filter re-renders with the selected workflow's status set — or back to the five status categories when set to `All`. The sidebar's worker badges and activity feed are workflow-agnostic.

### 8. Orchestrator + Zen integration

The orchestrator's reaction rules already key off events like `task=<id> in_progress -> review reason=worker:review-marker`. With Workflows, the events log line shape becomes:

```
<ts> task=<id> workflow=<name> <from-status> -> <to-status> reason=<short> category=<from-cat>->...<to-cat>
```

The `category` annotation lets the orchestrator's reactions match on semantic transitions rather than literal status names:

- *"On any transition into a `handoff`-category status whose next owner is `user`"* → that's the Zen Mode auto-merge trigger, regardless of whether the user called the status `Review` or `QA` or `Awaiting Sign-off`.
- *"On any `active`-owned-by-agent status becoming idle"* → auto-dispatch logic.

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

1. Shelbi creates `workflows/default.yaml` with the canonical 5-status definition.
2. All existing tasks get `workflow: default` written to their frontmatter (idempotent; only adds if absent).
3. Existing events log lines are left alone — the parser handles old-shape lines transparently.

One-time migration, then the project is on the new schema.

## Rollout

Two phases.

**Phase 1 — Workflows substrate.**

- Status categories + workflow YAML schema (parser + validator).
- `workflows/` directory scaffold for new projects.
- One-time migration for existing projects (auto-create `default.yaml`, backfill task frontmatter).
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

(Locked once we walk through the plan together.)

## Open questions

- **Are transitions restricted by default, or any-to-any?** Today the user can move tasks freely between any columns. The YAML schema has an optional `transitions:` map. Question: when absent, is the default any-to-any or only adjacent? Recommend any-to-any (matches today's freedom).
- **Workflow inheritance?** Should a workflow be able to `extends: default` and only specify deltas (add a status, override an owner)? Useful for projects with many similar workflows; more schema to maintain. Recommend: not in v1; revisit if users actually author 4+ workflows.
- **Cross-workflow task moves?** Can a task switch its `workflow:` after creation (e.g., from `default` to `design-review`)? If yes, what status does it land in (the new workflow's `initial_status`?). Recommend: yes, allowed; lands in the new workflow's initial status; emits a `workflow-change` event.
- **Filter persistence across sessions.** When the user switches the Kanban filter away from `All` and quits, does the next session re-open on `All` or remember the last selection? Recommend: remember per-project so context survives a restart; reset to `All` only when the previously-chosen workflow's YAML is deleted.
- **Same-category card ordering in `All` mode.** When multiple workflows' tasks share a category column (e.g., two `active` statuses from different workflows both pour into `Active`), how are cards ordered within the column? Options: by task priority (`shelbi task prio`), grouped by workflow first then priority within group, or fully interleaved by priority. Recommend: fully interleaved by priority, with the workflow label on each card carrying the visual distinction.
- **Migration safety.** Touching every task file to add `workflow: default` to frontmatter is a write operation across many files. Want a `--dry-run` flag on the migration, plus a single squashed commit so the diff is reviewable. Confirm.
- **Per-workflow Zen config?** Should `zen.checks.local` / `zen.ci_timeout` / `zen.danger_paths` be definable per-workflow as well as per-project? Useful when a `research` workflow has no code-checks but the `default` does. Recommend: per-workflow with fallback to project-level.
- **Auto-creation of categories.** Could a custom workflow define a NEW category beyond the five listed? Probably not — keep the category set closed so generic code stays correct. Confirm.
- **Naming.** "Workflow" is a heavy word (overloaded with CI workflows, GitHub Actions workflows, n8n workflows). Should this feature have a different name (e.g., "Pipelines", "Tracks", "Boards")? "Workflows" is what the user named it; sticking with it unless something better emerges.
