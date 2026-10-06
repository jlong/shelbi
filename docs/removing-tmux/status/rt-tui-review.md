# rt-tui-review — Phase 4e: review interface in the single-process TUI

**Status:** implemented, behind the `session_backend` dev flag. tmux review
runtime unchanged.

The review interface is rebuilt natively: a native panel (the existing shared
`ReviewPanel` widget, `render_full`) beside a content terminal view in the
single-process shell. Editor and diff are **daemon-spawned** sessions
(`<project>/review/<slot>/<role>`) the client starts/stops over the control
socket and only attaches terminal views to; teardown (ending editor/diff/server
sessions + freeing the port) is the daemon's job.

Surface:
- `shelbi-proto` control v2: `ClientMsg::ReviewSession` + `ReviewSessionRequest`
  / `ReviewSessionOp` (`Ensure{role}` / `Close`) / `ReviewRole`.
- `shelbi-orchestrator::review_session`: `ensure_content_session`,
  `close_review` (reaps editor/diff + `stop_review_server` → port free),
  `review_open_info`; reuses `review_ui` diff/editor command builders.
- daemon `daemon/control.rs`: handles `ReviewSession` on a detached thread.
- `shelbi-client::ControlClient::review_session`; `shelbi-app::review_session`.
- shell `shell/review.rs` (`ReviewInterface`: panel + 2nd `SessionManager`),
  `SessionRef::Review`, layout-event subscriber + reconcile (opens AND closes),
  approve/reject via `execute_mutation` off-thread with competing-input blocking.

Tests: approve blocking while merging; q closes / tab focus; close-reconcile
(task left the review column); `close_review` reaps the dev-server group + frees
the port; `content_session_name`; proto/client/daemon control green.

Deferred (noted for review):
- Loading a **queued** review onto a free slot (the `ReviewConfirmed` overlay
  path) is still a status-note stub; this task opens reviews already on a slot.
- Remote review slots: spawning editor/diff is local-only (a remote slot returns
  a clear error), matching the tmux `RemoteFallback` behavior.
