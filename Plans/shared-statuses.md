# Shared Statuses

## Context

Today every Shelbi Workflow (`workflows/<name>.yaml`) declares its own `statuses:` block. The four shipped workflows — `app`, `app-feature`, `default`, `site` — all repeat nearly the same six-status definition (`backlog`, `todo`, `in-progress`, `review`, `done`, `canceled`). Editing a status name, changing its display label, or adding a new lane (e.g. a `qa` status) means touching every workflow file.

The duplication also blocks the **all view** — the cross-workflow Kanban that shows every task in the project. Today each workflow defines its columns independently, so there is no canonical column set. If two workflows both have a `review` status, the all view has to decide ad-hoc whether they're the same column.

This plan extracts **status identity** — id, display name, and ordering — into a single shared file: `workflows/statuses.yml`. **Status semantics** — owner, agent, category — stay in the workflow file, declared per status the workflow uses. The all view consumes the canonical id+name+order list directly; per-task rendering layers on the owning workflow's semantic attributes.

This plan builds on [[workflows]] (which established status categories + the inline `statuses:` schema) and [[agents-workspaces]] §4 (the two-field `owner` + `agent` design).

## Design

### 1. `workflows/statuses.yml`

The new project-level file defines every status the project understands — **identity only**:

```yaml
statuses:
  - id: backlog
    name: Backlog
  - id: todo
    name: Todo
  - id: in-progress
    name: In Progress
  - id: review
    name: Review
  - id: done
    name: Done
  - id: canceled
    name: Canceled
```

Fields per status:

- `id` — stable identifier referenced by workflows and tasks
- `name` — display label

That's it. **No `category`, `owner`, or `agent` in this file.** Those are workflow-scoped concerns.

**Status declaration order in `statuses.yml` is the canonical column order for the entire project.** The all view, every workflow's filtered board, and any future analytics surfaces all read from this single ordered list.

### 2. Workflow YAML declares semantics per status

Workflow files keep a `statuses:` block, but each entry now references a status id and declares the workflow's per-status semantics:

