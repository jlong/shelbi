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
   ▶ delta     Developer
   · echo      idle
   · foxtrot   idle
```

The agent name (e.g. `Developer`, `QA`) is the currently-loaded agent (see §10), surfaced inline so the user can read role + slot in one glance. The `▶` glyph marks an active workspace (a task is dispatched); `·` marks an idle workspace.

**Single-machine project, collapsed:**

```
- Workspaces -
▶ alpha     Developer
▶ bravo     QA
· charlie   idle
```

When the project has only one machine, the group header collapses away — the tree degenerates to today's flat list, no setting needed. Group headers appear automatically the moment a second machine is declared in `project.yaml`.

Group headers are themselves focusable: pressing enter on a machine row could navigate to a "machine view" (status, in-flight tasks, log tail), but that's a separate plan — out of scope here.

The word "workspace" matches the persistent-slot-with-worktree mental model better than "worker." A worker is what runs; a workspace is what holds it. The slot persists across task switches, owns a worktree on disk, and is tied to a specific machine — all properties of a workspace. The agent (Developer, QA, etc.) is what *runs* inside the workspace; "worker" used to mean both, which is why we're disentangling.

CLI vocabulary follows in v1 — `shelbi worker *` renames to `shelbi workspace *` with deprecation aliases for one release. See Rollout and CLI compatibility for the per-command breakdown.

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

- **Developer** — `instructions.md` is a small system prompt: "You're working in a Shelbi workspace. You've been handed a task; read the task body, implement it, and write a review marker when done. Follow project conventions in `agents/_shared/preamble.md` (if present) and any agent-level skills." The `skills/` directory ships with conventions like commit style, "don't run shelbi yourself," etc.

Both are overridable per-project — see §6 below.

### 4. Workflow status → agent binding

Each status declares two related fields:

- **`owner: user | agent`** — whose responsibility this status is when automation is OFF. `user` means a human acts (triage, review, accept); `agent` means the orchestrator dispatches automatically (e.g. out-of-todo work). Binary — exactly two valid values. This is what shows up in any "do I need to look at this?" filter.

- **`agent: <agent-name>`** *(optional)* — which agent is empowered to act when automation IS on. Names a directory under `agents/`. The field is what makes Zen behavior **declarative**: a `user`-owned status with an `agent:` value means "under Zen, this agent can do the work without me." A status with no `agent:` has no automation path — even Zen leaves it alone.

The split moves Zen behavior from orchestrator-prompt prose into the workflow schema. Today CLAUDE.md tells the orchestrator "under Zen, auto-promote backlog and run merge-conditions on review." After this, `backlog` and `review` simply declare `agent: orchestrator` and the orchestrator enacts what the workflow says. Per-status Zen ("auto-merge but don't auto-promote") becomes a YAML edit, not a prompt change. Each project picks its own Zen surface.

The default workflow:

```yaml
name: default
statuses:
- id: backlog
  name: Backlog
  category: backlog
  owner: user
  agent: orchestrator    # Zen: orchestrator auto-promotes per judgment categories
- id: todo
  name: Todo
  category: ready
  owner: agent
  agent: orchestrator    # always: orchestrator dispatches
- id: in-progress
  name: InProgress
  category: active
  owner: agent
  agent: developer       # the developer agent does the work
- id: review
  name: Review
  category: handoff
  owner: user
  agent: orchestrator    # Zen: orchestrator runs merge-conditions
- id: done
  name: Done
  category: done
  owner: user
  # terminal — no agent, no automation
- id: canceled
  name: Canceled
  category: archived
  owner: user
  # terminal — no agent, no automation
