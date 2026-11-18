# Implicit state machines: orch-workspace slice

**Slice:** `crates/shelbi-orchestrator/src`, everything outside the orch-core set. That is 43 Rust files (about 52.8k lines), plus `tests/remote_sessions.rs`, `tests/remote_source.rs` and `tests/detector_snapshot_parity.rs`.

**Coverage:**
- **Production code read in full** (above each `#[cfg(test)]`): `workspace.rs` 1-7777, `workspace_session.rs`, all `session_*.rs`, `remote_session.rs`, `far.rs`, `codex_rpc.rs`, `quit.rs`, `lifecycle.rs`, `load.rs`, `lib.rs`, `dispatch.rs`, `cancel.rs`, `review_session.rs`, `review_ui.rs`, `branch.rs`, `state_write.rs`, `migration.rs`, `tracker_connect.rs`, `machine.rs`, `machine_readiness.rs`, `machines_model.rs`, `project_create/*`, `mutate.rs`, `mutate/*`, `pool_admin.rs`, `sidebar_model.rs`, `source.rs`, `tools.rs`.
- **Read in part:** `git.rs` (stateless PR-listing helpers grep-checked only), `golden.rs`, `zoneinfo.rs` (pure parser, no state).
- **Tests:** in-file tests and `tests/` were scanned for pinned states and past-bug references.

**Findings:** 27 in total: **6 high**, **10 medium** and **11 low**, plus a Minor list.

Every high finding was re-verified against the code before writing. Line numbers are as of `e8145b3f`.

The common thread: the workspace slot (session, worktree, `.shelbi/` markers, card fields) has no single owner or state record. Each high finding is one face of that.

---

## HIGH

### H1. Dispatch launch lifecycle (persist → start/resume → `deploy_and_spawn` → delivery verdict)

- **Entity:** one launch of one card onto one workspace slot.
- **Encoding:**
  - 5: step order, enforced only by code position.
  - 6: `READY_TIMEOUT`, `launch_timeout()`, launch epoch.
  - 2 + 3: the outcome exists only as `dispatch … status=` strings appended to `events.log`.
  - 4: session liveness.
  - 8: `LaunchWait` and `LiveDelivery` are local enums, but the phase itself is never a value.
- **States:**
  - recheck ok
  - card persisted (status moved, then `assigned_to`/`branch` set as a **second** write)
  - registered with cancel generation
  - waiting on `lock_workspace`
  - cancelled
  - `sync-failed`, `branch-mismatch`, `pr-body-claim-failed`, runner missing, `settings-merge-required`
  - old pane killed
  - spawned
  - epoch stamped
  - delivery `unverified` / `confirmed` / `stuck` / `stalled{readiness_timeout|session_unreachable|delivery_failed|no_busy_signal_after_retry}`
  - completed / completed-late / rolled back
  - Representable but invalid:
    - card active and assigned with no pane (crash after persist)
    - card moved but fields not set (crash between the two writes)
    - live, idle, unprompted pane on an active card (kill + spawn ok, delivery failed)
- **Transitions:**
  - `mutate/start.rs`: recheck `:172`, persist `:178-193` (move, then `set_fields`), launch thread `:204-221`, await/rollback `:225-298`, `cancel::bump_workspace` `:281`, `rollback_start` `:402-424`, late-confirm by scraping `events.log` `:584-643`.
  - `workspace.rs`:
    - `start_workspace_on_task` 2624-2869 (cancel check `:2670`, `:2850`; lock `:2665`)
    - `resume_workspace_on_task` 2913-3040
    - `deploy_and_spawn` 3093-3454 (kill `:3205`, spawn `:3249`/`:3272`, `let _ = stamp_launch_epoch` `:3287`)
    - `finish_live_delivery` 3462-3516
    - `deliver_when_live` 3550-3600
  - Review-slot variant: `load.rs:dispatch_task_onto` 878-940 (set_fields `:886`, launch `:901`, rollback `:915-928`).
- **Risk: high.**
  1. **Nothing durable records "launching since T".** A process death between `start.rs:178` and the launch thread finishing leaves an active, assigned card with no agent. This is the deliberate F7 trade-off (`start.rs:174-176`), and recovery is left to supervisors in other slices.
  2. **The launch epoch never reaches the reaper.** It is stamped best-effort after spawn (`workspace.rs:3287`), and on the current backend the stamp goes nowhere at all (see H3). The reaper's launch-age grace (`poller.rs:6960-6975`, "the recurring bug") therefore never protects a launching slot.
  3. **`launch_timeout()` is read from each process's own env** (`workspace.rs:2227-2235`). The CLI await (`start.rs:200`) and the daemon reaper (`poller.rs:6972`) can disagree.
  4. **Cancellation is checked only before the irreversible step.** A superseded launch still kills, spawns and delivers for up to `READY_TIMEOUT`.
  5. **The worktree is mutated while the old agent is still alive.** `wire_settings_local` `:3128`, `deploy_runner_hooks` `:3145`, the beacon reset `:3162`, the context mount `:3172` and `deploy_startup_prompt` `:3196` all run before the kill at `:3205`.
  6. **Resume never verifies the branch.** Start checks it (`:2772-2787`); resume does not. `reattach_detached_resume_head` leaves an attached HEAD on *any* branch alone (`:7213-7217`, `:7237-7239`).
  7. **`classify_handoff` falls back to Developer on a store or workflow read error** (`:6060-6068`). That contradicts "never mislabel a gate as a developer" (`:6085-6088`).
  8. **The launch can use a stale workspace config.** It holds a `WorkspaceSpec` cloned before `lock_workspace` (`start.rs:51`, `:206-221`). A pool remove or relocate that lands in that window (`pool_admin.rs:173`, `:383`) can launch onto an undeclared workspace or the old host.
  9. **The review-load rollback can strand a parked card.** It restores only `assigned_to`/`branch`, but `clear_parked` already ran (`load.rs:899`), so a failed load of a parked task un-parks it. A rollback failure is only `eprintln!`'d in the daemon (`load.rs:923-927`).
- **Past bugs cited:**
  - phantom in_progress after a dropped prompt (`workspace.rs:3394-3400`)
  - seed false-confirm (`:3311-3315`)
  - resume scrollback false-busy (`:3424-3431`)
  - `reopen-dev-resume-races-launch` (`:3408`)
  - `rt-daemon-cancellation` (`:2652`)
  - #1494 / the 2026-10-04 supplant incident (`mutate.rs:514-527`)
