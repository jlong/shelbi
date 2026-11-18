# Implicit state machines: `tui` slice findings

**Slice:** `crates/shelbi-tui` in full: all 94 `.rs` files (~94K lines). That covers `shell/` (mod.rs, settings/, session, review, workspace, overlays, diff_view, changelog, keybindings, onboarding, ...), `overlay/`, `launch/`, `prereqs/`, `kanban.rs`, `app.rs`, `handlers/` and every top-level module. Tests were read as evidence only. Surveyed 2026-11-18 on `main` at e8145b3f.

**Findings:** 3 high, 12 medium, 11 low, plus a Minor list. The dialog and form machines in this crate are mostly explicit and live in `shelbi-app`. The risk sits in the TUI glue around them: async results (`Option<Receiver>` slots, detached threads) that land without checking what the user did in the meantime, and modal/focus precedence encoded as `if` order separately in keys, mouse, paste and draw.

**Well encoded, no action:** `SessionManager::Slot` (with the shared `RestartController`), Settings ticketed loads, `ProjectWorkers` retirement, `QuitAction`, and the closed-history `run` generation.

**Verification:** the high findings and most medium ones were checked against source. Line numbers are from the commit above.

---

## HIGH

### H1. Main-area open lifecycle (review / workspace interface opening)
- **Entity:** `ShellState`'s main area (`crates/shelbi-tui/src/shell/mod.rs`).
- **Encoding:** 1, 5, 7, 8. The `MainView` enum exists (L1004), but about ten side fields carry the rest:
  - `review` (L1414), `workspace` (L1417)
  - `review_opening` (L1442), `workspace_opening` (L1423)
  - `review_open_pending` (L1453), `content_on_open` (L1447)
  - `review_rx` (L1426), `workspace_rx` (L1420)
- **States:**
  - Idle on Session or Native
  - Opening review T (`review_opening=Some`, view unchanged)
  - Picker raised (overlay)
  - Load pending (`review_open_pending=Some(T)`, waiting for the daemon `ReviewOpened` layout event)
  - Review open (`main_view=Review(T)` with `review=Some`)
  - Opening workspace W
  - Workspace open
  - Content ensuring (an ensure job on the same rx)
  - **Representable but invalid:**
    - `review` and `workspace` both `Some`.
    - `main_view=Review` with `review=None`. The draw falls back to "opening review…" (L6828).
    - `main_view=Session|Native` with `*_opening=Some`.
    - `review_opening=Some` with `review_rx=None` (stranded).
    - `content_on_open=true` with nothing opening.
- **Transitions:**
  - `show()`: Session arm L2325-2327, Native arm L2350-2353. These clear `review`/`workspace` but not `*_opening`, `*_rx` or `review_open_pending`.
  - `switch_project` L2577-2588 is the only full reset.
  - `apply_restored_view` L2634-2640.
  - Review:
    - `begin_review` L4858/4868/4871
    - `on_review_resolved` L4886/4897
    - `build_review_interface` L4911-4934
    - `start_review_load` L4976/4979, `on_review_load_done` L4998, `on_review_opened` L5009
    - `poll_review` L5023/5029
    - ensure-content L5073, Back L5153, `close_review` L5210, `leave_ended_review` L5263
    - reconcile close L5472, layout `ReviewClosed` L5487
  - Workspace: `begin_workspace` L5532-5544, `poll_workspace` L5576/5582, `on_workspace_opened` L5604, `build_workspace_interface` L5630/5633, ensure L5687, Back L5753, `close_workspace` L5792.
  - The draw takes the interfaces out and puts them back at L6683-6685 and L6939-6940.
- **Risk: high.**
  1. **Navigating away does not cancel an open in flight.**
     - The field docs say a late resolve is dropped "after the user navigated away". But `on_review_resolved` only checks `review_opening.take()` (L4886), and `show()` never clears it.
     - Repro: click a review row (slow remote resolve), then click Issues. When the resolve lands, `build_review_interface` sets `main_view=Review` and `focus_main()` (L4911-4912) and yanks the view back.
  2. **Both interfaces can be live at once.**
     - `build_review_interface` never clears `workspace`, and `build_workspace_interface` never clears `review`.
     - The draw paints every interface that is `Some`, regardless of `main_view` (L6863, L6872), with the workspace last. So the user sees the workspace while keys and mouse go to the review (`handle_main_key` L5835, `handle_mouse` L6200).
     - Reachable by clicking a review row and then a workspace row before both resolves land, or via `ReviewOpened` → `on_review_opened` → `begin_review` while a workspace is open.
  3. **One channel carries two job kinds.**
     - `review_rx` serves both the open resolve and the content ensure, and `workspace_rx` does the same.
     - Pressing Edit in review A (L5073) while B is resolving replaces `review_rx`. B's `Resolved` is dropped and `review_opening=Some(B)` is stranded. `want_content_on_open` (L4835) then only sets `content_on_open`.
     - A worker panic (Disconnected, L5023/L5576) strands `*_opening` the same way.
  4. **`review_open_pending` waits on a daemon event with no timeout or cancel.** If `ReviewOpened` is missed (for example on a subscriber reconnect), a later `ReviewOpened(T)` steals the view whenever it arrives.
  - **No tests** cover navigating away during an open or a double open. `opening_a_session_drops_an_open_review` (~L9289) only covers an already-built interface.
- **Recommendation: redesign.**
  - Use one `MainArea` enum: `Session | Native(View) | Opening{kind, id, ticket} | Review(Box<ReviewInterface>) | Workspace(Box<WorkspaceInterface>)`.
  - Tag every async open with a generation ticket.
  - Route every path through one `set_main_area()`: show, the builders, Back, close, reconcile, switch.
  - Give content-ensure its own channel.

### H2. Optimistic kanban card-move chain (`PendingMove`)
- **Entity:** one optimistic move per task id: `KanbanApp.pending_moves: HashMap<String, PendingMove>` (`crates/shelbi-tui/src/kanban.rs:162`, struct at :283).
- **Encoding:** 1 (`rx: Option<Receiver>` plus `queue: VecDeque`), 4 (overlays a status mirrored from disk, the tracker or the daemon index), 5, 8.
- **States:**
  - InFlight (`rx` Some, queue empty)
  - InFlight+Queued
  - Settled (`rx` None, queue empty, waiting for a refresh to agree)
  - Absent
  - **Representable but invalid:** `rx` None with a non-empty queue. It occurs only transiently, inside `start_next_hop_if_idle`.
  - **Missing:** no Abandoned or Expired state. `PendingMove` has no `Instant`.
