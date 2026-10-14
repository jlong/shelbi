# dt-board-dnd: drag and drop on the Issues board

## What landed

- **Drag a card between columns** (`src/views/issues.rs`). A card body is a
  drag source: a ghost of the card follows the pointer, the card stays behind
  as a faint placeholder, and every column (collapsed strips included) is a
  drop target. While a card is dragged each column's header says what a drop
  does ("Drop to move here", "Drop to run the gated merge") or why it can't
  ("Can't drop: not in the `subtask` workflow"); refused columns are dimmed and
  never highlight under the pointer. A drop sends the registry's
  `MoveIssue { id, to_status }` through the window's exhaustive dispatch and
  `execute_mutation_expecting`, carrying the card's read state, exactly like
  the `H`/`L` keys.
- **Reorder within a column.** In the dragged card's own column every card is
  a reorder target: hovering one marks the edge the card lands on (above when
  dragging up, below when dragging down), and a drop takes its place. A drop
  on the column body sends it to the bottom. `Shift+K`/`Shift+J` (the TUI's
  reorder keys) now work in the desktop too, replacing the "not available yet"
  message.
- **Pending / refused / accepted.** While a move or reorder is in flight the
  board shows the card where it is headed (in the target column, or at its new
  slot), dimmed, with a pending row ("Moving to DONE (gated merge)…",
  "Reordering…"). An ack keeps it there until a read confirms it. A refusal
  (another client moved it first, a gated merge failed, the daemon rejected
  the transition) drops the overlay, so the card snaps back to where it really
  is with the daemon's message on it, and the board re-reads.
- **Gated merges.** A drop is a plain `MoveIssue`, which the daemon runs with
  `skip_transition_actions: false`: the transition's actions (push, merge,
  ...) run exactly as for `shelbi issue move`, never a status-only move.
- **Keyboard** is unchanged; drag is additive.

### New shared pieces

- `MutationKind::Prioritize { position }` on the control protocol (bumped to
  v5) and its daemon executor `mutate::apply_prioritize` (shelbi-orchestrator):
  load the issue, recheck, `set_priority(PrioMove::Set(position))`, and print
  the same line `shelbi issue prio` does. The daemon's per-issue queue,
  expected-state gate and change broadcast apply to it like every other
  mutation.
- `shelbi_app`: `CommandKind::PrioritizeIssue { id, position }` and
  `Mutation::PrioritizeIssue`, mapped to the wire kind in `exec_daemon`.
- `shelbi_app::board::drop_target` (what dropping a card on a column does:
  `Reorder`, `Move { merge }`, or `Refused(reason)`) and
  `board::reorder_slot` / `current_slot` (the absolute slot a reorder sends).
- **TUI**: `Shift+K`/`Shift+J` go through the shell's executor persister as the
  registry's `PrioritizeIssue` (so through the daemon), and the standalone
  board keeps its direct store write. Both now compute the slot with
  `reorder_slot`, which fixes a reorder under a workspace filter landing next
  to a hidden card instead of past the visible neighbour.

## Decisions

- **Legality is the card's workflow.** Transitions are any-to-any between the
  statuses a workflow declares (`Workflow::transition_allowed`); the
  `transitions:` block only attaches actions. So a column is refused when the
  card's workflow doesn't declare its status (a subtask on REVIEW), or when the
  workflow isn't loaded. A reorder in a done column is refused too: done shows
  newest first, so a priority there does nothing visible.
- **Refused drops still reach the board's handler.** gpui consumes a drop on an
  element whose `can_drop` is false without running any listener, so the
  status line couldn't say why. Refused columns get no `drag_over` highlight
  and the state's `drop_on_column` refuses (sends nothing, explains).
- **The slot indexes the whole stored column** in the stores' canonical order
  (priority, then id). The board shows a filtered column, so a visible row
  index isn't a slot; the slot is where the target card sits in the full
  column. A drop that wouldn't change the order sends nothing (and so can't
  leave a pending row waiting for a change that never comes).
- **Optimistic placement, not optimistic state.** `dt-issues-board` kept the
  card in place with a pending row. A drop that leaves the card where it was
  reads as a failed drop, and "snaps back" needs it to have gone somewhere, so
  the board now overlays pending moves and reorders on the model. This applies
  to the keys as well (the selection follows the card, like the TUI). Nothing
  is written until the daemon acks; the overlay is rebuilt from the pending
  set on every read.
- **Renumbered columns wait.** A move renumbers its source column and a
  reorder its own column, and every card whose priority shifts gets a new
  `updated_at`. On a local board that is the daemon's revision, so a second
  drag in that column from the old snapshot would be refused as "changed
  elsewhere". Until the first change lands (the next read, under a second),
  the column's other cards answer with "waiting for the last change in TO DO
  to land" instead of sending a mutation the daemon would refuse. The
  control-socket test shows the daemon refusing that stale sibling reorder.
- **CLI `issue prio` is unchanged** (still writes the store in-process); it
  could route through `MutationKind::Prioritize` later, but nothing here
  needed it.

