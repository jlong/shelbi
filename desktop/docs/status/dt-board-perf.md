# dt-board-perf: the Issues board at frame rate on a large board

## What landed

Scrolling the board (a column, or sideways across columns), moving the
pointer over it and stepping the keyboard's selection now stay within a
frame on a board of 1,180 cards (1,100 of them done), where every frame
took the better part of a second before. The board looks the same: every
Issues and issue-detail frame of `--visual-figma`, and every board frame of
`--visual-views` (drag, empty column, detail, dropdowns), is pixel-identical
before and after, in both themes.

What made it slow, measured first (`--bench-board`, below):

1. **Every card of every column was built on every frame.** `render_board`
   cloned the whole model and built each card (its element id, strings,
   click and drag listeners, the four-layer shadow) every time the window
   drew: a hover crossing an element, a scroll tick, a refresh, any notify
   in the window. On the large board an idle redraw took ~650ms.
2. **Every 750ms refresh rebuilt the model twice**, even when the reads
   were the same board as before: `apply_open` and `apply_closed` each
   rebuilt it, and each rebuild merged (cloned) every task three times.
3. **The open detail rebuilt its model on every frame**, which also merges
   every task on the board.

What changed:

- **Each expanded column is a virtualized gpui `list`** (`board_list.rs`).
  Only the cards in view are built; cards within 400px of the view are
  measured once as they come near. The list keeps its scroll position as an
  item and an offset into it, so a card measured as it scrolls in above the
  view never moves what is on screen. `ColumnList::sync` tells the list
  which items changed when the model does (the span between the common
  prefix and suffix, so the rest keep their measured heights) and keeps the
  view on the card it was on. The rows under the cards (the empty column's
  placeholder, "Loading history…", "Load more") are list items too. Each
  item is padded as the column's flex gap and glow room placed it, so the
  layout is unchanged. Lists are kept by status id, so a column keeps its
  scroll position across a collapse or a filter change.
- **The keyboard's selection is revealed by the least scroll**
  (`ColumnList::reveal`), as the old scroll container did: nothing when the
  card is in view, its top or bottom to the view's edge otherwise. A card
  never measured yet (Up from the first card wraps to the last of 1,100) is
  scrolled to directly, and the list fills the view upward from it.
- **The scroll axes stay as `dt-board-scroll-axes` made them**
  (`board_scroll.rs`). A list takes only y but locks no trackpad gesture, so
  the board's wheel layer (the shift+wheel layer, now `with_board_wheel`)
  also runs gpui's gesture lock (`OngoingScroll`) in the capture phase: a
  gesture locked sideways scrolls the board and never reaches a column. A
  wheel mouse's lines over a column step it by the window's line height, as
  the old scroll container did (a list steps 20px a line). The existing axis
  tests run against list columns now, plus one for the wheel step.
- **A refresh that reads the same board is not folded.** The refresher
  fingerprints each read off the UI thread (its debug rendering, hashed);
  the view folds a read only when its fingerprint changed, or while a change
  is pending (a read is what settles a pending move). A persisted filter or
  column override clears the fingerprint so the next read is folded either
  way. A refresh that does change the board folds both reads with one
  rebuild (`BoardState::apply_reads`), and the merged task list is only
  made when there is a pending change to overlay or settle.
- **The detail's model is cached** by issue id and the board's generation
  (`BoardState::generation`, bumped on each rebuild).

Not done, because the measurements didn't call for it: precomputing card
strings in the model (only the dozen or so visible cards are built now),
paging the Done column, or caching the board view against redraws from
elsewhere in the window (an idle redraw of the whole window is ~2-4ms). A
profile of the remaining scroll frames shows gpui's own work for the visible
items (each list item's layout as its own root, glyph painting, sprite
sorting), with no board code standing out.

## Measurements

`shelbi-desktop --bench-board` (feature `visual-check`, `src/bench_board.rs`)
writes a fixture project into a sandboxed `SHELBI_HOME` (`--open` open issues
across Backlog, Todo, In Progress and Review, `--done` done ones, default 80
and 1,100; titles and excerpts of varied length, some blocked), shows the
board in the real window shell offscreen at 1440x1024, and drives it
synchronously: each step dispatches one input through the window, then draws
a frame. A step's time is the dispatch plus the whole window's render,
layout and paint on the main thread. No display is involved, so the numbers
don't depend on a locked screen.

