# rw-remote-two-hop

Phase P9 of the remote-window plan (ContextStore `Shelbi`,
`Plans/remote-session-window.md`, section 5.2): a workspace the far project
runs on a third machine streams into the native remote window through the far
hub. This Mac has no route (and maybe no credentials) to that machine.

## What landed

- **Relay `session` service** (`shelbi_proto::RelayService::Session {
  project, target }`, target `SessionTarget`, the wire twin of
  `shelbi_app::session::SessionRef` via `SessionRef::to_target` /
  `from_target`). No relay protocol bump: a relay that doesn't know the
  service answers `OpenError` and keeps the channel, and the native window
  only opens on the exact same version anyway.
- **Far side.** `shelbi relay` injects
  `shelbi_orchestrator::session_connect::relay_opener` as
  `RelayServices::session`. It runs `session_connect::open_transport`, the
  raw twin of `session_connect::connect`: the same `remote_lookup` (the
  agent under its dispatch name `w-<ws>/ws/agent`), then a stream through
  the far process's own relay cache to that machine
  (`remote_session::open_stream`, never signing in: nobody to ask on a
  non-interactive channel), or the session socket on the far machine
  itself. The relay resolves it on a worker thread (an SSH start never
  stalls the channel or its keepalive), registers a `Link::Bridge`, answers
  `Opened`, and forwards the session's frames down whole
  (`forward_to_hub`, now generic over the read half). The window's
  `Connection` does its own hello and attach end to end, so replay and
  resync work as for any pane.
- **Errors** ride `OpenError` as JSON (`SessionOpenError::to_wire` /
  `from_wire`; a plain string reads as `Failed`): `Unreachable` (either the
  far side's relay to the machine won't start or died), `NoSession { exited
  }`, `Failed`.
- **Window side** (`shelbi_app::session::remote`). A workspace-scoped target
  that misses on the far host's list (idle, or a content session still
  starting) asks the far source for the workspace's details; when it
  `is_remote`, `attach_through` opens the `session` service and connects
  over it. Classified like a far-local attach: unreachable keeps
  reconnecting with the shared "Reconnecting to devbox… (attempt N)"
  notice, the reason naming the third machine ("`gpu-box` is
  unreachable"); no agent session is idle on its own machine; a content
  session not registered yet is starting; an exited one is a message. The
  client's `open_service` gives a `session` stream session backpressure
  (drop to a replay), not a service's close.
- **Reconnect.** Either hop dropping ends the stream (the far bridge closes
  when its stream to the third machine EOFs; the window's stream closes with
  its channel), and the pane reconnects like any other. The far relay
  replaces its dead relay to the third machine on the next open.
- **No new forwards here.** This Mac's relay `ssh` is unchanged (still pinned
  by `the_relay_link_carries_no_forward_and_no_shared_connection`); the far
  side's relay to the third machine, and its `-R`, are the far machine's.
- `remote_window::two_hop_placeholder` is gone.

## Tests

- `crates/shelbi-proto`: `services_have_a_pinned_wire_shape` (the `session`
  shape), `a_session_open_error_round_trips_through_the_open_error_string`.
- `crates/shelbi-client/tests/relay_services_e2e.rs`:
  `session_bridges_a_second_hop_both_ways_and_closes_with_it` (two chained
  relays), `a_session_that_cant_be_opened_says_why_and_keeps_the_channel`.
- `crates/shelbi-orchestrator/tests/remote_sessions.rs`, with a fake third
  machine behind the far side's `remote_session` seam whose sessions live in
  their own directory: `a_workspace_on_another_far_machine_streams_through_the_far_side`,
  `a_two_hop_pane_reconnects_when_the_far_side_s_hop_drops`,
  `a_two_hop_pane_reconnects_when_this_machine_s_hop_drops`,
  `a_two_hop_workspace_with_no_session_is_idle_on_its_own_machine`.

## Not in this phase

- No UI change beyond the pane: the workspace's sidebar row and details came
  from the far source already.
