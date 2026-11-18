# Implicit state machines: state-core slice

**Slice:** `crates/shelbi-state` and `crates/shelbi-core`, src and tests: all 107 `.rs` files, about 101k lines. Every file was walked; per-group coverage is listed in the Coverage section at the end. Surveyed 2026-11-18 at `e8145b3f`.

**Findings:** 4 high, 14 medium, 17 low, plus a Minor list. Every high finding, and the headline claim of each medium one, was re-checked against the code by hand. Points marked *suspected* were reasoned from the code but not reproduced.

**Themes:**
- Status semantics keyed off literal ids instead of the workflow catalog.
- Workflow move policy hidden in `Option`-ness.
- Two lifecycle signals whose producer or reader drifted away (the Zen heartbeat and the open-project set).
- Many unlocked read-modify-write JSON/YAML files shared across processes.
- Pid records with no process identity.

---

## HIGH

### H1. Status semantics keyed off literal core ids, not the catalog category

- **Entity:** an issue's status (`Issue.column: Column(String)`, shelbi-core model.rs:2503), and what it *means*: terminal or open, done for dependencies, review.
- **Encoding:** 2 (stringly), 8 (`StatusCategory` exists, statuses.rs, but behavior bypasses it).
- **States:** any id declared in `statuses.yaml`, each with a category (Backlog/Ready/Active/Handoff/Done/Archived). Representable but invalid:
  - a custom Done- or Archived-category status, which most code treats as open;
  - a status id that alias folding turns into a different, undeclared id (`normalize_status_id`: `completed`/`wip`/`cancelled` → core ids, model.rs:2338-2348);
  - an undeclared id, which silently falls back to Backlog (`Column::category()`, model.rs:2310-2320, whose own doc at 2306-2309 says behavior code must never use it).
- **Transitions / deciders:** there is no single "is this terminal / done / review" function. There are at least four rules:
  - `Issue::is_blocked` compares against the literal `done` (model.rs:2609-2614). statuses.rs:115 claims it keys off the terminal category.
  - `is_terminal_column` uses the fallback category, so only `done`/`canceled` count (issue_store.rs:464-469). Used by `CachedIssueStore::list_in_status` (issue_cache.rs:464-475) and `done_history::page_in_status` (done_history.rs:236-241).
  - GitHub `is_terminal` is the literal `done || canceled` (github_store.rs:5107-5109). It chooses open/closed on create and move (:965, :1229, :1239, :1334) and index routing (:2517, :2540, :4907, :4960-4966).
  - Linear uses the state type `is_closed_state` (linear_store/mapping.rs:69-71) and `row.closed` (linear_store/mod.rs:451-466, 755-766).
  - About 40 production `== Column::review()/todo()/in_progress()/done()` checks in other crates (e.g. orchestrator load.rs:300/509, review_session.rs:88/221, poller.rs:3055/6044, sidebar_model.rs:82, actions.rs:541/797/865/1851; state lib.rs:3367/3649; tui app.rs:666/686; cli message.rs:225).
  - Category-fallback users: app board.rs:89 and orchestrator poller.rs:3538-3549.
- **Where:** shelbi-core model.rs:2205-2350, 2605-2615; state issue_store.rs:460-470; issue_cache.rs:458-476; done_history.rs:234-242; github_store.rs:5107.
- **Risk: high.** Concrete failures, all confirmed in code:
  - **Canceled dependencies block forever.** A dependency in `canceled` (or any non-`done` Done/Archived status) blocks its dependents for good. `list_ready` (lib.rs:3367, used by `shelbi issue` at cli issue.rs:680) filters `!is_blocked`, so the dependent never shows as ready. All clients badge it blocked (app view.rs:1101, tui kanban.rs:2556/3489, cli issue.rs:760/854).
  - **Synced Linear closed states list nothing.** `resolve_issue_store` wraps Linear in `CachedIssueStore` (issue_store.rs:59-64). Its `list_in_status` routes a synced Linear closed state such as `duplicate` (category Archived) to the open snapshot, which never holds closed cards, so the lane is always empty. Routing to the done page would not help, because `page_in_status` re-filters by the literal rule. The only test (linear_store/tests.rs:553-583) calls `LinearStore` directly and misses the wrapper.
  - **Custom done statuses stay open on GitHub.** A user-defined Done-category status (e.g. `shipped`) is written as an *open* GitHub issue and stays in the open index.
  - **Linear review cards may miss literal review checks.** Linear's default "In Review" syncs to id `in-review` (Handoff category, linear_statuses.rs:93/111), so every literal `== Column::review()` check should miss those cards. *Suspected; the orch-core slice should confirm.*
  - **Alias folding desyncs task and catalog.** `mapping.rs:57` builds the task column via `Column::from_status_id`, so a Linear state named "Completed" lands on `done` while the catalog declares `completed`. *Suspected.*
- **Recommendation: make explicit.**
  - Add one `StatusSemantics` resolver (status id + resolved workflow/catalog → category and roles), and make `is_blocked`, the cache/done-history routing, the GitHub open/closed choice and the review checks go through it.
  - Stop alias-folding non-core ids, or reject catalog ids that would fold.
  - This is the most cross-cutting machine in the slice.

### H2. Workflow move policy is encoded by `transitions: Option<Vec<_>>` being None, empty or non-empty

- **Entity:** a workflow's move-legality and side-effect policy (`Workflow.transitions`, shelbi-core workflow.rs:90).
- **Encoding:** 8, 1 (`Option`-ness as mode), 2 (YAML).
- **States:**
  - **Legacy (`None`):**
    - agents may make any move (orchestrator poller.rs:3609-3613 `None => true`);
    - the handoff always pushes and opens a PR (poller.rs:3983 `touches_git = transitions.is_none() || …`);
    - the workflow is never branch-free (`produces_no_branch`, workflow.rs:556-568).
  - **Declared, non-empty:**
    - agents may only take listed edges ("edge-not-in-workflow", poller.rs:3614-3618);
    - humans may still move any-to-any (`transition_allowed` only checks that both ids exist, workflow.rs:286-288, used by app board.rs:222);
    - git work runs only on edges whose actions push, open a PR or merge.
  - **Declared, empty (`Some([])`), representable and incoherent:**
    - agents can make no move at all;
    - `produces_no_branch` is true.
- **Transitions (mode writers):** all in shelbi-state workflow_config.rs:
  - `add_transition` (:1068): `None` → `Some`.
  - `delete_transition` (:1085-1094): deleting the last edge sets `None`, i.e. back to legacy.
  - `remove_workflow_status` (:994-1003): `retain` can leave `Some([])`.
  - Hand-written `transitions: []` round-trips as `Some([])`.
- **Where:** shelbi-core workflow.rs:80-91, 280-290, 535-568; orchestrator poller.rs:3609-3620, 3978-3990; state workflow_config.rs:994-1094.
- **Risk: high.**
  - **The docs say the opposite.** The field doc says transitions are any-to-any and "`None` ↔ `Some(vec![])` ↔ no actions on any edge" (workflow.rs:84-88). The poller restricts agent moves by them, and `None` vs `Some([])` behave differently.
  - **Deleting the last edge in Settings silently flips a branch-free planning workflow into legacy mode.** Agent moves become unrestricted and the handoff starts pushing and opening PRs.
  - **Adding the first edge does the reverse.** It silently confines agents to that one edge and turns off the implicit handoff push.
  - **Removing a status can strand every agent move.** It can leave `Some([])`, where every agent transition is refused.
  - **No single function answers** "may actor X move A→B, and what runs".
- **Recommendation: make explicit.**
  - Add an explicit policy (`mode: legacy | declared`, or an enum) and one `Workflow::check_move(actor, from, to) -> Result<Edge>` shared by the board, the CLI and the poller.
  - The editor should never change mode as a side effect of an edge edit.
  - Fix the field doc.

