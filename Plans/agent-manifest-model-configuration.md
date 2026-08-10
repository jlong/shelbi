# Agent Manifest & Model Configuration

Builds on **\[\[agents-workspaces]]**, which established the vocabulary: a
*workspace* is capacity (a machine slot: one tmux pane + one worktree), an
*agent* is a role (a system prompt + skill set), a *task* is work. A dispatch
reads *"run task T using agent A in workspace W."* This plan gives the **agent**
its own on-disk configuration — specifically its LLM runner and model — and sets
up agents to eventually be distributable packages.

## Context

Two gaps today, both stemming from the same root: an agent (role) has no
runner-agnostic config of its own.

1. **Model/runner config lives on the** ***workspace***\*\*, not the agent.\*\* A
   `Workspace` carries `runner: String` (`crates/shelbi-core/src/model.rs:916`),
   which names an entry in the project's `agent_runners` map
   (`AgentRunnerSpec { command, flags, prompt_injection, dialog_signatures, integration }`). There is **no** **`model`** **field** — the model is baked into
   `flags` (e.g. `flags: ["--model", "opus"]`). At dispatch the workspace's
   runner is resolved (`crates/shelbi-orchestrator/src/workspace.rs:1452`)
   regardless of which agent-role runs there. So you **cannot** express "the
   `review` agent runs on Opus while `developer` runs on Sonnet" — the model is
   a property of the *slot*, when it should be a property of the *role*.

2. **The only per-agent file is Claude-specific.** Each `agents/<name>/`
   directory holds `instructions.md` (system prompt), `skills/`, and a
   `settings.json` — but that `settings.json` is a **Claude Code hooks** file
   (`SessionStart`/`Stop`/`Notification`/`UserPromptSubmit`/`PreToolUse` →
   `.shelbi/hooks/*.sh`) that shelbi needs for pane-state detection. It is inert
   under a non-Claude runner and is the wrong home for runner-agnostic config.

**Longer arc.** Agents should become distributable **packages** in a Shelbi
marketplace: an author publishes an `adversarial-reviewer` agent, you install it
into a project. A package ships with the author's *recommended* runner and model
("I was written for Opus with high reasoning effort"), and the **consuming
project overrides** those recommendations to fit its own runner fleet ("actually
run the orchestrator on Codex"). This framing drives the two load-bearing
decisions below: the config file is a **manifest** (identity + version +
declared preferences), and the project layer sits **above** the manifest in
precedence.

## Design

### 1. `agent.yaml` — the agent manifest

A new runner-agnostic file at `agents/<name>/agent.yaml`, sitting alongside the
existing `instructions.md`, `skills/`, and (Claude-only) `settings.json`.

```yaml
# agents/adversarial/agent.yaml
name: adversarial-reviewer          # package identity (stable across installs)
version: 1.2.0                      # manifest version (semver)
description: Adversarial code review before a human sees the branch

preferred_runner: claude            # which runners: block is the default (a KIND, see §2)
runners:                            # per-runner-kind config; the manifest is multi-runner-first
  claude:
    model: claude-opus-4-8          # logical model id, applied per-adapter (see §3)
    reasoning_effort: high          # per-runner value; travels with this runner (see §3)
  codex:
    model: gpt-5
    reasoning_effort: high

permissions_mode: read-only         # a request; project caps it, never escalated up (see §6)

requires:                           # compatibility hints, enforced at install/dispatch
  runner_kinds: [claude, codex]     # which runner kinds this agent can actually run on
  shelbi: ">=0.8"
```

Notes on the fields:

- **`name`** is the package identity, independent of the install directory
  (like `package.json`'s `name`). The directory name remains how a *project*
  addresses the agent (in a status `agent:` field); `name` is how the
  *marketplace* addresses it. They may differ (installed as `review`, published
  as `acme/adversarial-reviewer`).

- **`preferred_runner`** **+ per-kind** **`runners:`** **blocks.** The manifest is
  *multi-runner-first*: it declares a `runners` map keyed by runner kind, each
  block carrying that kind's `model` and `reasoning_effort`, and
  `preferred_runner` names which block is the default. Every value is a
  recommendation the project can override (§4) — a project may swap
  `preferred_runner`, override a block, or add a block for a kind the author
  didn't ship. This also reads honestly for the availability case: a preferred
  runner kind may simply not be installed, in which case the project's chosen
  kind (or another declared block) applies.

- **`description`** feeds both docs and the sidebar/marketplace UI.

### 2. `preferred_runner` names a runner *kind*, not a project-local runner

A distributable package **cannot** reference a project's `agent_runners` keys —
the author has no idea you named your runner `my-claude`. So `preferred_runner`
(and any project override of it) names a canonical **runner kind**: `claude` /
`codex` / `generic` — the same `RunnerKind` classification that the launch-flag
assembly, submit profile, readiness probe, resume strategy, and hook wiring
already key off (`crates/shelbi-core/src/model.rs:1146` `RunnerKind`).

The project maps a kind to a concrete `agent_runners` entry (which carries the
`command`, `flags`, prompt-injection, etc.). If a project declares exactly one
runner of a kind, the mapping is automatic; with several, the project picks
(see §5). This keeps packages portable across projects that name their runners
differently.

### 3. Model *and* effort application is per-runner-adapter

A `model` like `claude-opus-4-8` is a **logical** value; turning it into a launch
flag is runner-specific (`--model` for Claude/Codex; something else, or nothing,
for a generic runner). The same is true of `reasoning_effort` (Claude thinking
budget vs Codex effort) — both are per-runner values, which is why they live
*inside* each `runners:` block rather than as top-level scalars. So each block
carries the *values* and the resolved runner's adapter (`RunnerKind` /
`shelbi_agent::RunnerAdapter`) carries the *how*. This is the same seam that
already owns launch-flag assembly (`with_permission_mode`, `with_continue`), so
model and effort injection become adapter responsibilities rather than strings
spliced into `agent_runners.flags` by hand.

Consequence: because the manifest is multi-runner-first, `model` and
`reasoning_effort` **travel together per runner kind**. Overriding an agent from
Claude onto Codex selects the `codex` block (its own `gpt-5` + effort) rather
than leaving a stale `claude-opus-4-8` behind; an override that introduces a new
kind supplies that block's `(model, reasoning_effort)` together.

### 4. Resolution / precedence

The override chain, highest to lowest:

```
task/status override  →  project agents.<name> override  →  agent.yaml preferred_*  →  built-in default
```

- **task/status override** — a specific dispatch may pin a runner/model (rare;
  e.g. a one-off "run this on the big model"). Most specific, wins.

- **project** **`agents.<name>`** **override** — the consuming project's say (§5). This
  is what makes `preferred_` "preferred": the project overrides the package.

- **`agent.yaml`** **preferred\_\*** — the package author's recommendation.

- **built-in default** — shelbi's fallback when nothing above resolves (and the
  graceful-degradation target when a preferred runner kind isn't installed on
  the chosen machine).

