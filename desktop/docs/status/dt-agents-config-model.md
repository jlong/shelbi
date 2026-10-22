# dt-agents-config-model: the editing model behind Settings · Agents

First of the three Agents tasks (`dt-agents-config-model`,
`dt-agents-settings-page`, `dt-agent-details`). No UI: this is the
toolkit-free layer both pages sit on, `shelbi_state::agents_config`, next to
`shelbi_state::workflow_config` and built the same way. The API, by screen,
and the decisions are in the module docs (`crates/shelbi-state/src/agents_config.rs`);
this page maps the Figma frames onto it.

## Frames to model

**Settings · Agents · Dark** (`2377:41040`, `agents/1-agents-index.png`) reads
`AgentsConfig::summaries()`:

| Frame | Model |
|---|---|
| Agent name, description | `AgentSummary::name` (the directory name), `description` (`agent.yaml`) |
| Customized badge | `provenance == Provenance::Customized`. `Shipped` shows nothing here (the details page's Default badge); `UserCreated` shows nothing |
| Preferred runner / Recommended | `preferred_runner` (`None`: unset, the project's runner applies) / `runner_recommended` |
| Workflow statuses / Not assigned | `assignments` (workflow, status id, catalog name, category); empty is Not assigned. The same status in two workflows is two entries, so the page dedups by status id if it wants one chip |
| Skills | `skills` |
| ⋮ menu | `duplicate_agent`, `rename_agent`, `delete_agent`, `reset_to_default`; disable with `check_rename` / `check_delete` / `can_reset` |
| New agent | `create_agent(name, description)` |
| Shared instructions, Edit instructions | `shared_instructions()` / `set_shared_instructions`; `shared_instructions_path()` for an external editor |
| Files that don't parse | `unreadable()` (not in the design) |

**Settings · Agent Details · Developer · Dark** (`2405:6965`,
`agents/2-agent-details.png`) reads one `Agent`:

| Frame | Model |
|---|---|
| Default badge | `provenance(name) == Provenance::Shipped` |
| Name | `name()`; editing it is `rename_agent`. Shipped agents refuse (`AgentError::Shipped`), so the field is read-only for them |
| Description | `description()` / `set_description` |
| Instructions (Markdown) | `instructions()` / `set_instructions` |
| Skills, Add skill, No skills added | `skills()`, `add_skill(name, description, body)` or `import_skill(path)`, `remove_skill` |
| Preferred runner | `preferred_runner()` / `set_preferred_runner`; options from `runner_options()` |
| Model / Reasoning effort, "Use runner default" | `model(runner)` / `reasoning_effort(runner)`, `None` is the runner default (key absent). Options: `RunnerOptions::models` (suggestions; a value on disk outside them, like `claude-opus-5-5`, is shown as-is, and free text is allowed) and `efforts`. Both are empty for Generic, and setting either is refused |
| Permissions, "Use project settings" | `permissions_mode()` / `set_permissions_mode`, `None` is absent. Options: `PERMISSION_MODES` |
| Workflow assignments | `assignments(name)`, read-only |
| Save changes | `is_dirty()`, `save()` (`AgentError::Conflict` when files changed on disk) |
| "Changes apply to new agent sessions" | True as is: dispatch reads the agent's files at every launch and nothing caches them |

John answered q-001: the model picker's suggestions are the runners' aliases
(Claude Code `opus`, `sonnet`, `haiku`; Codex `gpt-5`, `gpt-5-codex`),
"Use runner default" first, on-disk values kept as-is, free text allowed;
effort low/medium/high/max for Claude and Codex, none for Generic. They live
in `shelbi_core::{model_suggestions, reasoning_efforts, PERMISSION_MODES}`.

## Notes for the UI tasks

- The design's descriptions differ from the shipped ones; the page shows
  what `agent.yaml` says.
- `settings.json` (runner hooks) isn't edited, isn't part of Customized, and
  reset leaves it alone. A duplicate copies it.
- Removing a key also removes the comment lines directly above it, and
  `Saved::dropped_comments` lists them (removing `preferred_runner:` from a
  shipped `agent.yaml` drops its two `##` lines).
- `shelbi agent new` now writes the shared `placeholder_instructions`.
  `shelbi agent list`'s skill count (flat `skills/*.md` only) is unchanged;
  the model's count also includes `skills/<name>/SKILL.md` directories,
  which is what the runner loads (the review agent's shipped skill is one).
