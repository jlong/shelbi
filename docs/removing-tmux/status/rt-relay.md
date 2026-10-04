# rt-relay — In review

`shelbi relay` and the remote transport in `shelbi-client`: one stdio channel
per machine that bridges to every session socket there (plan sections "Remote
machines" and Phase 5).

## What landed

- **Relay envelope** (`shelbi-proto`, `relay.rs`): an additive, independent
  multiplexing layer, `[len][type][payload]` with its own type space.
  `Hello`, `ListSessions`/`SessionList` (discovery), `Open`/`Opened`/`OpenError`,
  `Data { stream, bytes }` (raw), `Close`, and channel-level `Ping`/`Pong`. The
  session frames ride inside `Data` **unchanged**, so the frozen core is
  untouched. Plus `frame_boundary()` in `frame.rs`: the length of the next whole
  frame from the length prefix alone, so the relay peels frames to forward
  without decoding (and never chokes on an additive frame a newer session emits).
- **Transport seam** (`shelbi-client`, `transport.rs`): `Transport` splits a
  channel into boxed read/write halves; `LocalTransport` is the Unix socket.
  `Connection::connect(Box<dyn Transport>, …)` is the one path local and remote
  share; `Connection::open`/`handshake` now go through it. `reader.rs` and the
  write half are boxed (`SharedWrite`), so the same API drives either.
- **Hub side** (`relay.rs`, `RelayChannel` + `RelayStream`): demuxes the channel
  into per-session logical streams, each a `Transport`. `list_sessions()`,
  `open(short_id)`, `is_reachable()`. Channel keepalive turns silence past the
  deadline into **unreachable** (never "dead"). Per-stream bounded inbound queue
  with **drop-to-replay**: on overflow the demux drops the queue and re-`attach`es
  (answered by the session's `resync`), never blocking the channel reader — so one
  slow session's consumer can't stall another.
- **Remote side** (`relay.rs`, `serve_relay`): the body of `shelbi relay`. Answers
  discovery from the sessions root, connects a session socket per `Open`, and
  forwards whole frames both ways unchanged (session→hub split by
  `frame_boundary`). Holds no session state. `shelbi relay` CLI command
  (`shelbi-cli`, hidden) wires stdin/stdout to it.

## Decisions

- **Backpressure lives on the hub-side demux**, not the relay. On a shared
  channel the only place one session's consumer can lag independently is the hub
  fan-out; the demux applies the session's own bounded-buffer + drop-to-replay
  rule there and never blocks. The relay stays stateless, and the "replay" reuses
  the session's existing `attach`→`resync` (no relay-synthesized snapshot).
- **Relay forwards whole frames** session→hub (peeled by `frame_boundary`) so a
  hub-side drop stays frame-aligned; hub→session is written straight through (the
  session reassembles).
- **Binary-path resolution and the SSH wiring are out of scope** (`rt-machine-setup`
  / `rt-remote-spawn`). `shelbi-client` stays dependency-light and tokio-free:
  `RelayChannel::new` takes the channel's read/write halves, so the hub wires an
  `ssh <host> <bin> relay` child's piped stdio to it. The remote binary path
  defaults to `shelbi` on PATH there.

## Tests

`shelbi-client/tests/relay_e2e.rs` drives real `shelbi_session::run` sessions
through `serve_relay` over an in-process pipe (no SSH): three sessions bridged on
one channel + discovery; the full client API (attach/input/resize/snapshot/kill)
over the relay; relay killed mid-stream and replaced, client resuming by
sequence number; a silent channel flipped to unreachable within the keepalive
deadline; one slow stream dropped-to-replay without stalling another on the same
channel; and a mock frozen-core session reached through the relay. Plus
`shelbi-proto` round-trip/boundary unit tests.

## Rebase onto rt-replay

Rebased onto `jlong/remove-tmux` after `rt-daemon-cancellation`, `rt-term`,
`rt-snapshot` and `rt-replay` landed. The only functional touch was adapting to
rt-replay's `Resync`: it now carries `replay: Vec<u8>` (a regenerated
escape-sequence byte stream, binary-encoded `[seq: u64 BE][replay bytes]`)
instead of `screen: String`. The relay treats session frames as opaque — the
drop-to-replay path's `is_resync` check only reads the frame **type** byte, so
the binary body passes through untouched, and the re-`attach`→`resync` now hands
the client a real replay stream. `relay_e2e` asserts on the replay bytes
(matching `protocol_e2e`); no `Resync.screen` references remain.

## Config-upgrade note

No shipped `*.template` / default config changed (all new behavior is in-code
protocol + a new hidden command), so no config-upgrade sniffer is needed.
