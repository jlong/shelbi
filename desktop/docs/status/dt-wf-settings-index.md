# dt-wf-settings-index: the project section of Settings, and the Workflows page

Second of the four Workflows-and-Statuses tasks (`dt-wf-config-model`,
`dt-wf-settings-index`, `dt-wf-editor-form`, `dt-wf-editor-builder`). It builds
the Settings · Workflows · Index frame (`2301:5824`, `workflows/1-index.png`)
on the editing model `dt-wf-config-model` shipped
(`shelbi_state::workflow_config`). Every read and write goes through that
model; the page never touches YAML.

## The change

**Settings sidebar.** Below Global, as the frame draws it: a divider, the
project switcher (the project's name and the up-down chevron), then the
project's pages. Only **Workflows** (⌘4, `view-columns`) exists; the frame's
Workspaces, Agents and Integrations are left out until their pages do.
`settings_nav::SECTIONS` gained a `Header::Project` section. The switcher is
the app sidebar's own trigger and menu (`sidebar::switcher_trigger` and
`sidebar::switcher_menu`, factored out of `render_project`), on the window's
one `Switcher` state. Switching projects from it keeps Settings open on the
same page, rebuilt for the new project: `switch_to` reopens Settings, and
`finish_switch` builds the project's main view behind it (for Back) rather
than in front of it. The selection chip now sits in one layer under all the
sections, so it slides across the divider like it does within a section.

**Workflows page** (`views/workflows.rs`, state in `views/workflows_state.rs`):

- **Workflows** header (helper, **New workflow**) over the table: name, a
  **Default** badge, description, status and transition counts, an edit
  (pencil) button and a ⋮ menu (Duplicate, Set as default, Delete). The
  default workflow is listed first, then the rest by name. Files the model
  can't parse are listed with their error, not editable.
- The pencil, a row click, and a newly created workflow emit
  `WorkflowsEvent::Edit(name)`, then `SettingsEvent::EditWorkflow(name)`, and
  the window routes it to `ShelbiWindow::edit_workflow`. Until
  `dt-wf-editor-form` lands, that opens the workflow's YAML in the user's
  editor (the same launch as `edit:workflows`). The editor task replaces that
  body.
- **New workflow** and **Duplicate** type the name into a new row at the
  bottom of the table (Enter creates, Esc cancels). Duplicate prefills
  `<name>-copy` (`-copy-2`, ... to be unique).
- **Statuses** header (helper, **Edit YAML**, **New status**) over the
  catalog: drag handle (`bars-2`), the board's status icon by category at
  20pt, the name in a textbox, the id in Source Code Pro, the **Category**
  picker (six categories), **Used in** (in the table's order) and delete.
  Then the footnote and the **Status categories** legend in two columns.
- **New status** adds a row with a name field, the id it will get (live,
  from `status_id_for`) and its own category picker.
- **Edit YAML** opens `statuses.yaml` in the user's editor. If the project
  has none yet, it writes the shipped catalog first
  (`scaffold_project_statuses`).
- The page scrolls with the Settings page. It re-reads the files each time
  it's shown, to pick up hand edits.

**Saving.** Edits save as they're made: a name on Enter or blur, a category
when picked, an order when a row is dropped, the rest on click. Each one is an
`Edit` applied with the model's op and `save()`d off the UI thread. Saves run
one at a time, and an edit made during a save is queued and applied to the
model that save returns, so the page never conflicts with its own previous
save. If the model refuses an edit or a save fails, nothing is written. The
page reloads the files and shows the error under the row it's about (a
refused rename puts the name back). A conflict (a file changed on disk since
it was read) shows the same way, and the hand edit stands. If a save drops
comments, a notice lists them.