- **Recommendation: make explicit.**
  - Add a persisted `LaunchRecord { phase, since, task, generation }`, with one transition function that also emits the event. It replaces both the `events.log` scraping and the epoch metadata.
  - Re-read the workspace config under `lock_workspace`.
  - Kill first, then mutate the worktree.
  - Verify the branch on resume.

### H2. Worktree handoff marker protocol (ready / transition / deferral / refusal sidecars)

- **Entity:** one workspace worktree, coupled to its card (`column` + `assigned_to`).
- **Encoding:**
  - 3: files under `<wt>/.shelbi/`.
  - 2: string bodies and `<task>:<reason>` / `<stage>:<class>` tokens.
  - 5: clear-before-launch, by call order only.
  - 8: legacy `.claude/` copies double the state space.
- **Files:**
  - `shelbi-ready` (`workspace.rs:199`) and legacy `.claude/shelbi-ready` (`:215`)
  - `shelbi-transition` (`:949`) and legacy (`:960`)
  - `shelbi-ready-deferred` (`:342`)
  - `shelbi-transition-deferred` (`:357`)
  - `shelbi-ready-refused` (`:381`)
- **States per worktree:**
  - none, ready(task), transition(task, target)
  - Representable but invalid:
    - ready + transition together (no precedence encoded in this slice)
    - new + legacy copies
    - torn or invalid body: `Err`, marker left in place (`:234-257`, `:1004-1036`)
    - any of the above combined with stale deferred or refused sidecars
- **Writers:**
  - The agent writes the ready and transition markers via tmp + `mv`, using commands embedded in the prompt (`:6183`, `:6219`, `:6287-6292`).
  - Sidecars: `write_deferred_marker` `:475-505`.
  - Dispatch clears: start `:2686-2711`; resume `:2952-2969`.
  - The poller consumes and clears: `poller.rs:2999-3049`, `:3678-3709`, `:3974-4056`.
- **Risk: high.**
  1. **Markers are cleared before the old agent is killed.** The clears run at `:2686`; the kill is at `:3205`, after worktree sync, settings writes and so on. A still-live old agent can write a marker that survives into the new launch. The poller's guard (`column == in_progress && assigned_to == ws`, `poller.rs:3051-3053`) does not stop a **same-task** re-dispatch or resume from being handed off by the previous run's marker.
  2. **Start and resume clear different sets.** Resume skips review-loaded (start clears it at `:2708`). Neither clears `ready-deferred`, `ready-refused` or `push-blocked`. Because the refused token is keyed `<task>:<reason>` (`poller.rs:4029-4034`), a task returning to the same worktree with the same refusal reason has its log line and worker nudge silently suppressed.
  3. **Sidecar writes are not atomic.** Local writes use `fs::write` (`:481-485`), remote ones use `printf >` (`:493-497`). The review-loaded marker, by contrast, does tmp + rename (`:867-891`). A torn read changes the dedupe decision.
  4. **The legacy fallbacks are permanent extra states.** Both say "remove in a later release" (`:213`, `:958`), and their docs name the wrong path (`:207-208`, `:956`).
- **Past bugs cited:**
  - clearing on a load error stranded a task in progress forever (`:335-341`)
  - torn writes (`:243-249`)
  - pane-title / Stop-hook races replaced by the file marker (`:185-188`)
- **Recommendation: make explicit.**
  - One `HandoffLedger` type per worktree owning every path, with a documented ready/transition precedence.
  - One `reset_for_launch()`, shared by start and resume, that runs **after** the kill.
  - Atomic writes for every sidecar.

### H3. Workspace slot occupancy and the dead session-metadata store (user shell, launch epoch)

- **Entity:** the slot session of one workspace: agent, user shell, orphan, dead or unreachable.
- **Encoding:**
  - 3/4: session metadata `@shelbi-user-shell` (`workspace.rs:2157`) and `@shelbi-launch-epoch` (`:2188`), carried over from the tmux era.
  - 1: `SlotProbe::Alive { user_shell }` (`:2242-2256`) restates `Liveness` plus a flag.
  - 6: launch age.
- **States:**
  - empty
  - agent (fresh, within the launch grace)
  - agent (aged)
  - user shell
  - orphan agent
  - dead
  - unreachable
  - On the current backend, "user shell" and "fresh agent" **cannot be represented**: they are always read as aged agent.
- **Transitions and readers:**
  - `open_user_shell` `:2105-2149`: probe, then spawn, then a separate best-effort `mark_user_shell` at `:2145`. Its caller is `shelbi-cli/src/commands/open.rs:44`.
  - `stamp_launch_epoch` `:3287`.
  - `kill_workspace_pane` `:2426-2465`.
  - The backend's `set_metadata` is a no-op, and `get_metadata`/`get_env` always return `Ok(None)` (`session_process_backend.rs:377-408`). `backend()` always returns the session-process backend (`session_backend.rs:429-431`).
  - Readers:
    - `workspace_user_shell_open` `:2172`
    - `workspace_launch_age` `:2204`
    - `probe_workspace_slot` `:2278`
    - `mutate/start.rs:98-109` (dispatch guard)
    - `poller.rs:6773`, `:6971-6990` (review teardown, orphan reaper)
    - `pool_admin.rs:984`
    - `shelbi-cli/src/commands/workspace.rs:384`
- **Risk: high.** Verified: the readers act on state that no longer exists.
  1. **The reaper can kill launching or resumed agents.** `workspace_launch_age` always returns `None`, so the reaper's launch-age grace never fires (`poller.rs:6971-6975`). That leaves the grace's own documented bug open: a slot whose dispatch is rolling back, or which races a stale remote board, reads as an orphan.
  2. **A `shelbi open` shell is treated as an agent.** It is put in the **agent slot** but its mark is never stored, so it probes as `Alive { user_shell: false }`. The orphan reaper then treats it as an agent pane (`poller.rs:6989`). The dispatch guard (`start.rs:102`) never sees it, so `deploy_and_spawn` kills it with `KillReason::Redispatch` (`workspace.rs:3205`). The newer `<p>/ws/<ws>/shell` session (`workspace_session.rs:265`) is a second, separate user-shell model.
  3. **Kill reasons are lost.** `kill_workspace_pane` records the kill reason after its probe (`:2442-2444`), so a crash in that window reads as deliberate. On a remote slot nothing is recorded, and every death reads as a crash.
  4. **The module docs are stale.** They say tmux is the default and that metadata "moves in Phases 3-4" (`session_process_backend.rs:4-9`, `:32-39`).
