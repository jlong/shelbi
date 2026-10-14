# dt-review-view: the Review view

## What landed

- **The Review view** (`src/views/review.rs`). Opening a review-column task
  (its sidebar row, or the palette's `review:<id>`) routes the main area to the
  review layout: the native review panel on the left, a content terminal on
  the right. The window keeps its sidebar.
  - **The panel**, drawn from `shelbi_app::ReviewPanelModel` and driven by the
    shared `ReviewPanelState`: the back button and the cyan "Ready for review"
    status; the task title and a three-line description preview with `More`
    (the full markdown body in a popover, Esc or a click outside closes it);
    the worktree path (start-truncated, click to reveal it in Finder); the
    **server block** for the slot (below); the view switch (Reviewer / View
    Diff / Edit in `<editor>` / Open Browser when a URL exists); and Approve /
    Reject in green and red.
  - **The server block**: `Server · <slot>`, the sub-state with the sidebar's
    glyph (`✓ serving` / `▶ loading` / `· pending-load`) and the
    `machine:port` location once serving, a one-line meaning of the sub-state,
    the health check (`Health check passed` / `Waiting for the health check` /
    `No health check` when the workflow declares no `ready:` probe), and the
    review URL as a link (click opens the browser). It follows the task's
    sidebar row on every refresh, so it updates live.
  - **Content**: Reviewer attaches the review agent's session
    (`<project>/ws/<slot>`); View Diff and Edit in `<editor>` ask the daemon to
    ensure `<project>/review/<slot>/{diff,editor}` (`ReviewSessionOp::Ensure`)
    and only attach. The pane is the shared `SessionView` (with a
    `ReviewRelauncher`), so restarts, give-up and remote reconnect are the
    chat's. A refused view falls back to Reviewer with `couldn't open view: …`
    on the panel's status line.
  - **Keys** while the panel has focus: `↑`/`k`, `↓`/`j`, Enter/Space, `m`
    (description), Esc (Back), `q` (Close), Tab (to the content terminal once
    it is live). Ctrl/Alt/Cmd chords go to the window (the palette, Ctrl+H/L).
  - **Approve / Reject** emit the registry's `ApproveReview` / `RejectReview`
    (empty reason, which opens the reject-reason overlay). The window runs the
    decision through `execute_mutation` (the daemon's gated merge). While it
    runs, the panel is inert (switches, Back, Close, a second decision, the
    URL) and the actions become "Merging the PR…". Success asks the daemon to
    close the review's sessions and returns to the previous view; failure puts
    `approve failed: …` / `reject failed: …` on the panel's status line.
  - **Back** (button, Esc) returns to the view the review was opened over and
    gives the sidebar the keys; the daemon's sessions stay. **Close** (`q`)
    sends `ReviewSessionOp::Close` (the daemon ends the editor, diff and server
    sessions and frees the port) and returns to the previous view.
- **The window** (`src/app.rs`): `Route::Review` (transient, like Settings, so
  `ClientState`'s view stays the one to return to); the review load flow; the
  merge gate; leaving and closing; dropping a review whose task left review.
- **The feed** (`src/refresh.rs`) now forwards the daemon's layout events
  (`FeedEvent::Layout`) from `shelbi_app::layout_sub`, and a layout event
  wakes a refresh.
- **`--visual-review <dir>`** (feature `visual-check`): the view offscreen over
  fake sessions; frames for serving, diff, loading, merging, description.
- A vendored Heroicon: `arrows-right-left` (View Diff), byte-identical to
  heroicons 2.2.0's `24/outline`.

### Moved to or added in the shared crates

- `shelbi-app`:
  - New `review` module: `ReviewPanelModel` (moved from `view`, which
    re-exports it, and rebuilt: it was unused) built from `ReviewInfo` plus the
    task's `ReviewRow`; `ReviewServer`, `HealthCheck`, `sub_state_label`;
    `ReviewSwitch::label`; the panel's navigation (`PanelRow`, `PanelEffect`,
    `ActiveView`, `panel_rows`, `step_selection`, `activate_row`,
    `ReviewPanelState`); `description_text` (the TUI preview's markdown
    stripping); and the load flow `ReviewLoader` / `ReviewTarget` /
    `LoadStep`.
  - `layout_sub` moved here from `shelbi-tui` (unchanged code), so both
    clients subscribe to layout events the same way.
- `shelbi-orchestrator`: `ReviewOpenInfo` gained `review_url`,
  `has_health_check` and `remote_machine`; `review_session::review_url` moved
  here from the TUI's `review_panel.rs`.
- `shelbi-tui`: its review panel now uses the shared rows, effects, selection
  stepping, activation table, switch labels and markdown stripping (the enum
  variants `Vim` → `Editor`, `ShowVim` → `ShowEditor`, `FocusDashboard` →
  `Back`); `layout_sub` and `review_url` delegate to the shared crates. No TUI
  behavior changed; its 146 review/panel tests pass unchanged apart from the
  renames.

## Decisions

1. **Queued loads match the TUI** (orchestrator ruling on q-001). A queued
   task opens the "Load for review" dialog over the **free** slots; with every
   slot busy it shows the existing busy report and loads nothing. The task
   stays pending-load (the panel and sidebar show it) and the daemon's
   auto-loader (`autoload_review_queue`) puts it on the next free slot; that is
   what satisfies "queues and loads when one frees". No client-side queue.
2. **`ReviewOpened` opens a load this client started, and nothing else.** The
   flow marks the task pending when a slot is chosen (before the load is sent,
   since the daemon publishes `ReviewOpened` before it replies `Done`); only
   that task's event opens the view. Another client's load, the poller's
   resume or the auto-loader never take the view. Opening another review drops
   the pending wait.
3. **Approve has no confirm.** The spec says the buttons open
   `dt-palette-overlays`' review-confirm overlay, but that overlay is the
   "Load for review" slot dialog, and the TUI approves with no confirm (the
   ruling `dt-palette-overlays` recorded). Reject opens the reject-reason
   overlay, as specified.
4. **The panel sits in the main area, beside the window's sidebar**, as the
   spec says ("the main area becomes the review layout"), not in the
   sidebar's column as the TUI's does. So Ctrl+H goes to the window's sidebar
   (the TUI's goes to the panel), and Back gives the sidebar the keys.
5. **Leaving returns to the previous view**, for Back, Close and a finished
   decision alike, per the spec. The TUI's Close and decisions go to the
   orchestrator chat instead. Back leaves the daemon's sessions running; Close
   and a decision ask the daemon to close them (`ReviewSessionOp::Close`, as
   the TUI does after a merge).
6. **The content terminal keeps Tab.** The TUI's content view gives Tab back
   to the panel; here every key but the window's global chords reaches the
   terminal (as in the chat and Workspace views), and a click on the panel
   takes focus back.
7. **Server sub-state and health check come from the sidebar row plus the
   workflow**: the row's `ReviewState` is the live sub-state (it is what the
   sidebar shows, from the slot's loaded marker and liveness), and whether the
   workflow declares a `ready:` probe turns it into the health-check line
   (`HealthCheck::of`). The URL is resolved once when the review opens
   (`ReviewOpenInfo::review_url`).
8. **Close-reconcile from snapshots.** A shown review whose task is no longer
   among the sidebar's review rows is dropped (the TUI reconciles the same way
   from `review_layout_state`), besides reacting to `ReviewClosed`. Skipped
   while the board is loading, the config has an error, or a decision is in
   flight (its own reply finishes it).
9. **Remote review slots** show Reviewer and Open Browser only: the daemon
   spawns diff and editor for local slots (`ensure_content_session` refuses a
   remote one), and Reveal is local. The content reconnects through the relay
   like a remote workspace.
10. **No spinner animation** for "Merging the PR…": the TUI's braille spinner
    needs a tick; the static line reads the same and keeps the view free of a
    timer.
11. **A review pane with no session** says "No live session for `<slot>`"
    rather than the dev slot's idle placeholder (the TUI's review interface;
    new `SessionView::set_idle_placeholder`).

### The edge cases from `docs/removing-tmux/status/rt-review-*.md`

- **ENOTCONN on re-entry / zombie siblings, wedged sessions, the 5s
  handshake**: fixed in `shelbi-client` / `shelbi-session` (`choose_session`,
  `probe_reachable`, the bounded hello). The desktop's connect is the shared
  `retry_connect` over `session_connect::connect`, so it inherits them; a
  connect that still fails settles in a failed notice, and re-selecting the
  view re-attaches (test: `a_failed_attach_is_retried_by_selecting_it_again`).
- **Re-entering a review**: hiding detaches the pane and showing re-attaches,
  a diff left open included (test: `re_entering_a_review_reattaches_every_time`,
  four rounds).
- **Content sized to the area** (`rt-review-content-session-edit-in-vi…`): the
  terminal element reports its size on layout, as in the chat.
- **A live review with no health check reads ready**
  (`rt-a-review-with-a-live-review-agent…`): the sub-state is the sidebar
  row's, which already uses `review_slot_is_serving`.
- **Closing the app mid-merge**: the merge runs in the daemon's job thread;
  the daemon test `a_merge_crossing_mutation_finishes_after_the_client_disconnects`
  covers a client that leaves mid-merge. Closing the window sends nothing.

## Verification

From `desktop/`:

- `cargo build`: ok
- `cargo clippy --all-targets -- -D warnings`: ok
- `cargo clippy --all-targets --features visual-check -- -D warnings`: ok
- `cargo test`: **256 passed** after rebasing onto `dt-terminal-complete` (224 before). New:
  - Review view (16): the panel from `ReviewInfo` / `ReviewRow` fixtures
    beside the review agent's chat; the server state following the row; diff
    and editor attaching to daemon-ensured sessions; a refused view falling
    back; Browser, the URL and Reveal; no URL, no Browser; Approve / Reject as
    registry commands; the merge gate (no switch, Back, Close or URL); panel
    keys; Close tearing down; Back leaving it loaded; re-entry; a failed attach
    retried; another review starting fresh; a remote slot; finishing a
    decision.
  - The feed forwarding a layout event; the window's decision and load
    commands through the registry and the daemon gate.
- `cargo run --features visual-check -- --visual-review <dir>`: 4 checks ok,
  5 frames (serving, diff, loading, merging, description).

From the repository root:

- `cargo build --workspace`: ok
- `cargo clippy --workspace --all-targets -- -D warnings`: ok
- `cargo check --workspace --locked`: ok. The root `Cargo.lock` is unchanged.
- `cargo test -p shelbi-app --lib`: the new `review` tests (load flow,
  panel navigation, model) and the moved `layout_sub` tests pass.
- `cargo test -p shelbi-tui --lib -- review panel`: 146 passed.
- `cargo test --workspace --no-fail-fast`: **4005 passed, 0 failed** (before the rebase onto `dt-terminal-complete`). After it: `cargo build --workspace`, clippy and `check --locked` again ok, and `cargo test -p shelbi-app -p shelbi-tui -p shelbi-term` 961 passed plus `-p shelbi-orchestrator --lib review_session`, the crates this branch or that one touched.

- Rebased onto `dt-menus-notifications` (umbrella `718a4fdc`): kept both `gpui.md` notes, and `refresh.rs` keeps its orchestrator-title probe beside the `FeedEvent::Layout` forwarding (a layout event still wakes a refresh), with both feed tests. After it, from `desktop/`: `cargo build` ok, `cargo clippy --all-targets -- -D warnings` ok, `cargo test` 282 passed, `--visual-review` 0 checks failed; from the root: `cargo check --workspace --locked` ok, `cargo clippy --workspace --all-targets -- -D warnings` ok, `cargo test -p shelbi-app -p shelbi-tui --lib -- review panel layout` 46 + 149 passed.

## What's left

- **Not driven by hand in the live app.** No synthetic input was posted (other
  workers and the user share this machine), so a real review load, approve and
  reject were exercised through gpui test windows, the offscreen harness and
  the existing daemon tests, not against a live hub.
- **The panel width is fixed** (300px); the TUI shares its width with the
  sidebar's. A drag handle could follow.
- **The TUI's review panel still draws its own ratatui rows**; only the
  navigation table, labels and stripping are shared. The TUI could adopt
  `ReviewPanelState` outright (its shell's merge gate is the same rule).
