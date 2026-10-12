# rt-review-session-alive-but-not-listening-and-the-tui-sits-on-connecting-instead-of-failing

Status: **ready for review**

Root cause and fix for a review that strands on "Connecting to review…".

- **Root cause (session):** `spawn_accept_thread` broke its accept loop and
  dropped the `UnixListener` on *any* `accept()` error. A transient error
  (EINTR, ECONNABORTED, or fd exhaustion EMFILE/ENFILE) therefore killed the
  listener permanently while the process kept running — the "alive but not
  listening" zombie (lock held, socket refusing). Fixed: transient errors are
  logged and retried (brief backoff on fd exhaustion); a genuinely fatal
  listener error now sets `TERMINATE` so the session tears down and *exits*,
  releasing its lock. "Keep the listener, or exit."
- **Liveness (client + backend):** added `shelbi_client::probe_socket` /
  `DiscoveredSession::{socket_refusing,usable}`: a lock-held session whose
  socket refuses a bare connect is a zombie and not usable. Wired into the
  `SessionProcessBackend` supervision probes (`probe`, `enumerate_slots`) only
  — they run on the poller cadence. So a zombie review (or dev) slot reads as
  not-alive and `maybe_resume_stranded_review_slots` / the slot resume path
  relaunch a fresh, working agent. Kept `find_live` / `live_session_names`
  lock-based so the per-keystroke `send_*` / per-tick `title` paths don't gain
  a second probe connect.
- **TUI:** the give-up was already bounded in `SessionManager` (retry to the
  `RetryPolicy` deadline, then `Failed`). Added a test that drives the real
  `ReviewInterface` content path against a refusing socket and asserts it
  settles in `Failed` (never perpetual `Connecting`), with re-selecting Chat
  re-arming the connect.

Tests: new unit tests in `shelbi-client` (probe/usable), `shelbi-session`
(accept-error classifier), and `shelbi-tui` (review content against a refusing
socket). None touch the real HOME / `~/.shelbi` — tempdir sockets and injected
connectors throughout. `cargo build` + `cargo clippy` clean on the four crates;
no `Cargo.lock` change (no MSRV check needed).
