# rt-deflake-session-integration-a-silent-client-does-not-block-another-clients-hello

Done. `crates/shelbi-session/tests/session_integration.rs` is now deterministic
under load. **Tests only; no production change** — the handler architecture
already answers a fresh hello independently of a stuck client; the test was at
fault.

## The race(s)

The reported panic at `session_integration.rs:133` was the **socket-appearance
wait** in `RunningSession::start`, not the body of the test it was attributed to.
`run()` binds the socket only after opening the PTY, spawning the child, and
starting three background threads; under parallel-build load that cold startup
can exceed the old 5 s bound. The same too-tight pattern was spread across the
file (child-output waits, fd/thread settling, 3 s frame reads).

Fixing those exposed a **second, independent flake** the original report never
named: `bare_connect_and_drop_probes_do_not_leak` (and the sibling hang test)
fire connects in rapid bursts; on a CPU-saturated host the accept thread is
starved long enough for the listen backlog to fill, and macOS then returns
`ECONNREFUSED` to new connects against a perfectly healthy session. The old
`connect()` did `.expect("connect")` and panicked. This is a test artifact of the
burst (the test's own comment already notes connects can "outrun the session's
accept-and-reap"), not the behavior under test.

## Changes (test file only)

- Two shared, generous constants replace the scattered ad-hoc bounds:
  - `SETTLE` (30 s) for any one-time condition polled to completion: socket bind,
    child writing a file, fd/thread settling, run-thread join. A longer deadline
    never weakens these — a real hang/leak never satisfies the condition and still
    trips the assertion at the deadline.
  - `FRAME_READ` (15 s) for an expected protocol frame; applied to both
    `connect()`'s read timeout and `read_frame`'s deadline (previously disagreeing
    at 3 s each).
- `connect()` now retries transient `ConnectionRefused`/`NotFound` up to `SETTLE`
  instead of panicking; a genuinely dead listener keeps refusing and still fails.
- Kill backstops factored into one `send_kill()` helper that retries a transient
  refusal briefly but returns at once on a missing socket (session already exited)
  so teardown never hangs.
- `a_silent_client_does_not_block_another_clients_hello` keeps its timing proof,
  unchanged in intent. The hello read uses the generous `FRAME_READ` window so a
  slow reply is read and *measured*; the elapsed bound moved 1 s -> 1.5 s: far
  above the millisecond-scale healthy path (two thread wake-ups) even on a loaded
  host, still with clear margin below the 2 s `CLIENT_WRITE_TIMEOUT` a regression
  (broadcast writes serialized under a held registry lock) would stall it by.
- Left the two 300 ms setup sleeps (wedging the silent client; giving the child a
  beat before asserting the raw log is *absent*): their only failure mode is a
  false pass on a pathologically slow host, never a false fail.

## Verification

- `cargo build -p shelbi-session --tests` — clean.
- `cargo clippy -p shelbi-session --tests -- -D warnings` — clean.
- `cargo test -p shelbi-session --test session_integration`, 30 runs in a row,
  with a parallel `cargo build --workspace` (separate `CARGO_TARGET_DIR`)
  saturating the host (18 full workspace rebuilds ran concurrently during the
  loop): **30 passed, 0 failed.**
  - For contrast, the same 30-run loop **before** the `connect()` hardening (but
    after the timeout widening) was 21 pass / 9 fail, every failure an
    `ECONNREFUSED` from `connect()` under the burst — not the `a_silent_client`
    test.
- `Cargo.lock` unchanged, so no MSRV step needed.