- **Recommendation: redesign (small).**
  - Derive launch age from `meta.json` `launched_at`, which already exists (`session_process_backend.rs:530-536`).
  - Retire `open_user_shell` in favor of the dedicated shell session, or carry the role in the spawn spec.
  - Delete the no-op metadata methods so nothing can write state that nobody can read.

### H4. Session name resolution and liveness (local vs remote, zombie, exiting, duplicates)

- **Entity:** a logical session name (`<p>/ws/<ws>`, `<p>/orch`, `w-<ws>/ws/agent`, `<p>/ws/<ws>/<role>`), which maps to one or more session directories.
- **Encoding:**
  - 3: `meta.json`, the lifetime lock, `sock`, `exiting`, `exit.json`, `kill-reason`.
  - 4: process, socket and relay listing.
  - 1: `alive` plus probe results.
  - 8: `Liveness` exists, but every reader builds its answer differently.
- **States:**
  - Starting
  - Live
  - Zombie (lock held, socket refusing or wedged)
  - Exiting
  - Exited (record written)
  - Vanished (SIGKILL, no record)
  - Several directories under one name
  - On a remote, all of these collapse to `alive` = lock held (`shelbi-client/src/relay.rs:1066`).
- **Readers that disagree:**
  - `probe` uses `find_any` (`session_process_backend.rs:170-176`, `:259-262`). It can pick an **exiting** session, so Alive is reported while `find_live`/`choose_session` (`:161`) says none. `send_text`, `history` and `title` then fail or read empty (`:270-272`, `:316-318`, `:365-367`).
  - `enumerate_slots` filters on `usable` only (`:428-441`).
  - `live_session_names` uses alive + choose + de-dup (`:636-644`).
  - `name_wedged` uses "any candidate not listening" (`:596-600`).
  - Remote readers use `s.alive` and the first entry in `read_dir` order (`remote_session.rs:373`, `:466`, `:580`, `:601`, `:669`, `:715`).
- **Risk: high.**
  1. **A remote zombie probes Alive forever, so it is never relaunched.** This is the local bug `rt-review-session-alive-but-not-listening` (`session_process_backend.rs:140`), still unfixed on remotes. `reap_zombie_duplicates` returns nothing for SSH (`session_backend.rs:497`).
  2. **A remote zombie next to its replacement can be the one attached to.** This repeats `rt-re-entering-a-review-fails-to-attach`, which was fixed locally (`:155-160`).
  3. **Killing a remote zombie reports success.** Remote `signal`, `title` and `resize` treat a `ClientError::Relay` as "already gone" (`remote_session.rs:529`, `:551`, `:565`). The relay sends `OpenError` exactly for a refusing zombie (`relay.rs:954-962`), so Quit counts the zombie as stopped.
  4. **Duplicates under one name survive Quit Shelbi.** Quit kills one session per name (`quit.rs:367-371`, `signal_by_name` `:511`).
- **Recommendation: make explicit.** Add one `SessionState` classifier and one `resolve(name) -> { chosen, duplicates, state }` in `shelbi-client`, used locally and sent over the relay's `ListSessions`. Make probe, enumerate, live_names, find_live and kill all use it.

### H5. Review-slot occupancy, load and serving lifecycle

- **Entity:** a (review-tagged workspace, task) pair. The pane, editor/diff content sessions, dev server and markers all hang off it.
- **Encoding:** 1, 2, 3, 4, 5, 6, 8.
- **States** (derived, never named in one place):
  - Free
  - Claimed: fields written, no pane yet (`load.rs:883-893`)
  - Checking out: git HEAD compared with the branch tip, plus liveness (`review_session.rs:366-405`)
  - Serving: pane alive
  - Ready for Review: the `shelbi-review-loaded` marker names the task (`workspace.rs:272`, `:655-684`)
  - Stranded: assigned, pane dead
  - Evicted (`load.rs:284-345`)
  - Parked
  - Auto-load given up (L1)
  - Representable but invalid:
    - two tasks assigned to one slot (assignment is a per-task field, with no per-slot record)
    - a marker naming task A while the slot is assigned to B
    - a content session left over from the previous task (handled at `review_session.rs:288-299`)
    - a Cold board read as all-idle (`shelbi-state/src/occupancy.rs:101-104`)
    - a marker still saying "serving" after the server died
- **Where the state lives:** about eight stores:
  - card `assigned_to`/`branch`
  - the assignment overlay (`load.rs:53-58`)
  - the lagging `board-index.json` (`load.rs:38-52`)
  - the review-loaded marker
  - pane and content sessions tagged with `task` metadata (`review_session.rs:322`)
  - the dev-server pgid record (`workspace.rs:2331-2401`; `shelbi-state` `workspace_status.rs:221-267`)
  - parked markers (`load.rs:540`, `:899`)
  - the failure ledger (`load.rs:546`)
  - The cross-process `lock_review_load` flock (`load.rs:204`, `:227`, `:265`, `:486`) guards them.
- **Writers in this slice:**
  - `load.rs:dispatch_task_onto` `:878-940`
  - `evict_review_slot_locked` `:314`, `:329-335`
  - `review_ui.rs:close_review_window` `:490-532`
  - `review_session.rs`: `close_review` `:505-531`, `clear_marker_naming` `:537-555`, `ensure_content_session_with` `:253-327`, `kill_content_sessions` `:475-486`
  - `workspace.rs`: marker write `:846`, clear `:898`, start clear `:2708`
- **Writers elsewhere:**
  - `poller.rs` `:388`/`:5453` (auto-load), `:5888` (resume), `:6573` (marker write), `:6744-6759` (clear on unassign), `:837` (teardown)
  - `shelbi-cli/src/commands/daemon/control.rs:912`
  - `shelbi-tui/src/app.rs:1007`
