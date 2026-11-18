# Implicit state machines: orch-core

**Slice:** `crates/shelbi-orchestrator/src`, all 25 files assigned:
- `zen.rs`, `zen_flow.rs`, `poller.rs`
- `wake.rs` and `wake_{awareness,batch,rejection}_tests.rs`
- `actions.rs`, `transition.rs`, `transition_move.rs` (+ tests)
- `awareness.rs`, `awareness_plugin.rs`, `awareness_tests.rs`, `system_plugin.rs`
- `job_exec.rs`, `job_route.rs`, `supervision.rs` (a re-export of `shelbi_app::supervision`)
- `handoff.rs`, `ready.rs`, `submit.rs`, `agent_send.rs`, `pr_record.rs`, `danger_glob.rs`, `githook.rs`

**Coverage:** I walked all production code. Test modules were read as evidence. `danger_glob.rs`, `system_plugin.rs` and `awareness_plugin.rs` hold no lifecycle machine; they appear under Minor.

**Findings:** 26 in total. 5 high, 12 medium, 9 low.

**Bugs:**
- Confirmed by reading the code:
  - The handoff push retry cap does nothing when the tip is unreadable (M1).
  - The dev orphan reaper kills live panes when a configured workflow fails to load (H3).
- Plausible, not reproduced:
  - A hand-merged Zen PR is never reconciled to done (M10).
  - A running job can be reported as failed if its pid write fails (M7).

**Recurring shape:** a multi-step side-effect sequence (merge → move → event → cleanup) whose only journal is "a marker file is still there". A crash or error between two steps either repeats the early steps or silently loses the late ones. The protocol is also re-implemented by 5+ callers with different failure policies.

Line numbers are at `main` e8145b3f.

---

## HIGH

### H1. Workflow edge application (gate/merge → status write → event → Zen bookkeeping → cleanup), five copies

- **Entity:** one issue crossing a workflow edge, together with its PR, branch, `events.log` line and Zen flow record.
- **Encoding:**
  - (5) Sequencing.
  - (4) Tracker status, PR state and git refs mirrored locally.
  - (8) `MoveError` / `GatedMerge` / `MergeOutcome` exist, but every caller re-implements the order and the failure policy.
- **States:**
  - Real states:
    - not started
    - gate prefix ran (`push_branch`)
    - merged but not moved
    - moved, no event
    - moved with event
    - cleanup done
    - cleanup failed
  - Representable but invalid:
    - card in review while its PR is merged
    - card in done with its branch alive
    - status changed with no `events.log` line
    - "cleanup owed" recorded nowhere
- **Writers (each with its own order and failure semantics):**
  - **`transition_move.rs:300-511` `move_issue_with`.** Used by CLI `issue move` and the TUI board.
    - Order: branch cut (357-362), gated merge (370-397), `move_status` (398), event append.
    - If the event append fails, it **rolls the card back** to `from` (411-429), even though the merge already landed and `delete_branch` never ran.
    - Then `record_zen_user_move` (433), `zen_flow::forget` (448), detach (466), and the remaining actions (491, warn only).
  - **`review_ui.rs:399-418` (Approve, through `mutate.rs:474`).** Runs `run_gated_merge`, then `move_status`, then `let _ = append_task_event(..)`. There is no rollback and no Zen demotion record.
    - This contradicts `transition_move.rs:1-9`, which claims to be "the one library function behind every user-driven status move".
    - Post-merge cleanup runs on a detached thread (`mutate.rs:480-486`) and is lost if the daemon exits.
  - **`poller.rs:2992-3480` ready handoff** (H2) and **`poller.rs:3670ff` transition marker** (M2): each its own copy.
  - **`zen.rs:1529-1568` GitHub-merge reconcile** (M10). Order: `note_finished` 1534, `zen_flow::finish` 1535, `move_status` 1538, event 1542, `execute_transition_except([Merge])` 1551 (warn only, 1557).
  - Other `move_status` callers:
    - `zen.rs:7260`
    - `mutate/start.rs:180,411`
    - `shelbi-cli/src/commands/issue.rs:1156,1272`
    - `shelbi-cli/src/commands/workspace.rs:693`
    - `poller.rs:3742`
- **Where it lives:** `transition.rs:188-270` (`run_gated_merge`), `transition.rs:329-371` (`run_transition_actions`), `transition_move.rs:300-511`.
- **Risk: high.**
  - **The merge is irreversible, and nothing records that it happened.** If `move_status` fails (for example a rate-limited GitHub label swap), or an event append triggers a rollback, the task is left in its old status with a merged PR.
    - Recovery depends on luck:
      - the next caller's merge being idempotent (`HubMerge::AlreadyIntegrated`, `actions.rs:1357` for ancestry and 1418-1423 for an empty squash, so a retry does pass);
      - or the out-of-band reconcile (M10), which has its own blind spots.
  - **Post-move cleanup is fire-and-forget everywhere.**
    - A crash or error after the move means `delete_branch`, `run:` and pane cleanup never retry, because the next tick sees the task is no longer in the source status.
    - No test covers "gate ok, then `move_status` errors" (`transition_move_tests.rs` covers a failed gate at 426, failed cleanup at 447 and a failed branch cut at 523).
  - **A `run:` / `ready:` command can run again on retry.** It starts a server, `wait_ready` times out (`transition.rs:516-542`), the poller retries, and the server starts again. This is suspected to be the review-serve leak class.
- **Recommendation: redesign.** Write one `apply_transition(edge, cause)` that owns gate, write, event, Zen bookkeeping and cleanup, with one failure policy. Persist a small "transition in flight" record (`{edge, merged_sha?, phase}`) so a crash or partial failure resumes at the right step. Every caller listed above routes through it.

### H2. Ready-marker handoff (worker done → card advanced → branch freed → pane closed)

- **Entity:** one finished dev task in a workspace.
- **Encoding:** 3 (on-disk markers), 5 (step order), 4 (tracker, git and pane state), 6 (retried every tick).
- **States:**
  - No marker.
  - Marker present, and one of:
    - task load failed, so deferred (`shelbi-ready-deferred` sidecar holds an error class);
    - refused by the worktree check (refused sidecar, L2);
    - rebased with push pending or failed (push ledger, M1);
    - edge actions failed;
    - merge landed (merge-edge path);
    - card moved with cleanup pending.
  - Pane killed and marker cleared.
  - Stale marker (task not in progress, or gone).
  - Representable but invalid:
    - merge landed but card still `in_progress` with the marker present;
    - worktree detached (it happens *before* the merge, `poller.rs:3199`) but task not moved;
    - card moved but marker not cleared.
