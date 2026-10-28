# tui-agent-editor: the agent details editor in the TUI

## The change

Settings ▸ Agents in the TUI (`tui-settings-agents`) listed the project's
agents, but Enter on one only opened its `instructions.md` in the user's
editor. It now opens a dedicated editor for one agent, the TUI's counterpart
of the desktop's (`dt-agent-details`), over the same draft, the same
validation and guards, the same Save / Discard and leave prompt, and the same
mapping onto `agent.yaml`, `instructions.md` and `skills/`. Nothing is written
before Save.

## Shared (`shelbi_app::settings::agent_editor`)

The desktop's toolkit-free `views/agent_editor_state.rs` moved here whole
(`AgentEditorState`, `Change`, `Field`, `Leave`, `run_save` /
`run_duplicate` / `run_delete`, the labels). The desktop module is now a
re-export, and its state tests moved with it
(`crates/shelbi-app/src/settings/agent_editor/tests.rs`, over the shared
Agents fixture). Added on the way, used by both clients:

- **`Leave::Open(AgentFile)`** replaces `Leave::EditYaml`: `agent.yaml`,
  `instructions.md` or a skill's `SKILL.md` in the user's editor, through the
  same leave prompt, with `AgentEditorState::file` saying where each is as
  saved. **`Leave::Workflow { name, status }`** carries the assignment's
  status so the TUI's workflow editor opens on its row (the desktop ignores
  it, as before).
- **The pickers' choices** (`runner_choices`, `model_choices`,
  `effort_choices`, `permission_choices`, each a `Choice` with its label,
  change and whether it's current) and `options_blocked` (why a generic
  runner takes neither model nor effort). The desktop's pickers now build
  from them instead of their own copies.
- **What applies, and from where** (`runner_in_effect`, `model_in_effect`,
  `effort_in_effect`, `permissions_in_effect`, each with a `Source`: this
  agent, the project, or the default) and their one-line notes, following
  `resolve_agent_launch`: the project's `agents.<name>.runner` pin wins over
  the preferred runner, the project's `runners.<kind>.model` /
  `.reasoning_effort` win over the agent's, and `workspace_permissions_mode`
  is a ceiling the agent's request is lowered to. **`runner()` now follows
  the pin too**, so Model and Effort edit the runner the agent really runs
  on; without a pin nothing changes. `runner_footnote` is the desktop's
  Runner preferences footnote, now naming a pin.
