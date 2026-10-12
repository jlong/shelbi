# rt-review-session-wedges-after-repeated-attaches-leaks-threads-and-sockets-closes-every-new-connection

**Status:** complete.

A review session wedged after ~35 minutes of repeated attach/detach: 172 threads
and 250 open socket fds, connects succeeding but getting EOF without a reply, and
supervision leaving it in place. Two independent fixes.

## Session side: a stuck client can no longer wedge its handler

- Root cause: `client_writer` (`crates/shelbi-session/src/transport.rs`) did a
  blocking `write_all` with **no write timeout**. A client that keeps its socket
  open but stops reading fills the kernel buffers, so the write blocks forever;
  `serve_client`'s unconditional `writer.join()` then blocks forever too, leaking
  the reader+writer thread pair and the connection's three fds — one set per
  abandoned attach, until the process runs out of descriptors and `accept()`
  starts failing `EMFILE` (new connects dropped).
- Fix: `sock.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT))` (2s) on the writer
  socket. A write that makes **no** progress for the bound errors out of
  `write_all`, which the loop already treats as a dead client: break, close,
  shut down the read half, wind the handler down and release its fds. A merely
  *slow* client still drains a little per write and is never dropped here — the
  bounded `Outbox` + `Resync` recovery handle backlog, unchanged. Per-client
  writes already hold no shared lock while blocking (the batch is drained under
  `ch.out`, which is released before the write), so one stuck client never blocks
  another or the broadcast.
- Refactored `client_writer` to take a `resync_base` closure instead of
  `Arc<Shared>` so the loop is unit-testable in isolation.

## Supervision probe: a real hello round-trip, not a bare connect

- The #1560 probe (`probe_socket`) only checked that `connect()` succeeded, so a
  wedged session (accepts connections, never answers) read as reachable and was
  never replaced.
- New `shelbi_client::connect::probe_handshake` (a throwaway hello round-trip,
  no reader thread) and `discovery::probe_reachable` (connect + hello, bounded by
  `HELLO_PROBE_TIMEOUT` = 2s). New `SocketReachability::Wedged`. `usable()` now
  requires a hello answer; `not_listening()` covers both `Refusing` and `Wedged`
  (replacing `socket_refusing()`); `choose_session`'s duplicate probe and
  `zombies_to_reap` use the round-trip so a wedged session is excluded/reaped
  like a refusing one.
- `zombies_to_reap` now returns each reaped session paired with its
  `SocketReachability`; the backend maps it to a supervision action so the poller
  logs `supervision=reap-wedged` (accepts-but-silent) vs `supervision=reap-zombie`
  (refusing). A lone wedged session is handled by the existing flow: `usable()`
  false -> `probe` Dead -> relaunch -> next tick reaps the old one by pid.

## Tests

- `transport::tests`: a writer to a never-reading client winds down within the
  timeout (shrinks socket buffers via `setsockopt` so the block is deterministic;
  fails/hangs without the fix); a clean close winds it down at once.
- `session_integration`: 200 attach/detach cycles leave the fd count flat; a
  silent client does not block a second client's hello.
- `discovery::tests`: an accept-but-silent session probes `Wedged` and is not
  usable; `zombies_to_reap` reaps a stale wedged sibling and reports it wedged.
  Existing "reachable" stand-ins upgraded to answer the hello. All tests use a
  temp `SHELBI_HOME`.

No `Cargo.lock` change (no new deps). `cargo build` / `cargo clippy
--all-targets -D warnings` green on the three crates; the new tests pass.
