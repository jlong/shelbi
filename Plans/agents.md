# Agents & Workspaces

## Vocabulary

This plan cleanly separates three concepts that Shelbi has been conflating:

- **Workspace** — a persistent slot tied to a machine: one tmux pane + one git worktree at `.shelbi/wt/<name>`. Long-lived. `alpha`, `bravo`, `charlie` on hub; `delta`, `echo`, `foxtrot` on devbox. Today's "workers" are workspaces.

- **Agent** — a system prompt + a skill set. Logical and reusable across workspaces. `Orchestrator`, `Developer`, `QA`, `Security Review` are agents.

- **Task** — a unit of work that gets dispatched.

A dispatch reads cleanly as: *"run task* ***T*** *using agent* ***A*** *in workspace* ***W**."*

The `owner:` field on a workflow status names the **agent** that handles the status — the orchestrator picks a free **workspace** to run it in based on machine constraints and declaration order. Workspaces are capacity; agents are roles; tasks are work.

## Context

Shelbi today conflates two distinct ideas under the word "Agent":

1. **In the sidebar**, `Section { label: "Agents" }` lists the worker pool — alpha, bravo, charlie on hub; delta, echo, foxtrot on devbox. Each entry shows a tmux slot + its current state. These are *capacity*, not roles.

2. **In conversation and in the orchestrator's prompt**, "agent" is used loosely to mean "the Claude instance running somewhere" — sometimes the orchestrator, sometimes a worker, sometimes a worker spawned with a particular system prompt.

The user means something more specific by "Agent": **an LLM run with a particular prompt and a particular set of skills, distinct from the task it's been handed**. Examples:

- The **Orchestrator** agent (already exists, implicitly) coordinates the kanban board, dispatches workers, applies Zen merge conditions.

- A **Developer** agent picks up a task in `in-progress`, implements it, hands off when done.

- A **QA** agent could pick up tasks in a `qa` status, verify the implementation, surface findings.

- A **Security Review** agent could own a `security-review` status, focusing exclusively on threat-model thinking.

The workflow schema (`workflows/<name>.yaml`) already has the seam — every status has an `owner:` field, currently either `user` or `agent`. Today, statuses with `owner: agent` are handled by "whatever Claude pops out of `claude` with the project's CLAUDE.md as system prompt." We want to make that explicit: each agent-owned status names **which agent runs in it**, and the agent supplies the system prompt + skill set.

This plan splits the two ideas. The sidebar gets renamed to reflect what it actually shows (capacity / persistent slots) — see Vocabulary above — and reorganized to group those slots by their host machine. "Agent" becomes a first-class concept stored on disk, configurable per-project, named directly in the workflow's `owner` field on each status.

## Design

### 1. Sidebar rebrand + reorganization: "Agents" → "Workspaces", grouped by machine

The sidebar `Section { label: "Agents" }` becomes `Section { label: "Workspaces" }` and its contents reorganize into a two-level tree: each machine is a group, each workspace is a row underneath the machine that hosts it. Today's "agents in the sidebar" was a flat list that hid which machine each entry lived on — making `prefers_machine` routing a fact you had to memorize rather than read off the screen.

**Multi-machine project (hub + devbox), default layout:**

```
- Workspaces -
▾ hub
   ▶ alpha     Developer
   ▶ bravo     QA
   · charlie   idle
▾ devbox
   · delta     keybindings-phase-2-platform-aware-help-text   [developer]
   · echo      idle
   · foxtrot   idle
```

The `[developer]` tag is the currently-loaded agent (see §10), surfaced inline so the user can read role + slot in one glance.

**Single-machine project, collapsed:**

```
WORKSPACES
● alpha     idle
● bravo     idle
● charlie   in_progress: <task-id>   [developer]
```

When the project has only one machine, the group header collapses away — the tree degenerates to today's flat list, no setting needed. Group headers appear automatically the moment a second machine is declared in `project.yaml`.

Group headers are themselves focusable: pressing enter on a machine row could navigate to a "machine view" (status, in-flight tasks, log tail), but that's a separate plan — out of scope here.

