# dt-task-pane-session

The floating task pane (`dt-floating-task-pane`) showed a task's details
only. A task an agent is working now shows that agent's live session at the
pane's foot, as John's revised In progress frame draws it (Task pane ·
In progress · Dark, 2172:722, reviewed live on 2026-10-28). The idle frame
(2573:10737) is unchanged: no session, no Start footer, the details fill the
pane.

## What landed

- **Which session** (`shelbi_app::task_session`, toolkit-free). The pane's
  task resolves through the sidebar's rows, never its status: the review
  slot a review task is loaded onto (`ReviewRow::workspace`, Serving or
  Loading), else the dev workspace whose row is working it
  (`WorkspaceRow::current_task`). A task assigned to a workspace busy with
  another task, a queued review, a task nobody works, and a task gone from
  the board show no session. `TaskSession::open_command` is Open
  Workspace's command: `FocusWorkspace` for a workspace, `LoadReview` for a
  review (which reuses the slot already serving it).
- **The session** (`views/pane_session.rs`). The board owns one
  `SessionView` for its pane, the same view the Chat, Workspace and Review
  screens use (attach with replay, restart states, remote reconnect, idle
  re-attach), so local and SSH sessions and every agent runner work as they
  do there. Its relauncher is `NoopRelauncher`: opening, switching, closing
  or hiding the pane attaches or detaches this one client and never
  relaunches, restarts or dispatches anything. A session that exits is
  re-attached once the daemon brings it back; a remote one reconnects over
  its relay; an idle attach retries when the sidebar row changes. Another
  task drops the old attach before the new one starts, so the old task's
  output never shows under the new one. The board attaches it only while
  the window shows the board or the pane floating over another screen, and
  over a Workspace or Review screen that already shows the same agent the
  pane leaves it out rather than attach it twice. A project quit marks it a
  deliberate close like the other session views.
- **The layout** (`views/task_detail.rs` `with_session`, math in
  `views/pane_split.rs`). Under the Header: the details' scroll, the 17pt
  resize handle (its 1pt divider at the foot, the 32 x 4 grip centered,
  16pt corners, in `divider`, `icon` while dragged or hovered), then the
  session area, 356pt at the reference size with its 44pt header. At
  1440 x 1024 that is the frame's 583pt details scroll exactly. The session
  area paints `terminal_canvas` (the pane's canvas at the default contrast),
  and the grid sits 20pt in across, 16pt from the top and 8pt from the
  bottom (`SessionView::set_inset_y`, new). The header is 44pt, 12pt in, 8pt
  between: the sidebar's workspace state glyph (`sidebar::workspace_glyph`,
  or `review_glyph` for a review slot; `render_glyph` is now shared), the
  workspace name in Base Medium loud, and Open Workspace (Small, Default)
  at the trailing edge, over a divider. No Claude Code label, Running text
  or collapse chevron, per the revised frame. The prompt, progress and
  output are the PTY's own: the real terminal element draws them, with the
  user's font, ANSI colors, theme and accessibility settings, live.
- **The split.** Drag the handle (it centers on the pointer), or give it the
  keys (a click) and press ↑ / ↓; from the pane, ⌥↑ / ⌥↓. Each step is
  24pt. The session keeps at least its header and five rows (140pt), the
  details at least 120pt; a pane too short for both shares its room by
  those minimums. The asked-for height is kept through a smaller window and
  comes back when it grows. Resizing goes through the terminal's existing
  debounced resize and the active-client policy; it never makes a second
  attach.
- **Keys.** A click in the terminal gives it the keys (and moves the
  window's focus model to the main area, `IssuesEvent::Focused`); Tab from
  the pane does too. While it has them the board's and pane's keys are off
  (`IssuesView::on_key` returns early), so `j`, `l`, `e` and Esc reach the
  agent, and the window's capture phase treats it as a focused terminal for
  the global chords (`chat_focused`). Ctrl+L (focus main) or a click in the
  details gives the pane the keys back.
- **Independent scrolling.** The details and the terminal scroll on their
  own; output never moves the description or takes focus from the body
  editor (a session only takes focus on attach when it already had it).

## Decisions

- **Association by the sidebar's rows, not the card's assignee.** The card's
  `assigned_to` also names a workspace that has queued the task behind
  another; attaching that would show an unrelated agent. The rows are what
  the sidebar and Workspace view already trust.
- **A loaded review shows its reviewer**, even when the dev workspace's row
  still names the task: the reviewer is the live work, and Open Workspace
  opens the review.
- **Attach-only relauncher.** The pane's view never writes reopen markers,
  so re-selecting a given-up pane here doesn't reset the daemon's budget;
  that stays the Workspace view's gesture.
- **Open Workspace closes the pane** (as the pane's `o` key already did) and
  routes through the registry, so the Workspace or Review view attaches
  the same session as itself; the pane's own attach goes with the pane.
- **No duplicate over its own screen.** More over the Workspace or Review
  view that shows the agent leaves the session out of the pane.
- **Review slots not in the sidebar's workspace rows** attach as local
  (their machine isn't in the row): a dropped remote link then reads as an
  exit and re-attaches through the restart states instead of the reconnect
  backoff.
- Keyboard resize lives on the handle and on ⌥↑ / ⌥↓ from the pane rather
  than in More: it's a layout gesture, not a task action.

No `CommandKind` or `shelbi_app::view` field changed (the new types live in
`shelbi_app::task_session`), so the registry, both clients' exhaustive
matches and `parity.md` are unchanged; the TUI's task popover has no
session area.

## Verification

- `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings`
  (and `--features visual-check`), `cargo test` (845 passed, rebased on #1987).
- Root: `cargo check --workspace --locked`, `cargo clippy -p shelbi-app
  --all-targets -- -D warnings`, `cargo test -p shelbi-app`. `Cargo.lock`
  unchanged.
- New tests. `shelbi_app::task_session`: the working workspace, a busy or
  idle assignment resolving to nothing, a remote machine, a loaded review
  and its Open command, a queued review. `views::pane_split`: the frame's
  356 / 583, drag clamps, a short window and its return, too short for
  both, keyboard steps, junk input. `views::pane_session` (the board with
  fake sessions): the frame's geometry and the correct attach; assignment
  or status alone attaching nothing and an idle task launching nothing;
  switching dropping the old attach first; close detaching only this view;
  Open Workspace and the review's Open; terminal keys (`j l escape`)
  reaching the agent with the pane open and the board unmoved; Tab into the
  terminal; drag and keyboard resize without a second attach; a small
  window; hide / show; not twice over its own screen; reassignment and
  deletion; a session appearing on a row change; a remote link drop
  reconnecting; streaming output during a draft keeping the editor's focus
  and the details' scroll. `app::column_tests`: the pane's session follows
  the screen and isn't doubled over the Workspace view's More.
- `--visual-figma` (`SHELBI_VISUAL_ONLY=issues`): `task-pane` now renders
  alpha's live session from a fake PTY screen. Against the In progress
  frame the header, grip, insets, typography and ANSI colors line up; what
  differs is the example output itself (its row spacing, and the composer
  rule Claude draws). Light checked by eye. `task-pane-idle` is unchanged.
- Concurrent TUI / desktop attachment is the session protocol's
  multi-client attach the Workspace view already relies on; the pane is one
  more client and resizes through the same active-client policy.
