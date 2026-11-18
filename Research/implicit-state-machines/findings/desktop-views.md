# Implicit state machines: desktop-views slice

**Slice:** `desktop/src/views/` in full: 96 files, about 69k lines, including `diff_view/`, `doc_editor/`, `overlay/` and `prereqs/`. Surveyed 2026-11-18 against `e8145b3f`.

**Findings:** 20 in total: **0 high**, **9 medium**, **11 low**, plus a Minor list.

- **Why nothing rates high.** Most heavy machines in this slice are already explicit: `Slot` in `SessionView`, `ColumnSwap`, `SlideOut`, `SidebarReveal`, `TabMenu`, `ActiveOverlay`, `BarRole`. Or they live in `shelbi-app` and the views only wrap them: Add Project wizard, onboarding, prereqs, ConnectFlow, machine check, Linear setup, diff `Mode`, `issue_edit`.
- **Where the risk is.** It sits in the desktop glue around those machines. The dominant pattern is async results that come back with a weaker guard than the state they land in:
  - a per-entity `generation` that doesn't change on hide or on a re-toggle;
  - no sequence on writes or reads;
  - one-shot `bool`s that get spent before their condition is met;
  - "loading" Options that have no failed state.
- **Two silent wrong-data cases stand out.**
  - Settings ▸ General writes to `config.yaml` with no ordering, and each write is a whole-block snapshot (finding 2).
  - The machine form can be cancelled while a save is running. The save still lands, but the owner believes nothing was written (finding 3).
- **Coverage.** Every file was walked by grep for flags, Options, status/mode/phase strings, timers and `cx.spawn` result handlers, and every candidate was then read in full.
  - Thin re-exports of `shelbi_app::settings`: `*_state.rs`, `workflow_graph.rs`.
  - Render-only (no state of their own): `sidebar.rs` (a renderer over props), `palette.rs`, `project_menu.rs`, `toast.rs`, `task_detail.rs`, `task_panel.rs`, `estimate_bar.rs`, `list_screen.rs`, `bootstrap.rs`, `harness_picker.rs`, `settings_nav.rs`, `terminal_preview.rs`, `chat.rs`, `mod.rs`, `diff_view/{body,text,canned,overlays}.rs`, `doc_editor/{layout,element}.rs`, `overlay/{error_log,review_confirm,review_reject,zen_intro}.rs`.
  - I personally checked the code behind each medium finding's main claim (file:line below).

## Medium

### 1. Board per-card mutation lifecycle (pending / held / notice)

- **Entity:** `BoardState` (`board_state.rs:339`): `pending: BTreeMap<String, Pending>` (:365), `held: BTreeMap<String, Vec<CommandKind>>` (:374), `notices` (:368). The `Pending` struct is at :248. The issue's write lane lives in shared `shelbi_app::issue_edit::SharedEdits`; the worker is `run_mutation` (`issues.rs:183`).
- **Encoding:**
  - 1: `acked: bool`, `landed: Option`, `prev: Option<Box<Pending>>`, and `to_status`/`to_slot`/`to_workflow` Options.
  - 4: settled by board-read revisions.
  - 5: send, then ack, then a read confirms.
  - 8: map insert/remove at many sites.
  - A sentinel key, `ADD_KEY = "\u{0}add"` (:337), with a fake empty `Pending` (:1838-1858).
- **States:**
  - Idle
  - Held (in `held` only)
  - Sent-unacked
  - Follow-up-sent (`prev=Some`)
  - Acked/landed-unknown (older daemon)
  - Acked/landed (waiting for a read)
  - Refused (reverted to `prev`, notice pinned)
  - Representable but invalid:
    - `acked=false` with `landed=Some`
    - `acked=true` with `prev=Some`
    - both `to_status` and `to_slot` set
    - an `ADD_KEY` entry with targets
- **Transitions:**
  - `begin_mutation` (:1806): holds a command at :1884, inserts the add entry at :1838, takes the previous entry as `prev` at :1918, inserts the new entry at :1934.
  - `finish_mutation` (:2003): ack at :2012-2019, revert at :2028, notice at :2043.
  - `settle_pending` (:610), called from `apply_reads` (:504).
  - `release_held` (:1771), called from `poll_saves` (:1700), `finish_save` (:1745) and `finish_mutation` (:2008).
  - `dismiss_notice` (:466).
  - Off-thread `record_mutation` (:1984).
  - Held commands re-enter through the window: `issues.rs:914` → `app.rs:4293` → `app.rs:3880` `run_issue_command` → `begin_mutation`.
- **Risk: medium.**
  - **The two "can I act on this card" predicates disagree.**
    - `change_in_progress` (:1977-1979) allows a follow-up once a pending entry is `acked && landed.is_some()`.
    - `begin_drag` (:2140) and `drop_target_status` (:2173) refuse any `pending` entry at all.
    - So until a read settles the entry, H/L and the property pickers move the card, while a drag or a Shift+J/K reorder (`reorder_key` :2109) says "a change is already in progress".
  - **Several held commands for one card: all but the first are refused.**
    - While the lane is busy, each command is pushed onto `held` (:1884). The `change_in_progress` check at :1871 runs first and passes, because no `pending` entry exists yet.
    - `release_held` returns all of them at once (:1775-1780). The first creates an unacked `pending` entry, so the second hits :1871 and is refused with only a status line.
    - The module doc promises held changes are "never refused" (:36-40). The tests only ever hold one command (around :2952, :3634, :3675, :3700).
  - **Older daemon (`Ok(None)` ack).**
    - The card stays in landed-unknown. That blocks follow-ups, including a command released at :2008.
    - The settle fallback compares `tf.task.column` against a status id (:631), while the pane uses `rules.resolved_status_id` (`shelbi-app board.rs:122`). Where these differ, the entry may never settle, and the card stays undraggable.
  - **Lost on teardown.** `pending` and `held` live only in the view. A project switch (`stop_workers`, `issues.rs:476`) flushes saves only, so held commands disappear.
  - **Past bugs:**
    - `dt-board-stale-after-own-mutation`: module doc :28-32, test around :2776.
    - The test near :3700 ("would be refused as 'in progress' and lost").
- **Recommendation: make explicit.**
  - Use a per-card enum `{Held(Vec<CommandKind>), Sent{prev}, Acked{landed: Option<ExpectedState>}}`, with one `blocks_follow_up()` predicate and one `blocks_drag()` predicate that every call site uses.
  - Release held commands one at a time: the next one goes out only after the previous one is acked.