- **Transitions:**
  - Create: `move_card` inserts at kanban.rs:1657.
  - Enqueue at :1644-1645, then kick at :1646.
  - `start_next_hop_if_idle` pops at :1701 and sets `rx` at :1706.
  - `poll_pending_moves`: hop landed at :1759-1760; hop failed, remove and roll back at :1789-1791.
  - `reconcile_pending_moves` resolves at :1832 and overlays every survivor onto `self.tasks` at :1837.
  - The whole map is dropped by `mem::replace(&mut self.kanban, …)` on project switch (`shell/mod.rs:2563`).
  - Readers: the reorder lock (kanban.rs:1904), and the shell's `has_pending_moves` redraw gate (`shell/mod.rs:705`).
- **Risk: high.**
  1. **A Settled entry can stick forever.**
     - It resolves only when the refreshed board's `resolved_status_id == shown_status_id`, or when the card leaves view (:1820-1828).
     - If another writer moves the card after our hop lands (an agent, the orchestrator, the desktop, a workflow edge action), the board never agrees.
     - Then the card is pinned to a stale column on every refresh (:1837), and reorder is refused ("move in progress — reorder held", :1904).
     - `has_pending_moves()` also stays true, so the shell redraws every tick (`shell/mod.rs:705-709`).
  2. **Premature resolve on a cold or failed read.**
     - `apply_board_data` rebuilds `self.tasks` only on a warm read (kanban.rs:874-880), but always calls `reconcile_pending_moves` (:885).
     - On a cold read, `self.tasks` still holds our own overlay, so a Settled entry "agrees" with itself and is dropped.
     - The next warm read from a lagging index then bounces the card back: the exact bug the overlay exists to prevent.
  3. **Project switch and quit lose queued hops and errors.**
     - On switch, queued hops (`L L L` then switch) are never persisted. A failing in-flight write's error never reaches `fail_status`.
     - Persist threads are detached (`std::thread::spawn`, kanban.rs:3176) and the quit path has no drain (`shell/mod.rs:2653`).
     - With `dev.daemon_mutations` off, the gated merge, branch cut and `move_status` run in-process. Exit can cut a merge-edge move off part-way (suspected, not reproduced).
  4. **Persister asymmetry.** `ExecutorMovePersister` maps success to `PersistedMove::default()` (kanban.rs:248), so the daemon route never reports `merged` or `cleanup_error`.
  - **Past bugs pinned by tests:**
    - `moving_a_settled_card_again_persists_the_new_hop` (:5297)
    - `settled_move_overlays_until_the_board_agrees_then_resolves` (:5201)
    - `refresh_during_in_flight_move_does_not_bounce_the_card` (:5152)
- **Recommendation: make explicit.**
  - Use `enum MoveState { InFlight{target, rx, queue}, Settled{since: Instant, confirmed} }`.
  - Add a settle timeout, or resolve when the board shows any status other than the pre-move one.
  - Reconcile only after a warm read.
  - Drain or flush on switch and quit.
  - Consider moving it into `shelbi-app`. The desktop has no optimistic overlay at all, so this is also a parity gap.

### H3. Settings unsaved-edits leave prompt and the save it starts
- **Entity:** `SettingsShell` (`crates/shelbi-tui/src/shell/settings.rs`), with these parts:
  - The shared `guard: LeaveGuard<Leave>` (`shelbi-app/src/settings.rs:370-450`).
  - `prompt_choice`, `save: Job<()>`, `prompt_properties: bool` (L494), `save_error`.
  - Four other sources of "saving": the Agents commit queue, the workflow editor `busy()`, the agent editor `busy()`, and the General form `saving()`.
- **Encoding:** 1, 5, 8. The "Saving" state is computed rather than stored: `prompt_saving()` (L1035) is `save.running() || (leaving_after_save && (agents saving || editor busy))`. Which kind of save is running is never stored.
- **States:**
  - Idle
  - Asking(leave)
  - Saving(leave, kind), where kind is General / Properties / Agents shared instructions / workflow editor / agent editor
  - Landed-ok → `go(leave)`
  - Landed-err (stays, with `save_error`)
  - **Representable but invalid:**
    - `prompt` and `after_save` both Some.
    - `after_save` Some with no save running.
    - `prompt_properties` true with no Properties save in flight.
- **Transitions:**
  - `guard.leave` at L1167, inside `request()` (L1156).
  - `guard.cancel` at L681 and L2087; `guard.discard` at L2081; `guard.save` at L2061.
  - `guard.forget_save` at L1030, L2066 and L2073.
  - `guard.saved`: L819/828 (`agent_landed`), L931/937 (`editor_landed`), L1067 (`saved`), L1529 (`agents_committed`).
  - `save.begin` at L1000/1006/1014; `save.complete` at L1045; `save.reset` at L1226.
  - `prompt_properties` is set at L1016 and taken at L1049.
  - From `shell/mod.rs`: the save starts at L3875 and lands at L3905; the palette SwitchProject calls `request()` at L4631.
- **Risk: high.**
  1. **The mouse isn't gated while the prompt's save runs.**
     - Keys check `prompt_saving()` (L1837). `handle_mouse` (L2094) checks only `guard.prompt()` (L2131), and only `render_layers` (L2296) and the prompt render (L2639) consult `prompt_saving()`.
     - While "Saving…" shows, a sidebar `Hit::Stop` click goes through `activate_stop` → `request()`, which refuses only when `editor_busy()` (L1157).
     - After `commit_edit()` the General form is clean, so `go()` runs immediately.
     - When the save lands, `guard.saved(true)` (L1067) runs a second, stale `go(original leave)`. Settings closes, or the project switches, after the user already went elsewhere.
     - The palette SwitchProject (`mod.rs:4631`) has the same hole.
  2. **The guard outlives a project change.**
     - `project_changed()` (L1213-1230) resets `save` but not `guard.after_save` or `prompt_properties`.
     - The old landing is then dropped at L1045, before `guard.saved`, so `after_save` stays Some. It survives close and reopen, since `open()` only cancels the prompt (L681).
     - A later unrelated editor `Landed::Wrote` or Agents commit reads `settings_save = true` (L814, L926, L1526) and runs `go()` on the stale leave.
     - A prompt save for shared instructions whose Agents page was retired (L1502-1517) never resolves the guard either.
  3. **Errors go to the wrong form.** `Err(e) if self.form.saving()` (L1064) sends the error to the General form. A Properties prompt-save failure that lands while a theme or tool write is in flight resets the General control and reports the wrong failure.
  4. **Paste reaches the editors under the prompt.** `paste()` (L1121) forwards text to the workflow and agent editors before any `guard.prompt()` or `prompt_saving()` check, into a draft that `go(Leave::Page)` then drops.
  - **No test** covers clicks or paste during a prompt save.
