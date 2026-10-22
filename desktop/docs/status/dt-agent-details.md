# dt-agent-details: the agent details editor

Third of the three Agents tasks (`dt-agents-config-model`,
`dt-agents-settings-page`, `dt-agent-details`). It builds the Settings ·
Agent Details · Developer · Dark frame (`2405:6965`,
`agents/2-agent-details.png`) on the editing model `dt-agents-config-model`
shipped (`shelbi_state::agents_config`). The editor never touches the files.

## The change

**Route.** The Agents page's pencil, a row click and a newly created (or
duplicated) agent emit `SettingsEvent::EditAgent(name)`;
`ShelbiWindow::edit_agent` no longer opens `agent.yaml` but the editor
(`SettingsView::open_agent_editor`), over the Agents page's model as last
loaded. It takes the window exactly as the workflow editor does:
`SettingsView::editing` covers both editors, so the window drops the leading
column and the main titlebar, ignores ⌘N, and routes Esc to the editor's
Back (`editor_back`). Closing re-reads the Agents page.
`SettingsEvent::EditorClosed` now also moves the settings sidebar to the page
the view closed onto, since the agent editor can close onto Workflows.

**Titlebar** (`views/agent_editor.rs`): the workflow editor's row without
its segmented control: Back, the agent's name, its badge (Default for
`Provenance::Shipped`, Customized for `Customized`, none for the user's own,
from the files as saved, as on the Agents page), Discard changes (dirty
only), Save changes (Primary, disabled while clean) and ⋮: Edit YAML,
Duplicate, Reset to default, a rule, Delete.

