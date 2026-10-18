# dt-figma-settings-controls

Restyles Settings to John's updated Figma frame and builds its controls as
shared components, from the Figma components page (`2196:10061`). The Figma
MCP was available. I pulled `get_design_context` and `get_variable_defs` on
the dark Settings frame (`2165:545`, main content `2165:633`), the light
frame (`2220:21264`), and every component: Segmented Control `2238:25203`
(item `2238:25185`), Labeled Control `2238:25403`, Textbox `2238:25293`,
Picker `2238:25517`, Check `2238:25598`, Checkbox `2238:25621` and Checkbox
List `2238:25702`. I also pulled `get_metadata` on the Textbox and Checkbox
variant sets for their sheet positions. Where Figma's numbers and its own
render disagree, the values below were checked against the PNGs (1 px =
1 pt).

Images (outside the repo):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-settings-controls/`.
They hold the before and after frames, `figma-diff.sh`'s side, heat and
overlay images, `report.txt`, the undrawn states (`state-*`), and
`components/`: each component sheet in both themes, plus
`cmp-<name>.png` (Figma, Light render and Dark render side by side at 3x).

## The light frame is stale (John's call)

The light frame `2220:21264` (and `settings-light.png`) still has the v1
layout: trackless segments, a plain size field, no Choose font or Browse,
and helper lines under the font and size. Only the dark frame uses the new
components. I asked; John answered that the components page is the source of
truth. Light now uses the dark frame's layout in the components page's light
colors. **The Light figma-diff rise below is expected**, because
`settings-light.png` predates the new components.

He also confirmed these defaults:

- The font and size helper lines are dropped (the dark frame has none).
- The label stays "Open keys.yaml". Figma says `keys.yml`, but the real file
  is `keys.yaml`.
- The harness fixture keeps Source Code Pro at 14pt. The frame shows Geist
  Mono at 13pt, which predates the Source Code Pro default.

## figma-diff, before and after

`scripts/figma-diff.sh` at the default FUZZ 10, on the full 1440x1024 frame
(the harness sidebar is 280pt now, so nothing needs cropping):

| screen | before | after |
|---|---:|---:|
| settings-dark | 6.56% | **2.34%** |
| settings-light | 2.03% | 3.43% (expected, see above) |

On the main column only (x 280 and up, ImageMagick `compare -metric AE
-fuzz 10%`), dark goes from 1.06% to 0.50% and light from 0.39% to 0.89%.

The dark residual is text: the Geist 1.2 vs 1.7.2 advance widths that
`dt-figma-qa-activity` measured. They also make the segment items, the Choose
font and Browse buttons, and the option track 1 to 7pt narrower than Figma's.
Beyond that, the font and size values differ by design, and the frame's Key
bindings button is a broken export (an empty fill with "Open keys.yml"
outside it). Fill, edge, label, helper, value and checkbox colors match the
frame to the unit (peak-ink measurement):

| what | Figma (dark) | render |
|---|---|---|
| segment track | `#414758` (`#acb4cf40` over the canvas) | same |
| unselected segment text | 216,218,223 (`#f2f3f7d9` over the track) | same |
| textbox well / edge | `#020303` / 73,79,96 (`#acb4cf4d`) | same |
| textbox value | 206,207,210 | same |
| helper | 147,150,158 (`#f2f3f78c`) | same |
| button fill / label | 56,61,76 (`theme.background`) / `#f2f3f7` | same / `#f8f8fa` (`text_loud`, under fuzz) |
| stepper icons | `#acb4cf` at 60%, 1.25pt stroke | `button_icon`, the 20px Heroicons |
| check | `#31ade9`, white mark | same |

## Components against their PNGs

Each component sheet is rendered at its variant frame's size and positions,
on the page's `#f5f5f5`. Mismatch at FUZZ 10%, with the sheet's dashed
purple edge shaved off:

| component | mismatch | residual |
|---|---:|---|
| textbox (4 states) | 0.53% | glyph antialiasing; Figma draws a caret in the focused states (the live view's caret comes from the Input, in `focus_ring`) |
| checkbox (4 states) | 1.56% | label glyphs |
| segmented-control | 0.27% | glyphs |
| labeled-control | 0.38% | glyphs |
| picker | 1.22% | the Browse label's width (Geist) |

The first sheet render caught two real differences, both fixed. Figma
strokes the check's 12pt box **outside**, so it draws 14pt edge to edge. Both
focus glows are a 3pt band fully **outside** a pure-color ring. Figma's CSS
export (`inset-[-1px] border-3`) reads as a band over the ring, but its
render doesn't show that.

Dark has no component PNGs. Its sheets (`components/*-dark.png`) take the
dark frame's tokens and were checked by eye.

## What was built

`desktop/src/elements/controls.rs` holds `labeled_control`/`helper`,
`segmented_track`/`segment`/`selected_segment`, `textbox`, `picker`,
`form_button`, `form_icon_button`, `check`, `checkbox` and `checkbox_list`.
They replace `style::choice_track`, `choice`, `field`, `FIELD_W` and the old
`form_button`; nothing else used those. `style::raised_drops` is
`shadow/raised card` without its ring, for the selected segment.
`style::segment_chip` stays because Activity uses it.

Palette changes (`docs/theme.md` is updated, and the role and contrast tests
pass):

- `form_text` is now `#f2f3f7d9` / `#020303b2` (`theme.foreground` as the
  frame and the components set it). It was the old frame's `#7b85a7` gray
  override.
- `input_border` is now `#acb4cf4d` / `#acb4cf80` (it was `#acb4cf1a` in
  both themes).
- New roles: `form_hint` (`#f2f3f78c` / `#444c6580`), `textbox` (`#020303` /
  `#ffffff`, measured, since the dark frame's binding draws near black),
  `check_fill` (`#31ade9` / `#f21478`), `check_glow` (`#12adff33` /
  `#ff66ab33`; Figma has no Dark value).
- `well` is unchanged and now serves only the bootstrap command box.
- New contrast exceptions, since the design keeps Figma's values: dark
  `text_on_color on check_fill` 2.53 and light `form_hint on` 2.46. The
  `form_text` exceptions are gone because it now passes AA. `input_border`'s
  ratios moved to 1.76 / 1.38.
- gpui-kit's caret is now `focus_ring` app-wide: the Textbox draws its cursor
  in the ring color, and the palette's own caret is the same cyan family.

Settings behavior:

- Theme, Option key, the notification switches, the zoom sync and
  persistence are unchanged.
- The font picker is now the Picker. A press on the textbox or on "Choose
  font" toggles the list. The anchor covers both, so a press on the button
  while the list is open closes it instead of reopening it (by construction;
  no test presses it). The keyboard
  paths are unchanged: Return, Space or ↓ open the list, then ↑ / ↓ /
  Return / Escape.
- The size field shows `14pt` and accepts `15` or `15pt`. ↑ / ↓ still step
  it. − and + are the 32pt Icon Buttons, dimmed and inert at either end of
  the range.
- New: **Browse** opens a file dialog (`prompt_for_paths`, files only). The
  pick fills the Shelbi CLI field and is saved.
- The Settings checkboxes stay mouse-only, as before. `check`/`checkbox`
  take a `focused` flag, so a keyboard-focusable form can draw the focus
  state.

## Harness

- `SHELBI_VISUAL_ONLY=settings` renders just Settings, the focused-CLI state
  and the component sheets, in about 25 seconds instead of 4.5 minutes.
- New renders: `state-settings-cli-focused-<theme>` (the textbox focus ring
  in the real view) and `components/<name>-<theme>.png` (six sheets).

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --all-targets --features visual-check -- -D warnings`: clean.
- `cargo test`: 508 passed, including the new
  `the_size_field_shows_its_unit_and_takes_a_size_with_or_without_it`, the
  font picker's keyboard tests, the palette role, doc and contrast tests, and
  both smoke tests.
- `--visual-views`: every check is ok in both themes. I eyeballed
  `settings-font-picker` (the list under the Picker, 400pt wide), the stepper
  at 8 and 32 (the matching button dimmed) and `settings-option-right`.
- `--visual-figma`, Settings only, both themes: compared side by side and as
  heatmaps with the references, as above.
- Not checked: a real-window walk (Browse's dialog, hover fades on the new
  buttons). `desktop/scripts/figma-window-walk.sh` covers it when the screen
  is unlocked.
