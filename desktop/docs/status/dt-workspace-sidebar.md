# dt-workspace-sidebar: the workspace panel takes the sidebar's column

## What landed

Opening a workspace puts its panel in the navigation sidebar's column, with
the same push (or Reduce Motion crossfade) the review panel uses, and the
agent's terminal fills the main area from the top. The header row that sat
above the terminal (state, name, truncated task, view switch, More, Reveal,
Shell) is gone; everything it held is in the panel.

```
┌ workspace panel ──────┬ agent terminal ────────────────────────────┐
│ ←  ⏵ Working          │                                            │
│ alpha · Developer     │                                            │
│                       │                                            │
│ Warm the application  │                                            │
│ cache at startup      │                                            │
│ cold-start            │                                            │
│ Warm the application… │                                            │
│ More                  │                                            │
│                       │                                            │
│ ⑂ jlong/cold-start    │                                            │
│ ▭ …/.shelbi/wt/alpha  │  (click: reveal in Finder)                 │
│ ▤ hub                 │                                            │
│                       │                                            │
│ ▣ Developer           │  (view switch: active row highlighted)     │
│ ⇄ View Diff           │                                            │
│ ✎ Edit in Vim         │                                            │
│                       │                                            │
│ ▢ Shell          off  │  (toggle)                                  │
└───────────────────────┴────────────────────────────────────────────┘
```

- **One column state machine** (`src/views/column_swap.rs`): `Occupant` is
  `Sidebar | Review | Workspace`. The two panels share the upper level of
  the push, so swapping one for the other queues the second: the first pops
  back to the sidebar, then the second is pushed from the moment the pop
  ended. A toggle mid-way still turns around from where the column stands,
  and a change of course while a hop is queued drops the queued push.
- **The panel** (`src/views/workspace.rs` `render_panel`): the back button
  and the live state (the sidebar badge's glyph, words and color), the name
  and agent role; the full task title (wrapped), its id, a three-line
  description preview and More (the existing popover); the branch, the
  worktree folder (click reveals it in Finder, replacing Reveal) and the
  machine; the view switch as rows (agent, View Diff, Edit in <editor>, with
  a × on the active Diff or Editor that ends those sessions); the Shell
  toggle row (`on` / `off` / `starting`); the status line for failures.
- **Idle**: a reduced panel (name, state, branch, worktree, machine) beside
  the idle placeholder. No task, view switch or Shell.
- **Remote**: no Diff, Editor or folder reveal (the path still shows); the
  agent and Shell rows stay; the machine row has the globe and `(remote)`.
- **Keys** (the panel holds focus when it opens, as the review's does):
  ↑/↓ or j/k, Enter/Space, `m` (description), Esc (Back), `q` (close the
  diff and editor, then Back), Tab (to the agent's terminal). The window's
  sidebar chord focuses the panel while it stands in the column.
- **Shared navigation** (`crates/shelbi-app/src/workspace_panel.rs`): the
  rows, selection, activation and keys, as `shelbi_app::review` does for the
  review panel. The TUI's `workspace_panel.rs` now builds its rows and
  activates them through it (it asks for no Shell row).
- **Icons**: Heroicons' `share` is vendored (all three sizes) for the branch
  row, Heroicons having no git-branch glyph; `document-text` (the old More
  button) is no longer used and was removed.

## Decisions

- **Back returns to the view the workspace was opened over**, as the TUI's
  workspace interface and the desktop's review do, with the sidebar's
  selection, scroll and focus as they were. The window remembers that view
  (`workspace_return`); moving between workspaces, or returning from a
  review opened over the workspace, keeps it; a window that opened straight
  onto a workspace goes back to the Orchestrator.
- **The panel takes focus on open** (TUI and review parity); Tab or a click
  moves to the terminal.
- **No slim title bar**: the panel shows the name and state, so nothing sits
  above the terminal.
- **The machine row shows for local workspaces too** (as in the task's
  wireframe), not only remote ones.
- **A collapsed sidebar lends its column** to the workspace panel at the
  default width, as it does to a review.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo test` (339 tests), also with `--features visual-check`.
- Column state machine tests: entering and leaving the workspace panel, a
  toggle mid-push, workspace to review and back through the sidebar, a frame
  that skips the whole hop, changing course while a hop is queued.
- Window tests (`src/app_column_tests.rs`): the push and its geometry, the
  terminal at the top of the main area, Esc and the back button restoring
  selection, scroll and focus, the sidebar chord, the review hop, a
  collapsed sidebar, Reduce Motion.
- View tests: the panel with a task, idle, and remote; a long title wraps;
  panel keys, clicks (folder, More, switch rows, Shell, Back), `q`, Tab.
- `--visual-workspace <dir>` renders the panel beside the main area for
  the working, shell, description, idle and remote states.
- Root: `cargo test -p shelbi-app` (the shared module and `parity_doc`),
  `cargo test -p shelbi-tui workspace`, clippy on both.