```

Loader validation:

- `owner` must be exactly `user` or `agent`. Any other value is a parse error.

- `agent` (when present) must match a subdirectory under `agents/`. The loader errors at parse time with the list of available agent names if it doesn't match.

- **`owner: agent`** **without an** **`agent:`** **field is a hard error** — the whole point of the split is to make automation explicit. Diagnostic: "status '<id>' has `owner: agent` but no `agent:` field — which agent should run here?"

- `owner: user` without an `agent:` field is fine; means "no automation for this status, period." Terminal states (`done`, `canceled`) typically take this shape.

Legacy migration: existing workflows that use the single-field design (`owner: agent` alone, or `owner: <agent-name>`) auto-migrate with a one-time deprecation warning:

- `owner: agent` alone → defaults `agent:` by category (`ready` → `orchestrator`, `active` → `developer`, anything else → error).

- `owner: <name>` (the abandoned named-owner design) → rewrites to `owner: agent, agent: <name>`.

For projects with a richer workflow, custom agents bind via the same two fields:

```yaml
- id: qa
  name: QA
  category: handoff   # or 'active' — depends on whether QA blocks done
  owner: user         # human signs off normally
  agent: qa           # Zen: qa agent runs the verification gate
- id: security-review
  name: Security Review
  category: handoff
  owner: user
  agent: security-review
```

### 5. Worker dispatch with agents

When the orchestrator dispatches a task to a worker, it now resolves three things:

1. **Which workspace?** Same logic as today — first free workspace in declaration order, honoring `prefers_machine`.
2. **Which agent?** Look up the task's current status in the workflow. Check `owner:` — if `user` AND no automation mode is active (Zen off), no dispatch (this status is human-driven by design). Otherwise read the `agent:` field; that's the agent to spawn. If `owner: user` and the status has no `agent:` field, no dispatch even under Zen — that status is fully human-driven.
3. **Spawn the workspace pane with that agent's context.**

"Spawning with the agent's context" means: when `shelbi task start` spawns Claude in the workspace's tmux pane, it:

- Passes `--system-prompt $(cat agents/<agent>/instructions.md)` (or equivalent).

- Mounts the agent's `skills/` directory into the workspace's `.claude/skills/` (symlink or copy) so the skill files are discoverable by Claude Code's skills mechanism.

- Drops the existing project `CLAUDE.md` mechanism for workspace spawns — the agent's `instructions.md` is the source of truth. (`CLAUDE.md` stays for the Orchestrator agent until it's fully migrated to `agents/orchestrator/instructions.md`.)

The same workspace slot can run different agents on consecutive dispatches. Today switching tasks already clears the workspace's context; switching agents is the same flush plus a different system prompt.

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
<ts> task=<id> ready -> active reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer
```

Two name changes from today: `worker=` becomes `workspace=` (matches the v1 vocabulary rename), and the new `agent=<name>` field appears on every event where a workspace is spawned. The activity feed surfaces the agent inline as a small badge or tag next to the workspace name, so the user reads role + slot in one glance. Yes, the agent name is already derivable from the status's `agent:` field — but having it on the dispatch event keeps the feed self-contained.

`shelbi workspace list` (renamed from `shelbi worker list` in v1) restructures its columns:

```
NAME      HOST    MODEL           AGENT          STATE
alpha     hub     opus-4-7        developer      in_progress: <task-id>
bravo     hub     opus-4-7        -              idle
charlie   hub     opus-4-7        developer      in_progress: <task-id>
delta     devbox  sonnet-4-6      qa             in_progress: <task-id>
```

- `MODEL` replaces today's `claude` column. More generic name future-proofs for non-Claude runtimes and disambiguates from the new `AGENT` column.

- `AGENT` is new. Shows `-` when the workspace is idle; the agent name (matching a directory under `agents/`) when a task is dispatched.

Event parsers should accept both `worker=` and `workspace=` for one release (deprecation window for the rename).

## Rollout

Two phases. Each is independently shippable; v1 does the rebrand + introduces the abstraction + completes the CLI rename; v2 polishes for custom workflows.

**Phase 1 — Rebrand + Orchestrator + Developer + CLI rename.**

- Rename sidebar section "Agents" → "Workspaces" (grouped-by-machine tree per §1).

- **Rename** **`shelbi worker *`** **→** **`shelbi workspace *`** **across the entire surface** — CLI command, all flags' help text, every CLAUDE.md / doc / orchestrator-prompt reference, the `worker:*` event-line reason prefixes. `shelbi worker *` stays as a deprecation alias for one release with a one-line stderr nag.

