# Implicit state machines: consolidated catalog

Consolidated 2026-11-18 from the nine slice surveys (orch-core, orch-workspace, state-core, cli-daemon, runtime, app, tui, desktop-views, desktop-shell; about 180 findings). Repo HEAD at consolidation is `e8145b3f`, the same commit the slices surveyed, so cited line numbers have not drifted.

**Shape of the result:** 45 catalog entries (17 high, 25 medium, 3 low) plus a low/leave-alone appendix. 59 confirmed live bugs (50 re-verified by me, 5 partly, 4 worker-verified only).

**Verification legend.**
- **V**: I re-opened the cited code during consolidation and the claim holds.
- **PV**: partly verified. The code is as described, but the downstream consequence is inferred (not reproduced), or only the headline was re-read.
- **W**: worker-verified only. The slice author says they re-read it; I did not re-open it.

Paths are repo-relative. `orch` = `crates/shelbi-orchestrator/src`, `state` = `crates/shelbi-state/src`, `cli` = `crates/shelbi-cli/src`, `app` = `crates/shelbi-app/src`, `tui` = `crates/shelbi-tui/src`, `client` = `crates/shelbi-client/src`, `session` = `crates/shelbi-session/src`, `desktop` = `desktop/src`.

---

## 1. Confirmed live bugs

These are wrong today. Some are deterministic and some are races; the tag says which. Failure-path bugs fire only when an earlier step fails, but the wrong handling is certain once it does.

| # | Bug (one line) | Where | Kind | Entry | Ver. |
|---|---|---|---|---|---|
| 1 | Event-append failure after a landed gated merge rolls the card back to `from`, leaving a merged PR on an un-moved card with no branch delete | `orch/transition_move.rs:409-428` | failure-path | C1 | V |
| 2 | `issue resume --workspace Y` on an already-in-progress card overwrites `assigned_to` and never restores it when the spawn fails | `cli/commands/issue.rs:1269-1310` | failure-path | C1, C5 | V |
| 3 | Push retry cap is a no-op when the local tip is unreadable: `" 1 retry"` round-trips to `tip="1"`, attempts fail to parse, ledger reads as `None` forever | `orch/poller.rs:4299-4313, 4564-4569`; `orch/workspace.rs:464` | deterministic (on that path) | C2 | V |
| 4 | Ready/transition markers are cleared before the old agent is killed, so a still-live old agent can write a marker that hands off the new same-task launch | `orch/workspace.rs:2686-2708` vs `:3205` | race | C2 | V |
| 5 | Ready-refused sidecar is never cleared at dispatch, so a returning task with the same refusal reason is refused silently | `orch/poller.rs:4029-4061`; `orch/workspace.rs:2680-2712` | deterministic | C2 | W |
| 6 | Dev orphan reaper treats every task in a workflow that fails to load as inactive, so a YAML typo gets every live dev pane in that workflow killed after the grace | `orch/poller.rs:7063-7075, 7104-7114` | deterministic | C3 | V |
| 7 | Launch-age grace never fires: `get_metadata` always returns `None`, so `workspace_launch_age` is always `None` and a launching or rolling-back slot can be reaped | `orch/session_process_backend.rs:377-389`; `orch/workspace.rs:2204`; `orch/poller.rs:6971` | deterministic | C3, C5 | V |
| 8 | A `shelbi open` user shell's mark is never stored (`set_metadata` is a no-op), so it probes as an agent: the reaper may kill it and dispatch kills it as `Redispatch` | `orch/session_process_backend.rs:391-401`; `orch/workspace.rs:2145, 3205` | deterministic | C3 | V |
| 9 | Remote zombie sessions probe Alive forever (the relay listing sends only `alive`, and SSH zombie reaping is a no-op), so they are never relaunched | `client/relay.rs:1056-1072`; `orch/session_backend.rs:497-499` | deterministic (given a zombie) | C7 | V |
| 10 | Remote `title`/`signal`/`resize` treat any relay error as "already gone", so Quit counts a refusing zombie as stopped | `orch/remote_session.rs:529, 551, 565` | deterministic | C7 | PV |
| 11 | If Quit's kill fails, the `quit` kill-reason stays on the live session; a later crash reads as deliberate and supervision stands down | `orch/quit.rs:631-640` | failure-path | C7 | V |
| 12 | Torn `exit.json` (plain `fs::write`) reads as `Crash` | `session/session.rs:949`; `orch/session_process_backend.rs:576-584` | race | C7 | V |
| 13 | A dependency in `canceled` (or any non-`done` terminal status) blocks its dependents forever | `crates/shelbi-core/src/model.rs:2610-2614` | deterministic | C8 | V |
| 14 | Custom Done/Archived statuses read as open: `is_terminal_column` uses the stock-id fallback, so synced Linear closed lanes (e.g. `duplicate`) list nothing through `CachedIssueStore` | `state/issue_store.rs:464-469`; `crates/shelbi-core/src/model.rs:2310-2320` | deterministic | C8 | V |
| 15 | GitHub writes a custom Done-category status (e.g. `shipped`) as an open issue | `state/github_store.rs:5107-5109` | deterministic | C8 | V |
| 16 | `message` deliverability check compares only `column == done`, so canceled/archived tasks still accept pushes | `cli/commands/message.rs:225` | deterministic | C8, C33 | W |
| 17 | Deleting the last transition edge in Settings sets `transitions: None`, silently flipping the workflow to legacy mode (agents unrestricted, handoff pushes and opens PRs); the field doc says the opposite | `state/workflow_config.rs:1085-1094`; `orch/poller.rs:3609-3613, 3983`; `crates/shelbi-core/src/workflow.rs:84-88` | deterministic | C9 | V |
| 18 | Zen crash-recovery hooks (`__zen-orch-start`, `__zen-heartbeat`, `__zen-orch-exit`, `__orch-record-exit`) have no production caller: Zen is never auto-disabled after an orchestrator crash, no crash record is ever written, and the poller comment relying on it is false | `cli/main.rs:391-420`; `orch/poller.rs:376-377`; `orch/lib.rs:1288-1292` | deterministic | C10 | V |
| 19 | `claude_md_migration_hinted` latches forever (its only reset is in the dead `__zen-orch-start`) | `cli/commands/zen_lifecycle.rs:59`; `state/agent_workspaces.rs:647-656` | deterministic | C10 | V |
| 20 | `list_open_projects` scans only flat `projects/*.yaml`, so a split (in-repo) project is never in the open set: no poller, and the daemon may idle-exit under it | `state/lib.rs:2809-2835` vs `:377-401` | deterministic (latent: no split projects exist here today) | C11 | V |
| 21 | Desktop catalog `note_open` is erased by any reload whose disk read began before the daemon wrote the record, so a quit project's tab can come back | `desktop/projects.rs:117-125, 212, 228` | race | C11 | V |
| 22 | Daemon start drops every persisted forward record whose master isn't up (all of them after a reboot), including the sticky TCP decision, so Tailscale hosts re-wedge on the Unix path | `state/ssh_control.rs:658-672` via `cli/commands/daemon/serve.rs:793` | deterministic | C12 | PV |
| 23 | `save_host_forward` is an unlocked read-modify-write of the whole map; its comment claims cross-host safety it only has within one process | `state/ssh_control.rs:563-581` | race | C12 | V |
| 24 | Diff error banner sticks: a failed watch read sets `error`; the next good read has the same digest and is never published, and nothing else clears `error` | `app/diff/session.rs:719-730`; `app/diff/state.rs:619-622` | deterministic (after one failure) | C13 | V |
| 25 | After a failed scope switch the user can't pick the shown scope again (`choose_scope` no-ops on it; refresh and watch re-read the failed one) | `app/diff/state.rs:1089-1091` | deterministic | C13 | V |
| 26 | Diff "Loading" forever when `r` is pressed before a slow `open_backend` fails (failure tagged generation 0 is dropped) | `app/diff/session.rs:217-221, 337-340, 379-381` | race | C13 | W |
| 27 | A comment whose save failed vanishes from the view and its "not saved" notice is cleared on the next store poll | `app/diff/session.rs:402-412, 509-510, 545-551` | deterministic (after a failed save) | C13 | V |
| 28 | Diff `sending` sticks at true when the send job can't be spawned or the backend never opened; every later send answers "Still sending…" | `app/diff/state.rs:1754-1758`; `app/diff/session.rs:303-305, 653-655` | failure-path | C13 | V |
| 29 | PR lookup: after `Some → Err(X) → None`, `lookup_failed` keeps X (short-circuit `||`), so a later identical error is suppressed | `app/diff/session.rs:614-621` | deterministic | C13 | V |
| 30 | TUI settled optimistic move never resolves if another writer moves the card, pinning it to a stale column, blocking reorder and forcing redraw every tick | `tui/kanban.rs:1820-1838` | deterministic | C14 | V |
| 31 | TUI resolves settled moves against its own overlay on a cold read, so the next lagging warm read bounces the card | `tui/kanban.rs:874-885` | deterministic | C14 | V |
| 32 | Desktop board: several held commands for one card are released together and all but the first are refused | `desktop/views/board_state.rs:1771-1780, 1871, 1884` | deterministic | C14 | W |
| 33 | TUI: navigating away doesn't cancel a review open; a late resolve yanks the view back | `tui/shell/mod.rs:2325-2327, 4886-4912` | race | C15 | V |
| 34 | Desktop review: a failed Ensure always re-selects Chat, even after the review was hidden or the user moved views | `desktop/views/review.rs:907-917` | race | C15 | V |
| 35 | TUI Settings: mouse (and palette SwitchProject) isn't gated while the leave prompt's save runs, so a stale `go(leave)` fires after the user already went elsewhere | `tui/shell/settings.rs:2131, 1156-1170, 1060-1070` | race | C16 | PV |
| 36 | `AgentsState::committed` has no in-flight check and starts the next queued save even with nothing in flight | `app/settings/agents.rs:366-389` | deterministic | C16 | V |
| 37 | Properties drops a queued commit when the in-flight save fails | `app/settings/properties.rs:456-460` | failure-path | C16 | V |
| 38 | Desktop Settings ▸ General writes whole-block snapshots with no ordering; an older snapshot can land last | `desktop/views/settings.rs:1302-1322` | race | C16 | V |
| 39 | Desktop per-host watch dies for good when a Changed host is re-adopted by any path but Reopen (finished task stays in `_watch`) | `desktop/app.rs:1844-1863, 1911-1922` | deterministic | C17 | V |
| 40 | `HostLink` stores one project's check failure host-wide and hands it to other projects' panes | `app/launch/link.rs:332-352` | race | C17 | V |
| 41 | `shelbi reload` reports "daemon restarted" when `stop_daemon` returns `Err` | `cli/commands/reload_session.rs:114-118` | failure-path | C18 | V |
| 42 | v2 hooks write the cursor before the buffered lines are printed (acked but never shown if the hook dies); v1 `events drain` writes the cursor without the delivery lock | `cli/commands/orchestrator_awareness.rs:235-239, 392-398`; `cli/commands/orchestrator.rs:929-936` | race | C19 | V |
| 43 | `claim_event_follower` SIGTERMs the recorded pid with no identity check | `state/event_log.rs:1496-1499` | race (pid reuse) | C19 | V |
| 44 | GitHub `move_status` rebuilds the label set from a `labels(first: 10)` read with no hydration, deleting labels past the tenth | `state/github_store.rs:1322, 1364`; `:1992-2001`, query at `:4201` | deterministic (issues with >10 labels) | C22 | PV |
| 45 | `check_write_reserve` ignores `reset_at`, so writes stay refused after the window resets until some REST read refreshes the tier | `state/github_store.rs:2813-2837` | deterministic | C22 | V |
| 46 | `tracker disconnect` / switch never clears `statuses.yaml` `managed_by: linear`, leaving the catalog read-only with no exit but a hand edit | `orch/tracker_connect.rs:248-260` | deterministic | C24 | V |
| 47 | Blocked auto-heals are never surfaced: the template calls `auto_heal=M` "informational (already applied)" and `--needs-judgment` filters them | `state/default_orchestrator.md.template:726-728`; `cli/config_upgrade_apply.rs:1042-1045` | deterministic | C26 | V |
| 48 | Palette shows "Turn Zen Mode off" while Zen is Paused, and running it turns Zen On | `app/command_model.rs:132`; `app/command.rs:510-514`; `state/lib.rs:2880-2893` | deterministic | C29 | V |
| 49 | Hub-level clarification questions can never be marked answered (`mark_question_answered` always gets `Some(project)`) | `cli/commands/message.rs:300-305`; `state/questions.rs:260-272` | deterministic | C33 | V |
| 50 | `resolve_remote_bin` (the dispatch gate) ignores the record's host binding, unlike the view and `relay_bin_for_host` | `orch/remote_session.rs:398-400` vs `:430-436` | deterministic (after a relocate) | C34 | V |
| 51 | Machine form Cancel/Esc stay live during a save; the owner gets `Cancelled`, skips the refresh, and the verified-binary record is never written | `app/settings/machine_form.rs:517, 539`; `desktop/views/settings.rs:746-747` | race | C37 | V |
| 52 | Error-log overlay marks errors read even when the read failed, showing "no errors" and clearing the badge | `desktop/app.rs:6357-6358` | failure-path | C37 | V |
| 53 | TUI job/reminder sheet "Canceling…" never clears on a failed cancel (only a `tracing::warn`) | `tui/shell/mod.rs:6064-6076, 6111-6122` | failure-path | C21 | V |
| 54 | TUI Activity `r` re-reads the whole log from offset 0 and appends it to the snapshot window (duplicate rows) | `tui/activity.rs:257-289, 320-328`; `tui/handlers/activity.rs:27` | deterministic | C38 | V |
| 55 | Activity title cache checks the terminal/miss freeze before the mtime, so reopened or edited FS-backend done tasks are never re-read | `app/activity.rs:470-482` | deterministic | C38 | V |
| 56 | TUI never calls `refresh_search`, so search highlights and next/prev targets drift with new output | grep: only `desktop/elements/terminal/mod.rs:1335` calls it | deterministic | C39 | V |
| 57 | Desktop `launch_plan` drops an open slot whose project is `None`, so a window left on its New tab or chooser isn't restored | `desktop/window_state.rs:286`; `desktop/app.rs:3423-3433` | deterministic | C36 | V |
| 58 | Desktop notification tracker never clears a removed workspace's blocked notice | `desktop/notify.rs:135-155` | deterministic | C41 | V |
| 59 | `guard status` checks a hard-coded `.git/hooks/pre-commit`, so with `core.hooksPath` or a linked worktree it says "not installed" while the guard is active | `cli/commands/guard.rs:83-124` vs `orch/githook.rs:172-193` | deterministic | appendix | PV |

