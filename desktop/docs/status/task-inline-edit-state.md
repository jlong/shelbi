# task-inline-edit-state

Shared autosave and conflict-safe draft state for editing a task in place, so
the task pane's WYSIWYG editor (`dt-task-wysiwyg-editor`) and inline property
controls (`dt-task-inline-properties`) have one save controller to build on.
The existing desktop detail editor now runs on it: no Save button, no edit
mode left on save.

## What landed

### `shelbi_app::issue_edit` (toolkit-free)

[`crates/shelbi-app/src/issue_edit.rs`](../../../crates/shelbi-app/src/issue_edit.rs):
`IssueEdits`, one per project (`issue_edit::shared(project)`, kept for the
life of the process so a draft outlives the view and window that made it).

- **Drafts.** Per issue, the title and body the editor shows against the
  stored values they are based on (and the revision those were read at).
  `open` starts a draft from a read or picks up the one already there (left
  unsaved, or open in another window); `edit` records typing; `text` is what
  the editor shows.
- **When it saves.** `poll(now)` returns the saves due: typing paused for
  `SAVE_DEBOUNCE` (800 ms), or a `flush` (field blur, Cmd+S, leaving the
  task). Edits coalesce into one save; a field that stores the same as its
  base (a title's surrounding spaces, a body's trailing whitespace, typing
  undone) is never sent. Only changed fields are sent.
- **One lane per issue.** Every write of an issue goes one at a time: the
  draft's saves and the client's other mutations (`begin_write` /
  `end_write`). Typing during a save is kept and saved next, against the
  revision the save landed at. `expected` substitutes the client's own landed
  revision for a read that only trails its own writes (the board index lags),
  and sends anything else as seen.