- **Risk: high.**
  1. **The manual-load busy guard sees a Cold board as idle.** `review_slot_busy_with_other` (`load.rs:454-463`) reads `BoardOccupancy`, which treats Cold as all-idle. Auto-load requires Warm (`load.rs:496-515`), but the manual path does not, so it can load over a live slot (`start_workspace_on_task` kills the old pane).
  2. **The marker is sticky.** A marker hit short-circuits (`workspace.rs:670-672`), and the poller clears it only on unassign, so a server that dies later keeps showing **Ready for Review**.
  3. **The health-check lookup fails open.** `review_workflow_has_health_check` returns false on any load error (`workspace.rs:599-608`), so a review that does declare `ready:` is promoted on liveness alone. That contradicts the conservative rule at `:649-651`.
  4. **The failed-load rollback un-parks the card** (see H1.9).
  5. **The review status id is a hard-coded string.** Despite the "nothing branches on the name review" doc (`load.rs:7-9`), there are literal `Column::review()` checks at `load.rs:300`, `:509`, `review_session.rs:88`, `:221`, `review_ui.rs:628`, `sidebar_model.rs:82` and `mutate.rs:444-450`. The slot role is matched with `contains("review")` in about 10 places.
  6. **Remote markers are never cleared on close** (`review_session.rs:542-544`).
  7. **A pgid liveness check can't tell a recycled pgid from the server.** `kill(-pgid,0)` (`workspace.rs:2382-2385`) succeeds for any process group with that id, so `stop_review_server` can SIGTERM/SIGKILL an unrelated group after the server exits.
- **Past bugs cited:**
  - index lag re-dispatching a served slot every tick (`load.rs:38-52`, `:498-505`)
  - the resume pass killing a mid-load pane (`:213-220`)
  - kill/respawn churn (`:398-408`)
  - the diff showing the previous task's checkout (`review_session.rs:233-236`)
  - auto-load failures missing from events.log (`load.rs:571-577`)
- **Recommendation: redesign.**
  - One per-slot record owned by the daemon (`SlotState::{Free, Claimed{task,since}, Serving{task}, Stranded{task}, …}`) with one transition function. Markers, overlay entries and content-session ownership are derived from it; the parked flag and the failure ledger become fields on it.
  - Replace the review literals with status-category checks.

### H6. Remote machine binary record (`machines.json`)

- **Entity:** one record per machine name in the hub-wide `$SHELBI_HOME/machines.json`: `{path, version, compatible, source, checked_at, host}` (`shelbi-state/src/machine_state.rs:69-111`).
- **Encoding:**
  - 3: on-disk record.
  - 4: mirrors the remote binary.
  - 1: `compatible` frozen at write time; `host: Option`.
  - 6: `checked_at` is never used to expire anything.
- **States:**
  - absent / compatible+host / incompatible / legacy (no host)
  - Representable but invalid:
    - `compatible: true` for a version the current hub rejects
    - a record bound to a host the machine no longer uses
    - a record under a renamed machine's old name
    - a record overwritten by another project's machine with the same name (names are project-local, the file is hub-global)
- **Writers:**
  - `machine_readiness.rs:547-552` (`save_verified_record`), `:757-797` (form service)
  - `shelbi-cli/src/commands/machine.rs:96`, `:253`
  - Only tests delete records.
- **Readers:**
  - `remote_session.rs:398-412` (`resolve_remote_bin`, the dispatch gate; used from `workspace_session.rs:195`, `:344` and `workspace.rs:3261`)
  - `remote_session.rs:423-440` (`relay_bin_for_host`)
  - `machines_model.rs:35`
  - `shelbi-app/src/view.rs:1336-1341`, `:1474`
- **Risk: high** (verified).
  1. **The dispatch gate ignores the host binding.** `resolve_remote_bin` matches on `rec.compatible` alone (`remote_session.rs:400`), while the view and `relay_bin_for_host` filter with `is_for_host`. A machine relocated by `update_machine_with` (`pool_admin.rs:526-624`, which never touches the record) dispatches with the old host's path and verdict while the view says "not set up".
  2. **A renamed machine fails dispatch**, and the old record lingers forever.
  3. **`compatible` is never re-evaluated against `hub_version()`** after a hub upgrade.
  4. **Machine names collide across projects.**
  5. **The read-modify-write is unlocked** (`machine_state.rs:98-111`). The write itself is atomic, but two concurrent writers (the form thread and a CLI `machine status`) lose one update, contrary to the doc. An unparseable file reads as empty, which de-verifies every machine.
  6. **An install that finishes after its form is closed records nothing.** The cancel flag skips the record (`shelbi-app/src/settings/machine_check.rs:415-421`, `:505-509`), although the install itself is not cancellable (`machine_readiness.rs:535`).
- **Recommendation: redesign.**
  - Key records by `(project, machine)` or by host.
  - Apply `is_for_host` in `resolve_remote_bin` and recompute compatibility live.
  - Migrate or clear the record in `update_machine_with`.
  - Lock the read-modify-write.

---

## MEDIUM

### M1. Worktree git state (attached / detached / dirty / mid-rebase)

- **Entity:** `<work_dir>/.shelbi/wt/<ws>` and the branch it holds.
- **Encoding:** 4 (git), 5.
- **States:**
  - absent
  - dir without a valid `.git` (healed at `workspace.rs:6744-6788`)
  - attached to the task branch
  - attached to another branch
  - detached (idle `:7327`, review `:6939`, post-handoff `:7538`)
  - dirty
  - mid-rebase: **not modelled**
- **Writers:**
  - `workspace.rs`:
    - `sync_worktree` 6620-6740
    - `sync_review_worktree` 6939-6996
    - `sync_worktree_for_resume` 7112-7226
    - `ensure_workspace_worktree` 7296
    - `materialize_idle_worktree` 7327-7389
    - `remove_workspace_worktree` 7392-7424
    - `release_branch_from_workspace_worktrees` 7427-7491
    - `detach_workspace_worktree(_if_handoff_safe)` 7538-7697
    - `rebase_workspace_branch_onto_default` 1222-1430
  - Elsewhere: `poller.rs:4194`, `:4736`; `transition_move.rs:135`; `zen.rs:4521`, `:7935`; `pool_admin.rs`.