### H3. Zen crash-recovery heartbeat (`zen_orchestrator_alive_at`) has lost its producer

- **Entity:** a project orchestrator's liveness, and the "crashed last time" signal, in `state.json`.
- **Encoding:** 3 (state.json field), 6 (heartbeat staleness window), 5 (start → heartbeat → exit order, formerly enforced by the tmux pane wrapper).
- **States:**
  - cleared (`None`);
  - alive (recent timestamp);
  - crashed (a recent timestamp found at the next start);
  - stale (old timestamp, read as "no crash").
  - One field means "alive" while running and "crashed" after death.
  - Sibling fields: `orchestrator_last_crash_record`, `orchestrator_last_crashed_at` (lib.rs:1601-1649).
- **Transitions:**
  - `zen_heartbeat` (lib.rs:2521), `zen_clear_crash` (:2533);
  - `zen_check_crash_recovery` (:2547): consumes the signal and forces `zen_mode = Off`;
  - `record_orchestrator_crash` (:2602).
  - The only callers are the CLI hooks `__zen-orch-start` / `__zen-heartbeat` / `__zen-orch-exit` / `__orch-record-exit` (cli commands/zen_lifecycle.rs:51-157).
- **Where:** state lib.rs:1601-1649, 2496-2640; cli zen_lifecycle.rs.
- **Risk: high.** Verified by grep:
  - **The hooks have no producer.** They appear only in their clap definitions (cli main.rs:391-420) and doc comments. Orchestrator lib.rs:1288-1292 states that on the session backend "the zen heartbeat / signal traps … are not reproduced here" (deferred to "Phase 3 rt-daemon-poller"). So the heartbeat is never written and crash recovery never fires.
  - **Zen stays On after an orchestrator crash.** It keeps auto-merging on relaunch. The comment at orchestrator poller.rs:376-377 ("Zen stays off after the restart via its own `__zen-orch-start` crash-recovery step") is now false.
  - **No crash records are written,** so `shelbi zen status` always reports "last crash: never".
  - **Collateral latch:** `claude_md_migration_hinted` was reset only by `__zen-orch-start` (`reset_claude_md_migration_hint`, agent_workspaces.rs:647-656, called only at zen_lifecycle.rs:59). It now latches forever after its first fire.
- **Recommendation: redesign.**
  - Either re-home liveness and crash capture in daemon/session supervision with an explicit `OrchestratorLiveness { Running, ExitedClean, Crashed{at, record} }` written by the supervisor, or delete the dead hooks and fields and the false comments.
  - In either case, separate liveness from the crash marker.
  - Decide product-wise whether Zen should still auto-disable on a crash.

### H4. Project "open" flag: the open set misses in-repo (split) projects

- **Entity:** a project's open/closed lifecycle (`State::open`, lib.rs:1710-1721). It drives which projects get a daemon poller and board refresh, and whether the daemon may idle-exit.
- **Encoding:** 3 (bool in state.json, intentionally outlives the orchestrator), plus a registry scan that defines the universe of projects.
- **States:** open, or closed. There is no opening or quitting state; quit-in-flight lives in orchestrator memory (`quit::quit_in_flight`).
- **Transitions:**
  - `set_project_open` (lib.rs:2788);
  - set true by orchestrator `ensure_dashboard` (orchestrator lib.rs:563, best-effort);
  - set false by orchestrator quit.rs:542/596.
- **Readers:** `list_open_projects` (lib.rs:2809-2835), used by:
  - cli daemon/serve.rs:393 (idle exit), daemon/poller.rs:69 (poller reconcile), daemon/board.rs:846, daemon/lifecycle.rs:323;
  - orchestrator quit.rs:587; app command_model.rs:167; tui shell/mod.rs:1209;
  - also `is_project_open`, used by session daemon_watchdog.rs:80.
- **Where:** state lib.rs:1710-1721, 2788-2836.
- **Risk: high** (latent: no split projects exist on this machine today).
  - **Split projects are never in the open set.** `list_open_projects` scans only `projects/*.yaml` (lib.rs:2821). A split-registered project (`projects/<name>/local.yaml`, created by migrate-to-in-repo, which retires the flat yaml) is therefore never in the open set: no poller starts for it, and the daemon may idle-exit under it.
  - **The other registry scans already handle both shapes:** `has_project_registration` / `has_any_project_registration` (lib.rs:377-401) and `load_project` (lib.rs:1272-1282).
  - **One unreadable state.json closes the project.** It reads as closed (`unwrap_or(false)`, lib.rs:2829), which stops that project's poller.
  - The open-set test (lib.rs:2122) covers only flat files.
- **Recommendation: make explicit.**
  - Derive the open set from one shared "registered project ids" enumerator used by every scan.
  - Add an in-repo test.
  - Surface "open but unreadable" instead of quietly closing.

---

## MEDIUM

### M1. GitHub issue status = `shelbi:status/*` label × open/closed, with a lossy write path

