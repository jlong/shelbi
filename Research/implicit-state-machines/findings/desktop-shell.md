# Implicit state machines: desktop-shell

**Slice:** everything in `desktop/src/` except `views/`: `app.rs` + `app_*_tests.rs`, `app_tab_tearoff.rs`, `elements/` (incl. `terminal/`), `platform/`, `drops/`, `menus/`, `theme/`, `project_tabs.rs`, `projects.rs`, `refresh.rs`, `feeds.rs`, `window_state.rs`, `notify.rs`, `commands.rs`, `keys.rs`, `source_gate.rs`, `main.rs`, `app_icon.rs`, `assets.rs`, `licenses.rs`, `spike.rs`, `test_env.rs`, `remote_fixture.rs`, `bench*`, `visual_*`.
**Coverage:** every file was considered. Read in full: all of `platform/`, `project_tabs.rs`, `projects.rs`, `refresh.rs`, `feeds.rs`, `window_state.rs`, `app_tab_tearoff.rs`, `notify.rs`, `elements/terminal/mod.rs`, `session.rs`, `scrollbar.rs`, `search_bar.rs`, `drops/*`, `menus/mod.rs`, `theme/motion.rs`, `fold.rs`, `window.rs`. Read by section: `app.rs` (about 195-7330). Grepped or outlined, pure or test-only: style/palette/contrast/diff, terminal render helpers, `keys.rs`, `licenses.rs`, `assets.rs`; the bench and visual harnesses are feature-gated tooling (`main.rs:12-52`); `source_gate.rs` is a test-only syn gate. Tests were read for evidence where cited.
**Findings:** 3 high, 8 medium, 10 low, plus a Minor list. Confirmed bugs from reading: the dead host-link watch (H2), the window left on a New tab or chooser that is not restored (M2), the open-set note that a reload overwrites (M1), the stale workspace notice (M6), and the plugin backup deleted by the sweep (L10).
**Overall:** `ShelbiWindow` (`app.rs`, about 100 fields) is the centre: several machines live there as flag soup, and they move by hand-written reset protocols. Bootstrap (`platform/bootstrap.rs`) is the one explicit, well-tested machine. The trouble around it is ownership: one run per window, a cached `Ready`, and watch threads that never stop.

---

