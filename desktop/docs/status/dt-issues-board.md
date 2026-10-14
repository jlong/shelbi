# dt-issues-board: the Issues view and task detail

## What landed

- **The board** (`src/views/issues.rs`). Clicking Issues in the sidebar shows
  the project's kanban: the columns from `workflows/statuses.yaml` (narrowed by
  the workflow filter), the cards that bucket under each in board order
  (priority, or newest-first in a done column), and per card the title, a lock
  badge when a dependency isn't done, the workflow badge (when more than one
  workflow is loaded and none is filtered), the branch, the assignee and the
  dependency list. Empty columns collapse to a narrow strip; clicking a header
  toggles it and persists the override to `state.json`, shared with the TUI.
  The board scrolls horizontally, each column vertically, and the selection
  scrolls into view.
- **Live updates.** Two background refreshers on the TUI's 750ms cadence: the
  open board from the daemon's index (`read_open_board`, never GitHub), and
  the done/canceled history on its own refresher (`read_closed_page`, the
  done-history cache), so the done column fills in after the open board and
  never blocks it ("Loading history..." until it lands). "Load more (m)"
  fetches the next history page, user-driven only, like the TUI.
- **Keyboard** (`src/views/board_state.rs`, through `src/keys.rs`). Keys are
  the user's TUI keys, rebinds included: board keys dispatch through
  `keys.yaml`'s `kanban:` map (h/l and arrows between columns, j/k within,
  Enter/Space opens the detail, H/L move, Tab cycles the workflow filter, r
  refreshes, o focuses the card's workspace), the detail through `popover:`
  (Esc/Enter/Space/q close, j/k/u/d/g scroll, H/L move clamped), and the
  dropdowns use the TUI's fixed keys (f, w, m, c, j/k, Enter, Esc). The TUI has
  no start / assign / add / edit keys, so the desktop adds `s` (start), `a`
  (assign, a workspace picker), `n` (new issue), `e` (edit in place) and `E`
  (open in the editor), routed outside the keymap the way the TUI routes
  `f`/`w`/`m`, so the shipped `keys.yaml` is untouched.
- **Mutations.** Move, start, assign, edit and add leave the board as registry
  commands (`CommandKind`) through the window's exhaustive dispatch
  (`commands.rs`, whose `view:tasks` and issue arms now do real work), become
  the registry's `Effect::Mutate` (`CommandKind::effect_with_project`), and go
  to the daemon off the UI thread through the new
  `shelbi_app::execute_mutation_expecting`, carrying the card's read state
  (`IssueCard::expected`: stored status + `updated_at`). One mutation per issue
  at a time; the card shows a pending row ("Moving to REVIEW...") until a read
  shows the change.
- **The refusal path.** When another client changed the issue first, the daemon
  refuses (`ExecError::Conflict`); the card drops its pending state, shows the
  daemon's message in red with a dismiss button (cleared by the card's next
  action), and the board re-reads, so the card lands where it really is.
- **Filters**: the workflow filter (per session, like the TUI) and the
  workspace filter (persisted in `state.json`, so it follows the TUI's and
  vice versa), as header chips with counted dropdowns. The TUI board has no
  search, so neither does this one.
- **Task detail** (`src/views/task_detail.rs`): id, status and priority,
  workflow, workspace, branch, dependencies with their status, a PR link when
  one is recorded, created/updated, and a checklist count; the body renders
  with gpui-kit's Markdown view (headings, lists, code, links, task-list
  checkboxes; links open in the browser). Buttons for move, start, assign,
  edit and open in editor. **Edit in place** opens a gpui-kit text area on the
  body; Cmd+S saves through `EditIssue`, Esc cancels. **Open in editor** writes
  the body to a scratch file and launches the configured editor
  (`shelbi_state::resolve_editor`); each save is sent through `EditIssue`.
- **Add issue**: `n` or the header's "New issue" opens a title prompt; Enter
  sends `AddIssue` (the daemon files it in Backlog).

### Moved into shared crates

- `shelbi_app::board` (new): column layout (`columns_from`), status resolution
  (`resolve_task_status`, `BoardRules`), legal moves (`adjacent_column`), card
  order (`sort_column_cards`), the card branch (`card_branch`), column labels,
  collapse state (`ColumnExpansion`), the workspace / workflow filters and
  their options, the open + closed merge, the board readers
  (`read_open_board`, `read_closed_page`, `load_more_closed`) and the
  `BoardInput` fold. All moved out of `crates/shelbi-tui/src/kanban.rs`, which
  now calls them (its own tests still pass unchanged).
- `shelbi_app::view`: `IssuesModel::build` (the full board model, replacing
  the unused thin `from_board`), richer `IssueCard` (assignee, deps, read
  state), `BoardFilters`, `TaskDetailModel`.
- `shelbi_app::refresh::{Refresher, spawn_worker}`: the TUI shell's generic
  refresher, moved; `shelbi-tui/src/shell/refresh.rs` re-exports it.
- `shelbi_app::execute_mutation_expecting`: `execute_mutation` with the
  caller's expected state instead of one re-read at send time.

## What's left

- **Reorder (J/K)** reports "reordering isn't available in the desktop yet".
  The registry has no priority mutation and the daemon no priority op (the
  TUI writes the store directly); every desktop mutation goes through the
  registry and the daemon, so this needs a `ReorderIssue` command plus a
  daemon mutation first.