- **Recommendation: make explicit.**
  - Store the prompt save as a TUI enum: `PromptSave { General(Ticket), Properties(Ticket), AgentsShared(id), WorkflowEditor, AgentEditor }`, held inside a Saving state.
  - Put every modal entry point (keys, mouse, paste, `request`) behind one `blocked()` predicate.
  - Reset it in `project_changed` and `open`.
  - Fix `LeaveGuard` in `shelbi-app` so `prompt` and `after_save` are one enum. The desktop likely shares the hole (cross-slice).

---

## MEDIUM

### M1. Review/workspace content-area view: flag soup plus stale async answers
- **Entity:**
  - `ReviewInterface` (`shell/review.rs:114-150`) and its near-copy `WorkspaceInterface` (`shell/workspace.rs:82-107`).
  - What the content area shows: the Chat/Agent session, the Editor session, the built-in View Diff, or an external View Diff wait.
- **Encoding:** 1, 5, 7, 8.
  - Review fields: `diff_active`, `builtin`, `diff: Option<DiffView>`, `diff_load: Option<DiffLoad>`, `diff_answer: Option<Receiver>`, `diff_drawn`, `retry_rect`, `focus`, `content_wanted`, plus the content `SessionManager` slot.
  - Workspace keeps its own `diff_showing` and `diff_task`.
- **States:** {Session(role), BuiltinDiff, DiffWait(wait/attached/drawn/failed)} × focus {Panel, Content} × `content_wanted`.
  - **Representable but invalid:**
    - `diff_active` with neither `builtin` nor `diff_load`.
    - `builtin` and `diff_load` both Some.
    - `diff_answer` without `diff_load`.
  - These are excluded only because `show_builtin_diff` (review.rs:241-253) and `show_diff_at` (:349-362) each clear the other's fields.
- **Transitions:**
  - review.rs `diff_active`: :251, :356, :374.
  - review.rs `focus`: :252, :624, :658, :671, :799.
  - review.rs `content_wanted`: :483, :625, :640, :656, :800.
  - workspace.rs `focus`/`diff_showing`: :187-188, :199-203 (`follow_task`), :209, :390, :404, :423, :517.
  - Async answers: `editor_ready` (review.rs:324-335); `WorkspaceJobMsg::ContentReady` (`shell/mod.rs:5585-5593`); the diff ensure thread (review.rs:380-393).
- **Risk: medium.**
  - **Stale answers win.**
    - `editor_ready` treats an answer as stale only when `diff_active`. So: Edit, then Chat before the daemon answers, then `Ok` → `show_role(Editor)` takes the pane back from Chat.
    - The only test is `a_stale_editor_answer_doesnt_take_the_pane_from_view_diff` (review.rs:1360).
    - The workspace `ContentReady(Ok)` calls `show_role` with no staleness check at all, and an `Err` forces `show_agent()` (mod.rs:5585-5593).
  - **A review session can come back after Close.**
    - `SessionManager::start_connect` (`shell/session.rs:197-227`) keeps the worker only as `_join`, so replacing the slot does not cancel it.
    - A worker carrying a relaunch step (session.rs:582-589) still sends `ReviewSessionOp::Ensure` through `ReviewRelauncher` (`shelbi-app/src/session/relaunch.rs:122`) after `close_review` (mod.rs:5195-5211) has closed the sessions.
    - Result: an editor or diff session re-created after Close (possible slot or port leak).
  - **Mouse bypasses the focus guards.**
    - A content mouse-down sets `focus = Content` with no `content_focusable()` check (review.rs:799, workspace.rs:517), contradicting the guard comments at review.rs:662-665.
    - In review it also runs before the merging check (review.rs:795-804), and `handle_content_key` (:700) has no merging gate.
    - So mid-merge a click moves focus to the content and keys reach the agent. That breaks the "inert while merging" contract that `competing_input_is_blocked_while_merging` and `focus_moves_are_inert_while_merging` (:1493, :1579) test only for keys.
  - **The two files have already diverged** (merging gate, `DiffLoad` wait). `follow_task` also lets the workspace's panel describe a different task than its diff (deliberate, workspace.rs:99-101).
- **Recommendation: make explicit.**
  - Use one shared `ContentView` enum (`Session(role) | BuiltinDiff | DiffWait(DiffLoad)`) plus a focus transition fn with the merging gate in one place. Move it to a shared module or into `shelbi-app`.
  - Tag each ensure or connect with a generation the interface checks before applying.
  - Give connect workers a cancel flag that is checked before the relaunch step.

### M2. Shell input-layer stack and async overlay landers
- **Entity:** which layer owns input in the shell.
  - Fields: `overlay: Option<ActiveOverlay>` (mod.rs:1570, enum at `shell/overlays.rs:83-138`), `acknowledgements` (L1578), `settings_return` (L1583), `onboarding` (L1411), `search_input`, the review/workspace panels, and `client.focus()` (shelbi-app).
- **Encoding:** 7, 8. `ActiveOverlay` is explicit, but it has about 38 direct `self.overlay =` writers, and the precedence is hand-coded in four places that must agree:
  - `handle_event` L2758-2797
  - `handle_key` L3001-3055
  - `handle_mouse` L6169-6230
  - `draw_frame` L6700-6930
- **Async writers:**
  - `open_review_picker` (L4950, from a resolve landing)
  - `show_issue_detail` (L2994, from `poll_changelog_issue`)
  - `new_task_landed` (L4325)
  - `leave_acknowledgements` (L3289)
