# Implicit state machines: app slice

**Slice:** `crates/shelbi-app` in full: all 137 `.rs` files (src, build.rs, build_support, tests). Surveyed 2026-11-18 against `origin/main` e8145b3f. All paths below are relative to `crates/shelbi-app/src/` unless they name another crate.
**Findings:** 19 total: **2 high**, **11 medium**, **6 low**, plus a Minor list.
- The two high-risk machines are both in the diff view's `DiffSession`. Its changeset-load lifecycle can strand an error, a scope or a "Loading" state forever. Its comment-save pipeline can drop a comment that failed to save, together with its "not saved" notice.
- The most common pattern in this crate is a **"save in flight" or "request in flight" machine rebuilt by hand per form**: 9 copies in Settings, plus the new-project wizards, `LinearChoice`, `IssueEdits` and the diff session. Each copy has slightly different guards against stale answers, and several real bugs come from that drift.
- The second pattern is **shared controllers with client-owned state**: pane restart/reconnect, review open, decision gating, board loading. shelbi-app supplies the pieces, and the TUI and desktop each assemble their own machine. Parity then rests on hand-written code in two places.

**Coverage:** six parallel reading passes split the crate into groups. Each pass walked every file in its group, grepped for flags, timers, markers and enum writers, and read the candidates in full:
- (a) activity, view, board, board_search, refresh, changes, nav, supervision, workspace_panel, layout_sub, zen, toast, task_*, estimate, close_others;
- (b) diff/*, review.rs, review/diff_load.rs, tests/diff_*;
- (c) settings.rs, settings/** and tests/settings_*;
- (d) launch*, session/*, source*, far, reachability, remote_window, exec*, wire*, state_write, lib, tests/remote_link, tests/relay_query;
- (e) add_project_wizard, overlay/*, new_project, new_task*, linear_setup, tracker_connect, onboarding*, prereqs*, harness_picker, feedback*;
- (f) issue_edit*, markdown_edit/*, text_area, changelog, reminders, jobs, command, command_model, about, licenses, theme, build.rs, build_support, tests/command_scenarios, no_ui_deps, parity_doc.

I re-read the code myself for every claim marked **verified**. Claims marked *suspected* were traced but not reproduced.

**No findings in** (pure functions or plain data):
- diff/parse, intraline, highlight, search, help, git, model, rows;
- markdown_edit/inline, blocks; issue_edit/merge;
- theme, about, licenses (minor only), estimate, task_rows, task_properties, close_others, harness_picker, tracker_connect, exec, launch/remote;
- settings/workflow_graph, summary, terminal_preview;
- build.rs and build_support.

**Already explicit and healthy:** `SupervisionState::decide`, `ClosedHistory`, `ToastSlot`, `DiffLoad`, `prereqs` run generation, `MachineForm` save generation, `revert.rs` plan/confirm/run with a fingerprint re-check, settings `Job`/`Ticket`.

## High

### 1. Diff changeset load lifecycle (scope, generation, digest, loading, error)

- **Entity:** one `DiffSession` and its `DiffState`: which changeset is shown, which one is being read, and whether the read failed.
- **Encoding:**
  - 1: `DiffState.loading: bool`, `error: Option<String>`.
  - 5: a generation protocol between the session and the loader thread.
  - 6: watch timer, `WATCH_BACKOFF`, and a digest gate that publishes only on change.
  - 8: the scope is held in three places.
- **States:**
  - Opening.
  - Loaded(scope).
  - Loading(new scope).
  - Failed with rows still showing.
  - Failed with no rows.
  - **Dead session**, which the code never names: `open_backend` failed, so no workers exist and every channel send goes nowhere.
  - Representable but invalid:
    - `error = Some` after a later successful read of the same content.
    - The three scope copies disagreeing: `DiffState.changeset.scope`, `DiffSession.scope` (written at request time, `diff/session.rs:292`), and the loader's local `scope` (`diff/session.rs:675`).
- **Transitions / writers:**
  - `loading`: set at `diff/state.rs:225` and `:1093` (`choose_scope`); cleared at `:599` (`set_changeset`) and `:620` (`set_error`).
  - `error`: set only at `diff/state.rs:621`; cleared only at `:600`.
  - Session generation: `request_load` `diff/session.rs:337-340`; checked at `:379`.
  - Loader: `generation`/`scope` `:674-679`; digest reset only on an explicit Load (`:679`); digest gate `:727-731`; failure path `:721-724` publishes `Failed` without touching the digest.
  - Open failure: hard-coded `generation: 0` (`diff/session.rs:217-221`).
- **Where it lives:** `diff/session.rs:150-345, 370-390, 655-735`; `diff/state.rs:590-625, 1085-1095`.
- **Risk: high.** All four failure modes below are verified from code. No test covers any of them.
  1. **Sticky "couldn't refresh".** A live workspace's watch read fails once (an SSH blip), and `set_error` runs. The next good read has the same digest, so `read_changeset` returns without publishing (`:727-729`). Nothing else clears `error`, so the error banner stays up indefinitely on a quiet worktree. The TUI and the desktop both render it.
  2. **Can't return to the shown scope after a failed scope switch.** The user picks commit X and its read fails. `set_error` sets `loading = false`, while the shown changeset still has the old scope. Picking the old scope again hits `self.changeset.scope == scope && !self.loading` (`diff/state.rs:1090`), which is a no-op. Refresh re-reads `DiffSession.scope`, which is X (`:285`), and the watch keeps re-reading X. The only way out is to pick a third scope.
  3. **"Loading" forever.**
     - The user presses `r` before a slow `open_backend` (SSH) fails, so the session generation is now 1.
     - The failure event carries generation 0 and is dropped (`:379-381`).
     - `load_rx` died with the thread, so every later request is lost silently (`let _ =` at `:339`).
     - The TUI's "dead session" heuristic (`error && table empty`, `shelbi-tui/src/shell/diff_view.rs:194-211`) never fires, because `error` stays `None`.
  4. **Desktop Try Again is a no-op on an open failure.** `desktop/src/views/diff_view/mod.rs:529-532` calls `session.refresh()` into a session with no workers. The TUI reopens instead (test `r_reopens_a_session_that_failed_to_open`). This is parity drift caused by the missing Dead state.
- **Recommendation: make explicit.** One load-state enum owned by `DiffSession`:

  ```
  Opening | Dead(why) | Idle{shown} | Loading{scope, gen} | Failed{scope, why, shown}
  ```

  `DiffState` reads it. Reset the digest after a failed read, so the next good read republishes and clears the error. Compare `choose_scope` against the requested scope. Expose `is_dead()` so both clients reopen the same way.

### 2. Diff comment save pipeline (unsaved overlay, store_error, SeenState)

- **Entity:** one local comment op on its way into `comments.json`, locally or through the far daemon.
- **Encoding:**
  - 3: `comments.json`/`viewed.json` under a file lock, written read-modify-write (`diff/store.rs`).
  - 1: an `unsaved: VecDeque` overlay, `store_error: Option`, and `SeenState.comments: Option`.
  - 5: a positional `pop_front` protocol that relies on saves answering in order.
- **States per op:**
  - AppliedLocally.
  - InFlight.
  - Saved.
  - FailedButShown. Not durable: the op has already left the overlay.
- **Transitions / writers:**
  - `unsaved`: push at `diff/session.rs:297`; pop at `:403` (both Ok and Err); overlaid in `show_comments` `:361-366`.
  - `store_error`: set at `:411` and `:418`; cleared by *any* successful read (`:399` Comments, `:406`, `:415` Viewed).
  - `seen.comments = saved.ok()` at `:510`, so a failed save leaves it `None`.
- **Where it lives:** `diff/session.rs:290-300, 395-420, 500-560`.
- **Risk: high (user data loss, verified).**
  - The comment at `:409-410` says that on a failed save "what's on show stays, so the words aren't lost". The code does the opposite:
    - The failed op has already been popped (`:403`), and `seen.comments` is now `None` (`:510`).
    - On the next `REVIEW_STATE_WATCH` tick, `send_changed` (`:545-551`) reads the store. The document differs from `None`, so it sends `Event::Comments(doc)`.
    - `show_comments` replays only the ops still in `unsaved`, and the failed one isn't there. So the store's document, without the new comment, replaces the view, and the comment the user typed disappears.
    - In the same poll, `store_error = None` (`:399`) clears the "not saved" notice.
  - A plausible trigger is one transient `FarBackend` round-trip failure followed by a working read.
  - A failed viewed mark is the asymmetric twin: the local mark stays and `store_error` lingers until an unrelated read.
  - No test covers a failed save.
- **Recommendation: make explicit.** Keep failed ops in the overlay as `Failed{op, why}` until they are retried or discarded, and add a retry path. Split `store_error` into `read_error` and `save_error`, each with its own clearing rule.

## Medium

### 3. Settings "save in flight", hand-written nine times with drifting guards

- **Entity:** every Settings form that writes config off the UI thread.
- **Encoding:** 1, 5, 8.
  - `SaveStatus` is defined twice: `settings/general.rs:43-53`, and an identical `settings/orchestrator.rs:71-77` that `properties.rs:54` reuses.
  - Each page keeps its own `pending`/`saving`/`queued`/`busy` flag beside it.
- **States (conceptual):**
  - Unloaded.
  - Loaded × {Idle, Saving(+queued), Saved, Failed}.
  - Unreadable.
  - Representable but invalid: `status == Saving` with the idle flag set, or the reverse; `saving == true` with nothing running after a stray completion; a load applied mid-save.

| Copy | State | Writers / guards |
|---|---|---|
| `GeneralForm` (general.rs:423-440) | `pending: Option<Change>`, `status` | `begin` 762-766; `saved_result` 771-786; `load` refuses while pending (448) |
| `OrchestratorForm` (orchestrator.rs:106-115) | `pending`, `status`, `restart` | `choose` 177-185; `saved_result` 197-208; `load` guard 121; `restart_result` 241-246 takes any answer |
| `PropertiesForm` (properties.rs:172-185) | `saving`, `queued`, `status`, `workflow` | `commit` 374-430 (queues 376-379); `saved_result` 436-467; `forget_save` 472-481; `load` guard 192 |
| `KeybindingsState` (keybindings.rs:178-179) | `saving` | `finish` 451-462; `saved` 425-428 clears unconditionally; `load` 206-215 **unguarded** |
| `WorkflowsState` (workflows.rs:294-297) | `saving`, `queue` | `request` 436-452; `committed` 457-486 **drops a stray completion** (458-460); `loaded` refuses while saving (383) |
| `AgentsState` (agents.rs:274-276) | `saving`, `queue` | `request` 351-362; `committed` 366-389 **no in-flight check**; `loaded` 310-321 **no guard** |
| `TrackerForm` | `connecting` | see #10 |
| Workflow/agent editors | `busy` | see #12 |
| `MachineForm` (machine_form.rs:173-174) | `saving: Option<u64>` + `runs` | the only copy with a generation (healthy) |

- **Risk: medium.** Verified failure modes:
  1. **Agents and Workflows have drifted.**
     - `tests/settings_workflows.rs:333` pins the stray-completion and mid-save-load guards for Workflows only.
     - `AgentsState::committed` sets `saving = false` and starts the next queued edit (`agents.rs:385-388`) even when nothing was in flight. A second save can then start on a pre-save `config` while the first is still running, which leads to a self-inflicted "changed on disk" refusal.
     - The Agents load guard was pushed out to the clients, and they now disagree: desktop drops every load while saving (`desktop/src/views/agents.rs:141-144`), while the TUI takes failed loads (`crates/shelbi-tui/src/shell/settings/agents.rs:286-288`). That is three behaviours for one rule, and it breaks the CLAUDE.md "toolkit-free logic lives in shelbi-app" rule.
  2. **Properties silently drops a queued commit when a save fails.** On `Err`, `saved_result` clears `queued` and `workflow` (`properties.rs:456-460`). A default-workflow pick or field commit made while the first save was out is lost. The pick reverts, and a text field stays dirty with no save and no error of its own. No test covers `queued`.
  3. **Keybindings:** a reload that lands mid-save shows the pre-save file until the save's result replaces it (low).
  4. **Orchestrator restart:** stale answers are left to the client (comment at `orchestrator.rs:136-137`). The TUI uses `Job` tickets (`shelbi-tui/src/shell/settings.rs:1374-1377`); desktop is safe only because its page entity is per-project.
- **Recommendation: make explicit.** One shared `SaveSlot<T>` (or an extension of `Job`) with these states: idle, saving{generation, queue}, saved, failed. It would have `begin`/`complete(ticket)`/`queue`, and the load-during-save policy would live in one place. Merge the two `SaveStatus` enums. Move the Agents load guard into `AgentsState::loaded`. Decide and document what a failed save does to the queue.

### 4. Issue draft autosave and write lane (`IssueEdits`)

- **Entity:** the per-issue `Draft` (`issue_edit.rs:301-321`), its two `FieldDraft`s (`:270-285`), and the per-issue `Lane` (`:375-390`). It is shared by every desktop window on a project (`shared`, `:445-450`).
- **Encoding:**
  - 1: about 14 fields.
  - 2: the failure kind is a `String`.
  - 5: `begin_write`/`end_write`/`finish` protocol.
  - 6: `SAVE_DEBOUNCE` `:63`, `STALE_WAIT` `:67`, `WRITE_DEADLINE` `:74` (enforced by the host), `STALE_RETRY_LIMIT` `:78`.
  - 8: `SaveStatus` exists, but it is derived after the fact by priority in `status()` (`:913-945`).
- **States:**
  - Clean, Unsaved (debouncing), Saving (in flight), AwaitingRead (refused as stale), Failed(kind), Invalid, Conflict(field), Saved.
  - `Saving` merges "a write is out" with "refused, waiting for a fresh read" (`:934`).
  - `Failed(String)` merges five causes: stale retries exhausted (`:725`), NotFound (`:731`), Backend/ShuttingDown (`:733-734`), NoAnswer (`:742`), and the stale-wait timeout (`:598-600`).
  - Representable but invalid:
    - `awaiting` together with `failure`;
    - `theirs` set on both fields (only the Title conflict shows, `:355-357`);
    - `lane.held` with `in_flight = None`.
- **Transitions:**
  - All in `issue_edit.rs`: `open` 473-481, `edit` 494-506, `set_workflow` 521-533, `flush` 538, `close` 547, `retry` 560, `discard` 569-587, `poll` 591-606, `take_job` 610-674, `finish` 681-751, `begin_write`/`end_write` 763-794, `apply_read` 828-904, `resolve` 960-977, `collect` 996-1005.
  - Hosts: `desktop/src/views/board_state.rs` (poll 1701, record_save 1736, begin/end_write 1933/1991) and `desktop/src/views/issues.rs:877,898`, the only place `WRITE_DEADLINE` is enforced. The TUI does not use `IssueEdits`.
- **Risk: medium.** Each item below was verified unless marked suspected.
  1. **Stringly failure kind.** A NoAnswer failure is recognised only by `failure.as_deref() == Some(&ExecError::NoAnswer.to_string())` (`:898`). A backend message that happened to equal that text would be auto-cleared as "landed after all".
  2. **A NoAnswer that carried a workflow change never heals itself.** `:739-741` restores `workflow`, so `dirty()` stays true. The `!draft.dirty()` check at `:898` then never passes, even when a read shows the write landed, because `apply_read` never compares the workflow. The draft stays Failed until the user retries. The test `a_workflow_change_with_no_answer_goes_again_on_retry` pins only the retry.
  3. **Lane liveness depends on the host.** If a host drops a `SaveJob` or `WriteTicket` without calling `finish`/`end_write`, the lane stays `in_flight` forever. `take_job` then returns None (`:613`), and `collect` never frees the draft (`:998`).
  4. **`discard` (`:569-587`) has no callers and is unsafe mid-flight** (suspected). It resets `base`/`local` but leaves `sent`. A later `finish(Ok)` sets `base = sent` (`:704`), and the next poll writes the old text back.
- **Recommendation: make explicit.**
  - Per-draft phase: `Phase { Idle, Debouncing{last_edit}, InFlight{seq, sent}, AwaitingRead{since, refusals}, Failed{FailureKind}, Conflict{field, theirs} }`.
  - Use a typed `FailureKind` instead of the string.
  - Name the `Lane` states: Idle, InFlight, Unconfirmed, Holding.
  - Delete `discard`, or make it refuse while busy.

### 5. Remote host link and refusal memory (`HostLink` + `RemoteSessionConnector`)

- **Entity:** one far host's relay channel as seen by every window, tab and pane on that host, plus each connector's remembered refusal.
- **Encoding:**
  - 1: `HostLink.current: Mutex<Option<link>>` and `opening: Mutex<Option<(Instant, RemoteError)>>`.
  - 4: ssh and channel liveness.
  - 6: `REOPEN_PAUSE`.
  - 8: `ConnectFailure` has no "refused" variant, so a refusal travels in a side channel, `RemoteSessionConnector.refusal` (`session/remote.rs:80`).
- **States:**
  - NeverOpened.
  - Up.
  - Dropped. Seen lazily: `live()` clears it, `launch/link.rs:314-320`.
  - Opening. The `opening` lock is held through `check()` and `open()`.
  - FailedPaused. Answered from memory for `REOPEN_PAUSE`.
  - FailedExpired.
  - SignInRefusedHeld. Held until `allow_sign_in`, `:359-364`.
  - Representable but questionable: a connector holds `refusal = DaemonStopped` while the link is Up. `refusal()` hides a stale refusal only for sign-in errors (`session/remote.rs:100-103`).
- **Transitions:**
  - `launch/link.rs:332-352` (`connect`).
  - `:316-317` (drop a dead channel).
  - `session/remote.rs:107-132` (`channel` writes `refusal` and maps errors: Unreachable or a sign-in refusal becomes `ConnectFailure::Unreachable`; DaemonStopped, NoLongerNative and Failed become `ConnectFailure::Message`).
  - `remote_window.rs:108-118` (`reconnect_check`).
  - Desktop: `desktop/src/app.rs:1351-1376` (`keep_up`/`refusal`), `:1946-1970` (`host_kept`), `:2076-2100` (`after_far_action`).
- **Risk: medium.**
  1. **Cross-project bleed (verified).**
     - `HostLink::shared` keys the link by host only (`link.rs:281-294`). Each caller passes its own `check`, which is a per-project `reconnect_check` (`session/remote.rs:110`), but the failure is stored host-wide (`link.rs:349`) and returned to *every* caller for `REOPEN_PAUSE` (`:336-338`).
     - So project A's "no Shelbi project named A" (`RemoteError::Failed`) is handed to project B's panes for up to a second. B's connector turns it into `ConnectFailure::Message` and records it as B's refusal.
  2. **A refused host spends the pane's restart budget** (suspected, traced). The module doc says a stopped daemon "ends the pane's loop" (`session/remote.rs:27-29`). In practice DaemonStopped becomes `Message`, which desktop `session_view.rs:780-781` treats as a death, then restarts. Each restart re-runs `channel()` until `MAX_RESTARTS_IN_WINDOW`, so the pane ends at "Stopped after 5 restarts" next to the window's Start Shelbi bar.
  3. **A stale Start Shelbi bar** (suspected). After Start, the bar is cleared (`app.rs:2088`). The next watch tick falls back to `connector.refusal()`, which still holds DaemonStopped until some pane of that connector gets through, and panes that already gave up never retry.
- **Recommendation: make explicit.**
  - Use `enum HostLinkState { Down, Opening, Up, Paused{err, until}, AwaitingSignIn{err} }`, and keep per-project check failures out of host-wide memory.
  - Add `ConnectFailure::Refused(RemoteError)`, so panes stop instead of counting deaths and the window reads one source instead of three.

### 6. Session pane lifecycle: connect, live, reconnect, restart, back off, give up

- **Entity:** the one session a client view shows: orchestrator, agent, review, workspace or remote pane.
- **Encoding:** 8, 6, 3/4, 5.
  - 8: explicit cores, leaky edges. `SupervisionState` (`supervision.rs:82-199`) is explicit. `RestartController` (`session/restart.rs`) adds a `deliberate_close` side flag and projects the core lossily. `ReconnectController` (`session/reconnect.rs:52-59`) has a private `Phase`, but clients sequence its calls by hand.
  - 6: backoff, the 15 s connect deadline, a 250 ms pause.
  - 3/4: connect classification reads session dirs and `final.txt` on disk (`session/mod.rs:307-369, 439-453`).
  - 5: "call `alive()` before `dead()`", "`begin(reopen)` on show".
- **States:**
  - Connecting (`retry_connect` Starting/Awaiting, `session/mod.rs:385-426`).
  - Live.
  - Reconnecting{attempt}.
  - Restarting{attempt}.
  - Backoff.
  - GaveUp.
  - StandDown.
  - Representable but invalid:
    - (a) After give-up, `RestartController::dead` returns `Backoff{next_attempt: MAX+1}`, not GiveUp. `SupervisionAction::None` covers alive, never-alive, given-up and in-backoff (`supervision.rs:178-181`), and `restart.rs:125-131` maps the given-up `None` to `Backoff`. The test at `restart.rs:196-202` pins "further deaths change nothing".
    - (b) `deliberate_close` stays set after a failed teardown unless the client calls `clear_deliberate_close` (`restart.rs:48, 77-85`).
    - (c) Reconnecting and a restart backoff active at once.
- **Transitions:**
  - restart.rs `begin` 65-72, `alive` 95-104, `dead` 109-132.
  - supervision.rs `decide` 133-199 (`gave_up` latched at 187).
  - reconnect.rs 89-152.
  - launch/terminal.rs `run` 111-153 is a second driver.
  - Clients own the real pane machine:
    - Desktop: `desktop/src/views/session_view.rs` `Slot` plus `restart`, `reconnect`, `reopen_on_show`, `generation` (299-307, 703-787, 849-930). It hand-sequences `link_lost` → `take_due` → `unreachable` at 716-721.
    - TUI: `crates/shelbi-tui/src/shell/session.rs` has its own `Slot`, latches GaveUp itself (389-391), and does not use `ReconnectController`.
- **Risk: medium-low.**
  - The give-up latch lives only in each client's `Slot`. A path that calls `dead()` again on a given-up pane renders "restarting (6/5)", and on desktop it schedules backoff ticks forever.
  - Past-bug tags in the code all sit at this machine's edges: `rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`, `rt-tui-attach-retry-unbound-socket`, `rt-re-entering-a-review-fails-to-attach`.
- **Recommendation: make explicit.**
  - Make `RestartController::dead` return a sticky `GaveUp`.
  - Add `ReconnectController::attempt_failed` for the case where the first connect is unreachable.
  - Move the pane `Slot` enum and its transition function into shelbi-app so both clients share one machine.

### 7. Activity title cache: per-task-id resolve lifecycle (`ActivityMetaCache`)

- **Entity:** each task id that appears in the activity feed and its resolved title/meta (`activity.rs:333-355, 497-515`).
- **Encoding:** 1, 3, 4, 6.
  - Two maps: `entries` and `backoff`.
  - `meta: Option`, `terminal: bool`, `mtime: Option`, `fetched: Instant`.
  - `REMOTE_META_TTL` 15 s (`:363`).
  - Two log-only flags: `hydration_started`, `hydration_finished`.
- **States per id:**
  - Unknown.
  - ResolvedOpen(fs, invalidated by mtime).
  - ResolvedOpen(remote, TTL).
  - ResolvedTerminal (frozen).
  - ConfirmedMiss (frozen for the session).
  - BackingOff(`retry_at`, `attempts`), alone or over a stale entry.
  - The rules are centralised in `entry_stale` (`:470-489`), which is good.
- **Writers:**
  - `serve_open_ids_from_index` 594-614.
  - `resolve_task_meta`: batch failure 745-757 via `note_fetch_failure` 538-551; success 760-781; per-id error 786-788; confirmed miss 793-807.
  - Cache owners: `shelbi-tui/src/activity.rs`, `shelbi-tui/src/shell/mod.rs:1286,1302`, `shelbi-orchestrator/src/source.rs:51,58`.
- **Risk: medium.** It is display only, but the code's comments record four past incidents on this machine:
  - the 2026-09-08 per-frame `GitHubStore::get` storm (`:326-327`);
  - the sidebar-never-loads slug re-search (`:327-328, 464-466`);
  - the done-history refetch loop (`:349-350, 432-434`);
  - a 750 ms retry storm (`:369-371, 511-513`).

  Verified hazards remaining:
  1. **The terminal freeze is checked before the mtime** (`:479-481`). On the filesystem backend, a done task that is reopened, or whose title or assignee changes, is never re-read even though its file's mtime changed.
  2. **A confirmed miss is frozen before the mtime as well** (`:475-477`). An id that one successful fetch omits (for example a just-created issue the tracker hasn't indexed yet, *suspected*) shows the raw id until restart.
- **Recommendation: make explicit.**
  - Per-id `enum MetaState { Resolved{meta, validator: Mtime|Ttl|Frozen}, Miss{since}, BackingOff{retry_at, attempts, last} }` with one `next_action(now, mtime)`.
  - Let an mtime change unfreeze terminal and miss entries on the filesystem backend.
  - Give misses a long TTL instead of a permanent freeze.

### 8. Zen Mode tri-state collapsed to a bool behind a relative toggle

- **Entity:** the palette's Zen command: `CommandModel.zen_on` (`command.rs:388`), title at `command.rs:509-517`, `Mutation::ToggleZen`.
- **Encoding:** 4 (mirrors `ZenModeState {Off, Paused, On}`, `shelbi-state/src/lib.rs:1781-1786`) and 8 (the enum collapses into a bool).
- **States:** Off / Paused / On upstream. The mirror sets `zen_on = !matches!(zen_mode, Off)` (`command_model.rs:132`), so Paused reads as On.
- **Transitions:** daemon `toggle_zen_mode` (`shelbi-state/src/lib.rs:2880-2893`): On→Off, Off|Paused→On (`:2886`; pinned by test `toggle_zen_mode_from_paused_collapses_to_on`).
- **Risk: medium. Confirmed defect (verified).**
  - While Zen is Paused, the palette shows **"Turn Zen Mode off"** (`command.rs:512-513`), but running it turns Zen **On**.
  - The mutation carries no target and no expected prior state (`shelbi-proto/src/control.rs:392`). A stale snapshot, or a second client, therefore flips Zen the opposite way from what its label promised.
  - `tests/command_scenarios.rs:491-519` covers only Off and On.
- **Recommendation: make explicit.** Carry `ZenModeState` in `CommandModel` and title from it (for example "Resume Zen Mode" when Paused). Prefer an absolute `SetZen{to}`, or a toggle that carries the expected prior state.

### 9. New-project flow: built twice, each copy a page enum plus flags

- **Entity:** the desktop `Wizard` (`add_project_wizard.rs:373-427`) and the TUI `Form` (`overlay/add_project.rs:186-214`). Both wrap `IssuesChoice` (`new_project.rs`) and `LinearChoice` (#10).
- **Encoding:** 8, 1, 5.
  - Wizard fields: `screen`, `chosen`, `name`, `check`, `in_flight`, `preflighting`, `seeded`, `chose_runner`, `chose_issues`, `chose_mode`, `git_init`, `pick_up`, plus creation `Progress { request, running, done, active, failed, warnings }`, `created`, `opening`, `open_error`.
  - Form fields: `page`, `detected`, `check`, `offered`, three `chose_*` flags, `error`.
- **States:**
  - Folder → Defaults → Issues → Settings, then (desktop only) Creating → Created / Failed, then Opening.
  - Representable but invalid:
    - `running && failed.is_some()` (`stage_failed` 1133-1138 leaves `running` until `creation_failed` 1287-1303);
    - `opening` with `created == None`.
- **Transitions:**
  - Wizard `screen`: 734, 801, 993, 1000, 1006, 1028, 1040, 1046, 1083, 1172.
  - Form `page`: 294, 302, 399, 424, 626-628, 648, 654.
  - The step from Issues to Settings has three writers in each copy: Wizard 733-736, 800-803, 1045-1048; Form 398-401, 423-426, 653-656.
- **Risk: medium.**
  1. **Seeding discards user choices (verified by reading).**
     - `select_model` doesn't set `chose_runner` (Wizard 520-525, Form 587-592).
     - When another folder's check lands, `seed` rebuilds the picker with `HarnessPicker::new(recommended, None)` unless `chose_runner` is set (Wizard 673-675, Form 327-329).
     - Result: pick a model, go back, pick a different folder, and the model choice is silently reset.
     - `set_repo` doesn't set `chose_issues` either (Wizard 714-716), possibly on purpose.
  2. **The copies drift.**
     - The TUI `Form` has no in-flight state for the folder check, the GitHub preflight or the Linear reads. It is safe only because its host blocks the UI thread (`shelbi-tui/src/overlay/add_project.rs:236-276`).
     - TUI creation has no state and no retry. It closes the form and calls `create_project` with `Attempt::New` only (`shelbi-tui/src/launch/mod.rs:277-304`). After a partial failure, re-adding the same folder hits the registered-id clash (`shelbi-orchestrator/src/project_create/mod.rs:306-309`), where the desktop's `Attempt::Retry` finishes the project (inferred).
  3. **Creation success is accepted after a stage failure.** `creation_succeeded` checks only `creating()` (1153-1160). Correctness relies on the engine always returning Err after `stage_failed` (`project_create/mod.rs:1011-1027`). The run generation that drops reports from an earlier run is in the desktop host (`desktop/src/views/add_project.rs:629-630, 723, 743`), not in the model.
  - GitHub's in-flight flag lives on `Wizard.preflighting`, while Linear's lives inside `LinearChoice`. `IssuesChoice::busy` reports Linear only (`new_project.rs:476-480`).
- **Recommendation: make explicit.**
  - One shared flow machine in shelbi-app with a step enum and sub-states:
    - check: `Idle` / `InFlight(key)` / `Done`
    - issues: `Editing` / `Preflighting(repo)` / `Verified`
    - creation: `Running{gen}` / `Failed` / `Created{open}`
  - Both clients render it, and every edit on a step sets that step's `chose_*`.

### 10. Tracker connect and Linear setup (`LinearChoice` inside `TrackerForm` and the wizards)

- **Entity:** `LinearChoice` (`linear_setup.rs:310-327`): key verification, teams, lists, login, preflight, viewer. It is shared by Wizard, Form and Settings' `TrackerForm` (`settings/tracker.rs:112-125`).
- **Encoding:** 8, 1, 4, 5.
  - The parts are enums: `KeyState`, `Loadable`, `Login`, `Preflight`. They sit beside the flags `changing_key`, `verified`, `viewer_asked`, and one shared `error` slot.
  - `TrackerForm` adds `edited`, `connecting`, `error`, `refused`, `connected` and an optimistic mirror of the project's `issue_tracker:`.
  - Hosts drive `due()`/`linear_due` until it returns None.
- **States:**
  - Key: Missing, Checking, Valid, Rejected, ChangingKey.
  - Teams: Idle, Loading, Loaded, Failed.
  - Login: Wanted, Running.
  - Preflight: Wanted, Running(conn), Verified.
  - Form: Clean, Edited, Connecting, Refused, Connected.
- **Transitions:**
  - `key` 701, 722, 734, 741, 778-779.
  - `teams` 629, 663, 707, 711, 725.
  - `login` 606, 651-659, 719.
  - `preflight` 622, 680, 757.
  - `changing_key` 355, 363, 723.
  - `error` at 12 sites.
  - `viewer_asked` 685, never reset.
  - TrackerForm: `load` 144-153, `seed` 160-173, `edit` 209-214, `submit` 374-401, `connected` 406-420, `cancel` 423-428.
- **Risk: medium (desktop only).** Desktop dispatches every due request concurrently (`desktop/src/views/add_project.rs:501-514`, `desktop/src/views/tracker_section.rs:124-135`); the TUI runs them one at a time.
  - **The answer guards don't match each other (verified).** TeamLists is matched by `team_id` (747), Preflight by connection (754), and Login by `Running` (716). **Teams is guarded only by "currently loading"** (695) with no request identity, and **Viewer has no guard at all** (738-745).
  - **Stale teams after a key change** (suspected race):
    1. A key works and teams are reloading.
    2. The user changes the key.
    3. Login succeeds and resets teams to Idle (725).
    4. `due()` issues a second Teams read.
    5. The first Teams read lands late and is accepted, so the pickers list teams read with the old key.
  - **Key shown as missing.** A late `Viewer(Err(Rejected))` after a good Login flips the key to Missing through `note_key_failure` (744, 776-782).
  - **TrackerForm records the wrong connection.** `connected(Ok)` writes `current = wanted.config(current)` from the choice *at completion time* (`tracker.rs:412-414`), and the key mutators (`tracker.rs:281-314`) aren't gated while connecting. TrackerForm also has no ticket, so after `reset()` an older answer is accepted (the TUI compares the project only, `shelbi-tui/src/shell/settings/properties.rs:215-217`).
- **Recommendation: make explicit.**
  - Carry a request id on `LinearRequest`/`LinearAnswer`.
  - Fold `changing_key` and `login` into one key-flow enum.
  - Use a separate error per concern.
  - `TrackerForm::Connecting` carries the `ConnectTracker` it sent and derives `current` from it.

### 11. Board load and board search flags

- **Entity:** `BoardInput` (`board.rs:971-1003`) and `BoardSearch` (`board_search.rs:193-212`).
- **Encoding:** 7, 8, 1, 6, 5.
  - A `BoardOutcome` enum (`board.rs:657-676`) is flattened into `loading`, `error`, `banner`, `open`.
  - Search is flag soup: `due_since`, `in_flight`, `results`, `last`, `failed`, `remote`, `seq`, plus a cache.
  - `BoardInput.search_hits` is a copy that must be re-synced by hand after every search change (`board.rs:1134-1136`).
- **States:**
  - Board: NeverRead, Cold, Warm, FailedBeforeWarm, FailedAfterWarm, ColdAfterWarm (`loading = true` with non-empty `open`).
  - Search: Inactive, LocalOnly, Debouncing, InFlight, Results, Results+LoadingMore, Failed. `due_since` and `in_flight` are exclusive only by convention.
- **Transitions:**
  - Board: `apply_open` 1032-1092 (Failed 1074-1077 leaves `loading`; Cold 1078-1081; Warm 1082-1086), default `loading: true` 1017, copied to `IssuesModel.loading` at `view.rs:943`.
  - Search: `set_remote` 258-269, `set_query` 275-302, `request` 327-336, `poll` 340-347, `load_more` 351-357, `apply` 362-401 (Unsupported sets `remote = false` at 387).
  - External: `set_remote` on every board read (`shelbi-tui/src/kanban.rs:859`, `desktop/src/views/board_state.rs:498`); hit re-syncs at `kanban.rs:913` and `board_state.rs:499, 1169, 1188`.
- **Risk: medium.**
  - **The clients re-derive the board state differently.** The TUI uses `input.loading && tasks.is_empty()` (`kanban.rs:1957`; its tests mention a "stale flag", `:6043`). Desktop shows "Loading board…" in every empty column whenever `loading` (`issues.rs:1795`), and ranks `loading` above `error` in the detail pane (`issues.rs:658, 2284`). On ColdAfterWarm (for example after a daemon restart), desktop shows "Loading" columns beside populated ones while the TUI shows a normal board, and a failure before the first warm read reads as "Loading" on desktop.
  - **Two writers fight over `remote` (verified by reading).**
    - `board.rs:723-726` sets `remote_search = true` for any GitHub or Linear backend.
    - `search_issues` returns Unsupported when the `github:` or `linear:` block is missing (`shelbi-state/src/github_store.rs:719-729`).
    - With a query active, each board refresh then runs: `set_remote(true)`, re-arm, request, Unsupported, `remote = false`. That is one wasted request per refresh (cheap: it fails before any network call).
  - RateLimited and Unreachable search failures are never retried until the query changes (391-396).
- **Recommendation: make explicit.**
  - Board: `enum BoardLoad { Loading, Ready, Stale{cards}, Failed{last_known, msg} }` with one shared "show loading?" predicate.
  - Search: a `Phase` enum, plus a latched `RemoteCap { Yes, No, ProvenUnsupported }` so the board read can't undo the search's own finding.

### 12. Settings draft editors (workflow and agent) with `LeaveGuard`

- **Entity:** `EditorState` (`settings/workflow_editor.rs:539-566`) and `AgentEditorState` (`settings/agent_editor.rs:322-347`), duplicated in shape, plus `LeaveGuard<T>` (`settings.rs:382-451`).
- **Encoding:** 1, 2, 5, 8.
  - `busy` is shared by save, duplicate and delete.
  - `stale` is set by string-matching the error text.
  - `LeaveGuard` is two independent Options: `prompt`, `after_save`.
- **States:**
  - Clean / Dirty, where Dirty = `draft_dirty || typed_name != name || review_pending` (WE 622-629, AE 416-418).
  - Busy.
  - Stale (conflict).
  - Prompting.
  - LeavingAfterSave.
  - Representable but invalid:
    - `prompt` and `after_save` both Some (`LeaveGuard::leave` 400-407 ignores `after_save`);
    - `after_save` set while `busy` came from a duplicate or delete.
- **Transitions:**
  - `busy`: WE 586, 991, 1083, 1175, 1181; AE 362, 863, 875, 971, 977. There is no "job succeeded" transition; the client replaces the state (`desktop/src/views/workflow_editor.rs:761, 794`).
  - `stale`: WE 1102 and AE 894, both `e.starts_with("Changed on ")` (verified).
  - Guard: `save_and_leave` WE 1163-1166 / AE 943-946, `begin_save`'s `forget_save` WE 988 / AE 860, `saved` WE 1094/1104, AE 886/896.
  - TUI shell composition: `shelbi-tui/src/shell/settings.rs:1015-1037, 2057-2088`.
- **Risk: medium-low.**
  1. The conflict flag depends on the wording of `ConfigError::Conflict`/`ChangedOn` (`shelbi-state/src/workflow_config.rs:174,178`; `agents_config.rs:173,177`). Rewording the message silently disables it.
  2. `conflict()` is documented as "Save stays refused until Discard", but `begin_save` never checks it. Only the store's hash check re-refuses, and no client calls `conflict()`.
  3. Suspected: `save_and_leave` while `busy` sets `after_save`, then `begin_save` returns early without calling `forget_save`. If the busy job (a duplicate or delete) fails, `job_failed` leaves `after_save` set, and the next ordinary save navigates away unasked.
- **Recommendation: make explicit.**
  - One generic `DraftEditor` phase: Idle / Saving{leave_after} / JobRunning / Conflict.
  - Carry a typed conflict in `SaveOutcome` instead of matching text.
  - Clear the guard on `job_failed` and on a refused `begin_save`.

### 13. Review open flow and decision gate, duplicated across clients

- **Entity:** opening a review (resolve → choose a slot → load → wait for the daemon's `ReviewOpened` → open), and the "a decision is running" gate on the review panel.
- **Encoding:** 1, 5, 4.
  - `ReviewLoader` uses three Options (`review.rs:894-898`).
  - The panel's `merging: bool` (`review.rs:670`) is a copy of `ReviewDecisions` state (running = `failure.is_none()`, `review.rs:611-616`) that each client pushes in.
- **States:**
  - Open flow: Idle, Resolving(t), AskingSlot(t), Loading(t), AwaitingOpened(t).
  - Decision: Absent, Running, Failed.
- **Transitions:**
  - `ReviewLoader`: `open` 904-910, `resolved` 913-950, `slot_chosen` 955-962, `loaded` 967-981, `review_opened` 985-992, `cancel` 999-1006. Used only by desktop (`app.rs:4817, 6208`).
  - **The TUI reimplements the open flow** with `review_open_pending`, `review_opening`, `review_rx` (`shelbi-tui/src/shell/mod.rs:1453`, writers 2585, 4897-5010), without `cancel` or the stale-resolve check.
  - Decisions: `ReviewDecisions` `begin`/`landed`/`failed`/`take_failure`/`dismiss` (`review.rs:597-656`); `set_merging` (746-751) is pushed from TUI `shell/mod.rs:5343-5355` and desktop `app.rs:4771, 4776-4790`.
- **Risk: medium.**
  - This violates the "toolkit-free logic moves into shelbi-app" rule, and the two copies differ in behaviour.
  - There is no deadline on AwaitingOpened (suspected). If the health check never passes, `pending` stays set until another task opens, and a much later background `ReviewOpened` for that task then takes the view, contrary to the doc at `review.rs:887-889`.
  - A missed `set_merging` sync leaves the panel inert (Approve/Reject/Close are dropped, `review.rs:786-788, 800-802`) or ungated.
  - `ReviewPanelState::sync` (709-717) never revisits `active`.
- **Recommendation: make explicit.** Make the TUI use `ReviewLoader`, as an enum with a deadline on AwaitingOpened. Derive `merging` from `ReviewDecisions` at read time instead of copying it.

## Low

### 14. Diff comment delivery to the agent (`sent` flag plus a global `sending`)

- **Entity:** whether each task comment has reached the agent.
- **Encoding:** 3, 1, 5.
  - `sent` is persisted in `comments.json` (`diff/comments.rs:96-98`).
  - In-flight is one global `DiffState.sending` (`diff/state.rs:179`).
  - A reject marks threads sent by matching their formatted text inside the reason (`diff/comments.rs:326-332`).
- **States:** Unsent → InFlight → Sent → Unsent (on a human reply).
- **Transitions:**
  - `sending`: true at `diff/state.rs:1758`; false only at `:758` (`agent_sent`).
  - `sent`: true at `comments.rs:167-170` and `backend.rs:507-511` (`mark_sent_in`, called from both clients' reject paths); false only on Reply (`comments.rs:156`). `Edit` (`comments.rs:137-146`) leaves `sent` alone.
- **Risk: low-medium (verified).**
  - `sending` sticks at true when the job thread fails to spawn: `start_job` reports `StoreFailed`, not `AgentSent` (`diff/session.rs:653-655`). It also sticks when the backend never opened (`let _ =` send, `:304`). Every later `gY` answers "Still sending…" (`diff/state.rs:1754-1756`) for the life of the view.
  - A comment edited after it was sent is never re-sent.
- **Recommendation: make explicit.** Track a per-delivery `Delivery{ids, started}` that is cleared on every terminal path. Decide whether `Edit` resets `sent`, and document the rule beside the Reply rule.

### 15. Onboarding launch: a step enum plus side flags, and two writes with no "saved but not marked" state

- **Entity:** `OnboardingState` (`onboarding.rs:167-180`) plus the on-disk onboarded marker.
- **Encoding:** 1, 3, 5.
- **States:**
  - Loading (`setup == None`).
  - LoadFailed.
  - Intro / Harness / Instructions × `busy`.
  - Finished.
  - Upstream, `orchestrator_onboarded` ORs four on-disk signals and treats a read failure as onboarded (`shelbi-state/src/orchestrator_setup.rs:87-99`).
- **Transitions:**
  - `step` 246, 257-260, 366.
  - `busy` 343, 355, 408, 416.
  - `finished` 420.
  - `setup` 232, 358-364.
  - `load_error` 234-236.
- **Risk: low-medium (verified).**
  - `run_launch` saves the instructions, then writes the marker (`onboarding.rs:149-153`).
  - If the marker write fails after the save, `launched(Err)` (`:423-426`) leaves `setup.config` holding the pre-save hashes. The retry rebuilds from that stale config (`:400-403`), and `config_write::check` refuses with "The orchestrator's files changed on disk…" (`:158-160`). The user is stuck until they reopen.
  - By contrast, `continued(Ok)` does refresh `setup.config` (357-359).
  - A load failure has no retry transition, and clients start `load` once (`desktop/src/views/onboarding.rs:102-107`).
- **Recommendation: document, plus small fixes.** Have `run_launch` return the saved config (or model `SavedNotMarked`), and add a load-retry transition.

### 16. Reachability prober per-machine schedule, keyed by name only

- **Entity:** the live reachability of each declared remote machine (`reachability.rs:76-87`).
- **Encoding:** 6 (cadence 30 s, doubling to 300 s), 1/4 (three parallel `HashMap`s by machine name).
- **States per machine:**
  - NeverProbed.
  - Reachable.
  - Unreachable{interval}.
  - Pruned.
  - Representable but invalid: a result and backoff that belong to the machine's *previous* ssh host.
- **Transitions:** `set_targets` 126-135, `prune` 174-180 (by name), `collect_due` 183-189, `run` 203-248, `next_interval` 49-63.
- **Risk: low-medium (verified).** Editing a machine's ssh host while keeping its name wakes the worker, but `prune` keeps `results` and `next_due` by name. The view keeps showing the old host's result, and doesn't probe the new host until the old schedule expires (up to 300 s if it was backing off). There is no "probing" state, so the UI can't tell a stale result from a fresh one.
- **Recommendation: make explicit.** Key on `(name, host)`, or reset an entry when its host changes. Use one struct per target instead of three maps.

### 17. Markdown editor IME composition

- **Entity:** `Editor.marked: Option<Range<usize>>` and `composing: Option<(usize, String)>` (`markdown_edit/editor.rs:52,55`).
- **Encoding:** 1, 5.
- **States:**
  - Idle.
  - Composing.
  - Representable but invalid: `composing.0` or `marked` no longer pointing at the composed text.
- **Transitions:**
  - `set_marked` 1438-1464.
  - `commit` 1468-1494.
  - `unmark` 1497-1501.
  - `finish_composition` 1504-1520.
  - `replace_from_outside` 1408-1431 maps `marked` through the splice but **not `composing.0`**.
- **Risk: low-medium (the bug is in code; reachability is suspected).**
  - An outside splice before the composition leaves `composing.0` stale, so `finish_composition` records a wrong undo step.
  - `insert`, `backspace`, `delete`, `enter`, `undo` and `redo` refuse while composing. `toggle_mark`, `set_block`, `toggle_task`, `indent`/`outdent`, `set_link`, `delete_selection`, `cut` and `select` don't, and `apply` (279-297) never remaps `marked`. So a `set_block` mid-composition makes `commit` replace the wrong bytes.
  - Desktop sidesteps both by convention: it skips `set_from_outside` and key handling while composing (`desktop/src/views/doc_editor/mod.rs:195-198, 227-229`). Toolbar and mouse paths are unverified.
  - The fuzz test always commits right after `set_marked` (`markdown_edit/tests.rs:763-765`).
- **Recommendation: make explicit.** `enum Composition { None, Active{start, replaced, marked} }`, remapped in `apply` and `replace_from_outside`, or refuse/commit at every mutating entry point.

### 18. Mirrored GitHub PR state in the diff view (`SeenPr` → `PrView`)

- **Entity:** the task's PR as last read.
- **Encoding:** 4, 1, 6 (`PR_WATCH` 60 s).
- **States:**
  - NoPr.
  - Looking.
  - Shown(details).
  - Shown+Stale(why).
  - LookupFailed(why).
- **Transitions:**
  - `SeenPr::read` `diff/session.rs:611-639`.
  - `PrView::set_lookup`/`set_details` `diff/pr.rs:399-426`.
  - `comment_on_github` `diff/state.rs:645, 1245, 1251`.
- **Risk: low (verified).**
  - In `Ok(None)`, `self.record.take().is_some() || self.lookup_failed.take().is_some()` (`:615`) short-circuits. After Ok(Some) → Err(X) → Ok(None), `lookup_failed` keeps X, so a later identical Err(X) is suppressed (`:621`) and the view shows neither a PR nor an error while lookups fail.
  - `PrView.loading` (`pr.rs:384`) is written but never read.
- **Recommendation:** take both fields without short-circuiting, delete `PrView.loading`, and model `SeenPr` as `Unknown | None | Failed(why) | Found{record, details}`.

### 19. Jobs and reminders lifecycle read from presence in two files

- **Entity:** a job (Running → Finished) and a reminder (Pending → Fired / Canceled), mirrored from `awareness.json` plus `jobs/history.json` or `reminders/history.json` (`jobs.rs:7-12, 66-75`; `reminders.rs:7-14, 77-86`).
- **Encoding:** 3, 4, 6 (3 s re-read, attach retry 30×100 ms at `jobs.rs:420-450`).
- **States:**
  - Running = in awareness; Finished = in history.
  - Representable in one read: both at once, or neither.
- **Writers (cross-crate):** `shelbi-orchestrator/src/awareness.rs:219-241` removes, then records history; `cancel_job` 935-950.
- **Risk: low.**
  - Torn reads: `read_local` reads awareness first, then history, and the writer removes before it records. A job finishing between the two reads shows twice for one tick; `job_rows` (`jobs.rs:541-561`) doesn't de-duplicate. The opposite window makes it vanish for a tick.
  - The reminder sheet's cancel is "confirmed" when the next read lacks the reminder (`SheetState::follow`, `reminders.rs:358-370`). A *failed* read yields no rows, so the sheet closes as if the cancel succeeded.
- **Recommendation: document.** De-duplicate rows by id (prefer Finished). Have `follow` ignore non-`Rows` models while canceling.

## Minor

- **`SupervisionState`** (`supervision.rs:82-199`): explicit, with one `decide`. `ever_alive` doubles as "adopted" and as the StandDown-reported latch (175, 162-164). Leave. Shared with the daemon poller.
- **`ClosedHistory`** (`board.rs:826-952`): a `Depth` enum plus `fetching` and a `run` generation (stale answers dropped at 936). Leave.
- **`ConnectFlow`** (`launch/connect.rs:67-295`): `stopped` isn't cleared by `connect()`. After a DaemonStopped on session A, a failed connect to B leaves "Start Shelbi" showing, and `started(Ok)` connects A, not the selected B (250-252, 286). Low UX misroute.
- **`CheckStop` twin** (`settings/machine_check.rs:452-521`): the explicit `Check` enum has a hand-synced cancel flag that clients must trip with `stop_unless` after edits (`desktop/src/views/machine_form.rs:359`, `shelbi-tui/src/shell/settings/workspaces.rs:629,717`). A forgotten call lets a dropped check record a verified CLI.
- **`WorkspaceForm`/`RemoveConfirm`** (`settings/workspace_form.rs:304, 832-849, 865, 950-978`): `finished` routes the result to the confirmation whenever one is open, whichever job produced it. Desktop gates input, so this is likely unreachable.
- **Submit lifecycle as booleans**: `NewTaskForm` `creating`/`error` (`new_task.rs:199-200, 572-590`) and `FeedbackForm` `sending`/`sent`/`error` (`feedback/form.rs:110-112, 313-328`). `finished()` accepts an answer with nothing in flight, and the text setters aren't blocked mid-send.
- **`WorkspacePanelState.active`** (`workspace_panel.rs:219, 245-260`): `sync` never resets `active` when its switch row disappears, so Diff/Editor can persist on an idle workspace (desktop `views/workspace.rs:979-989`).
- **`nav::ClientState.overlay`** is write-only: desktop dual-writes it (`desktop/src/app.rs:1577, 6008-6022`), and the TUI keeps its own `ActiveOverlay`. **`GlobalFlags`** (`nav.rs:303-308`) is exported and unused; the real `zen_intro_seen` is on disk (`zen.rs:34-36, 64`). Delete or adopt.
- **`ReviewRow`** (`view.rs:126-145`) allows Pending with a workspace, Serving without a location, and so on. The orchestrator's writers are consistent (`shelbi-orchestrator/src/sidebar_model.rs:337-360`), and `task_session.rs:72-75` filters defensively.
- **Events.log status vocabulary** (`activity.rs`): `SystemEvent.status: Option<String>` matched against literals at render time (2339-2490, 2637-2669, Zen reasons 1817-1871). Unknown tokens fall to alarming defaults: merge or detach not `"ok"` renders red (2378, 2389), and CI not `"success"` renders "failed" (2466). A past bug is documented at 1818-1828 (the writer folds spaces to underscores). Shared token enums belong in shelbi-state `event_log`.
- **`DiffState` VISUAL anchor** (`diff/state.rs:1180-1185`): a row index reset after a rebuild only at 613, 636 and 671. `reflow` (1825-1830: split toggle, hunk filter, folds) leaves it pointing at other rows, so `y`/`c` act on rows the user didn't select. Clamped, so no panic. Reset VISUAL in `rebuild()`.
- **`DiffState` key flags** `pending`, `count`, `confirm_clear`, `flash` (`diff/state.rs:153-168, 805-860`): well tested. Leave.
- **`AgentsState` UI modes** (`settings/agents.rs:279-283`): public `draft: Option<Option<String>>`, `confirm` and `editing_shared`, with exclusivity rules duplicated per client (TUI `edit_shared` clears `confirm`; desktop `agents.rs:283` doesn't). There is one `error` slot, versus per-target errors in Workflows.
- **`BuilderState`** (`settings/workflow_builder.rs:83-156, 379-404`): explicit `Selection`, but clients write public `back`, `focus_transition`, `panning`, `hover`, `hot` directly. Transitions are identified by index, so removing an earlier one likely re-points the selection (suspected).
- **`KeybindingsState` capture** `capture`/`conflict`/`leader` (`settings/keybindings.rs:176-177, 339-390`): small and explicit enough.
- **`MachineForm`** (`settings/machine_form.rs:333-349`): `set` is allowed while saving, so edits made during a save are lost when success closes the form.
- **`overlay/palette_search.rs:597-677`**: `pending()` is `hits.query != asked`. If the worker fails to spawn, it stays pending forever; a corpus refresh shows no pending state.
- **`overlay/palette.rs:118-123, 333-338, 444-446`**: Projects-column focus is representable while searching hides that column.
- **`overlay/review_reject.rs:82-88`**: the late-fill guard is "text is empty", so a reason the user cleared on purpose can be refilled.
- **Undo coalescing** (`markdown_edit/history.rs:9-85`): a 1.5 s timer ended by `seal()`, which nine call sites must remember. `Editor.pending` marks aren't cleared by `replace_from_outside` or block commands.
- **`ChangelogWatch::observe`** (`changelog.rs:668-679`): marks a stamp seen on detection, not when the read lands, so a failed read isn't retried until the next change. Desktop uses its own stamp policy (`desktop/src/views/changelog.rs:189-194`).
- **`CommandKind::from_id` placeholders** (`command.rs:950-971, 273-281`): `RejectReview{reason: ""}`, `SwitchProject{project: ""}` and an all-None `EditIssue` are call-order protocols the host must fill in.
- **Refresh/subscriber workers**: `refresh.rs:44-51, 135-141` (a detached thread on `stop_without_waiting`); `changes.rs:151-218` and its near-copy `layout_sub.rs:94-195` (the reconnect loop's `sleep_slice` doesn't re-check stop mid-sleep).
- **`source/remote.rs`**: `OpenStream` closed/handle pair (439-465), `line_loop` publishes `connected` for `changes` only (541-577), a querier pool that retries once (173-196). `source/wire.rs:403-432` `Querier` request/early/final protocol poisons a stream on mismatch.
- **`exec_daemon.rs:91-93, 389-391`**: the out-of-date version gate guards mutations only, not `review_session_over`/`workspace_session_over`/`write_state_over`/`far_request`.
- **`session/remote.rs:293-315`, `source.rs:453-472`**: deadline-bounded calls leave a thread parked that may still apply the mutation after `NoAnswer` (documented).
- **Stringly classifiers**: `launch/ssh.rs:728-780` `classify_ssh_failure` (order-dependent stderr matching); `wire.rs:68-71` decodes a missing `WorkspaceState` as `AwaitingInput` (fail-open, unlike `fail_closed`/`cold_board`).
- **On-disk read-modify-write without a lock**: `launch/recent.rs:86-124` (last writer wins; tmp+rename prevents torn files).
- **`view.rs` mirrors**: `SidebarModel` flags (50-95; `daemon_version_mismatch` without a line is representable); `MachineWorkspace` shows three independent mirrors of one workspace (1180-1206, precedence 1408-1427); `WorkspaceHeader::needs_details` compares task ids (332-366).
- **`settings` misc**: `Job::stale_for` doesn't retry a failed load on revisit (`settings.rs:526-528`); `WorkflowsState` Sync now is a host call-order step (`workflows.rs:339-359`); `read_keys` runs keymap migrations as a side effect of a read (`keybindings.rs:552-555`).
- **`launch/route.rs:94-101`**: `daemon_state` folds "starting" into Running (intentional). **`launch/terminal.rs:111-153`**: canonical `ReconnectController` driver with no give-up.
- **`licenses::Browser`** selection can be hidden by the query and is re-snapped on `set_query`/`step` (`licenses.rs:214-291`). **`TextArea.wrap`** (`text_area.rs:130, 341`) is a trivial mode.
- **`board.rs`**: `ColumnExpansion` tri-state never returns to Auto via `toggled` (475-481); `WorkspaceFilter` uses the `"__unassigned__"` sentinel on disk (508).

## Cross-slice leads

- **Pane machine per client** (#6): `desktop/src/views/session_view.rs:299-307, 703-930`, `crates/shelbi-tui/src/shell/session.rs` (`Slot`, GaveUp latch 389-391), deliberate-close toggles at `desktop/src/app.rs:5764-5789`. `SupervisionState` is shared with `crates/shelbi-orchestrator/src/poller.rs` (many `request_relaunch` sites). Relaunch markers: `session/relaunch.rs:71-83` → `StateWrite::RequestRelaunch` → `shelbi_state::supervision_relaunch` (on-disk markers consumed per supervision pass). Liveness inputs: `crates/shelbi-client/src/discovery.rs:406-431` and `control.rs:476-500`.
- **Remote host link** (#5): `desktop/src/app.rs:1351-1376, 1946-1970, 2040-2100` rebuilds the window bar from three sources. `shelbi-client` `RelayChannel::is_reachable` feeds `RemoteLink::is_reachable`. Compare the runtime slice's ControlMaster/reverse-forward machine.
- **Diff session** (#1, #2, #14): TUI `crates/shelbi-tui/src/shell/diff_view.rs:194-211` (`open_failed` dead-session heuristic); desktop `desktop/src/views/diff_view/mod.rs:529-532` (Try Again → refresh) and `overlays.rs:138-144`. Reject-with-comments marking threads sent: TUI `shell/mod.rs:389` area, desktop `app.rs:6539` area. The far daemon's `Request::Diff` answer is the remote half of the save pipeline.
- **Settings save slots** (#3, #12): desktop `views/agents.rs:136-150` vs TUI `shell/settings/agents.rs:282-290` (divergent load guards); TUI `shell/settings.rs:1015-1037, 1364-1400, 2057-2088` (settings-level `LeaveGuard` composition and `Job` tickets); editor job-success transitions in `desktop/src/views/workflow_editor.rs:745-806`, `desktop/src/views/agent_editor.rs:536-590`, TUI `shell/settings/workflow_editor.rs:1247-1411`, `agent_editor.rs:890-1057`. Conflict wording lives in `crates/shelbi-state/src/workflow_config.rs:172-179`, `agents_config.rs:171-177`; the hash check in `shelbi_state::config_write`.
- **Issue edits** (#4): `desktop/src/views/board_state.rs` (a parallel `Pending{acked, landed}`/`held` machine around 1771-1781, 1878-1883, 1933-1991) and `desktop/src/views/issues.rs:877, 898` (sole `WRITE_DEADLINE` enforcement).
- **Zen** (#8): `crates/shelbi-state/src/lib.rs:2880-2893`, `crates/shelbi-proto/src/control.rs:392` (`StateWrite::ToggleZen`, no target), daemon `crates/shelbi-cli/src/commands/daemon/control.rs:1619, 2209`. Check whether the TUI sidebar's `zen_on()` (`crates/shelbi-tui/src/shell/sidebar.rs:317`) collapses Paused the same way.
- **New project / tracker** (#9, #10): `desktop/src/views/add_project.rs:501-514, 536-583, 628-745` (concurrent dispatch, creation generation); `desktop/src/views/tracker_section.rs:124-135`; TUI `crates/shelbi-tui/src/overlay/add_project.rs:232-288` (synchronous host) and `launch/mod.rs:277-320` (`Attempt::New` only); `crates/shelbi-orchestrator/src/project_create/mod.rs:306-346, 1011-1027`; `shelbi_orchestrator::tracker_connect::apply_tracker_connection`.
- **Board** (#11): TUI `crates/shelbi-tui/src/kanban.rs:859, 913, 1957`; desktop `views/issues.rs:658, 1795, 2284`, `views/board_state.rs:498-499, 1160-1188`; `crates/shelbi-state/src/github_store.rs:719-729` (`search_issues` Unsupported).
- **Review** (#13): TUI `crates/shelbi-tui/src/shell/mod.rs:1453, 2585, 4897-5013` (own open flow), `5219-5355` (decision sync); desktop `app.rs:4736-4790, 6560-6590`. `ReviewState` derivation is duplicated in `crates/shelbi-orchestrator/src/sidebar_model.rs:333-360` and `machines_model.rs:90-103`, both from the `review_slot_is_serving` on-disk marker.
- **Activity cache** (#7) owners: `crates/shelbi-tui/src/shell/mod.rs:1286-1302`, `crates/shelbi-orchestrator/src/source.rs:51-58`. **Events.log grammar** is parsed in at least three places (`shelbi-app/src/activity.rs`, `shelbi-orchestrator/src/wake.rs`, `shelbi-state` `EventKind::from_body`), and the writers are in `shelbi-state/src/event_log.rs`. A shared token type would cover all of them.
- **Onboarding** (#15): `crates/shelbi-state/src/orchestrator_setup.rs:87-107` (four-signal onboarded predicate, read failure means onboarded); `desktop/src/views/onboarding.rs:102-107`, `crates/shelbi-tui/src/shell/onboarding.rs:195-258`.
- **Jobs/reminders** (#19): `crates/shelbi-orchestrator/src/awareness.rs:219-241, 935-960`; `crates/shelbi-state/src/awareness.rs:888`.
- **Overlay/route state in the desktop**: `desktop/src/app.rs` `Route` (947), `Boot` (939), `overlay` (1577) layered over `ClientState`. The TUI's `ActiveOverlay` is independent.
- **IME** (#17): `desktop/src/views/doc_editor/mod.rs:195-198, 227-229, 948`.
- **Machine readiness**: `CheckStop` sync in `desktop/src/views/machine_form.rs:163-169, 331, 359` and TUI `shell/settings/workspaces.rs:115, 629, 717`, `machine_form.rs:70-87, 159-166`; server side `shelbi_orchestrator::machine_readiness::service`, far `far.rs:300-331`.