- **Transitions:**
  - The worker writes the marker, as the dispatch prompt instructs.
  - Dispatch and resume clear it: `workspace.rs:2686,2691,2952,2954`.
  - Poller:
    - read 3004-3026 (with a legacy `.claude/` path fallback);
    - deferred sidecar clear 3047, read/write 3449 and 3461;
    - refusal gate 3137;
    - rebase 3160;
    - push gate 3173;
    - detach 3199, then merge 3200, with an `ok` event at 3214 or `failed` at 3232;
    - edge actions 3268, failure event 3288;
    - `move_status` 3315, event 3325;
    - cleanup 3351 / 3384;
    - pane kill 3393;
    - marker clear 3118 and 3477.
  - Marker paths and IO: `workspace.rs:199-510`.
- **Risk: high.**
  - **Lost cleanup after a crash.** A crash between the move (3315) and cleanup (3351/3393/3477) takes the `not-in-progress` stale branch on the next tick (3418-3428). The marker is cleared, but `delete_branch`, `run:` and the pane kill never happen. The orphan reaper (H3) is the only backstop.
  - **No backoff or dedupe on a failing step.** A failing merge or edge action re-runs rebase, push and `gh` and appends a line on every poll. "a persistent conflict keeps emitting until a human intervenes" (3226-3230). `append_handoff_action_failed_event` has no dedupe.
  - **A failed event append after a tracker move** (3325, warn only) makes the issue reconcile see the hub's own move as external (M9).
  - The merge-then-move retry is safe: a squash re-merge returns `AlreadyIntegrated` (`actions.rs:1418-1423`). I checked this; it does not wedge.
- **Recommendation: make explicit.** Store a persisted per-task handoff phase (enum plus attempt/backoff) that each step advances, instead of four sidecars plus the marker. This is the poller-side caller of H1's single transition function.

### H3. Dev orphan-pane reaper (confirmed: a workflow load failure kills live workers)

- **Entity:** a dev workspace pane with no active task pointing at it.
- **Encoding:** 1 (`OrphanReap{since, failures, retry_at}`), 6 (grace, launch-young hold, backoff), 4 (pane liveness plus board).
- **States:**
  - not orphaned (`since=None`)
  - grace
  - launch-young hold (`poller.rs:6971-6975`)
  - eligible
  - backoff
  - reaped
- **Transitions:**
  - `poller.rs:6938`, `6948`, `7027`, `7034-7036`
  - reset when the pane is dead: 1831
  - created per thread: 595
- **Risk: high.**
  - **A YAML typo kills every live dev pane in that workflow.** `task_is_active` (7103-7116) fails closed for a configured workflow that won't load (`Err(_) => return false`). `workspace_orphaned_by_board` (7063-7075) then reads every task in that workflow as inactive, so every live dev pane in it is "orphaned" and killed after `ORPHAN_REAP_GRACE`.
    - Contrast the transition path (3719-3722) and the active-gate pass (5517), which fall back to the default workflow.
    - The same `task_is_active` feeds `redispatch_superseded` (5259) and `assigned_dev_task_for` (6085).
    - No test covers it.
  - **No overlay fold.** `workspace_orphaned_by_board` reads the index's `assigned_to` without folding in the assignment overlay. The review path does fold it (6015-6028). A past bug of the same class: the reaper killed a booting review slot ~2s after launch because the index lagged the overlay (6737-6741).
- **Recommendation: make explicit.** Return `Active | Inactive | Unknown` (as `AssignedDevTask` already does) and hold on `Unknown`, so the destructive action requires positive evidence.

### H4. Stranded-slot recovery: three crash-loop machines on two threads

- **Entity:** a workspace slot's pane (dev or review), plus the orchestrator pane.
- **Encoding:** 1, 3 (relaunch markers, `events.log`), 4 (pane liveness), 6 (backoff windows), 8.
- **The three machines:**
  - `SupervisionState{ever_alive, restarts, gave_up}` (`shelbi-app/src/supervision.rs:83-245`), held per workspace thread (`poller.rs:572`) and for the orchestrator (239).
  - `ReviewResumeState` (`poller.rs:5632-5726`) and `DevResumeState` (6108-6185), held on the supervisor thread (248, 255).
  - `decide_dead` is copied three times: 5707-5725, 6157-6175, and `supervision.rs` `decide`.
- **States:**
  - alive or booting
  - dead, unconfirmed (`dead_streak=1`)
  - dead, confirmed
  - backoff
  - resuming
  - GaveUp
  - StoodDown
  - dispatch-in-flight (read from `events.log`)
  - Unknown (board not warm)
  - Plus an owner dimension: pane supervisor or resume pass.
  - Representable but invalid: workspace-thread `ever_alive=true` while supervisor-thread `ever_alive=false`, so both passes think they own the same dead slot.
- **Transitions:**
  - `note_alive`: 5776, 5897, 6251, 6269, 6334
  - `note_dispatch_in_flight`: 5803
  - `observe_dead`: 5866, 6369
  - `decide_dead`: 5870, 6381
  - `forget_last_resume`: 5896
  - `request_relaunch`: 5116, 5341, 5843, 6363
  - direct field poke `entry.dead_streak = CONSECUTIVE_DEAD_BEFORE_RESUME - 1`: 5844
  - `stand_down_logged`: 5855, 6348
  - `state.remove`: 5813, 5829, 6298, 6320
  - `SupervisionState::decide`: 5131, 5348
  - relaunch markers (written by the TUI and desktop "reopen") consumed at 5102, 5335, 5837, 6357
- **Risk: high.**
  - **Only a log grep prevents a double launch.** The two `ever_alive` latches live on different threads (documented at 6104-6106 and 6273-6278). The guard is `recent_dispatch_active`, which greps the last 64 KB of the hub-global `events.log` (`shelbi-state/src/event_log.rs:2836-2931`) and returns `false` on I/O error (2890), the destructive side. That window can be flushed by `active-gate-failed` spam (L4).
  - **Past bugs cited in the code:**
    - `rt-review-slot-relaunch-loop`: a resume was laundered out of the crash history (5797-5799), and one wedged read relaunched a live slot (5602).
    - A spurious `--continue` ~2s after a launch (6261-6264).
  - **A cancelled orchestrator launch** returns after `decide` already recorded a restart (5355-5357). It counts toward the cap with no event.
  - **In memory only.** `ever_alive=false` after a poller restart means a pane that is already dead is **never adopted** (`supervision.rs:162-165`).
