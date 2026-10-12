# rt-protocol-client — In review

The full session protocol over the session socket, plus the `shelbi-client`
crate that discovers, spawns, and connects to sessions (plan sections "The
protocol", "Compatibility with old sessions", "Shared client crates").

## What landed

- **Additive-capability wire types** (`shelbi-proto`, `ext.rs`): `info`,
  `paste`, `set-meta`, `detach`, in-band `resized` (sequenced), the pushed
  `event-title` / `event-bell` / `event-resized`, backpressure `resync`, and
  keepalive `ping`/`pong`. All take type bytes `>= CAPABILITY_BASE`; a unified
  `decode_any` reads core or ext off one stream. Frozen core untouched.
  Capability names + `ALL` list updated (added `output-resized`, `resync`,
  `keepalive`).
- **Full protocol server** (`shelbi-session`, `transport.rs` + `session.rs`):
  hello announces the session's capabilities; every request handled; title/bell
  captured via a custom emulator event sink and pushed as events; the
  frozen-core `exited` event pushed on child death.
  - **Sequenced output + ordered resize.** One output gate hands out sequence
    numbers for output chunks *and* in-band `resized` markers and broadcasts
    them while held, so the two travel in one totally ordered stream.
  - **Backpressure.** Each client has a bounded outbox; on overflow its queue is
    dropped and it is flagged for a fresh `resync` snapshot (or disconnected if
    it did not negotiate it) — the PTY reader never blocks and memory stays
    bounded.
  - **Input arbitration.** Every input/paste write is whole, under the PTY
    writer mutex; concurrent clients never interleave mid-frame.
  - **Sizing.** The PTY follows the most recently active client (last to send
    input), debounced (`RESIZE_DEBOUNCE`) so a window drag settles into one
    child resize. Emits in-band `resized` + out-of-band `event-resized`.
  - **Keepalive.** Periodic `ping` to connected clients; symmetric `pong` reply.
- **`shelbi-client`**: `discovery::list`/`reap_dead` (scan the sessions root,
  liveness via the session lifetime lock, reap dead directories), `spawn`
  (delegates to `shelbi_session::spawn_detached`), and `Connection` (hello
  handshake, blocking request API, a reader thread delivering output/events over
  channels, keepalive auto-pong). **No tokio** (`cargo tree -i tokio` is empty).
  Capability-gated with frozen-core fallbacks (`paste` → raw `Input`, `detach` →
  no-op; `info`/`set-meta` error when unsupported).

## Compatibility & attach replay

- The frozen core is unchanged; everything new is an additive capability a
  client uses only when the session announced it. There is no "session too old"
  state. (A CI matrix of current-client-vs-old-session binaries is a follow-up
  the plan calls for; the seam is in place.)
- Attach **replay** is still a full-screen text snapshot (the `Resync`
  stand-in), used for both the initial attach and backpressure recovery. Full
  emulator-state reconstruction is `rt-replay`; both hooks are clearly marked.

## Tests

`shelbi-client/tests/protocol_e2e.rs` drives a real in-process
`shelbi_session::run` over a Unix socket with a `/bin/sh` child, covering every
acceptance criterion: all requests/events end to end, sequenced output with
ordered `resized`, a slow client dropped to a `resync` without stalling the PTY
or other clients, non-interleaved concurrent input, most-recently-active sizing
(debounced), capability gating + fallback, keepalive, and discovery reaping.
Plus `shelbi-proto` ext round-trips and `shelbi-session` emulator title/bell/mode
unit tests.

## Config-upgrade note

No shipped `*.template` / default config changed (all new behavior is in-code
protocol and a new crate), so no config-upgrade sniffer is needed.