- **Entity:** one GitHub issue's column.
- **Encoding:** 2, 4, 8.
- **States:** (status label, open/closed) mapped by `GhIssue::column` (github_store.rs:4955-4985). Representable but invalid:
  - open + terminal label (reads as backlog);
  - closed + non-terminal label (reads as done/canceled via `state_reason`);
  - no label (reads as the workflow's initial status);
  - two status labels (`status_label()` takes the first, :4885);
  - a status label past the first 10 labels.
- **Transitions (github_store.rs):**
  - `move_status`: one verified PATCH of labels+state (1321-1454), with a two-axis no-op check (1348).
  - `add` into a terminal status: POST open, then `set_state` close (1240).
  - `delete`, `cancel` (1616, 1650); `reject_review` (1710-1720).
  - Humans on github.com, detected by `reopened_terminal` (4902).
- **Risk: medium.**
  - **Label loss.** `get_raw` → `get_raw_by_number` (1992-2001) uses `SINGLE_ISSUE_QUERY` with `labels(first: 10)` (4201) and does not hydrate truncated labels as the board path does (2098-2103). `move_status` then PATCHes the full replacement set built from `gh.non_status_labels()` (1364). On an issue with more than 10 labels, every label past the 10th is deleted. If the status label is past 10, the no-op check is also wrong. No test covers more than 10 labels on the write path.
  - **Half-applied terminal create.** If the close at 1240 fails, the issue stays open with a terminal label: it reads as backlog, the daemon announces a false "reopen", and a retry hits "already exists" (1134).
  - **Past bug:** `move_retries_a_half_applied_terminal_move` (test ~7306; comment at 1339-1347).
- **Recommendation: make explicit.**
  - Route every status write, including create-into-terminal (POST open, then the verified move), through the one verified transition.
  - Hydrate labels in `get_raw_by_number`.

### M2. Composite tracker writes (body + move + assignment overlay + parked marker) are multi-step on every backend

- **Entity:** a card's (status, local `assigned_to` overlay, `parked-review` marker, description) during reject, cancel, delete, park and unassign.
- **Encoding:** 3 (marker files `assignments/<id>`, `parked-review/<id>`, `review-load-failures/<id>`, lib.rs:3677-3990), 4, 5.
- **States:** the trait promises single-write atomicity: `reject_review` "as one atomic operation, so a crash can't leave the card edited-but-not-moved or unowned-but-still-in-review" (issue_store.rs:758-763); `move_status_and_unassign` is a "single write" (725-730). Reachable on remote backends:
  - feedback appended, card still in review;
  - moved, still assigned;
  - unassigned, not parked.
- **Transitions:**
  - GitHub: `reject_review` 1710/1720; `move_status_and_unassign` 1630-1634; `park_review` 1676-1678.
  - Linear: description update, then move, then overlay (linear_store/mod.rs:1174-1179); `move_status_and_unassign` 1126-1135; `delete` 1137-1148; `move_inner` "move did not apply" after the update already landed (519-528).
  - FS: `park_review_task` saves the task, then the marker (lib.rs:3759-3766).
  - Orchestrator clears markers at load.rs:899/939 and mutate.rs:430.
- **Risk: medium.**
  - **Duplicate feedback.** A move failure after the body write leaves feedback appended with the card still in review, and a retry appends it again (nothing checks).
  - **Stale assignment.** A move error skips the overlay clear, so the card sits in its new state still routed to a workspace. This is the relaunch hazard the trait doc names.
  - **Re-grab after a crash.** A crash inside `park_review` leaves the card unowned and unparked, so the review autoloader grabs it again: the churn this subsystem exists to prevent.
  - **Markers outlive tasks.** `delete_task` (lib.rs:3191) sweeps no markers and takes no tasks lock, so a concurrent `move_task` can recreate the card.
- **Recommendation: make explicit.**
  - Model a per-task "slot claim" record (owner, parked, load failures) written atomically.
  - On remote backends, clear the local overlay first (idempotent) and make the feedback append idempotent (check for the dated header).
  - Fix the trait doc to say remote backends are best-effort.

### M3. GitHub rate-limit park, write reserve and write-retry sleep

- **Entity:** per-token budget tiers (graphql/rest/search) plus the unreachable breaker, in `gh-budget/<key>.json`, shared hub-wide; and the per-call write retry loop.
- **Encoding:** 3, 6, 1 (`parked_until`/`backoff_secs`/`reset_at`/`remaining` per tier, gh_budget.rs:114-131; breaker `window_secs`/`parked_until`, 147-154).
- **States:**
  - Per tier: free, or parked (reset | retry-after | backoff(n)) (`ParkMode`, :390). "Just expired" is inferred from a past `parked_until` (472-474).
  - Breaker: clear, or parked (window 15→300s).
  - Representable but invalid: `backoff_secs` without a park; `remaining`/`reset_at` describing an already-rolled window.
- **Transitions:**
  - `park_rate_limited` (gh_budget.rs:502, called from github_store.rs:3700);
  - `park_unreachable` (582, from 3744); `clear_unreachable` (607) and `clear_read_backoff` (542, from 3655-3658);
  - `record` (624, from 3456/3477/3651/3969/3990);
  - legacy `park` (366, test callers only).
  - Expiry is implicit in `park_verdict` (293).
  - Write retry: `RetryPolicy::run` (gh_retry.rs:212-262).
- **Risk: medium.** The core is well built (mutex + flock 313-354, pure `plan_rate_limit_park` 444 and `tick_plan` 719). The gaps:
  - **Stranded write refusal.** `check_write_reserve` (github_store.rs:2813-2837) refuses when the recorded `rest.remaining` is below the reserve, without checking whether `reset_at` has passed. `tick_plan` does check (743-747). Mutators resolve via GraphQL, so nothing refreshes the REST tier: writes stay refused, with a past reset time in the error, until some REST read happens.
  - **Unbounded write sleep.** The production write policy sleeps the server's `x-ratelimit-reset` hint verbatim (gh_retry.rs:233/255, `parse_wait_hint` 489-503), up to about an hour, on the calling thread. The read policy explicitly refuses this (224-230). *Which threads issue writes was not traced.*
  - **Lock failure reads as "parked".** If the flock fails, every writer treats the tier as already parked and records nothing (369, 509, 586).
  - **Last writer wins.** `record` overwrites `remaining` unconditionally (636-641).
  - **Past incidents** cited in comments: 2026-09-08 (900 attempts/min, gh_budget.rs:59-64), 2026-10-18 (an hour-long park for nothing, 438-440).
- **Recommendation: document**, and make two small fixes: treat `remaining` as unknown once `reset_at <= now` in the reserve check, and cap interactive write waits (fail fast with the reset time). Delete the unused `park`.

### M4. Event-follower registry signals pids without checking identity

- **Entity:** the per-project consuming event follower (`<project>/event-follower` pid file + `event-follower.lock`).
- **Encoding:** 3, 4 (`kill(pid,0)`), 5 (SIGTERM means either "stop" or "yield", told apart by re-reading the registry within a grace window).
- **States:** unowned; owned by a live pid; owned by a dead or recycled pid (representable); v2-only "yielded" (in-memory).
- **Transitions:**
  - claim: event_log.rs:1492-1501 (from cli orchestrator.rs:299, orchestrator_awareness.rs:271/352);
  - release: event_log.rs:1524.
  - Supersede checks disagree: `event_follower_owner` without a liveness check (orchestrator.rs:373-375) vs `live_event_follower` (orchestrator_awareness.rs:364-366).
- **Risk: medium.**
  - **Recycled pids get SIGTERM.** `claim_event_follower` SIGTERMs any live recorded pid (event_log.rs:1496-1499) without checking that it is a shelbi follower. A follower that died by SIGKILL, a crash or a reboot never releases, so the next claim can SIGTERM an unrelated process that reused the pid.
  - **Other code already guards this.** orchestrator awareness.rs:978 (`is_job_runner`) checks args "so a recycled pid is never signalled", and `DaemonPidRecord` records the start time (ssh_control.rs:143-147, 332-341).
- **Recommendation: make explicit.** Record pid + start time and use the strict `recorded_daemon_is_same_process` rule before signalling. Write the stop/yield protocol down in one place.

### M5. Event cursor + high-water mark written by consumers that don't share the delivery lock

- **Entity:** `<project>/event-cursor` plus `event-cursor.hwm`.
- **Encoding:** 3, 5, 8. Explicit parts exist: `CursorResolution` (event_log.rs:1201) and `FeedRead` (815).
- **States:**
  - absent: initialized at base (1281-1290, 1366-1378);
  - trusted;
  - rewound: repaired from the hwm (1227-1240);
  - expired: fast-forwarded (1304-1330);
  - future (cursor > head): fails closed (952-955), with no recovery;
  - legacy physical offset: normalized once (561-594).
- **Transitions:**
  - `write_event_cursor` (1383-1405) and in-file repairs (1237, 1288, 1327, 1376, 572);
  - v1 `events drain` and feed ack (cli orchestrator.rs:929-937, 953);
  - v2 stream and hooks (cli orchestrator_awareness.rs:217, 236);
  - the Codex bridge (orchestrator wake.rs:3107).
- **Risk: medium.**
  - **Double delivery.** Only v2 takes `event-delivery.lock` (orchestrator_awareness.rs:191), although its module doc promises exactly-once "across all three" (:17-20). v1 drain and feed ack do read-then-write without it, so v1 racing a v2 hook delivers a batch twice. The hwm prevents rewinds, not duplicates.
  - **No recovery from a future cursor.** If `events.log` is removed but the index is kept, every consumer errors until someone fixes it by hand (pinned at cli orchestrator.rs:1768-1774).
  - **Any rewrite counts as an ack.** Rotation judges consumer liveness from the cursor file's mtime (~3977).
- **Recommendation: document** v1/v2 exclusivity, or make every cursor writer take the delivery lock. Keep the hwm design.

### M6. Awareness command-job lifecycle re-derived each tick from four sources

- **Entity:** an awareness `Job`, made of:
  - the `awareness.json` entry;
  - `jobs/<id>/result.json`;
  - the runner pid (alive or not);
  - `jobs/history.json`.
- **Encoding:** 1 (`runner_pid: Option`; `estimate`/`next_alert_at` "set exactly when estimate is", awareness.rs:191-194), 3, 4, 6 (120s runner start grace, overdue grid).
- **States:**
  - registered;
  - running;
  - exited but not yet reported;
  - lost (pid dead with no result, or no pid after the grace);
  - reported;
  - canceled.
  - Representable but invalid: `estimate` without `next_alert_at`, or the reverse. `job_alert_due` needs both (orchestrator awareness.rs:333).
- **Transitions:**
  - `add_job` 414, `set_job_estimate` 446, `set_job_runner_pid` 457 (runner), `remove_job` 467, `write_job_result` 593 (runner), `record_finished_jobs` 888, `prune_job_history` 905;
  - the tick `fire_jobs` / `fire_job_overdue` (orchestrator awareness.rs:298-355, 404-424).
- **Risk: medium.**
  - **Mixed delivery guarantees.** Feed lines are appended inside the `update_awareness` closure, before the store's `atomic_write` (orch awareness.rs:219-232 vs awareness.rs:352-356), so a failed write re-emits them (at-least-once). Worker-routed reports go out on a detached thread after the write (orch :235-237, :566-579), so a crash loses them (at-most-once).
  - **A recycled pid strands a job.** `command_outcome`/`pid_outcome` use a bare `pid_alive` (orch :405/:436/:584), so a recycled pid keeps a dead job "running" forever. `cancel_job` does check identity (:978).
  - **Type drift.** `JobEvent.status` is a `&str` whose doc lists `lost` (event_log.rs:2149-2150), but `JobStatus` has no `Lost`.
- **Recommendation: make explicit.** Add one `JobPhase` derivation function, identity-checked pids, and a typed `JobEvent.status`.

### M7. Review dev-server pgid record has no process identity

- **Entity:** a review slot's dev-server process group (`workspaces/<slot>/review-serve.pgid`).
- **Encoding:** 3, 4.
- **States:** absent; recorded and live; recorded but dead or recycled (representable, and indistinguishable from live). A malformed record reads as `None` (workspace_status.rs:250-262).
- **Transitions:**
  - written by `shelbi __review-serve` (cli review_serve.rs:108);
  - read and cleared by `stop_review_server` (orchestrator workspace.rs:2331-2344);
  - moved by the legacy migration (workspace_state_migration.rs:76-105).
- **Where:** workspace_status.rs:221-273.
- **Risk: medium.** A bare integer guarded only by `kill(-pgid,0)`, a `>1` floor and "not our own group" (orchestrator workspace.rs:2365).
  - **Past incident:** a stale record matched the caller's group and SIGTERMed a `cargo test` run (comment at workspace.rs:2352-2361). The own-group guard patched only that case.
  - **A recycled pgid** belonging to any other live group still gets SIGTERM then SIGKILL.
  - **The record intentionally survives hub restarts** (workspace_status.rs:212-220), which widens the reuse window.
- **Recommendation: make explicit.** Store the leader's start time beside the pgid (reuse `process_start_time` and `recorded_daemon_is_same_process`), and refuse to signal on a mismatch.

### M8. Per-host reverse-forward decision (`forward-modes.json`): unlocked, and written from a read path

- **Entity:** one remote host's hub reverse forward (Unix, or TCP + port).
- **Encoding:** 3, 4 (ControlMaster liveness), 1 (mode + `Option<port>`), 8 (`EffectiveForward` enum, scattered writes).
- **States:** no record (auto); Unix; Tcp(port); `TcpUnbound` (Tcp with no port: representable, nothing can connect, ssh_control.rs:592-594). There is also a separate in-memory health map in shelbi-ssh (lib.rs:1284/1321) that can drift from the disk record.
- **Transitions:**
  - set Tcp+port on a successful sweep (shelbi-ssh lib.rs:1774);
  - cleared *inside the read* `effective_forward` when a `forward: unix` pin contradicts it (ssh_control.rs:637; callers shelbi-ssh lib.rs:278/1851/1900, orchestrator workspace.rs:5761);
  - dead masters reconciled at daemon start (ssh_control.rs:658-673, from cli serve.rs:793).
- **Risk: medium.**
  - **Lost entries.** `save_host_forward` (ssh_control.rs:566-581) is an unlocked load → mutate → atomic write of the whole map. Its comment ("a concurrent update for a *different* host isn't lost") is false. Per-project poller threads and CLI processes write concurrently.
  - **What a lost entry costs.** `remote_hub_endpoint` falls back to the Unix socket (:711), which is unusable on Tailscale hosts (:463-465), and the next sweep may pick a different port under live workers.
  - **Ordering by convention.** The startup reconcile is race-free only because it runs before the pollers start (serve.rs:786-792).
  - **Same pattern elsewhere.** `machines.json` (machine_state.rs:98-111, with the same false comment) has this read-modify-write; impact there is a cosmetic re-resolve.
- **Recommendation: make explicit.** Take `acquire_file_lock` around both maps, and move the pin-clearing write out of `effective_forward` into the recheck or launch sites.

### M9. Workflow roles (primary active, gate pass/reject, Approve target) re-derived by heuristics in several places

- **Entity:** the role each status plays in a workflow.
- **Encoding:** 8, 5.
- **States:**
  - primary active (literal `in-progress`, else the first Active status);
  - agent gate (Active + `Owner::Agent` + not primary);
  - pass/reject target per gate;
  - Approve target (next status in declared order).
- **Derivations:** workflow.rs:217-220 (`forward_status`), 356-372, 379-386, 406-416, 437-463, 481-518.
- **Risk: medium.**
  - **Three copies of "primary active":** workflow.rs:379, cli commands/issue.rs:959 and orchestrator mutate/start.rs:688. Only a doc comment says they agree.
  - **Approve lands by order, not by edge.** Approve = "next status in statuses.yaml order" (orchestrator review_ui.rs:381, zen.rs:1461), so reordering the catalog moves where Approve lands, with no edge check.
  - **Pass/reject computed twice.** Once for the dispatch prompt (orchestrator workspace.rs:6079-6080) and again at verdict time (poller.rs:3592), so a workflow edit in between makes the gate agent's instructions disagree with what the poller does.
  - **A fan-out silently falls back.** `forward_agent_active_gate` returns `None` and the flow falls back to the handoff status with no diagnostic.
- **Recommendation: make explicit.** Resolve a `WorkflowTopology` once at load, make ambiguity a validation warning, and delete the copies. Pairs naturally with H2.

### M10. Project YAML written by three uncoordinated protocols (plus unlocked hub YAMLs)

- **Entity:** the project config (`projects/<p>.yaml`, or `<repo>/.shelbi/project.yaml` + `local.yaml`); also `~/.shelbi/config.yaml` and `keys.yaml`.
- **Encoding:** 3, 5.
- **Writers and their protocols:**
  - **(a) Settings plans** (workflow_config.rs:1281-1312 via config_write.rs:297-331): content-hash check, then run. Serialized only by the daemon's in-memory `issue_lock(project, "config:workflows")` (cli daemon/control.rs:1008), and unlocked locally when no daemon is running (app settings/store.rs:82/133).
  - **(b) `PoolConfig::save`** (pool_config.rs:195-235): list-equality check plus re-apply, under `lock_pool_config` (lib.rs:1124), which (a) never takes.
  - **(c) `set_orchestrator_runner`** (orchestrator_setup.rs:127-189): no check, no lock (read at :129, write at :189). Called from onboarding (app onboarding.rs:124).
  - **`migrate_default_workflow_to_task`** (lib.rs:618-640): unlocked load/edit/save.
  - **Hub files:** `update_user_config` (user_config.rs:821-852) and `update_keys_file` (keymap/edit.rs:157-167) patch fresh text but have no lock and no hash check.
- **Risk: medium.**
  - **Lost updates.** Any pair of these can lose an update silently (no `Changed` raised); e.g. a Settings `default_workflow` save landing between (c)'s read and write is overwritten.
  - **Rare windows.** Each window is milliseconds, but desktop multi-window and the daemon's AutoHeal pass widen it.
- **Recommendation: make explicit.** Use one serialization point for project YAML: a file lock every writer takes, or the daemon `SaveConfig` path. Move `set_orchestrator_runner` onto a `config_write::Plan`.

### M11. Linear-managed status catalog outlives the Linear connection

- **Entity:** the tracker backend vs `statuses.yaml` `managed_by`.
- **Encoding:** 3, 4.
- **States:**
  - (backend=linear, managed=linear): valid;
  - (backend=linear, managed=none): transient until the daemon syncs;
  - **(backend=fs/github, managed=linear): invalid but reachable.**
- **Transitions:**
  - `write_issue_tracker` (tracker_config.rs:30-58);
  - `sync_with_states` writes `managed_by: Linear` (linear_statuses.rs:171-175, 303-327);
  - disconnect/switch (orchestrator tracker_connect.rs:250-261) drops only the filter cache and never touches `statuses.yaml`.
- **Risk: medium.** After `shelbi tracker disconnect` or a switch to GitHub:
  - Settings keeps the catalog read-only (workflow_config.rs:555, 1362-1366).
  - Validation errors tell the user to run `shelbi tracker linear sync` (shelbi-core workflow.rs:729-738), which now refuses (linear_statuses.rs:388-396).
  - The only exit is hand-editing `statuses.yaml`.
  - The disconnect test (cli tracker.rs:954-962) never checks `statuses.yaml`.
- **Recommendation: make explicit.** Disconnecting clears `managed_by` (and offers to drop `external_id`), or "read-only" is derived from backend + `managed_by` together.

### M12. `linear.json`: one file, three owners, unlocked; status sync spans two files

- **Entity:** `projects/<p>/linear.json`, holding:
  - filter cache (`filters`, `resolved`, `resolved_at`);
  - sync stamps (`states_hash`, `states_checked_at`, a 300s throttle at linear_statuses.rs:62);
  - `retired_statuses`.
- **Encoding:** 3, 6.
- **Writers:** all read-modify-write with no lock:
  - `resolve_filters_cached` (linear_client/filters.rs:179-205), from every `LinearStore::ctx()` in every process;
  - `store_filter_cache` (:249), `invalidate_filter_cache` (:270);
  - `update_doc` (linear_statuses.rs:421-426), via `sync_with_states` (321-326), `refresh_if_changed` (373) and `remember_retired` (430);
  - cli daemon board.rs:150; orchestrator tracker_connect.rs:256-264.
- **Risk: medium.**
  - **Lost updates across processes.** A CLI/TUI filter-cache write races the daemon's sync stamp, and the loser's keys vanish.
  - **A corrupt file reads as empty** (filters.rs:213-222), so the next write drops every other key.
  - **Crash window between the two files.** `sync_with_states` writes `statuses.yaml` (319) before `retired_statuses` (321). A crash in between loses the retired ids for good: the re-sync sees no diff, so `remember_retired` never re-runs, and `plan_workflow_rewrites` (517-524) can no longer map them.
  - **Settings edits can be clobbered.** The daemon's catalog write has no conflict check, so a concurrent Settings category edit is lost.
- **Recommendation: document**, and take `acquire_file_lock` (as board_index.rs:375 does) around `update_doc` and the filter-cache writes. Write the retired list before the catalog.

### M13. Process-local board cache: background refresh vs write invalidation

- **Entity:** `CachedIssueStore` per-process open snapshot (`Entry`, issue_cache.rs:88-95), plus the per-project "refreshing" flag (103-112), mirrored to `board-snapshot.json`.
- **Encoding:** 1, 6.
- **States:** Cold; Warm (<20s TTL); Stale (TTL passed or marked); Refreshing.
- **Transitions:** `publish` (136-150), `seed_stale_from_disk` (157-173), `mark_stale` (178-187, from `invalidate` 389), `kick_refresh` thread (352-380).
- **Risk: medium-low.**
  - **A pre-write board can serve as fresh.** There is no generation counter: a refresh that started before a write and publishes after it is stamped `fetched_at = now` (147) and serves the pre-write board as Warm for a full TTL.
  - **The refresh flag can stick.** It is cleared only on the thread's normal path (374), not by a drop guard, so a panic leaves the process "refreshing" forever, permanently Stale.
  - **Two caches of the same board.** The snapshot coexists with `board-index.json` (board_index.rs:15-24).
- **Recommendation:** add a write-generation check before `publish`, and reset the flag with a drop guard.

### M14. In-repo config migration and config-mode resolution (two sources of truth)

- **Entity:** a project's config layout. States:
  - global;
  - half-migrated, in one of three ways: shared written; local written; dirs moved but the global yaml not retired;
  - migrated;
  - representable but invalid: split present while `config_mode` ≠ InRepo.
- **Encoding:** 3 (mode decided by which files exist), 5 (ordered actions).
- **Transitions:**
  - `plan_in_repo_migration` / `apply_migration_plan` (migrate.rs:184-330): WriteShared, WriteLocal (201-215), MoveConfigDir/File (216-233), RetireGlobalYaml (234-239); caller cli project.rs:89-118.
  - Mode is read two ways: "local.yaml exists" (lib.rs:1277-1282, 1356) vs the `config_mode` field (project_paths.rs `config_root`; lib.rs:649-656; core model.rs:1494).
- **Risk: medium** (window *suspected*, not reproduced).
  - **The migration can strand workflows.** Once `local.yaml` exists, any concurrent `load_project` (daemon poller, TUI) loads the split, and its load-time self-heal writes `<repo>/.shelbi/workflows/statuses.yaml` (lib.rs:1319-1322, 535-546), creating the MoveConfigDir destination. The rename then fails (migrate.rs:378-381), and later re-plans only warn "both exist" (219-221), leaving the user's workflows stranded in the global dir. If `default_workflow` is custom, `load_project` then fails validation (1324-1331).
  - **Loads write into the user's repo.**
- **Recommendation:** make migration exclusive with loads (lock, or stop the daemon), or move dirs before writing `local.yaml`; "split present" should imply InRepo. Document the ordering.

---

## LOW

### L1. Observed workspace pane state (`status.yaml`) and a triplicated badge rule

- **Entity:** one slot's live agent pane as last observed.
- **Encoding:** 3, 4, 8.
- **States:** `WorkspaceState` = Working/AwaitingInput/Blocked/Paused/Serving (workspace_status.rs:42-62), plus absent.
- **Writers:**
  - saves: orchestrator poller.rs:1929/2157/2396/2430;
  - clears: poller.rs:6790/7009 and review_ui.rs:531.
- **Risk:**
  - **The join rule is copied three times.** "Trust the status only if it names the current task; Blocked wins; else guess Working" lives in occupancy.rs:225 (`slot_reads_working`), orchestrator sidebar_model.rs:247 and tui app.rs:1642. CLAUDE.md asks for a single copy in shelbi-app.
  - **A clear can be undone.** The file has no lock and no ordering, so a poll that sampled before teardown can re-save after review_ui.rs:531 clears it. This is masked by the one-tick "guess Working".
- **Recommendation:** document, and use a single rule in shelbi-app. Stamp writes with the observed session id.

### L2. PR state/CI mirror (`pr-status.json`)

- **Entity:** a PR's `state`, `ci`, `ci_head` and `history_loaded` (pr_status.rs:31-48, 74).
- **Encoding:** 4, 3, 1.
- **Writers:**
  - `note_state` (172; orchestrator pr_record.rs:45/89);
  - `note_ci` (198), which forces `state = Open` at 207 (orchestrator zen.rs:1186);
  - `clear_ci` (224); `merge_listing` (114).
  - All are locked (163).
- **Risk:**
  - **A late CI note can resurrect a merged PR.** `note_ci` landing after `note_state(Merged)` flips the PR back to Open until the next sweep.
  - **Two sources of PR state.** `Issue.pr` also carries state and CI (core model.rs:2420-2440).
- **Recommendation:** `note_ci` must not downgrade Merged/Closed.

### L3. Board order maintenance (GitHub priority meta block, Linear `sortOrder`)

- **Encoding:** 4, 5 (N sequential writes).
- **Writers:**
  - GitHub `set_priority` (github_store.rs:1456-1498) loops `rewrite_priority` (2868), one GET + PATCH per card;
  - `renumber` (1658-1669);
  - Linear midpoint and respace (linear_store/mod.rs:1014-1031); `add` placement (861-863).
- **Risk:**
  - **A mid-loop failure leaves a torn column.** On Linear, cards after the failure jump to the top.
  - **Concurrent reorders interleave.**
  - **Linear `add` can duplicate.** It checks the id, then creates (856), so two racing adds make two issues.
  - **Past bug:** issue-add-top-ready-error (github_store.rs:1464-1477).
- **Recommendation:** document. Use fractional ranks if races show up.

### L4. Issue/ETag caches and on-disk write-through from every process

- **Entity:**
  - process-local id→number, negative-miss (600s) and number→(issue, ETag) caches (github_store.rs:2428-2505, `fetch` 2243-2320);
  - best-effort writes into `board-index.json` and `done-history.json` (`publish_write` 2539, `refresh_index_entry` 2516, `add` 1230).
- **Risk:**
  - **The module doc is stale.** It says "No content cache … the store holds no board state" (49-57).
  - **Past bug:** a 304 served a stale `assigned_to` (fixed 2276-2291).
  - **The negative cache can hide a new card.** A card created directly into a terminal status can be hidden for 10 minutes.
- **Recommendation:** document and fix the doc. The index write-through belongs with the board-index synthesis (cli-daemon).

### L5. Board index freshness lifecycle

- **Entity:** `board-index.json`: `stale`, `fetched_at`, `interval_secs`, identity, `last_cold_read`, plus the `board-refresh-error.json` episode sidecar.
- **Encoding:** 3, 6, 1.
- **States:** `BoardState` Cold/Warm/Stale (issue_store.rs:370-386); `BoardSource` (board_index.rs:649-666).
- **Writers:** all locked (`with_board_index_lock`, 403-417).
- **Risk:**
  - **Warm/stale is derived twice:** `board_state_from_index` (601-617) and `remote_board_report` (819-882).
  - **The "only a daemon index is ever Warm" rule is enforced by comment only** (862-873). It is a safety rule for destructive pollers.
  - **Write-through patches append cards out of order** until the next tick.
- **Recommendation:** fold the derivation into one `freshness(idx, cfg)`.

### L6. Linear status id derivation: two slug rules plus an unsynced fallback

- **States:**
  - synced: the catalog `external_id` lookup (linear_store/mapping.rs:55-57);
  - unsynced: the name fallback `mapping::slug` (26-41, Unicode);
  - the id the next sync assigns: `workflow_config::kebab` (ASCII, workflow_config.rs:1661-1671) with `-2` dedupe (linear_statuses.rs:150-158).
- **Risk:** a state used before the 300s sync (or with no daemon) reads under a different id after the sync for non-ASCII names or collisions. The poller then sees a phantom status change.
- **Recommendation:** use one slug function, and treat an unsynced state as unknown or trigger a sync.

### L7. Linear rate budget / park (`linear-budget/<hash>.json`)

- **States:** healthy/slow/paused/parked (`linear_tick_plan`, linear_client/budget.rs:216-238; explicit and tested).
- **Writers:** `record_headers` (268), `park` (280), `clear_park` (291, on any success, client.rs:230). All are unlocked read-modify-write across processes.
- **Risk:** a concurrent success clears a fresh park, costing a few rejected calls. Bounded by a one-hour cap (29).
- **Recommendation:** leave, or add a sibling lock.

### L8. Linear warm/cold delta reads keyed on the local clock

- **Mechanism:** the delta filters `updatedAt > since`, where `since` is the daemon's local pre-read clock (linear_store/mod.rs:700-713; cli daemon board.rs:295). The first `poll_changes` cursor falls back to `Utc::now()` on an empty board (1186-1189).
- **Risk:** clock skew skips edits until the next scheduled cold read (about 10 minutes).
- **Recommendation:** leave, and document the cold-read backstop.

### L9. `poll_changes` updated-at watermark

- **Mechanism:** GitHub (github_store.rs:1723-1788) and FS (issue_store.rs:1029-1056) both filter strictly greater than the watermark at one-second resolution, so same-second updates after a poll are dropped.
- **Callers:** no production caller of the GitHub version was found (only trait impls, issue_cache.rs:594, and tests).
- **Recommendation:** document it as unused, or delete it. If revived, track the ids already seen at the watermark second.

### L10. Hub daemon lifecycle (flock + socket + pid file)

- **States:** NotRunning/Starting/Running/Draining/Wedged, plus a stale pid record.
- **Transitions:** daemon_lifecycle.rs:56-463; ssh_control.rs:126-341. Mostly explicit: `decide_stop` 336, `should_escalate_stop` 375, and two identity predicates (ssh_control.rs:313/332).
- **Risk:**
  - **A flock error reads as "held"** (94-95), so a filesystem where flock fails never spawns a daemon (10s timeout).
  - **`stop_daemon_with` can report a stopped daemon that is still running.** It returns `Stopped{pid}` even if the process survives the post-SIGKILL wait (458-462).
  - **Two keys for one daemon.** The lock path follows `SHELBI_HUB_SOCK` while the pid path follows `SHELBI_HOME`.
- **Recommendation:** leave, and add a `StillRunning` outcome.

### L11. Orchestrator supervision bridge (status line + relaunch markers)

- **Encoding:** 3, 6 (60s mtime freshness). It mirrors the in-memory `SupervisionState` in the orchestrator poller.
- **Writers:**
  - status: poller.rs:5404/5423 (write), 5329/5372 and orchestrator lib.rs:615 (clear);
  - markers: orchestrator state_write.rs:58-62 (request) and supervision_relaunch.rs:92-106 (consume).
- **Risk:**
  - **The give-up decision is forgotten on restart.** The crash-loop budget and "gave up" latch are in memory only (supervision_relaunch.rs:3-8), so a daemon restart forgets them while the status file still says "gave up".
  - **A failed probe clears the failure.** `orchestrator_pane_alive(..).unwrap_or(true)` (poller.rs:5325) clears the user-visible failure on a probe error.
- **Recommendation:** document. Optionally persist the latch beside the status line. Related to H3: neither machine records orchestrator crashes durably.

### L12. Zen mode tri-state and the state.json enum forward-compat hole

- **States:** `ZenModeState` Off/Paused/On.
- **Transitions:** `set_zen_mode` (lib.rs:2762), `toggle_zen_mode` (2880). Callers: tui app.rs:1100, activity.rs:181, the cli zen commands.
- **Risk:**
  - **The event line can be lost.** It is appended best-effort after the commit (`let _ =` at 2769/2891), so disk and event feed can drift, and the TUI caches the mode in memory (app.rs:717).
  - **New enum variants break old binaries.** `deserialize_lenient` rejects unknown strings (1818), and `KanbanColumnOverride`/`MigrationState` are strict. A newer binary's new variant makes all of state.json unparseable for an older one: every `update_state` fails, and H4 then reads the project as closed. This breaks the `extra` forward-compat contract (1577-1598).
- **Recommendation:** map unknown variants to a safe default.

### L13. Workspace tmux→session cutover state (`workspace_migration`)

- **States:** absent, Pending, Migrated (migration.rs; lib.rs:1722-1739), plus the `migration_unreachable_machines` set.
- **Writers:** `set_workspace_migration_state` (migration.rs:85; orchestrator migration.rs:459/600, pool_admin.rs:858) and `set_migration_machine_unreachable` (110).
- **Risk:**
  - **"Absent" means opposite things.** The module doc and `workspace_migrated` (migration.rs:19-24, 136-140) say "absent = not migrated", while the real gate `ensure_workspace_dispatchable` (orchestrator migration.rs:609-629) treats absent as dispatchable. `workspace_migrated` has no callers: a dead predicate with the opposite meaning.
  - **Lost flips.** `set_migration_machine_unreachable` reads outside the lock (111-114).
- **Recommendation:** fix the docs and delete the predicate. The module is scheduled for removal (`rt-cutover-delete`).

### L14. Settings multi-file save plans and agent-rename parking

- **Plans (config_write.rs:331-370; plan order at workflow_config.rs:84-87, 1201-1336):** a plan runs op by op with no rollback and no "partially applied" state.
  - When a save both adds and removes catalog statuses, `statuses.yaml` is written last (1325). In between, workflows already reference the new status: `list_workflows` skips them (workflows.rs:331-343) and `load_workflow` errors.
- **Agent rename (agents_config.rs:1220-1233):**
  - Each rename is `agents/<from>` → `agents/.renaming-<from>` → `agents/<to>`, and `run` stops at the first failure (config_write.rs:181-183). A failure or crash leaves the agent parked under a dot-dir that `list_agents` hides (agent_workspaces.rs:677-679), so the agent disappears from Settings and dispatch.
  - Nothing sweeps `.renaming-*` (the only reference is agents_config.rs:1223).
  - The plan never checks that the park path is absent, so a leftover makes the next rename fail.
  - `workflows::list_known_agents` (workflows.rs:509-522) does *not* skip dot-dirs.
- **Recommendation:** detect and restore stray `.renaming-*` on read or in self-heal, add an absent-check for the park path, and split the catalog write into adds-before and removes-after.

### L15. Legacy `zen_toggle` migration runs on two paths, one lossy

- **States:**
  - legacy chord in config.yaml;
  - copied to keys.yaml with the legacy field reset to `alt-z`;
  - legacy field stripped.
- **Writers:**
  - load-time `migrate_legacy_zen_toggle` (keymap/loader.rs:1181-1199), run inside every `load_keymaps` (391) in any process;
  - the heal path `heal_legacy_zen_toggle` (966-995, from cli config_upgrade_apply.rs:1025).
- **Risk:**
  - **The load-time path is lossy.** It writes config.yaml through a full serde re-serialize (`save_user_config`, 1194-1196; user_config.rs:800-804), dropping comments and unknown keys, which contradicts `update_user_config`'s guarantee (806-820).
  - **The detector re-fires** until the heal strips the field, because `Keymap.zen_toggle` has no `skip_serializing_if` (user_config.rs:602-608).
  - **No lock** against Settings writes.
- **Recommendation:** retire the load-time path, and keep the heal path only.

### L16. Workflow name `default` aliased to `task` by file presence

- **Mechanism:** `resolve_workflow_alias` (workflows.rs:236-249) resolves on every read. `default.yaml` present means default; otherwise `task.yaml` if present; otherwise the built-in.
- **Risk:** `check_new_name` (workflow_config.rs:1389-1399) does not reserve `default`. Creating, duplicating or renaming a workflow to `default` silently re-routes every open `workflow: default` issue from task to the new workflow. The reverse direction is guarded (872-890).
- **Recommendation:** reserve the name while the alias is active.

### L17. Small multi-writer / identity issues (each one stands alone)

- **`$SLOT` recomputed at transition fire time.** It comes from declaration order (`Project::workspace_slot`, core model.rs:1415-1423; read at orchestrator transition.rs:438). A pool edit shifts implicit slots, so teardown can run with a different `$SLOT` (port) than the start. `validate_workspaces` (model.rs:1440-1470) never checks explicit-vs-implicit collisions.
- **`Issue.branch` has two indistinguishable modes** (model.rs:2512-2519: prefilled vs orchestrator-cut). A missing write-back falls back to a hard-coded `shelbi/<id>` (orchestrator transition.rs:442), which ignores a configured prefix.
- **Message delivery status is forgeable.** It is rebuilt from the log with `split_whitespace` (event_log.rs:1812-1822) instead of `split_event_words`, so quoted reminder or job text containing `message=<id> ack=worker` can forge Delivered.
- **"Launch in flight" ignores newer failures.** The reverse scan `continue`s past non-active statuses (event_log.rs:2915-2930) instead of stopping at the slot's newest dispatch line, so `message-channel` followed by `stalled` still reads as active for 60s and suppresses the stranded-slot resume.
- **Push provenance is last-line-wins** across projects. `push` lines carry no `project=` (event_log.rs:3220-3285), so same-named branches in two projects shadow each other. The caller fails safe by blocking.
- **events.log rotation can wedge.** The rotation/index/journal machine is explicit (`EventLogRotationFailure`, event_log.rs:3818; recovery 627-721), but its "ambiguous" states refuse every append hub-wide with no automatic exit (669-673, 694-701, 3721-3730). Doctor only prints "unknown".
- **Clarification questions** (questions.rs:141-178, 260-275):
  - open/answered only, with no obsolete state;
  - unlocked read-modify-write;
  - "answered" means the reply was pushed, not acked;
  - the poller's fixed `push-blocked` id can hide a recurring block behind an already-answered record.
- **Default-agent provenance** (state.json `deployed_agent_defaults` / `notified_diverged_agents`; agent_workspaces.rs:911-1133): files are written before state. The notice latch is per agent, not per divergence as documented (957-959 vs 1103-1105). Settings ▸ Reset (agents_config.rs:1112-1131) doesn't update provenance.
- **Zen pinned-PR flow record** (`ZenFlowPhase` Probed/Published/CiGreen, zen_flow.rs:24-45): unlocked load-check-save from separate processes. `CiGreen` with no `integration_sha` is representable. Fail-closed because `landing_pin` re-verifies live (orchestrator zen_flow.rs:150-224).
- **Changelog attachments** (changelog/attachments.rs:220-405): the frontmatter list is computed from a pre-copy read, so two concurrent attaches drop one record. A crash leaves orphan `assets/` files.
- **`--root` is not inherited by child processes** (*suspected*). `set_root_override` only fills a process-local `OnceLock` (root.rs:63-65; cli main.rs:511-516), and the daemon spawn adds no `--root` (daemon_lifecycle.rs:228-245). Whether `launcher.env` carries `SHELBI_ROOT` was not checked.

---

## Minor (trivial, or explicit and fine)

- **Explicit, fine:**
  - `SessionLostRecord`/`RecoveryState` (session_lost.rs:33-46; single owner cli daemon/session_health.rs:143-230);
  - `SessionHealthTracker` (core session_health.rs:120-187, one `observe`);
  - `PollerLock` (daemon_poller.rs:44-67);
  - hub version handshake (`classify_daemon_version`, hub_version.rs:180-205);
  - `SlotOccupancy` (occupancy.rs:29-43, derived, fail-safe Active);
  - `OwnershipGate` (ownership.rs:35-46);
  - orchestrator handoff claim (`handoff.md` → `.claimed` RAII, agent_workspaces.rs:456-549).
- **Explicit, fine (continued):**
  - `RetryPolicy`/`Disposition` and `classify` (gh_retry.rs);
  - `scan_meta_block` (`enum St`, github_store.rs:5365-5469);
  - unknown-write-outcome reconcile for creates (github_store.rs:1189-1220; Linear 921-946, 1259-1337);
  - `run_gh_raw` child deadline (3081-3185);
  - Linear `plan_catalog`/`diff_catalogs` (linear_statuses.rs:121-283);
  - `update_state`/`update_global_state` locked read-modify-write (lib.rs:2719, 2013);
  - one-shot latches `contextual_greeting_pending` and `first_run_seen`;
  - `kanban_column_overrides`;
  - task moves under tasks.lock (lib.rs:3552-4120; past bug F18 at 3628-3634);
  - `MigrationPlan` re-plan from disk;
  - pool conflict 3-way rule (pool_config.rs:19-27, 200-219);
  - workflow/agents content-hash conflict checks;
  - `keys.yml`→`keys.yaml` rename;
  - `in_test_binary` detector + canary;
  - `HandoffCause`, `CursorLiveness`, `ZenHeartbeatCue`;
  - `update_awareness` locked store;
  - `done_history` TTL cache (locked);
  - Linear `LinearError` classification;
  - issue migration resume derived from "target holds id" (issue_migrate.rs:103-118);
  - the review-load failure ledger with `gave_up` latch (lib.rs:3804-3870, documented, manual load recovers).
- **Trivial:**
  - `GRAPHQL_FALLBACK_REPOS` episode (logging only, github_store.rs:2032-2049);
  - warn-once sets;
  - `MALFORMED_META_WARNED`;
  - `PARSE_WARN_CACHE`;
  - token cache with 401 invalidation (github_store.rs:3013; issue_auth.rs:247-285: probe runs under the mutex, so a stalled keychain blocks resolvers up to about 18s);
  - Linear API key re-resolved per call (a `security` spawn each time);
  - Linear process caches (states 600s, viewer forever);
  - filter cache never expires;
  - TLS errors classified as Connect (replayable; creates carry client ids);
  - tracker config write-then-restore (tracker_config.rs:52-57);
  - FS comment sequence numbers (unlocked, single writer by convention, issue_store.rs:1100-1123);
  - `FileSystemStore::get` exists-then-load;
  - `EventKind::from_body` order-dependent classifier (event_log.rs:254-322).
- **Trivial (continued):**
  - error-log unread watermark (timestamp taken before the lock, error_log.rs:109/117);
  - hub-vs-external move discriminator (event_log.rs:862-889, rotation can double-emit, documented);
  - `change_bus` edge-vs-snapshot gap on subscribe;
  - hub `shelbi.yaml` unlocked read-modify-write, where a parse error resets it (hub_config.rs:44-66);
  - `claude_md_migration_hinted` (see H3);
  - zen demotions written after the move lands (zen_demotions.rs:26-58);
  - legacy workspace-state migration moves entries one by one, self-heals on re-sniff (workspace_state_migration.rs:153-169);
  - `DaemonLauncher` static;
  - `machines.json` (see M8);
  - onboarded predicate over four sources (orchestrator_setup.rs:87-107);
  - changelog template content-as-version;
  - `ChangelogStamp`;
  - tool detection `DETECTED` (refreshed by the TUI only, never by the desktop, tools.rs:594-615);
  - `ConfigMode` inferred from `local.yaml`;
  - workflow-generation inference from disk shape (workflows.rs:197-200, 302-308, 476-480);
  - scaffold exists-then-write;
  - `login_env` cache (success-only, 10s deadline per retry, no backoff, login_env.rs:28-131);
  - `Reachability::Unknown` conflates never-probed and in-flight (core model.rs:1770-1790);
  - `IssueTrackerConfig` doc says Linear "not yet implemented" (model.rs:802, stale);
  - `Agent.status` lifecycle enum with no production writer (model.rs:2144-2177; `save_agent` unused): dead;
  - `load_session`/`save_session` dead API;
  - `Workflow` parse → `resolve_against` uses one type for both stages (sentinel category, workflow.rs:1756-1763);
  - `Owner::Agent` accepted on Done/Archived statuses;
  - the `PoolConfig → Workspace → GitWorktree` lock order is documented only in comments (lib.rs:1098-1123).
- **Not state machines:** pure or data-only modules: yaml_edit, project_properties, keymap key/display/contexts/chord, pool.rs plan fns, agent_manifest clamp, scaffold, placeholders, duration, shell, home, file_limit, system_memory, workspace_names, integration, disk, test_support, build.rs, gh_requests (append-only log), core github.rs, awareness_variants (pure parser), resolve.rs (walk-up; identity note: returns the in-repo `name:` as the id, resolve.rs:297-303).

---

## Cross-slice leads

- **orch-core / orch-workspace:**
  - H1 literal-id readers (load.rs, review_session.rs, review_ui.rs, poller.rs, actions.rs, sidebar_model.rs); confirm how Linear `in-review` cards fare on review paths.
  - H2 agent move policy (poller.rs:3609, 3983).
  - M9 duplicate role resolvers (mutate/start.rs:688, workspace.rs:6079, poller.rs:3592, review_ui.rs:381, zen.rs:1461).
  - M2 marker clears (load.rs:899/939, mutate.rs:430).
  - M7 `stop_review_server` (workspace.rs:2331-2365).
  - L11 `SupervisionState` (poller.rs:5320-5425).
  - L2 PR mirror writers (pr_record.rs:45/61/89/125, zen.rs:1186).
  - L1 status.yaml writers (poller.rs:1929-2430, 6790/7009; review_ui.rs:531).
  - L17 `$SLOT` and branch fallback (transition.rs:438-442).
  - The dispatch status vocabulary (workspace.rs:3159/3378/3517/3630).
  - Zen flow (zen_flow.rs, zen.rs).
  - `tracker_connect.rs:250-266` (M11).
- **runtime / cli-daemon:**
  - **H3: who detects an orchestrator crash now** that the zen hooks are gone (cli zen_lifecycle.rs, main.rs:391-420; orchestrator lib.rs:1288-1292).
  - H4 open-set readers (cli daemon/serve.rs:393, poller.rs:69, board.rs:846, lifecycle.rs:323; session daemon_watchdog.rs:80).
  - M5/M4 cursor and follower consumers (cli orchestrator.rs:299-540, 929-953; orchestrator_awareness.rs; orchestrator wake.rs:3107).
  - Board refresh, merge and the reopen announce (cli daemon/board.rs:150-440, 710-766).
  - M8 forward sweep (shelbi-ssh lib.rs:1284-1900; cli serve.rs:786-793).
  - Config AutoHeal writes racing Settings saves (cli config_upgrade_apply.rs).
  - Daemon in-memory `issue_lock` for SaveConfig (daemon/control.rs:986-1022).
  - L17 `--root` propagation (session spawn.rs:119-126).
- **app / tui / desktop:**
  - L1 triplicated badge rule (occupancy.rs:225, orchestrator sidebar_model.rs:247, tui app.rs:1642).
  - H1 category fallback (app board.rs:89).
  - H2 human drag (app board.rs:205-230).
  - Onboarding step machine (app onboarding.rs:124-153) over unlocked M10 writers.
  - Settings store routing (app settings/store.rs).
  - L12 TUI cached zen mode (tui app.rs:717/1101).
  - Tool detection refresh asymmetry (tui shell/mod.rs:1818 vs desktop views/settings.rs:953).
  - Reachability prober (app reachability.rs).
  - The pending chord (app diff/state.rs:153/488/830).
  - The awareness job and reminder views.

## Coverage

All 107 `.rs` files in the slice were walked, split across seven readers by area, and every high finding and medium headline claim was re-checked by hand.

- **GitHub:** github_store.rs (production 1-5532), gh_retry, gh_budget, gh_requests, pr_status, core github.rs.
- **Issue stores and Linear:** issue_store, issue_cache, board_index, issue_migrate, issue_auth, tracker_config, linear_statuses (+tests), linear_store/* (+tests), linear_client/*. fake, issues and queries were grep-only: fixtures and GraphQL types.
- **Event log and awareness:** event_log.rs (all), awareness, awareness_variants, questions, error_log, change_bus, done_history.
- **Workspace and daemon state:** agent_workspaces, workspace_status, workspace_state_migration, occupancy, ownership, machine_state, session_lost, supervision_status, supervision_relaunch, daemon_lifecycle, daemon_poller, hub_version, hub_config, ssh_control, zen_flow, zen_demotions, zenmode.
- **lib.rs and project state:** lib.rs (all), root, resolve, project_paths, disk, test_support, migration, migrate, pr_template, build.rs, tests/in_test_binary.rs, tests/yaml_round_trip.rs.
- **shelbi-core:** all files (model.rs, workflow.rs read in production range; tests by name).
- **Config:** workflows, workflow_config (+tests), keymap/*, user_config, agents_config (+tests), config_write, yaml_edit (+tests), pool_config (+tests), project_properties, orchestrator_setup, tools, tool_paths, changelog (+attachments, +tests).
- **Test modules** were read by name, and in full only where they pin a state or a past bug.
