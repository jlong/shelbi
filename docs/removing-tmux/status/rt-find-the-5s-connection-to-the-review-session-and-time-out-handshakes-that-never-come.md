# rt-find-the-5s-connection-to-the-review-session — In review

Follow-up to `rt-review-session-wedges-after-repeated-attaches` (#1570, write
timeout + hello-probe liveness). This closes the two remaining parts of that
root cause: the ~5 s caller that leaked a handler per connect against the review
slot, and the server having no deadline for the hello handshake.

## The ~5 s caller (identified)

The per-workspace poll samples `backend().snapshot(&host, &addr)` every
`workspace_poll_interval` (~5 s) for the pane-stall detectors
(`poller.rs:~1647`). On the session-process backend that is
`SessionProcessBackend::snapshot` → `shelbi_client::snapshot()` →
`Connection::open()`. A new `Connection` opens the socket **and starts a reader
thread**; the session, in turn, spawns a reader+writer handler and holds the
connection's three fds. The poll completes the hello, issues one `snapshot`
request, and drops the `Connection`.

The leak was that dropping a `Connection` wound nothing down. Its reader thread
holds a clone of the shared write half, so the write half's own teardown can't
run while the reader lives — and the reader is parked on `read` of a channel the
drop never closed. So each poll left the client reader thread **and** the
session's handler (two threads + three fds) alive forever. One per ~5 s tick is
exactly the observed cadence and the "172 threads / ~250 fds in ~7 minutes"
growth, hitting the launchd 256-fd `maxfiles` ceiling.

**Why only the review slot in the capture:** the snapshot poll runs for every
*local board workspace with a live session*. In that window the review slot was
the only one — a fresh workspace, no dev workers running — and a confirmed
serving review slot is polled continuously. The orch / diff / editor sessions
are not board workspaces the poller samples (the orchestrator's own session, and
the daemon-spawned review-panel PTYs the TUI attaches to), so they never went
through this path and stayed flat. The same leak would hit any continuously
polled dev slot; the review slot was simply the one kept alive.

## Fix

- **Client, non-leaking teardown (`shelbi-client`).** `Connection` now carries a
  `ShutdownHandle` from the transport seam and fires it on `Drop`, then joins the
  reader. The handle closes the channel (local: `shutdown(Both)` on a third
  socket handle; relay: `StreamState::close`), so the reader's parked `read`
  returns EOF and the thread exits, and the session sees the close and winds its
  handler down. This fixes every throwaway connection — `snapshot`, `info`,
  `title`, `send_*`, liveness probes — not just the snapshot poll. (Making the
  caller non-leaking, per the task's option; no cadence or reuse change needed.)
- **Server, hello deadline (`shelbi-session`).** `read_loop` bounds the pre-hello
  phase by `HELLO_TIMEOUT` (5 s, env-overridable via `SHELBI_HELLO_TIMEOUT_MS`
  for tests) against an absolute deadline; a peer that never produces a decodable
  hello in the window is dropped and both handler threads exit. The bound is
  cleared the instant the hello arrives, so a handshaken-but-idle client blocks
  normally. `set_read_timeout` is best-effort: on macOS it returns EINVAL once
  the peer has closed, which must not abandon buffered frames (e.g. a bare
  `Kill`), so its error is ignored.

## Tests

- `shelbi-session` `bare_connect_and_drop_probes_do_not_leak` — 200 no-hello
  connect-and-drop probes (bounded batches), fd + thread counts flat.
- `shelbi-session` `connect_and_hang_probes_do_not_leak` — 200 no-hello probes
  held open past the (shortened) window; the session closes each on its own
  clock (client read → EOF while it still holds its end), counts flat.
- `shelbi-client` `dropping_a_throwaway_connection_leaks_nothing` — 200
  open/`info`/drop cycles (the poll shape) leave the fd count flat. Verified
  meaningful: with the `Drop` teardown disabled it leaks base=34 → after=1034.

## Live verification

`shelbi-client` `live_review_poll_soak_stays_flat` (`#[ignore]`, timed soak)
drives the real poll shape (open `Connection` → `snapshot` + `info` → drop)
against a real session at the 5 s cadence for 36 polls (~3 min), the automated
stand-in for the 30-minute TUI check. Measured, perfectly flat the whole run:

```
SOAK baseline: fds=9 threads=7
SOAK t=0s:   fds=9 threads=7
SOAK t=30s:  fds=9 threads=7
SOAK t=60s:  fds=9 threads=7
SOAK t=90s:  fds=9 threads=7
SOAK t=120s: fds=9 threads=7
SOAK t=150s: fds=9 threads=7
SOAK final:  fds=9 threads=7
```

(Before the fix this climbs ~5 fds + ~2 threads per poll.)

## Rework (2026-10-07): Linux CI leak-test failure

CI run 37568523816 (Linux, PR #1575) failed `connect_and_hang_probes_do_not_leak`
(`base=16, after=54`) while macOS passed.

**Diagnosis: the test settled too early, not a real Linux leak.** Each connection
is two threads (reader `serve_client` + writer `client_writer`); both *return*
from their functions on the hang path, dropping the connection's three sockets as
they go — so the fd count falls back the instant the handlers finish. The fd
assertion already polled (`wait_for`) for that, but the thread assertion took a
single `thread_count()` sample. Linux reclaims a returned thread's
`/proc/self/task` entry *asynchronously*, a beat after the function returns and
after the client has already seen EOF, so a sample taken the moment the last
probe's fds settle still counts handler threads that have returned but aren't yet
reaped. The flat fd count is the proof the threads had returned (they'd dropped
their sockets): an actual thread leak would hold its socket and leak fds too.

**Reproduced on Linux** (`docker run ... rust:latest`, the exact command the
rework suggested):
- Unfixed HEAD, 15 runs: **15/15 failed**, always the thread assertion, both
  tests — `connect_and_hang base=9 after=17..39`, `connect_and_drop base=7
  after=17`. Docker's scheduler makes the teardown lag even more pronounced than
  CI, so the single-sample race fires nearly every run. fd assertions never
  tripped — only the un-polled thread sample.
- Fixed, 10 runs: **10/10 passed**.

**Fix (test only; no production code change).** Added `settled_thread_count(base,
slack, timeout)` — polls the thread count down to `base + slack` the same way the
fd check polls, returning `None` on platforms without `/proc/self/task` so the
assertion stays skipped off-Linux exactly as before. Both leak tests now wait on
it instead of sampling once; the bound is unchanged (`base + 4`), so a genuine
leak never settles and still trips the assertion after the 10 s deadline. Also
wrapped `bare_connect_and_drop`'s final fd check in `wait_for` to match
`connect_and_hang` (per the rework's "make the drop test robust the same way").

Left `discovery::tests::a_live_session_that_answers_the_hello_is_usable` alone
(alpha is fixing that flake separately).

## Notes

- `Transport::split` now returns a third element (the `ShutdownHandle`); the one
  other caller (`relay_e2e`) and both transport impls are updated.
- No new/bumped dependency, so `Cargo.lock` is untouched (no MSRV re-check).
- No shipped default/template changed, so no config-upgrade sniffer applies.
