# dt-agents-settings-page: the Agents page in Settings

Second of the three Agents tasks (`dt-agents-config-model`,
`dt-agents-settings-page`, `dt-agent-details`). It builds the Settings ·
Agents · Dark frame (`2377:41040`, `agents/1-agents-index.png`) on the
editing model `dt-agents-config-model` shipped
(`shelbi_state::agents_config`). Every read and write goes through that
model; the page never touches the files.

## The change

**Settings sidebar.** The project section now lists **Agents** after
Workflows, with the frame's `rectangle-stack` icon and **⌘6**.

**Agents page** (`views/agents.rs`, state in `views/agents_state.rs`):

- **Agents** header (helper, **New agent**) over the table:
  - Agent: the name, a **Customized** badge (`Provenance::Customized`), and
    the description from `agent.yaml`.
  - Preferred runner: Claude Code, Codex or Generic (`agents_state::
    runner_label`), with **Recommended** under it when the model's
    `runner_recommended` says it's the shipped choice. An agent with no
    `preferred_runner` shows "Project default".
  - Workflow statuses: each assigned status's board icon (14pt) and catalog
    name, once per status even when several workflows assign it (its
    tooltip names them), or "Not assigned".
  - Skills: the model's count (`skills/<name>/SKILL.md` and flat `*.md`).
  - An edit (pencil) button and a ⋮ menu: **Edit YAML**, **Duplicate**,
    **Reset to default**, a rule, **Delete**.
- Shipped agents first in their shipped order, then the user's by name (the
  model's order). Agents whose files don't parse are listed with the error
  and a ⋮ holding Edit YAML only, as on Workflows.
