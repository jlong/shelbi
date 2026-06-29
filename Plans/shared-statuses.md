# Shared Statuses

## Context

Today every Shelbi Workflow (`workflows/<name>.yaml`) declares its own `statuses:` block. The four shipped workflows — `app`, `app-feature`, `default`, `site` — all repeat nearly the same six-status definition (`backlog`, `todo`, `in-progress`, `review`, `done`, `canceled`). Editing a status name, changing its display label, or adding a new lane (e.g. a `qa` status) means touching every workflow file.

The duplication also blocks the **all view** — the cross-workflow Kanban that shows every task in the project. Today each workflow defines its columns independently, so there is no canonical column set. If two workflows both have a `review` status, the all view has to decide ad-hoc whether they're the same column.

This plan extracts **status identity** — id, display name, category, and ordering — into a single shared file: `workflows/statuses.yml`. **Workflow-scoped semantics** — owner, agent — stay in the workflow file, declared per status the workflow uses. The all view consumes the canonical status list directly; per-task rendering layers on the owning workflow's owner/agent.

This plan builds on [[workflows]] (which established status categories + the inline `statuses:` schema) and [[agents-workspaces]] §4 (the two-field `owner` + `agent` design).

## Design

### 1. `workflows/statuses.yml`

The new project-level file defines every status the project understands — **identity + category**:

```yaml
statuses:
  - id: backlog
    name: Backlog
    category: backlog
  - id: todo
    name: Todo
    category: ready
  - id: in-progress
    name: In Progress
    category: active
  - id: review
    name: Review
    category: handoff
  - id: done
    name: Done
    category: done
  - id: canceled
    name: Canceled
    category: archived
```

Fields per status:

- `id` — stable identifier referenced by workflows and tasks

- `name` — display label

- `category` — semantic floor (`backlog`/`ready`/`active`/`handoff`/`done`/`archived`)

**No** **`owner`** **or** **`agent`** **in this file.** Those are workflow-scoped concerns.

**Status declaration order in** **`statuses.yml`** **is the canonical column order for every view in the project** — the all view, per-workflow boards, and any future analytics surfaces. Workflows cannot reorder columns; they can only choose which statuses to include.

### 2. Workflow YAML declares semantics per status

Workflow files keep a `statuses:` block, but each entry references a status id and declares the workflow's per-status owner + (optional) agent:

```yaml
name: app-feature
description: |
  Individual subtasks within a feature in the Shelbi Rust crates.

statuses:
  - id: backlog
    owner: user
    agent: orchestrator
  - id: todo
    owner: agent
    agent: orchestrator
  - id: in-progress
    owner: agent
    agent: developer
  - id: review
    owner: user
    agent: orchestrator
  - id: done
    owner: user
  - id: canceled
    owner: user

initial_status: backlog

git:
  base_branch: feature/{{feature}}
  merge_strategy: squash

transitions:
  - from: in-progress
    to: review
    actions: [push_branch, open_pr]
  - from: review
    to: done
    actions: [merge, delete_branch]
  - from: in-progress
    to: canceled
    actions: [close_pr, delete_branch]
  - from: review
    to: canceled
    actions: [close_pr, delete_branch]

zen:
  checks:
    local:
      - cargo build --workspace
      - cargo test --workspace
      - cargo clippy --workspace --all-targets -- -D warnings
```

Per-status fields in the workflow:

- `id` — must reference a status in `statuses.yml`

- `owner` — `user` | `agent` (whose responsibility when Zen is off)

- `agent` (optional) — which agent runs when Zen is on; names a directory under `agents/`

A workflow's `statuses:` list is the subset of `statuses.yml` the workflow uses. **There is no implicit "all statuses" shortcut** — each workflow lists what it uses. Keeps the workflow self-documenting and prevents a future addition to `statuses.yml` from silently extending every workflow's board.

By design, the same status id can have **different** `owner`/`agent` in different workflows. A research workflow could mark `review` as `owner: agent` with `agent: qa`; the default `app` workflow keeps it `owner: user`. The shared identity (id, name, category, column position) is preserved; the agent/user responsibility flexes per workflow.

### 3. The all view column order

The cross-workflow Kanban renders columns in `statuses.yml` declaration order. Each task slots into the column matching its `status` field, regardless of which workflow it's on. A status defined in `statuses.yml` but not referenced by any current task still renders an empty column — keeps the layout stable as work flows through.

