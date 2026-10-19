# dt-figma-activity: the Activity view after the Figma frame

## What landed

The Activity view restyled to the Figma Activity frame (dark `2165:364`,
light `2220:21092`), in both themes. Values come from the Figma MCP
(`get_design_context` on the dark frame, `get_variable_defs` on the dark
main content `2165:455` and the light frame), checked against the 1x PNGs
with ImageMagick.

- **Header**: 16 across, 8 down, 8 between its two lines. "Activity" in
  Base Medium, loud; beside it, 8 on, the project's **display name** in
  X-Small muted (it was the slug). The name comes from the project config,
  read on the feed's refresher thread beside the log (`ProjectInfo`), never
  on the UI thread.
- **Filters**: the frame's own controls (`style::filter_track`,
  `filter_segment`, `filter_dropdown`). Kind segments sit in a bare row 4
  apart, with no track. Each is 24 tall, 8 across, X-Small; muted at rest,
  loud on the `selection` chip when selected. The selected chip still slides
  (`SlideTrack`). The pickers start 16 after the kinds: `theme.search`'s
  fill, the icon, an X-Small `gray` label, no chevron. The board's
  `dropdown` and Settings' `segment` are unchanged.
- **Day groups**: 40 tall, a 12pt rule, the X-Small label in
  `theme.ansi_dark_gray`, a rule to the edge.
- **Rows**: 8 across, 4 down, 6 corners. The actor sits in the 84pt column
  in Base Regular. The verb is in `muted`, 8 before the message. The title is
  Base Regular in the frame's `theme.gray`. The second line is X-Small in
  `ansi_dark_gray`, with `⎇` on a branch. On the right, the tag is Base
  Medium in brackets, colored by status, then the time in `ansi_dark_gray`,
  8 apart. The selected row is a full-width band in `row_selected`
  (`theme.background` dark, `theme.selection` light, as measured:
  `#383d4c` / `#dee1ec`). The accent pill at its edge is gone, as drawn.
  Rows hover in `background`.
- **Paging line**: "Start of the log", 10pt, "8 events loaded" (the frame's
  three spaces), X-Small muted, 16 down. "Load older" keeps its button.
- **Key hints**: a 26pt bar under a divider, 16 across, X-Small muted.

### Colors, by kind and status

| What | Color | Figma |
|---|---|---|
| `zen` | `green` | `theme.ansi_green` |
| `you` | `log_text` (new role) | `theme.gray` as this frame sets it |
| machine actors (a host, the daemon as `system`, a workspace's state ping, a freed workspace) | `muted` | `theme.muted` |
| a review workspace (tagged `review` in the project) | `theme_cyan` | `theme.cyan` |
| other workspaces | their hue: alpha `cyan`, bravo `blue` | `theme.ansi_cyan`, `theme.ansi_blue` |
| `[done]` / `[ready]` / `[review]` / `[in progress]` | `green` / `blue` / `magenta` / `yellow` | `theme.ansi_*` |
| verb | `muted`; a system row's status tint (`unreachable` red) stays | `theme.muted` |
| title | `log_text` | `theme.gray` |
| second line, time, day label | `dark_gray` | `theme.ansi_dark_gray` |

`log_text` is new: the Activity frame overrides `theme.gray` to `#7b85a7`,
opaque, in **both** modes (the palette's `gray` is `#acb4cfe5` dark). It
falls short of AA (4.30 on the dark canvas, 3.65 on white, under 3 on the
selected band). It is listed in the contrast exceptions and
`docs/theme.md`, flagged for John like the other Figma shortfalls.

## Desktop labels (`views/activity_labels.rs`)

The shared feed (`shelbi_app::activity`) and the TUI are untouched. The
desktop words a row its own way, over the shared `FeedRow`:

- **Tags show where the task is now.** A tag is the task's latest status
  among the loaded events, so a finished task reads `[review]` until it
  merges and `[done]` after. "ready for review" reads `review`, as drawn.
- **"available"**: a worker's worktree released after its handoff
  (`worktree-detach … status=ok`) reads `charlie available Workspace is
  ready for another issue`, actor muted. That is the event that frees a
  workspace for another issue. A failed detach keeps the shared wording.
- **"started"**: the daemon's own lines (`daemon <action> [reason=…]`) read
  `system <action> Shelbi daemon`, with the reason on the second line.
- **bravo is blue.** The shared tints give bravo magenta, the `[review]`
  tag's color. The frame draws it blue, so the desktop trades bravo's and
  echo's hues; alpha, charlie, delta and foxtrot keep theirs.

Two labels are recognized by the shared row's own words: the daemon's
`daemon` chip with the `event` verb, and the `detached` verb in the ok
tone. Unit tests parse real log lines through the shared parser, so a
change to those words fails them.

## Harness changes

`--visual-figma`'s fixture now carries the mockup's events, its charlie row
a `worktree-detach … status=ok`. The project YAML names the project
"Shelbi" (`name: Shelbi`, replacing the deprecated `display_name: My
project`); the sidebar's "My project" is the fixture sidebar model's and
doesn't change. A new check expects the desktop's verbs: `merged,
promoted, finished, is building, is building, available, finished,
started`. The Machines and Issues headers now read "Shelbi" too.

## Checked

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings
  && cargo test` (428 passed), and `cargo clippy --all-targets --features
  visual-check -- -D warnings`. No `crates/` change, so no root workspace
  run.
- `--visual-figma` in both themes: 0 checks failed. Looked at both Activity
  renders beside their references.
- `scripts/figma-diff.sh` (defaults), Activity before and after:

  | frame | before | after |
  |---|---:|---|
  | activity-dark | 10.27% | 4.98% |
  | activity-light | 8.41% | 3.86% |

  The largest regions left are in the sidebar column, which belongs to
  `dt-figma-sidebar`: it is 257pt wide here, 280pt in the frame, so every
  main-area pixel sits 23pt to the left. With the main area cropped past
  that offset (the frame from x 280, the render from x 257, 1140x1000), 0.28%
  of pixels differ in dark and 0.38% in light: Geist's glyph advances
  against Figma's, and the larger picker icons. The other 18 frames change
  only where they print the fixture project's name, which now reads "Shelbi"
  (Machines' mockup draws "Shelbi" there; the Issues and issue-detail
  mockups draw no name in that header, so the board's restyle drops it).

## Not matched, and why

- **Picker icons are 20pt; the frame draws them at 14.** Every desktop icon
  is the 24/outline drawing at 20pt, guarded by
  `assets::every_icon_draws_at_20_pixels`. That was John's
  `dt-icons-mini-outline` decision, not a Taskpage shape, so this task keeps
  it. The pickers stay 24 tall (2pt in around the icon). **Flagged for John**
  if the Figma 14pt should win.
- **The right edge.** In the frame, the tags and times reach x 1438: the
  frame's main content is 1183pt wide in a 1160pt window, so it overflows.
  The render keeps the 16pt gutter.
- **"hub · cli 0.10.0"** under the daemon row: the daemon line carries no
  host or version, so the row has no second line. The paging line sits 18pt
  higher than drawn as a result.
- **"⇧↑/⇧↓ page"**: the real Activity page keys are PageUp/PageDown, so the
  hint bar shows `⇞/⇟` from the user's key map. It keeps showing the bound
  keys rather than the mockup's.