- **Risk: medium.**
  - An async landing replaces whatever overlay is up. A review resolve that lands while the user types in New task, Contact Us, the palette or a reject reason throws that dialog away. `poll_changelog_issue` checks only `main_view_key` (L2938), not the overlay.
  - When the New task dialog was clobbered, `new_task_landed` falls back to `self.status = message` (L4319-4321). The message is `None` on `Err`, so a failed create is lost silently.
  - Async replies find their dialog by variant match only (`new_task_landed` L4319, `feedback_landed` L4356), so a reply lands on whichever dialog of that kind is open.
  - An overlay that lands during Acknowledgements takes input over it (L2758), and leaving Acknowledgements for About then overwrites the overlay (L3289).
- **Recommendation: make explicit.**
  - Async landers check `overlay.is_none()` or queue the result.
  - Tag dialog jobs with a dialog id.
  - Derive one `InputLayer` that keys, mouse, paste and draw all consult.

### M3. Review decisions vs project switch
- **Entity:** in-flight approve or reject per task.
  - `decisions: ReviewDecisions` (mod.rs:1430; type at `shelbi-app/src/review.rs:590`).
  - Also `reject_reasons` and `decision_tx`/`decision_rx`.
- **Encoding:** 1, 5, 8.
- **States:** none, running, failed (kept until taken), landed.
- **Transitions:**
  - `start_decision` → `decisions.begin` (L5226)
  - `switch_project` → `decisions.clear()` (L2582)
  - `on_decision_reply`: L5296 (the switched-away arm), L5298 (`landed`), L5310 (`decision_refused`)
  - `sync_review_decision` (L5343-5354) mirrors the state into `ReviewPanel.merging`.
- **Risk: medium.**
  - A switch clears the record, but the reply thread keeps running.
  - If the user switches away and back before the reply lands, `decisions.landed(&task)` is `None` (L5298). Then `close_review_sessions` is skipped, so the daemon is never told to close the review's sessions, and "approved" never shows.
  - Meanwhile the row's "Merging…" note is gone (L2189), and `refuse_second_decision` (L5252) allows a second decision on a task that is still merging.
  - An `Err` after a switch puts "Couldn't approve T…" on the new project's status line.
  - No test covers a switch during a decision.
- **Recommendation: make explicit.** Key decisions by `(project, task)` and filter by the current project instead of clearing on switch.

### M4. Single-slot shell background job and status-line ownership
- **Entity:** `job: Option<Receiver<Option<String>>>` (mod.rs:1654) plus `status: Option<String>` (L1633, about 44 writers).
- **Encoding:** 1, 5.
- **Transitions:**
  - `start_job` sets `job` (L4797) and `poll_job` clears it (L4806).
  - Every `spawn_job` / `spawn_mutation` / `spawn_quiet_job` call site replaces the slot: L4244, L4692-4704, L4755, L4764, L5121, L5180, L5203, L5710, L5769, L5785.
- **Risk: medium.**
  - Each spawn drops the previous receiver, so an earlier job's result is silently lost. That includes failures such as "move failed: …" and "review close failed: …".
  - `spawn_job` sets "working…" (L4775). If a quiet Zen job then replaces it and returns `None` (L4824-4826), "working…" stays until some other status write.
- **Recommendation: make explicit.** Use a job set (`Vec` of receivers, or one shared channel carrying job ids) so every outcome lands.

### M5. Activity feed: two refresh protocols on one struct (confirmed bug)
- **Entity:** `ActivityApp` (`crates/shelbi-tui/src/activity.rs:71-124`): `events`, `log_offset`, `task_cache`, `meta_cache`.
- **Encoding:** 5, 3. Two protocols share the fields: a standalone incremental tail and the shell's full snapshot. They are coordinated only by `log_offset = 0`.
- **States:**
  - Standalone tailing (`log_offset` = head)
  - Shell snapshot (`log_offset` = 0)
  - **Invalid but reachable:** a shell snapshot followed by an incremental `refresh()`.
- **Transitions:**
  - `refresh()` (activity.rs:257-289): reads from `log_offset`, clears only if `read.start < log_offset`, appends, then runs `resolve_task_meta()`.
  - `apply_activity_data` (:320-329) replaces events and sets `log_offset = 0`.
  - The shell routes keys to `handlers::activity::handle_activity_key` (`shell/mod.rs:6021`), where `ActivityAction::Refresh` (default `r`) calls `app.refresh()` (`handlers/activity.rs:27`).
- **Risk: medium.**
  - In the shell, `r` after any background apply reads the whole retained log from offset 0 and appends it to the snapshot window, which comes from `read_event_log_recent` (`shelbi-app/src/activity.rs:~855`). Feed rows are duplicated until the next background apply.
  - The same key runs `resolve_task_meta` synchronously on the UI thread with a `meta_cache` the shell never fills (cold title pass, possibly `gh`), and replaces `task_cache`.
  - The comment at activity.rs:318 ("re-reads cleanly") is wrong.
  - The standalone `__activity` entry point appears gone, so the incremental path is vestigial but reachable.
  - `auto_scroll` (activity.rs:337-369) is written but never read.
- **Recommendation: redesign (delete).**
  - Remove the standalone path: `refresh`, `maybe_refresh`, `log_offset`, `meta_cache`, `should_quit`.
  - Have `r` request the shell's activity refresher.

### M6. Launch screen busy state split across two owners
- **Entity:** the standalone launch screen (`crates/shelbi-tui/src/launch/`).
  - `LaunchScreen.job: Option<Receiver>` (screen.rs:148; `busy()` = `job.is_some()`, :201)
  - `LaunchApp.create_job: Option<Receiver>` (mod.rs:103)
  - `layer` (mod.rs:100) and `notice`.
- **Encoding:** 1, 5, 8.
- **States:**
  - Idle, Opening/Connecting (`screen.job`), Creating (`create_job`), Form or Connect layer.
  - **Representable but invalid:** Creating together with Opening; Creating with the AddProject form open again; two creates in flight.
- **Transitions:**
  - `screen.job`: screen.rs:358 / :373.
  - `create_job`: mod.rs:299 / :314.
  - `layer`: mod.rs:228, 234, 245, 249, 270, 284.
