# dt-figma-qa-terminal-screens: pixel QA of the terminal screens

Pixel QA of `dt-figma-terminal-screens` against John's frames: the main
content of Orchestrator (2160:1307 / 2220:20936, main 2164:173), Workspace
(2165:186 / 2220:21038, agent terminal 2172:991), Review (2165:275 /
2220:21066, reviewer terminal 2168:679) and Workspace idle (2174:851 /
2220:21583, main 2174:855), dark and light. Values came from the Figma MCP
(`get_design_context`, `get_metadata` on those nodes) and were measured in
the renders with ink-row scripts (first and last inked row and column per
line, dominant colors). Images: `~/.shelbi/projects/shelbi/design-refs/
desktop-figma/qa/dt-figma-qa-terminal-screens/` (not committed).

## Method

`cargo run --features visual-check -- --visual-figma <dir>`, then two diffs:

- **Main area, aligned.** The sidebar is not this task's (the render's column
  is still 256pt plus its 1px edge where Figma's is 280pt,
  `dt-figma-sidebar`), and it shifts the whole main area 23pt. So the
  reference is cropped from x=280 (281 for Review, after its divider) and the
  render from x=257, both 1160pt wide, and diffed with `figma-diff.sh`
  (`CORNER=1`). These are the numbers that measure this task.
- **Whole frame**, `figma-diff.sh` as is, for the record.

`--2x` needs a Retina display; none is attached here (John's main display is
a 1x external), so it falls back to 1x. Close inspection used 3-6x nearest-
neighbor zooms of the 1x renders instead (`zoom-*.png`).

## Mismatch, before and after

Share of pixels whose largest channel differs by more than 10%.

| screen | theme | main before | main after | frame before | frame after |
|---|---|---:|---:|---:|---:|
| orchestrator | dark | 3.32% | 3.01% | 5.79% | 5.54% |
| orchestrator | light | 3.37% | 3.07% | 4.60% | 4.36% |
| workspace | dark | 0.97% | 0.91% | 3.39% | 3.37% |
| workspace | light | 0.96% | 0.90% | 3.41% | 3.39% |
| review | dark | 1.15% | 1.00% | 3.63% | 3.51% |
| review | light | 1.16% | 1.01% | 3.21% | 3.09% |
| workspace-idle | dark | 0.40% | 0.40% | 3.18% | 3.18% |
| workspace-idle | light | 0.43% | 0.43% | 1.97% | 1.97% |

The residual, per screen:

- **Workspace (0.90-0.91%)** is glyph antialiasing only. Every line's ink
  sits within 1pt of the frame (first ink rows 16/52/88/…/359 against
  16/53/89/…/359, columns 18-374 both, and "Working…" at 934 both); text
  colors sample to the token (foreground `#b2b4bb` vs Figma's `#b2b5bb`,
  a rounding of the same `#f2f3f7b2`; muted `#777a86`, green `#5acd25`,
  red `#e04f52` exact). Figma's rasterizer draws Source Code Pro lighter
  and a pixel shorter at the top of x-height glyphs than CoreText does; that
  is what the heatmap shows along every line.
- **Workspace idle (0.40-0.43%)**: antialiasing, plus the Shell button's
  icon (below). Toolbar, rule and placeholder lines are within 1pt.
- **Review (1.00-1.01%)**: antialiasing, the two fallback glyphs and the
  composer's rows (below).
- **Orchestrator (3.01-3.07%)**: antialiasing plus the grid departures
  below, which move most of the transcript 6-7pt and the composer 10pt.

## Fixes

- **Box drawing is drawn, not taken from the font**
  (`src/elements/terminal/boxdraw.rs` new; `layout.rs`, `element.rs`). The
  font's `─` / `│` is a 1pt stroke that straddled a pixel boundary at 1x:
  the Codex header box and every rule painted as a 2px smear at half
  strength (`#696d79` + `#444955` where the frame has one `#777a86` row),
  with hairline seams between cells. The pane now draws the common
  box-drawing characters itself: light and heavy straight lines, corners,
  tees, crosses and half lines as quads snapped to device pixels, the
  rounded corners `╭╮╯╰` as a stroked quarter circle meeting them. Light
  lines are 1pt at 14pt (scaled with ⌘+/⌘−), heavy 2pt; arms never overlap,
  so a dim (translucent) line doesn't darken at its joins. The box border
  now samples exactly `theme.muted` (`#777a86` dark, `#858a9b` = muted over
  white in light), 1px, as the frame's border. Double, dashed and
  mixed-weight characters, and underlined or struck-through ones, still come
  from the font. This was a listed follow-up of `dt-terminal-complete`.
- **The undrawn states take Figma type styles** (`elements/terminal/mod.rs`):
  the find bar's query is `Desktop/Base` (was gpui's `text_sm`, 14/20, not a
  Figma style), its "3 of 3" `X-Small`, "Jump to bottom" `X-Small/Medium`
  (was `text_xs` + a weight), and the Connecting / exited notice
  `Desktop/Small`. The find bar's icon buttons round at 6pt like the Figma
  buttons (were 8).
- **Fixtures** (`src/visual_figma.rs`), where the render's content differed
  from the frame's copy or color:
  - Bullets (`•`) draw in their line's color, as every frame binds the whole
    line to one variable (they were dim).
  - The Codex box has no blank row under its title: the frame's lines are
    26pt apart, so the title, model and directory now land +7, -1 and -9pt
    from it (were +7, +17, +9), with the blank row at the bottom, where the
    frame's 108pt box has its slack.
  - "Working…" sits on the row the frame draws it (rows-3, y 931 vs 930;
    was 17pt high).
  - The reviewer's composer rules are 102 cells, 857pt (frame 858; were
    874).