- The footnote, with **Workflows** a link to the Workflows page (it moves
  the settings sidebar's selection too).
- **Shared instructions** header (helper, **Edit instructions**), a preview
  of `agents/_shared/preamble.md`, and the footnote. The preview shows the
  first 8 lines, lightly formatted (`agents_state::preview`): blocks at blank
  lines, 12pt apart; headings semibold without their `#`s; list items as `•`
  lines; wrapped lines joined; `**`, backticks and HTML comments dropped; a
  `…` when there's more. No preamble shows "No shared instructions yet."
- **Edit instructions** swaps the preview for a text area (the workflow
  editor's Run box, 360pt tall) and the header's button for **Cancel** and
  **Save**. Save goes through `set_shared_instructions` + `save()`. Cancel
  drops the text. A conflict (the file changed on disk) keeps the editor and
  its text open with the model's error under it; the page has reloaded, so
  a second Save is a deliberate overwrite.
- The page scrolls with the Settings page. It re-reads the files each time
  it's shown and when the window comes back to the front after an Edit YAML
  (the `dt-workflow-edit-yaml` activation re-read).

**Actions and guards.**

- **New agent** and **Duplicate** type the name into a new row at the
  bottom of the table (Enter creates, Esc cancels), as on Workflows.
  Duplicate prefills `<name>-copy` (`-copy-2`, ... to be unique). The model
  validates the name; a refusal shows on the new row and keeps it open. A
  created agent opens (below).
- **Reset to default** is disabled, with the reason as a tooltip, for an
  agent Shelbi doesn't ship and for one that is still the shipped default.
  **Delete** is disabled for a shipped agent (the self-heal would recreate
  it) and for one a workflow status or the project's runner pin names (the
  model's `check_delete` message lists them).
- Both ask to confirm under the row (what's lost, Cancel, and a Danger
  Reset/Delete), since neither can be undone from the page.
- **Edit YAML** opens the agent's `agent.yaml` through
  `external_edit::open_file`.
- The pencil and a row click emit `AgentsEvent::Edit(name)`, then
  `SettingsEvent::EditAgent(name)`, and the window routes it to
  `ShelbiWindow::edit_agent`. Until `dt-agent-details` lands, that opens the
  agent's `agent.yaml` in the user's editor (the window's `open_in_editor`,
  which flashes what it opened). The details task replaces that body.

**Saving** is the Workflows page's machinery (`agents_state::commit`,
`AgentsState::request`/`committed`): each edit is applied with the model's
op and saved off the UI thread, one at a time, queued behind a save in
flight; a refusal or failed save writes nothing, reloads, and shows the
error where the edit was made. Dropped comments show as a notice.

## Decisions

- **⌘6 for Agents**, as the frame numbers it. ⌘5 stays free: the frames
  give Workspaces ⌘3 (Terminal's), so Workspaces needs a new chord when its
  page lands, and 5 is the obvious one. Workspaces and Integrations are left
  out until their pages exist.
- **Agent names as they are.** The frame title-cases them ("Orchestrator",
  "QA"); an agent's name is its directory name, which workflows, the runner
  pin and dispatch use. Same call John made for workflow names (q-001 on
  `dt-wf-settings-index`), so not asked again.
- **Descriptions from `agent.yaml`.** The frame's copy differs from the
  shipped descriptions; the page shows the real ones.
- **Customized badge color.** The frame fills it with `surface/depth-60`
  (`#acb4cf` at 50% dark, 40% light). No role has that alpha; it uses the
  `workflow_badge` role, as the Workflows page's Default badge does, rather
  than adding a token for one badge.
- **Confirm Reset and Delete inline** under the row rather than in a modal:
  the page lives inside the scrolling settings view, and an inline strip
  needs no overlay plumbing. Workflow delete doesn't confirm, but deleting an
  agent removes its instructions and skills, and resetting discards the
  user's edits, so these two ask.
- **The shared-instructions editor is inline**, in the preview's place, with
  Cancel and Save in the section header so they stay in view whatever the
  text area's height. Esc isn't bound to Cancel, so a stray Esc can't drop
  the text.
- **Unset runner reads "Project default"**: the model's `None` means the
  project's runner applies; the frame has no such row.
- **Table corners** use the frame's `radius/default` (12pt), larger than the
  Workflows table's 6pt.

## figma-diff

`--visual-figma` renders `settings-agents-{dark,light}.png` (fixture:
`agents_state::fixture`: the Workflows fixture's catalog and workflows, the
six shipped agents as `shelbi init` materializes them, the orchestrator's
and review agent's instructions edited, and the frame's three-paragraph
preamble; written for these screens only), plus `state-agents-menu`,
`state-agents-confirm` and `state-agents-shared-editor` (checked by eye).
Against `agents/1-agents-index.png` (copied as `settings-agents-dark.png`
into a reference dir):

| Render | Mismatch | Largest regions |
|---|---|---|
| settings-agents-dark | 2.76% | 859x25+324+789, 746x25+323+819 (preview text), 671x24+306+583 (footnote) |

The layout lines up (columns, rows, badges, buttons, rules, preview box).
What differs is content and type: the bundled Geist sets text narrower than
Figma's Geist v1.2 (known from earlier QA), the names and descriptions are
the real shipped ones, the frame's orchestrator also holds Review, and the
frame's sidebar draws Workspaces and Integrations. Light uses the same
layout in light tokens and was checked by eye.

## Tests

- `views::agents_state::tests`: the rows (order, Customized, Recommended,
  skill counts, status chips deduped across workflows, Not assigned), new
  and duplicate persist, a taken name stays on the new row, shipped and
  assigned agents can't be deleted (and a blocked delete asks nothing), a
  user agent deletes once confirmed, reset restores a customized agent and
  its badge goes, shared instructions save and a hand edit is reloaded
  rather than overwritten, edits queue behind a save, and the preview's
  light formatting and line limit. They run against a throwaway
  `SHELBI_HOME` under `crate::test_env::lock()`.
- `app::settings_tests`: the nav row after Workflows and ⌘6; the table's
  rows, badges, Recommended and status chips; the footnote link to
  Workflows; the edit button and row click routing (and the stub opening
  `agent.yaml`); New agent with a refused then accepted name; the ⋮ menu's
  inert items, Duplicate, Reset with Cancel and confirm, Delete confirmed;
  Edit YAML opening the manifest and the re-read on return; Edit
  instructions with Cancel then Save. The ↑/↓ walk test now ends on Agents.

## Verification

`cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
cargo clippy --all-targets --features visual-check -- -D warnings && cargo
test` (712 passed). No `crates/` change.