The word "workspace" matches the persistent-slot-with-worktree mental model better than "worker." A worker is what runs; a workspace is what holds it. The slot persists across task switches, owns a worktree on disk, and is tied to a specific machine — all properties of a workspace. The agent (developer, QA, etc.) is what *runs* inside the workspace; "worker" used to mean both, which is why we're disentangling.

CLI vocabulary (`shelbi worker list`, etc.) is left untouched in v1 — see Open questions about whether to rename in a follow-up.

### 2. The Agent concept

An **Agent** is a directory under the project's settings folder:

```
~/.shelbi/projects/<project>/agents/
├── orchestrator/
│   ├── instructions.md       # the core prompt
│   └── skills/
│       ├── merge-pr-flow.md
│       └── zen-conditions.md
├── developer/
│   ├── instructions.md
│   └── skills/
│       ├── rust-conventions.md
│       └── commit-style.md
└── qa/                       # user-added
    ├── instructions.md
    └── skills/
        └── playwright-tips.md
```

Each agent's directory contains:

- **`instructions.md`** — the system prompt loaded when the agent runs. This becomes the *first thing* the LLM sees, before any task-specific message.

- **`skills/`** — a directory of skill files (Claude Code skill format — `.md` with YAML frontmatter declaring trigger conditions). The agent's skills are auto-loaded when it runs and surface as available skills inside its session.

The directory name (`orchestrator`, `developer`, `qa`) is the agent's stable identifier. Used in the workflow YAML's `owner:` field, in CLI commands, in events log entries.

### 3. Default agents shipped with the binary

Two agents are bundled with `shelbi init` (and re-applied if missing):

- **Orchestrator** — `instructions.md` is what's currently in `crates/shelbi-orchestrator/src/default_orchestrator.md.template`. It moves from being "the embedded CLAUDE.md template" to being "the Orchestrator agent's prompt." Same content, new home. The `skills/` directory ships with whatever distilled flows we want to make discoverable (Zen merge conditions, auto-dispatch contract, etc.).

- **Developer** — `instructions.md` is a small system prompt: "You're a worker on the Shelbi kanban board. You've been handed a task; read the task body, implement it, and write a review marker when done. Follow the conventions in the project's CLAUDE.md and any agent-level skills." The `skills/` directory ships with conventions like commit style, "don't run shelbi yourself," etc.

Both are overridable per-project — see §6 below.

### 4. Workflow status → agent binding

The existing `owner:` field on each status becomes either:

- **`user`** — a human owns this status (triage, review, accept). Reserved sentinel.

- **`<agent-name>`** — the named agent runs when a task is in this status. Must match a directory under `agents/`.

The default workflow becomes:

```yaml
name: default
statuses:
- id: backlog
  name: Backlog
  category: backlog
  owner: user
- id: todo
  name: Todo
  category: ready
  owner: orchestrator    # the orchestrator handles auto-dispatch out of todo
- id: in-progress
  name: InProgress
  category: active
  owner: developer       # the developer agent does the work
- id: review
  name: Review
  category: handoff
  owner: user
- id: done
  name: Done
  category: done
  owner: user
```

Loader validation:

- `owner: user` is always accepted.

- Any other value must match a subdirectory under `agents/`. Loader errors at parse time with a list of available agent names if the value doesn't match.

