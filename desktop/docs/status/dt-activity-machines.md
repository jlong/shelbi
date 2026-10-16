# dt-activity-machines: the Activity feed and the Machines view

## What landed

- **Activity** (`src/views/activity.rs` over the toolkit-free
  `src/views/activity_state.rs`). The event feed, newest first, built by the
  shared `shelbi_app::activity::build_feed`, which the TUI's Activity view now
  paints too: the same rows, in the same order, with the same glyphs and
  tones (the `[name]` identity chip in the workspace's tint, the dim verb, the
  title with its trail (`— checks failed`, `(18 attempts)`), the
  category-tinted `[state]` pill, the relative time with `took Xm · `, the
  `⎇ branch` / detail second line, the `── Today ──` day headers, consecutive
  infra events folded into one row, heartbeats never shown).
  - Filters as native controls: kind pills (All · Tasks · Zen · Workspaces ·
    System, a multi-select union like the TUI's All / Zen / Workspaces pills),
    a project picker and a workspace picker (the choices are the projects and
    workspaces the loaded events name). The TUI's `activity:` keys work too
    (`z` / `w` toggle, `a` resets, `r` refreshes).
  - Paging: the read is windowed. `read_activity_window` parses only the
    newest 500 lines of `events.log` (reading backwards from the end in 64 KiB
    blocks), and "Load older" re-reads with a window one page larger. The
    footer row says how many events are loaded, "Loading older events…" while
    a page is in flight, or "Start of the log".
  - Live: the view's refresher re-reads on the TUI's 750ms cadence. Titles
    resolve incrementally into a cache the refresher keeps across reads (the
    shared `resolve_task_meta_into`: one batched fetch per pass, done issues
    and misses frozen, fetch errors backed off), never on the UI thread.
  - Clicking a row (or `j` / `k` then Enter) opens it: a task row routes the
    window to the Issues board and opens that task's detail
    (`IssuesView::open_task`); a workspace row runs
    `CommandKind::FocusWorkspace` through the window's dispatch (which shows
    the "Not implemented yet: workspace:<name>" banner until
    `dt-workspace-view` lands, exactly as the sidebar's workspace rows do).
- **Machines** (`src/views/machines.rs` over `src/views/machines_state.rs`).
  Every declared machine with every workspace on it (review slots included),
  from the shared `shelbi_app::view::MachinesModel`:
  - the machine header: name, `local` / `ssh <host>`, live reachability for a
    remote (`○ checking…`, `● reachable`, `● unreachable: <why>`), its tags,
    and its recorded remote binary (`<path> v<version>`, flagged when
    incompatible, or "not set up — run `shelbi machine setup`");
  - each workspace: its observed state (`working` / `awaiting` / `blocked` /
    `paused` / `serving` / `idle`, from the poller's `status.yaml`), the agent
    and the STATE cell `shelbi workspace list` prints (`in_progress: <id>`,
    `review: <id>`, `idle`; agent from the task's `agent:`, default
    `developer`, or `review` for a review slot), the task's title, its branch,
    and for a review slot the review server (`serving` / `starting`) and its
    URL (click to open it in the browser);
  - reachability runs on the shared `ReachabilityProber`'s own thread (30s
    cadence, backing off to 5 minutes on failure), so a down host reads
    "checking…" and then "unreachable" without ever blocking the window;
  - clicking a workspace (or Enter) runs `FocusWorkspace`.
- Routing: `commands::view_stub_id` no longer stubs Activity or Machines; the
  window builds each view on first show (like the board) and hands it focus.
- `cargo run --features visual-check -- --visual-views <dir>`
  (`src/visual_views.rs`): renders both views offscreen against a sandboxed
  `SHELBI_HOME` (short `/tmp` path, hub socket variables unset) holding a
  fixture project (a local machine with a dev and a review slot, an SSH
  machine whose host can't resolve), tasks, a workspace status and an
  `events.log`. It checks the rows, folding, the Zen filter, the
  workspace-list state cell and the unreachable remote, and saves
  `activity.png`, `activity-zen.png`, `activity-picker.png` and
  `machines.png`.

### Moved into shared crates

- `shelbi-app::activity` (new): everything toolkit-free that lived in the
  TUI's `activity.rs`: the `events.log` parser (`Event`, `SystemEvent`,
  `parse_event_line`, the zen-reason parser), the title cache and its
  incremental resolver (`ActivityMetaCache`, `resolve_task_meta_into`),
  `read_activity_data` (the TUI shell's full read), and the row content the
  TUI used to build inline as ratatui spans, now plain data: `FeedRow` (chip,
  verb, title, trail, pill, time, secondary, click target) with `Tone` token
  names instead of colors, `build_feed` (filter, fold, day headers, the
  `took Xm` pairing), `relative_time`. New: `ActivityFilter` (kinds, project,
  workspace), `ActivityKind`, `event_workspace`, `filter_choices`,
  `read_activity_window` / `read_last_lines` (paging), and a `project` field on
  task / workspace / dry-run events (the parser now keeps the `project=` scope
  it used to drop). The TUI keeps only the terminal layout (`paint_row`, the
  pill row, scrolling) and paints the shared rows; its output is unchanged.
- `shelbi-app::reachability`: the `ReachabilityProber`, moved from
  `shelbi-tui/src/reachability.rs`. Its `Drop` no longer joins the thread (a
  probe can be mid-SSH for its 10s deadline; a view dropping it must not wait
  that out); `stop()` still joins.
- `shelbi-app::view`: `MachinesModel` is now the real model both clients
  render (`MachineRow`, `MachineWorkspace`, `SlotAssignment`, `ReviewSlot`)
  with a pure `MachinesModel::build(&MachinesInput)`, `probe_targets`,
  `fold_reachability`, and the labels both clients show
  (`reachability_label`, `remote_binary_label`, `workspace_state_badge`). The
  TUI's `MachinesApp` holds it instead of its own `MachineEntry` types.
- `shelbi-app::theme::Tone`: a token name a shared model can ask for
  (`Tone::Cyan`, `Tone::DarkGray`, ...); each client resolves it with
  `Tone::pick` (`Theme::tone` in the desktop).
- `shelbi-orchestrator::machines_model` (new): the IO half of the machines
  view, shared by both clients. It reads the project, the machine records,
  each workspace's status, and the board's in-progress and review tasks the
  way `shelbi workspace list` does (the daemon's board index, review-slot
  ownership from the assignment overlay), resolves a review slot's serving
  state (the sidebar's `review_slot_is_serving`) and URL (the workflow's
  `review:` template with the slot's port), and each task's branch.
- `shelbi-orchestrator::machine::probe_host_reachability`: the SSH probe the
  TUI's machines view used, now shared by both clients.
- `shelbi-core::Reachability`: moved from `shelbi-orchestrator::machine`
  (re-exported there) so the toolkit-free machines model can carry it.

## What's left

- `FocusWorkspace` from either view shows the not-implemented banner until
  `dt-workspace-view` lands; the routing is in place.
- Opening a task that is no longer on the board (a done task older than the
  board's loaded history) shows the board's "This issue is no longer on the
  board" panel.
- `shelbi workspace list` also probes each *idle* slot for a stray session
  (`orphaned session`, `user shell`). That is a per-workspace SSH probe the CLI
  runs once per invocation; the views don't repeat it every refresh. They show
  machine reachability instead.
- The feed is a plain scroll container, not a virtualized list: gpui's `list`
  re-anchors to the top on every splice, which fights a feed that grows at the
  top every 750ms. A few thousand rows (several "load older" pages) render
  fine; a virtualized list with key-anchored scroll is the follow-up if a
  very deep history needs it.

## Decisions

- **Shared row content, not shared painting.** The TUI's activity rendering
  was one function per event kind building ratatui spans. The *content* of a
  row (which chip, verb, title, pill, tone, secondary) moved to `shelbi-app`
  as plain data with `Tone` token names; only the layout stayed per client.
  That is what guarantees "the same rows, order and glyphs" without the
  desktop depending on ratatui. The TUI's tests for row content and layout
  still pass unchanged against the shared builder.
- **All projects by default.** The TUI's feed shows every project's events
  (the log is hub-wide); the desktop does the same by default, with the
  project picker to narrow it. A project filter hides lines that name no
  project (ssh tunnels, legacy lines).
- **Zen dry-runs count as Zen.** The TUI's Zen pill matched only zen-driven
  task transitions; the shared kind classifier puts `zen-dryrun` previews
  under Zen too (they were otherwise reachable only under All). The TUI's
  pill follows.
- **Paging reads less, not just shows less.** "Load older" re-reads with a
  bigger window rather than slicing a full read: the hub's `events.log` here is
  ~43k lines / 7 MB, and the desktop re-reads every 750ms. The TUI shell keeps
  its full read (`read_activity_data`) and is unchanged.
- **Machines state is `shelbi workspace list`'s.** The STATE cell and agent
  follow the CLI exactly (in-progress assignment first, then a review slot's
  task, else idle). The observed state badge is the poller's `status.yaml`,
  as the TUI's machines view shows it.
- **Activity / Machines keys.** Activity uses the user's `activity:` key map
  (ScrollUp / ScrollDown move a row selection, PageUp / PageDown ten rows,
  Home / End the ends) plus Enter to open; Machines uses the TUI machines
  view's fixed keys (up / down / `j` / `k`, Enter, `r`). Modified keys always
  reach the window (Ctrl+H / Ctrl+L focus).

## Verification

Run from `desktop/`:

- `cargo build`: ok.
- `cargo clippy --all-targets -- -D warnings`: ok (also with
  `--features visual-check`).
- `cargo test`: 129 passed before the rebase (151 after). New: `activity_state` (feed rendering from event
  fixtures, filters, the TUI's filter keys, paging state, selection and Enter,
  click targets, selection following its row across reads) and
  `machines_state` (key navigation across machines, click to focus,
  reachability surviving reads and touching remotes only, failed reads), plus
  the routing test now expecting no Activity / Machines stub.
- `cargo run --features visual-check -- --visual-views <dir>`: 7 checks ok;
  the frames show the feed (chips, pills, folded ssh run, branch lines, day
  headers, "Start of the log"), the Zen filter, the project picker, and the
  machines view with `in_progress: t1` / `review: r9  starting` and the
  remote's `● unreachable: ssh: Could not resolve hostname …`.

Run from the repo root (shared crates changed):

- `cargo build --workspace`: ok.
- `cargo clippy --workspace --all-targets -- -D warnings`: ok.
- `cargo check --workspace --locked`: ok. The root `Cargo.lock` gains one
  edge (`shelbi-app → tracing`, already in the tree); no version changes.
- `cargo +1.88 check --workspace --all-targets --locked`: ok.
- `cargo test -p shelbi-app`: 132 passed (the moved parser / title-cache /
  reachability tests, plus new feed, filter, paging and machines-model tests).
- `cargo test -p shelbi-tui --lib -- activity machines`: 49 passed (the TUI's
  row-content and layout tests unchanged against the shared builder).
- `cargo test --workspace --no-fail-fast` on the pre-rebase tree (load
  average ~72): 3900 passed, 5 failed. All five are real-process session or
  PTY tests this change doesn't touch. Run alone, three passed
  (`login_env_controlling_tty`, the PTY input test, `remote_session_e2e`).
  `session_survival` and `old_session_compat` kept failing ("session socket
  never appeared" within 10s) on that old base, but both pass on the umbrella
  tip (`a2790602`, after dt-chat-view's session changes) and on this branch
  rebased onto it.

After rebasing onto `a2790602` (conflicts in `app.rs` / `commands.rs` /
`gpui.md` with dt-chat-view, resolved keeping both: the chat, Activity and
Machines all route natively, and `focus_main_view` hands focus to whichever
is shown):

- desktop: `cargo clippy --all-targets -- -D warnings` ok, `cargo test` 151
  passed.
- root: `cargo clippy --workspace --all-targets -- -D warnings` ok,
  `cargo check --workspace --locked` ok, `cargo test -p shelbi-app -p
  shelbi-tui` 751 passed, `session_survival` and `old_session_compat` pass.