### 2. Settings ▸ General save pipeline (unordered whole-block writes, stale per-window copy)

- **Entity:** `SettingsView` (`settings.rs:369`): `loaded: bool` (:445), `state: SaveStatus` (:448), `tools: GeneralForm` (:400).
- **Encoding:** 1, 4, 5, 8.
- **States:**
  - NotLoaded
  - Idle
  - Saving
  - Saved
  - Failed(msg)
  - Representable but invalid:
    - `loaded=false` + `Failed("Couldn't read config.yaml")` forever. After that, every `save()` silently returns (:1273), but tool picks still write through `GeneralForm`'s `Change::write` (:1585), which doesn't check `loaded`.
    - `state` duplicates `tools.status()`, copied at :1598.
- **Transitions:**
  - `loaded=true` in `fill` (:1001).
  - `state` is written at five sites:
    - :968 (load failed)
    - :1304 (Saving)
    - :1314-1319 (landed)
    - :1517 (browse refused)
    - :1570/:1598 (tool save)
  - Save triggers:
    - switch toggles (:942)
    - every valid hex-color keystroke (`color_edited`, around :1243)
    - slider commit (:1083)
    - `set_theme` (:1329) and `set_option_as_meta` (:1337)
    - FontPicker `Picked`
- **Risk: medium.**
  - **Writes can land out of order.**
    - Each `save()` snapshots the whole form (`values()` :1004) and spawns its own `update_user_config` on the background pool (`write`, :1302-1322).
    - There is no queue, no sequence and no in-flight guard. `shelbi_state::update_user_config` (`user_config.rs:821`) is read-modify-write with no lock.
    - Two quick saves can finish in either order (two switch flips, typing a hex color, a slider let go right after a theme pick). The older snapshot can be the last one to land.
    - `config.yaml` then holds the older values while the UI and `TerminalSettings` show the newer ones.
    - The status line shows whichever write landed last.
  - **Cross-window clobber.**
    - `apply` (:285, :295) rewrites the whole `desktop:` block from this view's copy.
    - The view only observes other windows' translucency, contrast, app icon and `TerminalSettings` (:504-560). It does not observe `theme`, `notifications`, `cli_path` or `option_as_meta`.
    - With Settings open in two windows, a save in A restores A's stale values over B's write.
  - **Past bug.** The comment at :443-444 ("so an early blur can't save blank values over it") records the bug the `loaded` flag was added for.
- **Recommendation: make explicit.**
  - One serial save queue per view: coalesce, latest snapshot wins. Or a write sequence number that drops stale completions.
  - Write only the changed keys, as `Change` already does for tools, instead of the whole block.
  - The lock-free `update_user_config` is a cross-slice lead.

### 3. Dialog close-while-busy: one rule per dialog, and a dropped save result

- **Entity:** five modal surfaces, each with a background job:
  - `MachineFormView` (`machine_form.rs:150`)
  - `ConnectDialog` (`connect_remote.rs:68`)
  - `AddProjectView` (`add_project.rs:276`)
  - `WorkspaceDialog` (`workspace_dialog.rs:84`)
  - `FeedbackDialog` (`feedback.rs:68`)
- **Encoding:** 1, 5, 7. Each dialog decides on its own whether "busy" blocks "close".
- **States:** Editing, Busy (save/connect/create/send in flight), Closed. Closed-while-Busy is reachable in three of the five, and the job's result is then thrown away.
- **Transitions:**
  - Refuse close while busy:
    - `FeedbackDialog::dismiss` (`feedback.rs:206-209`)
    - `WorkspaceDialog::dismiss` (`workspace_dialog.rs:308`)
  - Allow close while busy:
    - `MachineFormView`: Cancel and Esc map to `Action::Cancel` with no `saving` check (`crates/shelbi-app/src/settings/machine_form.rs:513-540`). The Cancel button stays enabled during a save (`machine_form.rs:705`; only Submit is disabled, :707). `act` emits `Cancelled` (:327-331) while the save keeps running on the background executor (`save`, :368).
    - `ConnectDialog::dismiss` emits `Closed(None)` unconditionally (`connect_remote.rs:138-141`). The app's ⌘W routes to it (app.rs:4186-4190).
    - `AddProjectView`: ⌘W calls `window.remove_window()` unconditionally (`add_project.rs:1569`). Esc does wait while the engine runs (module doc :57-60).
- **Risk: medium.**
  - **Machine form.**
    - The owner receives `Cancelled`, so `close_machine_form(saved=false)` skips the config re-read and the `PoolChanged` refresh (`settings.rs:746-747`).
    - The page and sidebar don't show the machine that was just written, and the verified-binary record (written after `finish_save`, `machine_form.rs:376-382`) is never written.
    - The event's doc says "nothing was written" (:143-144), which is false here.
    - Re-adding the same name is probably refused as a duplicate (inferred).
  - **Connect dialog.**
    - A cancelled connect still runs `launch::connect_remote`, which calls `recents.remember` (`shelbi-app launch.rs:302-312`), so the host appears in Recent remote sessions.
    - A cancelled "start daemon" still starts the far daemon (`connect_remote.rs:193`).
  - **Add Project.**
    - Closing mid-create is documented and tested: `closing_mid_creation_drops_its_reports_and_opens_nothing` (:2185).
    - It leaves a half-created project. A fresh wizard sends `Attempt::New`, not `Retry` (`add_project_wizard.rs:1085` vs `:1108`).
- **Recommendation: make explicit.**
  - Pick one rule for all five dialogs: close refused while busy, or close detaches the job and the owner still gets its outcome.
  - Fix the machine form first: disable Cancel and Esc while `saving()`, or emit `Saved` from the detached save.
  - The TUI machine form shares the shared-form gap.

### 4. Review and workspace async daemon ops: errors ignore visibility; the user-shell toggle has no attempt token

- **Entities:**
  - `ReviewView` (`review.rs:336`, `Shown` :308): `shown`, `review: Option<Shown>` + `resolved`, `hold` + `held`, `content_wanted`, `refocus`, `focus`, `generation`, `shell_open` + `shell_starting` (:357-359).
  - `WorkspaceView` (`workspace.rs:242`), which has the same shell pair plus `details: Option` + `details_pending` + `generation`.