- Add `~/.shelbi/projects/<project>/agents/` to the project layout. Materialize `orchestrator/` and `developer/` on `shelbi init` (and self-heal on `shelbi reload` if either is missing).

- Move the embedded orchestrator prompt from `crates/shelbi-orchestrator/src/default_orchestrator.md.template` into `agents/orchestrator/instructions.md` (the shipped default). Template still exists in the binary for init/self-heal.

- **Deprecate** **`CLAUDE.md`.** Project-wide context moves into `agents/_shared/preamble.md` (prepended to every agent's prompt); orchestrator-specific overrides go into `agents/orchestrator/instructions.md`. The orchestrator stops auto-loading `CLAUDE.md`; if a project still has one, emit a one-time migration hint pointing at the new locations.

- Extend the workflow loader for the two-field design: `owner: user | agent` + optional `agent: <name>`. Hard-fail if `owner: agent` without `agent:`. Auto-migrate legacy single-field workflows (`owner: <name>` → `owner: agent, agent: <name>`; bare `owner: agent` → category-defaulted `agent:`) with a one-time deprecation warning.

- Update `shelbi task start`'s spawn path: load the agent's `instructions.md` as system prompt (prepended with the project's `agents/_shared/preamble.md` if present), mount `skills/` into `.claude/skills/`.

- Add the `agent=<name>` field to dispatch event lines. Rename `worker=<name>` → `workspace=<name>` in events (with the parser tolerating the old form for one release).

- Restructure `shelbi workspace list` columns: `NAME`, `HOST`, `MODEL`, `AGENT`, `STATE`. (`MODEL` replaces today's `claude` column with a more general name; `AGENT` is new.)

- Add `shelbi agent list` and `shelbi agent show`.

After Phase 1: the abstraction exists, the default workflow uses it, the CLI vocabulary is fully migrated, and Zen behavior is declarative. Users running plain dispatcher mode see the column changes and the sidebar reorg; nothing else visibly changes.

**CLI compatibility — v1 promises to existing** **`shelbi worker *`** **users.**

- **Every** **`shelbi worker <subcommand>`** **invocation keeps working** as a deprecation alias that resolves to `shelbi workspace <subcommand>` and prints a one-line stderr hint pointing at the new name. Aliases stay for at least one release; remove in v2.

- `shelbi workspace list` (the new canonical name) — columns are `NAME` / `HOST` / `MODEL` / `AGENT` / `STATE`. The old `claude` column is replaced by `MODEL`; `AGENT` is new (shows `-` when idle).

- `shelbi workspace stop <name>` — same semantics as `shelbi worker stop` today.

- Event-log reasons: `worker:*` prefixes rename to `workspace:*`. Parsers should accept both for one release.

- **New parallel surface:** **`shelbi agent list / show`** (and `new` in Phase 2). Operates on the agent concept, not workspaces.

**Phase 2 — Custom agents + workflow integration polish.**

- Add `shelbi agent new` to scaffold custom agents.

- Document custom-agent patterns (a QA agent for a custom workflow; a Security Review agent gated to specific paths).

- Update the activity feed to surface `agent=<name>` badges on dispatch / handoff lines.

- Refine the orchestrator self-heal on binary upgrade (detect modified-from-default, leave alone; refresh if untouched).

- Drop the `shelbi worker *` deprecation aliases and the `worker=` event-line parser fallback.

- Optional: `shelbi agent edit <name>` opens in `$EDITOR`.

After Phase 2: custom workflows + custom agents are fully composable. A user can drop in a QA agent, wire it to a `qa` status, and have every task pass through their custom verification gate.

## Decisions

- **Sidebar rebrand + reorganize: "Agents" → "Workspaces", grouped by machine.** Frees the word "Agent" for the new concept and aligns with the persistent-slot mental model (each workspace = one pane + one git worktree on a specific machine). Group headers collapse to a flat list when the project has only one machine. Vocabulary: workspace = slot, agent = role, task = work.

- **CLI rename to** **`shelbi workspace *`** **in v1.** The sidebar rename pulls the CLI rename forward: `shelbi worker list` → `shelbi workspace list`, etc. `shelbi worker *` stays as a deprecation alias for one release with a one-line stderr nag. Event-line `worker=<name>` renames to `workspace=<name>` with parser fallback for one release.

- **Agent storage:** **`~/.shelbi/projects/<project>/agents/<name>/`** containing `instructions.md` and `skills/`. Mirrors the workflows folder layout.

- **Default agents: Orchestrator + Developer**, shipped in the binary and materialized into the project on init / self-healed on reload. Editable per-project; binary upgrade doesn't clobber edits.

- **Workflow binding: two fields,** **`owner: user | agent`** **+ optional** **`agent: <name>`.** `owner` is the binary "whose responsibility under no-automation"; `agent` names which agent acts when automation (Zen, etc.) is on. Decouples responsibility from automation, so a `user`-owned status can still have an agent-driven Zen path (e.g. `review: owner: user, agent: orchestrator` for auto-merge). Hard-fail if `owner: agent` without `agent:`. Legacy single-field workflows auto-migrate with a deprecation warning. Net effect: Zen behavior becomes declarative data in the workflow YAML instead of orchestrator-prompt prose.

- **Workspace spawn loads agent's** **`instructions.md`** **as system prompt** (prepended with `agents/_shared/preamble.md` if the project has one) and mounts the agent's `skills/` into `.claude/skills/`. The same workspace slot runs different agents on consecutive dispatches.

- **Deprecate** **`CLAUDE.md`.** Project-wide context for all agents moves into `agents/_shared/preamble.md`; orchestrator-specific overrides go into `agents/orchestrator/instructions.md`. Removes the special-case file and unifies how agents source their context. v1 still reads an existing `CLAUDE.md` if present (with a one-time migration hint); v2 drops the read path.

- **Orchestrator agent is special** — runs persistently on its own pane, not per-task-dispatch. Statuses declaring `agent: orchestrator` are declarative documentation of what the orchestrator does; the orchestrator process itself runs always.

- **Skills format follows Claude Code's existing convention** — `.md` with YAML frontmatter declaring trigger criteria. No new skill format to learn.

- **CLI surface:** **`shelbi agent list / show`** in v1; `new` in v2; `edit` deferred (open question).

- **Events log gains** **`agent=<name>`** **field** on dispatch events (redundant with the status's `agent:`, but keeps the feed self-contained).

- **`shelbi workspace list`** **columns:** **`NAME`,** **`HOST`,** **`MODEL`,** **`AGENT`,** **`STATE`.** Replaces today's `claude` column with `MODEL` (more generic, future-proofs for non-Claude runtimes); adds new `AGENT` column for the loaded role.

## Open questions

Two questions remain genuinely open; everything else has been folded into Decisions.

**Deferred to v2 (not blockers, no current demand):**

- **Per-workspace preferred agent?** Should a workspace declare `prefers_agent: developer` so the orchestrator routes that workspace to matching statuses when possible? Useful if certain hosts have tools or auth only one agent needs. Deferred — no concrete use case yet; pairs naturally with `prefers_machine` if it ships.

- **Global agent library at** **`~/.shelbi/agents/`?** Cross-project agents (one "Security Review" agent reused everywhere). Deferred — no cross-project sharing demand yet; revisit when someone hits the pain of maintaining the same agent in N projects. (Also blocks the related "skill inheritance / composition" question, since there's nothing to compose with until global agents exist.)

**Still genuinely open:**

- **`shelbi agent edit <name>`** — opens the agent's `instructions.md` in `$EDITOR` (mirroring `shelbi workflow edit` if/when that exists). Trivial to ship; only question is whether it's necessary. Deferred to v2 by default; revisit if users ask.

- **Sidebar mockup glyph legend.** §1's tree mockup uses `▶` for active workspaces and `·` for idle; the prose doesn't yet name those glyphs. Worth a one-liner legend or sticking with "infer from context." (Not blocking; cosmetic.)
