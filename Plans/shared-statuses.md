# Shared Statuses

## Context

Today every Shelbi Workflow (`workflows/<name>.yaml`) declares its own `statuses:` block. The four shipped workflows — `app`, `app-feature`, `default`, `site` — all repeat nearly the same six-status definition (`backlog`, `todo`, `in-progress`, `review`, `done`, `canceled`). Editing a status name, changing an owner, or adding a new lane (e.g. a `qa` status) means touching every workflow file.

The duplication also blocks the **all view** — the cross-workflow Kanban that shows every task in the project. Today each workflow defines its columns independently, so there is no canonical column set. If two workflows both have a `review` status, the all view has to decide ad-hoc whether they're the same column.

This plan extracts status definitions into a single shared file: `workflows/statuses.yml`. Workflows reference status ids and declare valid transitions; they no longer carry status definitions. The all view consumes the canonical status list directly, in declaration order.

This plan builds on [[workflows]] (which established status categories + the inline `statuses:` schema) and [[agents-workspaces]] §4 (the two-field `owner` + `agent` design).

## Design

### 1. `workflows/statuses.yml`

The new project-level file defines every status the project understands:

```yaml
statuses:
  - id: backlog
    name: Backlog
    category: backlog
    owner: user
    agent: orchestrator

  - id: todo
    name: Todo
    category: ready
    owner: agent
    agent: orchestrator

  - id: in-progress
    name: In Progress
    category: active
    owner: agent
    agent: developer

  - id: review
    name: Review
    category: handoff
    owner: user
    agent: orchestrator

  - id: done
    name: Done
    category: done
    owner: user

  - id: canceled
    name: Canceled
    category: archived
    owner: user
```

Fields per status:

- `id` — stable identifier referenced by workflows and tasks
- `name` — display label
- `category` — semantic floor (`backlog`/`ready`/`active`/`handoff`/`done`/`archived`)
- `owner` — `user` | `agent` (whose responsibility when Zen is off)
- `agent` (optional) — which agent runs when Zen is on; names a directory under `agents/`

**Status declaration order in `statuses.yml` is the canonical column order for the entire project.** The all view, every workflow's filtered board, and any future analytics surfaces all read from this single ordered list.

### 2. Workflow YAML becomes status-reference-only

Workflow files drop the inline `statuses:` block and instead list the status ids they use:

```yaml
name: app-feature
description: |
  Individual subtasks within a feature in the Shelbi Rust crates.

statuses: [backlog, todo, in-progress, review, done, canceled]

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

The `statuses` field is the subset of `statuses.yml` the workflow uses. **There is no implicit "all statuses" shortcut** — each workflow must list its required ids. Keeps the workflow self-documenting and prevents a future addition to `statuses.yml` from silently extending every workflow's column set.

### 3. Per-workflow status overrides

Most workflows accept the project-level status definition unchanged. The rare case where a workflow needs to override one attribute (e.g., make `review` agent-owned in a fully-automated workflow) uses an inline form mixed into the same list:

```yaml
statuses:
  - backlog
  - todo
  - in-progress
  - id: review
    owner: agent
    agent: developer        # this workflow auto-reviews via the developer agent
  - done
  - canceled
