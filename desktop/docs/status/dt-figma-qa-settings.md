# dt-figma-qa-settings

Pixel QA of Settings (`dt-figma-settings`) against John's Figma Settings
frame: dark `2165:545` (main content `2165:633`), light `2220:21264`. The
Figma MCP was available. I pulled `get_metadata`, `get_design_context` and
`get_variable_defs` on the main content and the Notifications group
(`2169:671`), and downloaded the frame's `Icon / check` asset.

Images (outside the repo):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-settings/`.
They include the full-frame and main-column side-by-side, heatmap and overlay
images, before (`before-*`) and after, the 8x checkbox zooms
(`zoom-checkbox-*`), the undrawn states (`state-*`), and `reports.txt`.

## Mismatch, before and after

`scripts/figma-diff.sh` at the default FUZZ 10:

| screen | full frame before | full frame after | main column before | main column after |
|---|---:|---:|---:|---:|
| settings-dark | 5.18% | 5.18% | 1.90% | 1.81% |
| settings-light | 3.97% | 3.97% | 1.99% | 1.91% |

The full-frame number belongs to the sidebar. The harness sidebar is 257pt
wide, and the frame's Navigation component is 280pt. That is
`dt-figma-sidebar`'s job and out of scope here. It also shifts the whole
main column 23pt left. The main-column diff takes the shift out: the
reference is cropped from x=280 and the render from x=257, both 1160 wide,
with `CORNER=1`.

The after residual splits into three parts:

| part | dark | light |
|---|---:|---:|
| Content that differs by design (the font and size fields and their helpers: the frame predates Source Code Pro / 14 and the picker) | 0.15% | 0.16% |
| Checkboxes (corner AA only) | 0.002% | 0.002% |
| Text: glyph AA and advance widths | 1.66% | 1.75% |

The text residual is the Geist version difference that `dt-figma-qa-activity`
and `dt-figma-qa-palette` measured. Figma uses John's installed Geist 1.2,
and the app bundles Geist 1.7.2, whose advances are narrower. Glyph by
glyph, every text run starts at the same pixel. Ink bounding boxes match the
frame to within 1pt on the left and top edges for all 25 text runs and
controls. Right edges fall short by up to the drift on long runs: 1pt on
the long helpers, and 2 to 4pt on the notification labels. gpui's CoreText
rasterizer also draws stems heavier than Figma's.

## What the measurements show

- **Geometry** (pixel grids and run edges): the chips are 26pt tall at
  12pt across with 6pt corners. The fields are 360x32. The button is 26pt
  tall. The checkbox is 14pt at (0,2) of its 18pt row, and its label sits
  at 22. Section spacing is 24, the inner gap is 4, and the outer padding is
  32. All match to the pixel. Corner AA differs by a level or two.
- **Flat fills** (sampled): the chip `#6c7387` vs `#6d7388`, the button
  `#424758` vs `#414758`, the field edge `#2c3240` vs `#2c3140`, and the
  canvas and checkbox fill exact. Light is the same: `#dee1ec`, `#f2f3f7`,
  `#f7f7fa` vs `#f6f7fa`, exact. The differences are alpha-blend rounding.
- **Text colors** (peak ink pixel per run): the title, labels, selected
  choice and notification labels are `theme.white`. Unselected choices, the
  field value and the button label are the frame's `#7b85a7` (`form_text`).
  Helpers are `theme.muted`. All are exact in both themes.
- **Type**: Heading 20/28 Bold, Base 14/18 Regular and Medium, and X-Small
  12/16 Regular all match the frame's text styles.

## Fixes

1. **The checkbox mark is Figma's `Icon / check`** (`src/assets.rs`,
   `assets/icons/checkbox/check.svg`, `src/views/settings.rs`). The checked
   box used to draw a text `✓` (XSmall Semibold), a different glyph from
   the frame's. The frame draws a 12pt icon: the Heroicons check path with a
   1.5pt stroke in the canvas color. It is vendored as Figma exports it
   (stroke set to `currentColor`) and drawn by a new `check_mark()` helper at
   `CHECK_MARK_SIZE` (12pt), set apart like the status glyphs, with its own
   test. The box and mark now match the frame pixel for pixel, apart from
   corner AA (see `zoom-checkbox-*`). I kept this apart from the
   `heroicon(Icon)` set on purpose: `dt-figma-qa-activity` (not yet merged)
   reworks that set for 14pt icons, and this way the two don't collide.
