# dt-switch-project-in-window: a switch stays in the window

## What landed

Switching projects (the sidebar's switcher, the palette, File ▸ Open Project,
a notification click, a project just added) retargets the current window
instead of opening another. File ▸ New Window (⌘⇧N, and the palette's "New
Window") opens a second window on the current project, which then switches
on its own.

```
[window: web] ── switch to "api" ──▶ [same window: api]   (frame, size, sidebar width kept)
            ◀── switch to "web" ──   [same window: web, where web was left]
File ▸ New Window (⌘⇧N) ──▶ a second window on the current project, cascaded 28px
```

- **Switch in place** (`ShelbiWindow::switch_to`). The outgoing project's
  view state is saved, then its views are torn down: the overlay, the chat,
  board, activity, machines, review and workspace panels (their sessions
  detach; the daemon's stay), their subscriptions, the review flow, the panel
  in the column, the feed subscription, the notices tracker. The target's
  prefetched sidebar skeleton (`projects::Catalog`) paints on the next frame
  with its saved selection, and its main view is built and attached once that
  frame is drawn (`SwitchHold`), the `dt-instant-panel-push` pattern. The
  window title follows the project, so the Window menu (AppKit's) lists each
  window by its current project.
- **Where a project was left** (`window_state::ProjectViewState`, in
  `projects/<name>/desktop-view.json`): the view, keys on the sidebar or the
  main area, the selected sidebar row (by what it opens, so it survives rows
  coming and going), the sidebar's scroll, the open review, the panel's Back,
  the board's workflow filter. The catalog prefetches it with the skeletons,
  so a switch reads it from memory; a switch away updates memory at once and
  disk off the thread. A workspace panel comes back without a push; a review
  reopens through the load flow and backs out quietly (no slot prompt, no
  banner) if it no longer opens. A workspace the project no longer declares
  gives way to the chat.
- **Shared feeds** (`crate::feeds::FeedHub`). Two windows may show one
  project: they share its `ProjectFeed`; a later window is handed the last
  snapshot and link at once, and the last one out stops the feed. The first
  window showing a project posts its notifications, so none comes twice.
- **New Window** (`CommandKind::NewWindow`, registry id `action:new-window`):
  offered only to a multi-window client (`CommandModel::multi_window`, which
  only the desktop sets). The window takes ⌘⇧N itself (`keys::NEW_WINDOW`),
  ahead of a focused terminal; the File menu shows it on the item. The new
  window takes the lowest free slot and the frontmost window's sidebar width
  and collapse.
- **Notifications** (`app::open_notice_tag`) go to a window already showing
  the project, else switch the frontmost window; the notice's target wins
  over the view the project was left on.
- **Quit Project** keeps the window: every window showing the project moves
  to another open project, or to the empty-state chooser (every project, and
  Add Project; ↑↓ Enter or a click) when none is left.
- **Window slots** (`window_state::WindowSession`, in
  `~/.shelbi/desktop-windows.json`): each slot keeps its window's frame and
  sidebar, the project it last showed, and whether it was open. A launch
  reopens the open slots, each on its project (`launch_plan`; a project
  named on the command line takes the first window). Closing a window while
  others stay open marks its slot closed; the last window closing quits the
  app with its slot still open, so it comes back. The first launch without
  the file migrates: one window on the launch project with the frame its old
  `projects/<name>/desktop-window.json` saved.
- **No join on the UI thread.** The board, activity and machines views stop
  their refreshers without waiting on a read in flight
  (`shelbi_app::Refresher::stop_without_waiting`, new), so dropping them on
  a switch never waits on a slow read (the TUI's cold-switch freeze,
  `rt-cold-switch-instant-sidebar`, was a join like that).

## Decisions

- **The TUI never lists New Window.** `CommandKind::NewWindow` is shared, so
  the TUI's exhaustive `dispatch_effect` has an arm: a status line saying to
  run `shelbi` in another terminal. The registry gates it on
  `CommandModel::multi_window`, so the TUI's palette never offers it (the
  user's call when asked).
- **The window is retargeted, not rebuilt.** The project is a field the
  window changes; the window keeps its frame, sidebar width, theme and
  bootstrap result, and only the project-bound pieces are reset
  (`teardown_project`).
- **View state is per project, frames per window slot.** Two windows on one
  project share its view state (last save wins), and each window keeps its
  own frame across switches.
- **Async results from the old project are dropped.** Bringing up the
  orchestrator, the palette's inputs and the keymaps check the window still
  shows the project they were read for.

## Verified

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo test` (with `--features visual-check` and `--features bench` too);
  root `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  `cargo test -p shelbi-app` (scenarios and the parity doc guard) and the
  TUI's palette tests.
- `src/app_window_tests.rs`: a switch retargets the window with no new
  window; the switch's first frame paints only the sidebar and the next
  attaches; A→B→A restores view, selection, scroll, panel and its Back, with
  one feed and one live session attach left (the old ones closed); New
  Window opens a second, cascaded window that switches on its own; two
  windows on one project share its feed and only the first posts notices;
  a notice goes to the window showing its project, else switches the
  frontmost; quitting the shown project keeps the window (another project,
  or the chooser); a launch reopens the open windows and a close forgets
  one. `window_state` tests cover the launch plan and the migration;
  `feeds` tests the sharing and replay.
- Not walked by hand in the real app: a sandboxed launch came up (its own
  daemon, window slot written), but the user was at the machine and driving
  the app would have taken focus, so the walk stopped there.