```

The override is **partial**: it inherits everything from `statuses.yml` and applies only the fields it specifies. `id` is required to identify which status is being overridden.

**`category` and `name` cannot be overridden** — those are project-wide invariants. `owner` and `agent` are the only overridable fields. Attempting to override anything else hard-fails at load time.

### 4. The all view column order

The cross-workflow Kanban renders columns in `statuses.yml` declaration order. Each task slots into the column matching its `status` field, regardless of which workflow it's on. A status defined in `statuses.yml` but not referenced by any current task still renders an empty column — keeps the layout stable as work flows through.

If a workflow overrides a status's `owner` or `agent` (per §3), the override is **per-task**: the column itself appears in the global order, but per-task rendering (activity-feed reasons, orchestrator routing) reflects the workflow-applied attributes.

### 5. Loader validation rules

- `statuses.yml` must exist in the project's `workflows/` directory. On `shelbi init` and on `shelbi reload`, materialize the default file if missing (the six-status set in §1 is the shipped default).
- Each entry in `statuses.yml` must have a unique `id`, valid `category` (one of the six values), valid `owner` (`user`|`agent`). `agent` if present must match an agent directory under `agents/`.
- A workflow's `statuses:` list must reference only ids defined in `statuses.yml`. Unknown ids hard-fail with the list of available status ids.
- A workflow's `transitions:` `from` and `to` must reference status ids the workflow includes.
- A workflow's `initial_status` must be in the workflow's `statuses:` list.
- Per-workflow status overrides cannot change `category` or `name` — only `owner` and `agent`.

### 6. Legacy migration

Existing workflow files with inline `statuses:` blocks (the current form) auto-migrate on load:

- The loader extracts the inline definitions and matches them by `id` against `statuses.yml`.
- If both define a status with **identical** attributes, the workflow's inline copy is silently ignored.
- If the inline form defines a status id NOT in `statuses.yml`, the loader **hard-fails** with a one-line nudge to add it to `statuses.yml`.
- If the inline form defines a status id with **conflicting** attributes (different `category`, `name`, `owner`, or `agent`), the loader **hard-fails** with a diff of the conflict.
- After a successful migration load, the loader emits a **one-time-per-workflow** stderr deprecation warning telling the user to remove the inline `statuses:` block and replace with `statuses: [<ids>]`.

A separate Phase 3 task (filed later) removes the legacy code path.

### 7. CLI surface

A new subcommand reports the project's status set:

```
shelbi status list
```

Output:

```
ID            NAME            CATEGORY    OWNER    AGENT
backlog       Backlog         backlog     user     orchestrator
todo          Todo            ready       agent    orchestrator
in-progress   In Progress     active      agent    developer
review        Review          handoff     user     orchestrator
done          Done            done        user     —
canceled      Canceled        archived    user     —
```

No new flags on `shelbi task move`, `shelbi task list`, etc. — they keep referencing statuses by id.

## Rollout

### Phase 1 — schema + loader

One task on the `app` Shelbi Workflow:

- Extract status definitions to `workflows/statuses.yml`.
- Materialize the default file on `shelbi init` and self-heal on `shelbi reload`.
- Update the loader to read `statuses.yml`, apply the §5 validation rules, and accept the new reference form (`statuses: [<id>, ...]` and the partial-override list).
- Implement the legacy inline-block migration with deprecation warning (§6).
- Update each shipped workflow file (`app`, `app-feature`, `default`, `site`) to the reference form.
- Add `shelbi status list` CLI command.
- Tests: round-trip statuses.yml + reference workflows; reject unknown ids; reject category/name overrides; auto-migrate legacy inline form; deprecation warning emits exactly once per workflow.

### Phase 2 — TUI all view

One task on the `app` Shelbi Workflow:

- Update the all-view Kanban renderer to consume `statuses.yml` directly for column order.
- Drop any workflow-by-workflow column reconciliation logic.
- Render stable empty columns for statuses that no current task uses.

### Phase 3 — remove legacy (file later)

One task on the `app` Shelbi Workflow, filed after Phase 1 has been live for one release:

- Delete the inline-block loader fallback.
- Workflow files become reference-only.

## Open Questions

- **Per-workflow column ordering** — assumed inherited from `statuses.yml`. Allowing per-workflow ordering would let, say, a research workflow show `archived` between `active` and `done`. Probably overkill for v1; revisit if a real use case appears.
- **Per-(workflow, status) Zen tuning beyond `agent:` override** — if a workflow needs status-specific Zen behavior beyond what `agent:` covers, may need a separate `zen:` block in the workflow keyed by status id. Defer until a concrete case shows up.