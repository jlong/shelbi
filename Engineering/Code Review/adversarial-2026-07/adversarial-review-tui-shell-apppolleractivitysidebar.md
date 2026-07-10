# Adversarial review: TUI shell (app/poller/activity/sidebar)

Reviewed:

- crates/shelbi-tui/src/activity.rs (2669 lines)

- crates/shelbi-tui/src/app.rs (1908 lines)

- crates/shelbi-tui/src/poller.rs (1332 lines)

- crates/shelbi-tui/src/sidebar.rs (891 lines)

- crates/shelbi-tui/src/lib.rs (552 lines)

- crates/shelbi-tui/src/zen\_probe.rs (406 lines)

- crates/shelbi-tui/src/review\.rs (386 lines)

- crates/shelbi-tui/src/handlers/activity.rs (327 lines)

- crates/shelbi-tui/src/handlers/sidebar.rs (265 lines)

Supporting code read for context (findings are reported against scope files only):
`shelbi-state` (task/status/event IO), `shelbi-orchestrator/src/workspace.rs`
(marker + rebase), `shelbi-ssh` (transport options), `shelbi-tmux` (pane title),
`shelbi-cli/src/main.rs` (entry wiring).

Notes on the seed questions:

- **Poller thread vs UI thread**: no shared in-memory state — the poller and the
  sidebar communicate exclusively through files (`status.yaml`, `events.log`,
  task YAMLs). No data races found there. The real problems are on-disk races
  (F3), blocking IO inside the draw path (F2, F11), and unbounded blocking of
  poll threads (F7).

- **zen\_probe.rs subprocess handling**: the seed's premise doesn't hold —
  `zen_probe.rs` spawns no subprocesses; it is a pure keyboard probe + chooser.
  No zombie/timeout issues exist there. Its state machines are well tested.
  One minor liveness note is folded into F21.

- **Resize/zero-width**: arithmetic in the scope files consistently uses
  `saturating_sub`/`min` (`centered_rect`, `paint_row`, `right_align`,
  `render_zen_row`); I could not construct a panicking width/height. No finding.

- **Keys leaking through modals**: the only modal in scope is the zen-probe
  chooser, which owns the event loop exclusively while open — no leak-through.

| #   | Finding                                                                                                                                                                                                                                | Severity | Confidence | Category         |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ---------- | ---------------- |
| F1  | No panic hook: a panicking draw leaves the terminal in raw mode + alt screen with the panic message invisible                                                                                                                          | medium   | certain    | hardening        |
| F2  | Activity feed does O(all-events) work — including one `stat()` per task event — inside every draw (\~5×/s)                                                                                                                             | medium   | certain    | bug              |
| F3  | `stat`-then-`read_to_end` race in `ActivityApp::refresh` duplicates tail events permanently                                                                                                                                            | medium   | certain    | bug              |
| F4  | Sidebar mouse hit-test ignores the list's scroll offset — clicks activate the wrong row once the list scrolls                                                                                                                          | medium   | certain    | bug              |
| F5  | Pane-title review promotion skips the auto-rebase and ContextStore sync that the marker path performs                                                                                                                                  | medium   | certain    | bug              |
| F6  | Auto-rebase rewrites the worktree while the agent may still be running in it                                                                                                                                                           | medium   | likely     | failure-scenario |
| F7  | A hung SSH command permanently and silently stops polling for that workspace — no timeout, no respawn, no staleness surfaced                                                                                                           | medium   | likely     | failure-scenario |
| F8  | Workspace status and events are keyed by bare workspace name — two projects using the default phonetic names collide                                                                                                                   | medium   | certain    | assumption       |
| F9  | Dead workspace pane leaves the sidebar badge stuck on "Working" indefinitely                                                                                                                                                           | medium   | likely     | failure-scenario |
| F10 | 750 ms auto-refresh can restructure the row list between draw and Enter/click — activation lands on the wrong row                                                                                                                      | medium   | likely     | bug              |
| F11 | Review checkout / view focus runs synchronously in the key handler — UI freezes with no feedback for the duration of git/SSH work                                                                                                      | medium   | certain    | best-practice    |
| F12 | Review marker is cleared (handoff dropped) on *any* `load_task` error, including transient IO failures                                                                                                                                 | medium   | likely     | hardening        |
| F13 | Marker-supplied task id is unsanitized — `../` traversal reaches `task_path` and cross-project task files                                                                                                                              | medium   | certain    | hardening        |
| F14 | `ActivityApp.status_line` is written but never rendered — zen-toggle feedback and read errors are invisible                                                                                                                            | low      | certain    | bug              |
| F15 | `auto_scroll` is dead state; the anchoring behavior it documents doesn't exist, so new events yank a scrolled-back reader                                                                                                              | low      | certain    | bug              |
| F16 | `total_lines`/`scroll` are `u16` — long feeds truncate at 65 535 lines and break scroll clamping                                                                                                                                       | low      | certain    | bug              |
| F17 | `review.rs`'s `run_tmux` inherits stdio (screen corruption on tmux errors); duplicates `app.rs`'s null-stdio helper                                                                                                                    | low      | certain    | bug              |
| F18 | `ReviewApp::scroll_body_down` has no bottom clamp — scrolls into unbounded blank space                                                                                                                                                 | low      | certain    | bug              |
| F19 | `run_activity` skips the pre-alt-screen keymap-diagnostics logging every other view performs                                                                                                                                           | low      | certain    | best-practice    |
| F20 | Any `fs::metadata` error (not just NotFound) wipes the activity feed and resets the read offset                                                                                                                                        | low      | likely     | failure-scenario |
| F21 | Smaller items: `restore_terminal` masks the loop error; `App::refresh` returns an infallible `Result` and silently degrades to empty lists; no single-poller lock; dead `indent_w`; zen chooser can spin forever on a keyless terminal | low      | certain    | simplification   |