**Page** (scrolling, the frame's 32pt inset and gaps):

- Name (240pt) and Description.
- Left column: **Instructions** (the Markdown tag, the helper, a 410pt
  monospace text area holding `instructions.md`, the footnote whose "Shared
  project instructions" link goes back to the Agents page) and **Skills
  (count)** with Add skill: each skill's name, description and a remove
  button, or the frame's empty state.
- Right column (456pt): **Runner preferences** (Preferred runner, Model,
  Reasoning effort, Permissions, the footnote naming the runner), a rule,
  **Workflow assignments** (a row per assigned status, then "Manage
  assignments in Workflows").
- A rule and the note on when changes apply.

**Draft** (`views/agent_editor_state.rs`, toolkit-free): the workflow
editor's machinery. A loaded model and a draft; every edit is a `Change`
applied through the model, refused ones show under their field; text fields
apply as typed, the Name at Save (a rename moves the directory). Save runs
off the UI thread; a conflict keeps the draft and Discard re-reads. Back,
Esc, Edit YAML, Duplicate, an assignment row and both footnote links ask to
save or discard unsaved edits first (`Leave`). Edit YAML re-reads a clean
draft when the window comes back to the front.

**Shared helpers.** The workflow editor's toolbar button, menu items, error
line, footnote, header and row buttons, text field and status icon are now
`pub(super)`; its dropdown, Discard link and dialog overlay became free
functions (`dropdown`, `discard_link`, `dialog_overlay`) both editors use,
plus `readonly_field`. `AgentsPage::state` and `SettingsView::page` are no
longer test-only.

## Mapping and decisions

- **Model and Reasoning effort** apply to `runners.<runner>` for the runner
  the agent runs on: the preferred runner, else Claude Code, as
  `resolve_agent_launch` falls back. "Use runner default" removes the key.
  The list is `shelbi_core::model_suggestions` plus the file's own value
  when it isn't one of them (kept, not rewritten). There's no free-text
  entry; an unlisted model is set with Edit YAML. A generic runner takes
  neither: both pickers are dimmed with the reason.
- **Preferred runner** offers "Project default" first (no key), the Agents
  page's label for an unset runner.
- **Permissions**: "Use project settings" (no key), then
  `PERMISSION_MODES` with readable labels (Plan, Default, Auto, Accept
  edits, Bypass permissions); an unknown mode on disk stays a choice.
- **Add skill** opens a form under the Skills header: Name, Description and
  the SKILL.md body (a monospace text area), with Cancel and Add skill
  (`add_skill`: `skills/<name>/SKILL.md` with `name:`/`description:`
  frontmatter). **Import…** in the same form picks an existing skill folder
  (with its SKILL.md) or a `.md` file (`import_skill`). Both go into the
  draft and are written at Save, as remove is.
- **Workflow assignments are link rows**: the frame draws a picker-like row
  with a chevron-down; assignments are read-only here (they belong to the
  workflows), so each row keeps the button look but ends in a chevron-right
  (Heroicons `chevron-right`, newly vendored) and opens that workflow's
  editor. Closing that editor lands on Workflows, not back on the agent.
  An agent with none shows "Not assigned to any workflow status."
- **Reset to default** puts the shipped files into the draft (Save or
  Discard like any edit), so it needs no confirm; it's disabled when Shelbi
  doesn't ship the agent or the draft already is the default. **Delete** is
  disabled with the model's reason (shipped, or named by a workflow or the
  runner pin) and otherwise asks to confirm, since it removes the agent's
  instructions and skills.
- **Name** is read-only (selectable, with the reason as a tooltip) where the
  model refuses a rename: every shipped agent and any agent a workflow or
  the runner pin names. The frame's field looks the same either way.
- **Names as they are**: the title and Name show `developer`, and rows show
  `task / In Progress`, the directory and workflow names (q-001 on
  `dt-wf-settings-index`).
- **Esc with the Add skill form open** closes the form (its text dropped);
  with a menu or dialog open it closes that, as in the workflow editor.
- The component text area's own 1.25rem line is overridden to the frame's
  `Desktop/Terminal` 14/18.

## figma-diff

`--visual-figma` renders `settings-agent-details-{dark,light}.png` (the
developer from `agents_state::fixture`) plus `state-agent-details-menu`,
`-model`, `-add-skill`, `-leave`, `-user` (a user agent with two skills and
no assignments) and `-delete` (checked by eye). Against
`agents/2-agent-details.png` (copied as `settings-agent-details-dark.png`):

| Render | Mismatch | Largest regions |
|---|---|---|
| settings-agent-details-dark | 4.46% | 599x99+43+392, 615x98+44+500, 600x61+43+285 (instructions text) |

The layout lines up (titlebar, fields, columns, text area lines, pickers,
rules, skills box, footer). What differs is content: the shipped
developer's instructions and description aren't the frame's copy, the
fixture assigns the developer in both Task and Subtask (two rows), the title
is the directory name, Save is disabled while clean where the frame draws it
enabled, and Back sits at `BESIDE_TRAFFIC_LIGHTS` (the real lights are
wider than Figma's). Light uses the same layout in light tokens and was
checked by eye.

## Tests

- `views::agent_editor_state::tests`: the draft until Save and Discard,
  comments kept; instructions as markdown; every runner preference and its
  default removing the key, Codex's models, generic refusing a model; an
  unknown model or mode staying a choice; skills added (a missing
  description refused), imported and removed; reset into the draft and the
  badge after; rename waiting for Save and refused for a shipped agent;
  leave prompt; a hand edit under the save kept and Discard reloading;
  duplicate and delete on the saved model; assignments.
- `app::settings_tests`: the editor takes the window (no sidebar, titlebar,
  badges, both columns, Back to the page with its row); edits wait for Save,
  Discard reverts, typed description and instructions save; the pickers
  write and the defaults remove keys, generic dims Model; the leave prompt
  from Esc and Back (Cancel, Discard, Save) and the shared-instructions
  link; Add skill (refused, added), Save, remove, Cancel; assignment rows
  open the workflow editor (and ask first when dirty), Manage assignments
  shows Workflows with its row selected; ⋮ guards, Edit YAML, Duplicate,
  Delete with Cancel and confirm, Reset; a user agent's rename and a shipped
  agent's read-only name. The Agents page tests now see the editor open.

## Verification

`cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
cargo clippy --all-targets --features visual-check -- -D warnings && cargo
test` (731 passed). No `crates/` change.
