# dt-terminal-bottom-clipped: a pane always reaches the session's last row

## The bug

The orchestrator pane sometimes stopped partway through the live output:
Claude Code's spinner, input prompt and status line sat below the pane, and
neither scrolling nor the jump-to-bottom button reached them. It happened
while the TUI sidebar was attached to the same session.

## Root cause

Lead 1 of the task, both halves:

1. **The clip anchored at the top.** The session's PTY takes the size of the
   most recently active client (the last one to send input). When that was a
   taller client (the TUI in a big Ghostty window), the session's screen had
   more rows than the desktop pane, and `shelbi_term::viewport::fit` clipped it
   from the top-left: the pane drew the screen's first rows and dropped the
   rest. Those are live rows, not scrollback, so no scroll position reached
   them; scrolled to the bottom, the pane's last row was the session's row
   `pane rows - 1`. The regression test reproduced exactly that (a 300-row
   session in a 63-row pane showed row 62 at the bottom).
2. **Looking at the pane never took the size back.** The desktop only
   reported its size when its own size changed; the session applies a
   report only from the active client, and only input made a client active.
   So the clip lasted until the user typed in the desktop pane. That is the
   "sometimes": it depended on which client had typed last.

Lead 2 (rows rounded up from the pane height) is not the cause: the grid is
`floor(height / line height)` (`Size::from_pixels`), and a test now checks,
across fractional window sizes, that every row ends inside the pane and the
pane inside the window. Lead 3 (a stale scroll clamp) is not either:
alacritty clamps the display offset to its own history.

## The fix

- **Clip to the content's bottom** (`shelbi-term`). `viewport::fit_showing`
  ends a clipped window at the lowest live-screen row that holds anything
  (text, a non-default background, reverse video, or the cursor), and never
  starts it below a shown cursor. A fresh shell with its prompt at the top
  still shows the top; an agent drawing to the bottom shows its prompt and
  status line. `TerminalView::placement` picks it (and skips the screen scan
  when nothing clips); `Placement::session_to_viewer` is the inverse mapping
  renderers paint through. `viewport::fit` stays as the plain top-left
  anchor.
- **Every client uses it**: the desktop element (row cache, cursor,
  selection, search and link highlights, the IME anchor, the accessibility
  row bounds), the TUI's terminal view, and `shelbi attach`. Pointer mapping
  already went through `Placement::viewer_to_session`, which honors the
  window's start.
- **Taking focus claims the size.** An empty `input` frame makes its sender
  the active client without writing to the PTY; every session binary
  already works that way, so no capability or protocol bump is needed
  (`Connection::claim_size`, locked in by a session integration test). The
  desktop pane sends it when it gains focus or its window becomes active
  (after flushing a size still waiting out the resize debounce), and on a
  reattach while focused. The TUI and `shelbi attach` send it on
  `FocusGained`. So the PTY follows whichever window the user is in, as
  tmux's `window-size latest` does, and the clip lasts only while the user
  is looking at an unfocused window.

## Tests

- `shelbi-term`: `viewport` placement tests (bottom anchor, short content
  keeps the top, cursor stays in view, clamping, letterbox unchanged,
  inverse mapping), `emulator` content-bottom tests, and a `view` test that
  scrolls a clipped window up and back.
- `shelbi-session`: `an_empty_input_claims_the_pty_size` (two real clients
  against a real PTY).
- TUI and `shelbi attach`: a clipped taller session renders its bottom rows,
  and a click on the last row reaches the session's last row.
- Desktop: `a_taller_active_client_still_shows_the_sessions_bottom_rows`,
  `after_a_resize_every_row_fits_inside_the_pane`,
  `focusing_the_pane_claims_the_session_size` (session view harness), the
  row cache with a window start, the highlight spans with a window start, and
  a `--visual-check` frame (`taller-session.png`) with the cursor, a
  selection and a search match on a clipped window.

## Known limits

- While clipped, the oldest `src_start` rows of history are past the top of
  the scroll range (the window's top line is `src_start - offset`, and the
  offset stops at the history length), and search reveals a match by the
  full screen's rows, so a match in the clipped-off top rows can stay out of
  view. Both last only until the pane is focused, which ends the clip.
- No live run against a sandboxed daemon with the real app and TUI: the
  gpui harness, the offscreen visual check and the real-PTY session test
  cover the three pieces.
