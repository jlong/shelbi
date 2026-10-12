# rt-deflake-shelbi-tui-a-live-pane-that-exits-is-auto-restarted

Status: done — the flaky `shell::session::tests::a_live_pane_that_exits_is_auto_restarted` is fixed; `cargo test -p shelbi-tui --lib` passes 30/30 under a parallel `cargo build --workspace`.

## Root cause

The assertion read the shared relaunch log the instant `poll_at` returned. But
`poll_at` → `start_connect` only *spawns* the connect worker; the worker runs
the relaunch step (`ConnectJob::run` → `relauncher.relaunch(..)`) a beat later
on its own thread. When the worker hadn't been scheduled yet, `calls` was still
`[]` and `assert_eq!(calls, vec![false])` panicked at `session.rs:1919`. Pure
test race — the production path is correct. The sibling
`reopening_a_given_up_pane_resets_the_budget_and_relaunches` already waited in a
loop for the call; this test did not.

## Fix

- `a_live_pane_that_exits_is_auto_restarted`: wait (up to 5s, polling) for the
  relaunch call to land before snapshotting and asserting `== vec![false]`. The
  `MainState::Restarting(1, _)` assertion is unchanged — it's set synchronously
  in `start_connect`, so it never raced.
- Hardened the shared session-spawn / go-live deadlines that starve under load
  (same "deadline too short" class), each returns the instant its condition is
  met so healthy runs are unaffected:
  - `session.rs`: `spawn_exiting_session` + `spawn_cat_session` socket wait
    5s→30s; `poll_until_live` 5s→30s; `wait_until_exited` 6s→30s.
  - `pty_input_tests.rs`: the three `sock.exists()` session-socket waits 5s→30s
    (one of these, in `a_copy_chord_with_a_selection_is_not_forwarded_to_the_agent`,
    flaked under extreme load during this work).

## Siblings checked

- `a_deliberately_closed_pane_is_not_restarted`: safe. A deliberate close stands
  down without spawning a relaunch worker, so `calls` stays empty — no async read
  race.
- `reopening_a_given_up_pane_resets_the_budget_and_relaunches`: already
  condition-waits for the reopen call; left as is.
- Give-up-after-5 behaviour lives in the deterministic `gave_up_state()` helper
  (injected clock), not a wall-clock race.

## Verification

- `cargo build -p shelbi-tui --tests` + `cargo clippy -p shelbi-tui
  --all-targets -- -D warnings`: clean.
- `cargo test -p shelbi-tui --lib` 30 consecutive runs with a parallel
  `cargo build --workspace` loop: **30 passed, 0 failed.** The target test itself
  also survived 30/30 under a deliberately pathological load (continuous
  touch+full-workspace rebuild pinning every core).

## Notes

- Tests only; no production code changed. No `Cargo.lock` change, so no MSRV run
  needed.
- Under the pathological touch+rebuild hammer (far harsher than one parallel
  build), two *unrelated* tests with deliberate `assert!(elapsed < 1s)`
  anti-hang upper bounds can still trip
  (`review::tests::a_refusing_content_socket_ends_in_failed_not_connecting_forever`
  and the handshake-timeout test). Those assert a give-up happens *near* a small
  internal deadline, not forever; they only break when indefinite full-core
  saturation starves thread scheduling. Weakening those bounds would blunt their
  purpose, so they were left alone — out of scope for this deflake.