2. **Curly apostrophes** (`src/views/settings.rs`). "Claude Code’s" and
   "what’s already on screen" now use ’, as the frame's copy does. They
   were straight `'`.
3. **The field icons take the frame's gray** (`src/theme/style.rs`
   `tinted_icon_button`, `src/views/settings.rs`, `src/views/font_picker.rs`).
   The Settings frame overrides `theme.gray`, the icon token, to `#7b85a7`.
   The closed picker's chevron and the stepper's − / + now use `form_text`,
   like the value beside them, instead of the app-wide `icon` role
   (`#acb4cfe5` in dark, which was brighter than the field's text). Hover
   and press still go loud. `icon_button` is now `tinted_icon_button` with
   the `icon` role, so nothing else changes.
4. **Harness fixture** (`src/visual_figma.rs`). The sandbox `config.yaml`
   sets the frame's Shelbi CLI path (`/opt/homebrew/bin/shelbi`), so that
   field is compared with the same content. The font and size keep the
   shipped defaults.
5. **Renders of the undrawn states** (`src/visual_views.rs`,
   `SettingsView::offscreen_show`/`offscreen_loaded`). `--visual-views`
   now also saves `settings-size-min.png` (Dark and Meta selected, size 8),
   `settings-size-max.png` (Light and Left is Meta, size 32) and
   `settings-option-right.png`, in both themes.

## Undrawn states, checked by eye in both themes

- **Each segmented option selected**: the chip lands on the choice, in Base
  Medium and loud on `theme.selection`. The rest stay Base Regular in
  `form_text`. This matches the frame's selected and unselected tokens.
- **Size stepper at 8 and 32**: that end's button dims to 40% and goes
  inert, and the other stays `form_text`.
- **Font picker open with installed fonts**: the field takes the focus ring
  (`#62cdf6` / `#31ade9` plus the glow) and a loud chevron. The list is the
  shared popover with the search row, the current family on the selection
  fill with the `icon/select` check (cyan / pink), the divider after Geist
  Mono, and each row in its own font. This is consistent with the board and
  Activity dropdowns.

## Departures, for John

- **The font and size helper copy.** The frame reads "The font family for
  terminals. Blank uses Geist Mono." and "In points. Blank uses the
  default." The README decision says "Blank uses Source Code Pro".
  `dt-figma-settings` replaced the text fields with a picker and a stepper
  that always hold a value, so "Blank uses …" would describe something that
  can't happen. The helpers read "The font family for terminals." and "In
  points." I kept that. If you want the clause back, the picker would need
  a blank entry.
- **The Geist version.** The frame's text is set in your installed Geist
  1.2 (`~/Library/Fonts/GeistVF.ttf`). The app bundles 1.7.2. That makes
  every long run 1 to 4pt narrower and accounts for almost all of the
  residual. Chips are sized by their text, so the option chips are each
  about 1pt narrower too ("Types characters" is 136 vs 139). Same flag as
  the other QA tasks: the bundled font is unchanged.
- **Popover shadow** (picker open, not drawn in the frame). It uses the
  shared `style::popover_shadow`. `dt-figma-qa-palette` flagged that gpui's
  `blur_radius` is a sigma, so these blurs come out twice as soft as their
  Figma values. That is a shared style, so I left it alone here.
- **`--2x`**: no Retina display is attached, so the harness falls back to
  1x. Close inspection used 8x nearest-neighbor zooms and pixel grids of the
  1x frames.

## Verification

- `cd desktop && cargo build` (also with `--features visual-check`),
  `cargo clippy --all-targets -- -D warnings` (with and without
  `visual-check`), and `cargo test`: 451 pass on the umbrella at `a065f2af`, including the new
  `the_check_mark_loads_on_a_12_grid_in_current_color`.
- `--visual-figma` and `--visual-views`: 0 checks failed in both themes.
- No `crates/` change, so the root workspace checks don't apply.
