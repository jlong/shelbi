# Implicit state machines in Shelbi: summary and triage

Survey run 2026-11-18 against `main` at `e8145b3f`. Nine research tasks each spidered one slice of the codebase (about 610k lines of Rust) against a shared definition; an agent then merged the slices into one catalog and re-verified every high-risk claim and claimed bug against the code. I (the orchestrator) re-checked the two most serious bugs myself and wrote this triage.

**Docs in this folder (space `Shelbi`):**
- `README.md`: this page. Triage and the short list.
- `catalog.md`: the consolidated catalog. 45 entries (C1–C45) with states, failure modes, files, verification, and recommendations; the 59 confirmed bugs table; 8 cross-cutting patterns; a low/leave-alone appendix.
- `definition.md`: what counted as an implicit state machine and the rubric.
- `findings/<slice>.md`: the nine raw slice reports (orch-core, orch-workspace, state-core, cli-daemon, runtime, app, tui, desktop-views, desktop-shell). Read these for line-level detail.

**Headline numbers:** about 180 raw findings, merged into 45 machines (17 high, 25 medium, 3 low). 59 confirmed live bugs (50 re-verified, 5 partly, 4 worker-only). Nothing checked turned out false; four entries were downgraded high → medium on verification (C18, C34, C35, C36).

## The one-paragraph diagnosis

Shelbi's riskiest machines share one shape: a multi-step side effect (merge, kill, spawn, remote write) whose only journal is "a marker file is still there", re-implemented by several callers with different failure policies, and observed by several processes that each re-derive the state from files, the tracker and pane liveness. When a step fails or the process dies between steps, the early steps repeat or the late ones are silently lost. Three things make it worse: read errors are often converted into defaults that drive destructive actions (kill, relaunch); some state lost its producer in the tmux-to-session migration while readers kept trusting it; and the TUI and desktop each build their own copy of client machines that belong in shelbi-app.

## Triage

### A. Fix now (bugs; no design needed)

Small, local fixes. Suggested grouping into tasks:

