# dt-task-inline-properties

The task pane's Status, Workflow and Priority values were faces only
(`dt-floating-task-pane`). They now edit the task in place, with no edit
mode, after John's revised Task pane frames (In progress 2172:722, Ready to
start 2573:10737) and the Button set's new Ghost style (Small / Ghost
2565:13261 in 2220:17600), reviewed live on 2026-10-29. John's requirement:
"I want the user to be able to edit the task without entering an edit mode.
It should be WYSIWYG."

## What landed

- **The rules, shared** (`shelbi_app::task_properties`, toolkit-free, unit
  tested). For an issue on the board's read:
  - `status_choices`: its effective workflow's statuses in `statuses.yaml`
    order, the current one marked, each legal or refused by
    `board::drop_target` (the gate a drag uses), merge edges marked. An
    unloaded workflow lists every status refused with "its workflow `x`
    isn't loaded". `status_command` turns a choice into `MoveIssue`, so the
    daemon still runs the transition's actions and its gated merge.
  - `workflow_choices`: every loaded workflow, the project default marked,
    the effective one current (an issue with no `workflow:` follows the
    default). A workflow that doesn't declare the issue's stored status is
    refused: "`subtask` has no Review status; move the task to a status it
    has first", so a change never strands the issue. `workflow_command`
    gives the new `CommandKind::SetIssueWorkflow`, or nothing for the
    effective one.
  - `queue_position` / `priority_choices` / `priority_command`: the
    issue's 1-based place in its stored column, in the stores' canonical
    order (priority, then id, the order `board::reorder_slot` uses), so a
    non-contiguous stored priority still reads `2 in queue`. A place is sent
    as `PrioritizeIssue` at `position - 1` (`QueuePosition::slot_for`). A
    done column (ordered by completion) has no queue; past-the-end and 0 are
    refused.
- **`CommandKind::SetIssueWorkflow { id, workflow }`**: its effect is the
  existing `Mutation::EditIssue` with only `workflow` set, the daemon's
  validated edit (it checks the name and that the workflow file exists, and
  refuses a stale expected state). Added to `is_mutating`, the
  `board_mutations_carry_the_project` scenario, the desktop's
  `dispatch_command` (`issue_command`), `sends_to_daemon`, the menu
  placement (Board), and their tests. The TUI matches on `Effect`, so an
  effect from it reaches `dispatch_effect`'s existing `EditIssue` arm; the
  TUI has no UI for it (parity Known gaps). Not a palette entry, like the
  other board mutations.
- **Ghost** (`theme/style.rs`). `ButtonKind::Quiet` became
  `ButtonKind::Ghost` with the component's colors, read from the variants:
  no fill at rest, `theme.background` hovered, `button/default/background-
  hover` pressed, the label `text_loud` (`#020303` Light), the Default
  icon gray. Its Focus variant (2565:13282) rests on the hover fill under
  the default ring, so `ButtonColors` gained `focus_fill` (the rest fill for
  every other style). Every former Quiet button (dialogs' Cancel, the pane's
  Done / Retry / Keep mine / Use theirs, the workspace shell button) takes
  Ghost, which is what those Figma frames' low-key buttons are. Ghost's
  Select variant is the Select style, so an open picker's value draws
  `ButtonKind::Select`. `disabled_button_base` and `disabled_menu_row` were
  added for content the Button's own pieces don't cover (a status glyph).