***

## F1: No panic hook — a panicking draw leaves the terminal in raw mode + alt screen

- **Where:** crates/shelbi-tui/src/lib.rs:292-306 (and every `run_*` entry point)

- **Category:** hardening / failure-scenario

- **Severity / Confidence:** medium / certain

- **Evidence:** `setup_terminal` enables raw mode, alt screen, and mouse capture:

  ```rust
  fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
      enable_raw_mode()?;
      let mut stdout = io::stdout();
      execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
  ```

  `restore_terminal` is only reached on the ordinary return path, e.g.
  lib.rs:213-215:

  ```rust
  let result = handlers::sidebar::sidebar_loop(&mut term, &mut app);
  restore_terminal(&mut term).context("restoring terminal")?;
  ```

  `grep -rn "set_hook\|catch_unwind" crates/` returns nothing — no panic hook is
  installed anywhere in the workspace. An `Err` from the loop still restores
  (good), but a panic unwinds straight past `restore_terminal`.

- **Failure scenario:** any `unwrap`/index panic inside `term.draw` (or a ratatui
  internal panic) kills the process with the pane left in raw mode + alt screen +
  mouse capture. The panic message itself is printed into the alternate screen
  and vanishes when anything else redraws. For the stash-hosted views the parent
  `while true` respawn loop papers over it (the new process re-enters the alt
  screen), which means panics are also *silently looped* — a deterministic
  panic-on-draw becomes an invisible crash loop with no message anywhere except
  possibly `tui.log`.

- **Recommendation:** in each `run_*` (or once in the CLI entry), install the
  standard ratatui panic hook: chain `std::panic::take_hook()`, and in the new
  hook run `disable_raw_mode()` + `LeaveAlternateScreen` + `DisableMouseCapture`
  before delegating to the original hook so the panic message lands on the
  normal screen.

- **Effort:** S

## F2: Activity feed does O(all-events) work — with per-event disk stats — inside every draw

- **Where:** crates/shelbi-tui/src/activity.rs:846-861 (`render_body`), 895-966 (`build_lines`), 522-553 (`task_meta`), 558-565 (`started_at`)

- **Category:** bug (performance) / failure-scenario

- **Severity / Confidence:** medium / certain

- **Evidence:** `activity_loop` calls `term.draw` on every iteration of a 200 ms
  poll loop (handlers/activity.rs:19-22), i.e. \~5×/s even when idle. Each draw
  runs `render_body` → `build_lines(app, width, now)`, which walks **every**
  event ever parsed:

  ```rust
  let order: Vec<usize> = (0..app.events.len())
      .rev()
      .filter(|&i| filter.matches(&app.events[i]))
      .collect();
  ...
  for idx in order {
      let ev = app.events[idx].clone();
  ```

  For every `Event::Task` in the history, `render_task_event` calls
  `app.task_meta(id)`, which stats the task file on disk *per call, per frame*:

  ```rust
  let mtime = fs::metadata(&path).ok().and_then(|m| m.modified().ok());
  ```

  For every review handoff, `started_at` additionally does a reverse linear scan
  of all prior events (activity.rs:558-565). `self.events` is never pruned
  (refresh only appends, activity.rs:464-469), and `events.log` is a
  project-lifetime append-only file.

- **Failure scenario:** a months-old project with, say, 10 000 task events means
  ~50 000 `stat()` syscalls per second plus tens of thousands of `Event` clones
  and full-history `Vec<Line>` rebuilds, permanently, in the render loop of a
  pane that is usually just sitting there. CPU usage and draw latency grow
  without bound over the life of the log; the "blocking IO on the render path"
  seed question is answered affirmatively here.

- **Recommendation:** (1) cache built lines and rebuild only when
  events/filter/width change (a dirty flag set by `refresh` and the toggle
  methods); (2) render only the viewport slice instead of materializing every
  line; (3) `task_meta`'s mtime check already caches content — hoist the *stat*
  behind the same 500 ms refresh cadence rather than per-frame; (4) consider
  capping `events` (e.g. keep the newest N thousand).

