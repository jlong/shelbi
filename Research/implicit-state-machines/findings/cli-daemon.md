# Implicit state machines: cli-daemon slice

**Slice:** `crates/shelbi-cli` (all 59 `src/` files including `commands/` and `commands/daemon/`, plus all 29 integration tests), `crates/shelbi-client` (11 src files + 3 tests), `crates/shelbi-proto` (8 src files). All 110 files were read; files with nothing relevant are listed at the end of "Minor".
**Findings:** 30 total: **3 high, 16 medium, 11 low**, plus a Minor list. Surveyed 2026-11-18 against `e8145b3f`.
**Headline:** (1) the hub daemon's lifecycle is spread over two atomics and four disk files, and "is the daemon up?" is computed four different ways; (2) the Zen crash-recovery machine lost all its production writers when tmux was removed, but the poller, `status`, and the bootstrap prompts still rely on it; (3) `shelbi issue resume` moves board state outside the daemon's mutation engine, with its own partial rollback.

Paths are repo-relative. "Verified" means the synthesizer re-read the cited lines; every other cite comes from a full read of the file.

---

## High

### H1. Hub daemon process lifecycle (start, serve, drain, exit) and "is it running?"
- **Entity:** the one `shelbi daemon` process per root, and every client's view of it.
- **Encoding:** 1, 3, 5, 6, 8. In memory: `stop: AtomicBool` (`daemon/serve.rs:361`), a separate `control_close: AtomicBool` (`serve.rs:404`), and `job_deadline: Option<Instant>` plus the listener's nonblocking mode, which together stand for "draining" (`serve.rs:608`, `638-647`). On disk: the `hub.sock.lock` flock, `hub.sock`, `control.sock`, and `shelbi.pid` (pid + version).
- **States:**
  - Absent.
  - Starting: lock held, no socket yet. Slow pre-bind housekeeping runs here (login-env capture, ControlMaster prune, config-upgrade pass, unit retire; `serve.rs:298-326`).
  - Bound, pid not yet written (`serve.rs:343-357`).
  - Serving.
  - Draining: `stop`=true, and the hub socket still accepts and answers hellos normally (verified `serve.rs:609-647`, `989-997`). The control socket refuses with `ShuttingDown`. Lasts up to `DAEMON_JOB_DRAIN` (120s) + 3s.
  - Closing: sockets unlinked (`serve.rs:421-422`), pid removed (`426`), lock held until return.
  - Exited.
  - **Representable but invalid:**
    - Crashed: a stale `hub.sock` / `shelbi.pid` with no lock holder.
    - Serving with no pid file: a failed write at `serve.rs:357-359` is only logged (verified).
    - Draining is indistinguishable from Serving on the wire, because the hello carries no phase.
- **Transitions (writers):**
  - Lock: `serve.rs:298` (`acquire_bind_lock` `846-879`).
  - Stale socket reclaim: `serve.rs:888-907`.
  - Control unlink + bind: `daemon/control.rs:492-503`.
  - Pid: write `serve.rs:357`, remove `serve.rs:426`.
  - `stop`=true from four places:
    - signal: `serve.rs:926`
    - idle monitor: `serve.rs:543`
    - `QuitShelbi`: `control.rs:249` via `643`
    - session-lost recovery: `daemon/session_health.rs:563`
  - Drain entry: `serve.rs:638-647`. Drain exit: `serve.rs:628-631`, `652-662`.
  - `control_close`: `serve.rs:420`. Force exit on a second signal: `serve.rs:931-933`.
- **"Is the daemon running?" is answered four different ways:**
  - lock || hello probe: `daemon/lifecycle.rs:291-295`
  - lock only: `quit.rs:126`, `detach.rs:131`
  - `probe != NotRunning`: `lifecycle.rs:272`
  - socket first, then lock: `shelbi-state/src/daemon_lifecycle.rs:207-224` (`ensure_daemon_running_with`, verified)
- **Idle exit sub-machine:**
  - States: InGrace → Polling → Idle → Stopping, plus Deferred while a quit is in flight.
  - Grace timer: `serve.rs:492-497`, `534`. Idle check: `516-519`. Exit: `541-546`.
  - `QUITS_IN_FLIGHT` is an in-process counter in `shelbi-orchestrator/src/quit.rs:88,114`.
  - Open flags on disk come from `set_project_open`.
- **Risk: high.**
  - **A draining daemon is accepted as "up".** `ensure_daemon_running_with` returns Ok as soon as the socket answers (`daemon_lifecycle.rs:211`). A client that opens a project while a `QuitShelbi` drain is running, or just after the idle monitor fires (`serve.rs:541-546`; the check-then-stop isn't atomic with an opener's `set_project_open(true)`), is told the daemon is up. That daemon then exits with the project open, up to ~123s later. Recovery depends on a session watchdog (`shelbi-session/src/session.rs:478-480`) or the next client.
  - **A daemon with no pid record can't be stopped or restarted.** If the pid write fails (`serve.rs:357`), `stop_daemon` refuses, so `restart` bails (`lifecycle.rs:262-263`) and reload bails (`reload_session.rs:114-115`).
  - **The lock is held long before the socket binds.** Quit and detach therefore carry their own 10s connect-retry loops (`quit.rs:141-165`, `detach.rs:152-170`).
  - **"No daemon ⇒ nothing open" is assumed, never enforced.** `quit.rs:27-30`, `104-106` and `detach.rs:77-79` rely on it. A crashed daemon leaves open flags set and sessions alive (`tests/daemon_root_isolation.rs:283`), so `shelbi quit` prints "nothing is running" and closes nothing until a watchdog restarts the daemon.
  - **The exit reason is never recorded.** The one `stop` flag doesn't say whether it was a signal, idle, quit, or session loss; only stderr does.
  - **Past bugs:**
    - `worker-kill-took-down-hub` (`tests/daemon_root_isolation.rs:1-15`)
    - probes that took the bind lock made a starting daemon exit (`serve.rs:149-153`, `tests/daemon_shutdown_drain.rs:378`)
    - idle exit during a quit (`serve.rs:2308`, `idle_exit_is_deferred_while_a_quit_is_in_flight`)
    - a job admitted just before the flag flips must be seen by the drain (`control.rs:2303`)
- **Recommendation: make explicit.**
  - Add a `DaemonPhase` enum {Starting, Serving, Draining, Exiting(reason)} behind one atomic, owned by `serve`.
  - Carry the phase in the hub hello, so `ensure_daemon_running` waits for a successor instead of accepting a dying daemon.
  - Add one shared `daemon_state()` classifier in shelbi-state, used by status, quit, detach, stop and ensure.
  - Why: four ad hoc predicates over the same five signals already disagree in the startup and drain windows.

### H2. Orchestrator crash / Zen crash-recovery lifecycle: orphaned since tmux removal
- **Entity:** orchestrator liveness and crash state per project, and Zen's automatic disable after a crash.
- **Encoding:** 3, 5, 2, 6, 8.
  - The marker is `state.json` `zen_orchestrator_alive_at`.
  - The wrapper call order is start → heartbeat → exit / record-exit.
  - Exit reasons are strings (`exit:0`, `signal:SIGTERM`, ...).
  - The recovery window is 1h.
  - `zen_mode` is flipped outside `set_zen_mode`.
- **States:**
  - NotRunning (no marker)
  - Alive (recent marker)
  - CrashedRecent (marker < 1h)
  - CrashedStale (marker > 1h)
  - GracefullyExited (marker cleared)
  - **Representable but invalid:** a marker set with no orchestrator running and nothing left that will ever clear it.
- **Transitions (writers):**
  - `__zen-orch-start`: `zen_lifecycle.rs:62` calls `zen_check_crash_recovery` (`shelbi-state/src/lib.rs:2547-2563`). That function sets `zen_mode=Off` directly at `lib.rs:2558`, bypassing `set_zen_mode`, then `zen_lifecycle.rs:67` appends `zen=off reason=crash-recovery`.
  - Heartbeat: `zen_lifecycle.rs:90` calls `lib.rs:2521`.
  - Graceful clear: `zen_lifecycle.rs:105` calls `lib.rs:2533`.
  - Crash record: `zen_lifecycle.rs:137-168`.
  - The hidden commands are declared at `main.rs:391`, `420`.
