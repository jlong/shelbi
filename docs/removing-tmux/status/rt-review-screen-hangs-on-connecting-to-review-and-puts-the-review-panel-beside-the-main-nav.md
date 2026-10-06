# rt-review-screen-hangs-on-connecting-to-review-and-puts-the-review-panel-beside-the-main-nav

Status: **ready for review**

Two review-screen fixes in the single-process TUI:

1. **Connect no longer hangs.** The session hello handshake read was unbounded,
   so a review session that accepts the connection but never answers (a wedged
   or older-build session) left the content view on "Connecting…" forever. Added
   a bounded hello read (`Connection::HANDSHAKE_TIMEOUT`, 10s) via a new
   `transport::ReadTimeout` seam on the read half; a timed-out hello surfaces
   `ClientError::HandshakeTimeout`. Re-selecting a failed content item re-attaches
   (`SessionManager::show_or_retry`); the failed placeholder says so.
2. **Review panel replaces the sidebar.** The panel now renders in the nav
   sidebar's column (at its width) and the content view fills the whole main
   area — two columns, not three. Esc (like the back arrow) returns to the nav
   sidebar with the previous view/selection restored; `q` still tears the review
   down. Ctrl+P palette and the panel footer keep working.

Tests: bounded-handshake unit test (client), silent-socket→Failed test (shell
session manager), Esc→Back test (review interface). No `Cargo.lock` change.

## Rework (2026-10-06): rebased onto the resizable sidebar

Rebased onto `origin/jlong/remove-tmux`, which now carries
`rt-make-the-sidebar-resizable` (#1548). The only conflict was in
`crates/shelbi-tui/src/shell/mod.rs` (`handle_mouse`), where both changes insert
logic at the top. Resolved by ordering the divider press-drag-release **before**
the review's mouse routing, so a divider press takes priority: while a review is
open the panel sits in the sidebar's column (`sidebar_rect`), so the divider
between panel and content stays draggable and a divider press neither selects a
panel item nor reaches the content view. The `draw` sidebar hunk auto-merged
(the `is_review` guard and the `display_width` clamp are independent).

Added test: `dragging_the_divider_while_a_review_is_open_resizes_the_panel` —
a divider press over an open review begins a drag (review stays open, client
focus unchanged, so the review routing was not reached), the drag resizes the
panel, and the released width persists. All prior tests from both changes pass.

## Rework 2 (2026-10-06): rebased onto the umbrella again

Rebased onto `origin/jlong/remove-tmux` after four more subtasks merged. The only
conflict was in `crates/shelbi-tui/src/shell/review.rs`, in `ReviewInterface::resize`,
against two of them:

- `rt-review-content-session-edit-in-vi-doesn-t-fill-the-content-area` (#1553)
  made `resize` take `&mut self` so the content `SessionManager` records the last
  requested size (`last_size`) and re-applies it on go-live.
- My relocation sends the content view the whole main area (no internal split),
  since the panel now lives in the sidebar column.

Resolved by keeping `&mut self` (so the size is remembered and re-applied) while
taking the content rect directly — `resize(&mut self, content: Rect)` with no
`split`. The caller (`draw`) already passes `main_rect` via `review.as_mut()`.

`rt-ctrl-h-ctrl-l-move-focus-between-the-nav-sidebar-and-the-main-pane` (#1554)
auto-merged: `ReviewInterface::focus_panel`/`focus_content` and the review branch
in `ShellState::move_focus` are preserved, and with the panel in the sidebar
column Ctrl+H focuses the panel (`FocusSidebar → focus_panel`) and Ctrl+L the
content (`FocusMain → focus_content`).

`cargo test -p shelbi-tui` (502) and `-p shelbi-client` pass, clippy clean, no
`Cargo.lock` change.