- **States render in the harness.** `--visual-figma` now also saves
  `state-orchestrator-{selection,find,jump,zoom-in,zoom-out}-<theme>.png`
  over the orchestrator (no reference; `figma-diff.sh` skips them). New
  `visual-check` hooks: `ShelbiWindow::offscreen_chat_pane`,
  `TerminalPane::offscreen_find`, and `TerminalPane::offscreen_flush_resize`
  (the re-measured grid's size goes out after a debounce, and gpui timers
  don't fire offscreen, so without it a zoomed render letterboxes).

## The undrawn states, checked by eye in both themes

- **Selection**: `text_selection` behind the cells (the token the theme maps
  `theme.selection` to), text unchanged on top; a two-line selection starts
  at its column and runs to the line's end, as a terminal's does.
- **Find bar**: the raised surface and popover shadow the other popovers
  use, the magnifier and chevrons in `theme.gray` (`icon`), the query in
  Base loud with the focus-ring caret, the count in X-Small muted; matches
  in `search_match`, the current one in `search_current`. Legible in both
  themes.
- **Jump to bottom**: X-Small/Medium loud on the raised surface, 12pt above
  the bottom, clear of the scrollbar.
- **⌘+ / ⌘−** (two steps each, 16pt and 12pt): the grid keeps its 16pt
  leading and 12pt top inset at both sizes (box border at x 277.8 / 276.6
  = 16 + half a cell, top at 12 + half a row), and fills the area once the
  new size is reported.

## Departures left, flagged for John

1. **The Orchestrator frame draws off the 18pt grid.** Its Codex box is a
   108pt rectangle followed by a 24pt gap, and a 30pt gap before the cyan
   notice, so the transcript lines sit at fractional rows (7.33, 9.39,
   11.39, …). A terminal draws on 18pt rows; the render puts each line on
   the nearest row, 6-7pt above the frame (the cyan line, which the frame
   puts back on the grid at row 27, matches). The box itself is drawn with
   box-drawing characters through the middle of its edge cells, so it runs
   21-111 where the frame's runs 12-119 (inherent to box drawing). *Ask:*
   should the frame's transcript snap to 18pt rows (box 12-120, then the
   prompt at 138)?
2. **The composer rows and the leftover height.** 1000pt of grid height is
   55.6 rows: the 55 rows end 10pt above the frame's bottom inset (the grid
   is top-anchored, as the Workspace frame's agent lines require). Figma
   also packs rule, prompt, rule, status into 37-38pt where a terminal needs
   four rows (72pt). So the Orchestrator composer sits 10-11pt high and the
   Review composer's rule / `❯` / rule / status 35 / 27 / 18 / 10pt high.
   This is the program's output on a grid, not chrome.
3. **The composer rules' color.** The frames draw the agents' composer rules
   in `theme.divider`; Codex and Claude Code draw them in their own color,
   and no terminal color names the divider. The fixture keeps them dim
   (`theme.muted`), now crisp 1pt.
4. **Fallback glyphs.** Source Code Pro has no `✻` or `❯`. Figma's fallback
   face draws `✻` thinner and wider than a cell (the title after it starts
   2pt later) and `❯` 3pt further left; the app's fallback (Menlo) is pinned
   to the cell grid, as every terminal glyph is.
5. **The Shell button's icon is 20pt; the frame's is 14pt** (with `px 8`,
   `gap 8`). The button keeps the frame's height (24) and total width (the
   previous task balanced 20pt + 6/6 padding against 14pt + 8/8), so only the
   icon's size and the label's position (2pt right) differ. The 14pt icon
   size John approved lands with `dt-figma-qa-activity` (`ICON_SIZE_SMALL`,
   `assets/icons/heroicons-14/`), not yet on the umbrella; once it is, the
   button takes `COMMAND_LINE_SMALL` with 8/8 padding and gap (the Jump to
   bottom arrow, an X-Small control, likewise).
6. **Figma's frames are narrower than their children.** The Workspace and
   idle frames' main content is 1160pt wide but its toolbar, rule and agent
   terminal are 1183pt (the render's main width), so Figma clips the Shell
   button at the frame's edge. Measured in main-area coordinates the button's
   right edge agrees (1167).
7. **Divider rounding.** The render's `theme.divider` over the canvas is
   `#2c3240`; Figma's export has `#2c3140` (the same `#acb4cf1a`, rounded
   differently). Likewise foreground `#b2b4bb` vs `#b2b5bb`.

No new contrast exception: the box lines take their cell's color, which the
existing terminal contrast tests already cover.

## Checked

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo clippy --all-targets --features visual-check -- -D warnings &&
  cargo test` (459 passed, rebased on `b229b3c7`). No `crates/` change, so no root workspace run.
- New tests: `boxdraw::tests` (a horizontal line is one device pixel through
  the cell's middle at 1x and 2x, neighbors meet on the same pixel, a
  vertical line's arms meet without a gap, joined arms never overlap, heavy
  is twice light, a rounded corner meets its straight neighbors, other
  characters stay the font's) and
  `layout::tests::box_drawing_leaves_the_runs_as_lines`.
- `--visual-figma` in both themes: 0 checks failed, including the new
  "states: the orchestrator's terminal attaches".