## H1. Window content mode (project / chooser / New tab / pending remote tab)
- **Entity:** `ShelbiWindow`.
- **Encoding:** 1, 7.
- **States:** ShowingProject, Chooser (empty state), NewTab, PendingRemote (its host's sub-state is in H2). The fields are:
  - `project: String`, "meaningless while chooser" (app.rs:1461);
  - `chooser: bool` (1482);
  - `new_tab: Option<Entity<LaunchView>>`, "chooser is set too" (1502-1504);
  - `pending: Option<ProjectKey>`, "chooser is set" (1471-1474).
- **Representable but invalid:**
  - `new_tab` is Some and `chooser` is false;
  - `pending` is Some and `chooser` is false;
  - `new_tab` and `pending` are both Some;
  - `project` holds a stale name while `chooser` is set.
- **Transitions:**
  - Initial state: 755 (`Opening::Blank => w.chooser = true`), 1753, 2349.
  - `show_pending` 2150-2176; `switch_to_key` 3024-3093 (3042-3049); `show_chooser` 3096-3122 (3104-3106); `open_new_tab` 3127-3155 (3135-3147).
  - `close_new_tab` 3158-3172; `after_quit_project` / `leave_quit_project` 3478-3526; `new_tab_chose` 547-581.
- **Readers decode the combination by hand:** `on_pending_tab` 2180 (`chooser && pending.is_some()`), `retry_pending` 2258, `window_title` 1890, `record_slot` 3425-3429, `host_decided` 2234, `shown_host` 1818, `view_state_to_save` 3277, `sync_tabs` 5255-5259, `chat_wanted` 4851.
- **Risk: high.**
  - The "leave the shown project" step (`if !self.chooser { remember_view_state; teardown_project }`, then reset the model, palette inputs, title, slot and menus) is copied into `show_pending`, `show_chooser` and `open_new_tab`. Each copy resets a slightly different subset: only `show_chooser` resets `chooser_highlight` and re-reads the catalog, and only the other two reset `route`.
  - Every new mode or field must be threaded through all of these copies and every predicate. A missed `pending = None` leaves the title, the slot save and `host_decided`'s automatic switch (2234) acting on a tab that is no longer selected.
- **Recommendation: make explicit.** Use `enum Showing { Project(ProjectHandle), Chooser, NewTab(Entity<LaunchView>), Pending(ProjectKey) }` with one `leave_current()` and one `enter(Showing)`.

## H2. Per-window remote host lifecycle (`HostTabs`)
- **Entity:** each other machine a window has tabs on: `hosts: BTreeMap<String, HostTabs>` (app.rs:1468).
- **Encoding:** 1, 4 (mirrors the link, the far daemon and its version), 6 (a 3 s watch loop, `REMOTE_WATCH` 1424), 8 (`HostOpening` covers only part of it).
- **States:** fields at 1427-1458: `remote: Option<RemoteWindow>`, `bar: Option<RemoteBar{Stopped|SignIn}>`, `changed: Option<line>`, `opening: HostOpening{Idle,Opening,Failed}`, `_watch: Option<Task>`. The real states are Unopened, Opening, Failed(line), Open, Open+Bar, and Changed(line).
- **Representable but invalid:**
  - `Opening` with `remote` Some;
  - `bar` and `changed` both Some;
  - `_watch` Some holding a task that has already returned.
- **Transitions:**
  - Opening: `open_host` 2189-2217, `host_decided` 2220-2254, `retry_pending` 2257-2267.
  - Open: `adopt_host` 1844-1863, `become_remote` 1804-1813.
  - Link state: `host_kept` 1946-1973, `host_changed` 1975-1989, `after_far_action` 2076-2110.
  - Reset and removal: `reopen_remote` 2010-2023 (`*entry = HostTabs::default()`), `forget_idle_hosts` 1879-1886.
  - The watch loop itself: `watch_host` 1911-1944.
  - Writers outside `app.rs`: `move_tab` calls `adopt_host` at app_tab_tearoff.rs:349, and `tear_off` at about :390.
- **Risk: high.**
  - **(a) Confirmed bug: the watch can die for good.**
    - The watch loop returns once `changed.is_some()` (1916-1922), but the finished task stays in `_watch`. `adopt_host` starts a new watch only when `_watch.is_none()` (1856-1860).
    - Reopen resets the entry, so it works. Any other path re-adopts the changed host through the `_ =>` arm, which clears `changed` and `bar` but keeps the dead task: `open_remote_in` when a window already has the tab (app.rs:613-616, from Connect, the New tab or the launch screen), or `move_tab` (app_tab_tearoff.rs:349).
    - After that the link is never kept up again and no bar ever shows.
    - The test `a_host_that_changed…` (app_remote_tests.rs:496) only goes through Reopen.
  - **(b) The host's verdict doesn't travel with the tab.** `move_tab` and `tear_off` clone only the `RemoteWindow` (`host_of_tab` app_tab_tearoff.rs:268-270). The source window's bar, `changed` and `opening` are lost, so a Changed host's tab opens as healthy in the target window until a watch rediscovers it.
  - **(c) Bar and Changed can coexist.** `after_far_action` sets `bar = SignIn` (2096-2099) without checking `changed`.
  - **(d) Overlapping decides.** `open_host` guards only on `remote.is_some() || opening == Opening` (2194). A Reopen, or a tab re-added, while a decide is in flight starts a second one, and the last to land wins; `host_decided` has no generation.
  - **(e) "Refused" is held twice:** by the watch and by `RemoteWindow.sessions` (`RemoteSessionConnector::refusal`, 1375-1380, read at 1951).
- **Recommendation: make explicit.** Use `enum HostState { Opening(gen), Failed(line), Open { remote, watch: Task, bar: Option<RemoteBar> }, Changed(line) }`, with the watch owned by `Open`. Moving a tab moves the whole `HostState`.

## H3. The app's connection to Shelbi: one bootstrap run per window, a cached `Ready`, control watches that never stop
- **Entity:** `Ready` plus its `ControlHandle` and relaunch latch, the control-watch thread, and each window's `Boot`.
- **Encoding:** 5, 4 (daemon reachability), 6 (a 2 s reconnect, a 5 s hello timeout), 8 (`Ready` copied into several holders), 1 (`Option` latches).
- **States:** No run; N runs in flight; Ready held per window (`Boot::Ready`, app.rs:939-945); cached `Shell.ready` (204), which is never cleared. Per watch: Connecting, Disconnected (retry), Subscribed, RelaunchRequired (terminal). `Boot::InFlight(BootstrapState::Ready|Failed)` is representable but invalid; only the order of match arms in `apply_boot` (2818-2835) prevents it.
- **Transitions:**
  - Each `ShelbiWindow::new` reads `Shell.ready` (1712) and calls `start_bootstrap` when it is `None` (1735-1738, 2798-2812). Blank windows do the same (1748-1758).
  - `open_launch_windows` (278-284) opens every restored window synchronously, so N runs start at launch.
  - Each Ready overwrites `Shell.ready` (2827); `share_ready` writes it too (251-255, from views/cli_setup.rs:245 and views/launch.rs:457).
  - Each run's thread starts its own `watch_as` after Ready (bootstrap.rs:431-441).
  - The watch loop is control.rs:185-218. The latch is `require_relaunch` (control.rs:112-120, called at 133 and 181) and is never cleared.
  - `BootstrapDaemon` hand-off slot: filled at platform/launch.rs:83, taken at views/launch.rs:457.
- **Risk: high.**
  - **Install contention.** N windows with no CLI each try to install. Every run but one gets `InstallErrorKind::Busy` (install.rs:234-236), which becomes `Failure::InstallFailed` (bootstrap.rs:192-195). Those windows stay on a failure screen even though the CLI is now installed, until the user clicks Try Again (app.rs:2872).
  - **Daemon starts and their log.** Every run calls the process-wide `set_daemon_launcher` (daemon.rs:52, last writer wins) and rewrites one `desktop-daemon-start.log` (daemon.rs:35-41). A failure screen's stderr tail can belong to another run's start.
  - **Relaunch latch drift.** The latch is per handle (control.rs:74). The relaunch bar (app.rs:7656) and the command gate (3787) read the window's own handle, so a window whose watch is between reconnects keeps sending commands after another window has latched.
  - **Watch threads never stop and pile up.** The loop ends only on relaunch, and `hello` (control.rs:245-257) abandons a blocked thread every 5 s timeout. Against a wedged daemon, one thread leaks about every 7 s, per watch.
  - **Lost hand-off.** When `launch::open_project` fails after `ensure_running` succeeded (views/launch.rs:463-466), `take()` never runs. The run's watch keeps going with nobody listening, so its `RelaunchRequired` and `Detach` are lost, and the next open starts another full bootstrap.
  - **Stale cache.** `Shell.ready` is never invalidated, so after `desktop.cli_path` changes, `running_cli` and `automatic_cli` (259-269) report the old CLI until the app restarts.
- **Recommendation: redesign.** Use one app-global bootstrap owner (a shared run on `Shell`) that windows subscribe to, with one `ControlHandle` and one cancellable watch per process. Give the hello a socket read deadline instead of abandoning threads. Bootstrap's own `run` (bootstrap.rs:165-283) is a good model for the rest; leave it.

---

## M1. Catalog open set (a mirror of which projects the hub has open)
- **Entity:** `Catalog` (projects.rs:51-73): `liveness.open` / `liveness.working` (:56), plus `open_known` (:59).
- **Encoding:** 4, 1, 6 (a 150 ms debounce, :40 and :247), 5 (local notes expected to land before the next refresh).
- **States:** Unknown (`open_known` false, so `open_projects()` returns `None` and tabs drop nothing, project_tabs.rs:204-207); Known and matching disk; Known plus a local note disk hasn't confirmed. The last state is not represented: the note is written straight into the same set.
- **Transitions:**
  - Full swap in `reload` (projects.rs:182-219). It reads liveness at 187, then does slow per-project skeleton and view reads, then swaps at 212. Started from main.rs:317 and from `refresh_catalog` (app.rs:5289-5307).
  - `reload_liveness` (224-230), from the `watch_changes` thread (238-256).
  - Notes: `note_open` (117-125), called at app.rs:2932 and 5733.
- **Risk: medium.**
  - The doc at projects.rs:114-116 promises "a tab must neither come back nor go meanwhile". But a `reload` or `reload_liveness` whose disk read began before the daemon wrote the record replaces the whole `liveness` (212, 228) and erases the note. `reconcile` then re-adds the quit project's tab in other windows (project_tabs.rs:211-213).
  - Swaps carry no generation, and each `refresh` spawns its own thread (170-181), so an older read can land after a newer one.
  - View state already has a `remembered` overlay (62-64, 215-218) for exactly this race; liveness has none.
- **Recommendation: make explicit.** Add a monotonic read generation and a pending-notes overlay that a read clears only once disk agrees.

## M2. A window's tab list and its persisted slot (four copies, ownership across windows)
- **Entity:** the tabs of each window. There are four copies:
  - `ProjectTabs.order` (project_tabs.rs:65-67);
  - `window_state.tabs` (written at app.rs:5278 and 3053-3055);
  - `Shell.session.slots[n]` (`record_slot` 3423-3438);
  - `desktop-windows.json` (`save_session` 910-932).
  - Ownership across windows is decided by the `elsewhere` rule (project_tabs.rs:211, built at app.rs:5262-5269).
  - `Slot { project: Option<ProjectKey>, open: bool, state }` (window_state.rs:180-188).
- **Encoding:** 4, 5, 3, 6 (a 400 ms debounced save, app.rs:5986-6004), 2 (the `NEW_TAB` sentinel `"/new-tab"` sits in the same `Vec<String>`, project_tabs.rs:39).
- **Transitions:**
  - `reconcile` via `sync_tabs` (5253-5274).
  - `insert`: 2165, 3053, 3122. `replace(NEW_TAB)`: 2153, 3038.
  - `remove`: 3157, 5476, 574, 610, and app_tab_tearoff.rs:277.
  - Drag drop: app_tab_tearoff.rs:206 and 294.
  - Slot closed: `window_closed` (892-907), only when other windows remain. Restore: `launch_plan` (window_state.rs:260-309).
- **Risk: medium.**
  - **Confirmed bug: a window left on its New tab or the chooser is not restored.** `launch_plan` drops every open slot whose `project` is `None` (window_state.rs:286, `s.project.as_ref().filter(..)?`), even though the slot is `open: true` and holds that window's other tabs. On relaunch the window and its tab arrangement are gone, and `reconcile` re-adopts its projects into some other window. `the_new_tab_is_never_saved` (app_new_tab_tests.rs:282) covers only `window_state.tabs`.
  - **A tab change shortly before ⌘Q is lost.** `tabs_changed` only schedules a save (400 ms), the save runs on a detached task (918-929), and quit saves only view state (1791-1797).
  - **Closing a window never re-syncs the other windows' tabs** (`window_closed` only marks the slot). The closed window's open projects stay tabless until a refresh happens to run.
  - **A window that can't be read is skipped.** `elsewhere` skips it silently (app.rs:5266 `h.read(cx).ok()`), which can produce duplicate tabs.
  - **Remote tabs never leave by the open set** (project_tabs.rs:209 `is_remote_id`).
  - Past races are pinned by `a_project_with_a_tab_in_another_window_gets_none_here` (project_tabs.rs:332).
- **Recommendation: redesign lightly.** An app-level tab registry should own which window holds which tab id. Derive `window_state.tabs` from it, re-sync on window close, flush slot saves synchronously on quit, and restore an open slot that has tabs and no project.

## M3. Project-bound state teardown (reset-by-hand protocol)
- **Entity:** the ~40 project-bound fields of `ShelbiWindow`.
- **Encoding:** 5, 1.
- **States:** bound to project P, or torn down. Fields are reset one by one in `teardown_project` (app.rs:3197-3272).
- **Risk: medium.** Every new project-bound field needs a line there. Gaps are already visible:
  - `reject_reasons` (1624) is not cleared; completion at 6608-6626 copes with a switch.
  - `settings_over_review` (1538) is not reset there; it is reset at 6395 and consumed at 5939.
  - `close_remote_panes` (1991-2007) decides "anything open?" from a hand-kept list of 9 `Option`s that must mirror teardown.
  - The per-window `notifier` is rebuilt separately at 1809 and 3071 (see M6).
- **Recommendation:** group project-bound views, tasks and flags into one `ProjectViews` struct that is replaced wholesale on a switch.

## M4. Main-area navigation stack and the review composite
- **Entity:** what the main area shows, and the review open in it.
- **Encoding:** 1, 5, 7, 8. The stack is implicit:
  - `Route {View, Settings, Review}` (947-956);
  - `settings_over_review: bool` (1538: Settings over Review over View);
  - the `ClientState` view underneath;
  - `workspace_return: Option<View>`;
  - `detail_over: Option<Shown>`.
  - The review adds the `ReviewView`'s pending/shown/holding states (views slice), `review_loader`, `quiet_review: Option<String>` (1611), `decisions` (1621), `reject_reasons` (1624), and the column push (`column`, `column_moving`).
- **Transitions:**
  - Review: `load_review` 6410, `open_review_pending` 4547, `open_review` 4512, `run_load_step` 6422-6508, `abandon_pending_review` 4567, `leave_review` 4611, `run_review_mutation` 6570-6635, `land_decision` 4746, `decision_refused` 4761, `review_ended` 4803, the reopen after a switch at 3403-3406.
  - Settings: `open_settings` 5831-5880 (5837), `leave_settings` 5935-5955.
  - `set_route` is called from about 12 sites (2163, 3141, 3251, 3398, 3829, 4528, 4553, 4671, 5872, 6397, …).
  - Attachment is reconciled every frame by `sync_chat` (4890-4938), from a derived `chat_wanted` (4849-4857: boot, chooser, switch_hold, route, view, onboarding).
- **Risk: medium.**
  - A "quiet" reopen (no prompt, no banner) is told apart from a user's open only by `quiet_review` matching the task string (6440-6445, 6499).
  - Load and decision results are keyed by task id, and each completion checks something different: `review_loader.pending()` at 6484, the project key at 6609.
  - The push gate (`push_ahead` 4577, `release_settled` 4583) adds a frame-timed dimension.
  - Transitions inside `render_shell` (7244-7300) happen only when the window paints.
- **Recommendation: document** the stack and the review glue. `ReviewLoader` and `ReviewDecisions` are already explicit (cross-slice). If Settings or Review nesting grows, model the route as a small stack type.

## M5. Terminal pointer grab: selection drag, scrollbar drag, autoscroll, forwarded buttons
- **Entity:** `TerminalPane` (elements/terminal/mod.rs:316).
- **Encoding:** 1, 6, 7.
- **States:** Idle, ScrollbarDrag{grab}, LeftSelect (with autoscroll up or down), OtherButton(b). The fields are `left_down` (332), `other_down` (334), `scrollbar_drag` (340), `autoscroll: Option<(bool, Task)>` (360) and `last_motion_cell` (337), plus `MouseGesture`'s selecting state (shelbi-term). Several of these can be set at once, which is invalid.
- **Writers:**
  - `scrollbar_drag`: 1555, 1680. `left_down`: 1620, 1692. `other_down`: 1622, 1695.
  - `autoscroll`: 1751, 1691, 1717, and the task itself at 1729.
- **Risk: medium.**
  - Mouse listeners exist only while the element paints (element.rs:545-598). A button released while the pane isn't painted (for example, a keyboard view switch mid-drag) is never seen.
  - A stuck `scrollbar_drag` routes every later pointer move to `scrollbar_move` without checking the pressed button (1644-1646), so plain hovering scrolls the scrollback.
  - A stuck `left_down` keeps the 50 ms autoscroll task running (1724-1750) while the pane is hidden.
- **Recommendation: make explicit.** Use `enum Grab { None, Scrollbar{..}, Select{autoscroll}, Forward(button) }`, and end any grab when a move arrives with no pressed button.

## M6. Notification tracker (edge detection over mirrored agent state)
- **Entity:** `notify::Tracker` (notify.rs:100-244), one per window (app.rs:1643).
- **Encoding:** 4 (workspace badges, the review set, the orchestrator title marker), 6 (cooldown), 1 (`Option` used as "unprimed").
- **States:** Unprimed (`None`), or Primed with the last badge per workspace, the last review set and the last orchestrator `WorkspaceState`. Plus a `posted` map of last post times per tag.
- **Writers:** `observe_sidebar` (157, 177), `observe_orchestrator` (192), `record_posted` (232). The tracker is replaced wholesale on switch (app.rs:1809, 3071).
- **Risk: medium.**
  - **Confirmed: a removed workspace's notice is never cleared.** The workspace loop walks only the new model (notify.rs:135-155). Reviews diff old against new (171-175); workspaces don't, so a notice stays in Notification Center after its workspace is removed while blocked.
  - **A restart can post a false "finished a turn".** The orchestrator state is never reset when its session dies or relaunches, and a title with no marker keeps the old state (189-191). Working, then death, then the new session's first idle reads as a finished turn.
  - **Background projects are silent.** Notices follow only the window's current project and re-prime on every switch.
  - `posted` is never pruned.
- **Recommendation:** document the "transitions only" contract. Clear on removal for workspaces, reset the orchestrator state on a session change, and decide whether tracking is per project or per window.

## M7. Image and file-promise drop pipeline
- **Entity:** one AppKit drag through to gpui's `on_drop` (drops/mod.rs, drops/macos.rs, drops/pasteboard.rs).
- **Encoding:** 5 (perform, materialise, `PromiseBatch`, `land`, `deliver`, then a replayed Entered/Submit/Exited), 4, 6.
- **States:** Accepted, Materialising(remaining=n), Landed, Delivered or Dropped. Only the counter is explicit: `BatchState{slots, remaining, done}` (pasteboard.rs:270-320).
- **Writers:** `perform_drag_operation` (macos.rs:178-194), `materialise` (pasteboard.rs:91-120), `receive_promises` (188-248), `PromiseBatch::arrive` (294-319), `land` (mod.rs:95-106), `deliver` (109-128), `replay_drop` (134-144).
- **Risk: medium.**
  - **A drop can vanish silently.** It is reported accepted (macos.rs:192, pasteboard.rs:247) before any file exists, and there is no timeout or failed state. A reader block that never fires leaves `remaining > 0` forever, with no note to the user.
  - **Extra files can be dropped.** The expected count is `promised_count(r).max(1)` (214), so a receiver that delivers more files than it promised fires `done` early and loses the rest.
  - **The drop is replayed at a point, not a pane** (mod.rs:141-143), so a layout change during materialising pastes into the wrong pane or nowhere.
  - **A window closed meanwhile drops it silently.** The window is matched by NSWindow address (118), and `land` ignores `try_send` errors (100-104).
- **Recommendation: make explicit.** Keep a per-drop record that targets a pane entity, add a timeout or failed state that surfaces a note, and trust delivered callbacks over `max(1)`.

## M8. Cross-window tab drag session
- **Entity:** `TabDragSession(Option<DragState>)` (views/tab_tearoff.rs:274-276), with `over: Option<WindowId>`, `gap`, `float`, `float_opening` (174-210). It is mirrored into each window's `TabReorder` (views/tab_drag.rs:210-229), with its own `project`, `slot`, `settle`, `hosting`, `incoming`, `away`.
- **Encoding:** 7, 1, 5. The drop is deferred and two-phase: `take_tab` in the target, then `give_up_tab` in the source.
- **States:** Idle, over its own bar, over another bar, torn off. A drop resolves to reorder, move, tear off or window move; Esc cancels.
- **Writers:** `begin` (app_tab_tearoff.rs:90), moved (179-187), dropped (199-238), `move_tab` (331-356), `tear_off` (375-432), cancel (250-263).
- **Risk: medium.**
  - **The mirror drifts.** This has happened: `a_bar_left_hosting_by_a_missed_move_closes_its_gap_on_its_next_frame` (app_window_tests.rs:1232) and the comment at tab_drag.rs:205-208. The fix gates the bar's role on `cx.has_active_drag()` (tab_tearoff.rs:285-290).
  - **The session ends only through the source window's drop or cancel.** If the source closes mid-drag, the session and its floating ghost stay until the next `begin`.
  - **`DragState.tearable` is dead state.** It is always `true` (app_tab_tearoff.rs:85-86).
- **Recommendation: make explicit.** Add a `DragPhase` enum, derive each bar's role from the session at render, and end the session in `window_closed`.

---

## Low
- **L1. Switch hold and pending sidebar rows.** The fields are `switch_hold: Option<SwitchHold{rendered, review}>` and `pending_rows` (app.rs:1198-1229). They are set in `adopt_view_state` (3321-3354), advanced by frames in `render_shell` (7253-7260), finished by `finish_switch` (3377-3413), and settled by `apply_pending_rows` (3357-3375), which waits on `board_loading`. They are cleared at 3260-3261 and 2677. The switch is held while the host is changed (7253), and the rows wait for as long as the board keeps loading. Document.
- **L2. Deliberate close fanned out to views.** `mark_deliberate_close` (5760-5791) sets a flag on chat, workspace, review and issues once the user confirms (5612-5616), and clears it on refusal (5745-5747). A view created between the mark and the reply misses it, so its session's exit is restarted as a crash. Hold "closing P" once at window level.
- **L3. Project open and quit completions vs. switches.** `open_project`'s completion checks only `shows(&opening)` (2916), so after a quick A→B→A a stale completion applies `note_open` and a chat reconnect. This is harmless while `orchestrate` is idempotent (projects.rs:357-358). The quit race is pinned by app_window_tests.rs:414. Use a per-switch generation.
- **L4. Feed loop: link, poll timing, orchestrator-title cache, refcount** (refresh.rs:300-374, feeds.rs:47-247).
  - A late subscriber gets only `last_link` and `last_snapshot`; `Session` events aren't cached (feeds.rs:145-147, 192).
  - `orchestrator_title` is never reset on a `None` probe (refresh.rs:363-374), so a restart that sets the same title sends nothing and the waiting notice can be missed.
  - The hub entry reads through its first subscriber's handle forever (feeds.rs:113).
  - Mostly leave as is; replay or reset the title cache.
- **L5. Terminal PTY size reporting** (terminal/mod.rs:372-374, 1015-1057; the `ResizeHold` global at 135-162). The fields are `reported_size`, `pending_size` and `resize_task`; "hold just lifted" is inferred from `pending.is_some() && resize_task.is_none()` (1039).
  - The debounce task leaves `resize_task` holding a finished task, and takes `pending_size` even when `io` is `None` (1050-1054).
  - `claim_size` (797) reports a size mid-hold.
  - `ResizeHold` ids leak on window close.
  - Fix: a small `SizeReport` enum.
- **L6. Terminal `PaneStatus` vs `io`** (mod.rs:183-194, 328). Live with `io: None`, and Exited with `io: Some`, are both representable. Input is not gated on Live (1067, 1127), and the writer thread dies silently on a write error (session.rs:279-281). A connect failure emits no event (422), but only tooling uses `connect()` (spike.rs:38, bench). Fold `io` into `Live`.
- **L7. Find bar × IME composition** (mod.rs:377-379, search_bar.rs:16). Marked text is held twice. Cmd+F mid-composition drops `composition` (1266) without unmarking at the platform, so the IME's next commit lands in the find query. Use one input-target enum and unmark on switch.
- **L8. Hover and press fades keyed by string, not window** (theme/motion.rs:276-308, 399-451).
  - The same key painted in two windows overwrites `painted`/`hovered` across windows and triggers extra refreshes.
  - `FRAMES` is never pruned on window close.
  - Visual glitch only. Key by `(WindowId, key)`.
- **L9. Update check cache** (platform/update.rs:126-208). A 24 h on-disk cache (tmp file plus rename), plus `UpdateState{notice, dismissed}` in views/update.rs:16-19. The fixed tmp name can race an old and a new app during a relaunch (relaunch.rs:15-16). Leave.
- **L10. CLI install on-disk swap protocol** (platform/install.rs:403-463). Uses a staging dir, `.new-<pid>`/`.old-<pid>` names, and a lock file.
  - **Confirmed:** a crash between the plugin's move to `.old` (409) and the move of `.new` to live (420) leaves only the backup, and the next sweep deletes it (488-491). This is mostly self-healing, since the next install republishes the plugin.
  - A poisoned `INSTALLING` mutex (234) reports `Busy` forever.
  - Document; restore an `.old` plugin in the sweep when the live one is missing, and recover a poisoned guard with `into_inner`.

## Minor
- `Boot` (app.rs:939-945): explicit, apart from the invalid `InFlight(terminal)` noted in H3.
- Generation-counter debounces: `save_generation` (5986-6004), `Shell.session_generation` with a process-static `SAVED` (910-932), `flash_generation` (6266-6278). Fine.
- Shared status and banner slots: `status` holds startup, open errors, flashes and load progress, and only flashes are generation-guarded (6266). `banner` has about 20 writers. Cosmetic.
- Onboarding: `onboarding` plus `onboarding_wanted` (1550-1556; writers 2921, 3220, 4871, 4893). Fine.
- Modal slots: `overlay`, `pool_dialog`, `feedback`, `new_task`, `connect` (1661-1690) are independent `Option`s that can all be set at once; `modal_open` (4196) ORs them. A candidate for one `Modal` enum.
- Menu bar (menus/mod.rs:78-86, 224-255): the last `install` wins (main.rs:286, app.rs:2489 while `active`, views/launch.rs:777), and gpui bindings only accumulate. Leave; document.
- `window_drag::ARMED` (window_drag.rs:26-59): one thread-local flag for all windows. Documented and tested.
- Cursor blink, find refresh debounce, Cmd-link hover, focus mirror (terminal/mod.rs:1235-1384, 1833-1864): contained timers and flags.
- Scrollbar visibility (scrollbar.rs:220-307), `SlideTrack` (motion.rs:120-206) and `Folds` (fold.rs:95-153) are already explicit and tested, and can serve as models.
- Terminal settings: a zoom before the async config load lands is overwritten on screen (settings.rs:134-176). `SizeWriter` (183-248) coalesces correctly.
- Slider (controls.rs:532-689): `set_disabled` clears `dragging` without a commit, and an unmount mid-drag sends `Changed` without `Committed`.
- `WindowLook` (theme/window.rs:110-240): opacity is duplicated in `Theme.opacity`, and `APPLIED` mirrors AppKit and is forgotten at app.rs:892. Reduce Motion and Transparency are re-read only on activation (app.rs:2358-2360).
- `NEW_TAB` sentinel in the tab list (project_tabs.rs:39, 392). `remembered` view overlay is never cleared (projects.rs:62-64). `ControlWatcher` reconnects every 1 s with no backoff (refresh.rs:458-475). `desktop-windows.json` is written per pid with a process mutex (window_state.rs:377), so two app processes race.
- `relaunch.rs:11-26`: success means `open -n` spawned, not that the new app came up. `metal_blend`, `font_smoothing` and `scrollers` are one-time install-before-first-window steps. `drops::install` is guarded by a `OnceLock`.
- `source_gate.rs`, `test_env.rs`, `remote_fixture.rs`, `spike.rs`, `bench*`, `visual_*`: test or dev tooling, no production machines.

## Cross-slice leads
- **shelbi-app review machines.** `ReviewLoader` / `LoadStep` and `ReviewDecisions` are explicit machines the window drives (app.rs:6416-6508, 4736-4800). `ColumnSwap` / `Occupant` is the column push (4577-4610, 4688).
- **desktop-views.**
  - `views/review` `ReviewView` pending/shown/holding/finished.
  - `views/remote_session::{decide_with, host_links, open_remote_session_in}`.
  - `views/tab_tearoff.rs` and `views/tab_drag.rs` (M8).
  - `views/launch.rs:129-133, 457-466` (the hand-off in H3).
  - `views/cli_setup.rs:207-227`: its generation guard filters stale `State` events but not `RelaunchRequired` or `Detach`.
  - `views/session_view.rs:741, 813, 1942-1950` (production attach, supervision, `ResizeHold`).
  - `views/update.rs`.
- **shelbi-app / shelbi-client links.**
  - `shelbi_app::remote_window::reconnect_check` and `HostLink::{is_up, connect, allow_sign_in}`.
  - `RemoteSessionConnector::refusal`, a second holder of H2's "refused".
  - `shelbi_client::ControlClient::connect`: the hello has no read deadline, which is the root of H3's thread leak.
  - `shelbi_app::launch::{SharedDaemon, open_project}`.
- **shelbi-state.**
  - `ProjectLiveness::read`, `set_project_open`, `list_open_projects`: when the CLI and daemon write the open records relative to returning (M1).
  - `ensure_daemon_running_with` / `set_daemon_launcher`: are concurrent starts serialized (H3)?
  - `parse_orchestrator_title_state` (M6).
- **Daemon (cli-daemon slice).** N desktop subscriptions share one `client_id` with `ClientKind::Desktop` (control.rs:173-179, 202). How does the attached-client registry treat duplicates?
- **shelbi-app refresher.** `spawn_refresher` / `RefreshHandle` generation, `changes::spawn_all`, and the `connected` AtomicBool behind `ProjectLink::subscribe_changes` (refresh.rs:181-182, 302, 355). This is the only place stale snapshots are dropped.
- **shelbi-term.** `MouseGesture` selecting state (M5); `TerminalView` selection, search and scroll.