- **Risk: medium.**
  1. **Inconsistent lock discipline.** Dispatch, resume, materialize, ensure and remove take `lock_git_worktrees`. The poller's handoff rebase, push and detach, and `transition_move`'s detach, take no lock (verified: zero uses in `poller.rs` and `transition_move.rs`). A poller detach can land between a dispatch's checkout and its launch, after `drop(git_worktree_lock)` at `:2788`.
  2. **Another workspace's worktree can be detached under its live agent.** `release_branch_from_workspace_worktrees` detaches other workspaces' worktrees while holding only the target workspace's lock, so that agent's commits land on a detached HEAD (same class as `bug-resume-leaves-detached-worktree`, `:7205-7210`).
  3. **A crashed rebase is invisible.** A kill between the rebase and `--abort` (`:1316-1395`), or a failed abort reported as `Skipped`, leaves `.git/rebase-merge` behind, and nothing in the slice checks for it.
  4. **The heal logic is copied three times** (`:6744`, `:7135-7170`, `:7343-7370`).
- **Recommendation:**
  - Document a state table.
  - One lock rule for every worktree mutator, the poller included.
  - Detect an in-progress rebase or merge in the sync paths.
  - Keep a single heal helper.

### M2. Kill-reason marker protocol (deliberate death vs crash)

- **Entity:** a session directory's `kill-reason` file, plus `exit.json`/`exiting`, read back as `SlotDeath`.
- **Encoding:** 3, 5 (record, kill, then clear on failure), 8.
- **States:** `SlotDeath::{Exiting, Killed(Stop|Bounce|Redispatch|Handoff|Reap|Quit), CleanExit, Crash}` (`session_backend.rs:233-245`). Invalid but representable: a reason recorded on a session whose kill never landed.
- **Writers:**
  - Record: `session_process_backend.rs:544-554`. It targets `find_live`, else the newest *dead* directory.
  - Clear: `:558-565`.
  - Callers: `workspace.rs:2444`/`:2449`; `quit.rs:634` (record, **no** clear when the kill fails at `:638-640`).
- **Readers:** `slot_death` `:568-586`; `poller.rs:5093-5128`, `:5849-5853`, `:6342-6346`.
- **Risk: medium.**
  1. **Record and kill each resolve the session separately** (`:548` vs `:511`). A replacement spawned in between gets the reason while the old session is the one killed.
  2. **A failed quit kill leaves `quit` on a live session.** If that session later crashes, `deliberate()` reads true (`session_backend.rs:250`) and supervision stands down instead of restarting it.
  3. **Unknown tokens parse as `Stop`** (`:219-228`), which keeps the slot down.
- **Recommendation: make explicit.** One `kill_with_reason(host, name, reason)` that resolves once, records, kills that same short id, and clears on failure.

### M3. Orchestrator session lifecycle and handoff/greeting claims

- **Entity:** a project's orchestrator.
- **Encoding:** 3, 4, 5, 6.
- **States:**
  - project open flag (`lib.rs:563`)
  - session alive / dead / unreachable (Unreachable collapses to alive, `:306-309`)
  - handoff present → claimed → committed or restored (`:825-852`)
  - greeting armed → claimed → re-armed (`:1357-1383`)
  - supervision gave-up (`:615`)
- **Transitions:**
  - `ensure_dashboard` `:492-617`
  - `relaunch_orchestrator_session` `:626-660`
  - `supervise_restart_orchestrator` `:364-434`
  - `replace_orchestrator_session` `:676-688`
  - `restart_orchestrator` `:697-703` (from `state_write.rs:39`)
  - Elsewhere: `poller.rs` `maybe_supervise_orchestrator` (~`:5331`), `shelbi-cli` `reload_session.rs:74`, `:121`, `handoff.rs`.
- **Risk: medium.**
  1. **A reload can be counted as a crash.** `replace_orchestrator_session` kills and then polls for up to 5 s without the dashboard lock, so the supervisor can see Dead and relaunch, counting a deliberate reload as a crash restart.
  2. **A kill wait that times out attaches to a dying session** and returns Ok.
  3. **A failed launch spends the greeting.** The greeting is re-armed only on a spawn error; a runner that exits at once still consumes it.
  4. **The open flag is set before the spawn** (`:563` vs `:606`), so a failed cold launch leaves an "open" project with no orchestrator.
- **Past bugs cited:** F11 orphaned orchestrator pane (`:531-536`); `orch-relaunch-noop-after-session-death` (`:342`).
- **Recommendation: make explicit.**
  - Hold the dashboard lock across replace's kill and wait.
  - Give the greeting the same ready-check commit as the handoff.

### M4. Handoff push retry and provenance ledger

- **Entity:** one workspace's handoff push.
- **Encoding:**
  - 3: `shelbi-push-blocked` holds tip, attempts and blocked (`workspace.rs:405-409`); `shelbi-pushed-tip` holds one `<branch> <sha>` line per branch (`:439-443`).
  - 6: retry cap.
  - 8: `PushOutcome` (`:1438-1545`) is a verdict; the ledger lifecycle lives in the poller.
- **States:** up-to-date, pushed (ff or forced), no-remote, failed-transient (counted and capped), blocked (terminal, quiet re-attempts), and "a new tip resets the ledger".
- **Writers:** push `:1681-1822` (force decision `:1756-1789`); `poller.rs:4254-4350`, `:4557-4690`.
- **Risk: medium.**
  1. **A crash between a successful push and recording the tip loses provenance.** Without the recorded tip (push at `:1801`, record at `poller.rs:4558+`), the worker's next amend reads as foreign and returns terminal `Blocked`, unless the `events.log` fallback finds the push.
  2. **Both sidecars are written non-atomically** (`write_deferred_marker`), and a torn pushed-tip body loses provenance in the same way.
- **Past bugs cited:** an hour-long `stale info` loop (`:423`, `:1662`); the `dt-figma-qa-activity` single-slot provenance loss (`:432-436`).
- **Recommendation:** tmp + rename for both sidecars, and document the ledger's states next to `PushOutcome`.

### M5. Workspace occupancy and badge derivation (one question, six predicates)

- **Entity:** whether a slot is busy, and what it shows.
- **Encoding:** 4 (board, overlay, `status.yaml`, liveness), 2 (`Column::review()` literals), 8 (`SlotOccupancy` exists in `shelbi-state/src/occupancy.rs:26-42`, but each caller picks its own predicate).
- **Derivations:**
  - dispatch `is_busy` (`mutate.rs:613-632`)
  - supplant release (`mutate.rs:536-605`)
  - pool remove `is_claimed`, which ignores PreAssigned (`pool_admin.rs:391-404`, `:946-957`)
  - pool rename `check_idle` (`:806-853`)
  - sidebar badge (`sidebar_model.rs:247-271`)
  - machines view (`shelbi-app/src/view.rs:1400-1495`)