- **Recommendation: redesign.** One `SlotRecovery` enum with one transition function, owned by one thread per slot. Replace the `events.log` tail grep with a durable "launch in flight" lease (deadline plus pid).

### H5. Review-slot serving lifecycle (marker + `status.yaml` + `last_known`)

- **Entity:** a `review`-tagged workspace slot.
- **Encoding:** 3, 4, 1, 8 (`WorkspaceState::Serving` assigned from several paths).
- **States:**
  - assigned and booting (health probe failing)
  - serving
  - serving but unconfirmed this tick (folded)
  - orphaned / reaped
  - Unknown (board not warm)
  - Representable but invalid:
    - the marker names the previous task (4961-4964);
    - `status.yaml` says Serving A while the marker names B;
    - `last_known` says Serving after `status.yaml` was cleared (6604-6606).
- **Writers:**
  - Marker write: poller at 6573, and the Review agent itself per the shipped instructions (`workspace.rs:258-260`; sniffer in `config_upgrade.rs:516-546`).
  - Marker clear:
    - poller at 6757
    - `workspace.rs:2708`
    - `review_ui.rs:517`
    - `review_session.rs:551` (local hosts only)
  - `status.yaml`: save at 6629, clear at 6790 / 7009 / `review_ui.rs:531`.
  - `last_known`: 6644.
- **Readers:** the poller at 4976 and the TUI at `app.rs:1438,1537`. On a cold board, `slot_current_review_task` falls back to `status.yaml`'s own Serving task (4992-4996). The derived state is used to prove itself.
- **Risk: high.** Three copies must agree before `review-ready` fires (6614-6632), and they are written by three processes (poller, client, agent).
  - **Past bugs in comments:**
    - Serving flapped to awaiting_input and back, re-emitting review-ready (4940-4945).
    - A booting slot was reaped (6737-6741).
    - Requiring the marker before reaping stranded orphaned panes (6721-6727).
  - **Memory and disk drift.** A failed `save_workspace_status` only warns (6629-6631) while `last_known` still advances (6644).
- **Recommendation: make explicit.** Keep one persisted `ReviewSlotState{task, phase}` with one transition function, and derive the marker from it (or retire the marker).

---

## MEDIUM

### M1. Handoff push retry ledger + pushed-tip provenance (confirmed bug)

- **Entity:** the handoff push for one workspace branch.
- **Encoding:** 3 (`<tip> <attempts> <retry|blocked>` text sidecar), 6, 4.
- **States:**
  - no ledger
  - `Retry(tip, n<5)`
  - `Blocked(tip)` (cap reached or foreign commit)
  - quiet re-attempt
  - Pushed-tip provenance is a separate per-branch sidecar, with `events.log` as a fallback (4583-4592).
- **Writers:**
  - clear: 4616
  - pushed-tip upsert: 4624
  - retry: 4665
  - blocked: 4683
  - Dispatch never clears either file.
- **Risk: medium. Confirmed bug.**
  - **How it happens.** An unreadable local tip becomes `""` (4564-4565). The code then writes `" 1 retry"` (`encode` 4307-4313). `read_deferred_marker` trims that to `"1 retry"` (`workspace.rs:464`). `parse` (4299-4304) reads `tip="1"`, fails to parse `"retry"` as the attempt count, and returns `None`. `prior` is therefore always `None`, attempts never exceed 1, and the cap never trips.
  - **The comment says the opposite.** The comment at 4562-4563 says "the cap still bounds the loop". This is the same shape as the incident the cap was written for: 482 retries over ~75 min (4280-4281).
  - **Tests miss it.** They round-trip only non-empty tips (14839-14854).
  - **Silent write failures.** All ledger writes are `let _ =`. If the blocked-ledger write fails, the question and the worker directive are re-raised every tick (4698-4703).
- **Recommendation: make explicit.** Use a structured (JSON) ledger keyed by branch and add an empty-tip test. This folds into H2's phase record.

### M2. Agent transition marker (one-shot gate verdict)

- **Entity:** a gate agent's verdict file asking the poller to move its task.
- **Encoding:** 3, 5, 4.
- **States:**
  - none
  - pending (marker present)
  - `Deferred(<stage>:<class>)` sidecar
  - applied
  - refused
  - dropped (not-owned, task-gone, already-at-target)
  - legacy `.claude/` path variant (3688-3693)
- **Writers:**
  - The agent writes the marker.
  - The poller clears the marker at 3873, writes the sidecar at 3927 and clears it at 3878.
  - Dispatch (`workspace.rs:2694-2705`) and resume (`workspace.rs:2957-2967`) clear both.
- **Risk: medium.**
  - **Not atomic.** The sequence is: move (3742), task event (3749), edge actions (3767), pane kill (3805), marker clear (3873). A crash after the move means the next tick sees the task unassigned and drops the marker as "not-owned" (3848-3851). Edge actions such as `close_pr` (3761-3763) and the event are lost silently.
  - **Wrong fallback when the workflow fails to load.** Validation falls back to the any-to-any default (3719-3722), so a forbidden edge can be applied with the default workflow's actions. This is the opposite polarity from H3: here a load failure is permissive, there it is destructive.
  - **A resume can erase a deferred verdict.** A dev-resume of a gate slot (6383) clears the marker. If store reads are still failing, the verdict is gone, which "parks the card in the gate for good" (3857-3859).
- **Recommendation: make explicit.** Route it through H1's transition function with a journaled phase, and defer (don't default) on a workflow load failure.

### M3. Codex durable event-batch delivery (`codex-event-queue.json`)

- **Entity:** each `QueuedBatch` of orchestrator events awaiting delivery to a Codex thread.
- **Encoding:**
  - (8) The `DeliveryStatus` enum (`wake.rs:2704-2723`) is assigned directly in many places.
  - (1) `actionable`, `keep_alive`, `digest`, `attempted` (2737-2755).
  - (3) The queue file.
  - (4) It depends on the applied `event-cursor` and the log head.
- **States:**
  - `Pending`, `InFlight{thread}`, `Delivered{thread}`, `Injected{thread}`
  - implicit "applied", popped when `through <= cursor` (2917-2923)
  - implicit "discarded" (2924-2933, 2952-2958, 2871-2895)
  - Representable but invalid:
    - `Injected` on a non-digest batch;
    - `keep_alive && digest` (rewritten by `adopt_mode`, 2971-2981);
    - `!attempted` with `Delivered`.