- **Risk: medium.**
  - Only a left mouse-down is blocked during a create (mod.rs:181). Keys go to `screen.handle_key`, whose guards (screen.rs:276 and :316 `press`) check only `screen.busy()`.
  - So while "Creating project…" shows, Enter on a project row starts `open_project`, and whichever job finishes first decides the exit.
  - Enter on Add project reopens the form. A second submit overwrites `create_job`, so the first project is created but never opened or reported.
  - Esc, q or Ctrl+C quit at any time (screen.rs:273-274). The process then exits mid-scaffold, killing the detached create thread.
  - On a create error the form is already gone (mod.rs:284), so the answers are lost.
  - The shell holds a second copy of this submit lifecycle (`shell/mod.rs:4367-4412`).
  - `launch/tests.rs:439` covers only the happy path.
- **Recommendation: make explicit.**
  - One `LaunchBusy { Opening, Connecting, Creating }` owned by `LaunchApp` and consulted by both input paths.
  - Keep the form open on failure (`Form::set_error` exists).
  - Share the submit lifecycle with the shell.

### M7. Job and reminder sheet "Canceling…" never clears on failure
- **Entity:**
  - The TUI `JobSheet.canceling: bool` (`crates/shelbi-tui/src/jobs.rs:101`).
  - The shared `shelbi_app::reminders::SheetState { canceling, cancel_error }` (`shelbi-app/src/reminders.rs:339-391`).
- **Encoding:** 1, 4, 8. "Canceled" is reached only by mirroring the job's or reminder's state on a later read.
- **States:** Idle, Canceling, CancelFailed(error).
- **Transitions:**
  - Jobs: `take_cancel` sets `canceling` (jobs.rs:185). It resets only when a new sheet opens (:157).
  - Reminders: `begin_cancel` (reminders.rs:116). `follow()` clears it when the row leaves pending.
  - The cancel itself runs on a detached thread: `shell/mod.rs:6064-6076` (jobs) and :6111-6122 (reminders).
- **Risk: medium.**
  - On error the shell only `tracing::warn!`s (mod.rs:6072, :6119). It never calls `SheetState::cancel_failed` and never resets the job sheet's flag.
  - The sheet then shows "● Canceling…" indefinitely (jobs.rs:316), and the cancel key is hidden and refused (:182, :321).
  - `take_cancel()` is evaluated inside the `if let` tuple even when there is no project (mod.rs:6066, :6113). That marks the sheet Canceling with nothing sent.
  - The desktop resets on error (`desktop/src/views/jobs.rs:387`, `views/reminders.rs:237`), so this is a parity gap. The jobs sheet state is also duplicated per client, while the reminders sheet state is shared.
- **Recommendation: make explicit.**
  - Add a shared `JobSheetState` (or a generic cancel machine) in `shelbi-app`.
  - Send the cancel result back to the UI thread.

### M8. Settings editor work tickets vs the shared `busy` flag
- **Entity:**
  - `WorkflowEditorPage` (`shell/settings/workflow_editor.rs:412-440`) and `AgentEditorPage` (`shell/settings/agent_editor.rs:326-352`): `state`, `load_error`, `load: Job`, `job: Job`, `pending: Option<(Ticket, Work)>`.
  - The shared state's `busy` (`shelbi-app/src/settings/agent_editor.rs:344, 965-979`).
- **Encoding:** 1, 5. Whether work is in flight is stored twice: the TUI ticket `Job`, and the shared `busy` that gates `begin_save`/`begin_job`.
- **States:**
  - Loading, LoadFailed, Ready, Ready+Busy, Pending (queued for the shell).
  - **Representable but invalid:** `busy` true with no job running.
- **Transitions:**
  - Agent editor: `load.begin` at :452; `job.begin` at :853/:928/:959; `pending` set at :453/:854/:929/:960 and taken at :445; completions at :969/:1008/:1031/:1050.
  - Workflow editor has the same shape (`Loaded` at :1323-1324).
  - Shell side: `mod.rs:3716-3737` and :3774-3791.
- **Risk: medium (unlikely but sticky).**
  - If the shell can't spawn the worker, it reports any work (Save, Duplicate, Delete) as `Loaded(Err)` under the job's ticket (mod.rs:3731, :3788).
  - `landed()` then tries `load.complete(ticket)`, which fails, so the result is dropped and the shared `busy` stays true.
  - After that, `editor_busy()` blocks every Settings leave (settings.rs:1157), and the editor is wedged until a project switch.
- **Recommendation: make explicit.**
  - Return a failure variant of the right kind.
  - Let the shared state own the in-flight flag; the TUI keeps only the ticket.

### M9. Keybindings page load/save sequencing
- **Entity:** `KeysPage` (`shell/keybindings.rs:135-153`) over the shared `KeybindingsState` (`saving: bool`, `shelbi-app/src/settings/keybindings.rs:179`).
- **Encoding:** 1, 5.
- **States:**
  - Idle, Loading, Saving, Saving+ReloadQueued (`reload_after_save`).
  - One shared `error: Option<String>` with 4 kinds of writer.
- **Transitions:**
  - `begin_load` (:250-259) refuses or queues a load during a save.
  - `loaded` (:262-288) and `saved` (:292-304) complete them.
  - `saving` is set and cleared in shelbi-app keybindings.rs:457 and :426.
- **Risk: medium-low.**
  - Starting a save does not supersede a load already running: `Job` checks only its own ticket (`shelbi-app/src/settings.rs:520-546`).
  - Sequence: a read that returns T0 is in flight; the user saves T1; the stale T0 lands; `loaded` sees `T0 != text()`, calls `load(T0)` and returns true. The shell now runs the old bindings while T1 is on disk.
  - `error` is written at :213, :270, :285, :301, :309 and :320. The last writer wins, so a no-op reload clears a save failure (:276).
- **Recommendation: make explicit.**
  - Have a save invalidate the load ticket (or reject `loaded` while saving).
  - Fold `reload_after_save` into the Job state.

### M10. Agents page shared-instructions editor mirrored across crates
- **Entity:**
  - `AgentsPage` (`shell/settings/agents.rs:161-183`): `editor: Option<Editor>`, `menu`, `pending_focus`, `opened`, `note`.
  - The shared `state.editing_shared` / `confirm` / `draft`.
  - `SettingsShell.retired` (settings.rs:482).
