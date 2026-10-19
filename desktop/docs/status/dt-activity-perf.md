# dt-activity-perf: the Activity feed paints at once and scrolls at frame rate

## What landed

Opening Activity now paints real rows on its first frame (the window reads
the feed when the project opens), or within a read when it is opened before
that read lands, with no poll interval to wait out. Scrolling, hovering and
stepping the keyboard's selection stay within a frame on a feed of 6,000
rows, where every frame took seconds before. A live refresh that reads the
log as it was rebuilds nothing, and one with new events splices them in
without moving what is on screen. The feed looks the same: the
`--visual-figma` Activity frames are pixel-identical before and after, in
both themes.

What made it slow, measured first (`--bench-activity`, below):

1. **Every row was built on every frame.** The feed was one scroll
   container holding every day header and row, so a hover, a scroll tick or
   any notify in the window built all of them: on the whole 6,000-line log
   an idle redraw took seconds.
2. **The first rows waited on the read and the poll.** The view started its
   reader only when first shown and picked each read up on its next 100ms
   poll, and on a remote board a read returned no row until the tracker had
   answered for the page's titles. Titles past the first batch then waited
   a 750ms refresh each.
3. **Every refresh rebuilt the feed on the UI thread**, its labels and
   filter included, even when the read was the log as it was.

What changed:

- **The feed is a virtualized gpui `list`** (`activity_list.rs`), the board
  columns' approach (`dt-board-perf`): the day headers, rows and the rows
  under them (the empty note, Load older and its count) are list items, and
  only those in or within 400px of view are built. Each item carries a key
  of two hashes (`ItemKey`): `ident`, the row without its time, and
  `content`, all it draws (the relative time, the task's status for its
  pill, a reviewer chip's bold). `FeedList::sync` splices the span where
  idents came or went (new events at the top, older ones at the bottom) and
  re-measures items outside it whose content changed, so the list keeps its
  scroll on the row it was on. Items are padded as the old container placed
  them, so the layout is unchanged. The keyboard's selection is revealed by
  the least scroll (`board_list::reveal`, shared with the board), and a
  wheel line steps the line height (`board_scroll::wheel_list`).
- **Reads wake the view** (`activity_reader.rs`). The reader thread sends
  each read on an async channel the view awaits, so a read is folded the
  moment it lands. The 750ms cadence now only asks for a read.
- **The window prefetches.** A live window starts the project's reader when
  it opens (and on a project switch), and the Activity view takes it when
  first shown, so the first read is usually already there.
- **What's at hand first** (`shelbi_app::activity::read_activity_window_staged`).
  A read whose titles wait on the tracker is sent twice: first with the
  titles the cache and the local board index hold, then once the tracker
  answers. A read that left ids for a later batch reads again at once, so
  titles fill in a batch at a time rather than one batch per 750ms. The TUI
  keeps `read_activity_window` and `resolve_task_meta_into`, unchanged in
  behavior.
- **The feed is built off the UI thread.** Each read builds its labels and
  feed on the reader thread under the filter the view last showed. The view
  adopts a read only when its keys or choices differ from what it shows, and
  rebuilds on the UI thread only when the filter changed since that read.

Not done, because the measurements didn't call for it: caching each row's
element between frames (only a screenful is built now), or paging the
parsed log further (a 6,000-line read lands in about a second on a loaded
machine, off the UI thread).

Also on this branch, separately committed: the task panel's Back button no
longer fills blue when the keyboard is on it (`task_panel.rs`, one line). A
gpui shadow paints as a filled box under its element, and Back's fill is
translucent, so the focus ring's blue showed through the whole square; the
panel's opaque color now sits under the fill while it is focused.
`--visual-workspace` gained a `workspace-back-focused.png` frame for it.

## Measurements

`shelbi-desktop --bench-activity` (feature `visual-check`,
`src/bench_activity.rs`) writes a fixture project into a sandboxed
`SHELBI_HOME`: 500 tasks (one in ten left unwritten, as a deleted task) and
an `events.log` of 6,000 lines over 6 days, each task's lifecycle across five
workspaces. It shows Activity in the real window shell offscreen at
1440x1024 and drives it synchronously: each step dispatches one input
through the window, then draws a frame, and the step's time is the dispatch
plus the whole window's render, layout and paint on the main thread. No
display is involved, so the numbers don't depend on a locked screen.

```sh
cd desktop
cargo run --release --features visual-check -- --bench-activity
```

`first rows` is from the sidebar's Activity click to the first frame with
rows (the first 500-line page), `titles` until every task row on it has its
title. Then the window grows to the whole log (as Load older a dozen times
would) and the sweeps run: `idle` (a redraw with nothing changed), `refresh`
(the 750ms refresh's read, the log as it was, folded, then a frame; the read
itself is off the UI thread and not timed), `refresh new` (the same with a
new line appended first), `scroll` (a trackpad swipe down the feed and
back), `hover` (the pointer down the feed), `select` (the keyboard's
selection stepped down the feed). `prefetched` opens a second window, waits
500ms, then shows Activity.

These ran on a shared hub with many other workers building: load average
around 50 on 10 cores for both runs. That inflates every number, the tails
most. Before = `d1c76842` (this branch's base plus the bench); after = this
branch. Release builds; 120 steps a sweep.

**First paint**, in ms:

| measure | before | after |
| --- | ---: | ---: |
| first rows (500 rows) | 5,042.7 | 63.2 |
| titles of the first page | 5,043 | 63 |
| prefetched: Activity's first frame | | 6.4 (500 rows on it) |
| whole log: read and settled | 65,581 | 1,848 |
| whole log: its first frame | 21,243.6 | 2.4 |

**6,000 rows (the whole log)**, frame time in ms:

| sweep | before p50 | before p90 | after p50 | after p90 | after p99 | after frames > 16.7ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| idle | 5,466.5 | 11,668.7 | 2.1 | 3.8 | 135.5 | 4 / 120 |
| refresh | 3,703.3 | 5,253.6 | 3.6 | 4.5 | 24.0 | 1 / 30 |
| refresh new | 4,247.6 | 5,457.4 | 3.4 | 3.6 | 4.6 | 0 / 30 |
| scroll | 10,938.6 | 36,547.3 | 4.5 | 5.1 | 18.9 | 5 / 242 |
| hover | 6,951.4 | 18,477.5 | 2.1 | 4.1 | 27.5 | 3 / 240 |
| select | 7,358.4 | 10,703.8 | 4.4 | 5.0 | 29.4 | 5 / 120 |

Before, every frame of every sweep was over 16.7ms. After, the few frames
over it are isolated spikes in otherwise steady sweeps (an idle redraw does
the same work each time), which fits the main thread losing its core on the
loaded machine. The 469 untitled rows are the fixture's deleted tasks, which
never resolve.

The bench's refresh step polls the reader's channel instead of blocking on
it: the view's own wake loop also listens there (its task can't run while the
bench holds the main thread), and it could take the channel's one wakeup and
leave a blocked receive waiting forever, which hung one run.

## The real app

Not checked by hand. The screen was unlocked, but a release desktop build
from the hub checkout was running on the `shelbi` project, and opening
another app window on it would have rewritten that project's saved window
and view state. The bench's real-shell frames stand in.

## Verifying

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings`
  (also with `--features visual-check`) `&& cargo test`: 599 tests pass.
- Root (`shelbi-app` changed): `cargo build --workspace`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test --workspace` (4,189
  tests pass), and `cargo check --workspace --locked` (neither `Cargo.lock`
  moved).
- New tests: `activity_list` (a 6,000-item feed building only the items
  near view, a new top row keeping the on-screen row where it was, new
  events at the top keeping the scroll on its row, a row that only reads
  differently staying put, `runs`), `activity_state` (a read that changes
  nothing shown is not adopted, a read built under an older filter is
  rebuilt under the shown one, a row a minute older keeps its ident but not
  its content), and `shelbi_app::activity::a_staged_read_shows_the_local_titles_before_asking_the_tracker`.
- `--visual-figma` before and after: the Activity frames are pixel-identical
  (`magick compare -metric AE`: 0, both themes), and `scripts/figma-diff.sh`
  reports the same mismatch for `activity-dark` (1.85%) and `activity-light`
  (2.09%).