- **Encoding:** 1, 4 (mirrors daemon shell/editor sessions), 5, 7, 8 (explicit `ReviewFocus`/`WsFocus` plus side flags).
- **States:**
  - Review: Hidden / Shown-Placeholder (`resolved=false`) / Shown-Resolved, crossed with Holding (attach parked in `held`) and ContentWanted.
  - Shell (both views): Closed (F,F), Starting (T,T), Open (T,F). (F,T) is representable but invalid.
  - Workspace details: Idle / Fetching / Loaded / Loaded-for-another-task (representable; read as "none" by `current_details()`, `workspace.rs:604-609`). Failed is missing.
  - Representable-invalid combos:
    - `hold=true` while hidden: `hide()` clears `held` but not `hold` (`review.rs:606-612`, `workspace.rs:440`).
    - No "resolve failed" state for a placeholder.
- **Transitions:**
  - Review:
    - `show_pending` :473-515, `show` :536-563, `hide` :606-625, `hold_content`/`release_content` :445-459 (driven from app.rs:3871, 4531, 4555, 4586-4592)
    - `select_view` :882-920, `attach` :1117-1126
    - `generation += 1` only at :493 and :548
    - shell: `open_shell` :1336-1359, `close_shell` :1361-1375, the shell's `Ended` :395-399
  - Workspace:
    - shell: `open_shell` :1043-1066, `close_shell` :1068-1080, `Ended` :330-334, `show` :381, `hide` :442
    - details: `request_details` :702-727, triggered from `set_row` :592-594 and `tools_changed` :687-690
    - `session_op` :741-765
- **Risk: medium.**
  - **A failed Ensure yanks the view.**
    - In `select_view`, the `Ok` branch checks `still = active == view && shown` (`review.rs:908`), but the `Err` branch always calls `select_view(ActiveView::Chat)` (:912-917).
    - `run_op` drops results only on a `generation` change (:1148), and `hide()` doesn't bump it.
    - So an Editor/Diff Ensure that fails after the user hid the review, or moved to another view, re-attaches Chat: `attach` → `content.show_target` on a hidden view, unless `hold` is set. It also pulls the user back.
  - **The shell toggle races, identically in both views.**
    - Callbacks are guarded only by `this.shell_open` and `generation`. The ops (EnsureShell, CloseShell, EnsureShell) run unordered on the background pool.
    - On → off → on quickly:
      - The first EnsureShell's `Err` can land after the reopen. It sets `shell_open=false` with an error line (`workspace.rs:1060-1062`, `review.rs:1352-1355`), and the second attempt's success then returns early, leaving a daemon shell nobody attaches.
      - Or CloseShell reaches the daemon after the second EnsureShell. It kills the reopened shell; `OnDeath::Close` emits `Ended`, and the split closes unasked.
  - **Workspace details refresh can be lost.**
    - `request_details` drops a request while one is in flight (`workspace.rs:707-709`). Its doc relies on "the next refresh", but `update_row` returns early when the row is unchanged (:448).
    - A task change during an in-flight read therefore leaves the stale result (:724) in place, and the panel shows no branch/PR rows until another row change.
    - A failed read only logs `tracing::warn` (:725).
  - **Test gaps.** The tests cover the happy paths (`a_failed_attach_is_retried_by_selecting_it_again`, `an_editor_the_daemon_refuses_falls_back_to_chat`, `switching_away_and_back_mid_load_adds_no_ensure`), but not a failure arriving after hide or after a view switch.
- **Recommendation: make explicit.**
  - A `ReviewPhase {Hidden, Placeholder, Resolved}` plus an attach gate `{Free, Holding(Option<SessionRef>)}`.
  - A visit token bumped on show/hide/view-select and checked by every callback, including the `Err` branch.
  - A shared `ShellToggle {Closed, Starting(attempt), Open}` used by both views, with the shell ops serialized per slot.
  - A "dirty while pending" re-request for details.

### 5. Settings pages and editors reloading from disk (one-shot flags, unordered reads, hidden read errors)

- **Entities:**
  - `WorkflowsPage` (`workflows.rs:131`; `reload` :146, `reload_on_return` :148)
  - `AgentsPage` (`agents.rs:77`; :92, :95)
  - `PropertiesPage` (`properties.rs:69`; `reload` :78)
  - `WorkflowEditor` (`workflow_editor.rs:222`; `state: Option` :228, `load_error: Option` :229, `reload_on_return` :263)
  - `AgentEditor` (`agent_editor.rs:108`; :114, :115, :126)
  - `WorkspacesPage` (`workspaces.rs:100`; `reload` :114)
- **Encoding:** 1, 3 (config files on disk or on the far hub), 4, 6 (window activation as the trigger).
- **States:**
  - Page: InSync / ReloadOnNextDraw / ReloadWhenWindowActive / ReadInFlight.
  - Editor: Loading (None,None) / LoadFailed (None,Some) / Editing (Some,None). (Some,Some) is representable and does happen.
- **Transitions:**
  - `reload=true`:
    - `reload_when_drawn` (`workflows.rs:221`, `agents.rs:157`, `properties.rs:172`, `workspaces.rs:170`)
    - callers: `settings.rs:850`, :868-881, :891-899, :926-934
    - consumed by `mem::take` in render (`workflows.rs:1423`, `agents.rs:948`, `properties.rs:426`, `workspaces.rs:855`)
  - `reload_on_return=true`:
    - set at `workflows.rs:433`, :491, `agents.rs:308`, `workflow_editor.rs:663`, `agent_editor.rs:448`
    - consumed by the first `observe_window_activation` (`workflows.rs:171`, `agents.rs:110`, `workflow_editor.rs:300`, `agent_editor.rs:167`)
  - Editor `read_again`: `workflow_editor.rs:630-650`, `agent_editor.rs:~419-430`.