- Reserved word: `agent` itself becomes invalid as an owner value once this lands (it's no longer a generic placeholder; the agent must be named). The loader auto-migrates legacy `owner: agent` entries — `ready`-category statuses default to `owner: orchestrator`, `active`-category statuses default to `owner: developer` — and emits a one-time deprecation warning telling the user to update the YAML.

For projects with a richer workflow, additional statuses can point at custom agents:

```yaml
- id: qa
  name: QA
  category: handoff   # or 'active' — depends on whether QA blocks done
  owner: qa
- id: security-review
  name: Security Review
  category: handoff
  owner: security-review
```

### 5. Worker dispatch with agents

When the orchestrator dispatches a task to a worker, it now resolves three things:

1. **Which worker?** Same logic as today — first free worker in declaration order, honoring `prefers_machine`.
2. **Which agent?** Look up the task's current status in the workflow; read its `owner:` field. If `user`, no dispatch (this status is human-driven). Otherwise the value IS the agent name.
3. **Spawn the worker pane with that agent's context.**

"Spawning with the agent's context" means: when `shelbi task start` spawns Claude in the worker's tmux pane, it:

- Passes `--system-prompt $(cat agents/<agent>/instructions.md)` (or equivalent).

- Mounts the agent's `skills/` directory into the worker's `.claude/skills/` (symlink or copy) so the skill files are discoverable by Claude Code's skills mechanism.

- Drops the existing project `CLAUDE.md` mechanism for worker spawns — the agent's `instructions.md` is the source of truth. (`CLAUDE.md` stays for the Orchestrator agent until it's fully migrated to `agents/orchestrator/instructions.md`.)

The same worker slot can run different agents on consecutive dispatches. Today switching tasks already clears the worker's context; switching agents is the same flush plus a different system prompt.

### 6. Project overrides + defaults

Three-layer resolution:

1. **Built-in defaults** shipped in the binary. Materialized into `~/.shelbi/projects/<project>/agents/` on `shelbi init` (or when missing — see "agent self-heal" below).
2. **Project-local agents** in `~/.shelbi/projects/<project>/agents/`. The user can edit `instructions.md`, add skills, add new agent directories.
3. **Global agent library** (optional, future) at `~/.shelbi/agents/` — agents available to every project. Falls back to project-local when a project also has an agent of the same name. (Surfaced as an open question; not required for v1.)

Project overrides don't need a "fresh" base — once a user edits `agents/orchestrator/instructions.md`, future binary upgrades don't clobber it. On binary upgrade, the orchestrator checks whether each shipped default has been modified; if so, leave alone, log a one-time notice. If untouched, refresh to the latest shipped default.

This matches how `CLAUDE.md` is treated today (shipped on init, then owned by the project).

### 7. Skills

Skills follow the existing Claude Code skill convention: a Markdown file with YAML frontmatter declaring when the skill should activate. Example `agents/developer/skills/commit-style.md`:

```markdown
---
name: shelbi-commit-style
description: Use when authoring a commit message at the end of a task — Shelbi PRs squash to a single commit, so the worker's commit subject becomes the PR title.
---

Commit subjects: lowercase prefix (`tui:`, `state:`, `docs:`, etc.) followed by an
imperative-mood short description. Body explains the *why*, not the *what*. ...
```

When the agent runs, its `skills/` directory is exposed to Claude Code's skill loader. The LLM sees the skills as installable/triggerable per the description's match criteria.

Agents can share skills by symlinking — no separate "library" mechanism in v1.

### 8. The Orchestrator agent's special status

The Orchestrator agent runs differently from worker agents in two ways:

1. **Persistent pane.** A worker dispatches and clears between tasks; the orchestrator runs continuously in its own tmux session and reacts to events.
2. **Doesn't own a single status.** It owns the *transitions out of* multiple statuses (auto-promote out of backlog under Zen, auto-dispatch out of todo, run Zen merge conditions out of review). The `owner: orchestrator` field on `todo` is a *convenience* — the orchestrator runs whether or not todo lists it explicitly, since the orchestrator IS the dispatch loop.

Plan position: keep both — let `todo` (and any other "transient ready" statuses) declare `owner: orchestrator` for documentation purposes. The orchestrator process itself runs always. The CLI command `shelbi agent show orchestrator` should make this special status visible.

### 9. CLI surface

Three new commands under `shelbi agent`:

- **`shelbi agent list`** — prints every agent in the current project's `agents/` directory, columnar: name, status assignments from the workflow (which statuses have `owner: <this-agent>`), skill count, modified-since-default indicator.

  ```
  AGENT          STATUSES         SKILLS  CUSTOMIZED
  orchestrator   todo (special)   8       yes
  developer      in-progress      3       no
  qa             qa, security-r.  5       yes
  ```

- **`shelbi agent show <name>`** — prints the agent's `instructions.md` plus a list of its skills with their descriptions. Useful for understanding what an agent does without opening files.

