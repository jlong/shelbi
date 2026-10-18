# dt-figma-qa-activity: pixel QA of the Activity view

QA of `dt-figma-activity` against John's Activity frames (dark `2165:364`,
light `2220:21092`; main content `2165:455` / `2220:21094`), in both
themes, with fixes. Values come from the Figma MCP (`get_metadata`,
`get_design_context` and `get_variable_defs` on both main-content nodes, the
selected row `2220:21145` and the pickers `2168:717` / `2168:721`), checked
against the 1x PNGs by sampling pixels and measuring ink runs.

## Mismatch, before and after

`scripts/figma-diff.sh` defaults (FUZZ 10, GROW 6, CORNER 28). The full
frame is dominated by the sidebar column, which isn't this task's (it is
257pt wide here, 280pt in the frame: `dt-figma-sidebar`), so the main area
is also diffed aligned: the frame from x 280, the render from x 257, 1140x1024
each (`CORNER=1`).

| frame | full, before | full, after | main area, before | main area, after |
|---|---:|---:|---:|---:|
| activity-dark | 4.98% | 4.99% | 1.94% | 1.82% |
| activity-light | 3.86% | 3.85% | 2.07% | 2.00% |

Re-measured after the rebase onto the umbrella with Machines and the
palette QA and the Settings QA merged: the same numbers, full and main area.

The full-frame number barely moves because the sidebar offset shifts every
main-area pixel 23pt; the aligned main area is the measure for this task.
What is left there, by cause:

- **Glyph advances** (most of it): Figma's Geist is wider than the app's;
  see the first flag below. Each line's text starts within 1pt of the frame
  (measured per row: actor, verb, title, second line, tag, time) and ends up
  to 7pt short on the longest titles.
- **The daemon row's second line** and the paging line under it (the
  `255x79+10+438` region): see "Not matched".
- **The footer's page keys** (`⇞/⇟` against `⇧↑/⇧↓`), which shift the rest
  of the hint line left.
- **The day rules**, which Figma centers at y 19.5 of their 40pt row and so
  draws as two half-covered rows; gpui snaps the 1pt rule to one row (19).
  The flat color is the token (`#acb4cf1a` over the canvas: `#2c3140` dark,
  `#f6f7fa` light, sampled on the header divider, which sits on a whole
  pixel).
- Glyph-edge antialiasing between the two rasterizers.