Per-task rendering details (activity-feed reasons, orchestrator routing, Zen handoff logic) read the **owning workflow's** declared `owner`/`agent` for that status. The column header itself uses the global `name`; the global `category` drives generic semantic code (Zen Mode triggers, "what does this column mean" logic).

### 4. Loader validation rules

**`statuses.yml`:**

- Must exist in the project's `workflows/` directory. On `shelbi init` and on `shelbi reload`, materialize the default file if missing (the six-status set in §1 is the shipped default).

- Each entry must have a unique non-empty `id`, a non-empty `name`, and a valid `category` (one of the six values).

- `id` is a slug-shaped string (`[a-z0-9-]+`). `name` is free-form text.

**Workflow YAML:**

- Each `statuses:` entry's `id` must reference an id defined in `statuses.yml`. Unknown ids hard-fail with the list of available status ids.

- `owner` is required, must be `user` or `agent`.

- `agent` is optional; if present must match an agent directory under `agents/`.

- A workflow's `transitions:` `from` and `to` must reference status ids the workflow includes.

- A workflow's `initial_status` must be in the workflow's `statuses:` list.

- The same status id cannot appear twice in a single workflow's `statuses:` list.

### 5. Legacy migration

Today's inline `statuses:` block already carries `id`, `name`, `category`, `owner` together. The migration on first load:

- The loader walks every workflow file. For each inline status definition, it extracts `id` + `name` + `category` into a working set.

- If multiple workflows define the same `id` with **different** names or categories, the migration **hard-fails** and tells the user to reconcile (a project-wide id should have one display name and one category).

- The working set is written out as `workflows/statuses.yml` in the union order observed (first-seen wins for ordering when ids appear in multiple workflows).

- Each workflow's inline `statuses:` block is **rewritten** to the new form (id + owner + agent), dropping `name` and `category`.

- The loader emits a **one-time** stderr deprecation hint summarizing what was migrated.

On subsequent loads, the loader detects mixed-form workflows (an inline status definition with `name:` or `category:` still present and `statuses.yml` already on disk) and hard-fails with a diff — once the user has `statuses.yml`, workflow files must use the reference-only form.

A separate Phase 3 task (filed later) removes the legacy migration path.

### 6. CLI surface

A new subcommand reports the project's status set:

```
shelbi status list
```

Output:

```
ORDER   ID            NAME            CATEGORY
1       backlog       Backlog         backlog
2       todo          Todo            ready
3       in-progress   In Progress     active
4       review        Review          handoff
5       done          Done            done
6       canceled      Canceled        archived
```

A second subcommand reports per-workflow owner/agent:

```
shelbi workflow show <name>
```

Output for `app-feature`:

```
STATUS        OWNER    AGENT
backlog       user     orchestrator
todo          agent    orchestrator
in-progress   agent    developer
review        user     orchestrator
done          user     —
canceled      user     —
```

No new flags on `shelbi task move`, `shelbi task list`, etc. — they keep referencing statuses by id.

## Rollout

### Phase 1 — schema + loader

One task on the `app` Shelbi Workflow:

- Add `workflows/statuses.yml` to the project layout. Materialize the default file on `shelbi init` and self-heal on `shelbi reload`.

- Update the loader to read `statuses.yml`, apply the §4 validation rules, and parse the new workflow `statuses:` form (id + owner + agent).

- Implement the legacy migration (§5) with the one-time deprecation hint.

- Update each shipped workflow file (`app`, `app-feature`, `default`, `site`) to the reference form.

- Add `shelbi status list` and `shelbi workflow show <name>` CLI commands.

- Tests: round-trip statuses.yml + workflows; reject unknown ids; reject mixed forms after migration; reject conflicting names or categories across workflows; migration rewrites every workflow correctly.

### Phase 2 — TUI all view

One task on the `app` Shelbi Workflow:

- Update the all-view Kanban renderer to consume `statuses.yml` directly for column order.

- Drop any workflow-by-workflow column reconciliation logic.

- Render stable empty columns for statuses that no current task uses.

- Per-task overlays (badges, activity-feed reasons) read the owning workflow's declared owner/agent for that status.

### Phase 3 — remove legacy

One task on the `app` Shelbi Workflow, filed after Phase 1 has been live for one release:

- Delete the inline-form migration path; workflow files must use the reference form.

- Drop the migration helper code.
