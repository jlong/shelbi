# dt-instant-panel-push: the push starts on the input's next frame

## What landed

Opening a review or a workspace starts the column's push on the frame after
the click or Enter, and the push runs without a long frame.

```
click "Warm the application cache" in Ready for Review
  next frame: the push is moving; the panel shows the title, "Loading…" for
              the rest, the main area an empty canvas
  ~400ms:     the resolve answers mid-push; the panel fills in where it stands
  220ms:      settled; the reviewer's session attaches
```

Two things held the push back before:

1. **The swap waited on the daemon.** A review row's click ran the load flow
   (`LoadStep::Resolve`, a background read of the review state and the issue
   tracker), and only its `Open` routed the window to the review, so the push
   began after that round trip.
2. **The first frames carried the attach.** The review's (and a workspace's)
   content session attached as the view was shown: its connect, the replay
   fed into the emulator and the terminal's first layout landed in the
   frames the push was running in. The push's clock started at `now` in the
   render that built all that, so a slow first frame skipped the fast opening
   of the ease-out curve: a pause, then a jump.

What changed:

- **The review panel goes up on the input** (`ShelbiWindow::open_review_pending`,
  `ReviewView::show_pending`): what the sidebar row knows (title, slot,
  server state) and "Loading…" placeholders for the description and server;
  only Back works until it resolves. The resolve fills it in where it stands
  (`open_review` fills a shown review instead of re-opening it; the
  placeholder's content, built for the slot the row guessed, is replaced).
- **A failed load reverses the push from where it stands**
  (`abandon_pending_review`), and the banner shows once it is back.
- **The slot prompt opens over the placeholder.** Choosing a slot keeps the
  panel up while the daemon loads it; dismissing the prompt (or the busy /
  no-slot report) backs out.
- **Leaving cancels the open** (`ReviewLoader::cancel`, new in `shelbi-app`):
  a resolve still running, or a queued load's later `ReviewOpened`, opens
  nothing after the user went back. A load already sent runs on, and a
  failure of it still reports.
- **Attaches wait for the push to settle.** The window holds the review's
  and the workspace's content (`hold_content`) while the column is heading to
  the panel or still moving, and releases it on the first settled frame
  (`release_settled`). The main area shows an empty canvas meanwhile, not the
  pane's "No session". The orchestrator chat waits for a pop back to it the
  same way. Switching between workspaces with the panel already in place
  attaches at once.
- **The push's clock starts at its first frame** (`ColumnSwap::pace`): a
  swap from rest is pinned one 60Hz step in at its first drawn frame (so
  that frame already moves, however long ago the input was), and its second
  frame adds at most one more step (so a slow first frame can't skip the
  curve's opening). A swap turned around mid-way runs on the clock from the
  turn, so a reversal never jumps.

## Measurements

`--bench-push` (feature `visual-check`, `src/bench_push.rs`) opens a real
`ShelbiWindow` over fake review and workspace daemons and fake sessions:
each run opens a review (the `LoadReview` host call a click dispatches),
goes Back, opens a workspace (`ShowView`), goes Back. The fake resolve
answers after 400ms; every session replays 256 KiB of styled agent output.
It reports, per push: the input to the end of the first frame that moved the
column, the frames drawn before it, each frame's main-thread time (the
window's render through paint) and the interval between frames.

The screen was locked while this was measured, and macOS draws no frames
for a window on a locked screen, so these runs used `--offscreen`: the
window is hidden and the bench draws it itself on a 60Hz timer that fires on
the main thread (main-thread work between frames makes a frame late, as it
would miss a vsync). The ~18ms "gap max" in every row is that timer's wake
jitter, not a dropped frame. Before = the umbrella at `e2f1181a` plus the
bench; after = this branch.

Release (`--release`), 5 runs; the first row of each is the first open since
launch:

| Push | Before: first move | Before: still frames | Before: worst frame | After: first move | After: still frames | After: worst frame |
|---|---|---|---|---|---|---|
| review in (first) | 570ms | 26 | 134.5ms (150ms gap) | 0.8ms | 0 | 0.7ms |
| review in (later) | 435ms | 26 | 2.0ms | 0.7-1.4ms | 0 | 1.4ms |
| workspace in | 17-20ms | 1 | 3.3ms | 0.6-5.2ms | 0 | 5.1ms |
| review / workspace back | 16-19ms | 1 | 0.7ms | 0.6-1.4ms | 0 | 1.4ms |

Debug (`cargo run`), 3 runs:

| Push | Before: first move | Before: still frames | Before: worst frame | After: first move | After: still frames | After: worst frame |
|---|---|---|---|---|---|---|
| review in (first) | 595ms | 26 | 150.0ms (166ms gap) | 2.9ms | 0 | 2.8ms |
| review in (later) | 458-460ms | 26 | 11.9ms (30ms gap) | 3.7-8.1ms | 0 | 8.0ms |
| workspace in | 32-34ms | 1 | 11.9ms (30ms gap) | 3.9-8.3ms | 0 | 8.3ms |
| review / workspace back | 19-22ms | 1 | 4.5ms | 3.7-3.9ms | 0 | 3.9ms |

"First move" before is the 400ms fake resolve plus the push's own first
still frame; with the real daemon it is whatever the resolve takes. After,
the column moves in the input's own frame, whatever the daemon does.

In release no frame of any push came near 8.3ms (120Hz), let alone 16.7ms.
The before runs dropped frames in every review push in debug (30ms gaps as
the session attached mid-push) and one 150ms frame in the first.

**What is left:** the first terminal the app ever shows still costs one
long frame (131ms release, 144ms debug, the "after max" column): the
terminal's first-use setup, a pre-existing cost that also sat in the
first push before. It now lands just after the push settles, as the
reviewer attaches. Warming it before the first open is a separate change.

## Tests

- `column_swap.rs`: the first frame is one step in however late it comes,
  the second is capped, a 60Hz run is never held back, a turn mid-way and a
  hop through the sidebar keep their clocks.
- `app_column_tests.rs`: the push moves on the input's next frame with the
  placeholder up before the resolve has run, fills in without a jump, and
  attaches only once settled; a load that fails mid-push turns back from
  where it stands with the banner; the slot prompt shows over the panel and
  dismissing it backs out; a slow first frame still shows one step; a
  workspace attaches once its push settles. The existing first-frame
  assertions now expect the first step.
- `shelbi-app` `review.rs`: `a_cancelled_open_opens_nothing`.
- `--visual-review` adds `review-pending.png` (the placeholder panel, light
  and dark) and its check.

## Verification

From `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings`
(and with `--features visual-check`), `cargo test` (382 passed),
`--visual-review` and `--visual-workspace` 0 checks failed in both themes.
From the root: `cargo test -p shelbi-app --lib review` (41 passed),
`cargo clippy -p shelbi-app --all-targets -- -D warnings`. `Cargo.lock` is
unchanged.
