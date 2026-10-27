# tui-settings-agents: Settings ▸ Agents in the TUI

## The change

The TUI's Settings (`tui-settings-shell`) offered General, Keybindings,
Terminal and Workflows, but no Agents page. It now has **Agents** (page 6),
after the desktop app's page
(`dt-agents-settings-page`) over the same state: the project's agents with
what each runs and where it is assigned, the row actions, and the shared
instructions edited in place. Every read and write goes through
`shelbi_state::agents_config`; neither client's page touches the files.

## Shared (`shelbi_app`)

- **`settings::agents`**: the desktop's `views/agents_state.rs`, moved
  unchanged in behavior: `Edit` (create, duplicate, reset, delete, set shared
  instructions) and where its error shows (`Target`), `commit` (apply, save,
  reload on a refusal or failure), `AgentsState` (the serialized save queue:
  one save at a time, later edits queued behind it; the reset and delete
  guards; duplicate names; the dropped-comments notice), `Confirm`,
  `status_chips`, `preview`, `runner_label`. New, so the clients share them
  instead of copying: `Confirm::message` / `label` (what Reset and Delete
  lose), `RowAction` and `AgentsState::row_actions` (each action with why
  the model refuses it; an unreadable agent offers Edit YAML only),
  `Committed::created`, `unreadable_message`, and the page's copy
  (`AGENTS_HELPER`, `SHARED_*`, `NO_SHARED`, `UNSET_RUNNER`, `NOT_ASSIGNED`).
  A successful load now clears an earlier load error.
- **`text_area::TextArea`**: the reject reason's multi-line buffer, moved out
  of `overlay::review_reject` (re-exported there) and extended: built from
  text, exact round trip (split on `\n` only, so a trailing newline, tabs and
  `\r` survive), a paste that turns `\r\n` and `\r` into line breaks,
  Home / End / Page Up / Page Down, `edit(chord)`, and soft wrapping with the
  caret's wrapped position.
- **`settings::agents::fixture`** (tests, and other crates' tests through
  the new `test-support` feature): a throwaway `SHELBI_HOME` with a project's
  YAML, a status catalog, two workflows, the shipped agents (two customized)
  and a preamble. `shelbi-app`'s tests that set `SHELBI_HOME` now share one
  lock (`crate::test_env::ENV_LOCK`; the activity tests used their own).

The desktop's `views/agents_state.rs` is now a re-export of the shared
module plus its Figma-frame fixture and test homes; `views/agents.rs` uses
the shared confirm copy and strings. No desktop behavior changed. The
state's own tests moved to `shelbi-app` with it.

## The TUI (`crates/shelbi-tui/src/shell/settings/agents.rs`)

**Layout.** The Agents header and helper, a load error or the
dropped-comments notice, "Saving…" while a save runs, then **+ New agent**
and the table:

- 72 columns and wider: Agent (name, a **Customized** badge, the description
  under it), Preferred runner (Claude Code, Codex, Generic or "Project
  default", **Recommended** under it), Workflow statuses (each assigned
  status once, a dot in its category's board color, wrapping in its column,
  or "Not assigned"), Skills, and a `⋮`.
- Narrower: each agent's fields stack under its name ("Claude Code
  (recommended) · 1 skill", "Statuses: Backlog, To Do").
- An agent whose `agent.yaml` doesn't read: its name and "Can't be edited
  here: <error>", with Edit YAML only.

Then the footnote, a rule, **Shared instructions** with a preview of
`agents/_shared/preamble.md` (the shared `preview`: headings bold, list items
as `•` lines, a `…` when cut) and **Edit instructions**, and the footnote on
when changes apply. The page scrolls; the focused row (or the editor's caret
and buttons) stays in view, off the "↓ more" line.

**Keys** (the sidebar keymap's up / down / activate, as everywhere in
Settings, plus the arrows):

- Enter on a row opens the agent (below), on New agent or Edit instructions
  opens them; `n` New agent and `y` Edit YAML from the page; `m` opens the
  focused agent's actions menu.
- The menu lists Edit (instructions.md), Edit YAML, Duplicate, Reset to
  default and Delete, each with its key (`e y d r x`). One the model refuses
  stays listed, dimmed, with why under it when selected; choosing it does
  nothing.
- New agent and Duplicate type the name into a row at the table's end
  (Duplicate prefills `<name>-copy`): Enter creates, Esc cancels; a refusal
  shows under it and keeps it open. The created agent's row takes the focus.
- Reset and Delete ask under the row (`Confirm::message`): Cancel focused,
  ←/→ or Tab to the action, Enter presses, Esc cancels.
- The shared-instructions editor: every key types (Enter a line break,
  digits and `j`/`k`/`q` included), the arrows, Home / End and Page Up / Page
  Down move, a paste (or a dropped path) inserts at the caret, Ctrl+S saves,
  Tab steps to Save and Cancel, Esc closes it. With unsaved text, Esc (or
  Cancel, Back, another page, a project switch) asks Save, Discard or Cancel
  first ("Save your changes to the shared instructions?"); Save leaves once
  the save lands, and a failed one stays with its error.
