# rt-deflake-client-e2e

Status: **done** — `shelbi-client`'s `protocol_e2e` + `relay_e2e` now pass
40 runs in a row (two 20x batches) while a continuous all-core
`cargo build --workspace --release` loop pegs the machine, plus the normal
`cargo test` pass. Tests and test helpers only; no production change.

## Root causes (there were three, not just timeouts)

The tests were already condition-based (`wait_for`/`recv_until`), so bare
timeout bumps weren't the fix. Reproducing under a harsh all-core build loop
surfaced three distinct flakes:

1. **Resync replay raced the reader thread (the main relay flake).**
   `relay_connection_matches_the_local_api` and
   `relay_lists_and_bridges_three_sessions_at_once` attached *immediately* and
   then asserted the resync replay carried the child's marker (`HELLOINFO`,
   `AAA`/`BBB`/`CCC`). The reader thread feeds the child's first output into the
   emulator a moment after the socket appears, so a replay captured before that
   is legitimately empty — reproduced deterministically here. Fix: poll a
   snapshot for the marker *before* attaching, exactly as the local
   `attach_replays_…` test already did.

2. **ECONNREFUSED on a just-bound socket (the protocol flake under load).**
   The harness waited for the socket *file to exist*. `UnixListener::bind` runs
   `bind()` (creates the file) then `listen()`; the test thread can observe the
   file after `bind()` but before `listen()` completes, so a connect in that
   window is refused — and the window widens under load. Fix: `start`/
   `start_session` now wait until a real probe connect succeeds, so every later
   connect lands after the listener is accepting. (Same class as the production
   `rt-tui-attach-retry-unbound-socket` fix.)

3. **Echo split across Output frames.** The `echoback`/`bbb`/`first`/`second`/
   `COREOUT` checks inspected each `Output` event individually; a token's echo
   can split across two frames (PTY-master read or relay re-read on a byte
   boundary), so no single event matches. Fix: `recv_output_contains` accumulates
   across events and matches the running buffer (boundary-independent), returning
   the completing seq.

## Other hardening

- All positive/drain waits now use one generous `DEADLINE` (10s) constant; the
  negative waits (asserting something does *not* arrive) stay short on purpose.
- `concurrent_input_…`: the old collector demanded every byte and the parser
  false-broke on any non-frame byte. Under load two effects perturb the byte
  count without touching the invariant: a lagging observer may be dropped to a
  resync (correct backpressure), and `stty raw -echo` can momentarily lose the
  race so the tty cooks a few frames twice. Measured over ~240 runs: **0** false
  interleavings ever, clean-frame count always well above the floor. New
  `scan_frames` validates whole 32-byte frames, panics only on a delimited-but-
  mixed window (true interleave), skips perturbed fragments, and the test
  requires a healthy floor rather than the exact total.

## Production race on `main`?

No production code changed. The bind-before-listen window exists in
`shelbi-session` (remove-tmux) and is handled on the real attach path by
`rt-tui-attach-retry-unbound-socket`; this task only taught the test harness to
wait for accept. Nothing to port to `main`.

## Verification

- `cargo clippy -p shelbi-client --tests` clean; `cargo build` of the test
  binaries clean. No `Cargo.lock` change, so no MSRV check needed.
- Two 20x loops of `protocol_e2e` + `relay_e2e` (binaries invoked directly,
  `--test-threads=4`) under a continuous `cargo build --workspace --release`
  loop: 0 failures. Plus a 100x isolated `concurrent_input_…` sample: 0 false
  interleavings.
