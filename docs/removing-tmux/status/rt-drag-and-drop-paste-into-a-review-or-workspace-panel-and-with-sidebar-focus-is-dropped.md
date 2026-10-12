# rt-drag-and-drop-paste-into-a-review-or-workspace-panel-and-with-sidebar-focus-is-dropped

Status: ready for review.

Routed drag-and-drop pastes to whatever session the main area currently shows,
regardless of which pane holds focus.

## What changed

- `shell/mod.rs`: `Event::Paste` now calls a new `deliver_paste`, which routes
  by `main_view` — plain `Session` -> `self.sessions`; `Review(_)` ->
  `self.review` content; `Workspace(_)` -> `self.workspace` content; `Native(_)`
  (board/activity/machines, no terminal) is a no-op. It also focuses the main
  area so a drop while the sidebar/panel is focused still lands and focus
  follows. The old arm only pasted to `self.sessions` and only when main had
  focus, so review/workspace content and sidebar-focused drops were lost.
- `shell/review.rs`, `shell/workspace.rs`: added `paste_into_content`, which
  focuses the content view when live and sends the paste through the content
  `SessionManager` (bracketed encoding preserved by the existing `conn.paste`
  path). Review's is inert while a gated merge runs (AC #4 parity). Added a
  `content_state` test accessor to `WorkspaceInterface`.
- Tests (`shell/pty_input_tests.rs`, real PTY + observer): paste into the main
  session with the sidebar focused (path with a space arrives intact, focus
  moves to main); bracketed encoding when the program enabled DECSET 2004;
  paste into an open review content session; paste into an open workspace
  content session (both with the nav sidebar focused, both assert focus follows
  to the content view).

## Verification

- `cargo build -p shelbi-tui` and `cargo clippy -p shelbi-tui --all-targets -D
  warnings`: clean.
- `cargo test -p shelbi-tui --lib shell:: --test-threads=1`: 180 passed (the 4
  new paste tests included).
- The 64 KB / large-paste and special-character ACs ride the same `conn.paste`
  transport the main session already used; the "path with a space arrives
  intact" assertion exercises the no-requoting guarantee.

## Not done here (needs a human at a GUI)

The "drag an image from Finder and confirm Claude shows `[Image #N]`" check is a
manual GUI step I can't perform as a headless worker. The real-PTY tests prove
the byte-level equivalent end to end (the dropped path reaches the agent intact,
bracketed when the program asked for it), for all three session kinds. The
Finder-drag confirmation in the live TUI is left for the reviewer.

## Base check

Worktree base contains the umbrella foundation (`crates/shelbi-proto`,
`docs/removing-tmux/README.md`). No template/config changes, so no
config-upgrade sniffer is needed. No `Cargo.lock` change (no MSRV check needed).
