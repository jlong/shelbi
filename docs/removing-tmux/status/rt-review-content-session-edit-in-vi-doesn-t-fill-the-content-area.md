# rt-review-content-session-edit-in-vi-doesn-t-fill-the-content-area

Done. Review content sessions (Edit in Vi / View Diff / Chat with Reviewer)
now fill the content area instead of letterboxing their spawn-time default grid.

- Root cause: a content session attaches at the daemon's `default_size()`
  (120x40). The shell reports the viewport size once, only when `reported_main`
  changes, and the switch to a new content view starts a fresh connect — so the
  resize went out while the slot was still connecting and reached no live
  connection. On go-live the pane kept the default grid, which `viewport::fit`
  letterboxed (blank bands, status line above the bottom).
- Fix (in the sizing/resize path, per the task note so it merges cleanly with
  the review-panel-relocation task): `SessionManager` now remembers the last
  requested viewport size and re-applies it the moment a session goes live.
  `SessionManager::resize` / `ReviewInterface::resize` take `&mut self`.
- Test: `a_resize_while_connecting_is_applied_when_the_session_goes_live`
  (verified red without the go-live re-apply).