- **Encoding:** 1, 8. Whether the editor is open is stored twice (`editor` and `editing_shared`). `captures()` (:371-376) combines four sources across two crates.
- **Transitions:**
  - `editor`: set at :496, cleared at :320/:361.
  - `editing_shared`: :503, :362. `committed` (:316-337) reads it to decide whether to clear `editor`.
  - `menu`: :307, :480, :509, :525, :636, :821, :853.
  - Retire: settings.rs:1479-1487 and :1501-1517.
- **Risk: medium-low.**
  - Every writer keeps the two "editor open" fields in sync by hand.
  - A retired page's completion never touches the shell's guard (this feeds H3).
  - A→B→A builds a fresh page A whose load can land while the retired A's save is still writing. The new page isn't `saving()`, so the read is taken and can show stale data (:282-284).
- **Recommendation: document now.** Fold `editing_shared` into one owner later.

### M11. Startup phase inferred from the status-line text
- **Entity:**
  - `startup_rx` (mod.rs:1402) and `startup_phase` (L1406).
  - The check `status == STARTUP_STATUS` (const L426).
- **Encoding:** 2, 5.
- **Transitions:**
  - Writers: L504, L2615 (switch), L4470-4471 (`spawn_startup`), L4483-4499 (`poll_startup`), L4551 (onboarding Launch).
  - Readers: `status_text` (L2076) and `tick_startup_spinner` (L2084). Startup clears at L4506/L4517.
- **Risk: medium-low.**
  - "Startup running" is inferred from the status text, so any status write during startup hides the spinner.
  - `poll_create_job` calls `switch_project` (sets `STARTUP_STATUS`), then overwrites the status with "Created project…" (L4414-4415). The new project's startup shows no spinner, and the line is never cleared by startup.
  - If the startup thread fails to spawn, "starting orchestrator…" stays forever with `startup_phase=None`.
  - The stale-project guard is good (L4478-4483; test `poll_startup_drops_a_job_keyed_to_another_project`).
- **Recommendation: make explicit.** Drive the spinner from `startup_phase` and keep startup out of `status`.

### M12. Scrollback search prompt survives view changes
- **Entity:** `search_input: Option<String>` (mod.rs:1549).
- **Encoding:** 7.
- **Writers:** only in `handle_main_key` (L5914-5934, and `/` at L5980). There is no reset in `show`, `switch_project` or a focus move.
- **Risk: medium-low.**
  - Type `/` in a session's scrollback, then move to another session or switch project. The prompt stays.
  - It renders over every non-panel view, native views included (L6666, L6848), and hides the status line.
  - The next keystrokes in the new session go into the stale buffer instead of the agent, and Enter searches the new pane with the old query.
  - No tests.
- **Recommendation: make explicit.** Make it per-pane state, or clear it in `show`/`switch_project`.

---

## LOW

- **L1. Main-area session binding (`SessionManager`).** Well encoded.
  - Pieces: `Slot` (`shell/session.rs:56-94`), the shared `RestartController` (`shelbi-app/src/session/restart.rs:43-131`), and `TerminalPane.exited` (terminal_view.rs:51/116).
  - States: Empty, Connecting{restart_attempt}, Live, Idle, Failed, AwaitingRestart, GaveUp.
  - Writers: `show` (:172-191), `start_connect` (:221), `poll_connect` (:265-307), `act_on_death` (:379-425), `reconnect` (:477-484), `retry_if_stale` (:433-440). External callers at mod.rs:2718 and :4515.
  - Caveat: `reconnect()` clears to Empty, so `show()` treats it as a fresh bind. `RestartController::begin(false)` then wipes the crash budget, and a GaveUp pane would silently re-arm. Benign today, since `retry_if_stale` only fires on Idle/Failed.
  - Many past bugs are pinned by tests: `rt-review-screen-hangs-on-connecting`, `rt-auto-restart-killed-panes`, `rt-review-session-alive-but-not-listening`, and others.
  - **Document** the reset, and consider a `Slot::Closing` variant in place of `deliberate_close`.
- **L2. Settings overlay ladder.**
  - The pieces: `editor` and `agent_editor` (settings.rs:498-501, "never both"), plus each page's modal (`workspaces.layer`, `workflows.modal`, `tools.layer`, the `agents.menu`/`confirm`/`draft` group, `keys` capture, `form` draft, `properties` typing).
  - Exclusion and precedence are enforced only by `if` order, and that order differs between `handle_key` (L1827-1890), `handle_mouse` (L2094-2150), `render_layers` (L2244-2296) and `paste` (L1121-1150).
  - Writers: `editor` at :764, :796, :896, :910, :983, :1186, :1215; `agent_editor` at :763, :797, :849, :864, :873, :877, :901, :913, :1187, :1216. `open()` (L677-692) doesn't reset the agents menu.
  - Exclusion holds today because each writer clears the other editor.
  - **Make explicit:** `enum Overlay { None, Workflow, Agent }` plus one `top_layer()`.
- **L3. Board view prefs: memory + `state.json`, clobbered by in-flight reads.**
  - `apply_filter` (kanban.rs:1161-1164) and `toggle_column` (:596-607) write memory and then disk.
  - Every off-thread board read overwrites both in `BoardInput::apply_open` (`shelbi-app/src/board.rs:~1050`).
  - A read that started before the write reverts the chip or column for one cycle, and a failed persist is silently reverted on the next read.
  - **Document**, or generation-tag the reads.
- **L4. Properties page typing.**
  - `PropertiesPage` (`settings/properties.rs:114-125`) has `editing` and `tracker_typing`, plus a save `Job` and the shell's prompt-save channel.
  - `dirty()` (:314-316) counts only `editing`, so typed-but-uncommitted repo or API-key text never raises the leave prompt.
  - `go()` doesn't cancel `tracker_typing`; only `project_changed` (:272-281) and `cancel_typing` (:348-355) do.
  - **Include `tracker_typing` in `dirty()`**, or cancel it on leave.