**Guards.** Delete is disabled, with a tooltip, for the default workflow and
the only workflow (⋮ menu), and for a status any workflow uses ("Used in
task. Remove it from them first."). Deleting a workflow that open issues
use is refused when chosen, with the model's message naming them. That check
reads the board, so it can't run up front.

## Decisions

- **⌘4 for Workflows.** The frame's chords collide: it gives Workspaces ⌘3,
  which Terminal already has, then Workflows ⌘4, Agents ⌘6 and Integrations
  ⌘7. Workflows takes ⌘4 as drawn. Workspaces needs a new chord when its
  page lands.
- **Workflow names as they are (John, q-001).** The frame title-cases them
  ("Task", "Subtask", also in Used in), but a workflow has no display name in
  config, only its file stem. The page shows the real name (`task`,
  `subtask`), matching the board's badges and the editor's Name field. This
  is a deliberate difference from the frame.
- **Default first.** The model lists workflows by name; the page puts the
  default first, as the frame does, and orders Used in to match.
- **Inline new rows rather than a modal** for New workflow, Duplicate and
  New status. A status needs its name before it exists, because its id is
  derived from the name once and never changes.
- **Delete disabled at rest** where the guard is known without IO (40% and
  inert, with the reason as a tooltip). The frame draws every status's delete
  at rest, though every status in it is in use, so the frame's delete buttons
  read brighter than ours.
- **Settings' form roles.** The frame's `theme.foreground`/`theme.muted`/
  `theme.divider` resolve to the form values here (`form_text`, `form_hint`,
  `input_border` for the tables' edges, `divider_strong` for the rules), as
  on the other Settings pages. Button icons are `theme.icon` (the frame's
  gray), not `button_icon`.
- **Canceled glyph.** The frame draws Archived/Canceled as an x. The page
  uses the board's existing glyph (the struck circle) so the two match.
  Changing it means changing the board's glyph too, which is a follow-up if
  wanted.
- **Default badge.** The frame fills it with `theme.search` at 50%. There's
  no dark token at that alpha, so it uses the board's `workflow_badge`
  (slightly darker) rather than a new shared token.
- **No Workflows rows on the TUI.** The TUI has no Settings view, so there is
  no `CommandKind` or `shelbi_app::view` field to keep at parity.

## Icons

Vendored from Heroicons 2.2.0 `24/outline`: `view-columns`, `bars-2`,
`document-text` at 20pt, and `trash`, `pencil`, `ellipsis-vertical`
widened for 18pt (`heroicons-18/`, stroke 1.6667) for the 24pt row buttons.
The status glyphs gained a 20pt size (`assets::status_glyph_large`,
`style::status_icon_large`).

## figma-diff

`--visual-figma` renders `settings-workflows-{dark,light}.png` (fixture:
`workflows_state::fixture`, written for that screen only so the other
screens keep the built-in workflow). Against `workflows/1-index.png` (copied
as `settings-workflows-dark.png` into a reference dir):

| Render | Mismatch | Largest regions |
|---|---|---|
| Before (first render) | 2.75% | 1108x169+306+102 (workflow table), 1110x381+306+400 (status table), 715x24+306+362 (helper) |
| After | 2.30% | 715x24+306+362 (helper), 45x381+1371+400 (delete column), 462x24+307+785 (footnote) |

Fixed between them: the status icons at 20pt, the form text and hint
colors, the tables' edges and rules, the button icon color, and default
first. What's left: Figma's Geist v1.2 sets body text wider than the bundled
Geist (known from earlier QA), the disabled delete buttons, and the
lowercase names. Light uses the same layout in light tokens and was checked
by eye.

## Tests

- `views::workflows_state::tests`: rows and counts, rename keeps the id and
  the file's comments, category/reorder/new status persist, a status in use
  can't be deleted, create/duplicate/set default persist, a bad name stays
  on the new row, the default can't be deleted and an open issue blocks a
  delete until it's done, edits queue behind a save, and a file changed on
  disk is reloaded rather than overwritten. They run against a throwaway
  `SHELBI_HOME` under `crate::test_env::lock()`, now shared with the
  platform integration tests.
- `app::settings_tests`: the project section's order and switcher, a project
  switch reopening Workflows for the new project, the page's rows, editing
  by Enter/picker/drop/delete/new row, a refused rename, the ⋮ menu actions
  and their guards, and the edit route. The walk and ⌘N tests now cover ⌘4.
- Bounce 1: the project-switch test failed in the full suite only. Its
  `switch_to` subscribed to beta's feed with no `FeedHub` installed, so
  `FeedHub::global` started the live one, whose reader thread woke the gpui
  test scheduler off its thread ("Your test is not deterministic", reported at
  `end_test`, so only when the wake landed before the test ended). The test
  now installs `feeds::tests::canned_hub` and its own `SHELBI_HOME` with both
  projects, and `FeedHub::global` panics under `cfg(test)` when no hub is
  installed, so the mistake fails every run instead of under load.

## Verification

`cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
cargo clippy --all-targets --features visual-check -- -D warnings && cargo
test` (624 passed, three full runs in a row after bounce 1). No `crates/`
change.
