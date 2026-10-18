# dt-figma-qa-palette

Pixel QA of the command palette (`dt-figma-palette`) against John's Figma
Command palette frame, dark 2171:628 and light 2236:24244 (the overlay is
2171:752 / 2236:24283), in both themes. Exact values came from the Figma MCP
(`get_design_context` on 2171:752, `get_variable_defs` on both overlays);
the renders came from `--visual-figma` and `scripts/figma-diff.sh`, and
every element was then measured against the 1x reference PNGs with
ImageMagick (ink bounding boxes per text run and icon, flat-fill samples,
the shadow's falloff column by column).

## Mismatch, before and after

`figma-diff.sh` (FUZZ 10) over the whole 1440x1024 frame, and over the
palette panel alone (788x368 at 326,200, which leaves out the orchestrator
behind the scrim; that screen has its own QA task):

| screen | frame before | frame after | panel before | panel after |
|---|---|---|---|---|
| command-palette-dark | 3.86% | 3.86% | 0.66% | 0.66% |
| command-palette-light | 3.25% | 3.25% | 1.02% | 1.02% |

The shadow fix sits under FUZZ 10, so the table can't see it. Measured on
its own (the 32pt band under the panel, FUZZ 1): light went from 256
differing pixels to 0, dark stays 0. Column by column the light drop now
tracks the frame within 2 levels (`69 6C 6C 6D 6F 72` in the frame against
`6A 6B 6D 6E 70 72`; before, `6F 6F 70 71 72 73`, too faint at the edge and
still dark 30pt down).

The frame-wide regions the diff lists (the composer at the foot, the Codex
card at the top, the sidebar) are the orchestrator screen under the scrim,
not the palette.

## What matched without a change

- **Geometry**: the panel is 788x368 at (326, 200) in both themes, to the
  pixel. Query line 44pt; rows 32pt; icons at x 349-352 and names at x
  374-375 in both; every description starts at x 601 (600 on the selected
  row in both); Projects rows 26pt apart starting at the same y; footer at
  (343, 543). Every text and icon box is within 1pt of the frame's.
- **Fills**: panel `#2c3140` / `#f9f9fc`; the selected row `#434859` /
  `#f0f1f6` in the frame, `#43495a` / `#f0f1f7` here (the same
  `theme.background` alpha, composited with a 1-level rounding difference).
- **Text colors** (sampled at the ink's extreme): name `#f8f8fa` /
  `#020303` selected, `#b6b9c0` / `#4c4d4e` otherwise; descriptions
  `#777a86` / `#83889a`; the caret `#30c6ff` / `#1989c4`; the green dots
  `#5acd25` / `#367b16`. All equal to the frame within 1 level.
- **Type**: Base Regular for the query, descriptions and project names, Base
  Medium for the names, Base Semibold for the selected name, X-Small Regular
  for the footer, as the Figma text styles say.
- **Icons**: the same nine drawings at 20pt in `theme.gray`.

## Fixes

| file | what changed |
|---|---|
| `src/views/palette.rs` (`shadow`) | The drop's blur is now 7.5, not 15. Figma's blur, like CSS's, is twice the Gaussian's sigma; gpui's `blur_radius` is the sigma itself, so the frame's `0 10 15 -3` drew twice as wide and too faint at the edge. |
| `src/views/palette.rs` (no-match) | "No matching commands" sits in the first row's slot under the query line. It used to stretch over the whole results band and center vertically (about 50pt down, beside the Projects column's third row). It also sets its own type style (Base) instead of inheriting one. New render test `no_match_sits_in_the_first_rows_slot`. |
| `src/visual_figma.rs` | `--visual-figma` also renders the states the frame doesn't draw (`PALETTE_STATES`): a query with matches (`err`), one with none (`xyzzy`), Tab into the Projects column, and a project row selected there. They are saved beside the screens as `command-palette-<state>-<theme>.png` and have no reference, so `figma-diff.sh` skips them. |

## Undrawn states (checked by eye, both themes)

- **Search with matches** (`err`): Error log, then Orchestrator; the first
  is selected (fill, Semibold, loud name, foreground description); the
  query in `theme.white` with the caret after it. The panel shrinks to the
  Projects column's height. Consistent.
- **No match** (`xyzzy`): fixed above. The message is muted Base in the
  first row's slot, level with the first project row.
- **Tab into Projects**: the commands lose their selection fill and the
  first project ("Shelbi") takes it (`theme.background`, loud text, 6pt
  corners, 26pt tall, the full column width). Consistent with the command
  rows' selected state.
- **Project row selected** (Tab, Down): the fill moves to ContextStore.
  Consistent.

## Departures left, flagged for John

1. **Geist version (needs a decision; theme-wide).** The desktop bundles
   Geist 1.800 (`assets/fonts/geist`). Figma's Geist has the older metrics
   of Geist 1.200 (the `GeistVF.ttf` in `~/Library/Fonts`): summing the
   glyph advances, 1.200 predicts the frame's line widths exactly and 1.800
   predicts ours, e.g. "Manage what Shelbi is working on here" is 257.2pt
   in 1.200 and 255.2pt in 1.800 (ink 251 in the frame, 249 here). Regular
   lines therefore run about 0.8% short: 2-3pt over a 250pt description,
   past the 1pt bar at the far end of a long line; starts and heights are
   exact. Medium and Semibold names are within 1pt. The fix is to bundle
   the Geist that Figma uses, which moves every screen at once, so it isn't
   made here; it belongs to `dt-figma-qa-final` or a font task if John
   wants it.
2. **Fallback glyphs.** `❯` (the prompt) and `⌃` (the footer) aren't in
   Geist. gpui's fallback draws the prompt 1pt wider and the footer's `⌃`
   wider than Figma's, so the footer ends 3pt further right (314 against
   311). A gpui/CoreText fallback difference.
3. **Light text on dark** draws a little heavier in gpui than in Figma
   (more ink at the same peak color), most visible on the dark project
   names. Rasterizer; the residual above is mostly this and item 1.
4. **The dark scrim** composites to `#141820` here and `#151820` in the
   frame: no single alpha of `#020303` over `#1e2330` gives Figma's value
   on all three channels, so this is Figma's blending, not a token. Light
   matches exactly (`#858a9b`).
5. **Copy**: "Detach", not the frame's "Detatch" (John's decision). The
   harness runs with Zen on, so the Zen row reads "Turn Zen Mode off" with
   the knob-right toggle; the frame shows "on" while its sidebar shows Zen
   on (the mockup disagrees with itself).
6. **Shadows elsewhere (for the other screens' QA).** The blur mapping in
   the first fix applies to every shadow drawn from a Figma value:
   `style::raised_card_shadow` (blur 4), `popover_shadow` (15, 6) and
   `dialog_shadow` (25, 10) all pass the Figma blur straight to gpui and
   draw twice as soft. Not changed here, since they belong to other
   screens.
7. **No 2x render.** `--2x` found no Retina display on this machine (the
   main display is a 1x external), so close inspection used 3-4x
   nearest-neighbour crops of the 1x renders.

## Images

In `~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-palette/`
(outside the repo): `before-*` and `after-*` side-by-side, heatmap and
overlay for both themes, `panel-heat-*` (the panel's differing pixels in
red over the frame), `panel-side-2x-*` (frame over render at 2x), the
renders, and `state-*` for the four undrawn states.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings`
  (also with `--features visual-check`) `&& cargo test`: clean, 438 tests
  pass.
- `--visual-figma` in both themes: 0 checks failed; `figma-diff.sh` as in
  the table.