```sh
cd desktop
cargo run --release --features visual-check -- --bench-board
cargo run --release --features visual-check -- --bench-board --done 51 --open 4
```

The sweeps: `idle` (a redraw with nothing changed: what any notify in the
window costs), `refresh` (the 750ms refresh's reads folded in, then a frame;
the reads themselves happen off the UI thread and aren't timed), `refresh
chg` (the same, folded as a changed board), `scroll board` (a trackpad swipe
across the columns and back), `scroll column` (down the Done column and back
up, 60px a step), `hover` (the pointer down the Done column and across the
board), `select` (the keyboard's selection stepped down the Done column).

These ran on a shared hub with many other workers building: load average
50-58 during the before run, 39-50 during the after runs, on 10 cores. That
inflates every number, the tails most; a quiet machine does better. Before
= the umbrella at `f91e4bcc` plus the bench; after = this branch. Release
builds; 120 steps a sweep (`--steps`).

**1,180 cards (80 open, 1,100 done)**, frame time in ms:

| sweep | before p50 | before p90 | after p50 | after p90 | after p99 | after frames > 16.7ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| first frame | 822.6 | | 3.4 | | | |
| idle | 650.8 | 1048.6 | 4.3 | 11.2 | 54.0 | 6 / 120 |
| refresh | 1250.0 | 1576.2 | 5.1 | 5.8 | 11.8 | 0 / 30 |
| refresh chg | (1250.0) | | 9.7 | 11.4 | 12.7 | 0 / 30 |
| scroll board | 1027.8 | 1735.5 | 4.3 | 9.3 | 18.0 | 2 / 122 |
| scroll column | 1072.1 | 6462.9 | 8.8 | 11.8 | 34.9 | 12 / 242 |
| hover | 677.2 | 3612.9 | 4.2 | 4.9 | 18.9 | 3 / 240 |
| select | 1416.2 | 2766.0 | 8.8 | 10.7 | 21.6 | 4 / 120 |

Before, every refresh folded and rebuilt (there was no unchanged-read
case), so its one `refresh` row stands for both. Before, every frame of
every sweep was over 16.7ms.

**John's board as the index has it today (4 open, a 51-card done page)**:

| sweep | before p50 | before p90 | after p50 | after p90 |
| --- | ---: | ---: | ---: | ---: |
| idle | 13.6 | 25.3 | 2.1 | 2.6 |
| refresh | 27.7 | 42.4 | 2.4 | 2.6 |
| scroll board | 28.0 | 38.1 | 2.0 | 3.4 |
| scroll column | 27.5 | 35.5 | 4.4 | 5.4 |
| hover | 13.7 | 18.3 | 2.1 | 2.8 |
| select | 27.9 | 40.3 | 4.4 | 6.2 |

Even a 55-card board missed most 120Hz frames and many 60Hz ones before;
the Done column's Load more (or the REST fallback's full closed sweep) grows
it from there.

## The real app

Not checked by hand on John's board. The screen was unlocked, but a debug
desktop build from the review worktree was running on it, and opening
another app window on the `shelbi` project would have rewritten that
project's saved window and view state (`desktop-window.json`,
`desktop-view.json`). The bench's real-shell frames stand in.

## Verifying

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings`
  (also with `--features visual-check`) `&& cargo test`: 502 tests pass.
- New tests: `board_list` (the change span, the rows under the cards, sync
  keeping the scroll on its card, a 1,000-card column building only the
  cards near view, and reveal: the last card, the least scroll, a card in
  view staying put), `board_scroll::a_wheel_line_steps_a_column_by_the_line_height`
  and `sideways_dx`, `issues::tests` (when a read is folded, the
  fingerprint), `board_state::one_refresh_folds_both_reads_with_one_rebuild`.
- `--visual-figma` before and after: `scripts/figma-diff.sh` reports the
  same mismatch for `issues-dark` (4.10%) and `issues-light` (5.50%), and
  the frames are pixel-identical (`magick compare -metric AE`: 0).