- **Risk: medium.**
  - **The flag is spent before the edit.**
    - "Edit YAML" arms `reload_on_return`. The user switches back to Shelbi before saving in the editor, then saves: nothing reloads.
    - The model stays pre-edit, and the next save is refused by the shared "files changed on disk" check.
    - For the far editor, focus moves between two Shelbi windows, so the flag is spent as soon as the main window is clicked.
  - **Dropped reloads are never retried.** `read_again` returns early when the draft is dirty or busy (`workflow_editor.rs:636-638`), and the flag is already gone.
  - **Read errors are invisible.**
    - `read_again`'s `Err` sets `load_error` (`workflow_editor.rs:647`), but render shows `load_error` only when `state` is `None` (:1440-1446).
    - Break the YAML by hand and return: no error is shown, and the draft silently stays pre-edit. The agent editor has the same problem (`agent_editor.rs:430`, :761).
  - **A stale read can revert a fresh save.**
    - Page loads are dropped only if a save is in flight when they land (`workflows.rs:205`, `agents.rs:142`; the shared `loaded()` checks `saving`).
    - A read started before a save that lands after the save completes replaces the committed model with the pre-save config.
    - `WorkspacesState::loaded` checks only the project slug (`shelbi-app settings/workspaces.rs:369-371`). Overlapping reads (page re-shown, then the pool dialog closes) can land old-last, re-emitting `WorkspacesEvent::Changed` (`workspaces.rs:161-162`) with the pre-edit pool.
  - **Copies drift.** `PropertiesPage` and `WorkflowsPage` each hold their own `WorkflowConfig`, reloaded independently.
- **Recommendation: make explicit.**
  - Add a load generation (or file mtime stamp) to the shared `loaded()` and ignore completions older than the last commit.
  - Replace the one-shot `bool` with "reload on every activation" (cheap), or an armed state that re-arms until the file's mtime changes.
  - Make the editor load an enum `Loading | Failed(String) | Editing(EditorState)` and route `read_again` errors into a visible banner.

### 6. External-editor watcher for an issue body

- **Entity:** the watcher thread from `external_edit::open`/`run` (`external_edit.rs:213`, :224), driven by `IssuesView::open_in_editor` (`issues.rs:1194-1204`) and drained at `issues.rs:740-750`.
- **Encoding:** 3 (scratch file `$TMPDIR/shelbi-desktop/<project>--<id>.md`, `scratch_path` :191), 4 (editor child liveness, file mtime), 6 (500 ms poll, `WATCH_LIMIT` 4 h at :25/:239).
- **States:**
  - Per (project, id): Launching / Watching(child) / Watching(time-limited) / Ended (silent) / Failed (launch only).
  - Two watchers on one scratch file is representable and reachable.
- **Transitions:**
  - `open` spawns a thread on every call, with no registry.
  - `run` writes the file (:228), spawns the editor (:230), then polls mtime and body (:234-260).
  - It exits on child exit, after the time limit, or when `tx.send` fails (only checked on a save).
- **Risk: medium.**
  - **Double submit.**
    - Nothing stops a second "Edit in editor" on the same issue.
    - The second call rewrites the scratch file under the first editor and starts a second watcher with its own `last_body`.
    - Each later save is then sent as `EditIssue` twice.
  - **No end signal.** There is no `Ended` event. The status "saves are sent as you write" (`issues.rs:1199`) never clears, and the UI can't tell whether a watcher is alive.
  - **App launches are watched blind.** `Launch::App` returns `Ok(None)` (:126-139), so GUI-app editors get the blind 4 h watch. The module doc implies only Terminal-hosted editors do (:16-18).
  - **Remote edits are overwritten.** An edit made elsewhere is overwritten by the next local save, which goes out as the whole body.
  - **Leftovers.** The scratch file is never removed.
- **Recommendation: make explicit.**
  - A per-(project, id) session registry: refuse or refocus a second open.
  - An `Ended` event so the status clears.
  - Consider a base-body check before submitting.

### 7. SessionView attach / restart / reconnect lifecycle (explicit enum, leaky edges)

- **Entity:** `SessionView` (`session_view.rs:262`), `Slot` (:218), projected as `SessionPhase` (:248).
- **Encoding:** 8, 6, 4 (PTY/session liveness, relay link).
- **States:**
  - `Slot::{Empty, Connecting{restart_attempt}, Live, Idle, Failed, AwaitingRestart, GaveUp, Reconnecting}`.
  - Parallel state that also decides behaviour:
    - `shown` and `reopen_on_show`
    - `RestartController` (crash budget, `deliberate_close`) and `ReconnectController` (attempt, backoff) in shelbi-app
    - `generation`, `dead_pane_focus`
    - one shared `_tick` for both the restart backoff and the reconnect wait
    - `drop_note` + `drop_note_generation`
  - `Slot::Reconnecting` carries no payload; its attempt lives in the controller.
- **Transitions:**
  - `clear` :598-605
  - `start_connect` :615-701 (keeps Reconnecting if `reconnect.is_reconnecting()`, :624-626)
  - `connected` :704-788
  - `link_lost` :835-848, `live_pane_died` :853-859, `died` :863-873
  - `act_on_death` :876-922
  - `schedule_tick` :925-939, `schedule_reconnect` :942-965
  - entry points `show`/`show_target`/`hide`/`reconnect`/`retry_if_idle` :505-583
  - external callers in app.rs (:5760-5782 deliberate close), review.rs, workspace.rs, pane_session.rs, remote_session.rs
- **Risk: medium.**
  - **"Reconnecting" is decided in two places.** `start_connect` and `connected` branch on the controller, while `show` branches on the slot (:507-510). The two must move together by convention.
  - **`_tick` is shared by two timers.** `schedule_tick` bails if `_tick.is_some()` (:926); `schedule_reconnect` overwrites it (:950). Each callback re-checks the slot, so there is no live bug, but this depends on `start_connect` clearing `_tick` first (:619).
  - **Manual reconnect resets the crash budget.** `reconnect()` calls `restart.begin(false)` (:568), which silently resets the crash-loop budget of a GaveUp pane.
  - **Past bugs:**
    - `rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched` (:16-19)
    - ENOTCONN zombie-sibling and the 5 s handshake (`review.rs:34-37`)
    - the reconnect tests :1991-2168
  - **Healthy part.** The generation guard is tested (`reconnect_replaces_an_in_flight_connect`, `a_result_for_a_hidden_view_is_dropped`).
- **Recommendation: make explicit at the edges only.** Move the attempt into `Slot::Reconnecting{..}` (or derive the controller from it), split `_tick` into two, and write the transition table into the module doc. Don't redesign it.

### 8. Task pane lifecycle (open on board / floating over another screen / sliding out / editing)

- **Entities:**
  - `BoardState`: `pane: Option<String>` (`board_state.rs:357`), `closed: Option<String>` (:360, a one-shot hand-off), `mode: Mode::Detail{id, editing}` (:67-72), `pane_focus` (:364).
  - `IssuesView` (`issues.rs:210`): `floating: bool` (:273), `pane_closing: SlideOut` (:281), `pane_editors` (:258), `pane_session` (:276), `needs_focus` (:226).
  - Mirrored in the shell as `AppView.detail_over` (app.rs:5114-5169).
