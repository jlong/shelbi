# dt-figma-visual-harness: render the Figma screens, diff them

## What landed

Two tools for the Figma redesign's QA: an offscreen renderer of every mockup
screen fed with the mockups' own content, and a diff script against the
reference PNGs in `~/.shelbi/projects/shelbi/design-refs/desktop-figma/`.
Usage is in `desktop/README.md` ("Pixel QA against the Figma mockups").

```
cargo run --features visual-check -- --visual-figma /tmp/vf   →  /tmp/vf/<screen>-<theme>.png   (20 frames, 1440x1024)
scripts/figma-diff.sh /tmp/vf                                 →  /tmp/vf/diff/<screen>-<theme>-{side,heat,overlay}.png
                                                                  + mismatch table (diff/report.txt)
```

- **`--visual-figma <dir> [--2x]`** (`src/visual_figma.rs`). The real
  `ShelbiWindow` (a new `ShelbiWindow::figma` constructor: `--visual-review`'s
  offscreen shell plus workspace and chat deps) in a fresh 1440x1024 window per
  screen, over a fake daemon and fake sessions, against a sandboxed
  `SHELBI_HOME` at `/tmp/shbfig-<pid>` (hub, control and `SHELBI_HUB_ADDR`
  variables unset; removed at the end). Each screen is reached the way a user
  reaches it: a sidebar row selected then its view shown (as a click does),
  the review opened through the window, the task detail through `open_task`,
  the palette through `open_palette`. Both themes by re-running itself
  (`visual_check::per_theme`, the existing `SHELBI_VISUAL_THEME` mechanism,
  now also usable by a mode that names frames by theme). Checks: the board's
  counts (10 / 6 / 2 / 1), the eight Activity rows newest first, the review
  panel opening, five Machines workspaces with the `gpu` box reachable, all ten
  screens saved.
- **Terminal transcripts** for the Orchestrator (Codex header box, dispatch
  log), Workspace (the "Add project switching" run) and Review (Claude Code's
  verification), with SGR bold, dim, green, red and cyan. A fake session's
  `SessionIo::resize` answers with the session's own `Resized` and the screen
  redrawn at that size, so the composer and footer lines sit on the pane's
  last rows as in the mockups. The cursor is hidden (the mockups draw none).
- **Seams** (all `visual-check`-gated): the Activity view reads its clock
  through `activity::now()`, which `pin_clock` pins (2026-10-17 15:00 local, so
  2m / 6m / 12m / 18m / 25m / 40m / 1d and Today / Yesterday read as drawn);
  `machines::stub_reachable` answers every remote reachable without an SSH
  probe; `ActivityState::select_item` and `MachinesState::select_row` select
  without opening; `ShelbiWindow::offscreen_select_target`,
  `offscreen_open_settings`, `offscreen_open_task`, `offscreen_views`.
- **Scale.** gpui draws the hidden window at the main display's backing scale.
  Frames are always saved at 1x: a 2x capture is downsampled (triangle
  filter). `--2x` opens each window on the first Retina display instead and
  keeps the native frame. **Not verified here:** this machine had only two
  1x external displays attached during the work (the built-in Retina panel
  wasn't listed), so `--2x` took its fallback ("no display draws at 2x") and
  the 2x capture path has not been seen producing a 2880x2048 frame.
- **`scripts/figma-diff.sh <render-dir> [ref-dir]`** (ImageMagick 7). Per
  pixel the largest channel difference; a pixel mismatches above `FUZZ`%
  (default 10). Writes side-by-side (labelled), heatmap (red to yellow over
  the dimmed reference) and 50% overlay, and a table of the mismatch % and the
  largest regions' boxes (connected components after a `GROW`-px dilation,
  so a line of text is one region). The references' rounded window corners sit
  on Figma's #f5f5f5 canvas and are masked out (`CORNER`, 28 px, measured from
  the PNGs: the canvas reaches 6 px in along the diagonal). A render of another
  size is scaled to the reference's first. About 90 s for all 20.

## Figma MCP

Not used for this task: the harness needs the mockups' content, not their
style values, and the 1x PNGs carry all of it. The style values are the
restyle tasks' to pull.

## Content: what matches, and where it can't

The fixtures follow the mockups: "My project", Ready for review "Desktop
application (umbrella)" `jlong/desktop-app` and the three Queued rows,
workspaces alpha (working, Developer), bravo (idle), charlie (awaiting input,
Developer), delta (idle), Zen Mode on; the board's 19 open cards with the
mockup's titles, excerpts and branches (three invented Backlog cards fill the
count of 10 below the fold) and a Done card; the umbrella's detail (id, Review,
priority 1, workflow task, @review, branch, created 2026-10-12, updated
2026-10-16, checklist 4/5, its body and headings); the Activity events; the
Machines rows (hub local; alpha and bravo working, charlie idle, review
serving; gpu `ssh gpu.lan` reachable, `[gpu, linux]`, `/usr/local/bin/shelbi
v0.10.0`, gpu-1 idle; 5 workspaces); the palette's projects (Shelbi,
ContextStore, 32pixels).

