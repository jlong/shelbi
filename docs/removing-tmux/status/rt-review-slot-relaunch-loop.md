# rt-review-slot-relaunch-loop

Follow-up to #1571 / PR #1575 (the fd-leak fix, merged as `7e0f663e`). This task
hardens the supervision machinery so that even a session that briefly stops
answering does not get killed and relaunched in an endless loop.

## What changed

### 1. Hysteresis before resume / reap (criterion 1)

A single `Wedged`/"not listening" probe is no longer enough to act:

- **Resume** (`maybe_resume_stranded_review_slots` and
  `maybe_resume_stranded_dev_slots`): each pass now carries a `dead_streak` and
  only resumes after `CONSECUTIVE_DEAD_BEFORE_RESUME` (= 2) **consecutive**
  confirmed-dead polls. Any alive reading, or a dispatch-in-flight tick, breaks
  the streak.
- **Reap** (`SessionProcessBackend::reap_zombies_with_hysteresis`, wired through
  `Backend::reap_zombie_duplicates` with a per-slot streak map held in the
  per-workspace poll loop): a zombie is SIGTERMed only after `REAP_WEDGED_THRESHOLD`
  (= 2) consecutive observations as a not-listening zombie. A session that drops
  off the candidate list (recovered, or already reaped) is forgotten so the
  count only ever reflects an unbroken run.

At the 5s poll cadence this is a ~10s confirming window — long enough to rule
out a one-tick GC/scheduler hiccup (or the fd-pressure moment just before a real
wedge), short enough to still act promptly on a genuine death.

### 2. Repeated resumes count toward the crash-loop cap (criterion 2)

The dispatch-grace latch in the review resume pass used to call `note_alive`,
which laundered a just-issued resume (whose own dispatch lines make
`recent_dispatch_active` true) out of the crash history — so the cap never
tripped. It now calls `note_dispatch_in_flight`, which stands the tick down and
breaks the dead-streak **without** clearing the restart history. A fast,
genuine resume loop therefore accumulates toward `MAX_RESTARTS_IN_WINDOW` and
surfaces `supervision=gave-up reason=review-load-crash-loop` instead of looping
silently. Genuine recovery (the slot staying alive past `STABLE_RECOVERY`) still
clears the history via `note_alive`, unchanged.

### 3. Resume/reap reasons distinguish wedged from crash (criterion 3)

- Reap already logged `reap-wedged` vs `reap-zombie`; unchanged.
- Resume now labels its `events.log` line `reason=wedged` (process up, socket
  silent) vs `reason=restart/crash` (process gone), computed via the new
  `Backend::slot_wedged` before the resume respawns the session.

### 4. RLIMIT_NOFILE raised; EMFILE logged (criterion 4)

- New `shelbi_core::raise_file_limit()` raises the soft `RLIMIT_NOFILE` toward
  8192 (clamped to the hard limit), called at the top of `shelbi __session`'s
  `run()` and the hub daemon's `run_foreground()`. Best-effort; a failure is
  logged, never fatal.
- The session accept loop now logs the `EMFILE`/`ENFILE` backoff instead of
  spinning silently.

### 5. Dead session dirs pruned automatically (criterion 5)

- New `shelbi_client::reap_dead_keeping_recent(root, keep_per_name)` keeps the
  N most-recent dead dirs per logical name (for a `final.txt` read) and prunes
  older duplicates; a live session is never touched or counted.
- Wired into the poller's project-wide supervisor tick on a slow cadence
  (`SESSION_PRUNE_CADENCE` = 60s, `KEEP_DEAD_SESSION_DIRS_PER_NAME` = 2), so the
  sessions dir no longer fills with exited predecessors.
- Because the prune deliberately keeps a couple of dead dirs per name (for their
  `final.txt`), a workspace selector still matches the live session plus those
  predecessors. `resolve_index` (`session_cli.rs`) now breaks that tie toward the
  single *live* session, so `shelbi session snapshot <ws>` resolves to the
  running session instead of erroring "matches several sessions". Covered by
  `live_session_wins_over_same_named_dead_predecessors`.

### 6. `title()` polling is flat (criterion 6)

`SessionProcessBackend::title()` opens a short-lived `Connection`, requests
`Info`, and drops it; #1575's `Connection::drop` fires the shutdown handle and
joins the reader thread, so the per-connection handler, descriptors, and thread
are all released. Verified two ways:

- `repeated_title_polls_leave_the_fd_count_flat` (new, in
  `session_process_backend.rs`) drives the real `title()` against a genuine
  in-process session 50 times and asserts the process fd count stays flat.
- #1575's `dropping_a_throwaway_connection_leaks_nothing` (in `shelbi-client`'s
  `protocol_e2e`) already exercises the identical open/`info()`/drop lifecycle
  200 times at the client level, with a matching `live_review_poll_soak_stays_flat`
  soak. The `title()` test confirms the backend wrapper around that lifecycle
  leaks nothing either.

### 7. Live 30-minute soak (criterion 7)

Not runnable in this sandbox (no live hub). The loop's root cause (the fd leak)
is fixed by #1575, which is in the base; the changes here are the belt-and-
suspenders that keep a future transient wedge from re-creating the loop. This
should be verified on the hub by the reviewer/operator per the acceptance note.

## Base

Branch cut from `jlong/remove-tmux`, which contains the umbrella foundation
(`crates/shelbi-proto`, `docs/removing-tmux/README.md`) and #1575.
