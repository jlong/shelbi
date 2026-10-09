# rt-tab-doesn-t-reach-claude-code-no-tab-completion-in-shelbi-next

Status: ready for review.

Fixed Tab completion in Claude Code under the in-process TUI. Root cause: with
the kitty keyboard protocol active (Claude Code pushes `CSI > 5 u`), the Tab
*key* must reach the agent as the CSI-u event `ESC[9u`; termwiz encodes plain
Tab as a bare `\t` regardless of the protocol, which Claude reads as literal tab
text, so completion never fires. Fix is a scoped carve-out in
`shelbi_term::input::encode_key`: unmodified Tab under kitty → `ESC[9u`.

Reproduced and validated against a real `claude` process driven through a PTY:
- `@partial` + `\t` did **not** accept the file-picker completion; `@partial` +
  `ESC[9u` **did**.
- Shift+Tab mode cycle is driven by the legacy backtab `ESC[Z` (what we already
  send) — `ESC[9;2u` does **not** drive it — so the carve-out is scoped to the
  unmodified Tab and Shift+Tab is left untouched.
- Enter/Shift+Enter, Ctrl+C, Esc, and modified arrows were checked and are
  already correct; no change.

Tests: byte-level `encode_key` tests for Tab/Shift+Tab in both modes
(shelbi-term), plus a shell PTY test that Tab with the main pane focused is
forwarded (not consumed) and reaches the agent as `ESC[9u`.