- **Reads.** `observe` folds a board read. One older than a landed write is a
  lagging copy and changes nothing; a copy still holding the value a save
  replaced is ignored until the save shows. A newer read replaces fields the
  draft hasn't changed (an `Update` tells the editor), and where both sides
  changed the body, `merge::merge3` combines the two when they touch
  different lines (touching or neighbouring lines don't merge). Otherwise the
  field is a `Conflict`: the editor keeps the draft, saving pauses, and
  `resolve` takes `KeepMine` (save over it, guarded on their text) or
  `TakeTheirs` (their text, the draft's kept as `set_aside`).
- **Failures.** A refusal as changed elsewhere waits (quietly "Saving…") for
  a read of the new copy, rebases on it and retries at once; after
  `STALE_WAIT` (15 s) with no new copy, or more than three refusals in a row,
  it says so. Any other failure keeps the draft and says why; it is retried by
  the next edit, a flush or `retry`, never on a loop. A workflow change the
  daemon refuses is dropped with its message.
- **Validation.** A blank (or multi-line) title isn't sent; the body still
  saves and the title shows `Invalid` with its message.
- **Status.** `status(id)`: `Clean`, `Unsaved` ("Edited"), `Saving`, `Saved`,
  `Invalid`, `Failed`, `Conflict`, each with a quiet `label()`. `unsaved()`
  lists drafts holding work the store doesn't have.
- **Leaving.** `close` flushes; a draft whose save is out or failed stays
  (reopening the task shows it), one fully stored is dropped.
- **Workflow.** `set_workflow` sends a workflow change at once, in its turn in
  the lane, with any typing (for `dt-task-inline-properties`; no desktop
  control yet).

### The shared mutation path

- `Mutation::EditIssue` gains `workflow`, `expect_title` and `expect_body`
  (`SaveJob::mutation` builds it). `EditSpec` gains `expect_title` /
  `expect_body` (serde-defaulted and skipped when absent, so the wire is
  unchanged for the CLI and older peers).
- The daemon's `edit` (`shelbi-orchestrator/src/mutate/add_edit.rs`) refuses
  an edit as stale when the stored title / body differs from the one it says
  it replaces, before any write. On a GitHub board a same-status revision bump
  isn't stale (the tracker bumps it on its own), so without this a save could
  overwrite an agent's edit the board index hadn't shown yet; the content
  guard catches it on both backends. The workflow change keeps the daemon's
  existing validation (the workflow must exist).

### Desktop

- `BoardState` (`views/board_state.rs`) drops `StagedEdit`, `edit_base` and
  the edit `MutationJob` body; it holds the project's shared `IssueEdits`.
  `begin_edit` opens the draft; `edit_text`, `flush_edit`, `end_edit`,
  `poll_saves`, `retry_save`, `resolve_conflict` and `save_external_edit`
  (the external editor's saves go through the draft too) return
  `Intent::Save(SaveJob)`.
- A move, start, assign or reorder asked for while the issue's save is out is
  **held**, not refused, and dispatched when the lane frees; with typing not
  yet saved, the typing is sent first (so a move to an active status hands
  the agent the new body). Each takes the lane while out, so a save waits for
  it and then expects the revision it landed at.
- `IssuesView` sends saves on a thread that records the answer straight into
  the shared store (a save that finishes after a project switch still
  lands), polls for due saves on its tick, flushes on the field's blur, Cmd+S
  (the editor stays open), Esc / Done / the close button, and any navigation
  away from the task (checked on the tick), and flushes every unsaved draft
  when the view stops (project switch). Text the store changes under the
  editor is spliced in (`issue_edit::Splice`), keeping the caret.
- The task pane (`views/task_detail.rs`, the floating pane from #1986): the
  body editor (More > Edit description, `e`) has no Save or Discard; **Done**
  (`Esc`) closes the editor, saving. Esc and the pane's Close save a changed
  draft through the store (an unchanged one sends nothing). A quiet line
  under the title (`pane-save-status`) shows the save status, with **Retry**
  for a failed save and **Keep mine** / **Use theirs** for a conflict. A
  draft left unsaved shows in place of the read body.
- Tests: `BoardState::new` gives each test board its own draft store (tests
  share project names), and a test `IssuesView` records its saves
  (`sent_saves`) instead of reaching the daemon.
- Visual harness: `--visual-views` adds `board-detail-editing.png` and
  `board-detail-unsaved.png` (a failed save, its draft and Retry).

## Decisions

- **The content guard on the daemon** rather than client-only merging: the
  board index can trail an agent's write by seconds, and on GitHub the
  revision gate lets a same-status change through, so only the daemon can
  promise "never overwrite text you haven't seen". The refusal reuses
  `MutationError::Stale` (no protocol change); its `expected` is the current
  state, since `edit` doesn't see the client's.
- **Merge granularity is lines, conservatively.** Adjacent or same-point
  changes are a conflict (as in diff3), titles never merge. A rebase changes
  the visible text by one splice; the textarea loses undo history for that
  splice only. The WYSIWYG editor can apply `Update`s its own way.
- **No timed auto-retry of a failed save**: a failing backend isn't hammered;
  the next keystroke, a flush (blur, Cmd+S, leaving) or Retry sends it.
- **Drafts live in process memory**, per project, not on disk: they survive
  closing the pane, switching task or project and reopening, not quitting
  the app. A failed final flush is logged (`tracing::warn`) and shown when the
  task is opened again.
- **Property-after-property is unchanged**: a second move while the first is
  unanswered is still refused, as before; only writes that would collide with
  a save are held.
- The edit's reason stays the CLI's default (`user:cli`), so events and
  Activity read as before.

## Left for the sibling tasks

- Title editing in the desktop UI and the WYSIWYG body
  (`dt-task-wysiwyg-editor`); the store already handles the title.
- The workflow picker and Ghost property controls
  (`dt-task-inline-properties`): `IssueEdits::set_workflow` is the path.
- The TUI has no in-place editor; nothing there changed.

## Verification

- `shelbi-app` `issue_edit::tests` (24): debounce and coalescing, no-op saves,
  typing during an in-flight save, acked-but-not-refreshed revisions and
  lagging copies, rapid title/body/property writes sharing the lane, clean
  fields following the store, line merges, conflicts and both resolutions, a
  withdrawn conflict, stale refusal then rebase and retry, the stale wait
  giving up, backend failure and retry, blank titles, closing with pending and
  failed saves, workflow refusal, late answers.
- `shelbi-orchestrator` `apply_edit_refuses_when_the_text_it_replaces_changed`
  (filesystem: refused before writing; trailing whitespace folded; untouched
  fields and frontmatter unchanged) and
  `apply_edit_on_github_refuses_a_save_over_a_body_it_hasnt_seen` (a canned
  GitHub: no PATCH when refused, a PATCH when it matches).
- `shelbi` `control_socket::an_open_draft_survives_a_cli_edit_and_never_overwrites_it`:
  a real sandboxed daemon and the real `shelbi issue edit` CLI. The CLI
  appends while the draft is open; the draft's save is refused, rebases on the
  next read and lands both edits with title, branch and status unchanged; a
  CLI edit of the same line leaves a conflict and saves nothing over it.
- Desktop `board_state` tests: saving without leaving the editor, Esc saving,
  a move held behind a save and expecting its landed revision, typing sent
  before a move, a save after a move expecting the move's revision through a
  lagging read, an agent's change merged into the open draft then a same-line
  conflict, the external editor through the store, a failed save surviving
  close and reopen.
- Root: `cargo build --workspace`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo check --workspace --locked` (root `Cargo.lock`
  unchanged), `cargo test -p shelbi-app -p shelbi-proto`, the orchestrator's
  `mutate::` tests, the TUI's `every_registry_command_runs_from_the_palette`.
- Desktop: `cargo build`, `cargo clippy --all-targets -- -D warnings` (and
  with `--features visual-check`), `cargo test` (822 passed, 1 ignored, after
  rebasing on #1986). `esc_in_the_body_editor_keeps_the_draft` types into the
  pane's editor: Esc saves the typed draft and keeps the pane, reopening shows
  the draft and an unchanged Esc sends nothing, Close with typing during the
  save keeps the newer text queued.
- After the rebase on #1986: root `cargo check --workspace --locked`, root
  clippy, `cargo test -p shelbi-app -p shelbi-proto`, the orchestrator's
  `mutate` tests and `shelbi --test control_socket` again.
- Looked at `board-detail-editing.png` and `board-detail-unsaved.png` in both
  themes.