(59 rows: 50 V, 5 PV, 4 W. Several entries carry more than one bug.)

---

## 2. Catalog

### HIGH

#### C1. Workflow edge application and status moves (merge, move, event, cleanup)
- **Entity:** one issue crossing a workflow edge, together with its PR, branch, `events.log` line and Zen bookkeeping.
- **Machine:** an edge is applied as gate prefix (`push_branch`) → gated merge (irreversible) → `move_status` → event append → Zen demotion/flow bookkeeping → post-move actions (`delete_branch`, `run:`, pane cleanup). Real states: not started, gate ran, merged-not-moved, moved-no-event, moved-with-event, cleanup pending, cleanup failed. None is persisted: a `MoveError`/`GatedMerge` type exists, but each caller re-implements the order and its own failure policy, and no record says "cleanup owed". At least seven writers do this: `transition_move::move_issue_with` (CLI `issue move`, TUI board), Review Approve (`review_ui.rs`, through `mutate.rs`), the poller's ready handoff and transition marker (C2), Zen's GitHub-merge reconcile, `shelbi issue resume`, `action apply-transition`, and the raw `move_status` calls in `mutate/start.rs`, `cli issue.rs`, `cli workspace.rs`. The file backend's `$EDITOR` mode bypasses all of it.
- **Failure modes:**
  - Event-append failure rolls the card back after the merge landed, and never runs `delete_branch` (`orch/transition_move.rs:409-428`; bug #1).
  - Approve runs merge, then move, then `let _ = append_task_event`, with no rollback and no Zen record; its cleanup runs on a detached thread and is lost if the daemon exits (`orch/review_ui.rs:399-418`; `orch/mutate.rs:474-486`).
  - Post-move cleanup is fire-and-forget everywhere. A crash after the move means the next tick sees the card out of its source status and never retries (orch-core H1).
  - A `run:`/`ready:` command can start twice on retry (`orch/transition.rs:516-542`).
  - Zen GitHub-merge reconcile: guard #2 compares `merged_at < task.updated_at`, but any later field write bumps `updated_at`, so a legit merge can look stale forever; guard #1 likely misreads a hand-merged Zen PR because the local ref isn't an ancestor of the integration commit (`orch/zen.rs:1389, 1410`; plausible, not reproduced).
  - `shelbi issue resume` writes the store directly with no expected-state guard, skips transition actions, overwrites `assigned_to` with no rollback on an in-progress card, and never undoes a `--take` claim (`cli/commands/issue.rs:1238-1332`; bug #2).
  - `action apply-transition --from review --to done` merges and deletes the branch without moving the card or checking it is on `from` (`cli/commands/action.rs:190-218`).
  - File backend `issue edit` opens the task file in `$EDITOR`: column, priority and assignee change with no validation, actions or event (`cli/commands/issue.rs:1490-1497`; pinned by `tests/issue_edit.rs:424`).
  - Review reject falls back to a literal `todo` (`orch/review_ui.rs:553-556`).
- **Merged from:** orch-core H1, M10, part of M2; orch-workspace M10; cli-daemon H3, M12, M14, Minor (`issue prio`/`depends`/`rm` direct writes, `mutate_client` expected `None` on read error); tui H2 item 4 (persister asymmetry).
- **Files:** `orch/transition_move.rs:300-511`; `orch/transition.rs:188-371`; `orch/review_ui.rs:371-470`; `orch/zen.rs:1380-1560`; `cli/commands/issue.rs:1149-1332`; `cli/commands/action.rs:95-218`.
- **Verification:** V (rollback path, Approve sequence, detached cleanup, resume sequence). Zen reconcile guards: W.
- **Workers:** redesign: one `apply_transition(edge, cause)` owning gate, write, event, Zen bookkeeping and cleanup, with a persisted in-flight record `{edge, merged_sha?, phase}`; resume becomes a daemon `MutationKind`.
- **My view:** agree. This is the highest-leverage single design item: C2, C6 and C14 all call into it. Start with the persisted cleanup-pending record; it fixes the crash windows without needing every caller ported at once.

#### C2. Worktree handoff marker protocol (ready / transition markers, deferral and refusal sidecars, push ledger)
- **Entity:** one workspace worktree's `.shelbi/` markers, coupled to its card (`column` + `assigned_to`).
- **Machine:** the worker writes `shelbi-ready` (or a gate agent writes `shelbi-transition`); the poller reads it every tick and walks deferral (task load failed), refusal (worktree check), rebase, push (with a `<tip> <attempts> <retry|blocked>` ledger and a separate pushed-tip provenance file), detach, merge, edge actions, move, cleanup, pane kill, marker clear. State is the combination of five marker files plus legacy `.claude/` copies, plus tracker, git and pane state. Dispatch and resume clear different subsets. Representable but invalid: merge landed while the card is still in progress with the marker present; worktree detached but task not moved; ready and transition markers together; new and legacy copies.
- **Failure modes:**
  - Push retry cap does nothing on an unreadable tip (bug #3); the comment at `orch/poller.rs:4562-4563` claims the opposite. Same shape as the 482-retry incident the cap was written for (`:4280-4281`).
  - Markers cleared before the old agent is killed (bug #4).
  - Refused sidecar never cleared at dispatch (bug #5).
  - Crash between move (`poller.rs:3315`) and cleanup (`:3351/3393/3477`) takes the stale branch next tick: marker cleared, `delete_branch`/`run:`/pane kill never happen.
  - Transition marker is not atomic: crash after the move drops the marker as "not-owned" next tick, losing `close_pr` and the event (`poller.rs:3742-3873`).
  - Transition validation falls back to the any-to-any default on a workflow load error (permissive), while the reaper (C3) fails destructive on the same error (`poller.rs:3719-3722`).
  - No backoff or dedupe: a persistent merge conflict re-runs rebase, push and `gh` and appends a line every poll (`poller.rs:3226-3230`).
  - Sidecar writes are non-atomic (`fs::write`/`printf >`, `workspace.rs:481-497`); a torn pushed-tip loses provenance and turns the next amend into a terminal `Blocked`.
  - A dev-resume of a gate slot clears a deferred verdict, parking the card in the gate for good (`poller.rs:3857-3859`, 6383).
- **Merged from:** orch-core H2, M1, M2, L2; orch-workspace H2, M4.
- **Files:** `orch/poller.rs:2992-3480` (ready), `:3642-3930` (transition), `:3940-4060` (refusal), `:4254-4703` (push ledger); `orch/workspace.rs:199-510, 949-1054, 2680-2712, 2940-2969`.
- **Verification:** V for bugs #3 and #4; the remaining claims W.
- **Workers:** make explicit: one persisted per-task handoff phase (enum + attempt/backoff) or `HandoffLedger` owning every path; one `reset_for_launch()` shared by start and resume, run after the kill; atomic sidecars; structured JSON ledger.
- **My view:** agree. Fix #3 and #5 now (small). The phase record should be the poller-side half of C1's transition record rather than a second ledger.

#### C3. Dev orphan reaper and slot occupancy (who owns this slot?)
- **Entity:** a workspace slot: agent pane, user shell, or orphan; and every derived "is it busy?" answer.
- **Machine:** the reaper walks not-orphaned → grace → launch-young hold → eligible → backoff → reaped (`OrphanReap{since, failures, retry_at}`), deciding "orphaned" from the board (`task_is_active` over the index's `assigned_to`) plus a live probe (`SlotProbe::Alive{user_shell}`). The launch-young hold and the user-shell flag both read tmux-era session metadata that the current backend no longer stores. Separately, "is this slot busy, and what badge does it show" is derived by about six predicates (dispatch, supplant, pool remove, pool rename, sidebar badge, machines view, CLI `workspace list`) and the "trust status.yaml only if it names the current task" join rule is copied three times.
- **Failure modes:**
  - Workflow load failure kills every live dev pane in that workflow (bug #6). `task_is_active` also feeds `redispatch_superseded` (`poller.rs:5259`) and `assigned_dev_task_for` (`:6085`).
  - Launch-age grace is dead code (bug #7), so the "recurring bug" the grace was written for is open again.
  - `shelbi open` shells read as agents (bug #8); the newer `<p>/ws/<ws>/shell` session is a second user-shell model.
  - Dev orphan path reads the index's `assigned_to` without folding the assignment overlay, unlike the review path (`poller.rs:6015-6028`); the same lag class once reaped a booting review slot (`:6737-6741`).
  - Sidebar shows an assigned slot with no status as Working forever (no dead/stuck badge) (`orch/sidebar_model.rs:268-270`); sidebar and machines view disagree on stale `status.yaml`.
  - Pool remove ignores PreAssigned cards (`orch/pool_admin.rs:391-404`).
- **Merged from:** orch-core H3; orch-workspace H3, M5; state-core L1; cli-daemon L8, Minor (`open.rs` busy-check TOCTOU).
- **Files:** `orch/poller.rs:6930-7116`; `orch/workspace.rs:2105-2290`; `orch/session_process_backend.rs:377-408, 530-536`; `state/occupancy.rs:26-58, 225`; `orch/sidebar_model.rs:247-271`; `orch/mutate.rs:536-632`.
- **Verification:** V (bugs #6, #7, #8 and the reaper's use of the dead grace).
- **Workers:** make explicit: `Active | Inactive | Unknown` with hold-on-Unknown; derive launch age from `meta.json launched_at`; retire the no-op metadata methods; one occupancy + display function in shelbi-state/shelbi-app.
- **My view:** agree, and #6 + #7 together are the most dangerous live bugs in the survey (both kill working agents). Both are small fixes that don't need the redesign.

#### C4. Stranded-slot recovery and supervision (three crash-loop machines on two threads)
- **Entity:** a workspace slot's pane (dev or review) and the orchestrator pane, when dead.
- **Machine:** `SupervisionState{ever_alive, restarts, gave_up}` per workspace thread, plus `ReviewResumeState` and `DevResumeState` on the supervisor thread, with `decide_dead` copied three times. States: alive/booting, dead-unconfirmed, dead-confirmed, backoff, resuming, GaveUp, StoodDown, dispatch-in-flight (read from `events.log`), Unknown (board cold). Two threads can each believe they own the same dead slot; the only guard is `recent_dispatch_active`, a grep of the last 64 KB of the hub-global `events.log`.
- **Failure modes:**
  - `recent_dispatch_active` returns `false` on any I/O error, the destructive side (`state/event_log.rs:2891-2895`); `active-gate-failed` lines appended every tick with no dedupe can flush its window (`orch/poller.rs:5547-5553`).
  - Its reverse scan `continue`s past non-active statuses instead of stopping at the slot's newest line, so `message-channel` followed by `stalled` still reads active for 60 s (`state/event_log.rs:2915-2930`).
  - In memory only: after a poller restart `ever_alive=false`, so an already-dead pane is never adopted (`app/supervision.rs:162-165`); the orchestrator "gave up" status file outlives the forgotten latch, and a probe error clears the visible failure (`poller.rs:5325`).
  - A cancelled orchestrator launch counts a restart with no event (`poller.rs:5355-5357`).
  - Past bugs: `rt-review-slot-relaunch-loop`, spurious `--continue` 2 s after launch (`poller.rs:5797-5799, 5602, 6261-6264`).
- **Merged from:** orch-core H4, L4, L9 (orchestrator supervision status); state-core L11, L17 (launch-in-flight scan); app Minor (`SupervisionState`).
- **Files:** `orch/poller.rs:5093-5440, 5632-5726, 6108-6400`; `app/supervision.rs:83-245`; `state/event_log.rs:2836-2931`; `state/supervision_relaunch.rs`.
- **Verification:** V (`recent_dispatch_active` false-on-error). Rest W.
- **Workers:** redesign: one `SlotRecovery` enum per slot, one owning thread, and a durable launch lease (deadline + pid) replacing the log grep.
- **My view:** agree; pair the lease with C5's `LaunchRecord` so one record serves both "launch in flight" and "launch age".

#### C5. Dispatch launch lifecycle
- **Entity:** one launch of one card onto one workspace slot.
- **Machine:** recheck → persist (status move, then `assigned_to`/`branch` as a second write) → register cancel generation → `lock_workspace` → sync/branch checks → worktree mutation → kill old pane → spawn → stamp epoch → delivery verdict (`unverified`/`confirmed`/`stuck`/`stalled{…}`) → completed / completed-late / rolled back. The outcome exists only as `dispatch … status=` strings in `events.log`, which `mutate/start.rs` scrapes to late-confirm.
- **Failure modes:**
  - Nothing durable says "launching since T": a process death after persist leaves an active, assigned card with no agent (deliberate F7 trade-off, `orch/mutate/start.rs:174-176`).
  - Launch epoch stamped best-effort and, on this backend, nowhere (C3 bug #7).
  - `launch_timeout()` read from each process's own env; CLI await and daemon reaper can disagree (`orch/workspace.rs:2227-2235`).
  - Worktree mutated (settings, hooks, beacon reset, prompt) while the old agent is still alive, before the kill at `:3205`.
  - Resume never verifies the branch; `reattach_detached_resume_head` leaves any attached branch alone (`workspace.rs:7213-7239`).
  - `classify_handoff` falls back to Developer on a store/workflow read error (`:6060-6068`).
  - Stale `WorkspaceSpec` cloned before `lock_workspace` (`start.rs:51`); a concurrent pool remove/relocate can launch onto the old host.
  - Review-load rollback restores only fields after `clear_parked` ran, un-parking the card (`orch/load.rs:899, 915-928`).
  - `shelbi workspace stop` releases before killing; a failed kill leaves the card free while the agent keeps working (`cli/commands/workspace.rs:528-547`).
- **Merged from:** orch-workspace H1, L9 (expected-state protocol); cli-daemon M11, Minor (`issue start` estimate written client-side); state-core L17 (`$SLOT` recomputed at fire time).
- **Files:** `orch/mutate/start.rs:51-643`; `orch/workspace.rs:2624-3600`; `orch/load.rs:878-940`; `cli/commands/workspace.rs:510-570`.
- **Verification:** W for the list; V for the dead epoch (shared with C3).
- **Workers:** make explicit: persisted `LaunchRecord{phase, since, task, generation}` with one transition fn that also emits the event; re-read config under the lock; kill first, then mutate; verify branch on resume.
- **My view:** agree. Kill-then-mutate and branch verification on resume are cheap and independent of the record.

#### C6. Review slot lifecycle (claim, load, serve, ready-for-review, decide, tear down)
- **Entity:** a (review-tagged workspace, task) pair, with its pane, content sessions, dev server and markers.
- **Machine:** Free → Claimed (fields written, no pane) → Checking out → Serving → Ready for Review (marker names the task and, if configured, the health check passed) → Stranded / Evicted / Parked / auto-load GaveUp. State lives in about eight stores: card fields, the assignment overlay, the lagging `board-index.json`, the `shelbi-review-loaded` marker (written by the poller and by the Review agent itself), `status.yaml`, the poller's `last_known`, the dev-server pgid record, parked markers and the load-failure ledger. Three processes write them.
- **Failure modes:**
  - Manual load's busy guard reads a Cold board as all-idle and can load over a live slot (`orch/load.rs:454-463`; `state/occupancy.rs:101-104`).
  - Marker is sticky: a dead server keeps showing Ready for Review until unassign (`orch/workspace.rs:670-672`).
  - Health-check lookup fails open on a workflow load error, promoting on liveness alone (`workspace.rs:599-608`).
  - On a cold board the poller falls back to `status.yaml`'s own Serving task, so derived state proves itself (`poller.rs:4992-4996`); a failed `save_workspace_status` warns while `last_known` advances (`:6629-6644`).
  - Remote markers are never cleared on close (`orch/review_session.rs:542-544`).
  - pgid record has no process identity: `kill(-pgid,0)` guards a SIGTERM/SIGKILL that can hit a recycled group (`workspace.rs:2331-2385`); a second serve wrapper overwrites the record and the first's clean exit deletes it (`cli/commands/review_serve.rs:73-98`). Past incident: a stale record SIGTERMed a `cargo test` run.
  - Composite writes (feedback append + move + overlay clear + parked marker) are multi-step on remote backends despite the trait promising atomicity; a crash in `park_review` leaves the card unowned and unparked so the autoloader re-grabs it (`state/issue_store.rs:725-763`; `state/github_store.rs:1630-1720`; `state/linear_store/mod.rs:1126-1179`).
  - Auto-load `gave_up` never ages out (`orch/load.rs:664-681`).
  - Teardown `?`-fails at the pane kill after the marker clear, leaving `status.yaml` stale (`orch/review_ui.rs:517-531`).
  - Review id is a literal (`Column::review()`) in about 7 places and the slot role is `contains("review")` in about 10 (see C8).
- **Merged from:** orch-core H5; orch-workspace H5, L1; state-core M2, M7; cli-daemon M10; tui L11 (dead legacy `App` with a third review-state derivation).
- **Files:** `orch/poller.rs:4940-5000, 6485-6800`; `orch/load.rs:38-345, 450-706, 878-940`; `orch/review_session.rs:253-555`; `orch/workspace.rs:255-910, 2331-2401`; `state/workspace_status.rs:212-273`.
- **Verification:** V (cold-board guard, pgid guard). Others W.
- **Workers:** redesign: one daemon-owned per-slot record `SlotState::{Free, Claimed, Serving, Stranded, …}` with one transition fn; markers, overlay entries and content-session ownership derived from it; pgid + start time.
- **My view:** agree. Retiring the agent-written marker (or making the poller its only writer) removes the hardest cross-process edge.

#### C7. Session host on-disk lifecycle and name resolution (local and remote)
- **Entity:** one `shelbi __session` process and its `~/.shelbi/sessions/<short-id>/` directory; and the logical session names that map onto one or more such directories.
- **Machine:** Reserved (dir only) → Starting (lock) → Starting/not bound (lock + meta) → Live → Zombie (lock held, socket refusing or wedged) → Exiting (`exiting` marker) → Exited (`final.txt` + `exit.json`) → Crashed (lock released without `exit.json`) → Reaped; plus a client-written `kill-reason` overlay. The host writes markers; at least four predicates in `client/discovery.rs` (`alive`, `exiting()`, `usable()`, `exit_record()`/`kill_reason()`) re-derive the state, `SlotDeath` maps it again in the orchestrator, and remote readers see only `alive`.
- **Failure modes:**
  - Remote zombies probe Alive forever (bug #9); a remote zombie next to its replacement can be the one attached to (repeats `rt-re-entering-a-review-fails-to-attach`); killing one reports success (bug #10).
  - Duplicates under one name survive Quit (`orch/quit.rs:367-371`) and Close; content/user-shell "ensure" is check-then-spawn with no claim, so two concurrent opens spawn two editors on one worktree, and a failed remote listing reads as "not live" (`orch/workspace_session.rs:159-207, 317-348`; daemon `control.rs:945-966` skips the per-issue lock).
  - Kill-reason: record and kill resolve the session separately (`orch/session_process_backend.rs:544-554` vs `:511`); a failed Quit kill leaves `quit` on a live session (bug #11); unknown tokens parse as `Stop`.
  - Non-atomic `meta.json`/`exit.json` (bug #12); `zombies_to_reap` skips the `exiting` filter `choose_session` applies (`client/discovery.rs:409 vs 465-467`).
  - Reaper vs a starting session: lock file created before it is flocked, so a concurrent reap can `remove_dir_all` a starting session (`session/lock.rs:37-46`; `client/discovery.rs:342-351`); Reserved dirs with no lock accumulate.
  - Zombie reap removes the dir after SIGTERM whether or not the process exits (`orch/session_process_backend.rs:702-707`).
- **Merged from:** runtime 1; cli-daemon M4, Minor (`session_cli` selector); orch-workspace H4, M2, M9, L7.
- **Files:** `session/session.rs:432-604, 923-951`; `session/lock.rs:35-50`; `client/discovery.rs:159-490`; `orch/session_process_backend.rs:161-720`; `orch/remote_session.rs:373-720`; `client/relay.rs:1056-1072`.
- **Verification:** V (non-atomic writes, `slot_death`, remote listing fields, SSH reap no-op, lock-then-flock order).
- **Workers:** make explicit: one `SessionDiskState`/`classify(dir)` in shelbi-session; one `resolve(name) -> {chosen, duplicates, state}` in shelbi-client used locally and over the relay; atomic markers; one `kill_with_reason`.
- **My view:** agree. Shipping the classified state over the relay is the fix for every remote item at once.

#### C8. Status semantics keyed on literal ids instead of the workflow catalog
- **Entity:** what an issue's status *means*: terminal, done-for-dependencies, review, in-progress, backlog.
- **Machine:** statuses are declared in `statuses.yaml` with a category (Backlog/Ready/Active/Handoff/Done/Archived), but behavior bypasses it. There are at least four "is terminal" rules (literal `done`, the stock-id fallback `Column::category()` whose own doc forbids behavior use, GitHub's literal `done || canceled`, Linear's state type) and about 40 `== Column::review()/todo()/in_progress()/done()` checks across crates. A second resolver (`resolve_task_status`) is duplicated between Zen and the TUI with different fallbacks.
- **Failure modes:** bugs #13, #14, #15, #16. Also:
  - Linear's default "In Review" syncs to `in-review` (Handoff), so literal review checks should miss those cards (suspected; `state/linear_statuses.rs:93,111`).
  - Alias folding maps a Linear state named "Completed" onto `done` while the catalog declares `completed` (suspected; `state/linear_store/mapping.rs:57`).
  - Zen scans compare literal `in_progress()`/`backlog()`, so a renamed backlog makes nothing eligible and the overlap guard sees no in-flight work (`orch/zen.rs:15205,15211`).
  - Unassign parks only on literal `review`; the sidebar's review sections filter on literal `review` (`orch/mutate.rs:444-450`; `orch/sidebar_model.rs:82`).
  - `resolve_current_status_id` silently remaps an unknown column to the first status in the same category (`poller.rs:3538-3550`).
- **Merged from:** state-core H1; orch-workspace H5 item 5, M5 item 3; orch-core L8, Minor; cli-daemon M7 (done check); orch-workspace M10 item 4.
- **Files:** `crates/shelbi-core/src/model.rs:2205-2350, 2605-2615`; `state/issue_store.rs:460-470`; `state/issue_cache.rs:458-476`; `state/github_store.rs:5107`; `orch/zen.rs:15200-15800`.
- **Verification:** V (#13, #14, #15).
- **Workers:** make explicit: one `StatusSemantics` resolver (id + resolved workflow/catalog → category and roles); stop alias-folding non-core ids.
- **My view:** agree. Needs the workflow in hand at every call site, so do it together with C9's topology object.

#### C9. Workflow move policy and status roles
- **Entity:** a workflow's move legality and side-effect policy, and the role each status plays.
- **Machine:** `transitions: Option<Vec<Transition>>` is three modes: `None` (legacy: agents may move anywhere, handoff always pushes and opens a PR, never branch-free), non-empty (agents restricted to listed edges, git work only on edges that push/PR/merge, humans still any-to-any), and `Some([])` (agents can't move at all, workflow branch-free). The field doc says transitions never restrict moves. Roles (primary active, gate pass/reject targets, Approve target) are re-derived by heuristics in several crates.
- **Failure modes:**
  - Deleting the last edge flips to legacy (bug #17); adding the first edge silently confines agents and turns off the implicit handoff push; removing a status can leave `Some([])` (`state/workflow_config.rs:994-1003`).
  - Three copies of "primary active" (`crates/shelbi-core/src/workflow.rs:379`; `cli/commands/issue.rs:959`; `orch/mutate/start.rs:688`).
  - Approve lands on "next status in catalog order", not an edge, so reordering moves where Approve lands (`orch/review_ui.rs:381`; `orch/zen.rs:1461`).
  - Gate pass/reject computed for the prompt at dispatch and again at verdict time; an edit in between makes them disagree (`orch/workspace.rs:6079-6080`; `poller.rs:3592`).
  - CLI falls back to the built-in default workflow on a load error (`cli/commands/issue.rs:926-947`).
- **Merged from:** state-core H2, M9; cli-daemon Minor; orch-core M2 (permissive fallback, cross-ref C2).
- **Files:** `crates/shelbi-core/src/workflow.rs:80-91, 217-570`; `state/workflow_config.rs:994-1094`; `orch/poller.rs:3592-3620, 3978-3990`; `orch/review_ui.rs:381`.
- **Verification:** V.
- **Workers:** make explicit: explicit policy mode plus one `Workflow::check_move(actor, from, to) -> Result<Edge>`; resolve a `WorkflowTopology` once at load; the editor never changes mode as a side effect.
- **My view:** agree; the editor flip is a one-line fix to stop the bleeding today.

#### C10. Orchestrator liveness, crash recovery and Zen auto-disable
- **Entity:** a project's orchestrator session and the "it crashed last time" signal.
- **Machine:** in the tmux era a pane wrapper ran start → heartbeat every 60 s → graceful exit / crash record, and `state.json` `zen_orchestrator_alive_at` meant "alive" while running and "crashed" after death; the next start turned Zen Off. The session backend never reproduced the wrapper. Today the orchestrator lifecycle is spread over `ensure_dashboard`, `relaunch_orchestrator_session`, `replace_orchestrator_session`, the poller's `SupervisionState` and handoff/greeting claim files.
- **Failure modes:**
  - Bugs #18 and #19: Zen stays On after a crash and keeps auto-merging on relaunch; `shelbi zen status` always says "last crash: never"; a leftover tmux-era timestamp prints as "orchestrator alive" forever (`cli/commands/zen.rs:545-547`).
  - Readers still trust the dead contract: bootstrap prompts and the template tell the orchestrator to look for a `zen=off reason=crash-recovery` line that is never written (`orch/lib.rs:938,968,990`); `status` greps a 6 h / 64 KB window and can only show stale lines (`cli/commands/status.rs:741-802`). Even revived, that line is classified as a generic event the v2 label hides.
  - `zen_check_crash_recovery` writes `zen_mode` directly, bypassing `set_zen_mode` (`state/lib.rs:2558`).
  - `replace_orchestrator_session` kills and polls 5 s without the dashboard lock, so the supervisor can count a reload as a crash (`orch/lib.rs:676-688`); a timed-out kill attaches to a dying session; a runner that exits at once spends the greeting; the open flag is set before the spawn (`:563` vs `:606`).
- **Merged from:** state-core H3, L11; cli-daemon H2; orch-workspace M3; orch-core L9 (supervision status file).
- **Files:** `state/lib.rs:1601-1649, 2496-2640`; `cli/commands/zen_lifecycle.rs:38-168`; `cli/main.rs:391-420`; `orch/lib.rs:492-703, 1288-1292`; `orch/poller.rs:376-377, 5320-5425`.
- **Verification:** V (no producer; grep finds callers only in tests and in `zen_lifecycle.rs` itself).
- **Workers:** redesign: an `OrchestratorLifecycle { Running, ExitedClean, Crashed{at, record} }` written by the session supervisor; route auto-disable through `set_zen_mode`; or delete the dead hooks, fields and prompt text.
- **My view:** agree, and first make the product decision the state-core author flags: should Zen still auto-disable on a crash? Until then, at minimum delete the false comment and prompt text so nothing relies on it.

#### C11. Project open set ("which projects are open?")
- **Entity:** `State::open` per project in `state.json`, the registry scan that enumerates projects, and the desktop's mirror of it.
- **Machine:** open/closed, set true by `ensure_dashboard` (before the spawn) and false by quit. It decides which projects get a poller and board refresh, whether the daemon may idle-exit, and whether the session-side watchdog resurrects the daemon. Readers: `list_open_projects` (daemon serve/poller/board/lifecycle, quit, app command model, TUI), `is_project_open` (watchdog). The desktop catalog mirrors it with optimistic local notes written into the same set.
- **Failure modes:**
  - Split projects never in the open set (bug #20); one unreadable `state.json` reads as closed (`state/lib.rs:2829`), stopping that project's poller.
  - A newer binary's new enum variant in `state.json` makes the whole file unparseable for an older binary (`deserialize_lenient` rejects unknowns, `state/lib.rs:1818`), so every `update_state` fails and the project reads as closed.
  - Desktop note erased by a racing reload (bug #21); swaps carry no generation and each refresh spawns its own thread (`desktop/projects.rs:170-219`).
  - Correctness of the watchdog depends on every stop path clearing `open` first (C18).
- **Merged from:** state-core H4, L12 (enum forward-compat hole); desktop-shell M1; runtime 6 (input side).
- **Files:** `state/lib.rs:1710-1721, 2788-2836, 1818`; `orch/lib.rs:563`; `orch/quit.rs:542, 596`; `desktop/projects.rs:51-230`; `session/daemon_watchdog.rs:78-96`.
- **Verification:** V.
- **Workers:** make explicit: derive the open set from one shared "registered project ids" enumerator; surface "open but unreadable"; desktop gets a read generation and a pending-notes overlay.
- **My view:** agree; bug #20 is a three-line fix (reuse `has_any_project_registration`'s walk) worth doing before anyone migrates a project in-repo.

#### C12. SSH reverse forward and ControlMaster per host
- **Entity:** the hub's `-R` reverse forward to one remote host (how remote workers reach `hub.sock`) and the multiplexed ControlMaster it rides on.
- **Machine:** an explicit resolver `EffectiveForward::{Unix, Tcp(port), TcpUnbound}` sits inside a wider implicit lifecycle: per mode Healthy / Broken(reason) / Inconclusive / MasterOpenFailed / Wedged (fallback Unix→TCP when auto), TCP sweep outcomes Bound / TransportUnstable / Exhausted, persisted in `forward-modes.json`, with process-global health/stamp statics and the YAML `forward:` pin. Masters open implicitly from any `ssh` call (`ControlMaster=auto`) but are dropped under a lock.
- **Failure modes:**
  - Sticky TCP decision forgotten on every daemon start (bug #22); the doc at `crates/shelbi-ssh/src/lib.rs:1902-1906` promises the opposite.
  - Lost update across processes (bug #23); `effective_forward` writes from a read path (`state/ssh_control.rs:637`).
  - A failed sweep drops the master (releasing the port) but keeps the record, so workers are pointed at a dead port (`crates/shelbi-ssh/src/lib.rs:1744-1748`).
  - `TcpUnbound` hands workers the Unix spec (`lib.rs:277-289`).
  - Any command can open a master with a pre-repair `-R` spec; `run_resilient` drops the shared master on any blank-stderr exit 255, ending every multiplexed session (`lib.rs:595-612, 686-689`).
  - Health de-dup is per process, so daemon, TUI and each CLI each log their own "first" failure.
- **Merged from:** runtime 2, Minor (`MuxFailure`, `eperm_logged`); state-core M8.
- **Files:** `crates/shelbi-ssh/src/lib.rs:237-320, 551-700, 1150-1993`; `state/ssh_control.rs:540-720`; `cli/commands/daemon/serve.rs:786-796`; `orch/poller.rs:634`.
- **Verification:** V (reconcile drops whole record; unlocked RMW). Re-wedge consequence PV.
- **Workers:** make explicit (small redesign): daemon is the single owner; `ForwardState` persisted under a file lock with one `transition(host, probe)`; reconcile drops the port, not the mode.
- **My view:** agree. Silent loss of worker→hub messaging for a whole host is the worst-case blast radius of any remote item.

#### C13. Diff session: changeset load and comment save
- **Entity:** one `DiffSession`/`DiffState` (which changeset is shown, which is being read, whether it failed) and each local comment op on its way to `comments.json`.
- **Machine:** load is `loading: bool` + `error: Option` over a generation protocol and a digest gate that publishes only on change; the scope is held in three places; "dead session" (backend never opened) is unnamed. Saves are an `unsaved` overlay popped positionally, `store_error` cleared by any successful read, and `SeenState.comments` set to `None` on a failed save. Delivery to the agent is a persisted `sent` flag plus one global `sending` bool.
- **Failure modes:** bugs #24 to #29 (sticky error; can't return to shown scope; Loading forever; comment loss; `sending` stuck; PR error suppressed). Also: desktop Try Again on an open failure calls `refresh()` into a session with no workers, while the TUI reopens (`desktop/views/diff_view/mod.rs:529-532`; W); a comment edited after it was sent is never re-sent; VISUAL anchor isn't reset by `reflow`, so `y`/`c` act on other rows (`app/diff/state.rs:1180-1185, 1825-1830`); desktop `list_focus` mirror drifts after a palette toggle (`desktop/views/diff_view/mod.rs:350-525`).
- **Merged from:** app #1, #2, #14, #18, Minor (VISUAL anchor); desktop-views 13; tui Minor (`open_failed` heuristic).
- **Files:** `app/diff/session.rs:150-420, 500-735`; `app/diff/state.rs:590-625, 1085-1095, 1750-1760`; `app/diff/comments.rs:96-170`; `tui/shell/diff_view.rs:194-211`; `desktop/views/diff_view/mod.rs:350-532`.
- **Verification:** V (#24, #25, #27, #28, #29); W (#26, Try Again).
- **Workers:** make explicit: one load enum `Opening | Dead | Idle{shown} | Loading{scope, gen} | Failed{scope, why, shown}`; reset the digest on failure; keep failed ops as `Failed{op}` with retry; split read/save errors.
- **My view:** agree. Comment loss is the only user-data-loss bug in the survey; fix it first (keep the failed op in the overlay), independent of the enum.

#### C14. Client optimistic card mutations (TUI move overlay, desktop pending/held map)
- **Entity:** a card's displayed status/slot while a client-initiated write is in flight.
- **Machine:** TUI `PendingMove{rx, queue, shown_status_id}` per task: InFlight → InFlight+Queued → Settled (waiting for the board to agree) → Absent, with no Expired state and no timestamp. Desktop `BoardState.pending` (`acked`, `landed`, `prev`) plus `held` commands while the issue's write lane is busy, settled by board-read revisions; desktop has no column overlay. Optimistic filters/column overrides in both clients are overwritten by in-flight reads.
- **Failure modes:** bugs #30, #31, #32. Also: two desktop "can I act on this card?" predicates disagree (`change_in_progress` vs `begin_drag`/`drop_target_status`, `board_state.rs:1977` vs `:2140, 2173`); older-daemon `Ok(None)` acks can leave a card undraggable forever because the settle fallback compares a column against a status id (`:631`); TUI quit/switch drops queued hops and in-flight errors (persist threads are detached, `kanban.rs:3176`); desktop project switch drops held commands; daemon persister reports neither `merged` nor `cleanup_error` (`kanban.rs:248`); board view prefs revert for a cycle when a read started before the write lands after it.
- **Merged from:** tui H2, L3; desktop-views 1, 19; app #11 (board load flags and search `remote` fight).
- **Files:** `tui/kanban.rs:162-283, 874-885, 1640-1840, 1904`; `desktop/views/board_state.rs:248-379, 497-640, 1771-2180`; `app/board.rs:657-1136`; `app/board_search.rs:193-401`.
- **Verification:** V (#30, #31); W (#32 and the predicate split).
- **Workers:** make explicit: `MoveState { InFlight, Settled{since, confirmed} }` with a settle timeout, reconcile only after warm reads, drain on switch/quit; desktop per-card enum with one `blocks_follow_up()`/`blocks_drag()`; release held commands one at a time; consider moving the overlay into shelbi-app.
- **My view:** agree, and move it into shelbi-app: the TUI and desktop solve the same problem with opposite designs today (overlay vs none), which is a parity gap as well as a bug source.

#### C15. Client main-area navigation and review open flow
- **Entity:** what the main area shows (session, native view, review, workspace) and the async "open a review/workspace" flow feeding it.
- **Machine:** TUI `MainView` plus about ten side fields (`review`, `workspace`, `*_opening`, `*_rx`, `review_open_pending`, `content_on_open`); the review open goes resolve → (picker) → load → wait for the daemon's `ReviewOpened` → build. Desktop has `Route` + `settings_over_review` + `workspace_return` + `detail_over` + `ReviewLoader` + `quiet_review`. Shared `ReviewLoader` is used only by desktop; the TUI re-implements it. Inside the review/workspace interfaces, content view (Chat/Editor/Diff/DiffWait) and focus are flag soup, and decisions (`ReviewDecisions`) are mirrored into a `merging` bool each client pushes.
- **Failure modes:** bugs #33, #34. Also (W unless noted):
  - TUI: both interfaces can be live at once; the draw paints both while keys go to the review (`tui/shell/mod.rs:6863-6872, 5835`).
  - One channel carries open-resolve and content-ensure, so an Edit during a resolve strands `review_opening` (`mod.rs:5073`).
  - `review_open_pending` has no timeout; a much later `ReviewOpened` steals the view.
  - Stale `editor_ready` answers take the pane back from Chat; workspace `ContentReady` has no staleness check (`tui/shell/review.rs:324-335`; `mod.rs:5585-5593`).
  - A connect worker can fire `Ensure` after Close, re-creating a session (`tui/shell/session.rs:197-227`; `app/session/relaunch.rs:122`).
  - Mouse bypasses focus guards and the merging gate, so mid-merge clicks route keys to the agent (`review.rs:795-804`).
  - Project switch clears `decisions` while the reply thread runs, so close-sessions is skipped and a second decision is allowed on a merging task (`mod.rs:2582, 5296-5310`).
  - Scrollback search prompt survives view and project changes and swallows keystrokes (`mod.rs:1549, 5914-5980`).
  - Desktop user-shell toggle races on → off → on with no attempt token (`desktop/views/workspace.rs:1043-1080`; `review.rs:1336-1375`); workspace details refresh can be lost.
- **Merged from:** tui H1, M1, M3, M12; app #13; desktop-views 4; desktop-shell M4.
- **Files:** `tui/shell/mod.rs:1400-1460, 2318-2360, 4830-5300`; `tui/shell/review.rs:114-804`; `tui/shell/workspace.rs:82-517`; `app/review.rs:590-1006`; `desktop/views/review.rs:336-1375`; `desktop/app.rs:4512-4810, 6410-6635`.
- **Verification:** V (#33, #34).
- **Workers:** redesign the TUI main area as one `MainArea` enum with generation-ticketed opens; TUI adopts `ReviewLoader`; a visit token on desktop; key decisions by `(project, task)`.
- **My view:** agree. The common root is "async answer applied without checking what the user did since". A shared visit/generation token in shelbi-app would fix both clients.

#### C16. Settings save/load pipelines and the leave guard
- **Entity:** every Settings form that reads or writes config off the UI thread, and the "unsaved edits, leave?" prompt.
- **Machine:** "save in flight" is hand-written about nine times with drifting guards (`SaveStatus` defined twice; per-page `pending`/`saving`/`queued`/`busy`); only `MachineForm` has a generation. `LeaveGuard` is two independent Options (`prompt`, `after_save`); the TUI computes "saving" from four sources. Page reloads after external edits use one-shot `reload_on_return` bools consumed by the first window activation.
- **Failure modes:** bugs #35 to #38. Also:
  - TUI guard outlives a project change; a later unrelated save runs `go()` on the stale leave (`tui/shell/settings.rs:1213-1230`).
  - A Properties prompt-save failure is reported on the General form (`:1064`).
  - Paste reaches editors under the prompt (`:1121`).
  - Editor work spawn failure is reported under the wrong ticket, leaving shared `busy` stuck and every Settings leave blocked (`tui/shell/mod.rs:3716-3791`).
  - A save doesn't supersede an in-flight load, so stale bindings are applied (`tui/shell/keybindings.rs:250-304`).
  - Conflict state is detected by `e.starts_with("Changed on ")` (`app/settings/workflow_editor.rs:1102`; `agent_editor.rs:894`).
  - Desktop `reload_on_return` is spent before the external edit is saved; dropped reloads are never retried; read errors are invisible when a draft exists; a stale read can revert a fresh save (`desktop/views/workflows.rs:171-221`; `workflow_editor.rs:630-650, 1440-1446`).
  - Desktop cross-window clobber of the whole `desktop:` block (`desktop/views/settings.rs:285-295`).
  - Properties `dirty()` ignores tracker typing.
- **Merged from:** app #3, #12, Minor (AgentsState UI modes, `Job::stale_for`, keymap migration on read); tui H3, M8, M9, M10, L2, L4; desktop-views 2, 5.
- **Files:** `app/settings.rs:370-560`; `app/settings/{general,orchestrator,properties,keybindings,workflows,agents}.rs`; `tui/shell/settings.rs:480-2300`; `desktop/views/settings.rs:369-1600`; `desktop/views/{workflows,agents,workflow_editor,agent_editor}.rs`; `state/user_config.rs:821-852`.
- **Verification:** V (#36, #37, #38); PV (#35).
- **Workers:** make explicit: one shared `SaveSlot<T>` (idle / saving{generation, queue} / saved / failed) with one load-during-save policy; `LeaveGuard` as one enum; typed conflict; load generations; reload on every activation.
- **My view:** agree, and treat it as the template for the cross-cutting client async pattern (section 3, P4): build `SaveSlot`/`Ticket` once, then port.

#### C17. Desktop remote host lifecycle and the shared host link
- **Entity:** each other machine a desktop window has tabs on (`HostTabs`), and the process-wide `HostLink` relay per host.
- **Machine:** per window `remote`, `bar{Stopped|SignIn}`, `changed`, `opening{Idle|Opening|Failed}` and a `_watch` task: Unopened / Opening / Failed / Open / Open+Bar / Changed. Per host `HostLink.current` + `opening` (NeverOpened / Up / Dropped / Opening / FailedPaused / FailedExpired / SignInRefusedHeld), and a per-connector `refusal` side channel because `ConnectFailure` has no Refused variant.
- **Failure modes:** bugs #39, #40. Also: moving or tearing off a tab clones only `RemoteWindow`, losing the source's bar/changed/opening verdict (`desktop/app_tab_tearoff.rs:268-270`); bar and Changed can coexist (`app.rs:2096-2099`); overlapping `open_host` decides with no generation (`:2194`); a DaemonStopped refusal becomes `ConnectFailure::Message`, which panes treat as a death and burn their restart budget on (suspected; `app/session/remote.rs:107-132`; `desktop/views/session_view.rs:780-781`); remote session indicator collapses Failed/GaveUp into "connecting" (`desktop/views/remote_session.rs:345-365`).
- **Merged from:** desktop-shell H2; app #5; desktop-views 18; app Minor (`ConnectFlow.stopped`).
- **Files:** `desktop/app.rs:1424-1470, 1804-2267`; `desktop/app_tab_tearoff.rs:268-432`; `app/launch/link.rs:281-364`; `app/session/remote.rs:80-132`; `app/remote_window.rs:108-118`.
- **Verification:** V (#39, #40).
- **Workers:** make explicit: `HostState { Opening(gen), Failed, Open{remote, watch, bar}, Changed }` owned per window and moved with the tab; `HostLinkState` enum; `ConnectFailure::Refused`.
- **My view:** agree; #39 is a one-line fix (clear `_watch` when the loop returns, or check `is_finished`).

### MEDIUM

#### C18. Hub daemon process lifecycle, reload and resurrection
- **Entity:** the one `shelbi daemon` per root and every client's view of it.
- **Machine:** Absent → Starting (lock held, slow pre-bind housekeeping) → Bound (no pid yet) → Serving → Draining (`stop` set, hub socket still answers hellos, control socket refuses) → Closing → Exited, held in two atomics plus `hub.sock.lock`, `hub.sock`, `control.sock` and `shelbi.pid`. "Is it running?" is answered four ways. `shelbi reload` sequences handoff → signal clients → restart daemon → replace orchestrator with no record of which step was reached. Session hosts run a watchdog that restarts the daemon whenever its lock is free and a project is open.
- **Failure modes:**
  - A draining daemon is accepted as up by `ensure_daemon_running_with` (`state/daemon_lifecycle.rs:211`), so a project opened during a drain (or racing the idle monitor's check-then-stop) is served by a daemon about to exit (`cli/commands/daemon/serve.rs:604-650`).
  - Pid write failure is only logged, after which stop/restart/reload refuse (`serve.rs:357-359`).
  - Reload false success (bug #41).
  - Exit reason never recorded.
  - `stop_daemon_with` can report `Stopped` for a survivor (`state/daemon_lifecycle.rs:458-462`); a flock error reads as "held".
  - Watchdog correctness rests on every stop path clearing `open` first; an operator `daemon stop` on an open project is undone within ~25 s by every session (`session/daemon_watchdog.rs:94-96`).
  - `install.sh` decides the restart by grepping the literal `shelbi daemon: running` (`scripts/install.sh:160-178`).
  - The per-project poller manager blocks on a barrier (`cli/commands/daemon/poller.rs:79-108`).
- **Merged from:** cli-daemon H1, M1, L4, L6, Minor (`lifecycle.rs:270-285`, stale docs); runtime 6, 11; state-core L10.
- **Files:** `cli/commands/daemon/serve.rs:290-430, 490-690`; `state/daemon_lifecycle.rs:56-465`; `cli/commands/reload_session.rs:60-120`; `session/daemon_watchdog.rs`; `cli/commands/daemon/lifecycle.rs:250-301`.
- **Verification:** V (drain keeps accepting while jobs run; `ensure` returns on any answer; reload Err path).
- **Workers:** make explicit: `DaemonPhase { Starting, Serving, Draining, Exiting(reason) }` behind one atomic, carried in the hello; one `daemon_state()` classifier.
- **My view:** downgraded from the worker's high to medium. The drain window only exists while control jobs are running, and the session watchdog restarts the daemon within ~25 s, so the harm is a pause rather than a wedge. Carrying the phase in the hello is still cheap and worth doing.

#### C19. Event-delivery cursor and follower registry
- **Entity:** `<project>/event-cursor` + `.hwm`, the `event-follower` pid registry, `event-delivery.lock`, and the v1 feed / v2 stream+hooks consumers.
- **Machine:** cursor states absent / trusted / rewound (hwm repair) / expired (fast-forward) / future (fails closed) / legacy offset; feed Idle / InFlight / Redelivering / Quarantined / Resynced; stream Owning / Yielded / Ended. SIGTERM means "stop" or "yield", told apart by re-reading the registry within a grace window.
- **Failure modes:** bugs #42, #43. Also: `events drain` acks everything the v2 stream hasn't shown, forbidden only by prompt text (`cli/commands/orchestrator_awareness.rs:512-516`); a stream's reclaim check is separate from `claim_event_follower`, so it can SIGTERM a newer v1 follower (`:345-352`); no recovery from a future cursor (pinned at `orchestrator.rs:1768-1774`); events cut by `cap()` are acked and only summarized; rotation judges consumer liveness from cursor mtime.
- **Merged from:** state-core M4, M5; cli-daemon M2, Minor (`ack_delivery` RMW, `event_kind` classifier, `cap()`).
- **Files:** `state/event_log.rs:1201-1405, 1490-1560`; `cli/commands/orchestrator.rs:290-540, 790-960`; `cli/commands/orchestrator_awareness.rs:150-520`; `orch/wake.rs:3106-3108`.
- **Verification:** V (#42, #43).
- **Workers:** make explicit: one `DeliveryConsumer` with a single ack function under the delivery lock; hooks print before acking; retire or lock v1 writers; pid + start time in the registry.
- **My view:** agree. Decide whether v1 is still supported; if not, deleting it removes most of this entry.

#### C20. Codex bridge delivery (event queue, NativeBridge gate, engagement file, RPC connection)
- **Entity:** queued orchestrator event batches for a Codex thread, the in-process `NativeBridge`, `codex-thread.json`, and the JSON-RPC connection.
- **Machine:** `DeliveryStatus{Pending, InFlight, Delivered, Injected}` assigned directly in many places, plus implicit "applied" and "discarded"; the bridge gate is seven flags and five deadlines over `ThreadPhase` and a generation; engagement is file present/absent/archived + `native_active` + reason; the RPC connection is two bools.
- **Failure modes:**
  - No failed state, attempt cap or backoff on replay: Delivered → Pending whenever the thread is idle and the cursor hasn't moved, so a non-draining orchestrator gets the same batch every idle (token-burning loop) (`orch/wake.rs:2935-2950`).
  - `awaiting_nonsteerable_completion` has no deadline; a missed `turn/completed` blocks all delivery silently (`wake.rs:1022-1027`).
  - Gate written three times with differences (`:971-977, 1052-1058, 1196-1204`).
  - Startup retries forever while the file still says `native_active: true`, so health reports Structured with no TUI (`wake.rs:274-292, 2333-2339`).
  - Two archives in one second overwrite each other.
  - Only a timeout poisons the RPC connection; id mismatch or malformed JSON leaves it misaligned but "usable" (`orch/codex_rpc.rs:133-188`).
  - The bridge reads the cursor without the delivery lock.
- **Merged from:** orch-core M3, M4, M5, Minor; orch-workspace L5.
- **Files:** `orch/wake.rs:367-501, 567-1279, 2029-2433, 2704-3108`; `orch/codex_rpc.rs:66-188`.
- **Verification:** W.
- **Workers:** make explicit: one `transition(batch, event)` with a persisted replay count/backoff or a `Stalled` state; a `Link` enum and one `can_deliver(kind)` with a forced-rehydrate deadline; persisted `Engagement { Starting | Native | Degraded }`; `Conn { Ready, Poisoned, Closed }`.
- **My view:** agree; the unbounded replay is the one to cap first (cost exposure).

#### C21. Job and reminder lifecycle
- **Entity:** an awareness command job (and pid/ci/pr watches) and a reminder, across `awareness.json` `jobs[]`, `jobs/<id>/{result.json, output.log, env.json}`, `history.json`, the runner pid, and the clients' job/reminder sheets.
- **Machine:** Starting (no pid) → Running → Overdue (derived) → Exited-unreported → Lost → Reported / Canceled, re-derived each tick from four sources with a 120 s start grace. Reminders: Pending → Fired (recurring back to Pending) / Canceled. Sheets: Idle → Canceling → CancelFailed, settled by mirroring a later read.
- **Failure modes:** bug #53. Also: a failed `set_job_runner_pid` (warn only) lets the tick report a still-running job as lost after 120 s (`orch/job_exec.rs:107-109`; `orch/awareness.rs:404-422`); worker-routed reports go on a detached thread after the store commit, so a hub exit loses them, contradicting "a report is never lost" (`orch/awareness.rs:231-235, 566-579`; `orch/job_route.rs:15-18`); events appended inside the lock before `atomic_write` re-fire on a failed write (at-least-once, doc claims exactly-once); cancel removes the entry before recording history, so a crash leaves it in neither; bare `pid_alive` keeps a recycled pid "running" (only cancel checks identity); `job log -f` can follow forever with no daemon; `env.json` holding the caller's env leaks; `JobEvent.status` documents `lost` but `JobStatus` has no `Lost`; client sheets apply a cancel result to whichever sheet is open and never clear `canceling` on job success (desktop); torn reads show a finishing job twice or not at all for a tick.
- **Merged from:** orch-core M7; state-core M6; cli-daemon M3, L10, Minor (`watch --pid`); app #19; tui M7, Minor; desktop-views 11.
- **Files:** `orch/awareness.rs:209-579, 851-973`; `orch/job_exec.rs:100-175`; `state/awareness.rs:191-905`; `cli/commands/job.rs:119-460`; `tui/jobs.rs:98-321`; `desktop/views/jobs.rs:156-640`.
- **Verification:** V (#53); rest W.
- **Workers:** make explicit: a `JobPhase` on `Job` with one transition fn in `shelbi_state::awareness`; record-then-remove under one lock; identity-checked pids; a persisted outbox for worker-routed notices; shared sheet cancel state in shelbi-app.
- **My view:** agree.

#### C22. GitHub tracker write path (labels, composite writes, rate-limit park, ordering)
- **Entity:** one GitHub issue's column (`shelbi:status/*` label × open/closed) and the per-token budget that gates writes.
- **Machine:** status = (status label, state) with invalid combinations representable (open + terminal label, two labels, no label, label past the first ten). Writes: one verified PATCH for moves, but create-into-terminal is POST-open then close; composite reject/park/unassign are multi-step. Budget tiers free / parked(reset | retry-after | backoff) plus an unreachable breaker in `gh-budget/<key>.json`, with expiry implicit in `park_verdict`.
- **Failure modes:** bugs #44, #45. Also: a failed close after create-into-terminal leaves an open issue with a terminal label that reads as backlog and triggers a false "reopen" (`state/github_store.rs:1240`); duplicate feedback on retry after a move failure; production write retries sleep the server's reset hint verbatim (up to an hour) on the calling thread (`state/gh_retry.rs:233, 255`); a flock failure reads as "parked"; `record` is last-writer-wins; N sequential priority rewrites leave a torn column on mid-loop failure and Linear `add` can duplicate (check-then-create); the module doc says the store holds no board state while it caches issues and ETags.
- **Merged from:** state-core M1, M2 (remote side), M3, L3, L4.
- **Files:** `state/github_store.rs:1240, 1321-1454, 1456-1720, 1919-2001, 2813-2837, 4195-4205`; `state/gh_budget.rs:290-641`; `state/gh_retry.rs:212-262, 489-503`.
- **Verification:** V (#45); PV (#44: code path confirmed, not reproduced against an issue with >10 labels).
- **Workers:** make explicit for writes (route every status write through the verified move; hydrate labels); document the budget and make two small fixes.
- **My view:** agree; #44 deletes user data on GitHub and should be fixed regardless of the rest.

#### C23. Tracker read mirror and reconcile (board refresh, caches, reconcile baseline)
- **Entity:** the hub's local view of the remote tracker: `board-index.json`, the per-process `CachedIssueStore` snapshot, the daemon's per-project refresh loop, and the reconcile baseline that tells hub moves from external ones.
- **Machine:** board Cold / Warm / Stale / Refreshing with freshness derived twice; refresh Due/NotDue × Healthy/FailingEpisode/Paused with pause in memory and failure on disk; reconcile off / unseeded / seeded{cursor, seen_status} / backoff.
- **Failure modes:** a failed `append_task_event` still advances the reconcile watermark, losing the change (`orch/poller.rs:1051`); "was this our move?" depends on `events.log` and a read error double-counts hub moves; switching GitHub → Linear doesn't reset the baseline (`:1159`); a refresh that started before a write publishes the pre-write board as Warm for a full TTL (no generation, `state/issue_cache.rs:147`); the refreshing flag isn't reset on panic; "only a daemon index is ever Warm" is enforced by comment; failed token resolution is cached as `None` until the project closes (`cli/commands/daemon/board.rs:785-808`); one manager thread serves every project; Linear deltas keyed on the local clock; `poll_changes` strictly-greater watermark (no production caller).
- **Merged from:** orch-core M9; cli-daemon M13, Minor (two freshness classifiers); state-core M13, L5, L8, L9.
- **Files:** `orch/poller.rs:1000-1250`; `cli/commands/daemon/board.rs:82-810`; `state/issue_cache.rs:88-476`; `state/board_index.rs:375-882`.
- **Verification:** W.
- **Workers:** mostly document; a write-generation before `publish`, a drop guard for the flag, reset reconcile on any backend change.
- **My view:** agree with "document plus small fixes". The watermark-advances-on-failure item is the one real correctness bug.

#### C24. Tracker connection and Linear config files
- **Entity:** the tracker backend config, `statuses.yaml` `managed_by`, and `projects/<p>/linear.json` (filter cache, sync stamps, retired statuses).
- **Machine:** connect = validate → empty-board check → preflight → write config → filter cache → status sync, with no transaction; (backend, managed_by) has an invalid-but-reachable combination (fs/github + linear); `linear.json` has three owners writing read-modify-write with no lock.
- **Failure modes:** bug #46. Also: lost updates across processes in `linear.json` and a corrupt file reads as empty (`state/linear_client/filters.rs:179-270`); a crash between writing `statuses.yaml` and `retired_statuses` loses the retired ids for good (`state/linear_statuses.rs:319-326`); the daemon catalog write can clobber a concurrent Settings edit; two slug rules mean a state used before sync reads under a different id after it; Linear budget park is unlocked RMW; `tracker linear sync --rewrite-workflows` writes the catalog before the workflows; failed connect caches are `let _`.
- **Merged from:** state-core M11, M12, L6, L7; orch-workspace L3; cli-daemon L1, Minor (`issue_tracker_setup` filter fallback).
- **Files:** `orch/tracker_connect.rs:184-266`; `state/linear_statuses.rs:62-430`; `state/linear_client/filters.rs:179-270`; `state/tracker_config.rs:30-58`; `cli/commands/tracker.rs:300-610`.
- **Verification:** V (#46).
- **Workers:** make explicit: disconnect clears `managed_by` (or derive read-only from backend + managed_by); `acquire_file_lock` around `linear.json`; write retired before catalog.
- **My view:** agree. Fresh code (#2333/#2335 landed this week), so cheapest to fix now.

#### C25. Project and hub config writers
- **Entity:** project YAML (`projects/<p>.yaml` or in-repo `project.yaml` + `local.yaml`), `config.yaml`, `keys.yaml`, workflow/agent dirs, and the in-repo migration.
- **Machine:** three uncoordinated write protocols for project YAML (content-hash plans serialized only by the daemon's in-memory lock; `PoolConfig::save` under its own lock; `set_orchestrator_runner` with neither), plus unlocked hub-file patches. Multi-file Settings plans run op by op with no partial state. Config mode is decided two ways (`local.yaml` exists vs `config_mode`).
- **Failure modes:** lost updates between any pair of writers (`state/orchestrator_setup.rs:127-189`; `state/user_config.rs:821-852`); in-repo migration can strand workflows when a concurrent `load_project` self-heal creates the move destination (suspected; `state/migrate.rs:216-233, 378-381`; `state/lib.rs:1319-1322`); loads write into the user's repo; a save that adds and removes statuses writes `statuses.yaml` last, so workflows briefly reference undeclared statuses; agent rename parks under `.renaming-<name>` which nothing sweeps and `list_agents` hides (`state/agents_config.rs:1220-1233`); load-time `migrate_legacy_zen_toggle` re-serializes `config.yaml`, dropping comments; creating a workflow named `default` silently re-routes `workflow: default` issues; pool edits leave a renamed workspace's `status.yaml` and don't migrate machine records.
- **Merged from:** state-core M10, M14, L14, L15, L16; orch-workspace M7; cli-daemon Minor (`agent new`); desktop-views 2 (the lock-free `update_user_config` root).
- **Files:** `state/config_write.rs:181-370`; `state/workflow_config.rs:84-87, 1201-1399`; `state/orchestrator_setup.rs:87-189`; `state/migrate.rs:184-381`; `state/user_config.rs:800-852`; `orch/pool_admin.rs:163-864`.
- **Verification:** PV (unlocked writers confirmed by C16 checks; migration race suspected).
- **Workers:** make explicit: one serialization point for project YAML (file lock or daemon `SaveConfig`); move `set_orchestrator_runner` onto a plan; exclusive migration.
- **My view:** agree.

#### C26. Config-upgrade finding lifecycle
- **Entity:** one `UpgradeFinding` across boots and reloads, plus `~/.shelbi/config-upgrade-findings.json`.
- **Machine:** Detected-AutoHeal → applied, or Detected-NeedsJudgment → surfaced → approved-and-applied / approved-and-refused → Resolved. Missing: Declined/Kept; unnamed: AutoHeal-blocked.
- **Failure modes:** bug #47; "keep it" can't be recorded, so several codes re-raise every boot; ids hash the message text, so a reword invalidates held ids; a crash between writing `task.yaml` and unlinking `default.yaml` creates a permanently blocked heal (`cli/config_upgrade_apply.rs:752-775`); absence sniffers re-create deliberately deleted defaults; the findings file has no production reader and is overwritten per project; healers use a fixed temp name with no lock; AutoHeal codes and healers are two string dispatchers with no exhaustiveness check.
- **Merged from:** cli-daemon M9, Minor (dispatch tables).
- **Files:** `cli/config_upgrade.rs:155-466, 2700-3373`; `cli/config_upgrade_apply.rs:199-353, 752-1342`; `state/default_orchestrator.md.template:726-728`.
- **Verification:** V (#47).
- **Workers:** make explicit: a decision store keyed by stable finding key (Kept / Approved / Blocked); reclassify skipped heals; one exhaustive code→healer table.
- **My view:** agree.

#### C27. New-project creation flow
- **Entity:** a project being created (registration, `project.yaml`, welcome task, agents, workflows, guard, Linear caches) and the three front-end wizards that drive it.
- **Machine:** engine stages Config → Agents → Workflows → guard → Linear, publishing the registration first and recording no partial progress; `Attempt::{New, Retry}` exists but only the desktop wizard remembers to retry. Three step machines (CLI `answer_steps`, TUI `Form`, desktop `Wizard`) agree only through a byte-identical test.
- **Failure modes:** a half-created project looks complete to the CLI (`init -y` says "already configured", bare `shelbi` treats setup as done) and the TUI hits the registered-id clash; pick-up ignores `attempt` and registers a second alias (`orch/project_create/pick_up.rs:82`); Retry may be refused for GitHub/Linear projects because the welcome `.md` is written before registration (suspected); the scaffold lock starts after the collision check; seeding discards the user's model choice when another folder's check lands (`app/add_project_wizard.rs:520-525, 673-675`); TUI launch screen accepts Enter/Esc/second submit during a create and can exit mid-scaffold (`tui/launch/mod.rs:181-314`); desktop ⌘W mid-create leaves a half-created project and a fresh wizard sends `New`.
- **Merged from:** orch-workspace M6; cli-daemon M16; app #9; tui M6, L7; desktop-views 3 (Add Project part).
- **Files:** `orch/project_create/mod.rs:78-95, 306-598, 946-1124`; `orch/project_create/pick_up.rs:42-111`; `cli/wizard.rs:146-367`; `app/add_project_wizard.rs:373-1303`; `tui/launch/mod.rs:100-320`.
- **Verification:** W.
- **Workers:** make explicit: a persisted creation record beside the registration so any front end can resume; one shared flow machine in shelbi-app.
- **My view:** agree; the persisted creation record matters more than the shared wizard.

#### C28. Zen pinned-PR flow (probe, publish, CI, land)
- **Entity:** one task's probe → publish → CI → land flow at `<project>/zen-flow/<task>.json`, the local ref / remote branch / PR head triad, probe worktrees and the PR-status mirror.
- **Machine:** `ZenFlowPhase{Probed, Published, CiGreen}` exists but each step is a separate CLI process doing load-check-save with no lock and no prior-phase validation; the retry contract lives in exit codes and the orchestrator prompt.
- **Failure modes:** lost updates or resurrection between `ci-watch` and a concurrent probe/finish; `zen pr-merge` bypasses the record (`orch/zen.rs:2073-2081`); failed advances only warn; push precedes verification, so `TransientVerification` leaves the remote moved with no record; `land_pr` retry after a successful merge fails instead of reporting it; a late `note_ci` flips a merged PR back to Open (`state/pr_status.rs:207`); CI verdicts go to different sinks from `ci_watch` and the scoped poll; probe worktrees and `refs/shelbi/probe-base/*` leak; non-Zen builds don't take the shared-target lock; `msrv-check` exits 0 for "skipped".
- **Merged from:** orch-core M11, L6, L7, L9 (`ScopedCiState`), Minor; state-core L2, L17 (zen flow); cli-daemon L11.
- **Files:** `orch/zen_flow.rs:11-224`; `state/zen_flow.rs:24-105`; `orch/zen.rs:373-2300, 7740-9352`; `state/pr_status.rs:31-224`.
- **Verification:** W. Note: the landing gate re-verifies live (`orch/zen_flow.rs:150-224`), so most of these fail closed.
- **Workers:** make explicit: one locked transition fn in `shelbi_state::zen_flow` validating the prior phase, with `pr_merge` routed through it.
- **My view:** agree; medium because the live re-verify makes it fail closed.

#### C29. Zen mode toggle (tri-state collapsed, relative flip)
- **Entity:** `ZenModeState{Off, Paused, On}` and every client's mirror of it.
- **Machine:** clients mirror it as a bool (`zen_on = !Off`); the daemon op is a relative `ToggleZen` with no target or expected prior (Off|Paused → On, On → Off). The event line is appended best-effort after the commit.
- **Failure modes:** bug #48; two clients both turning Zen on cancel each other out (`orch/state_write.rs:32`); a stale snapshot flips the wrong way; the event line can be lost (`state/lib.rs:2769, 2891`), and the TUI caches the mode in memory.
- **Merged from:** app #8; state-core L12 (event part); orch-workspace L10.
- **Files:** `state/lib.rs:2762-2893`; `app/command_model.rs:132`; `app/command.rs:508-517`; `crates/shelbi-proto/src/control.rs:392`.
- **Verification:** V.
- **Workers:** make explicit: absolute `SetZen{to}` op; carry the tri-state into `CommandModel`.
- **My view:** agree; small and user-visible.

#### C30. Git worktree and branch state (attached / detached / mid-rebase, deferred deletes)
- **Entity:** `<work_dir>/.shelbi/wt/<ws>`, the branch it holds, and a task's branch on origin and locally with its stacked children.
- **Machine:** worktree absent / invalid / attached-task / attached-other / detached / dirty / mid-rebase (unmodelled). Branch deletion and restack return terminal "deferred" outcomes (`restack-deferred`, `RemoteDeletedLocalDeferred`, `held-by-<ws>`) that nothing consumes.
- **Failure modes:** inconsistent lock discipline (poller rebase/push/detach and `transition_move` detach take no `lock_git_worktrees`), so a poller detach can land between a dispatch's checkout and launch (`orch/workspace.rs:2788`); `release_branch_from_workspace_worktrees` detaches other workspaces' worktrees under their live agents; a kill mid-rebase leaves `.git/rebase-merge` that nothing detects; heal logic copied three times; deferred deletes leak branches on origin and leave children stacked on merged parents while the edge reports success (`orch/actions.rs:773-822, 1760-1835`); crashed `$TMPDIR/shelbi-merge-*` dirs leak; "branch exists" counts as cut.
- **Merged from:** orch-workspace M1, L6; orch-core M12, Minor.
- **Files:** `orch/workspace.rs:1222-1430, 6620-7697`; `orch/actions.rs:773-1144, 1754-1835`; `orch/lifecycle.rs:155-349`.
- **Verification:** W (orch-core says it grep-checked the no-consumer claim).
- **Workers:** document a state table, one lock rule for every worktree mutator, detect in-progress rebase; persist a pending-cleanup ledger for deferrals.
- **My view:** agree; the deferred-cleanup ledger is the same record C1 needs.

#### C31. Observed pane state and usage-limit auto-resume
- **Entity:** one workspace pane as its poll thread sees it, and its usage-limit incident.
- **Machine:** `WorkspaceState` held in memory (`last_known`) and on disk (`status.yaml`), plus six side latches reset by hand-copied blocks; `LimitResumeState{Idle, Scheduled, NeedsHuman, Resumed}` with a good pure core and about 15 direct assignments; a screen classifier whose detectors veto on inconsistent busy predicates.
- **Failure modes:** `load_prior` takes state from memory and timestamp from disk; `*last_agent_exited = false` runs unconditionally so a capture blip re-emits `agent-exited` (`orch/poller.rs:1950`); `NeedsHuman` is in memory only, so a restart sends up to three more nudges after a "don't retry" decision (`:2708, 2881`); `status.yaml` has no lock, so a poll can re-save after a clear; classifier vetoes use `is_claude_busy` vs `is_claude_working` inconsistently (`orch/ready.rs:476, 523, 566`).
- **Merged from:** orch-core M6, M8, L1; state-core L1 (lock/ordering part).
- **Files:** `orch/poller.rs:526-595, 1826-2445, 2548-2945`; `orch/ready.rs:37-606`; `state/workspace_status.rs:42-62`.
- **Verification:** W.
- **Workers:** make explicit: a `PaneObservation` struct with named resets; persist the NeedsHuman latch beside Paused; one `classify(screen)` with written precedence.
- **My view:** agree.

#### C32. Relay channel and session stream resync
- **Entity:** the hub-side `RelayChannel` per SSH host and its streams, the far-side link map, and each client connection inside a session host.
- **Machine:** relay Connecting / Open / Closed with redundant `reachable`/`closed` atomics; stream Flowing / ReplayPending / Closed; session client Handshaking / Connected / Attached ⇄ ResyncPending / Closed as flag soup. The per-host relay cache is a process-global map.
- **Failure modes:** a backpressured stream drops everything, replies and `Exited` included, until a `Resync` that an older/frozen-core session never sends (`client/relay.rs:621-650`); the session server's resync path also clears priority frames (`Exited`, replies, `Pong`) despite documenting them as undroppable (`session/transport.rs:177, 404-407`); `attach` before `hello` is accepted with no base replay; handshake bound is a no-op over a relay, so a wedged far session hangs connect forever (`client/relay.rs:476-483`); `open_stream` and `list_sessions` have no timeout; the far main loop holds the links lock during a blocking write, starving Ping/Pong; `drop_relay` removes whatever is cached, so N threads drop each other's fresh relays, and dropping the cache entry kills attached views (`orch/remote_session.rs:301-365, 612-621`); untagged FIFO replies shift after one wrong-kind reply; EOF counts as done for `QuitProject`; a refusal frame can land inside a split daemon frame.
- **Merged from:** cli-daemon M5, M6, L2, L7, Minor (relay control refusal, clean EOF); runtime 3, Minor (`ClientChannel` caps); orch-workspace M8.
- **Files:** `client/relay.rs:118-1072`; `client/connect.rs:77-424`; `client/control.rs:54-386`; `session/transport.rs:110-604`; `orch/remote_session.rs:198-365`.
- **Verification:** W.
- **Workers:** make explicit: deadlines on ReplayPending/open/list/hello; split lossy output and lossless control lanes; reject attach before hello; compare-and-remove in the relay cache.
- **My view:** agree; the lossless control lane fixes the "EOF instead of Exited" misreads in C7 as a side effect.

#### C33. Message push/ack and clarification questions
- **Entity:** one pushed worker message (`msg_id`) and a stored clarification question.
- **Machine:** `MessageDelivery{Unknown, Queued, TimedOut, Delivered, Undeliverable}` rebuilt from four booleans parsed out of `events.log` with hand-coded precedence; daemon ack timers in memory. Questions are `answered_at: Option` + `answer_msg_id: Option`.
- **Failure modes:** bugs #16, #49. Also: crash between append and `push=ok`; a failed daemon notify means no timer, so the message is Queued forever; a fast ack before the notify produces a false `ack=timeout`; questions are stamped answered on queue, not ack; an identical re-ask returns an already-answered record; delivery status is forgeable because it is rebuilt with `split_whitespace` (`state/event_log.rs:1812-1822`); the message beacon reset runs before the old pane is killed.
- **Merged from:** cli-daemon M7, M8; state-core L17 (forgeable status, questions); orch-workspace L8.
- **Files:** `cli/commands/message.rs:180-482`; `cli/commands/daemon/serve.rs:186, 695-754, 1253-1343`; `state/event_log.rs:1777-1849`; `state/questions.rs:83-275`.
- **Verification:** V (#49).
- **Workers:** document the push sequence; category-based done check; mark answered on `ack=worker`; resolve the hub-level root.
- **My view:** agree.

#### C34. Remote machine binary record (`machines.json`)
- **Entity:** the hub-wide record per machine name `{path, version, compatible, source, checked_at, host}`.
- **Machine:** absent / compatible+host / incompatible / legacy (no host), frozen at write time and never expired.
- **Failure modes:** bug #50; a renamed machine fails dispatch and the old record lingers; `compatible` is never re-evaluated after a hub upgrade; names collide across projects in one hub-global file; unlocked RMW and an unparseable file de-verifies every machine; an install that finishes after its form closed records nothing (`app/settings/machine_check.rs:415-421`); CLI status shows a stale "yes (last known)" when the binary is gone; reachability prober keys results by name, so a host edit shows the old host's result for up to 300 s (`app/reachability.rs:126-189`).
- **Merged from:** orch-workspace H6; cli-daemon Minor (`machine.rs`); app #16.
- **Files:** `state/machine_state.rs:69-111`; `orch/remote_session.rs:395-440`; `orch/machine_readiness.rs:535-797`; `orch/pool_admin.rs:526-624`; `app/reachability.rs:49-248`.
- **Verification:** V (#50).
- **Workers:** redesign: key by `(project, machine)` or host; apply `is_for_host` in the dispatch gate; recompute compatibility live; lock the RMW.
- **My view:** downgraded from high to medium. It needs a machine relocate or rename (rare operator actions) to bite, and the consequence is a failed or misdirected launch, not silent corruption. The `is_for_host` filter in `resolve_remote_bin` is a one-line fix.

#### C35. Desktop connection to Shelbi (bootstrap per window, cached Ready, control watches)
- **Entity:** the desktop's `Ready` + `ControlHandle` + relaunch latch, each window's `Boot`, and the control-watch threads.
- **Machine:** per window `Boot::{InFlight(state), Ready}`, an app-global `Shell.ready` cache never cleared, per-watch Connecting / Disconnected / Subscribed / RelaunchRequired (terminal latch per handle).
- **Failure modes:** each window without a cached Ready starts its own bootstrap (`desktop/app.rs:1735-1738, 2797-2812`), so concurrent installs make all but one fail with `Busy` until Try Again (`desktop/platform/install.rs:234-240`); the process-wide daemon launcher and start log are last-writer-wins; the relaunch latch is per handle, so another window keeps sending commands; `hello` abandons a blocked thread every 5 s timeout (`desktop/platform/control.rs:245-257`), leaking threads against a wedged daemon; a failed `open_project` loses the hand-off and its watch keeps running unheard; `Shell.ready` is stale after a `cli_path` change; a crash mid plugin swap leaves only the `.old` backup, which the next sweep deletes (`install.rs:409-420, 488-491`); a poisoned `INSTALLING` mutex reports `Busy` forever.
- **Merged from:** desktop-shell H3, L9, L10, Minor (`ControlWatcher` no backoff).
- **Files:** `desktop/app.rs:204-284, 1712-1758, 2797-2872`; `desktop/platform/bootstrap.rs:165-441`; `desktop/platform/control.rs:74-257`; `desktop/platform/install.rs:228-491`.
- **Verification:** PV (per-window bootstrap, install busy, abandoned hello threads confirmed; the N-runs-at-launch ordering not re-traced).
- **Workers:** redesign: one app-global bootstrap owner with one handle and one cancellable watch; hello with a socket read deadline.
- **My view:** downgraded from high to medium. The install race needs several restored windows on a machine with no CLI (a first-launch-after-uninstall case), and the thread leak needs a wedged daemon. The hello read deadline belongs in `client::ControlClient` (C32) and fixes the leak everywhere.

#### C36. Desktop window content mode, tabs and slots
- **Entity:** `ShelbiWindow`'s content mode, its tab list and its persisted slot, and project-bound teardown.
- **Machine:** mode is `project` + `chooser` + `new_tab` + `pending` with several invalid combinations; tabs live in four copies (`ProjectTabs.order`, `window_state.tabs`, `Shell.session.slots[n]`, `desktop-windows.json`) with a `NEW_TAB` sentinel and an `elsewhere` ownership rule; about 40 project-bound fields are reset one by one in `teardown_project`; the cross-window tab drag is a deferred two-phase session mirrored into each window's `TabReorder`.
- **Failure modes:** bug #57; the "leave the shown project" step is copied three times with different reset subsets (`desktop/app.rs:2150-2176, 3096-3155`); a tab change shortly before ⌘Q is lost (400 ms debounced detached save); closing a window never re-syncs others' tabs; an unreadable window is silently skipped, producing duplicate tabs; teardown already misses `reject_reasons` and `settings_over_review`; the drag session outlives a source window closed mid-drag; `DragState.tearable` is dead state.
- **Merged from:** desktop-shell H1, M2, M3, M8; desktop-views 16, 8 (task pane `floating` double ownership).
- **Files:** `desktop/app.rs:1461-1504, 2150-2349, 3024-3272, 3423-3438, 5253-5307`; `desktop/project_tabs.rs:39-213`; `desktop/window_state.rs:180-309`; `desktop/app_tab_tearoff.rs:67-432`.
- **Verification:** V (#57).
- **Workers:** make explicit: `enum Showing { Project, Chooser, NewTab, Pending }` with one `leave_current()`/`enter()`; app-level tab registry; one `ProjectViews` struct replaced on switch.
- **My view:** downgraded H1 to medium as part of this entry: it is a maintainability hazard with no failure shown beyond #57. The `Showing` enum is still the right refactor.

#### C37. Client async operations without request identity (dialogs, jobs, overlays, drafts)
- **Entity:** any client-side async request whose answer lands into UI state the user may have changed since. This is a cross-cutting family; the instances below don't fit the bigger entries above.
- **Machine:** "in flight" kept as a bool or `Option<Receiver>`, answers matched by variant or by "something is open" rather than by a ticket; close-while-busy decided per dialog.
- **Failure modes:** bugs #51, #52. Also:
  - Close-while-busy differs across five dialogs; Connect's cancel still records a recent host and can start the far daemon (`desktop/views/connect_remote.rs:138-193`).
  - TUI async overlay landers replace whatever dialog is up, and a failed New-task create is lost when its dialog was clobbered (`tui/shell/mod.rs:4319-4356`).
  - TUI single-slot background job drops earlier results, including failures (`mod.rs:4775-4826`).
  - Startup phase inferred from status-line text (`mod.rs:2076-2084, 4414`).
  - `IssueEdits` failure kind is a string compared to `NoAnswer.to_string()`; a NoAnswer with a workflow change never self-heals; lane liveness depends on the host calling `finish` (`app/issue_edit.rs:598-904`).
  - `LinearChoice` Teams answers unguarded by request id and Viewer not guarded at all (`app/linear_setup.rs:695-782`); `TrackerForm` records the connection from the choice at completion time.
  - External-editor watcher allows two watchers on one scratch file, double-submitting every save, with no end signal (`desktop/views/external_edit.rs:213-260`).
  - Onboarding marker-write failure after a save leaves stale hashes so retry is refused (`app/onboarding.rs:149-160, 400-426`).
  - Prereqs `validating` is one view-wide bool; `Prereqs::located` matches only the tool.
  - Launch screen has no Loading state and keys `busy` by row.
  - Feedback dialog accepts typing while sending.
- **Merged from:** desktop-views 3, 6, 9, 10, 15; tui M2, M4, M11, L5, L6; app #4, #10, #15, Minor (submit booleans, `palette_search` pending, `review_reject` late fill, `ConnectFlow`).
- **Files:** `desktop/views/{machine_form,connect_remote,add_project,workspace_dialog,feedback,external_edit,prereqs,launch}.rs`; `desktop/app.rs:6348-6374`; `tui/shell/mod.rs:1402-1660, 2758-2797, 4300-4830`; `app/issue_edit.rs`; `app/linear_setup.rs:310-782`; `app/onboarding.rs`.
- **Verification:** V (#51, #52); rest W.
- **Workers:** per instance: a ticket/generation per request, one close-while-busy rule, typed failure kinds, `Phase` enums in shelbi-app.
- **My view:** treat as the pattern P4 in section 3, not as one machine. Fix #51 and #52 directly; then fold the rest into the `SaveSlot`/`Ticket` work from C16.

#### C38. Activity feed and title cache
- **Entity:** the activity feed's events, paging window and per-task-id title cache.
- **Machine:** TUI `ActivityApp` runs two refresh protocols (standalone tail and shell snapshot) on one struct, coordinated only by `log_offset = 0`; desktop `ActivityState` + reader with the window held twice; shared `ActivityMetaCache` per id: Unknown / Resolved(mtime or TTL) / ResolvedTerminal (frozen) / ConfirmedMiss (frozen) / BackingOff.
- **Failure modes:** bugs #54, #55. Also: `r` runs a synchronous title pass on the UI thread with a cache the shell never fills; one failed read empties the desktop feed, drops selection and ends load-older (`app/activity.rs:401-406`); a confirmed miss for a just-created issue shows the raw id until restart; display labels re-derived from shared strings (`desktop/views/activity_labels.rs:145-200`). Four past incidents are recorded on the cache (storms and refetch loops).
- **Merged from:** tui M5; app #7; desktop-views 14, 17 (activity relabel part).
- **Files:** `tui/activity.rs:71-369`; `app/activity.rs:326-807`; `desktop/views/activity_state.rs:141-322`; `desktop/views/activity_reader.rs:36-84`.
- **Verification:** V (#54, #55).
- **Workers:** delete the TUI standalone path; per-id `MetaState` with one `next_action(now, mtime)`; keep last good events on a failed read.
- **My view:** agree; deleting the vestigial standalone path is the whole fix for #54.

#### C39. Terminal view state (search, selection, scroll, gestures, pointer grab, size)
- **Entity:** `TerminalView` over the emulator, `MouseGesture`, and the desktop `TerminalPane`.
- **Machine:** search matches hold absolute grid lines that go stale on scroll (no Stale state); selection mirrored in view and emulator; scroll offset mirrored; gesture enum with no cancel; desktop pointer grab is several simultaneously-settable fields; PTY size report is three fields with "hold just lifted" inferred; Kitty keyboard flags tracked in both responder and emulator.
- **Failure modes:** bug #56; selection lost on every Ink repaint and mid-drag with nothing copied; resize clamps the scroll mirror instead of re-mirroring, so the next wheel jumps; a lost Release leaves `Selecting`/`Program`/`Pressed` and can click into a swapped view; a stuck `scrollbar_drag` makes plain hovering scroll (`desktop/elements/terminal/mod.rs:1644-1646`); autoscroll keeps running while hidden; responder can answer `CSI ? u` with flags the emulator no longer has; Cmd+F mid-IME composition commits into the find query.
- **Merged from:** runtime 4, 5, 8, 9, 10, Minor (damage, size mirror); desktop-shell M5, L5, L6, L7.
- **Files:** `crates/shelbi-term/src/{view,search,gesture,scrollback,responder}.rs`; `desktop/elements/terminal/mod.rs:316-1750`; `tui/shell/terminal_view.rs:91-184`.
- **Verification:** V (#56).
- **Workers:** make explicit in small pieces (mark search dirty in `feed`; report selection lost; `Grab` enum; derive scroll base from the emulator; answer Kitty queries from the emulator).
- **My view:** agree; local and well-bounded, low design cost.

#### C40. Client session pane lifecycle (connect, live, reconnect, restart, give up)
- **Entity:** the session a client view shows (orchestrator, agent, review, workspace, remote).
- **Machine:** explicit cores (`SupervisionState`, `RestartController`, `ReconnectController`) with leaky edges; each client owns its own `Slot` enum and sequences controller calls by hand. Representable but invalid: after give-up `dead()` returns `Backoff{MAX+1}`; `deliberate_close` stays set after a failed teardown; reconnect and restart backoff active together.
- **Failure modes:** a path calling `dead()` on a given-up pane renders "restarting (6/5)" and desktop schedules backoff ticks forever; manual reconnect `restart.begin(false)` resets a GaveUp pane's crash budget (`desktop/views/session_view.rs:568`; `tui/shell/session.rs` reconnect); desktop's `_tick` is shared by two timers; deliberate close is fanned out per view, so a view created between mark and reply restarts the exit as a crash (`desktop/app.rs:5760-5791`).
- **Merged from:** app #6; desktop-views 7; tui L1; desktop-shell L2.
- **Files:** `app/session/restart.rs:43-131`; `app/session/reconnect.rs:52-152`; `app/supervision.rs:82-199`; `desktop/views/session_view.rs:218-965`; `tui/shell/session.rs:56-484`.
- **Verification:** W.
- **Workers:** make explicit at the edges: sticky `GaveUp`; move `Slot` into shelbi-app; split `_tick`.
- **My view:** agree; medium-low.

#### C41. Desktop notification tracker
- **Entity:** `notify::Tracker` per window: unprimed or primed with last badge per workspace, last review set, last orchestrator state, and post times.
- **Failure modes:** bug #58; a restarted orchestrator's first idle reads as "finished a turn" because the state isn't reset on session change (`desktop/notify.rs:189-191`); background projects are silent and tracking re-primes on every switch; `posted` is never pruned; `orchestrator_title` in the feed is never reset on a `None` probe, so a same-title restart sends nothing (`desktop/refresh.rs:363-374`).
- **Merged from:** desktop-shell M6, L4.
- **Files:** `desktop/notify.rs:100-244`; `desktop/refresh.rs:300-374`; `desktop/app.rs:1643, 1809, 3071`.
- **Verification:** V (#58).
- **Workers:** document the transitions-only contract; clear on removal; reset on session change; decide per-project vs per-window.
- **My view:** agree.

#### C42. Desktop file-promise drop pipeline
- **Entity:** one AppKit drag through to gpui's `on_drop`.
- **Machine:** Accepted → Materialising(remaining) → Landed → Delivered or Dropped; only the counter is explicit.
- **Failure modes:** reported accepted before any file exists with no timeout or failed state, so a reader block that never fires loses the drop silently (`desktop/drops/macos.rs:192`; `pasteboard.rs:247`); `promised_count.max(1)` fires `done` early when more files arrive; replayed at a point, not a pane; a window closed meanwhile drops it silently.
- **Merged from:** desktop-shell M7.
- **Files:** `desktop/drops/{mod,macos,pasteboard}.rs`.
- **Verification:** W.
- **Workers:** make explicit: per-drop record targeting a pane, timeout/failed state with a note.
- **My view:** agree; medium-low.

### LOW (kept as entries because they cross slices)

#### C43. Handoff notes (`HANDOFF.md` consume-on-read, orchestrator `handoff.md`)
- Two handoff protocols: `status --handoff` reads, prints and deletes `<work_dir>/HANDOFF.md` with no claim step (two runs both print it; a newer note written in between is deleted unread; docs put it under the state root), while the orchestrator's `agents/orchestrator/handoff.md` uses rename-claim but returns `Written` the instant the file appears, so a partial write can be torn down. Sources: cli-daemon M15; orch-core L5. Files: `cli/commands/status.rs:602-646`; `state/agent_workspaces.rs:405-549`; `orch/handoff.rs:179-199`. W. Workers: one path and the rename-claim pattern. My view: agree; rated low because it's a session-boundary convenience, not a data path.

#### C44. Event-log status vocabulary parsed from display strings
- `events.log` status tokens and labels are matched as literals in at least three parsers (`app/activity.rs:2339-2490`, `orch/wake.rs`, `state/event_log.rs` `EventKind::from_body`) and the desktop re-parses typed state from display text (board freshness `contains("stale")`, section from row labels, activity relabel). Unknown tokens fall to alarming defaults (merge not `"ok"` renders red). Sources: app Minor; desktop-views 17; cli-daemon Minor (`event_kind`). W. Workers: shared token enums in `shelbi-state` `event_log`; typed fields on `SidebarModel`. My view: agree; cheap and removes a class of silent regressions.

#### C45. CI smoke and release scripts as sleep-driven machines
- `tui-smoke.sh` sends keys 0.3 s apart with no check that the palette opened (CI flake on loaded runners); `wait-for-apt-consistency.sh` has no curl timeouts and exits on one consistent read. Sources: runtime 7, 12. Files: `scripts/ci/tui-smoke.sh:131-226`; `scripts/release/wait-for-apt-consistency.sh:73-155`. W. Workers: poll `capture-pane` before Tab; curl timeouts. My view: agree; fix when the flake next bites.

---

## 3. Cross-cutting patterns

**P1. Multi-step side effects journaled only by marker files or not at all.** An irreversible step (merge, kill, spawn, remote write) is followed by steps whose only record is "a file is still there" or nothing. A crash or error between steps either repeats the early steps or silently drops the late ones, and each caller picks its own failure policy. Seen in C1, C2, C5, C6, C21, C22 (composite writes), C24, C25 (Settings plans, agent rename), C27, C28, C30. The common fix is one small persisted "operation in flight" record per entity with a phase field, written before the irreversible step and advanced after each one.

**P2. Check-then-act across processes, and unlocked read-modify-write of shared files.** Several processes (daemon threads, CLI, TUI, desktop windows, the agent itself) read a file, decide, and write the whole file back with no lock and no compare. Seen in C7 (session ensure), C11, C12 (`forward-modes.json`), C19, C21, C24 (`linear.json`), C25 (project YAML, `config.yaml`), C28 (`zen-flow`), C33 (questions), C34 (`machines.json`), plus `hub shelbi.yaml` and `launch/recent.rs` in the appendix. Several of these carry a comment claiming cross-writer safety that only holds within one process. A shared `update_json_locked(path, f)` helper (as `board_index` and `pr_status` already do) closes most of them.

**P3. State duplicated across memory, disk, tracker and clients, with no owner.** The same fact lives in two to eight places that drift: card fields vs overlay vs index vs marker vs `status.yaml` vs `last_known` (C3, C6, C31); supervision latches on two threads (C4); status category vs literal id (C8); open flag vs catalog note (C11); Zen tri-state vs bool (C29); tab list in four copies (C36); remote host verdict in window vs link vs connector (C17); session state re-derived by four predicates locally and one remotely (C7). The recurring fix is to pick one owner, persist the state there, and derive every other copy.

**P4. Client async operations without generation tokens.** Answers land into UI state without checking whether the user navigated, re-toggled, switched project or started another request. Seen in C13, C14, C15, C16, C17, C37, C38, C40, and the desktop catalog (C11). `MachineForm`, `ClosedHistory`, `prereqs` and the add-project run counter already show the pattern done right. A shared `Ticket`/`SaveSlot` in shelbi-app, used by both clients, is the single highest-leverage client fix.

**P5. Failure classified on the permissive or destructive side inconsistently.** A read error is turned into a default value that drives a decision. Destructive: C3 (workflow load fails → inactive → kill), C4 (log read fails → not in flight → relaunch), C11 (state unreadable → closed → no poller). Permissive: C2 (workflow load fails → any-to-any edge allowed), C6 (health-check lookup fails → promote on liveness), C5 (`classify_handoff` → Developer), C37 (error-log read fails → "no errors" and marked read). A tri-state `Known(x) | Unknown` at each of these sites, with "hold" on Unknown, is the fix.

**P6. Process identity recorded as a bare pid or pgid.** Signals sent to whatever process now holds a recorded number: event follower (C19), review dev-server pgid (C6), job runner liveness (C21), `watch --pid`. `DaemonPidRecord` and `is_job_runner` already store and check start time; reuse them.

**P7. Dead or orphaned producers after the tmux-to-session migration.** State whose writer disappeared while readers kept trusting it: the Zen heartbeat and crash record (C10), session metadata for user shell and launch epoch (C3, C5), `workspace_migrated` predicate with inverted meaning, and stale module docs describing tmux behavior (`session_process_backend.rs:4-39`, `review_ui.rs:1-82`, `hub_version.rs:39-66`). Worth a deliberate sweep for other `tmux`-era fields with no writer.

**P8. Client logic duplicated across TUI and desktop instead of living in shelbi-app.** Contrary to the CLAUDE.md rule, both clients assemble their own machine from shared pieces, and the copies have drifted: optimistic moves (C14), review open flow (C15), Agents load guard (C16), session pane `Slot` (C40), job sheet cancel (C21), new-project wizard (C27), diff dead-session handling (C13). Each drift has produced a behavior difference already.

---

## 4. Low / leave-alone appendix

One line per remaining low finding not folded into an entry above.

**orch-core**
- L3 Heartbeat schedule: four `Option`s; `now - interval` can underflow an `Instant`; global `events.log` mtime counts other projects' writes as activity. Make explicit (Unseeded/Armed).
- L9 PR record (`pr_record.rs`): best-effort under a file lock, sweep repairs drift. Leave.
- L9 Verified submit (`submit.rs`, `agent_send.rs`): explicit `SubmitStatus`; residual gap between poller in-process nudges and CLI dispatch; `agent_send` uses the project default runner profile. Leave.
- L9 Poller ownership: workspace threads not joined; a replaced poller can overlap an old thread mid-git. Document.
- L9 Blocking-dialog dedupe, reminders/estimate alerts, awareness v1/v2 adoption per bridge process. Leave.
- L9 Hub pre-commit guard installer: fixed temp name lets two installers race. Leave.
- L9 Task-ref finalize: well locked; primary checkout left detached; workspace lock held through 20-minute checks. Document.
- Minor: `#[cfg(test)]` CI poll remnants; `LocalCheck` exit-code sentinels collide; probe log truncated per probe; `AwarenessDigest.cursor` re-seeds on read error; `spawned` thread map respawns with fresh latches; `ready.rs` docs disagree on timeout behavior.

**orch-workspace**
- L2 Tmux-cutover `MigrationState`: absence reads dispatchable; comment wrong; failed `let _` write. Document (module scheduled for removal).
- L4 Quit composition: daemon crash mid-quit leaves `open=false` with live sessions. Document.
- L11 Daemon job cancellation registry: cooperative, process-local. Leave.
- Minor: `settings.local.json` write non-atomic before kill; mounted-skills manifest crash window; `info/exclude` RMW across worktrees; awareness plugin bundle remove-then-rewrite; `GITHUB_LOGIN` OnceLock caches a failed lookup; `dispatch_active_gate` takes no lock; `SshExec::run_script_with_stdin` has no deadline; awareness mode silently downgrades to v1 on staging failure; `RespawnOutcome` always `Failed`; `ReviewMidView` unreferenced; `far.rs` stop flag always false.

**state-core**
- L13 Workspace cutover state: `workspace_migrated` dead predicate with opposite meaning; `set_migration_machine_unreachable` reads outside the lock. Fix docs, delete predicate.
- L17 `Issue.branch` two indistinguishable modes, hard-coded `shelbi/<id>` fallback ignoring the prefix.
- L17 Push provenance last-line-wins across projects (no `project=` on push lines); fails safe.
- L17 events.log rotation "ambiguous" states refuse every append hub-wide with no automatic exit; doctor prints "unknown". (Low frequency, high impact if hit: worth a doctor action.)
- L17 Default-agent provenance latch per agent not per divergence; Settings Reset doesn't update provenance.
- L17 Changelog attachments: concurrent attaches drop a record; crash leaves orphan assets.
- L17 `--root` not inherited by daemon spawn (suspected, unchecked).
- Minor: hub `shelbi.yaml` unlocked RMW where a parse error resets it; `Reachability::Unknown` conflates never-probed and in-flight; `Agent.status` lifecycle enum with no writer; `load_session`/`save_session` dead; token probe under the mutex can block resolvers ~18 s on a stalled keychain; tool detection refreshed only by the TUI; zen demotions written after the move lands; `IssueTrackerConfig` doc says Linear not implemented.

**cli-daemon**
- L3 System config-update protocol exists only as agent prose; stage dirs never deleted. Redesign later with CLI verbs.
- L5 Lost GUI login session recovery: mostly explicit; RecoveryStarting with `record=None` representable; single 500 ms launch window. Leave.
- L9 Commit guard: wrong path in `guard status` (bug #59); teardown doesn't remove the hook despite docs. Document or wire.
- Minor: `issue start` estimate written after the daemon start; `issue_locks` never removed; `HANDOFF` and `send.rs` stale docs; `session_attach` outcome as Option+bool; `orchestrate --status` ignored; `config inventory` copies `settings.local.json` with default permissions (worth fixing: may hold user keys); Linear vs GitHub request-outcome encodings differ; `status` labels a Linear board "github:"; `issue-store migrate` loses comments of a card whose comment copy failed.

**runtime**
- Minor: resize debouncer (healthy); session shutdown flags (healthy); `info.child_running` always true; askpass `run_until` kills only the ssh child, not its group; `CancelScope` changes `run` behavior for any callee; `MouseModes` ordering unenforced; `build-apt-repo.sh` non-atomic `InRelease`; cask comment claims a pinned formula dependency it doesn't have; palette header says Ctrl+Space; host `eprintln!` diagnostics go to `/dev/null`.

**app**
- #17 Markdown editor IME composition: `replace_from_outside` doesn't remap `composing.0`; several mutators don't refuse mid-composition. Make explicit (`Composition` enum).
- Minor: `CheckStop` hand-synced cancel; `WorkspaceForm` result routed to whichever confirmation is open; `WorkspacePanelState.active` not reset; `nav::ClientState.overlay` write-only and `GlobalFlags` unused; `ReviewRow` invalid combos; `BuilderState` pub fields and index-keyed transitions; `MachineForm.set` allowed while saving; palette Projects-column focus while hidden; undo coalescing relies on nine `seal()` calls; `ChangelogWatch::observe` marks seen before the read lands; `CommandKind::from_id` placeholders; refresh/subscriber reconnect sleep doesn't re-check stop; `exec_daemon` version gate covers mutations only; `wire.rs` decodes a missing `WorkspaceState` as `AwaitingInput` (fail-open); `launch/recent.rs` last-writer-wins; `ColumnExpansion` never returns to Auto; `WorkspaceFilter` sentinel on disk.

**tui**
- L8 Terminal ownership hand-off and stderr redirect: idempotent and tested. Leave.
- L9 Sidebar divider drag never ends on FocusLost; next click resizes. Small fix.
- L10 Machine form readiness `started` timer not cleared on paste. Leave.
- Minor: kanban modal stack precedence by handler order; selection by (column, row) index in kanban, sidebar, machines and workspace panel; `pending_editor`/`pending_editor_line` pairing; `reported_main` cache with ~15 invalidations; changelog pending reveal lingers; jobs sheet `loading` twice; palette `Cell`s mutated during render; `caps.kitty` unprobed = no; `ActivityApp.should_quit` and kanban `Outcome::Quit` vestigial; progress printer `paused` bool; Connect dialog Esc still records a recent host.

**desktop-views**
- 12 Machines view: one failed read blanks the page. Keep last good model.
- 20 CLI setup: `advancing` latch never resets; old runs' listeners not run-guarded. Leave, document.
- Minor: `BoardState.status` as a "loading more" flag; both settings editors `Some` representable; `WorkflowEditor.details` keyed by transition index; `DocEditor.set_from_outside` drops an update during IME composition; builder canvas `pressed_at` lost on deactivation; `ToolSelect` step clamp would panic on zero entries (always at least 3); `UpdateState` reset safe only because called once; `FigureState::Missing` fixed at parse; desktop `KeybindingsPage.error` overwritten by load/save/open alike; desktop writes shared `pub` fields directly (`AgentsPage.shared_input`, `WorkflowsState.draft`).

**desktop-shell**
- L1 Switch hold and pending sidebar rows: frame-advanced, waits on board loading. Document.
- L3 Project open/quit completions vs switches: stale completion after A→B→A; harmless while `orchestrate` is idempotent. Per-switch generation.
- L8 Hover/press fades keyed by string, not window; `FRAMES` never pruned. Visual only.
- L9 Update check cache: fixed tmp name can race old/new app during relaunch. Leave.
- Minor: shared `status`/`banner` slots with ~20 writers; modal slots as independent Options (`modal_open` ORs them); menu bar last-install-wins; `window_drag::ARMED` thread-local; terminal settings zoom overwritten by the async load; slider unmount sends `Changed` without `Committed`; `WindowLook` opacity duplicated and `APPLIED` forgotten; `remembered` view overlay never cleared; `desktop-windows.json` per-pid mutex lets two app processes race; `relaunch.rs` success means "spawned", not "came up".