- **Risk: high.**
  - **No production code calls any of these entry points any more** (verified by grep).
    - Outside `zen_lifecycle.rs`, every call is in a test.
    - `shelbi-orchestrator/src/lib.rs:1288-1292` says the heartbeat and traps "are not reproduced here" and are left to the daemon and session supervision, which never picked them up.
    - Result: Zen is never auto-disabled after an orchestrator crash.
  - **Dependents still assume it works:**
    - The poller's relaunch comment says Zen "stays off after the restart via its own `__zen-orch-start`" (`shelbi-orchestrator/src/poller.rs:376-377`, verified). It doesn't, so Zen auto-merge resumes after a crash.
    - The bootstrap prompts (`shelbi-orchestrator/src/lib.rs:938,968,990`) and the orchestrator template (`default_orchestrator.md.template:283`) tell the orchestrator to look for a `zen=off reason=crash-recovery` line that is never written.
    - `status` greps `events.log` for that line. It uses a 6h window over a 64KB tail (`status.rs:50,58,773-802`) and matches any line in the window, even one followed by a later `zen on` ("on (crash-recovery flagged)", `status.rs:741-748`). So it can only ever show stale tmux-era lines.
    - A leftover tmux-era `zen_orchestrator_alive_at` is still printed as "orchestrator alive" (`zen.rs:545-547`, `status.rs:757`) and can never be cleared.
  - **Revival alone wouldn't surface the event.** The line has the shape `project=… zen=off reason=…`, not `mode=zen a -> b`. `event_kind` (`orchestrator.rs:1152-1216`) classifies it as generic `"event"`, and the v2 `label()` (`orchestrator_awareness.rs:86-110`) hides that kind.
  - The CLAUDE.md migration-hint latch reset (`zen_lifecycle.rs:59-60`) is orphaned the same way.
- **Recommendation: redesign.**
  - Move orchestrator liveness and crash detection into the session supervisor (which already knows the exit status) as one `OrchestratorLifecycle` enum with a transition function.
  - Route the auto-disable through `set_zen_mode`, so it emits the canonical `mode=zen` transition.
  - Persist the off reason in `state.json` instead of grepping the log.
  - Delete or rewire the four hidden commands.
  - Why: today the contract is silently broken while three readers still trust it.

### H3. `shelbi issue resume`: a status transition done client-side, outside the mutation engine
- **Entity:** one issue's dispatch: column, `assigned_to`, `branch`, the tracker claim, the worker session, the `events.log` transition, and the Zen user-move record.
- **Encoding:** 5, 4, 8.
- **States:**
  - Not in-progress
  - In-progress with the old owner
  - In-progress with the new owner and branch
  - Session spawned
  - Event logged
  - Zen record written
  - **Representable but invalid:**
    - In-progress with a stale `assigned_to` (crash or failure between the move and `set_fields`)
    - Moved and running, with no transition event (crash after the spawn)
- **Transitions (writers, all verified):**
  - Tracker claim on `--take`: `issue.rs:1238`.
  - `store.move_status(in_progress)`, only when the card wasn't already in progress: `issue.rs:1269-1273`.
  - `set_fields(assigned_to, branch)`, which always runs: `issue.rs:1276`.
  - Spawn via `resume_workspace_on_task`: `issue.rs:1287`.
  - On failure, rollback only `if moved_into_progress` (`issue.rs:1303-1304`) through `rollback_start` (`issue.rs:1149`). That rollback calls `move_status` at `1156` and `set_fields` at `1160`.
  - `append_task_event`: `issue.rs:1322`. `record_zen_user_move`: `issue.rs:1332`.
- **Risk: high.**
  - **No optimistic-concurrency guard.** Every other board mutation goes through `mutate_client::run_mutation` with the expected `(status, revision)` (`mutate_client.rs:30-33`). `resume` writes the store directly, so it can race the poller or a daemon dispatch. `ensure_workspace_dispatchable` (`issue.rs:1232`) is check-then-act.
  - **A failed resume can leave the card on the wrong workspace.** On a card already in progress, `--workspace Y` overwrites `assigned_to` (`issue.rs:1276`). If the spawn then fails, there's no rollback, so the card stays assigned to the workspace whose launch failed.
  - **A `--take` claim is never undone on failure.**
  - **The rollback only resets the board.** The daemon's Start also tears the session down (`rollback_and_teardown`, `shelbi-orchestrator/src/mutate/start.rs:428-440`), and `rollback_start` is duplicated between `issue.rs:1149` and `mutate/start.rs:402`.
  - **The raw `move_status` skips transition actions.** It doesn't go through the Move mutation, so the workflow's transition actions (`shelbi-orchestrator/src/transition_move.rs`) never fire.
  - **Same pattern elsewhere in the CLI** (see Minor): `issue prio`, `depends` and `rm` also write the store directly.
- **Recommendation: make explicit.** Make resume a daemon `MutationKind` that reuses Start's guarded transition and teardown. Why: there should be one transition function and one rollback for "put this card in progress on that workspace".

---

## Medium

### M1. `shelbi reload` sequencing (handoff → signal clients → restart daemon → replace orchestrator)
- **Entity:** one reload run.
- **Encoding:** 5, 3 (`handoff.md` and its `.md.claimed` claim file; `tests/orchestrator_handoff.rs:124-142`).
- **States:** HandoffRequested → ClientsSignalled → DaemonRestarted → OrchReplaced. Partial failures leave no record: `ReloadReport`'s bools are always true (`reload_session.rs:75-80`).
- **Transitions:** `reload_session.rs:67,70,72,74`.
- **Risk: medium (one false-success path).**
  - **An `Err` from stop is dropped** (verified). `restart_daemon` bails only on `Ok(StopOutcome::Refused)` (`reload_session.rs:114`). An `Err` from `stop_daemon` (e.g. a pid-record read error) falls through to `ensure_daemon_running`. The old daemon still answers, so that call returns Ok, and reload prints "daemon restarted on the current binary" (`reload.rs:102`) while the old binary keeps serving. `lifecycle.rs:257` propagates the same error correctly.
  - Clients are told to re-exec (step 2) before the daemon restart (step 3) can fail.
  - A failed orchestrator replace leaves the handoff on disk for some later launch.
- **Recommendation: make explicit.** Record which step was reached in the report, and treat a stop `Err` the way `lifecycle.rs:257` does. Why: a real partial state is currently reported as success.

### M2. Event-delivery cursor: v1 claim/ack feed, v2 stream/hooks and `events drain` share one cursor
- **Entity:** the per-project durable `event-cursor`, plus:
  - the follower registry (a pid file)
  - the delivery lock
  - the in-memory `InflightBatch`
  - the stream's `yielded` flag
- **Encoding:** 3, 5, 6, 1.
  - Print-then-ack vs ack-then-print is decided per caller.
  - Timers: a 90s visibility timeout, quarantine after 10 redeliveries, a 1s supersede grace, a 5s lock wait.
  - `yielded` is a bool, plus a signal flag reused as state (`orchestrator_awareness.rs:326-355`).
- **States:**
  - Feed: Idle, InFlight{cursor, emitted, redeliveries}, Redelivering, Quarantined, Resynced, then one of Expired / Terminated / Superseded / Failed.
  - Stream: Owning, Yielded, Ended.
- **Transitions (cursor writers):**
  - v1 quarantine: `orchestrator.rs:452`
  - v1 resync: `orchestrator.rs:482`
  - v1 `events ack`: `orchestrator.rs:802`
  - v1 `events drain`: `orchestrator.rs:935`
  - v2 deliver: `orchestrator_awareness.rs:219,239`
- **Follower-registry writers:** claims at `orchestrator.rs:299` and `orchestrator_awareness.rs:271,352`; releases at `orchestrator.rs:304` and `orchestrator_awareness.rs:273`.
- **Risk: medium.**
  - **Hooks ack before they print** (verified). `deliver` writes the cursor (`orchestrator_awareness.rs:239`) while the hook closure only buffers the lines (`432-435`, `460-463`); they're printed later (`398`). If the hook process is killed (Claude Code's hook timeout) or stdout fails in between, the events are acked but never shown. The module doc says delivery "never drops" a batch (`:20`), and `420-421` concedes the batch is "already acked".
  - **v1 paths never take `acquire_event_delivery_lock`.** Only v2 does (`orchestrator_awareness.rs:191`). A v2 hook and a v1 follower can deliver the same events. `events drain` (`orchestrator.rs:929-936`) writes the end-of-log cursor unconditionally, silently acking everything the v2 stream hasn't shown yet; only the prompt text (`orchestrator_awareness.rs:512-516`) forbids it.
  - **A stream can SIGTERM a newer follower.** Its reclaim check (`:345`) is separate from `claim_event_follower` (`:352`), so a v1 follower claiming in between gets killed. `claim_event_follower` signals the registry pid with no identity check (`shelbi-state/src/event_log.rs:1492-1503`).
  - **Past bugs:**
    - the poison-cursor "prbadge wedge" (`orchestrator.rs:2885`)
    - leaked-follower starvation (`orchestrator.rs:2730`)
    - silent parking on a dropped ack (`orchestrator.rs:203-210`)