- **Transitions:**
  - create: 2764-2789
  - coalesce: 2814-2839, 2983-2994
  - Pending→InFlight: 1073-1076, reverted at 1083
  - InFlight→Delivered: 1118-1120
  - →Pending on rejection: 1139
  - Pending→Injected: 1218-1222
  - Delivered→Pending when idle and the cursor hasn't moved: 2935-2950, 3070-3077
  - reconcile by client id: 3057-3069
  - other thread → Pending: 3078-3088
- **Risk: medium.**
  - **No failed state, attempt cap or backoff on replay.** Delivered→Pending fires whenever the thread is idle and the cursor hasn't moved ("RPC acceptance is not application", 2945-2948). If the orchestrator never drains, or the drain errors, the bridge `turn/start`s the same batch every time Codex goes idle. That is an unbounded token-burning loop. Replay is intended (test at 3944), but unbounded replay is not addressed.
  - **"Pending" means two things.** Health counts all non-delivered batches (2361-2370), while the runner-switch guard counts only actionable ones (3040-3046).
  - **Crash recovery is sound.** It uses InFlight plus a stable `clientUserMessageId`, and tests cover it (3881-3962).
- **Recommendation: make explicit.** One `transition(batch, event)` function, plus a persisted replay count and backoff (or a `Stalled` state that `status` surfaces).

### M4. Codex NativeBridge delivery gate

- **Entity:** the in-process `NativeBridge` (`wake.rs:567-593`).
- **Encoding:**
  - (1) `rpc: Option`, `tui_ready`, `bootstrap_sent`, `protocol_unsupported`, `blocked_generation`, `bootstrap_blocked_generation`, `awaiting_nonsteerable_completion`.
  - (6) Five deadline fields.
  - All of it over `ThreadPhase{Idle,Active,Unknown}` plus a `generation` counter (2475-2613).
- **States:**
  - starting
  - connected, split into bootstrap-pending, idle-deliverable and active-steerable
  - blocked-on-generation
  - awaiting a non-steerable (review/compact) turn
  - overloaded backoff
  - disconnected/rehydrating
  - protocol-unsupported (terminal)
  - Invalid but representable: awaiting set while `rpc` is None.
- **Writers:**
  - `tui_ready`: 728
  - `protocol_unsupported`: 730, 1030, 1149
  - `bootstrap_sent`: 985, 1010
  - awaiting set: 1022-1027, 1141-1146
  - awaiting cleared: 863-864, 879-880, 897-898, 908-909
  - `blocked_generation`: 956, 1121, 1146, 1161
  - `rpc`: 954 (Some), 1252 and 1264 (None)
  - timers: 959-960, 1035, 1155, 1239
- **Risk: medium.**
  - **No deadline on the non-steerable wait.** `awaiting_nonsteerable_completion` is cleared only by a matching notification or a reconnect snapshot. If the `turn/completed` is missed while the connection stays up, every delivery is blocked indefinitely and nothing is surfaced.
  - **The gate is written three times with differences:** 971-977 (bootstrap), 1052-1058 (event), 1196-1204 (digest). Events may deliver before bootstrap when Active; the other two require `bootstrap_sent`.
  - **Restart is safe.** Everything is rebuilt from `thread/resume`.
- **Recommendation: make explicit.** A `Link` enum (`Disconnected{retry_at}` / `Connected{gate}`), one `can_deliver(kind)` function, and a deadline that forces a rehydrate.

### M5. Codex bridge engagement (`codex-thread.json`)

- **Entity:** a project's orchestrator Codex integration mode.
- **Encoding:** 3 (file present / absent / renamed `.archived-<secs>`), 1 (`native_active: bool` + `native_inactive_reason: Option`, 2029-2047), 5 (in-memory supervisor loop 367-501).
- **States:**
  - absent
  - native-active
  - retrying startup (in memory only)
  - degraded-standalone (with or without a reason)
  - archived
  - Invalid but representable: `native_active=true` with `Some(reason)`.
- **Writers:**
  - active: 2145-2151, 2226-2235
  - inactive: 2397-2401 (from 516)
  - archive by rename: 2421-2433 (from `handoff.rs:351`)
  - Readers: `handoff.rs:275-280` (runner-switch policy), `shelbi-cli status.rs:540`.
- **Risk: medium.**
  - **Startup can retry forever and still look healthy.** Transient errors retry with no cap (274-292, 420-427), and the only signal is a `tracing::warn`. Meanwhile the file still says `native_active: true` from the last run, so health (2333-2339) reports Structured while no TUI exists and the pane looks hung.
  - **Two archives in one second overwrite each other** (2427-2432).
- **Recommendation: make explicit.** Persist an `Engagement` enum (`Starting | Native | Degraded{reason}`) and write `Starting` so health can see a stuck startup.

### M6. Observed workspace state + per-thread detector latches

- **Entity:** one workspace pane as its poll thread sees it.
- **Encoding:** 1, 3 (`status.yaml`), 4 (pane liveness and screen content), 8 (`WorkspaceState` assigned from many paths).
- **States:**
  - `WorkspaceState` (Working / AwaitingInput / Blocked / Paused / Serving …), held in memory as `last_known` (`poller.rs:526`) and on disk in `status.yaml`.
  - Six side latches: `last_dialog`, `unknown_dialog_streak`, `idle_debounce_streak`, `last_agent_exited`, `limit_resume`, `orphan_reap` (545-595).
  - Invalid but representable: `last_agent_exited=true` with a live `limit_resume` schedule. The agent-exited branch resets the other latches but not this one (1917-1920).
- **Writers:**
  - `last_known`: 1942, 2180, 2413, 2443, 6644
  - `status.yaml` save: 1929, 2157, 2396, 2430, 6629
  - `status.yaml` clear: 6790, 7009, `review_ui.rs:531`
  - Hand-copied latch-reset blocks:
    - pane dead: 1826-1831
    - orphan reaped: 1847-1851
    - agent exited: 1917-1920
    - re-arm: 1950
    - non-Claude runner: 1963
- **Risk: medium.**
  - **Mixed sources.** `load_prior` (2345-2369) takes the prior *state* from memory and the prior *timestamp* from disk. A client clearing `status.yaml` doesn't reset the in-memory state.
  - **A blip re-arms the agent-exited latch.** `*last_agent_exited = false` (1950) runs unconditionally, so a capture blip re-arms it and emits a duplicate `supervision=agent-exited`. The dialog latch deliberately does the opposite on a missing sample (2297-2300).
  - **Past bugs:**
    - A slot frozen at `working` for hours on `dialog:question` (2096-2099).
    - A false idle edge that could double-dispatch (4870-4876).