- **Effort:** M

## F3: `stat`-then-read race in `ActivityApp::refresh` duplicates tail events

- **Where:** crates/shelbi-tui/src/activity.rs:427-461, 571-580 (`read_tail`)

- **Category:** bug (race condition)

- **Severity / Confidence:** medium / certain

- **Evidence:** `refresh` captures the length first, then reads to EOF, then
  stores the *stale* length as the consumed offset:

  ```rust
  let meta = match fs::metadata(&path) { ... };
  let len = meta.len();
  ...
  let text = match read_tail(&path, self.log_offset) { ... };  // reads to EOF
  self.log_offset = len;
  ```

  `read_tail` uses `read_to_end`, which consumes everything currently in the
  file — including bytes appended *after* the `fs::metadata` call.

- **Failure scenario:** poller thread (same process!) appends a workspace event
  between the `metadata` call and `read_to_end`. This refresh parses and pushes
  that event, but `log_offset` is set to the pre-append `len`. The next refresh
  sees `len > log_offset` and re-reads the same bytes — the event is pushed a
  second time and rendered as a duplicate feed row forever (events are never
  deduplicated or pruned). The window is small but the trigger (poller writing
  while the activity view refreshes every 500 ms) runs continuously, so over a
  long session duplicates accumulate.

- **Recommendation:** set `self.log_offset += text.len() as u64` (bytes actually
  consumed) instead of the pre-read `len`; or open the file first and derive the
  length from the same handle (`f.metadata()`) before seeking. A related edge:
  the writer's `append_event_line` is a single `write_all`, so torn lines are
  unlikely on local filesystems, but offset-by-bytes-consumed handles that case
  too.

- **Effort:** S

## F4: Sidebar click hit-test ignores the list's scroll offset

- **Where:** crates/shelbi-tui/src/app.rs:214-228 (`row_at`), crates/shelbi-tui/src/sidebar.rs:69-75

- **Category:** bug

- **Severity / Confidence:** medium / certain

- **Evidence:** `row_at` maps a click's screen row straight to a rows-vec index:

  ```rust
  let idx = (row - area.y) as usize;
  let rows = self.rows();
  rows.get(idx).filter(|r| r.is_selectable()).map(|_| idx)
  ```

  But the renderer uses a stateful `List` that scrolls to keep the selection
  visible (sidebar.rs:69-75):

  ```rust
  let mut state = ListState::default();
  state.select(Some(app.sidebar_index));
  ...
  f.render_stateful_widget(list, inner, &mut state);
  ```

  When `sidebar_index` exceeds the viewport height, ratatui renders the list
  starting from a non-zero internal offset (recomputed each frame from the
  default-0 `ListState`). Screen row *y* then displays `rows[offset + y]`, but
  `row_at` returns `rows[y]`.

- **Failure scenario:** a project with enough workspaces/review tasks to
  overflow a short sidebar pane; the user keys down past the fold (list scrolls),
  then clicks a row. `handle_mouse` (handlers/sidebar.rs:95-99) sets
  `sidebar_index` to the wrong index and calls `activate_selection` — focusing
  the wrong workspace or, worse, kicking off a review checkout for a different
  task than the one clicked (`View::ReviewTask` → `start_review`).

- **Recommendation:** track the list offset explicitly — keep a persistent
  `ListState` on `App` (or compute the offset the same way ratatui does) and add
  it to the index in `row_at`. Kanban-style: store the first visible index at
  render time next to `list_area`.

- **Effort:** S

## F5: Pane-title review promotion skips auto-rebase and ContextStore sync

- **Where:** crates/shelbi-tui/src/poller.rs:401-429 vs poller.rs:466-512

- **Category:** bug / assumption

- **Severity / Confidence:** medium / certain

- **Evidence:** two independent promotion paths exist in `poll_one`. The marker
  path performs rebase + sync before/after the move:

  ```rust
  rebase_workspace_branch_before_review(project, workspace, machine, host, &task_id);
  match shelbi_state::move_task(&project.name, &task_id, Column::Review) { ... }
  ...
  sync_contextstore_from_workspace(project, machine, &tf.body);
  ```

  The pane-title path (`marker == PaneMarker::Review`, poller.rs:401-429) calls
  only `move_task` + `append_task_event` with reason `workspace:review-pane` —
  no rebase, no ContextStore sync.

- **Failure scenario:** a workspace whose hooks still set the `shelbi:review`
  pane title but whose marker write failed (disk full, `.claude/` missing,
  older settings template) gets its task promoted via the title path. The
  reviewer sees a stale diff against an old base (exactly what the auto-rebase
  exists to prevent), and any ContextStore writes on a remote machine are never
  pulled back to hub. Whether a given handoff gets the rebase becomes an
  accident of which signal the poller saw first.