## What's left

- Collapsed strips (empty columns) highlight as drop targets but have no room
  for the "gated merge" hint; the pending row says it after the drop.
- No auto-scroll while dragging near the edge of a horizontally scrolled board
  or a long column.
- A successful hub-side merge needs `gh` to answer: the merge action runs
  `gh pr list` before merging and fails on a non-GitHub origin. That is the
  existing action's behaviour (the CLI fails the same way), not the board's.

## Verification

Rebase (2026-10-14): rebased onto `jlong/desktop-app` at `259540b6`
(after `dt-chat-view`); the only conflict was `desktop/docs/gpui.md`, where
both sets of quirk notes are kept. Re-run after the rebase: from `desktop/`,
`cargo build` ok, `cargo clippy --all-targets -- -D warnings` ok, `cargo test`
140 passed; from the root, `cargo check --workspace --locked` ok and
`cargo clippy --workspace --all-targets -- -D warnings` ok.

Run from `desktop/`:

- `cargo build`: ok.
- `cargo clippy --all-targets -- -D warnings`: ok.
- `cargo test`: 74 passed before the rebase, 126 after (8 new in `views/board_state.rs`: drop legality from
  the shipped `task` and `subtask` workflows, the pending → accepted card with
  the gated-merge label, a refused drop snapping back with the daemon's
  message, reorder by drop through the registry with the renumbered-column
  hold, no-op drops, a refused reorder snapping back, `Shift+J`/`Shift+K` with
  the whole-column slot under a filter, and a move into an empty collapsed
  column; `commands.rs` exhaustive-dispatch tests include `PrioritizeIssue`).

Run from the repo root (shared crates changed):

- `cargo build --workspace`: ok.
- `cargo clippy --workspace --all-targets -- -D warnings`: ok.
- `cargo check --workspace --locked`: ok; root `Cargo.lock` unchanged.
- `cargo test --workspace --no-fail-fast` (with the hub env unset): 3867
  passed, 1 failed. The failure was `board_seam_gate`, which flagged my new
  test helper in `mutate/tests.rs` calling `shelbi_state::list_column`
  directly. The helper now reads through the `IssueStore`, and after that
  `board_seam_gate` and the `apply_prioritize` tests pass on their own.
- After rebasing onto the umbrella (dt-shell-sidebar #1692, terminal element
  #1694): desktop `cargo build` / `clippy` ok and `cargo test` 126 passed;
  root build / clippy / `--locked` ok; `cargo test --workspace` 3903 passed,
  1 failed: `old_session_compat::current_client_drives_each_release_session_through_the_frozen_core`
  ("session socket never appeared"). It fails the same way on a clean
  checkout of `origin/jlong/desktop-app` without this change (hub load average
  ~75-90), and passed before the rebase, so it is not this change; this task
  touches no session code. The touched crates' tests (proto, app,
  orchestrator `mutate`, TUI reorder, `control_socket`, `board_seam_gate`)
  pass on the rebased tree.
- New tests: `shelbi-proto` round-trip of `Prioritize`; `shelbi-orchestrator`
  `apply_prioritize` (lands and clamps, aborts on a stale recheck);
  `shelbi-app` registry effect + `drop_target` against the shipped workflows +
  `reorder_slot`; `shelbi-tui` reorder through the persister with the
  whole-column slot; `crates/shelbi-cli/tests/control_socket.rs`
  `a_prioritize_reorders_the_column_and_a_stale_sibling_reorder_is_refused`
  (a real daemon).

By hand, against a throwaway sandbox (its own `SHELBI_HOME` under `/tmp`, hub
env stripped, `sleep` as the agent runner, a bare-repo origin, a `dev`
worktree holding `feat/m1`):

- Dragging a subtask over REVIEW dimmed it with "Can't drop: not in the
  `subtask` workflow" while BACKLOG / TO DO said "Drop to move here"; releasing
  there moved nothing and the status line said why.
- Dragging a backlog card to TO DO showed "Moving to TO DO…" in TO DO, then
  settled; `events.log` had `backlog -> todo reason=user:app` and
  `shelbi issue list` showed it in todo.
- Dragging a review card with no branch to DONE showed "Moving to DONE (gated
  merge)…" in DONE, then snapped back to REVIEW with "merge for `m2` failed;
  leaving it in `review` ..." on the card (`events.log`: `merge task=m2 ...
  status=failed`). `shelbi issue move` of an identical card failed with the same
  message.
- Dragging the review card on `feat/m1` to DONE (with a `gh` stub answering "no
  PRs" and commit signing off in the sandbox repo) ran the gated merge:
  `merge task=m1 ... status=ok`, then `review -> done`, and origin `main` got
  the squash commit.
- Dragging Charlie onto Alpha in TO DO marked Alpha's top edge, then landed
  Charlie on top; the issue files read `c=0 a=1 b=2 x=3` and `shelbi issue list`
  matched.
- The sandbox TUI's board showed the same columns and order, and its
  `Shift+J` moved Charlie below Alpha (persisted).
