# rt-snapshot — snapshot matches `capture-pane -p -J`

**Status:** ready for review.

The session `snapshot` render now reproduces `tmux capture-pane -p -J` in shape:
wrapped lines are joined (the one behavior the previous render was missing),
trailing whitespace is normalized per line, and history-bearing snapshots line
up with `capture-pane -S -N`. A dead session's snapshot is read from `final.txt`
via `shelbi_client::snapshot`, replacing the tmux `capture-pane` tail used for
crash records.

Tests:

- `shelbi-session` `tests/capture_parity.rs` — feeds identical byte streams to a
  real tmux pane and to the emulator and compares, across the detector fixture
  shapes (input box, spinner, dialog, usage-limit banner, wide chars, wrapping)
  plus a scrollback (`-S -N`) case. Skipped when tmux is absent.
- `shelbi-orchestrator` `tests/detector_snapshot_parity.rs` — runs the
  `ready.rs` / `submit.rs` detectors against emulator snapshots and asserts they
  read identically to the raw fixtures.
- `shelbi-client` `snapshot` unit tests — dead-session `final.txt` read.

Note: `-J` *preserves* trailing spaces; the emulator trims them, and exact byte
parity is unreachable (tmux and this emulator track a line's used width
differently after erase-to-end-of-line). The parity test normalizes trailing
whitespace on both sides; the detectors are trailing-whitespace insensitive.