- While a menu, a confirm, the new row or the focused editor holds the keys,
  Esc, Tab and the digits are theirs; otherwise the shell's Settings keys
  apply (Tab to the sidebar, Esc back, digits, Page Up / Down, `r` reload).
  The hint band says what applies.

**Mouse.** A click selects a row and a second opens it; the `⋮` opens the
menu; a menu item runs; the confirm's and editor's buttons press; a click in
the editor places the caret. A click outside the menu closes it.

**Opening an agent.** Enter (and the menu's Edit) goes through one hook,
`AgentsPage::open_agent`: the TUI's dedicated agent editor once it has one
(`tui-agent-editor`, its own task), and until then the agent's
`instructions.md` in the user's editor through the existing `edit:agent`
target. Edit YAML opens `agent.yaml` (or an unreadable agent's). The shell
hands the terminal to the editor and reads the page again when it returns.

**Saving.** Each edit is the shared `Edit`, run by the shell off the UI
thread (`commit_agents`), one at a time, the next queued one started when it
lands. A refusal or failure writes nothing, reloads, and shows the error at
its row, the new row or the shared instructions. A conflict (the preamble
changed on disk) keeps the editor and its text, shows "Changed on disk…",
and has reloaded, so a second Save is a deliberate overwrite. After an edit
that adds, removes or resets an agent the General summary's counts are read
again when next shown, and the page's reload re-reads the workflow
assignments.

**Loads and project switches.** The page reads its files off the UI thread
each time it is shown, after the editor returns, and on `r`. Each project
gets a new page with its own id: a read or save for the project left that
lands later is dropped from the new page. A save still running at the switch
finishes, with its queued saves, for its own project; a failure there goes to
the status line.

## Preserving what the page doesn't own

All writes are the model's: `agent.yaml` through `yaml_edit` (unknown keys,
comments and formatting kept; comment lines an edit drops are reported in the
notice rather than claimed preserved), raw `instructions.md`, preamble and
skills, and `settings.json` hook wiring and other files never touched (a
duplicate copies them). An unreadable agent is never written. Tests check an
unknown field, an extra file and the hook file come through page edits
byte-for-byte.

## Platform differences (by design)

- **Opening an agent.** The desktop opens its details editor; the TUI opens
  `instructions.md` in the user's editor until `tui-agent-editor` lands, and
  doesn't open a newly created agent (its row takes the focus instead of
  handing the terminal to an editor unasked).
- **Actions.** The desktop's pencil and ⋮ menu; the TUI's Enter, `m` (or the
  row's `⋮`), `y` and `n`. Refused actions: a disabled item with a tooltip on
  the desktop, a dimmed item with its reason under it in the TUI.
- **The shared editor's Esc.** The desktop doesn't bind Esc to Cancel; the
  TUI's Esc closes the editor, but only through the unsaved-edits prompt
  when there is unsaved text.
- **Workflows link.** Both footnotes link to Workflows; in the TUI the link
  is one of the page's stops (Enter shows Workflows, through the
  unsaved-edits prompt). A build that doesn't offer Workflows says where
  assignments live instead.
- **Table.** The desktop's table is fixed; the TUI's stacks under 72
  columns.

## Tests

- `shelbi-app`: `settings::agents::tests` (rows, provenance, assignments
  deduped, skill counts; new and duplicate; a refused name; shipped and
  assigned agents can't be deleted; reset; row actions and their guards, an
  unreadable agent's Edit YAML only; shared instructions save Markdown as
  text and a hand edit isn't overwritten; the queue; a failed load; the
  preview), `text_area::tests` (exact round trip, paste line endings, edit
  keys, wrapping and the caret).
- `shelbi-tui` `shell::settings::agents::tests` (real fixture project, real
  key path, loads and saves run in place of the shell's threads): the wide
  table; the narrow stack with every action through the menu, readable in
  light; wide in light; Enter's hook and `y`; new agent with a refusal and
  Esc; duplicate; Reset and Delete guards, reasons, confirm defaults, an
  in-use agent refused until freed (its assignment refreshed on reload);
  shared instructions saved as typed (paste with `\r\n`, `j`/`q`/digits typed);
  Cancel through the prompt, and Save from the prompt leaving Settings; a
  conflicting save; an unreadable agent after an external edit, and unowned
  fields, files and hooks kept; the dropped-comments notice; a project switch
  dropping the old project's read and finishing its queued saves; no
  project; mouse; tiny sizes. The shell's stop tests now include Agents.
- `shelbi-tui` `shell::tests::the_agents_page_loads_edits_yaml_and_saves_shared_instructions_off_the_ui_thread`:
  through `handle_event` and the real threads: `6` opens the page, `y`
  queues the editor on `agent.yaml`, a paste and Ctrl+S save the preamble,
  and a switch reloads for the new project.
- Desktop: the existing Agents page and editor tests, unchanged, over the
  shared state.

## Not done

- The TUI agent details editor (`tui-agent-editor`).
- No palette entry for the page ("Settings: Agents"); the palette's Settings,
  `,` and `6` reach it.
- Not hand-walked in a live TUI; the tests drive the real key and draw paths
  on `TestBackend`.