Where today's views show something else, no fixture can make them agree; these
are design changes for the restyle tasks, or inconsistencies inside the
mockups:

- **Fields the views don't show (yet):** the sidebar's `#401` and ⌘ shortcuts
  (shown since `dt-figma-sidebar`, and the fixture's reviews carry `#401`);
  Machines' runner column ("Codex", "Claude": the view shows the agent, e.g.
  `developer`) and task titles in the assignment cell (the view shows
  `in_progress: <id>` then the title); the issue detail's date-only created /
  updated (the view prints times); the palette's command list (it lists the
  review rows, "Turn Zen Mode off" for Zen on, "Open error log", "Quit
  Project").
- **Activity vocabulary:** the feed has no "available" or "started" verbs.
  charlie's row is its nearest workspace event (`awaiting input`), the daemon
  start the generic daemon line (`daemon  event  started`). Yesterday's "alpha
  finished Share theme tokens" carries the `review` pill a finish has (the
  mockup shows `[done]`). The header names the project slug (`my-project`;
  the mockup reads "Shelbi").
- **Mockup inconsistencies:** the umbrella card's excerpt ("Build a native
  desktop client with an issue board, live agent terminals, and a queue for
  reviewing completed work.") differs from its detail body; the fixture follows
  the detail. The sidebar has bravo idle where Machines has it working, and
  Activity says alpha builds project switching where the sidebar and Workspace
  panel say the umbrella; each screen gets its own mockup's version (the
  sidebar is a fixture model, the views read disk). Settings' "Shelbi CLI"
  shows `/opt/homebrew/bin/shelbi` in placeholder gray; the app's placeholder
  is "Found automatically" and the fixture sets no value.
- **Workspace idle:** the mockup keeps the nav sidebar with an idle header
  ("· idle charlie", the worktree, a Shell button) in the main area; the app
  pushes the workspace panel and shows its idle placeholder.

## Baseline (today's umbrella, `e286bef1`)

`figma-diff.sh` defaults (FUZZ 10, GROW 6, CORNER 28), dark then light. A box
the size of the frame means the background itself differs (the dark canvas
is a different color everywhere).

| screen | dark mismatch | largest regions (dark) | light mismatch | largest regions (light) |
|---|---:|---|---:|---|
| orchestrator | 21.00% | 1440x1024+0+0 | 5.07% | 953x533+11+6, 1438x78+2+936, 251x171+10+353 |
| issues | 46.83% | 1440x1024+0+0, 131x44+1299+5, 108x32+1129+67 | 10.44% | 1172x960+268+62, 251x171+10+353, 250x129+564+381 |
| issue-detail | 29.05% | 1440x1024+0+0, 131x44+1299+5, 158x30+534+12 | 22.93% | 1438x1024+2+0, 219x118+10+526, 231x57+10+265 |
| activity | 24.41% | 1440x1024+0+0, 139x68+1301+284, 136x32+1285+492 | 8.11% | 1207x246+233+109, 251x171+10+353, 432x118+275+362 |
| review | 20.44% | 1157x1024+0+0 | 3.68% | 897x264+12+41, 845x69+242+945, 570x34+263+73 |
| workspace | 21.60% | 1420x1024+0+0 | 3.82% | 674x305+265+73, 251x157+12+101, 605x33+265+39 |
| workspace-idle | 19.56% | 1440x1024+0+0, 56x24+1384+10 | 1.69% | 634x104+268+79, 247x112+14+324, 217x119+14+111 |
| machines | 21.90% | 1440x1024+0+0, 489x66+661+140, 243x49+284+215 | 5.95% | 1171x90+269+47, 251x171+10+353, 152x250+11+14 |
| settings | 22.47% | 1424x1024+0+0, 88x23+524+396, 44x23+462+396 | 5.69% | 1141x230+283+301, 593x185+283+668, 745x132+283+534 |
| command-palette | 8.81% | 879x577+241+6, 1198x79+242+935, 229x308+10+14 | 80.46% | 107x25+940+346, 87x25+940+319, 71x23+940+268 |

The light palette's 80% is the mockup's scrim: it dims the whole light frame,
which the app's palette doesn't. Two renders of the same build give the same
numbers.

## Checked

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo test` (421 passed), and `cargo clippy --all-targets --features
  visual-check -- -D warnings`. No `crates/` change, so no root workspace run.
- `--visual-figma` in both themes: 0 checks failed, 20 frames at 1440x1024,
  no `/tmp/shbfig-*` left behind. Looked at beside its reference: every dark
  frame, and the light workspace and machines frames.
- `figma-diff.sh` on that render: 20 pairs, the three images each, the table
  above.
- `--2x` with no Retina display attached: the fallback note, 1x frames (see
  Scale).
