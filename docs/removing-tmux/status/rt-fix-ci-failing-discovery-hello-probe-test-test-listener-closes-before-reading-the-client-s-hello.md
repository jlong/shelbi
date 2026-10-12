# rt-fix-ci-failing-discovery-hello-probe-test — Ready for review

Fix the flaky `discovery::tests::a_live_session_that_answers_the_hello_is_usable`
test (added in #1570) that failed 3 runs in a row on CI (run 37568353674, panic
at `discovery.rs:484`) while passing locally on macOS.

## What landed

- **Test-only fix in `crates/shelbi-client/src/discovery.rs`.** The
  `hello_listener` stand-in replied with a `Hello` frame and then **dropped the
  stream without reading the client's hello**. On Linux, closing a Unix stream
  socket that still holds unread data resets the connection (RST), which can
  discard the reply in flight or fail the probe's own write — so
  `probe_reachable` classified the listener `Wedged` and `s.usable()` was false.
  macOS is more forgiving, which is why reply-then-drop passed locally.
- The helper now serves each connection the way a real session does, on its own
  per-connection thread (`serve_hello`): **read the client's hello first**,
  reply with our `Hello`, then **stay open draining to EOF until the client
  closes** (the probe drops its stream right after reading the reply). The close
  is now always driven by the client, never by us dropping unread data.

## No production change

The probe (`probe_handshake` / `probe_reachable` / `read_session_hello`) is
unchanged. It already handles a peer that closes right after replying: once a
valid `Hello` is decoded it returns `Reachable`, and only a timeout or an
EOF-*before*-the-hello maps to `Wedged`. The bug was entirely in the test
listener's connection lifetime, so no production behavior needed to change.

## Other listeners checked

Per the AC, I scanned every test listener in `shelbi-client`:
- `connect.rs` `a_socket_that_accepts_but_never_answers_times_out` is
  deliberately silent (the wedge case) — correct as-is.
- `tests/protocol_e2e.rs` and `tests/relay_e2e.rs` listeners already **read the
  client's hello before replying** (match `Frame::Hello(_)` then write). No
  reply-then-drop offenders beyond `hello_listener`.

## Verification

- `cargo build -p shelbi-client` + `cargo clippy -p shelbi-client --all-targets
  -- -D warnings` clean.
- macOS (Darwin 25.6, arm64): discovery tests 30 runs in a row — all pass.
- Linux (`rust:1.88-bookworm` in Docker): discovery tests 30 runs in a row — all
  pass.
- No `Cargo.lock` change, so no MSRV concern.