- **Recommendation: make explicit.** A `PaneObservation` struct with `reset_on_pane_death()` / `reset_on_lifecycle_change()`, and one decided policy for a missing sample.

### M7. Job lifecycle (`__job-exec` runner, store, result, history, routing)

- **Entity:** a job.
  - On disk: an `awareness.json` `jobs[]` entry plus `jobs/<id>/{result.json, output.log, env.json}` and `history.json`.
  - Elsewhere: the job session.
- **Encoding:**
  - (3) Presence in the store, `runner_pid: Option`, `result.json` present or absent.
  - (4) pid and session liveness.
  - (6) `next_alert_at`, `RUNNER_START_GRACE`.
- **States:**
  - registered with no pid
  - running
  - overdue (repeating)
  - finished-unreported
  - lost
  - reported (in history)
  - canceled
  - Invalid but representable: `runner_pid=None` with `result.json` present.
- **Writers:**
  - register: `awareness.rs:851`, rollback 894-898
  - pid: `job_exec.rs:107`
  - result: `job_exec.rs:175`
  - report and remove: `awareness.rs:305-328`
  - overdue advance: 353
  - cancel: 936-973
  - lost detection: 404-422
  - history: 238, 948
  - session cleanup: 501-525
  - The CLI also writes (`commands/job.rs`).
- **Risk: medium.**
  - **(a) A running job can be reported as failed** (plausible). If `set_job_runner_pid` fails it only warns (`job_exec.rs:107-109`). After 120s the tick calls the still-running job lost (`awareness.rs:404-422`), reports Failed and removes it, and the real `result.json` is never reported.
  - **(b) Worker-routed reports can be lost.** They go to a detached thread *after* the store commit (231-235, 566-579). A hub exit in that window loses the report, contradicting "a report is never lost" (`job_route.rs:15-18`).
  - **(c) Delivery is at-least-once.** Events are appended inside the lock but before `atomic_write`, so a failed write re-fires them. The module doc claims exactly-once.
  - **(d) Duplicates are possible.** A StillInBox send also writes the feed line, and the prompt can still be consumed later.
  - **(e) pid recycling.** A pid-watch job never finishes if its pid is recycled.
  - **(f) `env.json` leaks.** It holds the caller's full env and is left behind if the runner dies early.
- **Recommendation: make explicit.** An explicit `JobState` in the store (`Starting{deadline}` replaces the implicit grace), plus a persisted outbox for worker-routed notices cleared on a confirmed send.

### M8. Usage-limit auto-resume (`LimitResumeState`)

- **Entity:** a workspace's usage-limit incident.
- **Encoding:** 8, 6, plus a disk shadow (`WorkspaceState::Paused`).
- **States:**
  - `Idle`
  - `Scheduled{due, attempts}`
  - `NeedsHuman`
  - `Resumed{banner}` (2548-2567)
  - `Paused`, persisted separately.
- **Writers:**
  - Pure core: `advance_limit_resume` (2648-2729) and `advance_limit_resume_without_banner` (2605-2627).
  - About 15 direct `*state =` assignments:
    - 1830, 1851, 1963
    - 2779, 2790, 2816, 2830, 2839, 2845, 2861, 2871, 2881, 2891
    - 2942
  - `Paused` written at 2396, cleared at 2430.
- **Risk: medium.**
  - **A restart forgets `NeedsHuman`.** `NeedsHuman` (`resume-attempts-exhausted` 2708, `submit-unconfirmed` 2881) is in memory only. After a restart, a still-visible banner restarts the cycle at `attempts: 0` (pinned by `limit_resume_restart_rebuilds_due_from_persisted_stall_time`). So up to 3 more nudges can follow a "do not retry a possibly parked prompt" decision.
  - The pure core is well built and tested.
- **Recommendation: make explicit.** Persist the `NeedsHuman`/attempts latch next to `Paused`, and route the direct assignments through named transitions.

### M9. External-issue reconcile baseline (GitHub/Linear vs `events.log`)

- **Entity:** the sync baseline between the remote tracker and the hub.
- **Encoding:** 4, 1 (`seeded` + `cursor` + `seen_status`), 6 (backoff).
- **States:**
  - off
  - unseeded
  - seeded (cursor plus per-issue last-seen column)
  - backoff
- **Writers:**
  - reset: `poller.rs:1160`, 1167
  - backoff: 1187-1188, 1205-1206
  - seed: 1199-1201
  - cursor: 1241
  - `seen` mutated in `plan_issue_reconcile`: 1003
- **Risk: medium.**
  - **Lost changes.** An `append_task_event` failure still advances the watermark ("the watermark still advanced", 1051), so the change is lost.
  - **The "was this our move?" check is fragile.** It relies on `latest_task_transition_targets` from `events.log` (1230). A read error falls back to always-reconcile (1226-1228), which double-counts hub moves. H2's dropped event append has the same effect.
  - **A backend switch doesn't reset.** Switching GitHub to Linear at runtime does not reset the schedule (only FileSystem resets, 1159), so GitHub-keyed `seen_status` carries over.
  - **Downtime is skipped.** Edits made while the hub is down are skipped (accepted at 283-288).
  - `seen_status` never drops closed issues.
- **Recommendation: document it**, reset on any backend change, and consider persisting the cursor.

### M10. GitHub out-of-band merge reconcile (review → done)

- **Entity:** a review-category task whose PR may have merged on GitHub.
- **Encoding:** 4, 2 (`updated_at` used as handoff time), 5.
- **States:**
  - in review, no PR or open PR
  - in review, merged PR judged current (reconciled)
  - in review, merged PR judged stale (left alone forever)
  - done, cleanup ran
  - done, cleanup failed (representable, only logged)
- **Writers:**
  - `zen.rs:1534` `pr_record::note_finished`
  - `1535` `zen_flow::finish`
  - `1538` `move_status`
  - `1542` event
  - `1551` cleanup
  - Driven from `poller.rs:829`.
