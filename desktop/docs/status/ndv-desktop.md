# ndv-desktop: Shelbi's own diff view on the desktop

Part of `native-diff-view`. The foundation is `shelbi_app::diff`; this is the
gpui client of it. The TUI's is `ndv-tui`, and comments' delivery and revert are
`ndv-comments-agent`.

## What landed

- **Review's View Diff** shows a [`DiffView`](../../src/views/diff_view/mod.rs)
  in the content's place when `AppTools::builtin_diff` says so (`diff_tool`
  unset or `builtin`, no `review.diff_command`). No daemon session and no
  `DiffLoad` wait. An external tool keeps its pane and its wait, unchanged. A
  remote slot now offers View Diff for the built-in view (`ReviewPanelModel`):
  it reads any machine's worktree, while the daemon only spawns panes for a
  local one.
- **Workspace's View Diff** does the same in the left pane, on the live
  worktree including uncommitted and untracked files, re-reading every 2s while
  it's up (the foundation's digest keeps an unchanged read from redrawing).
- **The view**: the file list (status, name and directory, counts, a viewed
  check that clicks), `Commits: all ▾`, the rows, the status bar. Rows are a custom
  element (`body.rs`) that shapes only the rows in view, as the terminal does;
  the file list is a `uniform_list`. Unified and split; syntax from the
  terminal's ANSI palette and word highlights over tints (`theme/diff.rs`);
  folds, the hunk filter, search marks, blame gutter, visual selection; the
  palette (files, commands, commits) and the `?` help, built from the live
  keymap. It uses the terminal font and follows ⌘+ / ⌘−.
- **Mouse**: a click places the cursor, a click on a header's chevron folds, the
  wheel scrolls (sideways for long lines, with the stroke's larger axis winning),
  a file row jumps, a check marks viewed, a palette row chooses, a click off an
  overlay closes it.
- **Comments, basically**: thread rows and the comment editor draw, since the
  state already takes the keys and a thread can arrive from elsewhere. Their
  polish is `ndv-comments-agent`'s.

## Decisions

- **Keys.** The window asks `DiffView::wants_key` (`DiffState::wants_key`) before
  taking a chord from the focused diff. So `Ctrl-p` is the diff's file palette
  and `?` its help while it has the keyboard, the global palette stays on
  Ctrl+Space and ⌘K, and every ⌘ chord, Ctrl+H and Ctrl+L still reach the window.
- **Asking for the diff hands it the keyboard** (a click on View Diff, Enter on
  its row), unlike the terminals, where Tab moves in: it's a keyboard-driven view
  with nothing to do without keys. Esc with nothing to let go of goes back to the
  panel. ⌘N keeps the keyboard in the content, moving it to whatever's now there.
- **The file list's keyboard.** `Space` pages in the rows and marks viewed on the
  list, so the list holds the keyboard once clicked, or opened with its key
  (`\`). `Enter` and `Esc` go back. `m` marks viewed from anywhere.
- **The view outlives the switch.** A diff keeps its place while another view is
  up. `DiffSession::set_visible` rests a workspace's reads meanwhile, and a read
  on return publishes only if something changed. A new task on the workspace, or
  the × on View Diff, starts a fresh one.
- **Colors are derived**: there's no diff frame in Figma yet, so `theme/diff.rs`
  tints the app's text green and red and borrows the ANSI palette for syntax,
  with a contrast test in both themes.
- **Horizontal scroll, not wrap**, so rows stay one height and the row table stays
  the whole layout. A line past 4,096 bytes is cut, as the highlighter does.

## What's left

- `gY` copies the comments and says so; delivery to the agent is
  `ndv-comments-agent`.
- The scrollbar is an indicator: no dragging.
- No accessibility tree for the rows yet (the terminal has one).

## Verified

- `cargo test` (the view's keys, clicks and focus in `views/diff_view/tests.rs`,
  the hosts' swap in `views/review.rs` and `views/workspace.rs`, the window's
  chords in `app_column_tests.rs`), `cargo clippy --all-targets -- -D warnings`.
- `--visual-review`: `diff-*.png`, unified, split, file list, help, palettes,
  search, folds, blame, filter, selection, comment, each in dark and light.
- `--visual-workspace`: `workspace-diff.png`, `workspace-diff-shell.png`.
- `--visual-views`: `diff-review.png`, `diff-workspace.png` and
  `diff-workspace-edited.png` over a real git worktree through `DiffDeps::live`,
  with a real worker wake and an edit it noticed by itself.
- Not walked by hand in the running app.