- **Encoding:** 1, 5, 7.
- **States:**
  - Closed
  - Open-on-board
  - Open-floating
  - Closed-but-floating (until the shell's `sync_detail_over` calls `land`)
  - Sliding-out
  - each open state × `editing`
  - Representable but invalid:
    - `Mode::Detail{id}` ≠ `pane`
    - `Mode::Detail` with `pane=None`
    - `pane_editors.id` ≠ `pane` (repaired lazily at `issues.rs:777-780`, :1054-1058)
    - `closed` set while `pane` is set
- **Transitions:**
  - `BoardState`: `open_detail` :713, `close_overlay` :731, `close_pane` :739, `take_closed` :750, `rest` :756, `begin_edit` :1591, `editor_focused` :1623, `editor_blurred` :1639, `end_edit` :1670.
  - `IssuesView`: `open_task` :576, `open_task_over` :589, `land` :620 (discards `closed` :627-629), `note_pane_close` :649 (called from `sync_session` on every render, :409), `close_pane` :672.
  - Shell: app.rs:5114, 5128, 5156.
- **Risk: medium (mostly keyboard/visual).**
  - **"Floating" has two owners** (`floating` and `detail_over`), synced by the shell polling. If they diverge, the detail layer stays over another screen, or keys aren't handed back.
  - **`editing` mirrors gpui editor focus.** The comment at `issues.rs:1144-1146` notes that focus can land before the blur does. A stuck `editing=true` makes `handle_key` return `None` for everything but Esc (:1281-1287), so the board's keys go dead until Esc.
  - **The pane id is stored twice.**
- **Recommendation: document; better, derive `Mode::Detail` from `pane` (keep only `editing`) and own "floating" in one place.**

### 9. Error-log overlay: no failed state, and errors marked read anyway

- **Entity:** `ActiveOverlay::ErrorLog { viewer: Option<Viewer> }` (`overlay/mod.rs:65-73`), opened at `None` by `ActiveOverlay::error_log` (:99-105). Its writer is in the shell (app.rs:6348-6374).
- **Encoding:** 1, 4, 7.
- **States:**
  - Loading (`None`)
  - Loaded
  - Loaded-empty
  - **Failed is missing**
- **Transitions:**
  - The background read (app.rs:6353-6360) fills the viewer through the `this.overlay` match (:6365-6368).
  - Key handling (`overlay/mod.rs:196-222`) and Copy/Clear (`error_log.rs:29-37`, :123-133) branch on None/Some.
- **Risk: medium.**
  - The read does `handle.source.errors().unwrap_or_default()`, then writes `StateWrite::MarkErrorsRead` unconditionally (app.rs:6357-6358).
  - A failed read (a far daemon hiccup, a corrupt errors file) therefore renders the "no errors" empty state (`error_log.rs:52-56`) and clears the unread badge. The user loses the signal without ever seeing the errors.
  - Separately, `ActiveOverlay` is mirrored into `ClientState` as `nav::Overlay` (`overlay/mod.rs:160-172`), and `self.overlay =` is assigned at app.rs:3212, 6013, 6206: two copies kept in step by the shell.
- **Recommendation: make explicit.**
  - Make the viewer `Loading | Loaded(Viewer) | Failed(String)`.
  - Mark errors read only after a successful read.

## Low

### 10. Prereqs view: desktop-only flags on top of the shared `Prereqs` model

- **Entity:** `PrereqsView` (`prereqs.rs:158`): `skip_dialog`, `validating`, `auto_continue`, `save_error`, `copied: Option<(String,u64)>`, `clicks` (:167-173).
- **Encoding:** 1, 4 (tool presence on PATH/disk), 6 (copy timer), 7.
- **States:**
  - Phase (from the model): Checking / Missing / Ready
  - Locate panel open for a tool, crossed with `validating`
  - skip dialog
  - auto-continue armed or not
  - Invalid but representable:
    - `validating` with the panel closed or open for another tool
    - `save_error` with no panel
- **Transitions:**
  - `check_again` :253-255
  - `checked` :283-292
  - `copy` :304-319
  - `start_locate` :323-325
  - `validate` :359-363, result :378-380
  - skip/back :390-401
  - Esc :418
- **Risk: low.**
  - **`validating` is one view-wide bool.** While one tool validates, a Locate panel opened for another tool shows the spinner (:652) and silently ignores Return/Browse (guard at :360).
  - **Stale results land on a reopened panel.** `Prereqs::located` matches only the tool (`shelbi-app prereqs.rs:563-575`), so a stale result can mark a reopened panel Found.
  - **The path is saved after cancel.** The save runs off-thread (:370) even after a cancel or re-check.
  - **`leave` has no latch** (:296-302). `CliSetupView` has `advancing` for the same case.
- **Recommendation: make explicit in the shared model.** A Locate slot `{tool, Validating{run} | Result}`, with `validating` derived from it.

### 11. Job and reminder sheet cancel result applied to whichever sheet is open

- **Entities:**
  - `JobSheet` (`jobs.rs:156`): `canceling: bool` (:162), `cancel_error: Option` (:164).
  - Reminders use `shelbi_app::reminders::SheetState` the same way (`reminders.rs:227-247`).
- **Encoding:** 1, 5.
- **States:** idle / canceling / failed. `canceling` together with `cancel_error` is representable but invalid.
- **Transitions:**
  - `cancel` sets :379-380
  - the result clears :386-388
  - `open_sheet` (:300) resets
  - reminders: `reminders.rs:236-237`
- **Risk: low.**
  - **No id check on the result.** The answer is applied to `view.sheet.as_mut()` with no id check (:386; `reminders.rs:236`). Cancel job A, open job B, and A's failure shows on B. `sheet_loaded` does check the id (:331).
  - **Success never clears `canceling` for jobs.** "Canceling…" stays, with Cancel disabled (:640), while the job keeps running after an acknowledged cancel.
- **Recommendation: make explicit.**
  - Key the result by sheet id.
  - Move the job sheet's cancel state into `shelbi_app::jobs` next to `SheetState` (the parity rule).

### 12. Machines view: one failed read blanks the page

- **Entity:** `MachinesView` (`machines.rs:83`): `last_refresh: Instant`, a 100 ms `_poll` (:52, :108-113), a 1.5 s re-read (:50, :146). `MachinesState` holds `loaded: bool` + `error: Option` (`machines_state.rs:28-30`).
- **Encoding:** 1, 4, 6, 7.
- **States:** Loading / Error / Empty / Loaded (`machines.rs:202-235`).
- **Transitions:** `MachinesState::apply` (:48-68), `set_reachability`, `tick`/`refresh` (`machines.rs:137-157`).
- **Risk: low.**
  - A transient read error (a config mid-rewrite) wipes the model (`machines_state.rs:57`) into "Couldn't read the project" until the next good read.
  - The Workspaces page keeps its rows and shows the error beside them, so the two pages disagree.
- **Recommendation: document; keep the last good model on a re-read failure.**

### 13. DiffView file-list keyboard focus mirror

- **Entity:** `DiffView.list_focus` (`diff_view/mod.rs:207`), a local mirror of `DiffState::file_list_open()`.
- **Encoding:** 1, 4.
- **States:** list closed / list open with rows focused / list open with list focused. `list_focus=true` with the list closed is representable but invalid.
- **Transitions:**
  - `key` re-syncs only when a key changed the list (:350-356)
  - `list_key` Leave (:390)
  - `body_down` (:457)
  - `file_clicked` (:501)
- **Risk: low.**
  - `palette_clicked` (:518-525) runs the palette's choice with no re-sync. The Commands palette can apply `DiffAction::ToggleFileList` (`shelbi-app diff/state.rs:1215`).
  - A mouse-chosen toggle therefore closes the list while `list_focus` stays true.
  - `list_key_action` (:368-379) doesn't check `file_list_open()`, so j/k step files, Space toggles viewed, and Enter/Esc are swallowed with no list on screen.
- **Recommendation: derive.** Gate `list_key_action` on `file_list_open()`, or re-sync after every `client_effects`.

### 14. Activity feed: read and paging lifecycle, wiped on a failed read

- **Entities:** `ActivityState` (`activity_state.rs:141`) and `ActivityReader` (`activity_reader.rs:36`).
- **Encoding:** 1, 4, 6 (750 ms cadence, `activity.rs:151-156`).
- **States:** NotLoaded / Loaded / Loaded-with-error / LoadingOlder (inferred: `data.window < self.window`, :305-307).
- **Transitions:**
  - `adopt` (:203-231)
  - `load_older` (:316-322)
  - `filter_changed`/`rebuild`/`install` (:262-283)
  - the reader thread (`activity_reader.rs:66-84`)
- **Duplicated state:**
  - The window lives both in state and in the reader's `AtomicUsize` (`activity_reader.rs:40`), synced at `activity.rs:139`, :206-209.
  - The filter is synced every render (:652). Drift is handled by a rebuild (test `a_read_built_under_an_older_filter_is_rebuilt_under_the_shown_one`).
- **Risk: low.**
  - `ActivityData::failed` (shelbi-app `activity.rs:401-406`) has no events and `has_older=false`, and `adopt` replaces the data wholesale (:213).
  - One transient failure on the cadence therefore empties the feed, drops the selection, hides "Load older", and silently ends a load-older that was in flight.
- **Recommendation: document; keep the last good events and show the error beside them (a shelbi-app change).**

### 15. Launch screen: no Loading state; `busy` keyed by row

- **Entity:** `LaunchView` (`launch.rs:289`): `model`, `busy` (a row index), `dialog: Option`, load `generation`.
- **Encoding:** 1, 7.
- **States:** Idle / Busy(row) / Error / Dialog-open. Loading is missing: `LaunchModel::default()` reads as `LaunchState::NoProjects` (shelbi-app `launch.rs:146-152`).
- **Transitions:**
  - `reload` :381-395
  - `open_connect` and the dialog subscription :398-417
  - `perform` :425
  - `open_project` :444-466
  - `reconnect` :478-490
  - `connected` :504-507
- **Risk: low.**
  - **Wrong first frame.** Before the first read lands, the screen draws "No projects yet" with Add project as the primary button.
  - **Spinner can move rows.** `busy` is a row index, so a reload while busy can put the spinner on another row.
  - **Outcomes ignored.** `Ok(_) => {}` (:463) silently ignores open outcomes other than `OpenProject`.
- **Recommendation: make explicit.** Use `Option<LaunchModel>` (or `loaded`) and key `busy` by action.

### 16. Tab reorder and tear-off drag state

- **Entities:**
  - `TabReorder` (`tab_drag.rs:210`): `project`, `slot`, `settle`, `hosting`, `incoming`, `away`.
  - `DragState` in the `TabDragSession` global (`tab_tearoff.rs:174`, :274): `float: Option`, `float_native: Option`, `float_opening: bool`, `over: Option<WindowId>`.
- **Encoding:** 1, 4 (gpui `has_active_drag`), 5, 7, 8.
- **States:**
  - Reorder: Idle / OwnDrag{home, gap} / OwnDrag-away / Hosting-own / Hosting-incoming / Settling.
  - Ghost: Closed / Opening / Open.
  - Representable but invalid:
    - `away` with `project=None`
    - `incoming` without `hosting`
    - `incoming` together with `project` (forbidden only by `host()`, :247-249)
    - `float_opening` with `float` set
- **Transitions:**
  - `TabReorder`: `start` :232-239, `host` :244-252, `unhost`/`cancel` :256-274, `follow` :290-311, `accept` :326-340, `over` :344-350, `drop` :372-384, `frame` :395.
  - Ghost: `begin` :300, `end` :309, `close_float` :317, `sync_float` :372-419.
  - All driven from the shell: `app_tab_tearoff.rs:67-314`, app.rs:7513.
- **Risk: low.**
  - **Stranded state is documented:** "A drag that ends off the bar never hears its drop… the next drag's start resets it" (`tab_drag.rs:205-208`). The shell compensates with a per-frame `has_active_drag && ghost_alive` reconciliation; app.rs:7507-7512 notes "a drag gpui ended unheard can't leave a gap open here", i.e. a past bug.
  - **A late deferred open can adopt the wrong drag.** The ghost's deferred open identifies its drag only by source `WindowId` (:383, :414).
  - **The session global outlives gpui's drag.** If gpui ends a drag unheard, the session and its floating window stay until the next `begin` or Esc.
  - **Tests:** `a_new_drag_forgets_the_last_ones_gap`, `a_cancelled_drag_slides_the_tabs_back`.
- **Recommendation: document now.** If touched again, use an enum `{Idle, Own{project, slot, away}, Hosting{tab, slot}}` plus `Ghost{Closed, Opening(drag_id), Open(..)}`, and end the session whenever `has_active_drag` goes false.

### 17. State recovered from shared display strings

- **Encoding:** 2, 4.
- **Instances:**
  - **Board freshness.** `board_warning` keeps the banner only if the text `contains("stale") || contains("no daemon")` (`sidebar.rs:1544-1546`). The typed `BoardFreshness{stale, source}` exists, and its wording is built in `shelbi-state board_index.rs:748-754`.
  - **Section from labels.** `Section::of_label` matches the row labels "Ready for Review", "Queued for Review" and "Workspaces" (`sidebar.rs:141-148`).
  - **Activity relabeling.** It keys on `pill.text == "ready for review"` (`activity_labels.rs:145`), verb "event" + chip "daemon" (:192-195), "detached" + Gray tone (:198-200), and actors "zen"/"you" (:163-166).
  - **Task status from events.** `Labels.statuses` mirrors each task's status only from transitions inside the loaded window (:71-78).
- **Risk: low.** A wording change in shelbi-state or shelbi-app silently drops the stale-board warning, un-sections rows, or stops relabeling. Only the board-warning test (`sidebar.rs:2214-2236`) pins the strings.
- **Recommendation: make explicit.** Typed `stale`/`no_daemon` on `SidebarModel` and a kind on shared rows.

### 18. RemoteSessionView connection indicator

- **Entity:** `RemoteSessionView::status` (`remote_session.rs:345-355`), `Connection` (:365). It is a lossy projection of the `SessionView` slot.
- **Encoding:** 4, 8.
- **States:** Connecting / Connected / Reconnecting. `Failed`, `GaveUp`, `Empty` and `Idle` all collapse into Connecting.
- **Risk: low.**
  - A first-open failure (expected: "No CLI: the window says so when its session can't start", :197) shows the failure notice under a "Connected to user@host" title (shelbi-app `launch/terminal.rs:44-50`) with a grey "connecting" dot.
  - There is no retry gesture: `show` runs only in `new` (:309).
- **Recommendation: add a `Failed` arm with its own label.**

### 19. Optimistic board settings vs read-back

- **Entity:** `BoardState` filters, `input.workspace_filter` and `input.column_overrides`.
- **Encoding:** 4, 5.
- **States:** local → persisting → confirmed or reverted by a read.
- **Transitions:**
  - Written optimistically at `board_state.rs:1135-1145`, :1250.
  - Persisted off-thread by `IssuesView::persist` (`issues.rs:969`).
  - Overwritten by reads at `board_state.rs:497` via `BoardInput::apply_open` (`shelbi-app board.rs:1050-1057`).
  - Fingerprint dedupe at `issues.rs:2336`; `Done::Persisted` clears it at :730-736.
- **Risk: low.** Nothing records "persist in flight". A read started before the write but folded after it briefly flips a filter or column collapse back. The effect is cosmetic.
- **Recommendation: leave, or hold the local value until a read reflects it.**

### 20. CLI setup: bootstrap enum plus side flags

- **Entity:** `CliSetupView` (`cli_setup.rs:168`): `state: BootstrapState`, `installing` (:171), `run` (:173), `advancing` (:175).
- **Encoding:** 8.
- **States:** the `BootstrapState` states, crossed with `installing` (which changes the label and bar), crossed with `advancing` (a latch that never resets).
- **Transitions:**
  - `start` :208-211 (bumps `run`)
  - `apply` :236-246 (drops other runs, sets `installing`, advances on Ready)
  - `advance` :258-261
- **Risk: low (theoretical).**
  - A Failed state arriving during the 700 ms Ready dwell would still be replaced by the deferred `next(...)`. Today `Bootstrap` sends nothing after Ready.
  - Old runs' `RelaunchRequired`/`Detach` listeners aren't covered by the `run` guard.
- **Recommendation: leave; write the "nothing after Ready" assumption next to `advance`.**

## Minor

- `ColumnSwap` (`column_swap.rs:150`): already an explicit, clock-injected machine (`show`/`settle`/`finish`/`pace`). It is a healthy model.
- `SlideOut<T>` and `PaneFrame.resize` (`floating_pane.rs:194`, :125): explicit, with a close counter guarding the stale timer (:236).
- `SidebarReveal` (`sidebar_reveal.rs:49`), `TabMenu` (`tab_bar.rs:287`), `Morph` (`tab_tearoff.rs:125`), `Slide` (`tab_drag.rs:161`): encapsulated, clock-injected.
- `TabLayout.ghost: Option<WeakEntity>` (`tab_drag.rs:77`, :102) doubles as "this is a tab drag". Clever, but documented.
- `ActiveOverlay` (`overlay/mod.rs:65`): an explicit enum; the per-overlay machines are in shelbi-app.
- `WorkspaceView`/`ReviewView` focus handoff: one-shot `content_wanted`/`refocus` (`workspace.rs:262-275`, consumed :1578-1583). Worst case is a misdirected focus.
- `PaneSession` (`pane_session.rs:52`): a desired-state reconciliation in one `sync` (:142-180).
- `OrchestratorPage.reload` (`orchestrator.rs:46`): load-on-next-draw. Saves and restarts are guarded in `OrchestratorForm`.
- `ShellSplit.dragging` (`workspace_layout.rs:84`), `PaneSplit`, `PaneWidth`, `Split`: a drag bool and clamp math.
- `SessionView.drop_note` + `drop_note_generation` (`session_view.rs:265-266`): a timed toast with a correct guard.
- `BoardState.dragging` (`board_state.rs:379`): reconciled by polling `has_active_drag` (`issues.rs:758`, :2204).
- `BoardState.mode` pickers (`board_state.rs:1039-1063`, :1434-1520): an enum rewritten at about 30 sites, but each arm is local and `rest()` (:756) is the single exit.
- `BoardState.status` used as a "loading more" flag (compared to `LOADING_MORE_STATUS`, :517, :524): another status can overwrite it early. Harmless; test near :3191.
- `pre_search_selection`, `needs_focus` (Issues/Reminders/Jobs/Changelog/Activity), `detail_cache`, `open_print`/`closed_print`: one-shot or cache flags.
- `ChangelogView.loading_more`/`reveal` (`changelog.rs:176-179`): a one-frame guard, and a reveal that lingers harmlessly if its id never appears.
- `NewTaskDialog.keyboard`/`text_focus` (`new_task.rs:90`): a focus-ring flag; the form machine is in shelbi-app.
- `AddProjectView` `keyboard`/`menu`/`linear.menu`/`run` (`add_project.rs:284-297`): `run` correctly drops older reports (:629, :723, :743).
- `OpenWizard` and About `Windows` globals (`add_project.rs:205`, `about.rs:40`): single-instance registries that self-heal on a stale handle.
- `OnboardingView` (`onboarding.rs:65`), `FeedbackDialog`, `ConnectDialog` (apart from finding 3): thin wrappers over shared guarded machines.
- `WorkspaceDialog.running` (`workspace_dialog.rs:100`, :409): duplicates the shared form's `busy`. Harmless today.
- `MachineFormView` check machinery (`machine_form.rs:163`, :393-432): a run counter plus `CheckStop`. Fine.
- `ToolSelect` (`tool_select.rs:54-62`): an open/highlight dropdown. Its `step` clamp would panic on 0 entries, but there are always at least 3.
- `UpdateState` (`update.rs:16`): `notice` + `dismissed`. `spawn_check` resets the global, which is safe only because it is called once per launch.
- `markdown.rs` `FigureState` (:126): worked out each render. `Missing` is fixed at parse time, so an image added later shows only after a re-parse.
- `bundled_software.rs`: a shared `Browser` plus a lazy `texts` cache.
- `FontPicker` (`font_picker.rs:40`), `PropertiesPage.workflow_menu`, `WorkflowsPage.menu`/`AgentsPage.menu`, `TrackerSection`/`LinearUi` `menu: Option<Pick>`: popover flags. The Linear connection state is shared `TrackerForm`/`LinearChoice`.
- `KeybindingsPage` (`keybindings.rs:62`): capture is in shared state. The desktop's `error: Option` is overwritten by load, save and open errors alike.
- `AgentsPage.shared_input` vs shared `AgentsState.editing_shared` (`agents.rs:87`, :282-298, :221-222, render filter :796) and `WorkflowsState.draft` (`workflows.rs:382-423`): the desktop assigns shared `pub` fields directly. They could diverge; no path does today.
- Builder canvas gesture `pressed_at` + shared `BuilderState.panning` (`workflow_editor.rs:261`, `workflow_builder.rs:177-260`, :788-802): three release paths, one patched (comment :233). A release lost on window deactivation leaves `pressed_at` set.
- `SettingsView` editor slots `editor`/`agent_editor`/`machine_form`/`scroll_to_statuses` (`settings.rs:424-442`): both editors `Some` is representable. `open_editor` (:799-810) doesn't clear `agent_editor`, and `editing()` gives the workflow editor priority.
- `WorkflowEditor.details`/`script` keyed by transition index (`workflow_editor.rs:188`, :197): after a `read_again` that reorders transitions, Details follows the index to another row. Cosmetic.
- `DocEditor` (`doc_editor/mod.rs:75`): focus mirror plus blink/drag/reveal flags, reset on focus-out (:126-130). `set_from_outside` drops an outside update while the IME is composing (:196) and doesn't retry it.
- `FarEditorView` (`far_editor.rs:121`), `OpenForm` (`settings.rs:352`, RAII stop): no state of their own.

## Cross-slice leads

- **desktop-shell (`desktop/src/app.rs`, `app_tab_tearoff.rs`, `platform/`):**
  - The review/workspace open-hold protocol: `open_review_pending` :4546, `abandon_pending_review` :4566, `push_ahead` :4576, `release_settled` :4583 (finding 4).
  - `detail_over` :5114-5169 (finding 8).
  - The error-log read + mark-read :6348-6374, and `self.overlay =` at :3212, :6013, :6206 mirrored into `nav::Overlay` (finding 9).
  - Every `TabReorder`/`TabDragSession` transition, and the per-frame drag reconcile :7507-7516 (finding 16).
  - Deliberate-close marks :5616, :5750, :5760-5782 (finding 7).
  - `close_machine_form`/`close_pool_dialog` owners (finding 3).
  - `BootstrapState` after Ready, `FeedbackForm`, `UpdateNotice` cadence.
- **app (`crates/shelbi-app`):**
  - `issue_edit::SharedEdits`: lanes, `SaveStatus`, tickets (finding 1).
  - `settings::{workflows, agents, properties, workspaces}::loaded()` have no load generation (finding 5).
  - `settings::machine_form` Cancel is not gated on `saving` (finding 3; the TUI is likely affected too).
  - `session/restart.rs` `RestartController` and the `ReconnectController` (finding 7).
  - `review/diff_load.rs` `DiffLoad`, driven at 100 ms by `ReviewView::drive_diff` (`review.rs:1049-1110`).
  - `review::ReviewPanelState` (`set_merging` from app.rs:4771/4783).
  - `diff/state.rs` `Mode` plus `file_list`/`loading`/`pending`.
  - `activity::ActivityData::failed` (finding 14).
  - `launch::LaunchModel::state` (no Loading, finding 15).
  - `launch::connect::ConnectFlow`: `started` takes `stopped` after `busy` is set (connect.rs:279-288). Unreachable today.
  - `add_project_wizard::Wizard`: the real wizard machine, with `Attempt::New/Retry`.
  - `prereqs::Prereqs` locate slot (finding 10).
  - `reminders::SheetState` vs no job equivalent (finding 11).
  - `settings::general::GeneralForm`: a second `SaveStatus` (finding 2).
  - `settings::workflow_builder::BuilderState` pub fields.
  - `linear_setup::LinearChoice::answer`: a retry while Loading could accept an older answer.
  - `board_search` debounce, and `board::ClosedPage`.
  - `reachability::ReachabilityProber`: two independent poll timers.
- **state-core (`crates/shelbi-state`):**
  - `update_user_config` (`user_config.rs:821`) is read-modify-write with no lock. It affects every desktop and TUI settings writer (finding 2).
  - `BoardFreshness::banner` (`board_index.rs:748`) is the typed state that the desktop re-parses from text (finding 17).
- **tui:** finding 3's machine-form Cancel gap and finding 5's unordered page loads probably exist in the TUI too, since the guards live in the shared forms.