- **Risk: medium.**
  - **Guard #1 likely misreads a hand-merged Zen PR (plausible).** Guard #1 (1389) requires the branch tip to be contained in the merged PR head.
    - Zen publishes an integration commit built by `commit-tree <tree> -p base_sha` (8268) as the PR head.
    - The local task ref is CAS-advanced to the worker's `head_sha` (9058-9068), which is not an ancestor of that commit.
    - `resolve_branch_tip` prefers the local `refs/heads/<b>` (`git.rs:1056-1073`), which survives GitHub deleting the remote branch.
    - So a hand-merged Zen PR (which `zen_flow.rs:226-230` recommends as the override) plausibly never reconciles. `zen_flow::published_for_head` already models this divergence for `push_branch` (`actions.rs:317`), but the reconcile doesn't consult it. Not reproduced.
  - **Guard #2 uses the wrong timestamp.** It compares `merged_at < task.updated_at` (1410), but `updated_at` is bumped by any later field write. That includes the `pr_record` write itself (`shelbi-state issue_store.rs:983`), renumbering and priority changes. Any edit after a legitimate merge makes it look stale, permanently.
  - **Ordering.** `zen_flow::finish` precedes `move_status`, so a failed move drops the flow record while the task stays in review. A failed cleanup is never retried (1557).
- **Recommendation: make explicit.** Persist a real `handoff_at` (or use the PR / zen_flow record), teach guard #1 about integration commits, and record cleanup-pending for retry. Part of H1.

### M11. Zen pinned-PR flow record and landing gate

