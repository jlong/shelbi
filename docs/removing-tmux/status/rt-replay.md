# rt-replay — In progress

Attach replay: the session serializes its emulator's **full** state into a
regenerated escape-sequence byte stream, so a freshly attached (or
backpressure-recovered) client rebuilds an emulator identical to the session's
on **both** screen buffers. Plan section "Attach replay"; approach proven in
`docs/removing-tmux/phase0/emulator-replay.md`.

## What lands

- **Serializer** (`shelbi-session`, `replay.rs`): reads the vendored
  `alacritty_terminal` fork's state accessors and emits a self-contained stream
  (RIS reset, then reconstruct): normal screen + scrollback, the alternate
  screen if active, both saved cursors, the scroll region, tab stops, charsets,
  all term modes, and both kitty keyboard-protocol stacks. Cells carry the full
  `Flags` + `CellExtra` (underline styles/color, strikeout, zero-width combining
  marks), not the spike's reduced set.
- **Rest-boundary output framing** (`output_split.rs`): the PTY reader splits
  the live stream only where the VT parser is at rest (never mid escape sequence
  or mid UTF-8). Each output frame therefore ends at Ground, so the session
  emulator's state always corresponds exactly to the frames already emitted, and
  the replay/live split a resuming client resumes at is always rest-aligned.
- **Gapless snapshot**: a global `output`-gate-before-`emu` lock order lets
  `attach` / `resync_base` read `(replay, resume_seq)` as one consistent pair,
  so no output is lost or duplicated across the replay-to-live boundary.
- **Wire**: the `Resync` ext frame now carries the replay **byte stream**
  (`replay: Vec<u8>`, binary-encoded `[seq][bytes]`) instead of a text snapshot.
  Both the initial `attach` and the backpressure drop-to-fresh-replay hook send
  it.

## Config-upgrade

No shipped `*.template` / default config changed (in-code protocol + a new
module), so no config-upgrade sniffer is needed.

## Tests

Real-PTY, in-process round-trip tests cover the spike cases (full-screen
reattach, screen-underneath-after-quit, keyboard-protocol survival, heavy-output
replay/live boundary, backpressure recovery). The vendored spike fixtures
(`nvim`, shell+nvim) are replayed through the production serializer.