```yaml
name: app-feature
description: |
  Individual subtasks within a feature in the Shelbi Rust crates.

statuses:
  - id: backlog
    category: backlog
    owner: user
    agent: orchestrator
  - id: todo
    category: ready
    owner: agent
    agent: orchestrator
  - id: in-progress
    category: active
    owner: agent
    agent: developer
  - id: review
    category: handoff
    owner: user
    agent: orchestrator
  - id: done
    category: done
    owner: user
  - id: canceled
    category: archived
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
- `category` — semantic floor (`backlog`/`ready`/`active`/`handoff`/`done`/`archived`)
- `owner` — `user` | `agent` (whose responsibility when Zen is off)
- `agent` (optional) — which agent runs when Zen is on; names a directory under `agents/`

A workflow's `statuses:` list is the subset of `statuses.yml` the workflow uses, in any order it wants for declaration (the **rendering** order is always `statuses.yml` order — see §4). **There is no implicit "all statuses" shortcut** — each workflow lists what it uses. Keeps the workflow self-documenting and prevents a future addition to `statuses.yml` from silently extending every workflow.

By design, the same status id can have **different** `category`/`owner`/`agent` in different workflows. A research workflow could mark `review` as `owner: agent` with `agent: qa`; the default `app` workflow keeps it `owner: user`. The shared identity is preserved (one column on the all view); the semantics flex per workflow.

### 3. The all view column order

The cross-workflow Kanban renders columns in `statuses.yml` declaration order. Each task slots into the column matching its `status` field, regardless of which workflow it's on. A status defined in `statuses.yml` but not referenced by any current task still renders an empty column — keeps the layout stable as work flows through.

Per-task rendering details (activity-feed reasons, orchestrator routing, Zen handoff logic) read the **owning workflow's** declaration for that status's `category`/`owner`/`agent`. The column header itself uses the global `name`.

### 4. Loader validation rules

**`statuses.yml`:**

- Must exist in the project's `workflows/` directory. On `shelbi init` and on `shelbi reload`, materialize the default file if missing (the six-status set in §1 is the shipped default).
- Each entry must have a unique non-empty `id` and a non-empty `name`.
- `id` is a slug-shaped string (`[a-z0-9-]+`). `name` is free-form text.

**Workflow YAML:**

- Each `statuses:` entry's `id` must reference an id defined in `statuses.yml`. Unknown ids hard-fail with the list of available status ids.
- `category` is required, must be one of the six values.
- `owner` is required, must be `user` or `agent`.
- `agent` is optional; if present must match an agent directory under `agents/`.
- A workflow's `transitions:` `from` and `to` must reference status ids the workflow includes.
- A workflow's `initial_status` must be in the workflow's `statuses:` list.
- The same status id cannot appear twice in a single workflow's `statuses:` list.

### 5. Legacy migration

Today's inline `statuses:` block already carries `id`, `name`, `category`, `owner` together. The migration on first load:

- The loader walks every workflow file. For each inline status definition, it extracts `id` + `name` into a working set.
- If multiple workflows define the same `id` with **different** names, the migration **hard-fails** and tells the user to reconcile (a project-wide id should have one display name).
- The working set is written out as `workflows/statuses.yml` in the union order observed (first-seen wins for ordering when ids appear in multiple workflows).
- Each workflow's inline `statuses:` block is **rewritten** to the new form (id + category + owner + agent), dropping the `name` field.
- The loader emits a **one-time** stderr deprecation hint summarizing what was migrated.

On subsequent loads, the loader detects mixed-form workflows (an inline status definition with `name:` still present and `statuses.yml` already on disk) and hard-fails with a diff — once the user has `statuses.yml`, workflow files must use the reference-only form.

A separate Phase 3 task (filed later) removes the legacy migration path.

### 6. CLI surface

A new subcommand reports the project's status set:

```
shelbi status list
```

Output:

```
ORDER   ID            NAME
1       backlog       Backlog
2       todo          Todo
3       in-progress   In Progress
4       review        Review
5       done          Done
6       canceled      Canceled
```

A second subcommand reports per-workflow semantics:

```
shelbi workflow show <name>
```

Output for `app-feature`:

```
STATUS        CATEGORY    OWNER    AGENT
backlog       backlog     user     orchestrator
todo          ready       agent    orchestrator
in-progress   active      agent    developer
review        handoff     user     orchestrator
done          done        user     —
canceled      archived    user     —
```

No new flags on `shelbi task move`, `shelbi task list`, etc. — they keep referencing statuses by id.

## Rollout

### Phase 1 — schema + loader

One task on the `app` Shelbi Workflow:

- Add `workflows/statuses.yml` to the project layout. Materialize the default file on `shelbi init` and self-heal on `shelbi reload`.
- Update the loader to read `statuses.yml`, apply the §4 validation rules, and parse the new workflow `statuses:` form (id + category + owner + agent).
- Implement the legacy migration (§5) with the one-time deprecation hint.
- Update each shipped workflow file (`app`, `app-feature`, `default`, `site`) to the reference form.
- Add `shelbi status list` and `shelbi workflow show <name>` CLI commands.
- Tests: round-trip statuses.yml + workflows; reject unknown ids; reject mixed forms after migration; reject conflicting names across workflows; migration rewrites every workflow correctly.

### Phase 2 — TUI all view

One task on the `app` Shelbi Workflow:

- Update the all-view Kanban renderer to consume `statuses.yml` directly for column order.
- Drop any workflow-by-workflow column reconciliation logic.
- Render stable empty columns for statuses that no current task uses.
- Per-task overlays (badges, activity-feed reasons) read the owning workflow's declared category/owner/agent for that status.

### Phase 3 — remove legacy (file later)

One task on the `app` Shelbi Workflow, filed after Phase 1 has been live for one release:

- Delete the inline-`name:` migration path; workflow files must use the reference form.
- Drop the migration helper code.

## Open Questions

- **Should `category` stay in `statuses.yml` instead of per-workflow?** Today's instruction is "only id, name, order" in `statuses.yml`, which puts `category` in the workflow. The cost is a per-workflow repetition of category for each status used; the benefit is allowing the same status id to be a `handoff` in one workflow and a `done` in another (probably never useful in practice). If we don't expect that flexibility, `category` could move back to `statuses.yml` as part of status identity — the workflow would only declare `owner` and `agent`. Worth a second look before Phase 1 ships.
- **Per-workflow column ordering** — assumed inherited from `statuses.yml`. Allowing per-workflow ordering would let, say, a research workflow show `archived` between `active` and `done`. Probably overkill for v1; revisit if a real use case appears.