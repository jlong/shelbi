# dt-wf-editor-form: the workflow editor and its Form view

Third of the four Workflows-and-Statuses tasks (`dt-wf-config-model`,
`dt-wf-settings-index`, `dt-wf-editor-form`, `dt-wf-editor-builder`). It
builds the editor shell both editor frames share and the Settings · Workflow
Form frame (`2252:29281`, `workflows/4-workflow-form.png`), on the editing
model `dt-wf-config-model` shipped (`shelbi_state::workflow_config`). The
editor never touches YAML.

## The change

**Route.** Settings ▸ Workflows' pencil, a row click and a newly created
workflow emit `SettingsEvent::EditWorkflow(name)`; `ShelbiWindow::edit_workflow`
now opens the editor in the settings view (`SettingsView::open_editor`) over
the Workflows page's model as last loaded. While it shows, the settings view
draws only the editor, and the window drops the leading column and the main
area's titlebar row (`editing_workflow` in `render_shell`), so the editor
holds the whole window, as both editor frames draw it. ⌘N page chords and
the settings sidebar's keys are ignored under it; Esc is its Back. Closing
it (`EditorEvent::Close`) re-reads the page; Manage project statuses
(`EditorEvent::ManageStatuses`) also scrolls the page's Statuses section to
the top once it has drawn (`WorkflowsPage::statuses_bounds`, the settings
page's new `ScrollHandle`).

**Shell** (`views/workflow_editor.rs`): the titlebar row (52pt, `toolbar`
band): the traffic lights' room, Back, the workflow's name and its Default
badge, the Builder | Form segmented control, Discard changes (only while
there are unsaved edits; John's 10pt drawing as `glyphs::DISCARD`), Save
changes (the Button's Select style, disabled while clean, "Saving…" while a
save runs) and the ⋮ menu (Duplicate, Set as default, Delete; Delete
disabled with the reason for the default and the only workflow). Builder
shows a placeholder until `dt-wf-editor-builder`; the editor opens on Form.

**Draft** (`views/workflow_editor_state.rs`, toolkit-free): the loaded model
and a draft over it. Every edit is a `Change` applied through the model's
API, so it is validated as it is made; a refused change leaves the draft as
it was and shows the model's message at the field or row it concerns
(`Field`). Pickers apply on pick, text fields as typed; Save first applies
whatever the text fields hold (a field whose text was refused holds the save
back, its error showing), then the typed name, then writes. Discard puts the
loaded model back (and re-reads the files if the last save hit a conflict).
Back, Esc, Manage project statuses and Duplicate ask to Save, Discard or
Cancel when there are unsaved edits.

**Form** (scrolls): Name, Description, Initial status; Statuses (count,
Manage project statuses, Add status from the catalog's remaining statuses;
each row a drag handle, the status and its category, Owner, Agent, Workspace
tags, delete; the footnote); Transitions (count, Add transition, the helper;
each row From → To, the actions as chips with a menu each and a + to add
one, Details, delete); a rule; Git (Base branch, Branch name, Merge
strategy); Review (Review mode, and the recipe's fields when it runs the
branch); Automation (Zen settings, Required task fields); Delete workflow.

## Decisions (the frame against the real config)

- **Branch name is `git.branch`**, the full branch-name template the frame
  shows (`{{github_user}}/{{id}}`). A workflow naming branches by
  `git.branch_prefix` shows `<prefix>/{{id}}` and keeps the prefix when
  saved unchanged; other text becomes a `git.branch` (the two are exclusive).
  Empty Git fields with the project's merge strategy mean no `git:` block,
  so the workflow inherits the project's; the fields' placeholders show the
  project's values (read once, off the UI thread).
- **Review mode is `review:`**: Diff only is no block (the model's "diff-only
  review"); Run the branch is the `review:` recipe, its fields Serve
  command, URL, Setup, Ready check, Working directory. `serve` is required,
  so Save refuses Run the branch without it, at the Review section.
- **Zen settings is `zen:`.** Use project settings is no block. A workflow
  with one shows Custom (naming what it overrides); the form doesn't edit
  checks, CI timeout or danger paths, which stay in the YAML. Choosing Use
  project settings removes the block.
- **Required task fields is `required_params:`**, comma or space separated.
  Workspace tags parse the same way.
- **Details is shown on every transition row**, empty or not: it opens under
  the row with Target branch, URL, Run (one command per line), Ready check
  and Ready timeout (seconds), the transition's `target`, `url`, `run`,
  `ready`, `ready_timeout`. The button takes the Select style when any is
  set, so rows with hidden settings are visible at a glance.
- **Actions** reorder from each chip's menu (Run earlier, Run later), which
  also swaps the action or removes it; the + menu lists the actions the row
  doesn't have. Transitions themselves don't reorder (the frame draws no
  handle, and their order carries no meaning).
- **Add transition** adds the first pair of neighboring statuses that has no
  transition (else the first free pair), since the model refuses
  duplicates; From and To are then picked.
- **Rename waits for Save**: a rename reads the board (refused while open
  issues use the workflow), so the Name field applies at Save, off the UI
  thread, and its error shows under the field.
- **Duplicate and Delete act on the workflow as saved.** Duplicate asks
  about unsaved edits first, saves the copy and opens it in the editor;
  Delete deletes and closes (refused while open issues use it, with the
  model's message in the titlebar error line).
- **Names as files and agents are**: the title is the file stem (`task`,
  per `dt-wf-settings-index`'s q-001) and the Agent picker lists the agent
  directories as named (`developer`, `review`), where the frame title-cases
  them.
- **Add status stays enabled** when every catalog status is already in the
  workflow, as the frame draws it; its menu says so and points at Manage
  project statuses.
- Unsaved edits are guarded on Back, Esc, Manage project statuses and
  Duplicate, as the task asks. Leaving Settings another way (a nav chord,
  the palette) still drops the draft; a follow-up could guard those too.

## Fixture and assets

`workflows_state::fixture`'s Task now matches the Form frame: owners,
agents (`orchestrator` added to the fixture's agents), the `review` tag, the
frame's four transitions and their actions, and a `git:` block. The index's
counts are unchanged. New assets: `heroicons-18/plus.svg` (`PLUS_BUTTON`,
the + after a row's actions) and `glyphs/discard.svg` (10pt). The transition
arrow is `ARROW_RIGHT_BUTTON` (the 18pt drawing, 1.25pt stroke, as Figma's
16pt arrow is drawn) in the 16pt column.

## figma-diff

`--visual-figma` renders `settings-workflow-form-{dark,light}.png` in a
1440x1283 window, the frame's size, plus `state-workflow-form-edited`
(Discard and Save live, a refused timeout under its row, Details open),
`state-workflow-form-leave-prompt` and `state-workflow-form-bottom` (the
form's foot with Review running the branch), checked by eye. On a display
shorter than the frame macOS clamps the window (here 1041pt on the laptop),
so the capture now keeps its aspect when it downsamples instead of
stretching to the requested height; the reference was cropped to the same
height (`magick 4-workflow-form.png -crop 1440x1041+0+0`) for the diff.

| Render | Mismatch | Largest regions |
|---|---|---|
| First render | 5.79% | 1388x85+26+534, 1388x76+26+158, 610x46+755+474 (all rows 2pt low) |
| After | 1.93% | 581x22+27+671 (transitions helper), 541x44+26+573 (statuses footnote), 215x24+252+113 (description text) |

Fixed between them: a text field's `flex_1` inside a labeled column grew the
identity row to 58pt, pushing everything under it 2pt down. What's left:
Figma's Geist v1.2 sets body text wider than the bundled Geist (known from
earlier QA), the frame drawing the header dirty (Discard and an enabled Save)
where the render is clean, the lowercase title and agent names, and the
archived status's glyph (the shared status icon set draws a slashed circle
where the frame draws an x). Light uses the same layout in light tokens and
was checked by eye.

## Tests

- `views::workflow_editor_state::tests`: the draft (dirty, nothing written
  before Save, Discard reverts, undoing by hand leaves nothing to save),
  status add/move/assign/tags/remove, a refused owner at its row, transition
  add/endpoint/actions add-move-swap-remove and duplicate refusal, Details
  keys and a bad timeout, Git mapping (template, emptied to no block, a
  templated base needing a required field), a `branch_prefix` round trip,
  Review needing a serve command at Save, initial status, required fields,
  default and Zen with unknown keys preserved, rename at Save and a taken
  name, a refused text field holding the save back, the leave prompt's
  Cancel/Discard/Save, a conflict kept and reloaded on Discard, and
  duplicate and delete.
- `app::settings_tests`: the editor holding the window (no column, the
  titlebar row, Builder placeholder, ⌘N ignored, Back restoring the
  sidebar), edits waiting for Save and Discard reverting, the leave prompt
  from Esc and Back, statuses and transitions edited through their controls
  and saved, Details and lower-section errors shown inline, the ⋮ menu's
  default/duplicate/delete, Manage project statuses, and rename at Save. The
  index tests now go through the real editor.

## Verification

`cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
cargo clippy --all-targets --features visual-check -- -D warnings && cargo
test`. No `crates/` changes.
