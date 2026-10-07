# rt-select-text-with-the-mouse-and-copy-it-with-cmd-c

Status: **ready for review**

Mac-native text selection and copy in the in-process TUI terminal panes
(orchestrator chat, workspace sessions, review content).

- **Plain left-drag always selects**, even when the program has mouse reporting
  on. A left press is buffered (click vs. drag is unknown at press time): the
  first motion makes it a Shelbi selection anchored at the press; a release with
  no motion is a click. A click reaches the program (press+release forwarded on
  release) only when the program is reporting; otherwise it is a bare Shelbi
  click. Wheel/other buttons keep the old ownership policy.
- **Escape hatch:** Option/Alt+drag forwards the whole gesture (press/drag/
  release) to a reporting program. Documented in the palette footer key help.
- **Visible highlight** via reverse video (`Selection::contains` + a REVERSED
  modifier in `render_grid`). It persists after mouse-up until the next click, a
  key sent to the session (`encode_key` clears it), or new output.
- **Cmd+C (SUPER) / Ctrl+Shift+C copy** the current selection via the existing
  `copy_to_clipboard` (OSC 52 + `pbcopy`). The chord is consumed by the shell
  (`handle_main_key`) and the review content handler, so no stray `c` / Ctrl+C
  reaches the agent. A bare Ctrl+C (no Shift) is deliberately not a copy chord.
- Copy-on-release is unchanged: a finished drag still yields `MouseOutcome::Copy`.

Changed crates: `shelbi-term` (`Selection::contains`), `shelbi-tui`
(`terminal_view`, `shell/mod.rs`, `shell/review.rs`, `overlay/palette.rs`),
plus a Ghostty `performable:super+c` note in the install guide.

No shipped config templates changed, so no config-upgrade sniffer is needed.
`Cargo.lock` is untouched (no MSRV run required).