`Workspace.runner` is **retained as a fallback only through the migration
window**, then removed (§8). While present it sits below the agent layer: when an
agent resolves a runner it wins; when it doesn't, the workspace's runner still
applies. The end state assumes a **uniform fleet** — every machine has every
runner kind installed — so once agents own selection, runner availability needs
no per-workspace field (§8).

### 5. Project-side overrides: `agents.<name>` in `project.yaml`

```yaml
# project.yaml
runner_kinds:               # project-level kind -> concrete agent_runners entry
  claude: my-claude         # only needed when a kind maps to MORE THAN ONE entry
agents:
  orchestrator:
    preferred_runner: codex     # swap the default runner kind...
    runners:
      codex:
        model: gpt-5            # ...and set that kind's model/effort
  review:
    runners:
      claude:
        model: claude-opus-4-8  # keep the kind, bump just the model
```

Only the keys a project wants to change appear; everything else falls through to
the manifest (block-level merge — a `review.runners.claude.model` override leaves
the manifest's `reasoning_effort` intact).

**Runner-kind → concrete-runner mapping (resolved decision Q3).** A kind resolves
to a concrete `agent_runners` entry by a layered rule, specific-overrides-general
like §4: (1) if exactly one runner of a kind exists, the mapping is automatic;
(2) with several, a project-level `runner_kinds:` default per kind resolves it;
(3) an individual `agents.<name>` override may still name the concrete entry to
disambiguate further. Most projects only ever hit (1).

### 6. `permissions_mode`: a ceiling, not a free override

Unlike model (taste), permissions is a security boundary, so it resolves with
**opposite** asymmetry from §4:

- A project sets a **ceiling** (e.g. project cap = `auto-edit`).

- An agent-role may request **equal or tighter** (`review` → `read-only`) and
  gets it.

- An agent-role (or an installed third-party package) may **never escalate**
  past the project ceiling. A package requesting `full-access` against a
  `read-only` project cap is clamped to `read-only`, not granted.

This is the one field where the project always wins the "loosen" direction. It
supersedes today's project-wide `workspace_permissions_mode` for agent-owned
statuses (that setting becomes the ceiling).

### 7. Relationship to the other per-agent files

`agent.yaml` is the runner-agnostic layer. The rest stays:

| File              | Role                                                 | Runner scope    |
| ----------------- | ---------------------------------------------------- | --------------- |
| `agent.yaml`      | identity, runner/model/effort, permissions, requires | agnostic        |
| `instructions.md` | system prompt                                        | agnostic        |
| `skills/`         | agent-scoped skills                                  | agnostic        |
| `settings.json`   | Claude Code hooks (pane-state detection)             | **Claude only** |

Future generalization (out of scope here, noted for direction): if non-Claude
runners grow an equivalent hook contract, `settings.json` becomes
`settings/<kind>.json` selected by the resolved runner kind. `agent.yaml` is the
stable anchor that makes that possible.

### 8. Migration

- **Additive first, then a removal (resolved decision Q5).** Phase 1 is
  additive: `agent.yaml` resolves above `Workspace.runner`, and existing projects
  keep working with no manifest present. Phase 2, once dogfooding proves the
  resolution chain, **removes** **`Workspace.runner`** **outright** — the agent/project
  layers become the sole runner source.

- **Uniform-fleet assumption (resolved decision Q5a).** Removing
  `Workspace.runner` deletes the only field that recorded which runner kinds are
  installed on a machine. For this milestone we assume a **uniform fleet** (every
  machine has every runner kind), so dispatch resolves purely from agent/project
  and never checks availability — trivially true for today's all-Claude hub
  slots. **Follow-up (filed, not this milestone):** if heterogeneous machines
  appear, reintroduce availability as an explicit machine/workspace **capability
  declaration** (`runner_kinds: [claude, codex]`), keyed off the per-kind
  `CapabilityLadder` that already exists — the same moment the per-kind manifest
  blocks (§2) start earning their keep on foreign fleets.

- **Dogfood before codify** (per project convention): add
  `agents/*/agent.yaml` to *this* project first and prove the resolution chain
  live, then a held task codifies the manifest schema, the runner-kind mapping,
  and the resolution order into the shipped scaffold
  (`crates/shelbi-core/src/scaffold.rs`) and domain model.

- Back-compat for model-in-flags: an `agent_runners` entry that still bakes
  `--model` into `flags` keeps working; a resolved `agent.yaml` model, when
  present, takes precedence via the adapter (§3).

### 9. Marketplace implications (direction, not this milestone)

The manifest is the seam that makes agents installable:

- **Versioning** (`version`, `requires.shelbi`) lets an install fail fast on an
  incompatible host.

- **`requires.runner_kinds`** lets a project reject an agent it can't run (no
  matching runner installed) at install time rather than at dispatch.

