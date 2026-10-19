# dt-more-opens-detail: More opens the task detail over the screen

## The change

The Review and Workspace task panels show the issue title, a five-line
description preview and **More**. More used to expand the description in
place and turn into **Less**. It now opens the task's detail, the same modal
the Issues board opens for a card, over the current screen. The Review or
Workspace screen stays under it, and closing the modal (Esc, the close
button, a click on the scrim) returns to it with the same focus, panel row
and panes. The link always reads More, and the preview stays clamped.

## How

- **The board draws the detail.** The modal and everything it runs (fields,
  actions with keycaps, Markdown body, in-place editing with Cmd+S, open in
  editor, the popover keys, the assign picker) belong to `IssuesView` and
  its `BoardState`. Lifting them out would have meant moving the board's
  state, mutation channel, editor and refresh into a second owner, so the
  window reuses the board entity instead.
  `IssuesView::open_task_over` opens the detail and sets the view
  *floating*. A floating view renders only the detail and its assign picker
  (no board), filling its parent, with its own focus and key handler.
  `task_detail::render` is unchanged.
- **The window lays it over the main area.** `ShelbiWindow::open_task_over`
  (from the new `WorkspaceEvent::OpenTask` / `ReviewEvent::OpenTask`)
  creates the board entity if needed, opens the detail floating, and records
  `detail_over`: what the main area showed (`Shown`). `render_main` adds the
  floating board entity as an absolute layer over the main area. That means
  the scrim and the 80% x 85% modal size relative to the same main area the
  board gives them, and the panel column stays visible beside it, as the
  task's wireframe draws.
- **Closing.** The window observes the board entity. Once the board's mode is
  back to `Board` (any close path: Esc, the close button, the scrim, Start or
  an assign that ends the detail), the window clears `detail_over`, lands
  the board (no longer floating) and calls `focus_main_view`, which focuses
  the review's or workspace's own focus target. Their panel focus and
  selection were never touched.
- **The main area moving on.** If the main area changes while the detail is
  up (a sidebar row, a command), `settle_detail_over` (at the top of
  `render_shell`) closes the detail with it. If the board is what shows now,
  it only lands the detail, which stays open on the board. The board entity
  is never drawn twice.
- **The panel underneath refreshes.** While the detail is up, each board
  change pushes the detail model's title and body into the panel
  (`refresh_task_text` on the Workspace and Review views; a no-op unless
  it's the panel's own task and the text changed). An edit saved in the
  modal shows in the preview once the board reads it back.
- The Issues board's and Activity's entry points are unchanged: `open_task`
  still routes to the board and opens the detail there, not floating.

## Removed

- The `description_open` state in both views, the `expanded` argument of
  `task_panel::summary` and `task_panel::more_link`, and the Less label.
- The Workspace view's visual-check `description_open` /
  `toggle_description_for_check`, and the `review-description.png` /
  `workspace-description.png` frames of `--visual-review` and
  `--visual-workspace`. Those harnesses render the view without the window,
  which now owns the detail. `--visual-figma`'s `state-*-more` frames show it
  instead (below).

## Decisions

- **Reuse the board entity rather than a window-level overlay copy.** A
  second host would have needed the board's whole state machine, its reads
  and its mutation path. Reusing it means actions act on the right task
  through the same `IssuesEvent::Command` → `ShelbiWindow::run` path as on
  the board, the same refresh follows, and the modal looks and behaves the
  same because it is the same code.
- **Scrim over the main area only**, not the panel column, as the task's
  wireframe and the board draw it. The panel stays clickable under an open
  detail, as the sidebar does under the board's detail. A ⌘N there swaps
  the pane under the modal and the modal stays. A Back or another sidebar
  row closes it (the main area moved on).
- **A detail opened before the board's first read** shows "Loading the
  issue…" instead of "This issue is no longer on the board" (for the board
  too, which the Activity entry point could hit on a cold board).
- **Panel text refresh only while the detail is up.** If an edit is saved and
  the modal closed before the board reads the edit back, the panel keeps the
  old preview until its own next read (the Workspace view re-reads details
  on a row change; the Review view on re-entry). Keeping a sync alive after
  the modal closes would need the board's detail cache to outlive the
  detail, so I left that out.

## Checked

- `cd desktop && cargo build`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo clippy --all-targets --features visual-check -- -D warnings`: clean.
- `cargo test`: 514 passed. New and updated tests:
  - `views::workspace`: More (click, `m`, Enter on the More row) emits
    `OpenTask("t-1")` every time; the preview stays at five lines; the
    board's text refreshes the summary for its own task only.
  - `views::review`: the same for `review-more`, `m` and Enter on More, and
    `refresh_task_text`.
  - `app::column_tests`: More over the Workspace screen draws `issues-over`
    over the main area (not the panel), keeps the route (no
    `ShowView(Issues)`), and Esc closes it with the panel focused, the same
    row selected and the agent still attached; Enter on More reopens it.
    Over the Review screen, a click on the scrim closes it with the keys back
    in the review. Moving the main area to Machines closes the detail.
    Activity's `open_task` still routes to the board, not floating.
- `--visual-figma` (`SHELBI_VISUAL_ONLY=panels`, dark): 0 checks failed. The
  `state-review-more` / `state-workspace-more` frames now check that More
  opens the detail over the screen and that it loads its issue. By eye, the
  modal matches `issue-detail` (same fields, buttons with keycaps, Markdown
  body), dimming the main area with the panel beside it.
- Not done: a hand walk in the real app against a live daemon. The window
  tests drive the real shell headlessly.