- **`shelbi agent new <name>`** — scaffolds a new agent directory with an empty `instructions.md` (with a documented frontmatter / header), an empty `skills/` dir, and prints a hint about how to bind it to a workflow status (set `owner: <name>` on a status in `workflows/<workflow>.yaml`).

A fourth, optional: **`shelbi agent edit <name>`** — opens the agent's `instructions.md` in `$EDITOR` (mirrors `shelbi workflow edit` if that exists). Skip in v1 if not needed.

### 10. Events + observability

New event line shape for agent-driven dispatches:

```
<ts> task=<id> ready -> active reason=orchestrator:auto-dispatch_worker=alpha_agent=developer
```

The `agent=<name>` field appears on every event where a worker is spawned. The activity feed surfaces it as a small badge or tag next to the worker name, so the user can see at a glance which role each worker is playing. Yes, the agent name is already derivable from the status's `owner` field — but having it on the dispatch event keeps the feed self-contained (no need to cross-reference the workflow YAML to know which agent was running).

`shelbi worker list` gains an "agent" column (replacing or augmenting the current "claude" column, which is the runtime):

```
NAME      HOST    AGENT          STATE
alpha     hub     developer      in_progress: <task-id>
bravo     hub     -              idle
charlie   hub     developer      in_progress: <task-id>
delta     devbox  qa             in_progress: <task-id>
```

When idle, the agent column shows `-`; when running, it shows the agent that was loaded for the current task.

## Rollout

Two phases. Each is independently shippable; v1 does the rebrand + introduces the abstraction; v2 turns on the per-status binding for richer workflows.

**Phase 1 — Rebrand + Orchestrator + Developer.**

- Rename sidebar section "Agents" → "Workers".

- Add `~/.shelbi/projects/<project>/agents/` to the project layout. Materialize `orchestrator/` and `developer/` on `shelbi init` (and self-heal on `shelbi reload` if either is missing).

- Move the embedded orchestrator prompt from `crates/shelbi-orchestrator/src/default_orchestrator.md.template` into `agents/orchestrator/instructions.md` (the shipped default). The template still exists in the binary for init/self-heal.

- Extend the workflow loader to accept any agent name in the `owner:` field (not just `user` / `agent`). Validate against the project's `agents/` directory. Auto-migrate legacy `owner: agent` entries with a deprecation warning. Default workflow gets `owner: orchestrator` on `todo` and `owner: developer` on `in-progress`.

- Update `shelbi task start`'s worker-spawn path to load the agent's `instructions.md` as system prompt and mount its `skills/` into `.claude/skills/`.

- Add the `agent=<name>` field to dispatch event lines.

- Add the "agent" column to `shelbi worker list`.

- Add `shelbi agent list` and `shelbi agent show`.

After Phase 1: the abstraction exists and the default workflow uses it, but visibly nothing changes for users who don't customize agents. The Developer agent's behavior matches today's "default Claude on a worker."

**Phase 2 — Custom agents + workflow integration polish.**

- Add `shelbi agent new` to scaffold custom agents.

- Document custom-agent patterns (a QA agent for a custom workflow; a Security Review agent gated to specific paths).

- Update the activity feed to surface `agent=<name>` badges on dispatch / handoff lines.

- Refine the orchestrator self-heal on binary upgrade (detect modified-from-default, leave alone; refresh if untouched).

- Optional: `shelbi agent edit <name>` opens in `$EDITOR`.

- Optional: global agent library at `~/.shelbi/agents/` (cross-project), if real usage demands it.

After Phase 2: custom workflows + custom agents are fully composable. A user can drop in a QA agent, wire it to a `qa` status, and have every task pass through their custom verification gate.

## Decisions

- **Sidebar rebrand + reorganize: "Agents" → "Workspaces", grouped by machine.** Frees the word "Agent" for the new concept and aligns with the persistent-slot mental model (each workspace = one pane + one git worktree on a specific machine). Group headers collapse to a flat list when the project has only one machine. CLI vocabulary stays as `shelbi worker *` in v1 — rename deferred (see Open questions). Vocabulary: workspace = slot, agent = role, task = work.