- An `shelbi agent add <package>` install flow would drop
  `instructions.md` + `skills/` + `agent.yaml` (+ optional `settings.json`) into
  `agents/<name>/`, then surface any `requires` gaps for the user to resolve.

- Publishing is the inverse: an agent directory with a complete `agent.yaml` is
  a publishable unit.

## Resolved decisions (2026-08-10)

Worked through with the maintainer; each open question is now settled and folded
into the design above.

1. **Reasoning effort → per-runner values.** Effort lives *inside* each
   `runners:` block alongside `model` and travels with its runner kind, rather
   than a single shared ordinal scale. Reflected in §1 and §3.
2. **Multi-runner manifests → per-kind blocks from v1.** The manifest is
   multi-runner-first: a `runners:` map keyed by runner kind, each block carrying
   `model` + `reasoning_effort`, with `preferred_runner` naming the default.
   Reflected in §1–§3, §5.
3. **Runner-kind → concrete-runner mapping → layered.** Automatic when a kind
   has exactly one runner; a project-level `runner_kinds:` default when several;
   a per-`agents.<name>` concrete-entry override to disambiguate further.
   Reflected in §5.
4. **Naming → keep both.** Directory name stays the project-local handle (status
   `agent:` field) and canonical key; manifest `name:` is the global package id.
   Display prefers the manifest `description`, and **must fall back to the
   directory name** when no `agent.yaml` is present (manifest is optional under
   the additive migration). Reflected in §1, §7.
5. **`Workspace.runner` → removed outright after migration.** Retained only
   through the migration window as the non-breaking fallback, then deleted; the
   agent/project layers become the sole runner source. This milestone assumes a
   **uniform fleet** (every machine has every runner kind), so availability needs
   no per-workspace field. A machine/workspace **capability declaration** for
   heterogeneous fleets is a filed follow-up, not this milestone. Reflected in
   §4, §8.
