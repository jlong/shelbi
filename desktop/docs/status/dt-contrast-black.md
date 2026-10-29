# dt-contrast-black: a black canvas at Contrast 100

At Contrast 100 the dark theme's canvas (`Slate/900`, `#1e2330`) is now
black. Before, every dark surface mixed 35% toward black at 100, which left
the canvas at a visible slate (`#14171f`).

## How it works

`Palette::with_contrast` (`src/theme/contrast.rs`) still moves each role by
its class; only the dark surfaces changed. They darken as a ladder
(`Ladder`, `darkened`): every surface's `L + 0.05` (WCAG relative luminance
plus the ratio's offset) is multiplied by one factor, which is 1 at 0 and,
at 100, whatever takes the canvas to black. So:

- the canvas reaches `#000000` at 100;
- every ratio between two surfaces is what it was at 0, at every setting:
  the sidebar and the raised surface sit 1.21:1 over the canvas, the
  workflow badge over the sidebar as before, so the window keeps its
  levels instead of going flat black;
- the factor is geometric in the setting, so the canvas's contrast with
  black falls by the same ratio each step (half way is the square root), and
  no stretch of the slider does most of the work;
- a surface's linear channels scale together, so its hue holds (the
  sidebar stays a slate, just darker);
- the surfaces drawn darker than the canvas (board cards and the textbox,
  `Black/950` `#020303`) meet it at black (by about 50) and stay there;
  their edges (`ring`, `input_border`), which brighten with the setting,
  carry them.

| Dark | 0 | 50 | 100 |
|---|---|---|---|
| `canvas` (also `well`, `keycap`, `nav_selected`) | `#1e2330` | `#12161f` | `#000000` |
| `sidebar`, `raised` | `#2c3140` | `#222733` | `#171a24` |
| `workflow_badge` | `#3a4258` | `#32394c` | `#292f40` |
| `card`, `textbox` | `#020303` | `#000000` | `#000000` |
| Body text on the canvas | 10.6 | 13.3 | 16.8 |
| Muted text on the canvas | 5.2 | 7.8 | 11.7 |
| Icons on the canvas | 6.4 | 9.3 | 13.5 |

Unchanged: 0 is the palette exactly; the light theme (surfaces 50% toward
white); text, icons, borders and hues; the translucent fills (they
composite darker over the darker surfaces by themselves); the terminals
(contrast-0 palette, so a terminal pane keeps its slate background beside
the black canvas); translucency and Reduce Transparency, which apply after.

## Tests

- `theme::contrast::the_dark_canvas_reaches_black_evenly`: canvas
  luminance under 0.002 at 100, darker at every 10% step, and half way
  within 0.02 of the even (square-root) ratio to black.
- `theme::contrast::dark_surface_levels_stay_apart`: at 0, 50 and 100,
  sidebar and raised over the canvas and the workflow badge over the
  sidebar keep their order, at least 1.15:1 apart, within 0.03 of their
  ratio at 0; cards and the textbox never lighter than the canvas.
- `no_pairing_loses_contrast_as_the_setting_rises` passes unchanged (the
  one named dip, `dark_gray` on the selection band, still ends above where
  it started).

## Captures

`SHELBI_VISUAL_ONLY=main SHELBI_VISUAL_CONTRAST=<0|50|100>
shelbi-desktop --visual-figma <dir>`, in
`~/.shelbi/projects/shelbi/design-refs/dt-contrast-black/`
(`<screen>-dark-<contrast>.png`, plus `main-window-contrast-sheet.png`: the
orchestrator, workspace, board and Settings ▸ General at dark 0 / 50 / 100).