- **L5. Prereq screen flags around a well-encoded model.**
  - `PrereqApp` (`prereqs/mod.rs:133-155`) holds `checks`, `validating`, `skip_dialog`, `auto_continue`, `copied: Option<(String, Instant)>` and `save_error` beside the run-id-gated shared `Prereqs` (`shelbi-app/src/prereqs.rs:424-435`).
  - `skip_dialog` and `locating` are kept exclusive only by dispatch order (:320 before :328).
  - `auto_continue` clears only when a tick observes `Missing`, so a locate started during the first check can let a later Ready auto-continue.
  - **Document.**
- **L6. Feedback dialog accepts typing while sending.**
  - The shared `FeedbackForm` has `error`, `sending` and `sent` (`shelbi-app/src/feedback/form.rs:110-112`); `sending && sent` is representable.
  - The TUI's `in_text` (`overlay/feedback.rs:176`) and `field()` (:207-215) check only `sent()`, so keys and pastes edit the fields mid-send and `sync()` clears the error.
  - New task gates on `creating()` (`overlay/new_task.rs:183`), so the two dialogs disagree.
  - **Make explicit** as `Phase { Draft, Sending, Sent }` in shelbi-app.
- **L7. Add-project form: a synchronous request/answer protocol.**
  - The shared form returns `Step::Validate`/`Preflight` and `linear_due()`, and the TUI answers inline on the UI thread (`overlay/add_project.rs:236-275`).
  - `menu` and `custom` Options are kept exclusive by hand (:171-174, :190, :193, :215-219, :351-372).
  - A gh or Linear stall freezes the dialog with no Busy state.
  - **Document**; make it async if stalls get reported.
- **L8. Terminal ownership hand-off (shell / editor / remote ssh) and the stderr redirect.**
  - Process-global terminal modes plus the `SAVED_STDERR` atomic (`shell/stray_output.rs:25`).
  - `enter_terminal_modes`/`redirect_stderr` (mod.rs:7396-7413) and `restore_terminal`/`restore_stderr` (:7370-7386). Ordering is kept by the run loop alone (remote hand-off at :629-640).
  - The retry-notice sink stays installed during hand-offs (:507/:814).
  - The functions are idempotent and tested (bug tag `tui-stderr-corrupts-screen`).
  - **Leave**; optionally an RAII guard.
- **L9. Sidebar divider drag.**
  - `sidebar_dragging` (mod.rs:1531) starts at L6184 and ends only on a Left Up (L6294).
  - The overlay and Acknowledgements branches return before `handle_mouse` (L2758, L2785), and FocusLost doesn't end the drag.
  - If the Up is lost, the next click anywhere resizes the sidebar to that column (L6171-6172).
  - **End the drag on FocusLost and when an overlay opens.**
- **L10. Workspaces machine form: readiness timer.**
  - `MachineLayer` (`settings/workspaces.rs:139-149`) keeps `started: Option<Instant>` beside `stop: CheckStop` and the shared `ReadinessForm::running()`: "check running" in three places.
  - `started` is set at :736 and cleared at :719/:763, but not in `paste` (:629), which can stop the check.
  - It only affects the elapsed label.
  - **Leave**, or key `started` by run id.
- **L11. Legacy sidebar `App` (dead code with a drift hazard).**
  - `app.rs`'s `App` is constructed only by its own tests. It is still `pub use`d (`lib.rs:171`), so there is no dead-code warning.
  - It carries a second copy of the review-section split: `ReviewState {Serving, Loading, Pending}` derived from `assigned_to`, tags and the `.shelbi/shelbi-review-loaded` marker (`app.rs:1455-1531`), plus `review_job` (:258, writers :1012/:1031/:1047/:1054).
  - Its docs claim the rules "can't drift" from `SidebarModel`.
  - **Delete it**, after porting useful tests to `SidebarModel`.

---

## Minor (coverage)

- **Kanban modal stack.**
  - `popover`, `workspace_dropdown`, `workflow_dropdown` and `search_editing` (kanban.rs:68/78/113/194), with precedence by handler order (`handlers/kanban.rs:30-55`).
  - The mouse path never checks `search_editing` (:155-219), and the shell's `+ New issue` modal check omits it (mod.rs:6414-6416). Cosmetic.
- **Kanban selection by (column, row) index** (kanban.rs:61-62): a refresh can put a different card under the cursor.
  - `reorder` (:1917-1929) persists and refreshes synchronously on the UI thread, with no overlay.
  - The sidebar has the same index-not-identity issue (see Cross-slice leads), as do `MachinesApp.selected` (machines.rs:166-171) and `workspace_panel` after `set_tools` (shell/workspace.rs:295, review.rs:555).
- **ReviewPanel `merging`** (review_panel.rs:69-79): a single-sync-point mirror of `ReviewDecisions`. Fine apart from M3.
- **Quit/exit sequence** (`should_quit`, `pending_quit: Option<QuitAction>`, `should_reexec`; mod.rs:1553/1707/1710; writers at L2653, L2666, L579-592): well encoded.
  - Small gap: `pending_editor`/`pending_remote` are still served in the same loop pass after `should_quit` is set (L603, L611/L626).
- **`pending_editor` + `pending_editor_line`** (L1658/1660): two Options that must pair. `open_editor` (L4727) doesn't reset the line.
- **`reported_main: Option<Size>`**: a force-resize cache invalidated by about 15 `= None` writes. `full_redraw`/`dirty` are repaint flags.
- **Throttles and slots that are fine:**
  - `last_review_reconcile` + `reconcile_rx` (~1 Hz with a single read in flight, L5424-5475).
  - `changelog_issue_rx` + `issue_read_from` (cleared together).
  - `notice{until}` and `retry_notice` (timed one-shots).
  - `create_job`, `feedback_job`, `new_task_job`, `close_others_job` (one slot each, guarded by their dialogs).
- **Three copies of "what's shown"**: `client.view()`, `main_view` and `sessions.current_target()`. They match today by convention.
- **Connect dialog** (`launch/connect.rs:108`): its `job` mirrors the guarded `ConnectFlow.busy` (`shelbi-app/src/launch/connect.rs:72`).
  - Esc while busy still lets `connect_remote` record the host in `RecentRemotes` (:290).
  - Clicks resolve against the last frame's `hits`.
