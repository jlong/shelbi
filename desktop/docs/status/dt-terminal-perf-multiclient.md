# dt-terminal-perf-multiclient: terminal performance, concurrent clients

## What landed

- **The bench** (`src/bench/`, feature `bench`; one command:
  `scripts/bench-term.sh`). A real window, the real `TerminalPane`, real
  sessions in a sandboxed root. Inputs (`src/bench/corpus.rs`):
  - `claude`: a captured Claude Code session (`benches/corpus/claude-code.cap.gz`,
    829 KB of a 10-minute worker session recorded with `--bench-record`: the
    transcript replay, tool calls and their output, the spinner);
  - `yes`: `yes | head -1000000` as the terminal receives it;
  - `cargo`: a colored `cargo build` log in cargo's own escapes (bold green
    `Compiling`, yellow `warning:` blocks with blue gutters, the `Building
    [===>  ]` line redrawn with `\r` + erase-line);
  - `redraw`: a full-screen redraw loop on the alternate screen (every cell
    repainted per frame, true-color meters, `htop` style).

  Each is streamed two ways: `direct` (straight into the pane's event channel
  at 32 MiB/s, about 96 MiB per input, to stress the pump, emulator and
  element) and `session` (a sandboxed session's PTY, about 6 MiB, end to
  end). Reported per case: fps, frame time p50/p95/p99/max (root render
  through paint), dropped frames, CPU (process, all threads, share of one
  core), throughput, main-thread feed time, resyncs, and the **palette
  probe** (the real palette overlay opened every 400 ms from another
  thread's timer; time until a frame paints it). Afterwards the pane's grid
  is compared cell by cell with a second client that saw the same stream
  (the TUI's rendering path) and with the session's replay. Then **idle**
  (frames and CPU, 5 s) and **many** (six workspaces streaming, one shown
  through the real `SessionView`; frames and CPU while a quiet view is
  shown, then the switch latency to each). `--json` appends a result line.
- **Optimizations** (where the numbers said):
  - `CellMetrics::cached`: measuring the cell called gpui's
    `all_font_names()`, which enumerates every installed font (about 125 ms
    here), on **every new pane's first frame**. Every session shown paid it,
    so switching to a workspace took about 140 ms with the UI thread blocked.
    Now measured once per family and size: **switch 141 ms -> 21 ms**
    (about one frame after the replay lands).
  - The app window no longer repaints on unchanged refreshes:
    `ShelbiWindow::apply_feed` notified on every feed snapshot (every 5 s
    while the daemon pushes, every 750 ms while polling) even when nothing
    changed, so an idle window repainted (0.4 fps measured, terminal
    included). It now notifies only when the sidebar model, the link, or a
    session title changes: **0 frames idle** in the full app.
  - `layout_row` no longer allocates a string per cell (characters go
    straight into their run's text).
- **Concurrent clients** (`scripts/multiclient-check.sh`, rerunnable): one
  sandboxed session (bash) with the desktop app (a real window,
  `--bench-client`, 100x30, typing through gpui's key dispatch and input
  handler), the TUI (tmux, 150x45, main area 122x45), and `shelbi attach`
  (tmux, 80x24). Four rounds (desktop, TUI, attach, desktop): the typing
  client becomes active, the PTY follows its size (debounced), and every
  client's screen is checked against the session's own snapshot placed by
  the shared viewport rules (letterbox or clip). 20 checks per run.
- **Full-app idle** (`scripts/idle-cpu.sh`): the real app on a sandboxed
  project whose orchestrator is `sleep`; CPU time of the app's pid over 30 s
  after a 25 s settle, and frames counted from a new per-frame trace event
  (`shelbi_desktop::terminal::frame`).
- **A session replay bug, found by the bench and fixed** (`shelbi-session`):
  a row whose last painted cell had a background color left that color on
  the pen when the replay's CRLF scrolled, and the emulator fills a scrolled-in
  line with the current background, so blank cells below diff highlights
  turned green in the replay (136 cells after the Claude Code capture). A
  client attaching later, or one recovered by a resync, showed them.
  `emit_row` now ends on a reset pen; regression test
  `a_colored_row_end_does_not_bleed_into_the_rows_below`
  (`crates/shelbi-session/tests/replay_roundtrip.rs`). After the fix, the
  session-source `claude` and `cargo` cases' replays are cell-identical.
  Sessions keep the binary they started with, so this reaches running
  sessions only as they're relaunched on a new `shelbi`.
- Logging: `TerminalPane` logs `attached` at debug, and each painted frame
  at trace (`RUST_LOG=shelbi_desktop::terminal::frame=trace`).

## Numbers

Apple M4 (10 cores), macOS 26.6.2, a 60 Hz display, release build. The hub
was busy throughout (load average 40 to 66 from other workers), so absolute
numbers are noisy; before and after ran back to back. "Before" is this
branch's bench with `element.rs`, `layout.rs`, and `metrics.rs` at
`bd3ed502` (the `apply_feed` fix doesn't touch the bench window). Session
cases used the installed `shelbi` 0.10.0.

Before:

| Input | Source | MiB/s | fps | frame p50 / p95 / max (ms) | dropped | CPU | palette p50 / max (ms) | grid |
|---|---|---|---|---|---|---|---|---|
| claude | direct | 32.0 | 60 | 2.14 / 3.37 / 25.9 | 0 | 36% | 12.5 / 25.6 | identical |
| yes | direct | 32.0 | 60 | 0.12 / 0.49 / 1.0 | 0 | 55% | 16.4 / 17.9 | identical |
| cargo | direct | 32.0 | 60 | 1.10 / 1.64 / 2.7 | 0 | 35% | 15.1 / 20.4 | identical |
| redraw | direct | 32.0 | 60 | 1.56 / 2.47 / 5.3 | 0 | 32% | 9.9 / 16.6 | identical |
| claude | session | 43.7 | 55 | 0.82 / 4.22 / 4.2 | 0 | 44% | 11.5 / 11.5 | identical |
| yes | session | 1.8 | 60 | 0.18 / 0.55 / 1.2 | 0 | 19% | 13.0 / 17.9 | identical |
| cargo | session | 34.7 | 60 | 1.15 / 2.70 / 2.7 | 0 | 48% | 15.0 / 15.0 | identical |
| redraw | session | 4.0 | 60 | 1.47 / 2.36 / 4.1 | 0 | 24% | 11.7 / 16.4 | identical |

After:

| Input | Source | MiB/s | fps | frame p50 / p95 / max (ms) | dropped | CPU | palette p50 / max (ms) | grid |
|---|---|---|---|---|---|---|---|---|
| claude | direct | 32.0 | 60 | 2.08 / 3.18 / 21.5 | 0 | 35% | 16.9 / 18.3 | identical |
| yes | direct | 32.0 | 60 | 0.16 / 0.52 / 1.3 | 0 | 54% | 13.6 / 17.8 | identical |
| cargo | direct | 32.0 | 60 | 1.10 / 1.79 / 2.2 | 0 | 36% | 14.2 / 18.5 | identical |
| redraw | direct | 32.0 | 60 | 1.51 / 2.34 / 5.1 | 0 | 32% | 16.9 / 18.6 | identical |
| claude | session | 43.0 | 54 | 0.96 / 4.34 / 4.3 | 0 | 42% | 15.8 / 15.8 | identical |
| yes | session | 1.8 | 59 | 0.16 / 0.55 / 1.3 | 1 | 17% | 9.8 / 17.1 | identical |
| cargo | session | 36.5 | 57 | 1.03 / 2.83 / 2.8 | 1 | 47% | 4.1 / 4.1 | identical |
| redraw | session | 5.6 | 60 | 1.47 / 2.32 / 2.7 | 0 | 26% | 13.9 / 16.6 | identical |

"grid" is against the second client. Against the session's replay, the
`claude` session case differed by 136 cells before the replay fix (with
the installed CLI in both runs above) and is identical with a `shelbi`
built from this branch. The session-source `claude` and `cargo` streams
drain in under 0.2 s (7 to 10 frames, one palette probe); `yes` and
`redraw` through a PTY run at 2 to 6 MiB/s for 1 to 4 s. A Mac PTY is slow
on a loaded machine: `script` + `yes | head -3000000` alone moved 1 to 2
MB/s here, mostly in system time.

Main-thread feed time, direct (about 3 s of streaming each), before -> after:
`claude` 650 -> 647 ms, `yes` 1667 -> 1620 ms, `cargo` 819 -> 841 ms,
`redraw` 544 -> 541 ms.

Idle, many, and switching:

| | Before | After |
|---|---|---|
| Bench idle (pane attached to a quiet session, 5 s) | 0 frames, 0.28% CPU | 0 frames, 0.28% CPU |
| Full app idle (`idle-cpu.sh`, Chat attached, focused, 30 s) | about 0.4 fps of repaints from the feed | 0 frames, 0.27% CPU |
| Hidden views: quiet view shown, six streaming at 200 KiB/s (4.2 s) | 0 frames, 0.3% CPU | 0 frames, 0.3% CPU |
| Switch to a streaming workspace (until its content paints) | p50 141 ms, max 153 ms | p50 21 ms, max 23 ms |
| While one streams (200 KiB/s) | frame p95 4.0 ms, 3.6% CPU | frame p95 4.0 ms, 3.8% CPU |

Switch breakdown after (per switch): the pane is up at 2 ms, the replay
(130 to 195 KB) is fed by 3 to 4 ms, and the frame that shows it paints at
8 to 23 ms (the next display refresh). Before, the same paint landed at
about 140 ms, the cell measurement's font enumeration in between. (The
"connect" spread's 4 s maximum is the very first attach, to a session still
starting under load, not a switch.)

### Where the time goes (profiles)

`sample` on a symbolized release build (recipe in `../gpui.md`), main
thread, direct floods:

- `claude`: about 70% waiting (run loop, drawable); about 15% parsing in
  the emulator; layout, shaping, and paint each under 1%.
- `yes`: 55% waiting; about 38% in alacritty's per-line scroll
  (`scroll_up_relative` 19%, `Row::reset`, `Grid::scroll_up`). Layout 0.1%,
  taffy 0.1%.

## Acceptance

- [x] **The harness exists and runs with one command**
  (`scripts/bench-term.sh`); before/after numbers for each input are above.
- [x] **Under `yes` the UI stays interactive and frame time p95 < 16 ms**:
  the palette opens in 9.8 to 13.6 ms p50, 17.8 ms worst, and frame time
  p95 is 0.52 ms (direct, 32 MiB/s) and 0.55 ms (session). The worst frame
  across all inputs and sources was 21.5 ms (one, in the `claude` flood);
  p99 stayed under 9 ms.
- [x] **Idle CPU with one attached session < 1%**: 0.27 to 0.30% for the full app
  (`idle-cpu.sh`), 0.28% for the bench window. 0 frames in both.
- [x] **The concurrent-client script passes**: `scripts/multiclient-check.sh`,
  20 of 20, with the TUI and `shelbi attach` from this branch
  (`target/debug/shelbi`) and from the installed 0.10.0.
- [x] **Hidden terminal views cost no frames; switching is immediate**:
  0 frames while six hidden sessions stream; a switch paints its content
  21 ms after the click, one display refresh after the replay lands.

## Decisions

- **Hidden views detach** (the existing `SessionView` design, kept). The
  "Removing tmux" plan says scrollback is held only for sessions being
  viewed, and the TUI's `SessionManager` drops the connection when it
  switches away. A hidden view has no pane, no connection, and no parsing,
  so it costs nothing. Keeping hidden panes attached would have made a
  switch instant without a reconnect, but every hidden session's output
  would be parsed in the background and its 10,000-line scrollback held in
  memory. The reconnect turned out to be cheap (attach about 1 ms, replay fed
  by 4 ms); the 140 ms was the font enumeration, now gone. (A hidden pane
  would cost no frames either: gpui only repaints windows that rendered the
  notifying entity, see `../gpui.md`.)
- **No restructuring of the output path.** The task suggested coalescing
  output into one emulator feed per frame, caching shaped runs, and batching
  background quads. The profiles say rendering is already cheap: layout,
  shaping, and paint are each under 1% of the main thread, and every
  frame-time p95 is under 5 ms. The pump already caps each turn at 256 KiB
  and yields, so frames and input interleave. Layout is damage-driven, and
  gpui's line-layout cache already reuses shaped runs across frames. gpui
  paints at most once per display refresh however often the pane notifies.
  Feeding only at frame time (`on_next_frame`) would have throttled inactive
  windows to 30 fps (gpui's energy rule for forced frames). The remaining
  main-thread cost under a flood is alacritty's own scrolling, shared with
  the session and the TUI. Moving the emulator off the UI thread (behind a
  lock, as Zed does) would free that time, but nothing measured here needs
  it; see "What's left".
- **Two sources.** A real session is the honest end-to-end path, but on this
  Mac the PTY tops out at a few MiB/s under load, which never stresses the
  client. The direct source is the stress case.
- **The bench's window doesn't take focus**, so the user isn't interrupted.
  The bench's idle case is therefore unfocused (no blink); `idle-cpu.sh` runs
  the real app, which activates its window and so covers the focused path.
- **The Claude Code capture is from this task's own worker session**,
  recorded passively (no input, no resize, so the PTY wasn't touched). It
  holds this repo's paths and this worker's transcript, nothing else; the
  scan for tokens, keys, and email addresses was clean. Committed gzipped
  (51 KB).
- **The concurrent-client check compares screens to the session's snapshot**
  through the shared viewport rules (computed independently in the script's
  checker): exact for the desktop app (its grid and its painted viewport)
  and `shelbi attach`, and as a located block for the TUI (its sidebar
  surrounds the main area). `shelbi attach`'s ` detach: Ctrl+]` hint on the
  bottom row before its first key is allowed.

## Verification

Rebased (2026-10-14) onto `jlong/desktop-app` at `3eb326a3`
(`dt-menus-notifications`, `dt-review-view`). Their `apply_feed` gained the
notifier, `FeedEvent::Layout`, the review row, and a menu sync on every feed
event; the merged version keeps all of it behind the same change check (an
identical snapshot has nothing new for the notifier, which diffs against its
last model, and the menus derive from the model), so an idle window also
stops reinstalling its menus every refresh. Everything below was re-run on
the rebased tree unless it says otherwise.

From `desktop/`:

- `cargo build`: ok
- `cargo clippy --all-targets -- -D warnings`: ok (also with `--features
  bench` and `--features visual-check`)
- `cargo test`: 282 passed (283 with `--features bench`: the corpus test)
- `cargo run --features visual-check -- --visual-check <dir>`: 79 checks,
  0 failed; `--visual-workspace`: 5, 0 failed; `--visual-review`: 4, 0 failed
- `scripts/bench-term.sh` (via the release binary): the tables above (run
  before the rebase; the terminal code is unchanged by it)
- `scripts/multiclient-check.sh`: 20/20 with both CLIs
- `scripts/idle-cpu.sh 25 30` (with `SHELBI_BIN=../target/debug/shelbi`):
  0.30%, 0 frames (0.27% before the rebase)

From the repository root (`shelbi-session` changed):

- `cargo build --workspace`: ok
- `cargo clippy --workspace --all-targets -- -D warnings`: ok
- `cargo check --workspace --locked`: ok (the root `Cargo.lock` is unchanged)
- `cargo test -p shelbi-session`: 77 passed, including the new replay test
  (it failed before the fix)
- `cargo test --workspace --no-fail-fast` (`TASK_ID`, `PROJECT`, and the hub
  variables unset): 4051 passed, 0 failed (4032 before the rebase)

## What's left

- **Emulator off the UI thread.** Under a sustained `yes` flood the UI thread
  spends about half its time in alacritty's scroll (1.6 s of a 3 s, 96 MiB
  stream). The UI stays responsive (frames and the palette above) and real
  PTYs deliver far less. If a future case needs the headroom, the move is
  the parse onto the session's forwarder thread with the view behind a
  lock; the pane's `view` accesses are the surface to change.
- **Backpressure from the client.** The client's reader drains its socket
  greedily (`shelbi-client` reader, unbounded), so the session's
  lagging-client resync never triggers for the desktop app; a client that
  parsed slower than its session produced would buffer in memory. Not
  observed (0 resyncs, and a PTY is far slower than the client), so left as
  is.
- **Focused-window idle in the bench.** The bench keeps its window
  unfocused; the focused path is covered by `idle-cpu.sh` (the real app).
- **Display refresh assumed 60 Hz** for the dropped-frame count
  (`REFRESH` in `src/bench/mod.rs`); on a 120 Hz display the count would be
  against the wrong period.