- `skill(name)` (`SkillView`: description, layout, its files, its text, and
  whether it's saved yet), `type_texts`, `conflict()`, `badge()`, `project()`,
  `import_path` (`~/` and relative paths, for a typed path), and the editor's
  copy as constants (both clients' strings).

`shelbi_state::agents_config` gained what these read: `Agent::skill_files`,
`skill_file` and `skill_text` (the draft's, before Save), and the project
overrides (`project_runner_pin`, `project_model`,
`project_reasoning_effort`, `project_permissions_ceiling`, read leniently
from the project YAML). **`import_skill` now refuses** a path that doesn't
exist ("doesn't exist") and a `SKILL.md` or `.md` file whose frontmatter is
missing, unclosed, invalid YAML or without a `description:`, the same rule
Add skill already applied; both show at the Add skill / Import field.

## The TUI editor (`crates/shelbi-tui/src/shell/settings/agent_editor.rs`)

**Opening.** Enter (or the menu's Edit) on an agent on Settings ▸ Agents,
and, as on the desktop, an agent just created or duplicated there. With the
Agents page's model loaded the draft shows at once; a read starts either way
(the shell's Agents reader, so tests never read the real `~/.shelbi`). Like
the desktop's, it hides the settings sidebar and takes both columns. Back
lands on the Agents page with the agent's row focused once its fresh read
lands (a renamed or new agent included).

**Layout** (80x24 and up; nothing panics down to 1x1):

```
 ← Back   developer Default        Save changes  Discard  ⋮ Actions

 Agent
 › Name               developer
     Locked: `developer` ships with Shelbi and can't be renamed or deleted; reset it instead
   Description        Implements a dispatched task on its own branch
 Instructions  Markdown
 Define how this agent approaches a task and when its work is complete.
   │ # Developer
   │ ...
     … 220 more lines · ⏎ edit
   Edit instructions.md in your editor
   Shared project instructions are included before these instructions ›
 Skills (1)
   release
     Cut a release.
   + Add skill
   Import skill       a path: ~/skills/lint or lint.md
 Runner preferences
   Preferred runner   Claude Code ▾
     Runs on Claude Code.
   Model              opus ▾
     Claude Code runs opus (this agent’s setting).
   Reasoning effort   Use runner default ▾
   Permissions        Use project settings ▾
 Workflow assignments
   ● task / In Progress ›
   Manage assignments in Workflows ›
   Reset to default
   Delete agent…
 ↑↓ · ⏎ choose · Tab section · m menu · ^S save · Esc back
```

The header never scrolls: Back, the name with its badge (Default,
Customized, none for the user's own, from the files as saved) and a `●` while
there are unsaved edits, then Save changes (dimmed without edits, `Saving…`
while a save runs), Discard and `⋮ Actions` (Save, Discard, Edit YAML, Edit
instructions.md, Duplicate, Reset to default, Delete agent…; one the guards
refuse is dimmed with the reason under the list). The page scrolls under it:
each field's error shows in red under its row, the Save error (or the
dropped-comments notice) at the top.

**Keys** (all terminal-safe; no mouse needed):

| Key | Does |
|---|---|
| the user's up / down, arrows | move through the rows |
| PgUp / PgDn / Home / End | jump |
| Tab / Shift+Tab | next / previous section (header, Agent, Instructions, Skills, Runner, Assignments, Reset and Delete) |
| Enter | the row's main thing: type in a field, edit the instructions or a skill body in place, open a picker, a skill's menu, follow a link, press a button |
| `m` / `.` | a skill's menu, else ⋮ Actions |
| `i` / `e` / `x` on a skill | inspect / open its SKILL.md in your editor / remove (after a confirm) |
| `e` elsewhere | instructions.md in your editor |
| `n` | Add skill |
| `y` | Edit YAML |
| `c` on the header | Duplicate |
| Ctrl+S | Save |
| Esc | close the Add skill form when in it, else Back |

**Typing.** Name and Description type in their row; each key goes to the
draft at once (the Name waits for Save, since a rename moves the directory).
A Name the model refuses to rename (shipped, or named by a workflow status or
the runner pin) doesn't type: its row is muted and the shared guard's reason
shows under it. The **instructions** and a new skill's **body** edit in place
in a multi-line area: every key types (Enter a line break, `j`, `q`, digits
included), the arrows, Home / End and Page Up / Down move, a paste inserts,
Esc stops editing, Tab stops and goes on; the window follows the caret at any
height and the page keeps its line in view. Not being edited, the
instructions show their first ten lines and how many more there are.

**Pickers** list the shared choices with the current one marked: Preferred
runner (Project default first), Model (Use runner default, the runner's
suggestions, a model on disk that isn't one, and **Custom model…**, which
opens a row to type any name: Enter sets it, Esc cancels), Reasoning effort
and Permissions. A generic runner's Model and Effort list only "Nothing to
choose" with the reason. Under each picker a muted line says what applies and
where it comes from (this agent, the project's pin or runner setting, the
permission ceiling, or the default).

**Skills.** Each skill shows its name (and "flat .md" for a legacy one) with
its description under it. Its menu: Inspect (a scrolling view of its
description, layout, files and SKILL.md text as the draft holds them, "not
saved yet" for one added since the load), Open SKILL.md in your editor,
Remove skill… (Remove / Cancel, Cancel focused; the files go at Save). **Add
skill** opens a form: Skill name, Description, the SKILL.md body, Add skill
and Cancel (Enter in a field moves to the next); a refusal shows under the
form and keeps its text. **Import skill** types a path (`~/` for home,
relative to the working directory); Enter copies the folder (whole, its
structure kept) or `.md` file into the draft, or shows why not under the
field, keeping the path to correct.

**Save** (Ctrl+S, the header, ⋮ Actions) applies the text fields, then runs
`run_save` off the input loop (`AgentWork::Save`, through the shell's
Settings channel under the editor's ticket). A failed save keeps the draft
and shows why. **Discard** puts the loaded agent back. **Reset to default**
puts the shipped files into the draft (the fields show them), for Save or
Discard like any other edit; it's dimmed with the reason for a user's own
agent or one already at its default. **Delete** asks first (Cancel focused;
`y` deletes) and is dimmed with the shared guard's reason where refused.

**Leaving.** Back, Esc, Edit YAML, Edit instructions.md, Open SKILL.md,
Duplicate, an assignment row, Manage assignments and the shared-instructions
link go through the shared `Leave` guard: with unsaved edits a Save / Discard
/ Cancel prompt asks first (`s` / `d` / `c` or Esc, ←→ and Enter), and Save
goes on once its save lands. A project switch asks the same through
Settings' prompt; its Save runs the editor's save and switches once it
lands. An assignment row opens that workflow's editor on its status (closing
it lands on Workflows, as on the desktop); the shared-instructions link goes
back to the Agents page on Shared instructions.

**Conflicts and returning.** A save the model refuses because the files
changed on disk opens **Changed on disk**: Reload (`r`) or Keep editing
(`k`, Esc, Enter: it's focused); Keep editing leaves the draft and Save stays
refused, so a hand edit is never overwritten. Back from the user's editor,
the run loop's reload reads the editor's files again for its own project
only: a clean draft takes them, one with edits keeps them, an agent gone from
the files closes the editor with a note on the Agents page. Every result
carries the editor's ticket, so a read, save, duplicate or delete that lands
after a project switch (the editor goes with it) or a newer request is
dropped.

**Mouse.** A click selects a row, a click on a field starts typing, a second
click does Enter, a header button presses; menu entries and prompt buttons
are clickable; the wheel scrolls without moving the focus.

## Preserving what the editor doesn't own

Every write is `AgentsConfig::save`: `agent.yaml` through `yaml_edit`
(unknown keys, comments and formatting kept; dropped comments reported),
raw `instructions.md` and skills, `settings.json` hook wiring and unrelated
files never touched, and the on-disk conflict check before any write.

## Desktop changes

- The editor's state is the shared module; its pickers build from the shared
  choices; the Runner preferences footnote is `runner_footnote` (unchanged
  text without a project pin).
- ⋮ gains **Edit instructions.md**, and each skill row an **Open SKILL.md in
  your editor** button, both through the leave prompt (the same
  `Leave::Open` the TUI uses).
- An Import that picks a skill without a description (or a broken one) now
  shows the model's error at the form instead of importing it.

## Intentional differences from the desktop

- **Import** types a path; the desktop uses the macOS file chooser.
- **A custom model** is typed in the TUI's Model picker; the desktop still
  sets an unlisted model through Edit YAML (a value on disk stays a choice in
  both).
- **What applies** shows under each TUI picker; the desktop says it in its
  footnote only.
- **Skills**: the TUI inspects a skill in place and confirms its removal;
  the desktop removes from the row at once (Discard brings it back in both).
- **A conflict asks** Reload / Keep editing in the TUI; the desktop shows
  the error and points at Discard.
- **Rows instead of columns**: the desktop's two columns are one scrolling
  list of sections.

## Also fixed (TUI workflow editor)

- Its rows marked every add / link row as selected (`Some(false)` drew the
  unfocused selection fill); only the cursor's row is now.
- A focused row with nothing under it could scroll onto the page's last
  line, which "↓ more" draws over; both editors keep a line under the focus.

## Tests

- `shelbi-state` `agents_config::tests`: an import of a missing path or a
  manifest without frontmatter / description / closing `---` / valid YAML
  refused; skill files and text read from the draft (nested and flat); the
  project overrides read from the project YAML.
- `shelbi-app` `settings::agent_editor::tests` (18): the desktop's state
  tests, plus `type_texts`, a custom model kept as typed, the choices, the
  project's pin / model / ceiling in effect, skills inspected and imported
  with structure, a missing path and bad manifest as field errors, reset then
  discard, a pinned agent's guard, Open targets, a failed save, `import_path`.
- `shelbi-tui` `shell::settings::agent_editor::tests` (20): every section
  and every configuration group by keys alone (nothing written before Save,
  every value on disk after, defaults removing keys), a custom model and the
  generic runner's reasons, inherited values, skills added (refusal kept),
  imported (missing path, nested folder by paste), inspected, removed through
  the confirm, a bad import as a field error, reset and discard, rename and
  delete guards, the leave prompt for Back / Edit YAML / Edit instructions.md
  / an assignment, duplicate, a hand edit under the save (Keep editing, then
  Reload), a failed save, reload on return (clean, dirty, gone), a stale read,
  focus in view at 80x24 for every row, long instructions scrolling across a
  resize, tiny sizes, light theme, mouse.
- `shelbi-tui` `shell::tests`: through `handle_event` and the real threads,
  Settings ▸ Agents ▸ Enter opens the editor over both columns, a typed
  description saves off the UI thread, a project switch with unsaved edits
  asks and its Save writes then switches; `e` queues `instructions.md`, the
  run loop's reload takes the outside edit, and an assignment opens the
  workflow editor on its status.
- `shelbi-tui` `shell::settings::agents::tests`: Enter, a second click, New
  and Duplicate now open the editor, Back lands on the row.
- Desktop: its editor and app tests run unchanged over the shared state
  (`cd desktop && cargo test`: 799 passed).

## Not done

- No palette entry for an agent's editor ("Settings: Agent …"); Settings ▸
  Agents reaches it.
- Not hand-walked in a live TUI; the tests drive the real key and draw paths
  on `TestBackend`.
