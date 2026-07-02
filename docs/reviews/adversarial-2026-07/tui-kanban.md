# Adversarial review: TUI kanban view (shelbi-tui)

Reviewed:
- `crates/shelbi-tui/src/kanban.rs` (4509 lines; ~2571 lines production, remainder `#[cfg(test)]`)
- `crates/shelbi-tui/src/handlers/kanban.rs` (480 lines; the event loop + key/mouse dispatch — the task's stated `handlers/kanban.rs` path)

> **Scope note.** The task brief listed a 770-line `kanban.rs` with no `handlers/`
> directory; that matched the branch's stale checkout (`b4e90a8`). The worktree is
> now at `origin/main` (`a106e8c`), where `kanban.rs` is 4509 lines and the input
> handler lives at `crates/shelbi-tui/src/handlers/kanban.rs` exactly as the brief
> described. All findings below quote the current tree. Supporting code in
> `shelbi-state` / `shelbi-core` is cited for context only; findings are anchored
> in the two scope files.

| # | Finding | Severity | Confidence | Category |
|---|---------|----------|------------|----------|
| F1 | Card titles/meta use char-count, not display width — wide (CJK/emoji) glyphs overflow the column gutter and bleed into the neighbour | medium | certain | bug |
| F2 | Moving a card into the **Canceled** column is a silent no-op that strands the selection on an empty lane | medium | certain | bug |
| F3 | `reorder_up/down` on a card in the **Done** column reorders against the wrong axis and floats the card to the top via an `updated_at` bump | medium | certain | bug |
| F4 | Card click hit-testing ignores vertical scroll — in a tall column, clicks select the wrong task | medium | certain | bug |
| F5 | `move_card` has no optimistic-concurrency guard; a concurrent writer between refresh and keypress can skip the branch cut or clobber priorities | medium | likely | failure-scenario |
| F6 | `refresh()` rebuilds `all_columns` but never clamps `selected_column`; a shrunk `statuses.yml` leaves focus silently pointing off the end | low | certain | bug |
| F7 | Dropdown toggles (`f`/`w`) and the dropdown key handlers bypass the keymap layer, so rebinds don't apply inside them | low | certain | best-practice |
| F8 | Workspace and workflow dropdowns are ~near-identical duplicated code (render + 6 methods + handler) | low | certain | simplification |
| F9 | `kanban.rs` mixes state, navigation, column math, filters and five renderers in one 4.5k-line module | low | certain | simplification |
| F10 | Popover body scroll has no lower bound — `j` past the end shows a blank body with no stop, recoverable only via `g` | low | certain | best-practice |

---

## F1: Card title / meta width math counts chars, not display columns

- **Where:** crates/shelbi-tui/src/kanban.rs:2252 (`truncate`), :1811, :1818-1819, :1828-1835
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**
  `truncate` measures and cuts by `char` count:
  ```rust
  fn truncate(s: &str, max: usize) -> String {
      if s.chars().count() <= max {
          return s.to_string();
      }
      let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
  ```
  and the column budget is a raw cell count:
  ```rust
  let max_text = area.width.saturating_sub(2) as usize;   // :1811
  let title_text = truncate(&format!("{badge_prefix}{}", tf.task.title), max_text);  // :1819
  ```
  The meta line reuses the same char arithmetic:
  ```rust
  let id_w = tf.task.id.chars().count();                       // :1828
  let workspace_w = tf.task.assigned_to.as_deref()
      .map(|w| 3 + w.chars().count()).unwrap_or(0);           // :1829-1834
  let meta_line = if id_w + workspace_w <= max_text { ... }   // :1835
  ```
  A terminal cell grid allocates **two columns** per East-Asian-wide or emoji
  scalar. `char` count treats each as 1. The blocked badge is itself
  mis-measured: `"🔒 "` is 2 chars but renders as 3 cells (:1818).
- **Failure scenario:** a task titled `評価コンポーネントの再設計` (13 wide chars)
  in a 16-cell column: `13 ≤ 14 == max_text`, so `truncate` returns it unchanged,
  ratatui paints ~26 cells, and the overflow spills across the 2-cell gutter into
  the adjacent column's cards. A title of 14 emoji does the same. Because `List`
  does not clip `Line` spans (noted in the code's own comment at :1809), the
  neighbour column is corrupted for that row.
- **Recommendation:** measure with `unicode-width` (`UnicodeWidthStr::width`) in
  `truncate` and in the `max_text` comparisons; truncate to a target *display
  width*, not a char count. Apply the same to `column_label` collapsed-strip math
  if custom status names can contain wide glyphs.
- **Effort:** M

## F2: Moving a card into "Canceled" silently no-ops and strands the selection

- **Where:** crates/shelbi-tui/src/kanban.rs:2374-2386 (`category_to_column`), :1223-1224 and :1257-1281 (`move_card`); context: `shelbi_state::move_task` returns `Ok(None)` when the column is unchanged (crates/shelbi-state/src/lib.rs:1412)
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**
  `default_project_statuses()` and `default_workflow()` both declare a sixth
  status `canceled` with `category: Archived` (crates/shelbi-core/src/statuses.rs:133-135,
  crates/shelbi-core/src/workflow.rs:512-514), so the board renders a **Canceled**
  column (index 5) and `adjacent_column_in_workflow` treats it as an eligible
  move target for the default workflow. But the move maps it back through:
  ```rust
  StatusCategory::Archived => Column::Done,   // :2384
  ```
  ```rust
  let new_col = category_to_column(target_col.category);   // :1224
  ```
  and `move_task` no-ops when the on-disk column doesn't change:
  ```rust
  if task.column == new_column { return Ok(None); }   // state lib:1412
  ```
  A Done card being "moved" to Canceled therefore resolves to
  `move_task(id, Column::Done)` → `Ok(None)` → nothing written. `move_card` then
  still runs its follow logic:
  ```rust
  self.status_line = format!("{id} → {}", target_col.status_name);  // :1271 — says "→ Canceled"
  self.refresh();
  self.selected_column = new_col_idx;                                // :1274 — focus jumps to col 5
  if let Some(row) = self.column_tasks(new_col_idx).iter()
      .position(|tf| tf.task.id == id) { self.selected_row = row; } // :1275-1281 — never matches
  ```
  Because task storage has no Archived bucket, `resolved_status_id` can never
  return `"canceled"`, so **column 5 is permanently empty** and no card can ever
  land in it.
- **Failure scenario:** user selects a Done card and presses `L` (move right).
  Status line reads `fix-login → Canceled` (a lie — disk is unchanged), focus
  jumps to the empty Canceled column, and the card is left in Done. The user
  cannot cancel a task from the board at all, and the selection is now detached
  from any card.
- **Recommendation:** either (a) give task storage a real Archived/Canceled state
  so `category_to_column` stops collapsing it onto `Done` (the code's own TODO at
  :2381-2385), or (b) until then, drop `Archived`-category statuses from the move
  target set and from the rendered board so the UI doesn't advertise an
  unreachable lane. At minimum, don't set a success status line when `move_task`
  returns `Ok(None)`.
- **Effort:** M

## F3: Reorder on a Done card reorders the wrong axis and jumps the card to the top

- **Where:** crates/shelbi-tui/src/kanban.rs:1284-1310 (`reorder_up`/`reorder_down`/`reorder`), :570-574 (Done sort); context: `set_task_priority` + `write_column_order` (crates/shelbi-state/src/lib.rs:1428-1460)
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**
  The Done column is displayed sorted by `updated_at` DESC, every other column by
  priority:
  ```rust
  if ac.category == StatusCategory::Done {
      tasks.sort_by_key(|tf| std::cmp::Reverse(tf.task.updated_at));  // :573
  }
  ```
  `selected_row` indexes that *displayed* order, and reorder passes it straight to
  the priority API:
  ```rust
  pub fn reorder_up(&mut self) {
      if self.selected_row == 0 { return; }
      self.reorder(self.selected_row - 1);          // :1288
  }
  fn reorder(&mut self, new_pos: usize) {
      ...
      set_task_priority(&self.project_name, &id, new_pos as u32) ...  // :1304
      self.refresh();
      self.selected_row = new_pos;                                    // :1309
  }
  ```
  `set_task_priority` reorders against the **priority-sorted** column and
  `write_column_order` stamps `updated_at = now` on every task whose slot changed
  (state lib:1449-1457). Two things go wrong for Done: (1) `new_pos` was computed
  from the `updated_at`-sorted display, so it names the wrong target slot in the
  priority list; (2) the `updated_at` bump means that on the next `refresh()` the
  touched card re-sorts to the **top** of the Done column regardless of the
  direction the user pressed.
- **Failure scenario:** in a Done column showing `[newest, middle, old]`, the user
  selects `old` (row 2) and presses `K` (reorder up). Intent: swap with `middle`.
  Actual: `old.updated_at` is set to now, and after refresh Done re-sorts to
  `[old, newest, middle]` — the card jumps to the top. `selected_row` is set to 1,
  now pointing at `newest`, so focus also lands on the wrong card.
- **Recommendation:** disable reorder in the Done column (its order is derived, not
  user-controlled), or translate `selected_row` back through the displayed→priority
  mapping before calling `set_task_priority` and suppress the `updated_at` rewrite
  for pure reorders.
- **Effort:** M

## F4: Card click hit-testing ignores the list's vertical scroll offset

- **Where:** crates/shelbi-tui/src/kanban.rs:1880-1912 (`render_column` hit-rect loop)
- **Category:** bug
- **Severity / Confidence:** medium / certain
- **Evidence:**
  The `List` is rendered with a `ListState` that ratatui auto-scrolls to keep the
  selected row visible:
  ```rust
  let mut state = ListState::default();
  if focused && !tasks.is_empty() { state.select(Some(app.selected_row)); }  // :1881-1883
  f.render_stateful_widget(list, list_area, &mut state);                     // :1885
  ```
  But `card_hits` are always laid out from the top of `list_area`, row 0 first,
  with no scroll term:
  ```rust
  const ROWS_PER_CARD: u16 = 3;
  for (row, _) in tasks.iter().enumerate() {
      let card_top = list_area.y.saturating_add(row as u16 * ROWS_PER_CARD);  // :1897
      if card_top >= list_bottom { break; }
      ...
      hits.push(CardHit { area: Rect { ... }, col_idx, row_idx: row });       // :1902-1911
  }
  ```
  The in-code comment (:1889-1893) acknowledges this ("Scroll offsets aren't
  tracked here … will mis-attribute clicks for any row off-screen; tolerable until
  columns regularly exceed visible height"), but the failure is not limited to
  off-screen rows: once the list has scrolled, the *visible* cards are rows
  `offset..offset+N`, while the hit-rects still map screen positions to rows
  `0..N`. Every visible click resolves to the wrong task.
- **Failure scenario:** a Backlog column with 30 cards in a 20-row pane. User
  arrows down to card 25 (list scrolls); the top visible card is now card ~6. User
  clicks that top card to open it — `card_at` returns `row_idx: 0`, so the popover
  opens card 0, not card 6.
- **Recommendation:** track the list's scroll offset (compute it the same way
  ratatui does, or keep an explicit offset in `KanbanApp`) and add it to `row` when
  building `CardHit`s, skipping rows above the offset. Alternatively cap column
  height and paginate so the offset stays 0.
- **Effort:** M

## F5: `move_card` trusts the in-memory snapshot — no optimistic-concurrency guard

- **Where:** crates/shelbi-tui/src/kanban.rs:1216-1282 (`move_card`), esp. :1231-1256 (branch-cut guard) and :1257 (`move_task`)
- **Category:** failure-scenario / assumption
- **Severity / Confidence:** medium / likely
- **Evidence:**
  The kanban view runs with no lock and refreshes only every 750 ms
  (`maybe_refresh`, :1085-1089). Task files are also written by the orchestrator,
  workers, the sidebar's `WorkspacePoller`, and the CLI. `move_card` makes two
  decisions from the possibly-stale in-memory `self.tasks`:
  ```rust
  if tf.task.column != Column::InProgress {                 // :1237 — stale column
      match shelbi_state::load_project(&self.project_name) {
          Ok(project) => { ensure_branch_for_in_progress(&project, id) ... }
  ```
  and then calls `shelbi_state::move_task` (:1257), which is a non-atomic
  read-modify-write plus a multi-file `renumber_column` (state lib:1411-1423).
  There is no compare-and-set on the task's expected prior column.
- **Failure scenario:** the poller/orchestrator moves `t` from `todo` to
  `in_progress` (cutting its branch) at t=0. At t=+200 ms — before the next
  750 ms refresh — the user presses `L` on the card they still see in `todo`. The
  guard reads the stale `column == Todo`, re-runs `ensure_branch_for_in_progress`
  (redundant, and it can now *fail* if the branch already exists, aborting the
  move with `branch cut failed`), and `move_task` overwrites the freshly-written
  file. Concurrent `renumber_column` calls from two processes can also interleave
  and leave duplicate priorities in a column.
- **Recommendation:** anchor the move on the id + expected-from-column and have the
  state layer reject/replay if the on-disk column has changed (optimistic
  concurrency), or take a per-project file lock around move/renumber. At minimum,
  re-`refresh()` immediately before acting on a keypress so the guard sees current
  state, and treat "branch already exists" as success in the in-progress hook.
- **Effort:** L

## F6: `refresh()` rebuilds columns but never clamps `selected_column`

- **Where:** crates/shelbi-tui/src/kanban.rs:688-693 (`refresh`), :1094-1101 (`clamp_selection`), :554-557 (`column`)
- **Category:** bug
- **Severity / Confidence:** low / certain
- **Evidence:**
  `refresh` recomputes the column set (which can shrink when `statuses.yml` is
  edited or a workflow filter narrows it) and then clamps only the *row*:
  ```rust
  self.all_columns = self.compute_all_columns();   // :688 — count may drop
  match shelbi_state::list_tasks(&self.project_name) {
      Ok(tasks) => { self.tasks = tasks; self.last_refresh = Instant::now();
          self.clamp_selection(); }                // :693 — rows only
  ```
  ```rust
  pub fn clamp_selection(&mut self) {
      let n = self.column_tasks(self.selected_column).len();   // :1095 — uses clamped column()
      ...
  }
  ```
  `selected_column` is left untouched. `column()` masks the out-of-range index:
  ```rust
  let last = self.all_columns.len().saturating_sub(1);
  &self.all_columns[idx.min(last)]                             // :555-556
  ```
  so there is no panic — but focus, the header underline, and `column_tasks`
  silently operate on the last column while `selected_column` holds a larger value.
  (`apply_workflow_filter` does clamp `selected_column` at :984-987; `refresh` does
  not, so an external `statuses.yml` shrink is the gap.)
- **Failure scenario:** user has column 5 selected; `statuses.yml` is edited down to
  4 statuses; next poll refresh leaves `selected_column == 5` while only 4 exist.
  The board underlines column 3 (the clamp target) but `nav_left` decrements from 5,
  taking two presses before selection re-enters the visible range.
- **Recommendation:** in `refresh`, clamp `selected_column` to
  `all_columns.len().saturating_sub(1)` (and reset `column_scroll`) before
  `clamp_selection`, mirroring `apply_workflow_filter`.
- **Effort:** S

## F7: Dropdown toggles and dropdown key handling bypass the keymap layer

- **Where:** crates/shelbi-tui/src/handlers/kanban.rs:98-109 (`f`/`w` fallthrough), :200-222 (dropdown handlers)
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:**
  Every board action routes through the merged keymaps, except the two dropdown
  toggles, which are hardcoded:
  ```rust
  None => {
      match key.code {
          KeyCode::Char('f') => app.toggle_workspace_dropdown(),   // :105
          KeyCode::Char('w') => app.toggle_workflow_dropdown(),    // :106
          _ => {}
      }
  }
  ```
  and once a dropdown is open the handler reads raw `key.code`, ignoring
  `Keymaps` entirely:
  ```rust
  pub fn handle_workspace_dropdown_key(app: &mut KanbanApp, code: KeyCode) {
      match code {
          KeyCode::Up | KeyCode::Char('k') => app.dropdown_nav_up(),   // :203
          KeyCode::Down | KeyCode::Char('j') => app.dropdown_nav_down(),
          ...
  ```
  A user who rebinds `nav_up`/`nav_down` (or whose `f`/`w` are bound to a kanban
  action — in which case `km.kanban.dispatch` at :86 wins and the dropdown can
  never open) gets inconsistent behaviour. The footer even hardcodes `f filter`
  (kanban.rs:1924) while every other glyph is resolved from the keymap.
- **Recommendation:** add `ToggleWorkspaceFilter` / `ToggleWorkflowFilter` (and
  dropdown nav) to the action enums and dispatch them through `Keymaps`, as the
  in-code comment at :99-103 already anticipates.
- **Effort:** M

## F8: Workspace and workflow dropdowns are duplicated wholesale

- **Where:** crates/shelbi-tui/src/kanban.rs:1954-2072 (`render_workspace_dropdown`) vs :2090-2194 (`render_workflow_dropdown`); the paired `open_/close_/toggle_/nav_up/nav_down/select` methods (:749-941); handlers/kanban.rs:200-222
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:** the two `render_*_dropdown` functions are byte-for-byte identical
  apart from the options source, chip rect, title string and min width (`24` vs
  `22`). The state methods (`dropdown_nav_up`/`workflow_dropdown_nav_up`,
  `dropdown_select`/`workflow_dropdown_select`, etc.) and the two handler functions
  differ only in which field they mutate. `WorkspaceDropdown` and
  `WorkflowDropdown` are both `{ cursor: usize }` (:257-275), and the doc comment at
  :266-269 already flags the split as speculative ("so a future workflow-only
  field … has somewhere to live").
- **Recommendation:** extract a generic `Dropdown<T>` carrying `cursor` + options,
  a single `render_dropdown` parameterised by title/anchor/row-formatter, and one
  handler. Removes ~200 lines and keeps the two filters from drifting.
- **Effort:** M

## F9: `kanban.rs` is a 4.5k-line module mixing state, nav, column math and five renderers

- **Where:** crates/shelbi-tui/src/kanban.rs (whole file)
- **Category:** simplification
- **Severity / Confidence:** low / certain
- **Evidence:** one module holds the `KanbanApp` struct + eight satellite structs
  (:41-341), all navigation/move/reorder/filter state logic (:343-1311), the
  column builder and status resolution (:2314-2386), and five distinct renderers
  (`render_columns`, `render_collapsed_column`, `render_column`,
  `render_*_dropdown`, `render_popover`) plus ~1900 lines of tests. The
  `#[allow(clippy::too_many_arguments)]` on `render_column` (:1765) is a symptom of
  render state being threaded by hand rather than grouped.
- **Recommendation:** split along the seams the code already has:
  - `kanban/model.rs` — `KanbanApp`, selection/nav/move/reorder, `clamp_*`.
  - `kanban/columns.rs` — `KanbanColumn`, `kanban_columns_from`,
    `resolve_task_status`, `category_to_column`, `compute_column_widths`.
  - `kanban/filters.rs` — the generic dropdown (F8) + `WorkspaceFilter` state.
  - `kanban/render.rs` — the renderers, taking a small `RenderCtx` struct instead
    of 8 positional args.

  Group the frame-written hit-test vecs (`card_hits`, `header_hits`,
  `dropdown_hits`, `workflow_dropdown_hits`, `*_chip_hit`) into one `HitRegions`
  sub-struct so "render writes / handler reads" is one field, not seven.
- **Effort:** L

## F10: Popover body scroll has no lower bound

- **Where:** crates/shelbi-tui/src/kanban.rs:523-527 (`popover_scroll_down`), :535-539 (`popover_scroll_page_down`), :2450-2454 (render)
- **Category:** best-practice
- **Severity / Confidence:** low / certain
- **Evidence:**
  ```rust
  pub fn popover_scroll_down(&mut self) {
      if let Some(p) = self.popover.as_mut() { p.scroll = p.scroll.saturating_add(1); }  // :524-526
  }
  ```
  `scroll` is applied verbatim to the `Paragraph`:
  ```rust
  let scroll = app.popover.as_ref().map(|p| p.scroll).unwrap_or(0);
  let body = Paragraph::new(body_text).wrap(Wrap { trim: false }).scroll((scroll, 0));  // :2450-2453
  ```
  There's no clamp against the wrapped body's line count, so holding `j` scrolls the
  body entirely off the top, leaving a blank pane. It is recoverable (`g` →
  `popover_scroll_home`), but the "empty body" state reads as a rendering bug to the
  user.
- **Failure scenario:** open a task with a 5-line description, press and hold `j`;
  after ~6 presses the body is blank with no indication it's over-scrolled.
- **Recommendation:** clamp `scroll` to `max(0, wrapped_line_count - viewport_rows)`
  in the render pass (ratatui exposes line-count via `Paragraph::line_count` in
  recent versions), or track the last-rendered body height and cap
  `popover_scroll_down`.
- **Effort:** S

---

## Notes on things that held up (checked, not defects)

- **Move-by-id, not index.** `move_card_left/right` resolve the task id from
  `selected_task()` before acting (:1145-1151), so a background reorder between
  refreshes can't move the wrong card — only the follow-selection can miss (F2/F5).
- **Popover keyed by task id.** `TaskPopover` stores `task_id` and re-looks-up each
  frame (:496-499, :2398-2409), so a refresh that reorders/deletes the underlying
  card can't swap the popover contents; a deleted task degrades to the "no longer
  exists" body rather than panicking.
- **Index math is panic-safe.** `column()` clamps with `idx.min(last)` (:555),
  `nav_left/right` guard with `.max(1)` (:1104, :1114), and dropdown cursors clamp
  with `min(len-1)` before `ListState::select` (:2056). No out-of-bounds indexing
  was found even with a stale `selected_column` (F6 is a focus-correctness issue,
  not a crash).
- **Empty-column nav.** `nav_up/down` early-return on empty columns (:1119-1137);
  `clamp_selection` resets `selected_row` to 0 on empty (:1094-1101).
</content>
</invoke>