- **Entity:** one task's probe → publish → CI → land flow, stored at `<project>/zen-flow/<task>.json`.
- **Encoding:** 8 (`ZenFlowPhase` exists, but writes are direct `save`s from several CLI processes), 3, 5.
- **States:**
  - none
  - `Probed`
  - `Published{pr}`
  - `CiGreen{pr}`
  - deleted
  - Invalid but representable:
    - `integration_sha: None` with Published/CiGreen;
    - `Probed→CiGreen` directly (85-105 doesn't check the prior phase);
    - a resurrected record after `finish`.
- **Writers:**
  - Probed: `zen.rs:7865` → `zen_flow.rs:32`
  - Published: `zen.rs:373` → 68-80
  - CI: `zen.rs:988` → 85-105
  - delete: `zen.rs:1535`, `actions.rs:693`, `transition_move.rs:448`
  - Readers: `actions.rs:317`, 639, 675.
- **Risk: medium.**
  - **Lost updates.** Each step is a separate process doing load → check → save with **no lock** (`shelbi-state zen_flow.rs` takes none; `pr_status` does). A concurrent probe or `finish` between ci-watch's `find_by_integration` and `save_phase` is overwritten or resurrected.
  - **`zen pr-merge` bypasses the record.** `land_pr` (`zen.rs:2073-2081`) never calls `landing_pin` or `finish`.
  - **Failed advances only warn** (`zen_flow.rs:59-63`), so a later refusal is unexplained.
- **Recommendation: make explicit.** One locked transition function in `shelbi_state::zen_flow` that validates the prior phase, with `pr_merge` routed through it.

### M12. Branch delete / restack deferrals with no owner

- **Entity:** a task's branch (origin and hub-local) and its stacked children.
- **Encoding:** 4, 2 (reason strings), 8 (the deferred variants of `DeleteOutcome` and `RestackOutcome` are terminal).
- **States:**
  - `Skipped{restack-deferred:needed-by=<child>}` (`actions.rs:1779-1783`)
  - `RemoteDeletedLocalDeferred{workspace}` (1818-1822)
  - `restack-deferred:waiting-on=` (805-810)
  - `held-by-<ws>` (940-944)
  - `rebase-conflict` (1056-1068)
  - `restack-error:` (816-819)
- **Writers:** `delete_branch` (1760-1835), `restack_children` (773-822), `restack` (922-1144).
- **Risk: medium.**
  - **Nothing consumes the deferrals.** A grep of `crates/` finds `restack-deferred` and `RemoteDeletedLocalDeferred` only in `actions.rs` (I checked). The doc promises "the local ref is cleaned when the review slot frees" (66-68, 1754-1758), but the only production `git branch -D` is 1826.
  - **Leaks and silent success.** A parent skipped as `needed-by` leaks on origin, and a child that couldn't restack stays stacked on a merged parent with no record. Skips return `Ok`, so the edge "succeeds".
- **Recommendation: make explicit.** Persist a pending-cleanup ledger that a poller pass retries, or surface it as card state. At minimum, fix the docs.

---

## LOW

### L1. Pane activity classifier (`ready.rs`)

- **Entity:** an agent session as seen from screen captures.
- **Encoding:** 4, 6 (deadline extension `RESUME_COMPACTION_GRACE`, 37/137), 1 (startup latches in `wait_for_ready_with` 112-145).
- **States:** booting, trust dialog, resume-summary dialog, input-ready, busy, background tasks, blocking dialog (kinds), unknown selection, usage-limit stall, agent exited.
- **Risk: low-medium.**
  - **The vetoes are inconsistent.**
    - `detect_blocking_dialog` (476) and usage-limit detection veto on the narrow `is_claude_busy`.
    - `detect_agent_exited` (523) and `is_unknown_selection_dialog` (566) veto on the wider `is_claude_working`.
    - The doc at 177-182 says to use the wider one.
  - **Every detector's doc is a history of false positives:** 451-457, 600-606, 172-198, 78-88.
- **Recommendation: document**, then make it explicit: one `classify(screen) -> PaneState` with a written precedence order.

### L2. Ready-refused sidecar

- Holds a `<task>:<reason>` token: written at `poller.rs:4056`, cleared at 3996 and 4030.
- Dispatch never clears it (`workspace.rs:2680-2712`). A task that is hand-moved and later returns to the same workspace with the same refusal is refused silently: no `events.log` line and no worker directive, only a tracing warn at 4061.
- **Recommendation:** clear it at dispatch (or fold it into H2's record).

### L3. Heartbeat schedule

- **Entity:** the project's periodic heartbeat.
- **Encoding:** 1 (four `Option`s), 6, 3 (the global `events.log` mtime serves as the "something happened" clock).
- **Writers:** 1304, 1316-1319, 1330, 1352-1357, 1376-1377, 1389-1400, 737-742, 1418, 1573-1590.
- **Risk: low-medium.**
  - The past "zero heartbeats for 30+ hours" starvation is cited at 1271 and 1343. The clamp at 1325-1331 self-heals it.
  - Invalid but representable: `next_attempt` set without `window_start`. The fallback `now - interval` (1349, 1374) can underflow an `Instant`.
  - The global mtime counts other projects' writes as activity.
- **Recommendation: make explicit:** `enum { Unseeded, Armed{..} }`, plus a per-project cursor.

### L4. Active-gate dispatch pass

- Stateless and re-derived each tick. A probe error reads as "served" (5577-5595).
- A failed dispatch appends `active-gate-failed` on **every** supervisor tick with no dedupe (5547-5553). With no free workspace this floods the 64 KB `events.log` window that H4's `recent_dispatch_active` reads.
- **Recommendation:** dedupe with a last-reason latch, as `defer_transition` does.

### L5. Orchestrator handoff file (`agents/orchestrator/handoff.md`)

- **States:**
  - absent
  - requested (stale file swept, `handoff.rs:179`)
  - written (`exists()` at 199)
  - claimed (renamed `.claimed`, `shelbi-state/src/agent_workspaces.rs:512-540`)
  - settled or restored (493-499)
- `HandoffOutcome` (50-85) is explicit.
- **Risk:** `Written` is returned the instant the file appears, and the caller then tears the pane down. A write still in progress may be truncated (suspected; it depends on the agent writing atomically).
- **Recommendation:** document it, and optionally require a size that is stable across two polls.

### L6. Probe worktrees, probe-base refs, shared cargo target

- **Probe worktrees:**
  - Named `.shelbi-probe-<pid>-<task>-<seq>`, with an in-process registry (`zen.rs:8649`) and a sweep keyed on owner-pid liveness (8751-8800).
  - A reused pid, or EPERM counted as alive (8731), keeps a leak forever.
  - A signal between `worktree add` and register leaks.
- **Probe-base refs:** `refs/shelbi/probe-base/*` leak on an early return (9348 before the delete at 9352), and no sweep covers them.
- **Shared cargo target** (`CapOutcome`, 8811):
  - It is the hub's primary `target` (7765).
  - Non-Zen builds don't take the lock, so `rm -rf` (8877) can delete a target in use.
  - A failure to take the lock runs unlocked (`.ok()`, 7818-7820).
- **Recommendation:** document, extend the sweep to probe-base refs, and consider a dedicated Zen target directory.

### L7. Publish/land triad (local ref, remote task branch, PR head)

- **Writers:**
  - local CAS: 9058-9068
  - push of the integration commit: 514-518
  - leased base push: ~2207-2213
  - head delete: 2240, 2297
  - squash merge: 2276
  - head-verify retry: 681-709, 6 × 2s, then exit 75 (the retry lives in the orchestrator prompt)
- **Past dead ends cited in the code:** 649-651, 805-815, 7768-7777.
- **Risk:**
  - The push precedes verification. On `TransientVerification`, `record_opened` / `note_published` (357-373) are skipped, so the remote moved with no record. It recovers only because pr-create is idempotent.
  - `land_pr` is idempotent only on the leased path. A retry after `merge_via_pull_request` succeeded fails with "reviewed base moved" or "not open" instead of reporting the merge.
- **Recommendation:** document it, and recognise an already-merged PR in `land_pr`.

### L8. Status resolution in Zen scans (stringly columns vs workflow categories)

- `mechanically_eligible_from` compares the literal `Column::in_progress()` / `backlog()` (`zen.rs:15205,15211`).
- The CI poll (1216-1219), dry run (15724-15727) and reconcile (1482-1483) resolve categories via `resolve_task_status` (15783). That function is a copy of the TUI `kanban.rs` resolver, and its fallbacks differ: the column category vs `default_workflow`.
- **Risk:** a custom workflow that renames backlog or in-progress makes nothing eligible, and the overlap guard sees no in-flight work.
- **Recommendation:** one shared resolver in shelbi-core or shelbi-app.

### L9. Small, well-bounded machines (each: recommendation **leave as is** or **document**)

- **`ScopedCiState` CI dedupe** (`zen.rs:1041-1196`)
  - In memory; it re-emits after a restart by design (1035-1039).
  - It has two settle clocks (1004 vs `first_seen`) and two verdict sinks: `ci_watch` updates `zen_flow` but not `pr_status`, and the scoped poll does the reverse. The task sheet and the landing gate can therefore disagree.
  - An `append_ci_event` error (`?`) aborts the rest of the sweep.
- **PR record** (`pr_record.rs:46-130`; callers `actions.rs:464,499,643,656,1736`, `zen.rs:371,1116,1534,2079`, `poller.rs:854`): best-effort under a file lock, and the sweep repairs drift.
- **Verified submit** (`submit.rs:216-407`; `agent_send.rs:31-82`): `SubmitStatus` is explicit with one classifier.
  - The injection mutex is process-global (`session_backend.rs:625-640`), but `shelbi send` and dispatch also take the cross-process `lock_workspace` file lock (`shelbi-cli commands/send.rs`, `workspace.rs:2665,2940`), so they do serialize.
  - The residual gap: poller nudges that take only the in-process mutex against a CLI-process dispatch.
  - `agent_send` judges busy-ness with the *project default* runner's profile (133-138), not the workspace's.
- **Poller ownership** (`poller.rs:144-214`; manager in `shelbi-cli commands/daemon/poller.rs:68-115`):
  - Only the supervisor thread holds the flock, and workspace threads are not joined (503-510).
  - A "replace finished poller" can overlap an old workspace thread that is still unwinding mid-git.
- **Orchestrator supervision status file** (written 5404/5423, cleared 5329/5372): an orchestrator that is already dead at poller start is never adopted (H4), so "gave up" can linger.
- **Reminders and task-estimate alerts** (`awareness.rs:250-386`): sound locked read-modify-write. At-least-once, like M7(c).
- **Awareness mode v1/v2 adoption in the bridge** (`wake.rs:372`, 2971-2981): read once per bridge process, so a live switch needs a restart.
- **Blocking-dialog dedupe** (`decide_dialog`, `poller.rs:2214-2336`): pure decision with a per-thread latch, and it re-emits once after a restart (538-543).
- **Hub pre-commit guard** (`githook.rs:55-269`): the marker line in the hook body is the state, and `InstallMode` / `HookInstall` / `HookUninstall` are explicit.
  - The fixed temp name `.pre-commit.shelbi.tmp` (236) lets two installers race.
  - Leave as is.
- **Task-ref finalize / primary-checkout detach** (`zen.rs:9018-9290`): well locked (workspace lock, then git-worktrees lock).
  - The human's primary checkout is left detached and never restored.
  - The workspace lock is held through 20-minute checks (10710).

---

## Minor

- `WatchMode`, `MergeReadiness`, `PollOutcome`, `classify_poll` (`zen.rs:84-301`) are `#[cfg(test)]`-only remnants of an old CI poll machine.
- `PrMergeOutcome` has one variant. The queued, auto-merge and cancelled flow is refused outright (2188-2192). The test stub still models an enqueue/cancel flow (4120-4150). Not a machine.
- `ProbeReport` (7617-7680): correlated flags (`integration_sha` None iff conflicts). It is a report, not a lifecycle.
- `LocalCheck.exit_code` sentinels (124 for a timeout, -1 for a launch failure, 10715) collide with real exit codes.
- The `zen-<task>.log` probe log is truncated each probe (10866), so concurrent probes clobber each other's log.
- Dry-run tick (`zen.rs:15701-15900`) is pure. Its dedupe set lives in the CLI.
- `OwnershipGate` is a derived value. `danger_glob.rs` is a pure NFA matcher.
- `system_plugin.rs` and `awareness_plugin.rs` are pure "installed vs embedded" resolution. Staging lives elsewhere.
- `PlainText` `EscState` (`job_exec.rs:396-450`) is an explicit parser state machine.
- `NativeStartupError{protocol_unsupported, cancelled, ..}` (`wake.rs:160-232`) is flag soup on an error type.
- The TUI relay's `pending_resume_ids` / `ready_sent` (`wake.rs:1466-1489`) is local protocol state.
- `AppServerProcess::start` deletes any existing socket (1315). It assumes a single bridge per project.
- `AwarenessTickState.last_remote_probe` / `ended_sessions` are in memory, with an hourly sweep as the backstop.
- `debounce_idle_edge` (`poller.rs:4887-4923`), `fold_review_serving` (4951) and `decide` (4837) are pure.
- `resolve_current_status_id` (3538-3550) silently remaps an unknown column to the first status in the same category.
- `reap_wedged_streak` (5053) is a hysteresis map. `last_session_prune`, `last_ci_poll`, `last_github_reconcile` and `next_forward_check` are plain cadence timers.
- `AwarenessDigest.cursor` (`poller.rs:1486-1505`) is in memory, seeded at the log head, and re-seeds on a read error (drops lines).
- `spawned` thread map (311-337): a panicked thread respawns with fresh latches, and a removed workspace leaves its `status.yaml`.
- Handoff `RunnerTransition` (`handoff.rs:294-332`), the `push_branch` decision table and `OriginBranch` (`actions.rs:240-383`) are explicit and pure.
- The `actions.rs` comments "clean up any stale dir from a previous crashed merge" (1041, 1370) do nothing: temp paths include the pid and a counter (1199-1208), so crashed `$TMPDIR/shelbi-merge-*` dirs leak.
- `ready.rs:19-20` vs `41-43`: the docs disagree on whether a timeout sends the prompt anyway.

---

## Cross-slice leads

- **`workspace.rs` (orch-workspace):**
  - ready / transition / deferred / refused / push-blocked / pushed-tip / review-loaded marker paths and IO (199-510, 949-1054);
  - dispatch and resume clearing them (2680-2712, 2940-2967);
  - `resume_limit_stalled_pane`, `kill_workspace_pane`, `workspace_launch_age`.
  - These are the other half of H2, M1, M2, L2 and H5.
- **`shelbi-app/src/supervision.rs`** (app slice): `SupervisionState` (H4). Also used by `load.rs`, `shelbi-app/src/session/restart.rs` and `shelbi-tui/src/shell/session.rs`.
- **`shelbi-state/src/event_log.rs`** (state-core):
  - `recent_dispatch_active` (2836-2931), the cross-process "launch in flight" state built from a log-tail grep (H4);
  - `latest_task_transition_targets` (M9);
  - `last_pushed_tip_from_events` (~3250);
  - the global `events.log` used as the heartbeat clock (410);
  - the applied event cursor and `event-delivery.lock` (1263, 1545-1560), which the Codex bridge reads without the lock (`wake.rs:3106-3108`, M3).
- **`shelbi-state` stores:**
  - `zen_flow.rs`: no lock, unlike `pr_status.rs` (M11);
  - `awareness.rs` (`update_awareness` 344, history writers, M7);
  - `workspace_status.rs` (`status.yaml`, M6 / H5);
  - `agent_workspaces.rs:493-549` (handoff claim, L5);
  - `daemon_poller.rs:44` (poller flock);
  - `updated_at` writers (lib.rs 3571-4132, `issue_store.rs:983`) that defeat M10 guard #2;
  - the task-ownership overlay vs the index `assigned_to` (`board_index.rs:574-582`, lib.rs:3964), folded on the review path but not the dev orphan path (H3).
- **Status-move writers outside the slice that H1's single function would absorb:**
  - `review_ui.rs:371-470`
  - `mutate.rs:466-487`
  - `mutate/start.rs:180,411`
  - `shelbi-cli/src/commands/issue.rs:1156,1272`
  - `shelbi-cli/src/commands/workspace.rs:693`
  - tracker backends (`shelbi-state/src/{issue_store,github_store,linear_store}`).
- **Clients (tui / desktop):** relaunch markers written by "reopen" and consumed at `poller.rs:5102,5335,5837,6357` (H4). The review-loaded marker is read at `shelbi-tui app.rs:1438,1537` and cleared by `review_ui.rs:517` / `review_session.rs:551` (H5). `resolve_task_status` is duplicated in `shelbi-tui/src/kanban.rs` (L8).
- **`shelbi-cli` (cli-daemon):**
  - `commands/daemon/poller.rs:68-115` (poller replace without the quit barrier);
  - `commands/zen.rs` (signal cleanup at 60-70, exit 75 as the retry contract, the dry-run dedupe set);
  - `commands/job.rs` and `main.rs` `__job-exec` (M7);
  - `status.rs:537-540` (Codex health, M3 / M5);
  - `commands/send.rs` (workspace lock, L9).
- **Runtime:** `SlotDeath` / `KillReason` deliberate-vs-crash classification in the session backend. Single-bridge-per-project is assumed, not enforced (`wake.rs:1315`).
- **`zenmode.md` / orchestrator prompt:** it carries `PinnedPrIdentity` flags and the pr-create retry policy (L7, M11). That is protocol state held in agent prose.
- **Lock order to check across slices:** `lock_workspace` → `lock_git_worktrees` → `lock_zen_target_build` / cleanup (`zen.rs:7740, 9028-9033`) vs dispatch and resume.