- **Risk: medium.**
  1. **The sidebar and the machines view disagree.** The sidebar says "Idle wins over a stale `status.yaml`"; the machines view shows the raw state.
  2. **A dead agent shows Working forever.** An assigned slot with no status reads as Working (`sidebar_model.rs:268-270`), and there is no dead or stuck badge.
  3. **Custom handoff statuses fall out of the sidebar and get the wrong unassign rule.** The sidebar's review sections filter on the literal id `review` (`:82`), and unassign parks only on literal `review` (`mutate.rs:444-450`).
  4. **Removing a workspace can strand PreAssigned cards** on an undeclared workspace (`start.rs:64-75` then fails).
- **Recommendation: make explicit.** One occupancy + display function in `shelbi-state`/`shelbi-app`, used by every guard and view.

### M6. New-project scaffold lifecycle (create / retry / pick up)

- **Entity:** a project being created: registration YAML, in-repo `project.yaml`, `tasks/welcome.md`, agents, workflows, commit guard, Linear caches.
- **Encoding:** 5 (stage order Config → Agents → Workflows → guard → Linear, `project_create/mod.rs:946-960`), 3 (progress = which files exist), 8 (`ScaffoldStage`/`Attempt` exist, but partial progress is never recorded).
- **States:**
  - nothing
  - welcome seeded but unregistered
  - registered with Config incomplete
  - registered without agents
  - registered without workflows
  - complete without the guard
  - Linear filters or statuses unresolved (warnings only, `:375-394`)
  - The middle states are discoverable, half-built projects.
- **Writers:**
  - `create_project` `:329-372`
  - `scaffold_config` `:1033-1124` (publish `:777-825`)
  - `pick_up` (`pick_up.rs:42-111`)
  - Retry recognition `:501-598`
- **Risk: medium.**
  1. **Retry exists only in the desktop wizard's memory.** Only the desktop wizard passes `Attempt::Retry` (`shelbi-app/src/add_project_wizard.rs:1085`, `:1108`). The CLI and TUI always pass `New`, and they hit "a project already lives at `<slug>`" (`:521-522`).
  2. **A pick-up retry registers a second alias.** Pick-up ignores `attempt` (`:334-341`) and always takes `next_available_alias` (`pick_up.rs:82`), so a retry after a mid-stage failure registers `team-2` and leaves the first half-built.
  3. **Suspected, untested: Retry may be refused for GitHub/Linear projects.** The welcome `.md` is written before registration, and `ensure_board_not_stranded` (`:350`; `issues.rs:289-311`) counts it.
  4. **The scaffold lock starts later than its comment says.** The comment claims it covers the collision check (`:947`), but it is taken at `:1054`, after the check at `:520`.
- **Recommendation: make explicit.**
  - Persist a creation record (stage, mode, tracker) beside the registration so any front end can resume.
  - Give pick-up a retry that recognizes its own registration.

### M7. Workspace and machine pool edit lifecycle (`pool_admin`)

- **Entity:** a workspace's config entry, its worktree, and its hub-side state.
- **Encoding:** 5 (provision → write config → retire old), 3, 4.
- **States:**
  - `WorktreeState` Missing / Clean / AtRisk / Unknown (`pool_admin.rs:100-110`) and `SessionState` (`:92-97`)
  - Implicit intermediates:
    - worktree exists, undeclared (`:179-195`)
    - worktree at the new name, config still old (adopted on retry, `:268-283`)
    - old worktree left behind after a relocate (`:342-364`, warning only)
    - worktree removed but still declared (`:416-439`)
- **Writers:**
  - `add_workspace_with` `:163-196`
  - `update_workspace_with` `:204-367`
  - `remove_workspace_with` `:374-446`
  - `update_machine_with` `:526-624`
  - `carry_migration_state` `:856-864`
- **Risk: medium.** Mostly well handled: locks, an `expected` conflict check, retry-adopt, and tests at `pool_admin/tests.rs:771-851`. What remains:
  - a renamed workspace's `status.yaml` is left stale (`:52-53`)
  - machine records are not migrated (H6)
  - PreAssigned cards are not checked on remove (M5)
  - the pre-lock dispatch window (H1.8)
- **Recommendation: document** the intermediate states and that retry is the recovery path. Fix the H6 and M5 items.

### M8. Per-host relay cache (`remote_session.rs`)

- **Entity:** the cached `Arc<RelayHandle>` per SSH host, which owns the `ssh … relay` child (`:198-211`).
- **Encoding:** 1/4 (`reachable`/`closed` atomics in `shelbi-client/src/relay.rs:187-189`), 6 (keepalive), process-global map (`:267-279`).
- **States:** Absent / Cached+reachable / Cached+unreachable / Opening (outside the lock, `:311-312`).
- **Writers:**
  - `get_relay` `:301-323`
  - `drop_relay` `:335-340`
  - the `with_channel` retry `:349-365`
  - `reset_relays` `:262-279`
- **Risk: medium.**
  1. **`drop_relay` doesn't check which handle failed.** It removes whatever is cached (`:359`), so N threads hitting one dead relay (per-workspace polls, quit warm and sweep at `quit.rs:280-288`, `:400-405`) drop each other's fresh relays and churn SSH.
  2. **Dropping the cache entry kills attached views.** Streams hold only the channel's `Arc<Inner>`, not the handle (`:612-621`), so the SSH child dies under every attached view on that host.
- **Recommendation: make explicit (small).** Compare-and-remove via `Arc::ptr_eq`, and have streams hold the handle.

### M9. Content and user-shell session "ensure" (check-then-spawn, no claim)

- **Entity:** the `<p>/ws/<ws>/{editor,diff,shell}` sessions and the far editor `<p>/edit/<key>`.
- **Encoding:** 4, 5, 6 (`spawn_session_to_attach` ignores its 5 s wait timeout, `session_process_backend.rs:614-621`).
- **States:** Absent / Spawning (invisible to the check) / Live / Exiting.
- **Writers:**
  - `workspace_session.rs`: `ensure_content_session` `:159-207`, `ensure_user_shell` `:317-348`, `close_content` `:240-248`, `close_user_shell` `:358-371`
  - `far.rs:90-108`
  - Called from `shelbi-cli/.../daemon/control.rs:957-966`, which deliberately skips the per-issue lock (`:945`) and runs each client on its own thread (`:532`).