- **The pane** (`views/task_detail.rs`). Status (with its category glyph),
  Workflow and Priority are `value_picker`s: the Button at Small / Ghost
  with the chevron, 28pt rows 6pt apart, as the frames draw them. A click
  opens the picker anchored under the value (`workflow_editor::dropdown`,
  the More menu's): rows are menu rows with the status glyph (Status), the
  label, a muted hint (`merges`, `Default`, the title of the task at that
  place), and a check on the current value; a refused row is dimmed, not
  clickable, with why on hover. Ten rows show before it scrolls; the
  keyboard's row is kept in view. A value is Disable (with why on hover)
  while the task has a change still unanswered, and Priority in a done
  column. Branch is a selectable text view (Markdown with every punctuation
  mark escaped so `_` and `*` show as typed), and More gained Copy branch
  name.
- **The state** (`views/board_state.rs`). `Mode::PropertyPicker { property,
  cursor }` owns the keys while a picker is open: `j` / `k` / arrows (skipping
  refused rows), Home / End, Enter / Space, Esc / `q`. Choosing closes back
  to the pane (`rest()`), as every board menu does. `pane_focus` is the
  property the keyboard is on: Tab / Shift+Tab walk Status, Workflow,
  Priority and off again, Enter / Space opens the focused one, and closing
  its picker comes back to it (the Ghost focus ring). Another task or
  closing the pane starts it over.
- **Shown at once.** A choice is sent through `begin_mutation` like every
  board mutation: the expected state, the issue's write lane (held behind a
  body save, never refused for one, the draft kept), pending until a read
  settles it. `Pending` gained `to_workflow`; `BoardState::pane_properties`
  shows the value where an in-flight change is headed (a move's status and
  the bottom of its new queue, a reorder's place, a workflow), dimmed, and
  the card's workflow badge follows. A refusal drops the overlay and pins
  the daemon's message under the title (the existing notice row), and a
  refused row chosen anyway (a race) says why there too.
- **More** (`pane_actions`) now resolves availability from the board: Move
  left / right only when the workflow has that step, Assign only when the
  project declares workspaces, Copy branch name only with a branch. Start,
  Edit description, Open in editor and Open workspace as before. No Edit
  toggle, no action strip.

## Decisions

- **Client-side workflow compatibility, not a daemon guard.** The daemon's
  edit (also `shelbi issue edit --workflow`) doesn't check that the target
  workflow declares the issue's status, and changing that would change the
  CLI's behaviour. The pane offers incompatible workflows disabled with the
  resolution instead. Worth a follow-up if the CLI should refuse too.
- **Strict compatibility on the stored status id.** `resolve_task_status`
  would bucket a mismatched status by category on the board, but the store
  keeps the id, and later moves and gates key off it, so the stored id must
  be declared.
- **Priority is the stored column's order**, not the filtered board's rows,
  so the number matches what `reorder_slot` and the store renumber. The
  picker lists every place (scrolling), naming the task at each.
- **A done task's Priority reads `Done`**, disabled, rather than a number:
  its column is ordered by completion.
- **One property change at a time**, as before: a value is disabled while
  the task's last change is unanswered (the existing "a change is already in
  progress" refusal, now visible before the click).
- **Tab for keyboard access**, desktop-only like `s` / `a` / `e`, so the
  shipped `keys.yaml` is untouched. Esc on the pane still closes it. With
  a live session under the pane (`dt-task-pane-session`, #1989, which made
  Tab put the keys in the terminal), the order is one: Status, Workflow,
  Priority, then the terminal; with none, Tab cycles back off the values.
  (`dt-task-wysiwyg-editor` later put its title and body editors in this
  order at their reading places: title, the values, body, terminal.)
  `tab_past_the_properties_puts_the_keys_in_the_terminal` replaces #1989's
  `tab_from_the_pane_puts_the_keys_in_the_terminal`.
- **No new public `view` model fields**: the choices are their own shared
  types, and `TaskDetailModel.priority` keeps its meaning (parity rows
  updated to say the pane shows the queue place).

## Verification

- `cargo test -p shelbi-app` (507, incl. 8 `task_properties` tests and the
  new scenario row); `cargo test -p shelbi-tui every_registry_command_runs_from_the_palette`.
- Root: `cargo clippy --workspace --all-targets -- -D warnings`, `cargo
  check --workspace --locked` (no `Cargo.lock` change).
- Desktop: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (854, rebased on #1989). New: seven `board_state` tests (status picker over the
  registry, refused statuses skipped, Tab focus and its return, queue place
  and slot with the pending overlay and a done queue, workflow change as the
  validated edit with its overlay, refusal and an incompatible workflow, a
  change held behind a body save keeping the draft, Copy branch), and the
  window test `a_property_value_opens_its_picker_under_it_and_a_choice_is_sent`
  (click opens it under the value, the check, click-away, a click choice,
  Tab / Enter / End on Priority, focus back on the value).
- `--visual-figma`: new `state-task-pane-{status,workflow,priority}-picker`
  and `state-task-pane-focus`, both themes, checked by eye against
  2172:722 / 2573:10737 and the Ghost variants; the `buttons` sheet's Ghost
  row is pixel-checked (its Focused column now expects `focus_fill`); 0
  checks failed. The harness project has no workflow files, so its Status
  picker shows the unloaded-workflow refusals.

## Left

- The TUI has no in-place property editing (parity Known gaps).
- The daemon guard for workflow / status compatibility (above).