- **Agent storage:** **`~/.shelbi/projects/<project>/agents/<name>/`** containing `instructions.md` and `skills/`. Mirrors the workflows folder layout.

- **Default agents: Orchestrator + Developer**, shipped in the binary and materialized into the project on init / self-healed on reload. Editable per-project; binary upgrade doesn't clobber edits.

- **Workflow binding via the existing** **`owner:`** **field.** Value is either the reserved sentinel `user` (human-owned) or a named agent (matching a `agents/<name>/` directory). One field, not two. Legacy `owner: agent` auto-migrates with a deprecation warning.

- **Worker spawn loads agent's** **`instructions.md`** **as system prompt** and mounts the agent's `skills/` into `.claude/skills/`. The same worker slot runs different agents on consecutive dispatches.

- **Orchestrator agent is special** — runs persistently on its own pane, not per-task-dispatch. `todo` declaring `owner: orchestrator` is documentation; the orchestrator process runs independent of the workflow's status declarations.

- **Skills format follows Claude Code's existing convention** — `.md` with YAML frontmatter declaring trigger criteria. No new skill format to learn.

- **CLI surface:** **`shelbi agent list / show / new`** for v1. `edit` deferred to v2 if needed.

- **Events log gains** **`agent=<name>`** **field** on dispatch events (redundant with the status's owner, but keeps the feed self-contained). `shelbi worker list` gains an "agent" column showing the currently-loaded agent (or `-` when idle).

- **The "claude" column in worker list = the runtime.** Stays as-is for v1 (rename later if it becomes confusing). New "agent" column is additive, not a replacement.

## Open questions

- **CLI vocabulary follow-through:** **`shelbi worker *`** **→** **`shelbi workspace *`?** The sidebar rename (Workers → Workspaces) implies a CLI rename for consistency: `shelbi worker list` → `shelbi workspace list`, `shelbi worker stop` → `shelbi workspace stop`, and the `worker:*` event reason prefixes too. Cost: every CLAUDE.md / orchestrator-prompt / doc reference has to follow, plus a deprecation alias period for users with muscle memory. Benefit: one vocabulary across UI, CLI, events, and prose. Lean toward doing it as a v2 follow-up after the sidebar reorg has lived long enough to confirm the workspace framing actually feels right.

- **Per-workspace preferred agent?** Should a workspace declare `prefers_agent: developer` so the orchestrator routes that workspace to developer-statuses when possible? Useful if certain hosts have tools or auth only Developer needs. Probably overkill for v1 — defer.

- **Global agent library at** **`~/.shelbi/agents/`?** Cross-project agents (a single "Security Review" agent reused across every project). Cheap to add but no current user demand. Defer until someone asks.

- **`CLAUDE.md`** **migration path.** Today the project's `CLAUDE.md` is the orchestrator's prompt (mixing orchestrator content with project-specific overrides). After Phase 1, the orchestrator agent's prompt lives in `agents/orchestrator/instructions.md`. What's left for `CLAUDE.md`? Maybe project-wide context for *all* agents (the "you're working in the Shelbi monorepo, here's the layout" preamble), prepended to whatever agent runs. Worth a separate design pass before Phase 1 lands.

- **Skill inheritance / composition.** If a project has both a `developer` agent (with its own skills) AND ships some skills at `~/.shelbi/skills/` (global), do the agent's skills override or compose? Lean toward compose, agent-skills-win-on-conflict. Out of scope for v1.

- **Worker** **`agent`** **column name vs** **`claude`** **column.** Today the third column of `shelbi worker list` is "claude" (the runtime). The new "agent" column is the role. Adjacency might be confusing — should the runtime column rename to "runtime" or stay "claude"? Lean toward keep as-is for v1; rename later if real users get confused.

- **Reserved owner names.** `user` is reserved (means human). Should `agent` (the old generic value) also be reserved as forever-invalid, or just deprecated-then-removed? Current plan: deprecated-with-auto-migration in Phase 1, hard-reject in a later release. Open to making it a hard error from day one.