- **Recommendation:** either route both signals through one promotion function
  (marker path's body), or delete the pane-title promotion entirely — the module
  doc (poller.rs:17-21) already declares the file marker the canonical handoff
  precisely because titles are unreliable.

- **Effort:** S

## F6: Auto-rebase rewrites the worktree while the agent may still be running in it

- **Where:** crates/shelbi-tui/src/poller.rs:466-483 (call site), crates/shelbi-orchestrator/src/workspace.rs:218-252 (dirty check, for context)

- **Category:** failure-scenario / assumption

- **Severity / Confidence:** medium / likely

- **Evidence:** on marker detection the poller immediately rebases the live
  worktree:

  ```rust
  // Auto-rebase the workspace's branch onto the project's default
  // branch before the column move. ...
  rebase_workspace_branch_before_review(project, workspace, machine, host, &task_id);
  ```

  The rebase implementation checks `git status --porcelain` once, then runs
  `git rebase` — a classic TOCTOU across two subprocesses. Meanwhile the task
  handoff contract explicitly tells the agent it may keep going after writing
  the marker (the dispatch preamble says: "Write the marker once; you can keep
  working in this pane and talk to the user afterward without affecting the
  handoff").

- **Failure scenario:** agent writes the marker, then continues (user asks a
  follow-up; agent starts editing/committing). Within the next poll tick
  (≤ `workspace_poll_interval_secs`) the poller runs `git rebase` in that
  worktree. If the agent dirtied files between the clean check and the rebase,
  the rebase fails mid-flight and is aborted (recoverable but noisy); if the
  agent commits *during* the rebase, git state under the agent changes
  arbitrarily — the agent's shell is now sitting on a rewritten HEAD it doesn't
  know about, and its next `git` operation can conflict with or clobber the
  rewritten branch. There is no coordination with the pane.

- **Recommendation:** at minimum, re-check dirtiness immediately before the
  rebase inside a single remote invocation (`git status --porcelain && git rebase …` under one shell), and document that post-marker work in the worktree
  is unsupported. Better: perform the rebase in a hub-side clone/fetch of the
  branch, or defer it to the review checkout (`start_review`) where nothing else
  owns the tree.

- **Effort:** M

## F7: A hung SSH command permanently and silently stops polling for that workspace

- **Where:** crates/shelbi-tui/src/poller.rs:113-136 (respawn gate), poller.rs:179-201 (poll loop); crates/shelbi-ssh/src/lib.rs:46-58 (options, for context)

- **Category:** failure-scenario

- **Severity / Confidence:** medium / likely

- **Evidence:** `shelbi-ssh` sets `ConnectTimeout=5` and `BatchMode=yes` but no
  `ServerAliveInterval`/command timeout — `run()` is a plain `cmd.output()`
  with unbounded wait. The supervisor only replaces threads that have *exited*:

  ```rust
  if spawned.get(&workspace.name).is_some_and(|h| h.is_finished()) {
      if let Some(h) = spawned.remove(&workspace.name) { let _ = h.join(); }
  }
  if spawned.contains_key(&workspace.name) {
      continue;
  }
  ```

  A thread blocked inside `pane_title`/`read_review_marker` never finishes, so
  it holds its slot in `spawned` forever; the module doc's own caveat
  (poller.rs:154-156) concedes stuck threads are simply abandoned at shutdown.

- **Failure scenario:** network drops after the TCP connect (or a Tailscale-SSH
  web-auth interception, which the shelbi-ssh docs note ignores BatchMode). The
  established-but-dead ssh session blocks `cmd.output()` indefinitely — TCP with
  no keepalive can hang for hours. That workspace's marker checks stop; a
  finished task sits in-progress with no review handoff; nothing in the UI
  distinguishes "stopped polling" from "no news" (see F9 — `last_seen` is
  recorded but never surfaced).

- **Recommendation:** add `-o ServerAliveInterval=5 -o ServerAliveCountMax=2` to
  the SSH options (bounds a dead session to \~10 s), or wrap poll subprocesses in
  a timeout. Optionally have the supervisor track a per-thread "last completed
  tick" heartbeat and log/surface a warning when one stalls.

- **Effort:** S–M

## F8: Workspace status and events are keyed by bare workspace name — projects collide

- **Where:** crates/shelbi-tui/src/poller.rs:378 (`save_workspace_status`), 382-388 (`append_workspace_event`); crates/shelbi-tui/src/app.rs:708 (`load_workspace_status`); crates/shelbi-state/src/workspace\_status.rs:131-134 (path, for context)

- **Category:** assumption

- **Severity / Confidence:** medium / certain

- **Evidence:** the status file path is global, not project-scoped:

  ```rust
  /// `~/.shelbi/workspaces/<name>/status.yaml`.
  pub fn workspace_status_path(workspace: &str) -> Result<PathBuf> {
      Ok(workspaces_dir()?.join(workspace).join("status.yaml"))
  }
  ```

  and event lines carry no project: `{ts} workspace={workspace} {prev} -> {new}`.
  Meanwhile workspace names are conventionally the phonetic alphabet
  (activity.rs:60-68 hard-codes avatars for `alpha`…`foxtrot`), so *every*
  project tends to have a workspace named `alpha`.

- **Failure scenario:** two projects open in two tmux sessions, each with its
  own sidebar poller. Project A's `alpha` transitions to `working`; project B's
  poller (same 5 s cadence) observes its own `alpha` as `awaiting_input` and
  overwrites the shared `status.yaml`; each poller's private `last_known`
  diverges from disk and both emit transition events against the same
  `workspace=alpha` line. The activity feed — which parses the single global
  `events.log` with no project filter for task/workspace events — interleaves
  both projects' activity, and `derive_workspace_badge` (app.rs:708) shows
  project B's state on project A's sidebar row.

- **Recommendation:** scope the status path and event lines by project
  (`~/.shelbi/workspaces/<project>/<name>/status.yaml`,
  `workspace=<project>/<name>` or a `project=` field), and filter the activity
  feed's task/workspace events by `app.project_name`.

- **Effort:** M

## F9: Dead workspace pane leaves the sidebar badge stuck on "Working"

- **Where:** crates/shelbi-tui/src/poller.rs:334-345 (early returns), crates/shelbi-tui/src/app.rs:697-718 (`derive_workspace_badge`)

- **Category:** failure-scenario

- **Severity / Confidence:** medium / likely

- **Evidence:** when the pane dies or the title stops carrying a marker, the
  poller stops updating the status file entirely:

  ```rust
  if !shelbi_orchestrator::workspace::workspace_pane_alive(&host, &addr).unwrap_or(false) {
      return;
  }
  let title = match shelbi_tmux::pane_title(&host, &addr) {
      Ok(t) => t,
      Err(_) => return,
  };
  ```

  `status.yaml` keeps its last state (e.g. `working`) and a stale `last_seen`.
  The badge derivation reads only the state, never freshness:

  ```rust
  match load_workspace_status(workspace_name).ok().flatten() {
      Some(s) => match s.state {
          WorkspaceState::Working => WorkspaceBadge::Working,
  ```

  Note the module docs advertise `last_seen` precisely "so the UI can tell
  stale from fresh observations" (poller.rs:27-28) — no UI in scope consumes it.

- **Failure scenario:** claude crashes or the workspace's tmux server dies
  mid-task. The task stays in-progress, `status.yaml` says `working`, and the
  sidebar shows a green ⏵ "Working" badge indefinitely. The user has no signal
  that the workspace needs a restart; the dead-pane condition is only
  discoverable by manually attaching.

- **Recommendation:** in `derive_workspace_badge`, treat a `last_seen` older
  than a few poll intervals as unknown/stale (distinct glyph or dim the badge);
  alternatively have `poll_one` write a `stale`/`unreachable` state when the
  pane-alive check fails for a workspace with an assigned task.

- **Effort:** S–M

## F10: Auto-refresh can restructure rows between draw and activate

- **Where:** crates/shelbi-tui/src/app.rs:362-386 (`refresh`/`maybe_refresh`), 428-439 (`activate_selection`); crates/shelbi-tui/src/handlers/sidebar.rs:29-48

- **Category:** bug (race)

- **Severity / Confidence:** medium / likely

- **Evidence:** the selection is a raw index into a row list rebuilt from disk
  every 750 ms:

  ```rust
  pub fn maybe_refresh(&mut self) -> Result<()> {
      if self.last_refresh.elapsed() >= Duration::from_millis(750) { self.refresh()?; }
  ```

  ```rust
  pub fn activate_selection(&mut self) {
      let Some(row) = self.rows().get(self.sidebar_index).cloned() else { return; };
  ```

  The loop order is `maybe_refresh` → `draw` → `poll(200ms)` → key dispatch, so
  a refresh between the frame the user is looking at and their Enter press
  re-derives `rows()` from changed disk state while `sidebar_index` stays put.
  Nothing re-anchors the selection to the row identity (the review view does
  exactly this — review\.rs:77-88 re-finds the previous task id — but the sidebar
  doesn't).

- **Failure scenario:** the user arrows onto workspace `bravo` and presses
  Enter just as a task lands in review. The refresh inserts the
  `Ready for Review` section (a `Blank` + `Section` + N `Review` rows) above
  the spawned-agents block or shifts workspace rows; index now points at a
  different row — Enter focuses the wrong workspace or starts a review checkout
  the user never asked for (a mutating git operation).

- **Recommendation:** remember the selected row's identity (View) across
  refresh and re-locate it, mirroring `ReviewApp::refresh`; or defer structural
  refresh while a key event is pending in the same tick.

- **Effort:** S

## F11: Review checkout / view focus runs synchronously in the key handler

- **Where:** crates/shelbi-tui/src/app.rs:461-498 (`activate_view`/`start_review`), crates/shelbi-tui/src/review\.rs:160-189

- **Category:** best-practice / failure-scenario

- **Severity / Confidence:** medium / certain

- **Evidence:**

  ```rust
  View::ReviewTask(id) => match self.start_review(id) {
      Ok(focus_target) => { ... }
  ```

  `start_review` → `shelbi_orchestrator::review::start_review_by_id` performs
  project load, branch resolution, git fetch/checkout into the review work dir,
  and tmux orchestration — potentially over SSH — all inside the event loop,
  between two draws. The same shape exists in `ReviewApp::activate_selection`
  (review\.rs:166-172). The status line ("▶ reviewing …" / error) is only set
  after the whole operation completes.

- **Failure scenario:** Enter on a review task against a slow remote or a large
  fetch freezes the entire sidebar/review pane for seconds to minutes: no
  redraw, no spinner, keys queue up (and are then replayed against the
  post-operation state, compounding F10). If the SSH hangs (see F7's transport
  gap), the UI hangs with it, and the pane looks crashed.

- **Recommendation:** run activation work on a worker thread with a
  "starting review…" status painted immediately, polling for completion in the
  loop; or at minimum draw a status frame before invoking the blocking call and
  put a timeout on the transport.

- **Effort:** M

## F12: Review marker cleared — handoff dropped — on any `load_task` error

- **Where:** crates/shelbi-tui/src/poller.rs:514-524

- **Category:** hardening / failure-scenario

- **Severity / Confidence:** medium / likely

- **Evidence:**

  ```rust
  Err(e) => {
      tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "review marker names unloadable task; clearing");
  }
  }
  if let Err(e) = shelbi_orchestrator::workspace::clear_review_marker(host, &marker) {
  ```

  The `Err` arm falls through to unconditional marker clearing. `load_task` can
  fail for reasons that are transient and unrelated to the marker being stale:
  `EMFILE`/`EACCES`, a momentarily unavailable `SHELBI_HOME` (network home
  dirs), or a YAML parse error introduced by a concurrent hand-edit. Contrast
  with the `move_task` failure a few lines up, which deliberately returns
  *without* clearing "so we retry on the next tick" (poller.rs:498-502).

- **Failure scenario:** hub is briefly under fd pressure when the poll tick
  fires; `fs::read_to_string` fails; the marker — the workspace's only handoff
  signal — is deleted. The task sits in-progress forever, the agent believes it
  handed off, and nothing retries. Recovery requires a human noticing and moving
  the task manually.

- **Recommendation:** clear the marker only on the two genuinely-stale cases
  (task file NotFound; task loadable but not in-progress for this workspace).
  On other errors, log and leave the marker for the next tick, matching the
  `move_task` failure policy.

- **Effort:** S

## F13: Marker-supplied task id is unsanitized — path traversal into `task_path`

- **Where:** crates/shelbi-tui/src/poller.rs:451-465; crates/shelbi-state/src/lib.rs:1097-1099 (`task_path`, for context)

- **Category:** hardening

- **Severity / Confidence:** medium / certain (traversal); impact bounded

- **Evidence:** the poller takes the marker content verbatim from a file the
  workspace (a remote, semi-trusted agent) writes:

  ```rust
  let task_id = match shelbi_orchestrator::workspace::read_review_marker(host, &marker) {
      Ok(Some(id)) => id,
  ...
  let task_file = shelbi_state::load_task(&project.name, &task_id);
  ```

  `read_review_marker` only trims whitespace, and `task_path` does a raw join:

  ```rust
  pub fn task_path(project: &str, id: &str) -> Result<PathBuf> {
      Ok(tasks_dir(project)?.join(format!("{id}.md")))
  }
  ```

  A marker containing `../../other-project/tasks/some-task` resolves outside
  this project's tasks dir. If the traversed target parses as a valid task file
  that is `InProgress` and `assigned_to` this workspace name, the hub will
  rebase, `move_task` (which **writes** the traversed path via `save_task`),
  and emit events for a task in a different project — from a signal written by
  a workspace agent. The `.md` suffix and the frontmatter/assignment checks
  bound the blast radius, but the trust boundary (remote workspace → hub
  filesystem paths) is crossed with zero validation. The id is also interpolated
  into `events.log`/tracing lines, though interior whitespace can't survive
  the `split`-based event grammar as a forged record.

- **Failure scenario:** a prompt-injected or buggy agent writes a crafted
  marker; hub manipulates another project's task board (column/priority
  rewrite via `move_task`) or, more mundanely, a corrupted marker containing a
  path fragment makes the poller stat and log confusing paths every 5 s.

- **Recommendation:** validate the marker id before use — accept only
  `[A-Za-z0-9._-]+` (reject `/`, `\`, leading `.`), or canonicalize
  `task_path(...)` and require it to remain under `tasks_dir(project)`.

- **Effort:** S

## F14: `ActivityApp.status_line` is written but never rendered

- **Where:** crates/shelbi-tui/src/activity.rs:257, 347-351, 421-424, 455-458; renderer at 760-777

- **Category:** bug / simplification

- **Severity / Confidence:** low / certain

- **Evidence:** `status_line` is assigned on zen toggles and refresh errors:

  ```rust
  self.status_line = format!("zen {label}");
  ...
  self.status_line = format!("read events.log: {e}");
  ```

  but `render_full` paints only title, pills, body, and footer — no widget in
  activity.rs reads `app.status_line` (grep confirms: assignments and tests
  only). The user pressing the zen chord in the activity view gets zero visual
  feedback, and events.log read failures are completely invisible (the feed
  just silently stops updating).

- **Failure scenario:** `events.log` becomes unreadable; every 500 ms refresh
  sets an error string nobody displays; the user stares at a frozen feed with
  no explanation.

- **Recommendation:** render `status_line` in the footer row (replacing or
  prefixing the key hints when non-empty), as the sidebar and review views do —
  or delete the field and its writers if the feedback is genuinely unwanted.

- **Effort:** S

## F15: `auto_scroll` is dead state; scrolled-back readers get yanked by new events

- **Where:** crates/shelbi-tui/src/activity.rs:259-263, 477-516

- **Category:** bug / simplification

- **Severity / Confidence:** low / certain

- **Evidence:** the field documents an anchoring contract:

  ```rust
  /// True until the user scrolls back manually — once they do, new
  /// events appearing at the top no longer chase the cursor.
  pub auto_scroll: bool,
  ```

  Every scroll method writes it, but no code outside `#[cfg(test)]` ever
  *reads* it — not `render_body`, not `refresh`. Since `scroll` is measured in
  lines from the top and new events are inserted at the top, a reader parked at
  `scroll=40` sees the content under their viewport shift down by the height of
  every newly arrived event — precisely the behavior the field claims to
  prevent.

- **Failure scenario:** user scrolls back to read yesterday's activity on a
  busy board; each incoming event (≤500 ms cadence) shoves the text they're
  reading downward.

- **Recommendation:** implement the anchor — when `!auto_scroll`, increase
  `scroll` by the number of lines newly prepended (or anchor by remembering the
  top-visible event index) — or delete the field and its eleven write sites.

- **Effort:** S–M

## F16: `total_lines` / `scroll` are `u16` — long feeds truncate at 65 535 lines

- **Where:** crates/shelbi-tui/src/activity.rs:851, 265-270, 512-516

- **Category:** bug

- **Severity / Confidence:** low / certain

- **Evidence:**

  ```rust
  app.total_lines = lines.len() as u16;
  ```

  An unchecked `as u16` wraps: at 65 536 rendered lines `total_lines` becomes 0,
  so `max_scroll = total_lines.saturating_sub(area.height)` clamps `scroll` to 0
  and `scroll_end` jumps to garbage. A feed line-count of 65 k is \~16 k events
  (each event emits 2-4 lines including spacers) — reachable on a long-lived
  project, especially combined with F3's duplicates. (`Paragraph::scroll` takes
  `u16`, so a real fix must cap what gets *built*, which F2's viewport-slicing
  recommendation also delivers.)

- **Failure scenario:** history crosses the threshold; End/scroll-up behavior
  becomes erratic (snaps to top; older events unreachable).

- **Recommendation:** saturate instead of `as` (`.min(u16::MAX as usize) as u16`) as a stopgap; properly fixed by building only the viewport window
  (F2).

- **Effort:** S

## F17: `review.rs`'s `run_tmux` inherits stdio — duplicate of `app.rs` helper minus the fix

- **Where:** crates/shelbi-tui/src/review\.rs:192-202; compare crates/shelbi-tui/src/app.rs:758-771

- **Category:** bug / simplification

- **Severity / Confidence:** low / certain

- **Evidence:** app.rs nulls the child's stdio:

  ```rust
  std::process::Command::new("tmux")
      .args(args)
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
  ```

  review\.rs's copy of the same helper does not:

  ```rust
  std::process::Command::new("tmux")
      .args(args)
      .status()
  ```

  `Command::status` inherits the parent's stdout/stderr — the ratatui alt
  screen. `tmux select-window` failure messages (`can't find window: …`) print
  straight into the raw-mode pane, splicing text across the rendered UI until
  the next full redraw — the same corruption class the keymap-diagnostics work
  in lib.rs:308-315 documents at length.

- **Failure scenario:** Enter on a review task whose window was killed; tmux
  writes an error line into the review pane's buffer mid-frame.

- **Recommendation:** delete review\.rs's copy and share the app.rs helper (one
  `pub(crate) fn run_tmux` in a util module).

- **Effort:** S

## F18: `ReviewApp` body scroll has no bottom clamp

- **Where:** crates/shelbi-tui/src/review\.rs:141-151

- **Category:** bug (cleanliness)

- **Severity / Confidence:** low / certain

- **Evidence:**

  ```rust
  pub fn scroll_body_down(&mut self) {
      self.body_scroll = self.body_scroll.saturating_add(1);
  }
  ```

  Unlike the activity view (which clamps against `total_lines` each frame,
  activity.rs:854-858), nothing bounds `body_scroll` to the rendered body
  height; `Paragraph::scroll` happily scrolls past the end.

- **Failure scenario:** user holds `j`/PageDown; the detail panel goes blank
  and stays blank for as many keypresses as it took to get there — feels like
  the pane died.

- **Recommendation:** record the wrapped line count at render time (as the
  activity view does with `total_lines`) and clamp in the scroll methods or the
  renderer.

- **Effort:** S

## F19: `run_activity` skips the keymap-diagnostics contract every other view honors

- **Where:** crates/shelbi-tui/src/lib.rs:277-286; crates/shelbi-tui/src/activity.rs:294-299

- **Category:** best-practice

- **Severity / Confidence:** low / certain

- **Evidence:** `run_sidebar`, `run_tasks`, and `run_review` all load keymaps
  *before* the alt-screen swap and route diagnostics through
  `log_keymap_diagnostics` (lib.rs:182-183, 230-231, 257-258). `run_activity`
  does neither:

  ```rust
  pub fn run_activity(project_name: &str) -> Result<()> {
      let mut term = setup_terminal().context("setting up terminal")?;
      let mut app = ActivityApp::new(project_name);
  ```

  `ActivityApp::new` loads keymaps itself — after the terminal swap — and drops
  the diagnostics on the floor (`let (keymaps, _diags) = load_keymaps(...)`,
  activity.rs:299).

- **Failure scenario:** a user with a broken `keys.yml` gets warnings logged
  from three views but not the fourth; a keys.yml error that only affects the
  activity mode is never recorded anywhere.

- **Recommendation:** mirror the other entry points — load + log diagnostics in
  `run_activity` before `setup_terminal`, pass the keymaps into `ActivityApp`
  (as `run_tasks`/`run_review` do via the `keymaps` field).

- **Effort:** S

## F20: Any `fs::metadata` error wipes the activity feed

- **Where:** crates/shelbi-tui/src/activity.rs:427-436

- **Category:** failure-scenario

- **Severity / Confidence:** low / likely

- **Evidence:**

  ```rust
  let meta = match fs::metadata(&path) {
      Ok(m) => m,
      Err(_) => {
          // No log file yet — empty feed, no error.
          self.events.clear();
          self.log_offset = 0;
  ```

  The comment assumes NotFound, but the arm matches *every* error kind. A
  transient `EACCES`/`EMFILE`/`EINTR` clears the parsed history and resets the
  offset; the next successful tick re-reads the whole file (masking the blip
  but re-parsing everything, and re-triggering F3's duplication window at full
  file scale). Contrast with `events_log_modified_within` in the poller
  (poller.rs:297-312), which distinguishes NotFound from other errors.

- **Failure scenario:** brief fd exhaustion while other views hammer the disk;
  the feed flashes to "no activity yet", then rebuilds.

- **Recommendation:** match on `e.kind() == ErrorKind::NotFound` for the clear
  path; on other errors, keep state and surface the error (see F14).

- **Effort:** S

## F21: Smaller items (grouped)

- **Where / Evidence / Recommendations:**

  1. **`restore_terminal`** **masks the loop error** — lib.rs:213-216:
     `restore_terminal(&mut term).context("restoring terminal")?; result` — if
     restore fails after the loop already failed, the (usually more
     informative) loop error is discarded. Log the restore failure and return
     `result` instead. (best-practice)
  2. **`App::refresh`** **returns an infallible** **`Result`** **and silently degrades** —
     app.rs:362-379: every fallible load is `unwrap_or_default()`, so the
     `Result` return is decorative and a broken project YAML makes all
     workspaces silently vanish from the sidebar with no message. Either
     surface a status-line hint on load failure or change the signature to
     `()`. (best-practice)
  3. **No single-poller guard** — poller.rs:57-67: nothing prevents two sidebar
     processes for the same project from running two pollers; each keeps a
     private `last_known`, so both append transition lines and race the
     `status.yaml` writes (duplicate feed rows; benign but confusing).
     A pid/lock file under `~/.shelbi/workspaces/` would make the singleton
     assumption real. (assumption)
  4. **Dead code** — activity.rs:1731/1821: `indent_w` is computed and then
     `let _ = indent_w;` — delete both lines. (simplification)
  5. **Zen chooser has no escape/timeout for keyless sessions** —
     zen\_probe.rs:203-222: on a first run in an environment that delivers no
     key events, `chooser_loop` redraws forever at 5 Hz and the sidebar never
     starts. A one-shot deadline falling back to `ZenToggleChord::None` would
     bound it. Also, the probe overlay (zen\_probe.rs:194) is drawn once and not
     re-drawn on resize during the 3 s wait. (failure-scenario, speculative)

- **Category:** simplification / best-practice

- **Severity / Confidence:** low / certain (item 5: speculative)

- **Effort:** S each
