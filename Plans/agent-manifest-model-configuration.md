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
agents:
  orchestrator:
    runner: codex           # runner KIND; project maps to a concrete agent_runners entry
    model: gpt-5            # overrides the orchestrator package's preferred_model
  review:
    model: claude-opus-4-8  # keep the kind, bump the model
```

Only the keys a project wants to change appear; everything else falls through to
the manifest. When a runner *kind* maps to multiple `agent_runners` entries, an
override may name the concrete entry instead of the kind to disambiguate.

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

- **Additive, non-breaking.** `Workspace.runner` remains as the fallback (§4);
  existing projects keep working with no `agent.yaml` present.

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

## Open questions

1. **Reasoning effort portability.** `preferred_reasoning_effort` is
   Claude-thinking / Codex-effort shaped. Is a shared 3–4 level scale
   (`low`/`medium`/`high`/`max`) mapped per-adapter enough, or does it need
   per-runner values like model does?
2. **Multi-runner manifests.** Should a package be able to declare *distinct*
   preferred models per runner kind (`claude → opus`, `codex → gpt-5`) so it
   degrades well across projects, or is single-preferred + project-override
   sufficient for v1?
3. **Where does the runner-kind → concrete-runner mapping live** when a project
   has several runners of one kind — a project-level default, or forced to be
   explicit in each `agents.<name>` override?
4. **Naming vs. the workflow** **`agent:`** **field.** The status `agent:` names the
   install directory; the manifest `name:` is the package id. Confirm we want
   both, and that the sidebar/label shows the manifest `description`/name, not
   the raw directory.
5. **Should** **`Workspace.runner`** **eventually be deprecated** once agents own runner
   selection, or does it stay permanently as the machine-capacity fallback
   (e.g. a machine that only has Codex installed)?