- **Risk: medium.**
  1. **Two concurrent opens spawn two editors on one worktree.**
  2. **A failed remote listing reads as "not live" and spawns anyway** (`.unwrap_or(false)`, `:162`, `:320`).
  3. **Duplicates are never reaped and survive Close and Quit** (H4.4).
- **Recommendation: make explicit.** A per-name keyed mutex in the daemon, and close kills every candidate with the name.

### M10. Review accept/reject decision protocol

- **Entity:** a review-column task being decided.
- **Encoding:** 4, 5, 3.
- **States:** in review → merged but not moved → moved but not cleaned (branch delete and the edge's `run:` pending) → cleaned → slot torn down.
- **Writers:**
  - `review_ui.rs`: `approve_review_task` (merge `:399`, move `:410`, cleanup `:424`), `AcceptCleanup::run` `:454-470`, `close_review_window` `:490-532`, `reject_review` `:540-569`
  - The cleanup is spawned on a detached thread at `mutate.rs:478-486`.
- **Risk: medium.**
  1. **A crash between merge and move leaves a merged PR on a review card.** Recovery relies on zen's GitHub-merge reconcile (`git.rs:938-949`).
  2. **Accept cleanup has no durable record.** It is fire-and-forget, so if the daemon exits the branch is never deleted and `run:` never fires.
  3. **A failed pane kill aborts the teardown.** `close_review_window` `?`-fails at `:520` after the marker was cleared, so `status.yaml` (`:531`) is left stale.
  4. **Reject silently falls back to a literal `todo`** (`:553-556`).
- **Recommendation: document, then make explicit.** Persist a pending-cleanup record, or make the cleanup re-derivable by reconcile. Make teardown continue past a failed kill.

---

## LOW

### L1. Review auto-load failure ledger (backoff and give-up)

- **Where:** `ReviewLoadFailure { attempts, last_attempt, gave_up }` per (task, slot). Read at `load.rs:546`, suppressed at `:664-681`, folded at `:691-706`, written at `:604-624`, cleared on success at `:939`. The store is in `shelbi_state`.
- **Risk:** `gave_up` never ages out (`:665-667`), so a pair that gave up on a transient cause stays stuck until a human loads it. The give-up event omits the task id (`:619-624`).
- **Recommendation: make explicit** as `{ Backoff { n, since }, GaveUp { since } }`.

### L2. Workspace tmux-cutover migration state

- **Where:** `MigrationState::{ Pending, Migrated }`, where absence acts as a third state that reads dispatchable (`migration.rs:610-615`).
- **Risk:** the "safe direction" comment is wrong (`:598-599`). A failed `let _` write of Pending (`:459`, `:600`) leaves a never-recorded workspace dispatchable. Local probe errors are treated asymmetrically (`:159-160` vs `:554-555`). This only matters during the one-time cutover.
- **Recommendation: document** and fix the comment.

### L3. Tracker connection switch

- **Where:** steps run validate → empty-board check → preflight → write config → Linear filter cache → status sync (`tracker_connect.rs:189-260`). Callers: `shelbi-cli` `tracker.rs:308`, `:333` and `far.rs:81`.
- **Risk:**
  - No lock spans check to write.
  - A crash after `:245` leaves stale caches and an unsynced catalog (cache writes are `let _`).
  - The config write itself self-heals (`shelbi-state` `tracker_config.rs:51-57`).
- **Recommendation: document.** Consider a project config lock.

### L4. Quit composition phases

- **Where:** steps 1-5 enforced by order (`quit.rs:19-38`), `QuitInFlight` RAII (`:88-116`), `cancel::QuitPhase` (`cancel.rs:236-361`), and deadlines (`:263-275`).
- **Risk:** well tested (`:781-1239`). A daemon crash mid-quit leaves `open=false` with live sessions, and nothing finishes the sweep. `set_project_open` errors are dropped.
- **Recommendation: document.**

### L5. Codex JSON-RPC connection

- **Where:** `closed`/`desynchronized` bools (`codex_rpc.rs:66-67`). The handshake is enforced in the constructor (`:73-117`).
- **Risk:** only a timeout poisons the connection. An id mismatch (`:185-188`), malformed JSON (`:180`), a read error (`:177`) or a post-id send failure (`:133-143`) leave the stream misaligned but "usable". `wake.rs:1272-1279` (`inject_zen_policy`) keeps using it after an error.
- **Recommendation:** `enum Conn { Ready, Poisoned, Closed }`, poisoning on every non-Remote error.

### L6. Branch cut for Todo → InProgress

- **Where:** `lifecycle.rs:155-349`. Cut the ref, then `set_fields`, then the caller flips the column (`transition_move.rs:96`, `mutate/start.rs:141`). Base resolution is a decision table (`:259-290`).
- **Risk:** idempotent. "Branch exists" alone counts as cut, so a stale same-named branch is reused.
- **Recommendation: leave**, and document the "existing ref is trusted" rule.

### L7. Zombie-reap hysteresis

- **Where:** `session_process_backend.rs:680-719`. The streak map is per poll thread (`poller.rs:587`).
- **Risk:** the session dir is removed after SIGTERM whether or not the process exits (`:702-707`), so a SIGTERM-ignoring session runs on invisibly.
- **Recommendation: leave**, or remove the dir only after the lock is released.

### L8. Per-task message channel (log / cursor / beacon / acks)

- **Where:** `<wt>/.shelbi/messages/<task>.{log,cursor,session}`. The hooks are embedded in `workspace.rs:4653-4794`; `reset_message_session_beacon` is at `:3763-3781`.
- **Risk:** at-least-once delivery by design. The beacon reset at `:3162` runs before the old pane is killed (`:3205`).
- **Recommendation: document.**

### L9. Mutation expected-state / staleness protocol

- **Where:** `ExpectedState { status, updated_at }` with a `"<deleted>"` sentinel (`mutate.rs:173`). Rechecks are by convention (`mutate.rs:342-497`, `start.rs:172`, `add_edit.rs:156`). Rule at `mutate.rs:198-222`.
- **Risk:**
  - On remote trackers a same-status concurrent change is invisible.
  - The edit `Stale` payload reports `expected: actual` (`add_edit.rs:174`).
  - `add` checks id uniqueness with a local `exists()` even on remote backends (`:32`).
- **Recommendation: document**, and fix the payload.

### L10. Zen toggle and relaunch markers over the control socket

- **Where:** `StateWrite::ToggleZen` is a relative flip (`state_write.rs:32` → `shelbi-state` `lib.rs:2880-2893`), so two clients both trying to turn Zen on cancel each other out. `request_relaunch` writes three keys with a 60 s freshness window (`state_write.rs:56-64`).
- **Recommendation:** an absolute `SetZen { target }` op.

### L11. Daemon job cancellation registry

- **Where:** per-project `live` map, `quits` count and per-job generation flag (`cancel.rs:101-379`).
- **Risk:** cooperative and process-local only (`:43-46`), so launches running in a separate CLI process are not reached.
- **Recommendation: leave** (RAII, 20 tests).

---

## Minor

- **`session_guard.rs:20-36`:** `SPAWN_BLOCKED` atomic. Its only writer is `SessionLostState` in `shelbi-cli` (already explicit).
- **`session_backend.rs:625-650`:** injection mutex registry; no lifecycle.
- **`RespawnOutcome` (`session_process_backend.rs:445-455`):** always `Failed`, a dead branch for callers.
- **`workspace.rs:3685-3735`:** usage-limit auto-resume. `LimitResumeOutcome` is explicit and re-checks eligibility before each mutation. Scheduling lives in `poller.rs:2795`.
- **`workspace.rs:4135-4262`:** `settings.local.json` five-case reconcile (explicit enum). Its write is non-atomic and runs before the old agent is killed.
- **`workspace.rs:5289-5360`:** `mounted-claude-skills` manifest. A crash between removing the old mounts and writing the manifest leaves orphan mounts.
- **`workspace.rs:5499-5549`:** `info/exclude` read-modify-write shared across linked worktrees, under different locks. Cosmetic line loss.
- **`workspace.rs:5125-5150`:** awareness plugin bundle is removed, then rewritten; a midway failure leaves a partial bundle.
- **Outcome enums** `RebaseOutcome`, `PushOutcome`, `HandoffReadiness`, `DetachOutcome`: data only, but their `status_token` strings are an `events.log` wire contract.
- **`branch.rs:113-131`:** a `GITHUB_LOGIN` OnceLock caches a failed `gh` lookup for the daemon's lifetime.
- **`git.rs:1103-1192`:** bounded merge-sha retry with a stringly `"MERGED"` compare.
- **`git.rs:1221-1329`:** PR-body file plus `pr-body-task` stamp; a well-ordered marker pair.
- **`lib.rs:751-768`, `:1112-1124`:** awareness mode is decided by whether plugin staging succeeded; a failure silently downgrades to v1.
- **`review_ui.rs:1-82`:** `ReviewMidView`/`ReviewOpenOutcome` are unreferenced, and the module doc still describes the removed tmux swap model.
- **`load.rs:994-1073`:** `dispatch_active_gate` takes no lock; its event is appended before the dispatch and there is none on failure.
- **`machine.rs:389-414`:** `SshExec::run_script_with_stdin` has no deadline, so a wedged host hangs a non-cancellable install.
- **Process-lifetime OnceLock caches:** `project_create/detect.rs:239-256` (installed runners).
- **Pure classifiers:** `dispatch.rs:73-162` (Owner×Zen), `session_connect.rs:144-169`, `machine_readiness::Readiness`.
- **Caches:** `source.rs:85-117`, `:233-239`.
- **`tools.rs:179-207`:** 1.5 s early-exit window.
- **`far.rs:64-68`:** the stop flag is always false.
- **`quit.rs:418`:** job sessions are exempted by a name predicate.
- **Not state machines:** `golden.rs`, `zoneinfo.rs`.

## Cross-slice leads

- **Poller handoff engine (orch-core).** This is the consumer side of H2, H3, H5, M1, M2 and M4:
  - `poller.rs:2995-3130` (ready)
  - `:3642-3709` (transition)
  - `:3940-4060` (refusal)
  - `:4194` (rebase), `:4254-4690` (push ledger), `:4736` (detach)
  - `:5093-5128`, `:5849`, `:6342` (`SlotDeath`)
  - `:6485-6760` (review slot)
  - `:6960-7000` (orphan reaper with the dead launch-age grace)
- **Session classification (runtime):** `shelbi-client/src/discovery.rs:159-242`, `:406-490` and the relay listing at `shelbi-client/src/relay.rs:954-962`, `:1056-1070`. H4's fix belongs there.
- **Occupancy and assignment overlay (state-core):** `shelbi-state/src/occupancy.rs:26-58`, `:101-104` (Cold = idle), `:257-278`. Also the parked markers, the review-load failure ledger, `workspace_status.rs:221-267` (review pgid), `machine_state.rs:69-111` (H6) and `lock_workspace`/`lock_git_worktrees` (`lib.rs:1042`, `:1103`).
- **Daemon control (cli-daemon):** `shelbi-cli/src/commands/daemon/control.rs:325-338`, `:532`, `:788-823`, `:912`, `:940-975` (content sessions, no lock). Also `serve.rs:514-518` (idle exit during quit), `reload_session.rs:74`, `:121`, `review_serve.rs:108` (pgid writer) and `open.rs:44` (the H3 user-shell path).
- **App:**
  - `shelbi-app/src/view.rs:1336-1495` (machines view and record filter)
  - `shelbi-app/src/settings/machine_check.rs:370-513` (record and cancel)
  - `shelbi-app/src/add_project_wizard.rs:1085`, `:1108` (only `Attempt::Retry` caller)
  - `session::ReconnectController` / `launch::link::HostLink` (remote reconnect)
- **TUI:** `shelbi-tui/src/app.rs:1007` (evicting load), `:1439-1548` (`review_slot_is_serving` readers).
- **Orchestrator supervision (orch-core):** `supervision.rs`, `poller.rs` `maybe_supervise_orchestrator`, `handoff.rs`; the `shelbi-state` `set_project_open` / `claim_*` helpers (M3).
- **Accept protocol (orch-core):** `transition.rs` `run_gated_merge`, and zen.rs's GitHub-merge reconcile (M10).
- **Codex wake actor (orch-core):** `wake.rs:574`, `:641`, `:776-798`, `:1246-1279` (L5).