- **Progress printer** (`progress.rs:71-83`): `paused` is a bool rather than RAII (:379/:382). A daemon `Started` with no `Done` is erased silently on an early `?` return (`quit_progress.rs:46,48`).
- **Changelog pending reveal** (`shell/changelog.rs:430`, set :492, applied :496-514, cleared :512): stays pending indefinitely if the id never appears, then snaps the scroll later.
- **Jobs sheet** `loading: Option<Receiver>` and `SheetBody::Loading` encode one state twice (jobs.rs:98/104).
- **Palette** `scroll`/`follow` are `Cell`s mutated during render (overlay/palette.rs:982-987).
- **`review_reject`** wrap width comes from render (:100).
- **`caps.kitty`**: "not probed" looks the same as "no" until the probe (caps.rs:80-94). Harmless, because the keyboard notice arms after the probe.
- **`diff_view.rs`** `open_failed()` infers a failed open from `error && table empty` (:197-200).
- **`keybindings.rs`** `editing` has about 8 writers alongside `capture()`.
- **Orchestrator settings page** `orch_load.project()` doubles as the page's project.
- **Agent editor and workflow editor UI-mode Options** (`modal`, `editing`, `typing`, `skill_form`, `custom_model`; agent_editor.rs:338-347): `Act::Edit` (:652) doesn't clear `typing`, and `handle_key` precedence settles it.
- **Builder `panel_open`/`narrow`** is a layout-derived mode written during render (builder.rs:904-906).
- **Vestigial standalone flags**: `ActivityApp.should_quit` (reset at mod.rs:6022) and `handlers/kanban.rs` `Outcome::Quit`/`OpenPalette` (swallowed at mod.rs:6004).
- **Explicit and fine:** `ActiveOverlay`, `acknowledgements` Focus, `About`, `close_others::Dialog`, `zen_intro`, `review_confirm`, `TransientErrorLog` (error_report.rs:58-150; its module doc is outdated), theme `INSTALLED`, `ToolLayer`, `WorkflowsPage::Modal`, onboarding (shelbi-app `OnboardingState`).
- **No machines** (pure render or data): about_icon, button, textbox, table, modal_form, list_screen, picker, sidebar.rs (top-level), keymap, markdown, licenses (LazyLock), linear_rows, launch/rows, startup (OnceLock), build.rs, tests/no_color_literals.rs, panel.rs, handlers/mod.rs, diff_view/paint.rs, refresh.rs (re-export), remote_session.rs (thin adapter), machine_form.rs, workflow_editor/canvas.rs.

---

## Cross-slice leads

- **`shelbi-app/src/settings.rs` (`LeaveGuard`, `Job`, `Ticket`, L370-560).**
  - `prompt` and `after_save` can both be Some, and nothing resets on a context change: the root of H3. The desktop `settings_nav` likely shares it.
  - `Job` doesn't let a save supersede a load: the root of M9.
- **`shelbi-app` settings editors** (`settings/agent_editor.rs`, `workflow_editor.rs`, `agents.rs`): the shared `busy` / `editing_shared` that the clients mirror (M8, M10). Check the desktop's thread-spawn-failure path.
- **`shelbi-app/src/review.rs:590` (`ReviewDecisions`)**: keyed by task only (M3).
- **`shelbi-app/src/session/relaunch.rs:115-140` (`ReviewRelauncher`)** and **`session/restart.rs` (`RestartController`)**: a detached connect worker can fire Ensure after Close (M1). `reconnect` resets supervision (L1).
- **`shelbi-app/src/review/diff_load.rs` (`DiffLoad`, 15 s timeout)** and **`shelbi-app/src/diff/`**: the View Diff wait and the diff modes behind M1.
- **`shelbi-app/src/board.rs:857-1130`**: `BoardInput::apply_open` overwrites the persisted filter and overrides on every read (L3); `ClosedHistory` paging is well encoded.
- **The optimistic move lives in the TUI only.** The desktop's `CommandKind::MoveIssue` (`desktop/src/commands.rs:154, 448`) has no overlay. Move persistence runs `shelbi_orchestrator::transition::move_issue_with` (branch cut, gated merge, `move_status`). Its partial-failure semantics decide whether H2's "nothing persisted, roll back" assumption (kanban.rs:1787) holds. The daemon route is `shelbi_app::execute_mutation` (`shelbi-app/src/exec.rs`).
- **`shelbi-app/src/nav.rs:292` (`clamp_selection`)**: sidebar selection is by index. It is called from mod.rs:2103 on every snapshot and resolved via `SidebarView::target_at` (shell/sidebar.rs:287). A row inserted above the selection makes Enter activate a different row. (app or desktop slices.)
- **`shelbi-app/src/reminders.rs:339-391` (`SheetState`)** and **`desktop/src/views/jobs.rs:162,376-387`**: the cancel state. The TUI never calls `cancel_failed`, and the jobs sheet state is duplicated per client (M7).
- **`shelbi-app/src/activity.rs:~850`** plus **`shelbi-state/src/event_log.rs:1096-1120`** (`read_event_log_since` / `_recent`): M5.
- **`shelbi-app/src/feedback/form.rs`** (`sending`/`sent`/`error` flags, L6), **`overlay/add_project.rs`** (the Validate/Preflight/Linear protocol, L7) and **`launch/connect.rs`** (`ConnectFlow`, well encoded).
- **Daemon layout events.** `LayoutEvent::ReviewOpened` / `ReviewClosed` come from the orchestrator poller (`review_ui::review_layout_state`) via shelbi-state. `review_open_pending` (H1) depends on their reliable delivery.
- **The review-loaded marker.** Live sidebar review sub-states come from `shelbi_app::view::SidebarModel` and `shelbi_orchestrator::workspace::review_slot_is_serving` (the on-disk `.shelbi/shelbi-review-loaded` marker plus session liveness).
- **`shelbi-orchestrator` `project_create::create_project`**: is it crash-safe when the launch screen exits mid-create (M6)? Also `migration::MigrationPass`, driven from `lib.rs:233-338`.
- **`shelbi-client` / `shelbi-proto`**: daemon quit `Step`/`StepState` events, with no guarantee of `Done` after `Started` (progress printer). Also `Notice::Reexec` / `Detach` and `shelbi_state::daemon_version_status` (the re-exec listener).
- **`shelbi-app/src/launch/terminal.rs:111-140` (`ReconnectController`)**: the remote-session reconnect loop. **`shelbi-app/src/onboarding.rs`**: explicit.