1. **Reaper kills live agents** (#6, #7, #8). A workflow that fails to load reads as "no active tasks", so every live dev pane in it is reaped (`orch/poller.rs:7104-7114`); the launch-age grace never fires and `shelbi open` shells look like agents because session metadata is a no-op (`orch/session_process_backend.rs:377-401`). Hold on `Unknown`; store launch time and user-shell mark somewhere real. Verified by the orchestrator.
2. **User data loss** (#27, #44). A diff comment whose save failed vanishes with its warning (`app/diff/session.rs:402-412, 509, 545-551`); GitHub `move_status` drops labels past the tenth (`state/github_store.rs:1322, 1364, 4201`).
3. **Status semantics quick fixes** (#13, #14, #15, #16, #17). Canceled dependencies block forever; custom Done/Archived lanes (including synced Linear closed lanes) read as open; GitHub writes a custom Done status as an open issue; deleting the last workflow edge in Settings silently flips the workflow to legacy mode.
4. **Handoff ledger** (#3, #4, #5). Push retry cap is a no-op when the tip is unreadable; markers cleared before the old agent is killed; refusal sidecar never cleared.
5. **Linear/tracker config** (#46 and C24). Disconnecting Linear leaves the status catalog read-only forever; `linear.json` has three owners and no lock. Fresh code from #2333, cheapest to fix now.
6. **Zen and palette** (#48, #19). Palette says "Turn Zen off" while Paused and turns it On.
7. **Client UI fixes** (#24–26, #28–40, #51–58). Sticky diff error banner, stuck "Loading", stuck "Sending", stale optimistic card moves, review open yanking the view back, settings save queue bugs, dead desktop host watch, error log marked read on failure, duplicated Activity rows, terminal search drift, window not restored on a New tab.
8. **Odds and ends** (#2, #9–12, #20–23, #41–43, #45, #47, #49, #50, #59). See the bug table in `catalog.md`.

### B. Needs further thought (design work)

These are the items where a fix is a design decision, not a patch. Ordered by leverage.

**B1. One transition engine with an in-flight journal** (C1, C2, C30; pattern P1)
Every status move that has side effects (gated merge → status write → event → Zen bookkeeping → branch/pane cleanup) is implemented five times (CLI/TUI move, review Approve, ready-marker handoff, transition marker, Zen GitHub reconcile), plus `issue resume`, `action apply-transition` and the file-backend `$EDITOR` path, which bypass it. None records progress: a crash or failure after the irreversible merge either rolls the card back over a merged PR or loses the cleanup forever. Proposal: one `apply_transition(edge, cause)` owning the sequence and one failure policy, plus a persisted `{edge, merged_sha?, phase}` record so a crash resumes at the right step; the ready-marker handoff becomes its poller-side caller. Start with the persisted cleanup-pending record, which closes the crash windows before every caller is ported.
Files: `orch/transition_move.rs:300-511`, `orch/transition.rs:188-371`, `orch/review_ui.rs:371-470`, `orch/poller.rs:2992-3480, 3642-3930`, `orch/zen.rs:1380-1560`, `cli/commands/issue.rs:1149-1332`.

**B2. One owner for each workspace slot** (C3, C4, C5, C6, C31; patterns P3, P5, P7)
"Who owns this slot and what is it doing" is answered by the board, an assignment overlay, ready/review markers, `status.yaml`, an in-memory `last_known`, two supervision latches on two threads, and a 64 KB grep of `events.log` that is the only thing preventing a double launch. Read errors fall to the destructive side (kill, relaunch). Proposal: one persisted `SlotState` per workspace (idle / launching{lease} / working{task} / handing-off / serving{task} / recovering{attempt}) with one transition function owned by one thread, a durable launch lease replacing the log grep and serving as launch age, and `Unknown` always meaning "hold". The review slot keeps one `ReviewSlotState{task, phase}` and the agent-written marker is retired.
Files: `orch/poller.rs:4940-5000, 5093-5440, 5632-5726, 6108-6400, 6485-6800, 6930-7116`, `orch/workspace.rs:2105-2290, 2624-3600`, `orch/mutate/start.rs:51-643`, `app/supervision.rs:83-245`, `state/event_log.rs:2836-2931`, `orch/load.rs:38-940`.

**B3. Status semantics come from the workflow, not literal ids** (C8, C9)
Large parts of the code ask "is this done / open / review / terminal?" by comparing against the stock ids (`done`, `review`, `todo`) instead of the workflow's categories, and move policy is encoded by whether `transitions` is `None`, empty or non-empty. That was tolerable with the stock workflow; with Linear-synced custom statuses (live as of #2333) it now produces real bugs (A3). Proposal: a workflow topology object (categories, roles: primary active, gate pass/reject, approve target; explicit `MovePolicy` enum) that every call site takes, and a ban on literal status-id comparisons outside it.
Files: `crates/shelbi-core/src/model.rs:2205-2350, 2605-2615`, `crates/shelbi-core/src/workflow.rs:80-91, 217-570`, `state/workflow_config.rs:994-1094`, `state/issue_store.rs:460-470`, `state/github_store.rs:5107`, `orch/zen.rs:15200-15800`.

**B4. Orchestrator liveness and Zen crash recovery** (C10, C29; pattern P7) — needs a product decision
The Zen crash-recovery hooks (`__zen-orch-start`, `__zen-heartbeat`, `__zen-orch-exit`, `__orch-record-exit`) lost their only caller in the tmux removal. Zen is never auto-disabled after an orchestrator crash, no crash record is written, and the bootstrap "crash-recovery flag" the orchestrator instructions rely on can never be set. Decision needed first: should Zen still auto-disable when the orchestrator dies? Then either wire liveness from the session supervisor (which already knows) or delete the hooks, the comment and the prompt text. The Zen toggle itself is a tri-state collapsed to a bool behind a relative flip (palette bug #48).
Files: `state/lib.rs:1601-1649, 2496-2640, 2762-2893`, `cli/commands/zen_lifecycle.rs:38-168`, `cli/main.rs:391-420`, `orch/lib.rs:492-703, 1288-1292`, `orch/poller.rs:376-377, 5320-5425`.

**B5. Session state as one classified record, local and remote** (C7, C32, C40; also C18)
A session's state lives in about eight files in its directory (lock, sock, meta, exiting, final, exit.json, kill-reason) and clients re-derive it with four predicates; the remote path sees only `alive` over the relay, so remote zombies are Alive forever and Quit counts refusals as stopped. Proposal: the session host owns one classified state (starting / live / exiting{reason} / exited{code, sender} / zombie), written atomically, and the relay ships that classification instead of a bool. This also fixes the EOF-vs-Exited misreads in the relay stream.
Files: `session/session.rs:432-604, 923-951`, `client/discovery.rs:159-490`, `orch/session_process_backend.rs:161-720`, `orch/remote_session.rs:373-720`, `client/relay.rs:118-1072`, `orch/quit.rs:631-640`.

**B6. A client async-operation primitive in shelbi-app** (C13, C14, C15, C16, C37, C38, C40; patterns P4, P8)
Both clients apply async answers without checking what the user did since (navigated away, re-toggled, switched project, started another save): review open yanks the view back, saves land out of order, optimistic card moves pin cards to stale columns, dialogs close while a save they started is still running. The TUI and desktop solve the same problems with opposite designs (overlay vs none) and have drifted. Proposal: build `Ticket`/generation tokens and a `SaveSlot` (idle / saving{ticket} / queued / failed{err}) once in shelbi-app, move the optimistic-move model and the review-open flow there, then port the call sites. Settings (C16) is the natural first consumer.
Files: `app/diff/session.rs`, `app/settings.rs:370-560` + `app/settings/*`, `tui/kanban.rs:162-283, 874-885, 1640-1840`, `desktop/views/board_state.rs:248-640, 1771-2180`, `app/review.rs:590-1006`, `tui/shell/mod.rs:2318-2360, 4830-5300`, `desktop/views/review.rs:336-1375`.

**B7. Event delivery: one cursor, or retire v1** (C19, C20) — needs a decision
v1 claim/ack, v2 stream/hooks and `events drain` share one cursor without sharing the delivery lock; v2 hooks write the cursor before printing (acked but unseen if the hook dies); the follower registry SIGTERMs a recorded pid with no identity check; the Codex bridge keeps its own queue with unbounded replay. Decision: is v1 delivery still supported? If not, deleting it removes most of this entry.
Files: `state/event_log.rs:1201-1560`, `cli/commands/orchestrator.rs:290-960`, `cli/commands/orchestrator_awareness.rs:150-520`, `orch/wake.rs:367-3108`.

**B8. Remote host state** (C12, C17, C34, C35) — smaller, worth one design pass
Per-host SSH forward/ControlMaster mode, the desktop host link, `machines.json` binary records and per-window bootstrap each track host health separately, with unlocked whole-map writes and a daemon-start sweep that forgets the sticky TCP decision after every reboot (so Tailscale hosts re-wedge). Worst case is silent loss of worker→hub messaging for a whole host. Proposal: one per-host record with a lock and one owner in the daemon; clients subscribe instead of probing.
Files: `crates/shelbi-ssh/src/lib.rs:237-320, 551-700, 1150-1993`, `state/ssh_control.rs:540-720`, `desktop/app.rs:1804-2267`, `app/launch/link.rs:281-364`, `state/machine_state.rs:69-111`.

### C. Document or leave alone

Everything else in `catalog.md` (C11 open set beyond bug #20, C21 jobs, C22–C23 tracker mirror, C25–C28 config/new-project/Zen PR flow, C33 messages, C36 window mode, C39 terminal view, C41–C45, and the appendix): local, bounded, or already failing closed. Fix the listed bugs; add a doc comment naming the states where the catalog says "document".

## The 45 machines at a glance

| ID | Machine | Risk | Triage |
|---|---|---|---|
| C1 | Workflow edge application and status moves | high | B1 |
| C2 | Handoff markers, sidecars and push ledger | high | A4, B1 |
| C3 | Dev orphan reaper and slot occupancy | high | A1, B2 |
| C4 | Stranded-slot recovery and supervision | high | B2 |
| C5 | Dispatch launch lifecycle | high | B2 |
| C6 | Review slot lifecycle | high | B2 |
| C7 | Session host on-disk lifecycle and name resolution | high | B5 |
| C8 | Status semantics keyed on literal ids | high | A3, B3 |
| C9 | Workflow move policy and status roles | high | A3, B3 |
| C10 | Orchestrator liveness and Zen crash recovery | high | B4 |
| C11 | Project open set | high | A8 (#20, #21) |
| C12 | SSH reverse forward and ControlMaster | high | B8 |
| C13 | Diff session load and comment save | high | A2, A7, B6 |
| C14 | Client optimistic card moves | high | A7, B6 |
| C15 | Client main-area navigation and review open | high | A7, B6 |
| C16 | Settings save/load and the leave guard | high | A7, B6 |
| C17 | Desktop remote host lifecycle | high | A7, B8 |
| C18 | Hub daemon lifecycle, reload, resurrection | medium | B5 (adjacent), A8 |
| C19 | Event-delivery cursor and follower registry | medium | B7 |
| C20 | Codex bridge delivery | medium | B7 |
| C21 | Job and reminder lifecycle | medium | C |
| C22 | GitHub tracker write path | medium | A2, C |
| C23 | Tracker read mirror and reconcile | medium | C |
| C24 | Tracker connection and Linear config files | medium | A5 |
| C25 | Project and hub config writers | medium | C |
| C26 | Config-upgrade finding lifecycle | medium | A8 (#47) |
| C27 | New-project creation flow | medium | C |
| C28 | Zen pinned-PR flow | medium | C |
| C29 | Zen mode toggle | medium | A6, B4 |
| C30 | Git worktree and branch state, deferred deletes | medium | B1 |
| C31 | Observed pane state and usage-limit auto-resume | medium | B2 |
| C32 | Relay channel and session stream resync | medium | B5 |
| C33 | Message push/ack and clarification questions | medium | A8 |
| C34 | Remote machine binary record | medium | B8 |
| C35 | Desktop connection bootstrap | medium | B8 |
| C36 | Desktop window content mode, tabs, slots | medium | A7 (#57) |
| C37 | Client async ops without request identity | medium | A7, B6 |
| C38 | Activity feed and title cache | medium | A7 |
| C39 | Terminal view state | medium | A7 (#56) |
| C40 | Client session pane lifecycle | medium | B5, B6 |
| C41 | Desktop notification tracker | medium | A7 |
| C42 | Desktop file-promise drops | medium | C |
| C43 | Handoff notes (HANDOFF.md, handoff.md) | low | C |
| C44 | Event-log status vocabulary from display strings | low | C |
| C45 | CI smoke and release scripts on sleeps | low | C |
