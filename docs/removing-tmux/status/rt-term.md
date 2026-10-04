# rt-term — In review

Built `shelbi-term`: everything about *showing* a session that is not tied to a
UI toolkit (plan, "Shared client crates"). The ratatui TUI, the attach client,
and the later gpui app all render over this crate, so its public API carries no
ratatui/crossterm/gpui type.

## What landed

- **Client-side emulator** (`emulator::TermEmulator`). Drives the vendored
  `alacritty_terminal` fork headless with the kitty keyboard protocol on and
  10k lines of scrollback. `feed` advances one long-lived parser; `grid`/`mode`
  expose the screen for rendering; `resize` re-locks to a session size. Its
  `EventListener` **discards every query reply** the emulator generates
  (cursor-position, device attributes, color/text-area, clipboard-load) and
  only counts them — nothing it produces is written back, so the session stays
  the sole answerer of terminal queries. `discarded_query_replies()` proves a
  reply was generated and dropped.
- **Scrollback / selection / search** (`scrollback`, `selection`, `search`,
  `view`). `view::TerminalView` composes the emulator with all three and is the
  one place the **normal-screen-only** rule lives: each is a no-op / cleared on
  the alternate screen. Selection extracts stream text (wrapped rows joined like
  `capture-pane -J`); `selection::osc52_copy` is the OSC 52 copy encoder
  (clipboard plumbing stays the UI's job). Search is literal, per grid row.
- **Input encoding** (`input`). termwiz key encoder behind neutral `Key` /
  `Modifiers` types; kitty on → CSI-u form, so Claude Code's Shift+Enter encodes
  `ESC[13;2u` (plain `\r` without it). Mouse encoding (SGR + legacy X10) with
  viewport→pane coordinate translation, bracketed paste (embedded end-marker
  stripped), and focus events. `mouse_owner` is the pure ownership policy.
- **Clipping and letterboxing** (`viewport::fit`). Pure per-axis geometry:
  larger viewer centers (letterbox), smaller viewer clips from the origin; also
  the viewer→session coordinate map mouse encoding uses.

## Notes

- Added `termwiz` (MIT, `input` encoder only, `default-features = false`) to the
  workspace deps. `alacritty_terminal` is now used by product code, not just a
  build-time placeholder.
- Develops against snapshot + live output until `rt-replay` lands (the emulator
  takes any byte stream); nothing here depends on a full replay yet.
- Matches do not span a wrapped-row boundary (documented simplification).
- Pinned `uuid` to 1.26.1 in `Cargo.lock` (transitive via
  `wezterm-blob-leases`): 1.27.0 bumped its MSRV to 1.89 and broke the CI
  `msrv` job against the workspace's declared 1.88. 1.26.1 still requires only
  1.85, so the workspace builds on 1.88.