- **Recommendation: make explicit.**
  - One `DeliveryConsumer` enum with a single ack function that always runs under the delivery lock.
  - Hooks persist or print before acking.
  - Retire or lock the v1 `drain`/`ack` writers.

### M3. Command-job lifecycle spread over awareness.json, `jobs/<id>/result.json`, history.json and the runner pid
- **Entity:** a `shelbi job run` command job (and pid/ci/pr watches).
- **Encoding:** 3, 4, 6, 2.
  - The phase is inferred from:
    - the store entry
    - whether `result.json` exists
    - the history record
    - the job dir
    - runner pid liveness
  - Timers: `RUNNER_START_GRACE` (120s) and the `next_alert_at` overdue grid.
  - "running" / "overdue" are words assembled in the CLI.
- **States:**
  - Starting (in store, `runner_pid` None)
  - Running
  - Exited-unreported (result written, still in store)
  - Lost (runner dead, no result)
  - Reported (in history)
  - Canceled
  - Overdue is a derived sub-state (`job.rs:284,316,573`).
  - **Representable but invalid:** in neither the store nor history while the dir exists (the CLI's `Found::DirOnly`, `job.rs:354-359`).
- **Transitions:**
  - CLI register: `job.rs:119` (to `shelbi-orchestrator/src/awareness.rs:851`, rollback at `895`) and `job.rs:141`.
  - Runner pid / result: `shelbi-orchestrator/src/job_exec.rs:107,175`.
  - Estimate: `job.rs:156`.
  - Cancel: `job.rs:189`, which removes the entry (`awareness.rs:937`) and then records history (`awareness.rs:948`, verified).
  - Poller report: `awareness.rs:219-243`.
- **Risk: medium.**
  - **Cancel can lose the job.** It removes the entry before recording history, in two files; a crash or write error in between leaves the job in neither, and `job log` degrades to DirOnly.
  - **Poller reports can be lost.** The poller writes the store, then history (`awareness.rs:238`). Worker-routed reports go to a detached thread after the store write (`233`, `573`), so a daemon exit in between loses the report entirely: delivery is at-most-once.
  - **Recycled pids fool liveness.** The lost-runner check (`awareness.rs:404-405`) and `job log -f`'s `job_ended` (`job.rs:453`) use raw `pid_alive`, not cancel's identity check `is_job_runner`. A recycled pid keeps a dead job "running" and keeps firing overdue lines.
  - **`job log -f` can loop forever.** It snapshots the job once (`job.rs:380,426`). If the job is still in Starting (`runner_pid` None) and no daemon is running, a runner that dies before writing a result is followed forever.
  - **Cancel can mislabel a finished job.** If cancel races a runner that's finishing, history records Canceled even though a result lands right after (`awareness.rs:943-946`).
- **Recommendation: make explicit.** Put a `JobPhase` enum on `Job` with one transition function in `shelbi_state::awareness`. Cancel and report should record-then-remove under the same lock, and every liveness check should use the identity-checked runner probe.

### M4. Session lifecycle encoded as files in the session dir, with a weaker remote mirror
- **Entity:** one session, `~/.shelbi/sessions/<short-id>/`.
- **Encoding:** 3, 4, 8.
  - Files: `lock` (flock), `meta.json`, `sock`, `exiting`, `final.txt`, `exit.json`, `kill-reason`.
  - Reachability is probed live.
  - `DiscoveredSession.alive` is cached, while `exiting()` re-reads disk each call.
- **States:**
  - Creating (dir, no meta)
  - Starting (lock + meta, no sock; `NotBound`)
  - Serving (`Reachable`)
  - Zombie (`Refusing` / `Wedged`)
  - Exiting (`exiting` marker, still locked and accepting)
  - Flushed (`final.txt` + `exit.json`, lock still held)
  - Dead (lock released)
  - Reaped (dir removed)
  - `kill-reason` is an orthogonal overlay.
  - **Representable but invalid:**
    - `exiting` without `exit.json` after a SIGKILL during the flush grace
    - a `kill-reason` left behind after a failed kill
- **Transitions (writers):**
  - In shelbi-session (cross-slice), `shelbi-session/src/session.rs`:
    - mkdir `:451`
    - lock `:457`
    - `meta.json` `:472`, rewritten by set-meta `:256`
    - bind `:563-564`
    - `exiting` `:582`
    - `final.txt` `:600`
    - `exit.json` `:601`
    - sock removed `:602`
  - Client side: `record_kill_reason` / `clear_kill_reason` (`shelbi-client/src/discovery.rs:224-231`) and reap (`discovery.rs:372`).
  - State is re-derived by:
    - `is_live` (`:284-286`)
    - `exiting()` (`:207-209`)
    - `probe_socket` / `probe_reachable` (`:91-157`)
    - `usable` (`:194-196`)
    - `choose_session` (`:406-443`)
    - `zombies_to_reap` (`:460-488`)
    - `snapshot` (`snapshot.rs:51-63`)
  - It becomes an enum only in the consumer: `SlotDeath` (`shelbi-orchestrator/src/session_process_backend.rs:576-584`).
- **Remote mirror:** the relay's `discover_sessions` sends only `alive` (`shelbi-client/src/relay.rs:1056-1072`; `RelaySession` in `shelbi-proto/src/relay.rs:120-136`). It carries no `exiting`, `launched_at` or reachability. Hub-side pickers take the first `alive && name` match in `read_dir` order:
  - `shelbi-app/src/session/remote.rs:234,258,299,329`
  - `shelbi-orchestrator/src/remote_session.rs:373,669,715`
- **Risk: medium (medium-high for remote).**
  - **A live session can briefly vanish from discovery.** `meta.json` (`session.rs:256`) and `exit.json` (`session.rs:949`) are written in place with `fs::write`, and `list()` skips a dir whose meta doesn't parse (`discovery.rs:262-265`).
  - **A torn `exit.json` reads as a crash.** `exit_record()` returns None, and `slot_death` then reports `Crash`.
  - **Callers combine two moments in time.** `alive` comes from `list()` time while `exiting()` is read fresh (`session_process_backend.rs:576`).
  - **`zombies_to_reap` skips the `exiting` filter** that `choose_session` applies (`:465-467` vs `:409`).
  - **Remote pickers reintroduce bugs that `choose_session` fixed locally:**
    - attaching to a dying predecessor (`client-connect-skips-exited-sessions`)
    - attaching to an older duplicate (`rt-re-entering-a-review-fails-to-attach`)
  - **Past bugs in comments:**
    - `phantom-working-dead-session` (`discovery.rs:282`)
    - `rt-review-session-wedges-after-repeated-attaches` (`:66`, `:122`)
    - `rt-review-slot-relaunch-loop` (`:316`)
    - test `tests/protocol_e2e.rs:705`
- **Recommendation: make explicit.**
  - Add one `SessionPhase` derived in a single function in `discovery`, read once per call.
  - Write `meta.json` and `exit.json` atomically (temp + rename).
  - Have the relay run `choose_session`, or ship `exiting`/`launched_at`, so remote and local selection agree.

### M5. Hub-side relay stream drop-to-replay (`replay_pending`)
- **Entity:** one logical session stream over a `RelayChannel` (`Inbound`, `shelbi-client/src/relay.rs:118-128`).
- **Encoding:** 1, 5, 8.
- **States:**
  - Flowing
  - ReplayPending (drops everything until a `resync`)
  - Closed
  - **Representable but invalid:** `closed && replay_pending`.
  - **Missing:** "replay never came".
- **Transitions (verified):**
  - Flowing → ReplayPending when the queue hits `STREAM_QUEUE_LIMIT` (2048 frames): `relay.rs:632-635`. The hub then injects `Attach{since_seq:None}` (`:646-650`).
  - ReplayPending → Flowing only on a frame whose type byte is `Resync` (`:621-627`).
  - Every other frame is dropped while pending (`:629-630`).
  - → Closed: `StreamState::close` (`:159-163`), from teardown `:210-212`, the `Close` route `:740-744`, writer Drop `:522-524`, and shutdown `:435-437`.
- **Risk: medium.**
  - **The stream dies silently on a session that never answers with `Resync`.** The session sends `Resync` on attach only to clients that negotiated the capability (`shelbi-session/src/session.rs:268-273`, `transport.rs:157`). Production hub clients ask for `capability::ALL` (`shelbi-app/src/session/remote.rs:280,335`), so a current session recovers. A frozen-core or older far session never sends `Resync`, so the flag never clears.
  - **Requests hang while pending.** `SnapshotData`, `InfoData` and `Exited` are dropped along with output, and `Connection::request` has no timeout (`connect.rs:276-280`), so `snapshot()` / `info()` hang. The client sees a bare EOF instead of a death.
  - The doc comment `:124-127` disagrees with the code about when the flag clears.
  - The only test is `tests/relay_e2e.rs:420`, against a current session.
- **Recommendation: make explicit.** Use an enum with a deadline on ReplayPending (close the stream on expiry), and pass reply and `Exited` frames through. Why: a backpressure event shouldn't be able to turn into a permanent hang.

### M6. Relay channel and per-request lifecycle: liveness checked only at the channel layer
- **Entity:** one hub-side `RelayChannel` (`relay.rs:168-190`) and the far side's `links` map (`relay.rs:793`, `Link` enum at `relay/services.rs:190-209`).
- **Encoding:** 1, 5, 6.
  - `reachable` and `closed` AtomicBools; `peer_version: Option`.
  - Ordering: hello before `OpenService`, and an open waits for `Opened`.
  - A keepalive timer and `PEER_HELLO_DEADLINE`.
- **States:**
  - Hub side: Connecting, Open(v1 | v2), Closed.
  - Far link: Opening, Linked, Closed.
  - **Representable but invalid:**
    - `reachable=false, closed=false`. `reachable` is effectively redundant: every teardown sets both, and `is_reachable` checks both (`:288-290`).
    - A far link inserted after `shutdown_links` drained the map (`services.rs:495`).
- **Transitions:**
  - Init `:252-263`; hello `:266`; peer version `:715-716`.
  - Teardown `:208-209`, called from decode error `:692`, EOF `:700`, ping write failure `:766`, silence `:771`, and Drop `:406`.
  - Far link inserts: `relay.rs:980-983`, `services.rs:340,354,439,495,569`.
  - Far link removes: `relay.rs:923-934,1041-1045`.
  - Far link drain: `relay.rs:1048-1052`.
- **Risk: medium.**
  - **A wedged far session hangs connect forever.** The handshake bound is a no-op over a relay (`relay.rs:476-483`), yet `connect.rs:222-232` relies on it, and keepalive only measures channel silence. Behind a healthy relay, `Connection::connect` hangs forever: the `rt-review-screen-hangs-on-connecting` bug, now remote. `shelbi-app/src/session/remote.rs:279-285,301` has no outer bound.
  - **A hung second hop blocks the hub open.** `open_stream` waits on `rx.recv()` with no timeout (`relay.rs:385`) while the far side does an SSH hop on a worker thread (`services.rs:476-505`). Pings keep the channel green the whole time.
  - **`list_sessions` is unbounded** (`:297`).
  - **One stuck far socket can mark the whole machine unreachable.** The far main loop holds the `links` lock during a blocking `write_all` to a session or control socket (`relay.rs:918-920`, `services.rs:216`). That stalls Ping→Pong (`relay.rs:936-937`), and the hub then declares the machine unreachable.
- **Recommendation: make explicit.** Give open, list and hello their own deadlines, independent of keepalive, and move per-link writes off the far main loop. Why: today liveness is checked at one layer of a three-layer stack.

### M7. Message push and ack lifecycle (CLI push sequence + daemon ack timers)
- **Entity:** one pushed message (`msg_id`).
- **Encoding:** 3, 2, 4, 6.
  - The append-only `.shelbi/messages/<task>.log` and the `<task>.session` beacon.
  - `push=ok` / `ack=…` tokens parsed out of `events.log`.
  - pid+lstart liveness.
  - The daemon's in-memory `PendingMap` timers (`daemon/serve.rs:186`).
- **States:**
  - An enum `MessageDelivery` exists (`shelbi-state/src/event_log.rs:1777-1795`), but it is rebuilt from four booleans with hand-coded precedence (`:1806-1849`).
  - Values: Unknown, Queued, TimedOut (not terminal), Delivered, Undeliverable(TaskDone | NoSession).
  - **Representable but invalid:**
    - Logged with no `push=ok` (reads as Unknown).
    - Both `ack=timeout` and `ack=worker` in the log.
- **Transitions:**
  - CLI:
    - refuse → `ack=undeliverable`: `message.rs:180` (called at `226`, `274`)
    - beacon probe: `message.rs:261`
    - append: `message.rs:291`
    - `push=ok`: `message.rs:294`
    - question stamped answered: `message.rs:301`
    - daemon notify: `message.rs:323` (best effort, errors swallowed, `453-482`)
  - Daemon:
    - insert pending: `serve.rs:1313-1327`
    - ack: `serve.rs:1335-1343`
    - reaper: `serve.rs:695-754`
- **Risk: medium.**
  - **A crash between append and `push=ok`** (`message.rs:291-294`) leaves a message the worker will read while status says "unknown".
  - **A failed notify means no timer.** The message stays Queued forever and never reaches TimedOut.
  - **A fast ack can produce a false timeout.** The CLI appends the message before it notifies the daemon. If the worker's ack lands before the `message-pushed` notify, the timer is armed after the ack and fires a false `ack=timeout` 60s later.
  - **A late ack after a timeout still emits `ack=worker`** (`serve.rs:1342`).
  - **The deliverability check misses canceled and archived tasks.** It compares only `column == done()` (`message.rs:225`).
  - **TOCTOU** between the beacon probe (`:261`) and the append (`:291`).
  - **Pending timers are lost on daemon restart** by design (`serve.rs:86-91`).
  - **Past bug:** the "ghost keepalive ack" (`serve.rs:1377,1639,1802`); tests `tests/message_delivery.rs:170-245`.
- **Recommendation: document** the push sequence and its crash windows. Make the done check category-based. Keep ack-without-pending tombstones for one timeout window. The derived enum is acceptable as a read model.

### M8. Clarification question lifecycle (open → answered)
- **Entity:** a stored question `<project_dir|home>/questions/<task>/<id>.md`.
- **Encoding:** 3, 1 (`answered_at: Option` + `answer_msg_id: Option`).
- **States:**
  - Open, Answered.
  - **Representable but invalid:** `answered_at=None` with `answer_msg_id=Some`. It renders as open (`question.rs:162-166`); `is_answered()` reads only `answered_at` (`shelbi-state/src/questions.rs:83-85`).
  - Nothing ever reopens a question.
- **Transitions:**
  - `record_question` from the daemon (`daemon/serve.rs:1261`). It stores hub-level when the worker's `project=` isn't registered (`serve.rs:1253-1256`, verified).
  - `mark_question_answered` from `message.rs:301-305` (`questions.rs:259-275`, verified).
- **Risk: medium.**
  - **Stamped answered on queue, not on ack.** The comment at `message.rs:296-299` says so. If the reply times out or is never read, the question still drops off `question list`.
  - **Hub-level questions can never be answered** (verified). `question list` merges in hub-level questions (`question.rs:80-84`), but `message.rs:302` always passes `Some(&project_name)` and `mark_question_answered` has no hub-level fallback. Such a question stays open forever and the reply prints "no stored question".
  - **A re-ask can look already answered.** An identical re-ask returns the existing record, answered or not (`questions.rs:156`).
- **Recommendation: make explicit.** Add a `QuestionState` enum, mark answered on `ack=worker` (and reopen on timeout), and resolve the hub-level root when marking.

### M9. Config-upgrade finding lifecycle: no "kept" state, and blocked auto-heals reach no one
- **Entity:** one `UpgradeFinding` (id = hash of scope, surface, code, message and fix; `config_upgrade.rs:3364-3373`) across hub boots and reloads, plus `~/.shelbi/config-upgrade-findings.json`.
- **Encoding:** 3, 5, 8. State is re-derived from file contents on every pass, and the orchestrator follows a prose protocol. A `Classification` enum exists, but there is no lifecycle around it.
- **States:**
  - Detected-AutoHeal → applied
  - Detected-NeedsJudgment → surfaced → approved and applied via `--apply-finding`, or approved and refused (no mechanical fix)
  - Resolved (content no longer matches)
  - **Missing:** Declined / Kept-on-purpose.
  - **Unnamed:** AutoHeal-blocked.
  - **Indistinguishable:** a default deleted on purpose vs never present.
- **Transitions:**
  - detect: `config_upgrade.rs:195-229`
  - pass: `run_and_emit`, `config_upgrade.rs:282-308`, called from `daemon/serve.rs:307` (under the bind lock) and `reload.rs:91` (unlocked)
  - emit: `config_upgrade.rs:324-347` (verified; non-atomic `fs::write` at `:336`)
  - apply-finding: `config_upgrade_apply.rs:199-242` (promotes the finding to AutoHeal at `:223-224`; refuses at `:208-218`)
  - `config upgrade --apply` / `--apply-finding` (`config.rs:229-288`) never call `emit`
- **Risk: medium.**
  - **"Keep it" can't be recorded.** Several needs-judgment codes have "keep" as a valid answer:
    - `PROJECT_NAME_LOOKS_LIKE_ID` (`config_upgrade.rs:1284-1286`)
    - `DISK_CONFIG_MISSING` (`:1696`)
    - `ZEN_CHECKS_MSRV_MISSING` (`:1644`)
    - `PROJECT_UNKNOWN_KEY` (`:1266`)
    - Each is re-detected with the same id on every boot and reload, and re-raised.
  - **Rewording a message invalidates held ids.** The id hashes the message and fix text, so a release that rewords them breaks an id the orchestrator is holding ("may already be resolved", `config_upgrade_apply.rs:201-206`).
  - **Blocked auto-heals are never surfaced.**
    - The sibling rename refuses when the canonical file exists (`config_upgrade_apply.rs:1042-1045`, verified), yet the finding is AutoHeal (`config_upgrade.rs:2983`). With three legacy names (`:2700-2705`) only one rename can ever succeed.
    - Same for: a custom `settings.json` with no shipped default (`config_upgrade_apply.rs:976-979`); a template override (`:956-963`); `default.yaml` when `task.yaml` exists (`:752`); a keymap migration failure (`:1025`).
    - The residual `auto_heal=M` count is emitted, but the orchestrator template calls it "informational (already applied)" (verified, `default_orchestrator.md.template:726-728`), and `--needs-judgment` filters these out (`config_upgrade.rs:180-191`). Nobody resolves them, so they're re-detected every boot. Tests pin the "stays reported" behavior (`config_upgrade_apply.rs:2256,2385`).
  - **A crash can create a permanently blocked auto-heal.** `heal_workflow` writes `task.yaml` (`:770`) and only then unlinks `default.yaml`, ignoring the error (`:774-775`). Both files left behind is exactly the blocked case at `:752`.
  - **Absence sniffers have no tombstone.** `sniff_missing_lifecycle_defaults` (`config_upgrade.rs:2809-2846` → `config_upgrade_apply.rs:1075-1099`) covers `pr-template.md`, `changelog-template.md` and the preamble; also the orchestrator `## Changelog` section (`config_upgrade.rs:1031-1050`) and the preamble `## Jobs` section (`:2950`). A user who deletes one to opt out gets it rewritten on the next boot; `:990-992` admits the only opt-out is keeping the heading.
  - **Findings file drift.** The file has no production reader (`config_upgrade.rs:155-156`), yet the module docs say the orchestrator ingests it (`:29-30`, `:55-57`). `run_for_project` overwrites the hub-global file with one project's residual. Healers read-modify-write through a fixed temp name `.{name}.configupgrade.tmp` (`config_upgrade_apply.rs:1337-1342`) with no lock, so an `--apply-finding` overlapping a reload can collide.
- **Recommendation: make explicit.**
  - Add a hub-side decision store (`stable finding key → Kept | Approved | Blocked`), keyed independently of the wording, that `detect` filters through.
  - Re-classify a skipped auto-heal as NeedsJudgment (or a new `Blocked`), and fix template line 727.
  - Add a single exhaustive code→healer table (see Minor).
  - Lock apply with the hub lock.

### M10. Review-serve pgid record
- **Entity:** the review dev server's process group, recorded in `$SHELBI_REVIEW_PGID_FILE`.
- **Encoding:** 3, 4.
- **States:**
  - NoRecord
  - Recorded+live
  - Recorded+wrapper-killed (kept on purpose, `review_serve.rs:22-24`)
  - **Representable but invalid:** Recorded+stale (server gone, pgid possibly recycled).
- **Transitions (verified):**
  - Record with an overwriting `fs::write`: `review_serve.rs:73,92`.
  - Unconditional remove on clean exit: `review_serve.rs:78,98`.
  - Teardown read/kill/clear: `shelbi-orchestrator/src/workspace.rs:2332-2344`.
- **Risk: medium.**
  - **Two wrappers in one slot leak a server.** For example, the agent restarts the serve command: the second overwrites the record, then the first's clean exit deletes it without checking ownership. The second server is never reaped.
  - **A stale record can kill an unrelated group.** The teardown guard refuses only pgid ≤ 1 or its own group (`workspace.rs:2361-2363`).
  - **Past bug:** a stale record SIGTERMed the whole `cargo test` suite under `zen probe` (`workspace.rs:2350-2357`).
- **Recommendation: make explicit.** Write the record atomically with the leader's start time, delete it only if it still holds our pgid (compare-and-delete), and never signal a pgid whose start time doesn't match.

### M11. `shelbi workspace stop`: release-then-kill ordering
- **Entity:** a workspace slot's assignment vs its live session.
- **Encoding:** 5, 4, 3 (the park marker on review tasks).
- **States:**
  - Assigned+live
  - Released+live (meant to be transient)
  - Released+dead (idle)
  - Parked review task
- **Transitions:** park `workspace.rs:528` (to `568`), release `workspace.rs:536` (to `692-694`), kill `workspace.rs:546-547`.
- **Risk: medium.** Release is first on purpose, to avoid a "churn loop" (`workspace.rs:513-518`). If the kill then fails (e.g. the remote is unreachable), the error propagates after the task is already back in `todo`. The agent keeps working while the card is free to dispatch elsewhere, so two agents can end up on one task. F18 (`workspace.rs:688-691`) already fixed an earlier split write in this path.
- **Recommendation: document.** Better, compensate on kill failure (re-assign, or record a `stopping` slot state) so the board can't show the task as free while the session lives.

### M12. Workflow transition actions decoupled from the board move
- **Entity:** a workflow edge `from → to` and its git/PR side effects.
- **Encoding:** 5, 4.
- **States:**
  - Moved with actions run
  - Moved without actions (`issue move --skip-transition-actions`, `issue.rs:377-389`)
  - Actions run without a move
  - Actions partially run
- **Transitions:**
  - `action apply-transition --from --to` runs `execute_transition` (`action.rs:190-218`, `197`) without checking that the card is on `from`, and never moves it.
  - The single-verb primitives (`action.rs:121-188`) run in any state.
- **Risk: medium.**
  - **The card and the git state can disagree.** `apply-transition --from review --to done` merges and deletes the branch while the card stays in review. The only detector is the read-only `review_tasks_missing_pr` (`status.rs:227-269`).
  - **Partial runs leave no record.** `execute_transition` stops at the first failure (`action.rs:95-104`), leaving e.g. a pushed branch with no PR. Recovery relies on every action being idempotent.
- **Recommendation: make explicit.** Validate `from` against the card's column, and record a per-edge outcome so a partial run is visible and resumable.

### M13. Per-project board refresh health in the daemon (cadence, pause, failure episode, cold reads)
- **Entity:** each open remote-tracker project's refresh loop (`daemon/board.rs`).
- **Encoding:** 1, 3, 6.
  - In memory: `last: HashMap<_, Instant>`, `paused: HashSet`, and the `KeyCache` memo (`board.rs:616-623`).
  - On disk: the error-episode sidecar, the index's `stale` flag, and `last_cold_read` / `interval_secs` in `board-index.json`.
- **States:** Due / NotDue × Healthy / FailingEpisode / Paused(until), plus cold-read-due vs incremental.
- **Transitions:**
  - pause enter/exit: `board.rs:651-664`, `639-641`
  - tick: `642-649`
  - failure episode (first-failure gate = sidecar presence): `183-186`, `198-218`
  - clear: `182`, `222-226`
  - stale: `185`, `232-237`, `656`
  - cold-read schedule: `315-319`, `431-440`
  - close: `669-673`
- **Risk: medium-low.**
  - **Pause lives only in memory**, while failures live on disk; both mark the index stale by different routes.
  - **The index is rewritten every 2s while paused** (`656`).
  - **A token or key that failed to resolve is cached as `None` until the project closes** (`785-808`), so a token added later is ignored.
  - **One manager thread serves every project**, so one wedged read stalls them all (`82-86`).
  - The reopen announcement admits two gaps that need persistent per-id state (`333-342`).
- **Recommendation: document.** Make it explicit only if pause/failure semantics grow, and expire negative cache entries.

### M14. File-backend `$EDITOR` mode bypasses move, assign and priority rules
- **Entity:** issue frontmatter (`column`, `priority`, `assigned_to`) on the `file_system` backend.
- **Encoding:** 3, 8.
- **States:** whatever the user types, including undeclared statuses and duplicate priorities.
- **Transitions:** with no field flags, `issue edit` on a file backend opens `task_path` directly in `$EDITOR` (`issue.rs:1490-1497`). There's no validation, no daemon call, no transition actions and no event.
- **Risk: medium.** The tracker path refuses `column`, `priority`, `assigned_to`, `id` and the timestamps (`issue_editor.rs:27-34`, enforced `193-199`) with "use `shelbi issue move`", but the file backend allows all of them. Test `tests/issue_edit.rs:424` pins this behavior. The board-seam gate (`tests/board_seam_gate.rs`) can't see an editor-process write.
- **Recommendation: redesign.** Route the file backend through `edit_on_tracker`'s draft-diff-save path, so both backends share one set of read-only keys.

### M15. `HANDOFF.md` consume-on-read marker
- **Entity:** the session handoff note.
- **Encoding:** 3.
- **States:** Absent, Present, Consumed. A failed delete leaves it Present.
- **Transitions:** read `status.rs:602` → print → `remove_file` `status.rs:625`. The presence check is `status.rs:634`. The path is `<local work_dir>/HANDOFF.md` (`status.rs:643-646`).
- **Risk: medium.**
  - **No claim step.** Two concurrent `status --handoff` runs both print the note, and a newer note written between `602` and `625` is deleted unread.
  - **Path drift.** The layout docs and the unused `handoff_md_path` (`shelbi-state/src/project_paths.rs:40,142`; `commands/project.rs:31`) put it under the state root, not the work dir.
  - **Two handoff protocols.** The orchestrator's `agents/orchestrator/handoff.md` already uses a rename-claim protocol (`shelbi-state/src/agent_workspaces.rs:512-539`).
- **Recommendation: redesign.** Use one path and the rename-claim pattern.

### M16. CLI new-project flow: implicit step order and no Retry
- **Entity:** one `shelbi init` / first-run / `project add` run.
- **Encoding:** 5, 7, 1.
  - Steps are just the call order in `answer_steps` (`wizard.rs:168-294`). `Step` only drives headings (`:362-367`).
  - The Folder loop runs on `name: Option` + `asked_name: bool` (`:310-342`).
  - First-run counts as "done" when any registration file exists (`commands/wizard.rs:26`).
- **States:**
  - Folder → Agents → Issues → Settings → Creating
  - Created
  - Quit
  - **Missing:** Failed-after-register.
- **Transitions:** creation always uses `Attempt::New` (`wizard.rs:146`). The engine publishes the registration (Config stage) before Agents and Workflows (`shelbi-orchestrator/src/project_create/mod.rs:78-95`).
- **Risk: medium.**
  - **A half-created project looks complete.** If `create_project` fails after Config:
    - `shelbi init -y` prints "already configured" and quits (`wizard.rs:304-307`)
    - interactive mode hits `Problem::Taken` (`:326-337`)
    - bare `shelbi` treats setup as completed (`commands/wizard.rs:26-28`)
  - **Only partly mitigated** by reload's re-scaffold (`reload.rs:80-81`) and the config-upgrade pass.
  - **The CLI has no Retry.** The TUI and desktop have "Try Again" via `Attempt::Retry` (`shelbi-app/src/add_project_wizard.rs:1108`, `desktop/src/views/add_project.rs:153`).
  - **Three step machines are kept equal only by a test.** This flow, the TUI `Form` and the desktop `Wizard` agree only through the byte-identical test at `wizard.rs:822`.
- **Recommendation: make explicit.** Offer `Attempt::Retry` in the CLI, and drive it from the shared `Step` enum.

---

## Low

### L1. Tracker connect and Linear sync: multi-file writes with no transaction (medium-low)
- **Writers:**
  - config write: `shelbi-orchestrator/src/tracker_connect.rs:245`
  - filter cache: `:252` (error dropped with `let _ =`)
  - status sync: `:253`
  - `tracker linear sync --rewrite-workflows --yes` writes the catalog (`tracker.rs:523-524`) before rewriting workflows (`:546`)
- **States:** synced; connected but statuses not synced (`StatusSync::Failed`, `tracker.rs:347`); catalog synced but workflows broken (`:601-603`).
- **Risk:** a partial connect leaves a project on the new backend with unmanaged statuses; it is only printed. `tracker.rs:521-522` names this hazard.
- **Recommendation:** document; longer term, stage and commit the files together, or leave a "sync pending" marker the daemon finishes.

### L2. Session `Connection` and `ControlClient` request protocols without read deadlines (medium-low)
- **Entities:** `shelbi-client/src/connect.rs:77-98` and `shelbi-client/src/control.rs:54-59`.
- **Encoding:** 5, 6, 8.
- **States:**
  - Session connection: Handshaking → Ready → Attached → Detached → Gone. There is no client-side attached flag.
  - Control client: Hello → RequestMode | SubscribedMode. The switch to SubscribedMode is a real typestate: `subscribe` consumes `self` (`control.rs:367-386`).
- **Risks:**
  - **Replies have no timeout and are matched by order.** Replies are untagged FIFO (`connect.rs:276-280`). A wrong-kind reply errors but leaves the real reply queued, shifting every later reply (`:337`, `:349`).
  - **Frames buffered after the hello are discarded** (`connect.rs:424`), so a session exiting during connect yields EOF, not `Exited`.
  - **`since_seq` is unused** (`connect.rs:284`). The session ignores it, but `relay.rs:22-25` and `error.rs:63-65` promise exact resume.
  - **The control client has no read timeout.** Hello (`control.rs:86`) and every request can hang; `mutate_client.rs:71-87` bounds only the connect.
  - **EOF counts as done.** `lifecycle` treats EOF as success (`control.rs:234`), so a daemon crash mid-`QuitProject` reports done.
  - **Past bugs:** `rt-review-screen-hangs-on-connecting` (`connect.rs:72`); `rt-find-the-5s-connection…` (`connect.rs:108`; `tests/protocol_e2e.rs:828`).
- **Recommendation:** document the protocol states; add bounded reads and a per-message "EOF means done" flag; carry leftover handshake bytes into the reader.

### L3. System config-update protocol exists only as prose (medium-low)
- **The protocol:** inventory → stage → lint → preview → confirm → race-check → apply.
- **Where it lives:**
  - The agent skill: `shelbi-orchestrator/src/system_plugin.rs`.
  - Modeled only in a test harness: `tests/system_config_workflow_e2e.rs:212-230` (`Session`), `349-351` (`may_apply`), `378-380` (reconfirm).
  - `config inventory` makes a stage dir (`config_surfaces.rs:116-151`, `353-359`) that the CLI never deletes; the test's `Drop` does (`system_config_workflow_e2e.rs:225-229`).
- **Risk:**
  - Race detection and "apply only what was confirmed" rest on an LLM following prose.
  - Stage dirs accumulate in temp, and a failed inventory (`:129-135`) leaves a half-built one.
- **Recommendation:** redesign by adding CLI verbs (`config apply --staged --expect-baseline`) so the confirm and race-check gate is code.

### L4. Control-socket job admission and drain accounting
- **Entity:** `ControlJobs` and each job's guard.
- **States:**
  - Admitted → Running → ReplyQueued → Released
  - Refused (`ShuttingDown`)
  - Abandoned at exit
- **Transitions:**
  - `admit`: `daemon/control.rs:77-86`
  - `track` (used for quits, bypasses the gate): `control.rs:69,628,638`
  - `spawn_job`: `725-748`
  - release: `152-154`, `756-758`, `772`
  - abandon report: `serve.rs:681-689`
- **Risk:** low. The invariants are reasoned out in comments and tested (`control.rs:2247,2271`). Only a job that outlives `DAEMON_JOB_DRAIN` dies half-applied, recorded only as `shutdown-abandoned-jobs` (`serve.rs:688`).
- **Recommendation:** leave; fold `draining` into H1's phase.

### L5. Lost GUI login session recovery across daemon generations
- **Entity:** the session-lost marker + `SPAWN_BLOCKED` gate + re-exec takeover.
- **Status:** already mostly explicit (`SessionLostEvent` + `apply`, `daemon/session_health.rs:115-191`; `RecoveryState`; diagram at `:36-46`).
- **Side state:** the `unsynced` flag (`:141`) and the global `SPAWN_BLOCKED` (`shelbi-orchestrator/src/session_guard.rs:20`).
- **Representable but invalid:** RecoveryStarting with `record=None`. `set_recovery` skips silently (`:214-218`) and `sync` returns true (`:225`); this is only unreachable because of call order in `on_lost` (`:313-327`).
- **Risk:** low-medium.
  - Two processes write the marker; the successor's `takeover_failed` load/check/apply (`:78-82`) is a TOCTOU.
  - Recovery has a single 500ms launch window (`:395`, `469-475`).
  - **Past bugs:** `lost-session-reexec-race` (`serve.rs:157-169,258-263`); the test race at `tests/session_lost_takeover.rs:211`.
- **Recommendation:** leave; make RecoveryStarting-on-None an error.

### L6. Per-project poller manager entries
- **Entity:** `running: HashMap<String, WorkspacePoller>` (`daemon/poller.rs`).
- **States:** Absent / Active / Finished (cancelled, still cached) / Inert (lock held elsewhere, never cached).
- **Transitions:**
  - close: `poller.rs:79-96`, with a barrier wait at `89`
  - replace a finished poller: `106-108`
  - insert: `112-115`
  - teardown: `63`
- **Risk:** low-medium.
  - `barrier.wait()` blocks the single manager thread, delaying every other project's start.
  - **Past bug:** a reopen inside a tick left a cancelled poller cached, so the project went unpolled until restart (`poller.rs:267-299`).
- **Recommendation:** document. Liveness lives in `WorkspacePoller` (cross-slice).

### L7. Control-socket connection protocol (hello, subscribe, client registry, detach)
- **States:** Connected → Helloed → Subscribed / RegisteredClient → DetachRequested → Gone, with `client_version: Option` (`control.rs:562`) and membership in the `subscribers` / `clients` maps (`286`, `293`).
- **Transitions:**
  - hello: `572-581`
  - subscribe/register: `582-594`
  - reexec push on version mismatch: `599-608`
  - detach push (entry kept): `436-453`
  - unregister: `389-392`, `717`
- **Risk:** low. Hello is optional, so a Subscribe before Hello skips the out-of-date check (`599`). A client that ignores `Detach` stays listed until EOF.
- **Recommendation:** leave or document.

### L8. Workspace slot state derived two ways
- **The two derivations:**
  - `workspace list` classifies from the board plus a live probe (`workspace.rs:323-399`, `442-476`): idle / in_progress / review / user shell / orphaned / unreachable.
  - `workspace status` prints the poller's `status.yaml` `WorkspaceState` (`620-628`).
- **Invalid but rendered:** multiple in-progress ids on one slot (`470-475`).
- **Past bug:** a review slot mislabeled as orphaned (`1004-1009`).
- **Other:** `MigrationState::Pending` (`233-252`) clears only on reopen.
- **Recommendation:** document; move the `list` classifier next to `WorkspaceState` in shelbi-state or shelbi-app.

### L9. Pre-commit commit guard: Absent / Managed / Foreign
- **Entity:** the hub checkout's `pre-commit` hook (file presence + `HOOK_MARKER` + git `core.hooksPath`).
- **Transitions:**
  - install: `guard.rs:62-66`, `shelbi-orchestrator/src/project_create/mod.rs:926`
  - refresh only: `shelbi-orchestrator/src/lib.rs:580-583`
  - uninstall: `guard.rs:41`
  - The logic itself is explicit: `HookInstall` / `HookUninstall` in `shelbi-orchestrator/src/githook.rs:211-266`.
- **Risk:** low-medium.
  - **`guard status` reports the wrong path.** It hard-codes `<work_dir>/.git/hooks/pre-commit` (`guard.rs:124`; also `83`, `89`, `104`, `110`), while install resolves `git rev-parse --git-path hooks` (`githook.rs:172-193`). With `core.hooksPath` set, or a linked-worktree hub, status says "not installed" while the guard is active.
  - **Teardown doesn't remove the hook.** `githook.rs:45,254` (and CLAUDE.md) say it's removed on teardown, but the only uninstall caller is `guard.rs:41`.
- **Recommendation:** document; add `githook::status() -> enum` built on `hooks_dir`; wire teardown or fix the docs.

### L10. Reminder lifecycle
- **States:**
  - Pending (in awareness.json)
  - Fired (in history; recurring reminders return to Pending with `due_at` advanced)
  - Canceled (removed, no record)
  - The status words come from which file an entry was found in (`remind.rs:181,186,206,217`).
- **Writers:**
  - add: `remind.rs:102`
  - cancel: `remind.rs:87`
  - fire: `shelbi-orchestrator/src/awareness.rs:250-269`
  - history: `awareness.rs:241`
- **Risk:** low. The fire event line is appended inside the locked read-modify-write (`256`), so a failed store write fires again. A cancel leaves no history, unlike jobs.
- **Recommendation:** leave or document.

### L11. Zen merge protocol as CLI exit codes, and `msrv-check` outcomes
- **Zen merge exit codes:**
  - probe: 0 or 3 (`zen.rs:351-375`)
  - pr-create: 0 / 75 / 1 (`386-400`)
  - ci-watch: 0 / 1 / 2 (`430-445`)
  - pr-merge: SHA or `sha-pending` (`454-458`)
  - The durable phase is already explicit in `shelbi-orchestrator/src/zen_flow.rs` (`ZenFlowPhase`).
- **`msrv-check` (`msrv_check.rs:42-110`):** disabled, no MSRV, no rustup, toolchain missing, and install failed all exit 0, the same as "passed". The probe gate records "passed" on a machine that can never run the check; only stderr says otherwise.
- **Recommendation:** leave the Zen CLI (document its exit-code table in one place). Have msrv-check print a stable `skipped:` line the probe can surface.

---

## Minor
- **`issue prio` / `depends` / `rm` write the store directly** (`issue.rs:1089-1143`, `868-899`, `1567-1577`), outside the daemon's `Prioritize` / mutation seam.
  - `depends` is a read-modify-write with no expected revision, so concurrent edits lose an update.
  - `rm` runs delete → renumber → `clear_zen_demotion` with no status check, so a crash leaves the column unnumbered.
- **`issue start`'s estimate is written client-side after the daemon start** (`issue.rs:413-425`, `445-479`). A failure leaves an active issue with no estimate; `resume` never sets one.
- **The CLI falls back to the built-in default workflow on a load error** (`issue.rs:926-947`), so dispatch can resolve against statuses the real workflow lacks.
- **`mutate_client.rs:32`:** a `current_state` read error becomes `expected: None`, which silently skips the daemon's staleness gate (`shelbi-proto/src/control.rs:258-260`).
- **Unacked `message-pushed` and the subscribe stream:** a disconnected quiet subscriber holds a handler thread until the next change (`serve.rs:1094-1106`).
- **`control.rs:280-282`:** `issue_locks` are never removed and grow per issue for the daemon's lifetime.
- **Stale docs:**
  - `hub_version.rs:39-43,63-66` still say a supervisor relaunches the daemon.
  - `board.rs:821-825` says "open" means a live tmux session.
  - `send.rs:4-18` promises a removed agent-registry fallback.
- **`open.rs:41-51`:** the busy check then `open_user_shell` is a TOCTOU, and an unreadable occupancy reads as "not busy".
- **`session_attach.rs:206-210,258-272`:** the loop outcome is the `exited: Option` + `detached: bool` pair (three outcomes).
- **`session_cli.rs:152-191,387-409`:** Live / Dead(exit.json) / Dead(no record) derived from `alive` + file presence. The selector tie-breaks on alive/launched_at (past bugs `rt-review-slot-relaunch-loop`, `phantom-working-dead-session`).
- **`lifecycle.rs:270-285`:** restart verification uses `probe != NotRunning`, which a draining old daemon would also satisfy. Safe here only because stop waited first.
- **`orchestrator.rs:797-802`:** `ack_delivery` reads then writes the cursor outside one lock (mitigated by the high-water mark, `event_log.rs:1399-1404`).
- **`orchestrator.rs:1152-1216`:** `event_kind` is an order-sensitive classifier, and unknown shapes are hidden by v2.
- **`orchestrator_awareness.rs:156-177`:** events cut by `cap()` are acked and only summarized.
- **`job.rs:132`:** `watch --pid` registers after a liveness check, so a recycled pid is watched.
- **`zen.rs:58-71`:** a SIGKILLed probe leaves `.shelbi-probe-*` worktrees for the startup sweep.
- **`orchestrate.rs:32`:** `--status` is accepted and ignored.
- **`agent.rs:183-195`:** `agent new` checks, then writes three items non-atomically; a failure leaves a dir that blocks a re-run.
- **Config-upgrade dispatch:** AutoHeal codes and healers are matched by two separate string dispatchers (`config_upgrade.rs:385-466` vs `config_upgrade_apply.rs:246-353`) with no exhaustiveness check. A code added to one side only becomes a permanently blocked auto-heal (M9).
- **`config_surfaces.rs:117-118,353-358`:** `config inventory` copies `settings.local.json` (which may hold user keys) into a temp dir with default permissions, unlike `temp_dir.rs:37`'s 0700.
- **`issue_tracker_setup.rs:196-199`:** a failed filter-list load prints "no filter" but keeps a seeded filter.
- **Two request-outcome encodings:** Linear uses stringly `"ok"` / `"err:parked"` (`linear_health.rs:98,103`). GitHub uses `spent` / `not_modified` bools (`doctor.rs:295-304`), where `spent && not_modified` is representable but invalid.
- **`status.rs:389-411` vs `commands/mod.rs:93-105`:** two classifiers of board-index freshness. `status` labels a Linear board "github:" (`status.rs:163`).
- **`issue-store migrate`:** if a card is created but copying its comments fails, a re-run skips the card, so those comments are lost permanently (`shelbi-state/src/issue_migrate.rs:241-274`). Both boards stay live until the user switches (`issue_store.rs:313-325`).
- **`machine.rs:96,192,253,274`:** the per-machine binary record is never invalidated when a later probe finds the binary gone, so status shows a stale "yes (last known)".
- **Relay `control` service:** a refusal frame (`relay/services.rs:269-276`) is written by the main thread while the forwarder writes raw daemon bytes (`:307-310`), so it can land inside a split daemon frame and corrupt the client's `FrameReader` (`control.rs:570-598`). Latent; refusals are rare.
- **`relay.rs:207-219,293-298`:** a clean EOF is marked "unreachable", and a `pending_list` waiter pushed after teardown is never answered.
- **Files with nothing relevant (beyond the above):**
  - shelbi-cli: `commands/{daemon.rs, session.rs, temp_dir.rs, init.rs, config.rs (routing), project.rs, linear_health.rs, relay.rs, remote.rs (one-shot retry), events.rs (tail follower is sound), changelog.rs, workflow.rs, diff_comment.rs, review_serve.rs (only M10)}`, `main.rs` (`DefaultRoute` is already an enum), `commands/mod.rs`.
  - shelbi-client: `lib.rs, error.rs, spawn.rs, transport.rs`.
  - shelbi-proto: `lib.rs, capability.rs, error.rs, frame.rs, message.rs, ext.rs`. Most of `control.rs`: `StepState` and `SessionOpenError` are explicit enums.
  - Tests, read as evidence only: `version_flag, session_survival, old_session_compat, session_cli, session_backend_e2e, control_socket, relay_real_ssh, relay_services, remote_connect, board_seam_gate, changelog_attachments_cli, diff_comment_cli, job_history_cli, job_shell_e2e, review_serve_wrapper, awareness_unregistered, config_inventory_lint, init_noninteractive`.

---

## Cross-slice leads
- **shelbi-state:**
  - `daemon_lifecycle.rs`:
    - `:71-97` (`daemon_lock_held`, a probe that briefly takes the bind lock)
    - `:207-224` (`ensure_daemon_running_with`, accepts a draining daemon; H1)
    - `:404-465` (`stop_daemon_with` / `decide_stop`, the SIGTERM→SIGKILL escalation)
  - `hub_version.rs:123,153,179`: `DaemonProbe`; `NoHello` maps to Mismatch.
  - `lib.rs:2495-2563`: Zen crash recovery, with the `zen_mode` write at `2558` bypassing `set_zen_mode` (H2).
  - `event_log.rs`:
    - `:1383-1404` (cursor high-water mark)
    - `:1492-1503` (`claim_event_follower` SIGTERMs the registry pid with no identity check; M2)
    - `:1777-1849` (`MessageDelivery` flag-soup derivation; M7)
  - `questions.rs:141-159,259-275`: dedupe and the missing hub-level fallback (M8).
  - `agent_workspaces.rs:405-539` vs `project_paths.rs:142`: the two handoff mechanisms (M15).
  - `issue_migrate.rs:223-280`.
  - `migrate.rs:184-300`: in-repo migration is plan-based and re-derived; the global YAML is retired last.
  - `workspace_status.rs:42` / `migration.rs:42`: explicit enums whose writers live in the poller.
- **shelbi-orchestrator:**
  - `quit.rs:88-114,504-590`: `QUITS_IN_FLIGHT` and quit composition order (H1).
  - `cancel.rs:236,369`: `begin_quit` / `quit_project` barrier.
  - `poller.rs:144-205`: `WorkspacePoller` Inert/Active/Finished.
  - `poller.rs:376-377`: relies on the dead `__zen-orch-start` (H2).
  - `lib.rs:938,968,990,1288-1292`: bootstrap prompts reference the crash line; the tmux-removal note (H2).
  - `mutate/start.rs:402-440`: the duplicated `rollback_start` and `rollback_and_teardown` (H3).
  - `transition_move.rs`: Move + `skip_transition_actions` (H3, M12).
  - `awareness.rs:209-243,404-405,566-579,851-971`: job and reminder poller, report routing, cancel ordering (M3, L10).
  - `job_exec.rs:100-175`: runner pid/result writes.
  - `workspace.rs:2331-2363,2444-2449`: review-server teardown (M10); the kill-reason record-then-clear two-phase marker.
  - `session_process_backend.rs:544-585`: `slot_death` from disk markers; remote deaths are always `Crash` (M4).
  - `remote_session.rs:373,466,580,601,669,715`: remote pickers (M4).
  - `session_connect.rs` (`relay_opener`): does the second hop have a timeout? (M6)
  - `review_session.rs:253,505`: review content sessions keyed by slot, not task (evidence `tests/review_content_sessions_e2e.rs:229,252`).
  - `zen_flow.rs`: `ZenFlowPhase` warn-only advance (`:11-12`) may strand a phase.
  - `tracker_connect.rs:184-266` (L1). `githook.rs:172-266` (L9). `project_create/mod.rs:78-95,306-315` (M16).
  - `session_guard.rs:20-29`: global `SPAWN_BLOCKED`.
  - `system_plugin.rs`: config-update skill prose (L3).
- **shelbi-session:**
  - `session.rs:256,451-602,923-951`: session dir markers, non-atomic `meta.json` / `exit.json` (M4).
  - `session.rs:268-273` + `transport.rs:157,276-329`: capability-gated `Resync`, plus Ping/`Exited` sent to unattached clients (M5, L2).
  - `session.rs:478-480`: the watchdog that restarts the daemon (H1).
- **shelbi-app:**
  - `session/remote.rs:234-335`: remote pickers and unbounded connect/info (M4, M6).
  - `add_project_wizard.rs:1108`: the sibling new-project step machine (M16).
  - `linear_setup.rs:331-625`: `LinearChoice`, a fairly explicit machine driven by `issue_tracker_setup.rs:136-213`.
  - `diff/comments.rs:98,156,169`: a thread has only `sent: bool`, no open/resolved state.
- **desktop:** `src/views/add_project.rs:153` (Retry).
- **Templates:** `shelbi-state/src/default_orchestrator.md.template:283` (crash-recovery line; H2) and `:726-728` (`auto_heal` called "already applied"; M9).