Diff images (not committed) are in
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-activity/`:
`activity-<theme>-{side,heat,overlay}.png` (full frame, after),
`activity-<theme>-main-{side,heat,overlay}.png` (aligned main area, after),
`before-activity-<theme>-main-*.png`, and the reports `report-*.txt`.

**2x.** `--visual-figma --2x` fell back to 1x again: this machine still lists
no Retina display (the harness's own caveat). Close inspection used the 1x
frames cropped and scaled 4x and 6x instead, beside the same crops of the
references.

## Checked and matching

Measured in both themes, all within 1pt and the exact Figma values:

- **Geometry**: the toolbar (title at 16/8, the 8pt gap to "Shelbi", the
  filters row at y 34, 24 tall; segments 4 apart, 8 across; the 8pt filter
  gap), the 1pt divider at y 66, the day groups (40 tall, the 12pt rule, the
  label 8 after it), rows (44 tall with a second line, 26 without; 8 across,
  4 down; the actor column 84; the verb's 8pt; the 2pt between the title and
  its second line; the 8pt before the tag and between tag and time), the
  selected row's 6pt corners, the paging line's 16pt.
- **Fills** (sampled flat): the selected kind `#6d7388` dark / `#dee1ec`
  light (`theme.selection` over the canvas), the pickers `#414758` /
  `#f2f3f7` (`theme.search`), the selected row `#383d4c` / `#dee1ec`, the
  canvas, the dividers. Render values agree to the frame's within 1 of 255
  (gpui's blend rounding).
- **Text colors** (each text node's glyph cores): "Activity" `theme.white`,
  "Shelbi", the kinds, verbs, paging and hints `theme.muted`, the actors
  (`zen` ansi green, `you` `#7b85a7`, `review` `theme.cyan`, `bravo` ansi
  blue, `alpha` ansi cyan, `charlie` / `system` muted), the titles
  `#7b85a7`, second lines, times and day labels `theme.ansi_dark_gray`, the
  four tags in their ansi hues. In light, the thin X-Small glyphs never
  reach their full color in either rasterizer, so their cores read a few
  points lighter on both sides.
- **Type**: Base Medium title and tags, Base Regular actors, verbs, titles
  and times, X-Small Regular for "Shelbi", the kinds (selected included:
  the frame's selected "All" is Regular, not Medium), pickers, day labels,
  second lines, paging and hints.
- **Copy**: character for character, except the flagged items below.

## Fixes

| File | What changed |
|---|---|
| `src/theme/style.rs` (`filter_dropdown`) | The pickers' icon and label were `theme.gray` (`#acb4cfe5` dark) and `icon`; the frame sets `theme.gray` to the log's opaque `#7b85a7` in both modes, so both are `log_text` now. Measured: the label read `#a1a9c3` dark / `#8790af` light, now `#7b85a7`, as drawn. |
| `src/assets.rs`, `assets/icons/heroicons-14/` | **A 14pt icon size** (John's call, below). The pickers' funnel and user are 14pt in Figma: the 24/outline drawing scaled, its 1.5pt stroke kept. They are now `icons::FUNNEL_SMALL` / `USER_SMALL`: the same Heroicons drawings with only `stroke-width` widened to 2.5714 on the 24 grid (1.5pt at 14pt). An `Icon` carries its size, so `heroicon(icon)` stays the only way to draw one and no view sizes an icon. The pickers are 100 and 122pt wide, as drawn, where they were 106 and 128 (so the second sat 6pt right). |
| `src/assets.rs` (tests) | `every_icon_draws_at_20_pixels` became `every_icon_draws_at_a_figma_size`: an icon draws at 20 or 14 (`ICON_SIZES`, the sizes the Figma file uses), 14 only from `heroicons-14/`. A small drawing must equal its 20pt drawing with only the stroke changed. `only_the_icon_helper_draws_icons` also catches `icons/heroicons-14/` paths outside `assets.rs`. |
| `src/views/activity.rs` (footer) | The shortcuts bar was 26pt *including* its 1pt top border, so the divider sat at y 998, a point under the frame's 997. It is 27pt now: the divider, then the frame's 26pt bar. |
| `src/views/activity.rs` (`feed_row`) | **A long title didn't wrap** (an undrawn state, below): the title sat in a wrapping flex row as one unbreakable text child, so it ran under the tag and off the edge. The title and its trail are now one `StyledText` paragraph (the trail a colored run), so the title wraps at the message column like Figma's full-width text box and the trail flows after its last word. |
| `src/theme/palette.rs`, `docs/theme.md` | The contrast tests check the pickers' new pairing, `log_text on search over canvas`. It is covered by the existing `log_text on` exception, but it is now that entry's worst case in Dark (2.53), so the theme doc's row says so. The Activity filters row and the `log_text` row describe the pickers as they are. |
| `src/visual_views.rs` | `--visual-views` renders the undrawn states: `activity-tasks.png`, `activity-workspaces.png`, `activity-system.png` (each kind alone; `activity-zen.png` was there), `activity-workspace-picker.png` (`activity-picker.png` was there), and its fixture has a task whose title is too long for a line, with a check that the Tasks feed shows it. |

## Undrawn states

Rendered with `--visual-views` in both themes (`SHELBI_VISUAL_THEME`) and
looked at beside the frame:

- **Each kind selected** (All, Tasks, Zen, Workspaces, System): the selected
  segment is the `selection` chip with loud text, the others muted, every
  segment at the same 24pt and 8pt across. (The sliding chip's code is
  unchanged; the frames are its resting state.)
- **The pickers open**: the pressed picker takes `background_active`; the
  menu opens under it in the board's shared menu panel, with its title and
  the `accent` check on the current value.
- **A long title**: wraps at the message column (after the fix above), the
  tag and time stay on the first line at the right, the branch line under
  the wrapped title. A wider actor (`orchestrator`) pushes only its own row,
  as before.

## John's decision during this task

Asked (`q-icons-14`): add a 14pt size for the picker icons, or keep 20pt
everywhere? Answer: **add the 14pt size**. The Figma file supersedes the
older 20pt-everywhere rule, and the guard now allows only the sizes the
Figma file uses (20 and 14 for now), still drawn only through the assets
helper. **For `dt-figma-qa-issues-board`**: the board's filter dropdowns
still put a 20pt icon in Figma's 14pt slot (`style::icon_slot`,
`style::dropdown`). They will likely want the small icons instead (add the
drawing to `heroicons-14/` and the `@small` list).

## Flagged for John

- **Figma's Geist is an older Geist.** Figma lays the text out with the
  Geist installed on John's Mac (`~/Library/Fonts/GeistVF.ttf`, font
  version 1.2), whose advances are wider than Geist 1.7.2's, which the app
  bundles (the `geist` npm package, the same metrics as the Google Fonts
  build). Measured with HarfBuzz at 14pt: "Handle reconnects after a daemon
  restart" is 273pt in 1.2, 265pt in 1.7.2; Figma's ink is 270pt wide, the
  app's 264. This is the largest residual on this screen and on every text-
  heavy screen. Updating the installed Geist (so Figma draws the shipped
  metrics) would close it; the app shouldn't go back to the old font.
- **The branch glyph `⎇`.** Geist has no U+2387, so both sides fall back:
  Figma to its own fallback font, gpui to CoreText's (SF Pro's drawing).
  The two draw it differently, and no font on this Mac draws Figma's shape.
  Matching it would mean bundling a symbol font, or drawing the branch as an
  icon instead of text.
- **The daemon row's "hub · cli 0.10.0".** The daemon's event line carries
  no host or version, so the row has no second line and the paging line sits
  18pt higher than drawn. (Carried from `dt-figma-activity`.)
- **"⇧↑/⇧↓ page".** The Activity page keys are PageUp/PageDown (and `u` /
  `d`); nothing binds ⇧↑/⇧↓. The hint bar shows the bound keys from the key
  map (`⇞/⇟`). (Carried from `dt-figma-activity`.)
- **The right edge.** The frame's main content is 1183pt wide in a 1160pt
  window, so its tags and times reach x 1438. The render keeps the 16pt
  gutter. (Carried from `dt-figma-activity`.)
- **Contrast.** `log_text` on the pickers' fill is 2.53 in Dark, below AA,
  as drawn (listed under the existing `log_text on` exception).

## Checked

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings
  && cargo test` (451 passed, after the rebase onto the umbrella with
  Machines, the palette QA and the Settings QA), and `cargo clippy --all-targets --features
  visual-check -- -D warnings` plus `cargo test --features visual-check`
  (451 passed). No `crates/` change, so no root workspace
  run.
- `--visual-figma` in both themes: 0 checks failed. `--visual-views` in both
  themes: 0 checks failed (the new long-title check included).