- **`o` (open workspace)** emits `FocusWorkspace`, which still shows the
  not-implemented banner until `dt-workspace-view`.
- **Project**: the window shows one project, picked by `src/project.rs`
  (`SHELBI_PROJECT`, else the cwd's project, else the first open, else the
  first registered). `dt-shell-sidebar` owns switching and should rebuild the
  `IssuesView` per project.
- **Change-bus wakes**: the board polls on the 750ms fallback cadence; the
  daemon's change subscription (the TUI's `shell/changes.rs`) arrives with
  `dt-shell-sidebar`'s refresh pipeline.
- **PR link**: issues carry no PR record and a live `gh` lookup would spend
  GitHub budget, so the detail shows a PR link only when a param holds a
  `.../pull/<n>` URL (`board::pr_link`).
- **Fonts**: the board uses the window's font; Geist isn't wired yet. Colors
  are `shelbi_app::theme` tokens (via `src/theme.rs`), so the board follows
  the `theme:` setting and the system appearance.
- **Drag and drop**: `dt-board-dnd`.

## Decisions

- **No optimistic moves.** The task asks for a pending state until the daemon
  acks or refuses, so a card stays in its column with a pending row instead of
  jumping like the TUI's optimistic move. An acked mutation stays pending
  until a read shows it (a move until the card sits in its target, anything
  else until `updated_at` changes), so a read already in flight can't flash
  the old state.
- **The expected state is what the user saw.** A move/start/assign carries the
  card's state from the snapshot on screen. An in-place edit carries the state
  from when the editor opened, so a save refuses if someone edited the issue
  meanwhile. External-editor saves use the card's state at each save (each
  successful save bumps `updated_at`, which would otherwise refuse the next).
- **Keys from `keys.yaml`**, not a desktop table, so the desktop and the TUI
  can't drift and rebinds apply to both.
- **Commands round-trip through the window's dispatch** (`IssuesEvent::Command`
  to `commands::dispatch_command` to `issue_command`), so the board's keys and
  any later palette entry share the one exhaustive match.
- **Terminal editors open in Terminal.app** (`osascript`), since a window app
  has no tty; GUI editors spawn directly. The watcher stops when a spawned
  editor exits, or after four hours for a Terminal-hosted one.
- **Monochrome**: column headers use `muted` / `white` instead of the TUI's
  per-category colors; cards fill with `search`, the selected one with
  `selection`; the workflow badge keeps its badge tokens; refusals use `red`.
- **Daemon-bound commands wait for bootstrap.** Until `dt-platform-bootstrap`
  reaches Ready no CLI launcher is installed, and
  `shelbi_state::ensure_daemon_running` (inside `execute_mutation`) would fall
  back to `current_exe()`, which is the app. The window refuses
  `sends_to_daemon` commands with "Still connecting to Shelbi" until then.

## Verification

Run from `desktop/`:

- `cargo build`: ok.
- `cargo clippy --all-targets -- -D warnings`: ok.
- `cargo test`: 66 passed after the rebase onto the umbrella (26 of them
  this task's: board rendering from `IssuesModel` fixtures,
  keyboard-to-command mapping, the refusal path and pending settle, edit
  staging, job-to-mutation mapping, markdown rendering of the Feature / Bug
  task-body templates and the welcome card, keystroke conversion, editor
  launch, dispatch).

Run from the repo root (shared crates changed):

- `cargo check --workspace --locked`: ok; root `Cargo.lock` unchanged.
- `cargo build --workspace`: ok.
- `cargo clippy --workspace --all-targets -- -D warnings`: ok.
- `cargo +1.88 check --workspace --all-targets --locked`: ok.
- `cargo test --workspace --no-fail-fast` on the rebased tree: 3858 passed,
  0 failed. (A run before the rebase had 2 timing failures, both in relay /
  tty tests this change doesn't touch, at a hub load average of ~70; both
  passed when rerun alone.)
- New tests: `shelbi-app` board/view tests (56 in the crate), and
  `crates/shelbi-cli/tests/control_socket.rs`
  `a_board_move_from_a_stale_card_is_refused_after_a_cli_move`: a real daemon,
  a card read from the board model, `shelbi issue move` between read and move,
  then the card's move sent with `IssueCard::expected` (what
  `execute_mutation_expecting` sends) is refused as stale naming the real
  status, nothing changes, and the re-read card sits where the CLI put it. It
  drives `ControlClient` directly like its neighbours: an earlier version
  called `execute_mutation_expecting` with `SHELBI_*` set process-wide, which
  made two neighbouring tests' daemons time out when the file ran in
  parallel. The file's five tests pass in parallel.

By hand (`cargo run`, against the live `shelbi` project, read-only): the board
showed the same columns, cards and badges as the TUI; h/j/l/Enter navigated and
opened the detail; d paged the body (blockquote, lists, inline code, the
Acceptance Criteria checkboxes); `e` opened the editor and Esc cancelled; `f`
opened the workspace dropdown with counts; `n` opened the title prompt; `l`
scrolled the board right to a 50-card done column, newest first. No mutation
was sent to the live board. After the rebase onto the platform-bootstrap and
theme-tokens work, the board rendered again in the light theme, cards kept
their height in a long column, and `j` scrolled the column with the
selection.
