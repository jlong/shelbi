# dt-figma-settings

Settings now matches John's Figma Settings frame (dark `2165:545`, light
`2220:21264`) in both themes, and the terminal font and size are chosen with
real controls instead of typed by hand.

## What changed

- **Layout and type** (`src/views/settings.rs`): 32pt padding, 24pt between
  sections, the title in Heading (20/28 bold, loud). Each section is its
  label in Base Regular, loud (no longer bold), the control, then the helper
  in X-Small muted, 4pt apart. Every section sits at the frame's y to the
  point.
- **Segmented controls** (`style::choice_track` / `style::choice`): no track
  and no boundary. Choices are 26pt, 12pt across, 4pt apart, 6pt corners.
  The selected one is Base Medium, loud, on the `theme.selection` chip; the
  others are plain Base text. The chip still slides between choices (same
  `SlideTrack` probes), and unselected choices still fade a fill in on
  hover. Activity's kind filter (`filter_track`, from `dt-figma-activity`)
  was the other user of the old tracked `segment_track` / `segment`. Both are
  trackless now, so those two functions were removed.
- **Fields** (`style::field`): the frame's 360x32 field (10pt across, 6pt
  corners, the canvas inside a 1px `theme.divider`, Base text). The Shelbi
  CLI field, the font picker and the size stepper all use it.
- **Open keys.yaml** (`style::form_button`): the frame's button: the
  `theme.search` fill, 26pt, Base text. Hover and press fade to the stronger
  overlay fills.
- **Checkboxes**: 14pt with 2pt corners. Checked is the loud color with the
  mark in the canvas, as before. Unchecked has a 1px `muted` edge (the frame
  draws only checked boxes; the old `divider` edge was nearly invisible).
- **Font picker** (`src/views/font_picker.rs`, `src/elements/terminal/fonts.rs`):
  a dropdown in the field's shape (the family, a chevron). Open, it shows a
  search field and then the list: Source Code Pro (the default), Geist Mono,
  a divider, then the installed monospaced families. Each row is set in its
  own font, with a check on the current one. The installed families come from
  CoreText (raw FFI like `assets.rs`; no new dependency): every face's family
  name and its `kCTFontMonoSpaceTrait`. They are read on the background
  executor once per process, so Settings opens without waiting. Hidden
  `.`-families and the Nerd symbols font are left out. Typing filters (case
  insensitive); ↑ / ↓ move, Return picks, Escape closes, and a click outside
  closes. On the closed field, Return, Space or ↓ open it. A pick applies to
  every terminal at once and is saved to `desktop.terminal_font_family`. If
  the saved family isn't installed, it stays the value with a muted "Not
  installed" note in the field, and terminals fall back to Source Code Pro
  as before (`CellMetrics::measure`).
- **Size stepper**: the field with the typed size and − / + buttons at its
  end. Steps are 1pt, the range is 8 to 32, and the default is 14. ↑ / ↓ in
  the field step too. A typed number is clamped into the range on Return or
  blur, and anything else puts the current size back. Each end's button dims
  at its end of the range. The range is `ZOOM_RANGE`, the same 8 to 32 that
  ⌘+ / ⌘− step within (`dt-terminal-font-zoom`). A size set by hand in
  `config.yaml` outside it (the terminal still takes 6 to 72) is shown and
  kept as is. An unrelated save or a blur doesn't rewrite it, and a step
  follows ⌘+ / ⌘−'s rule: it lands back in range, or does nothing when it
  would only move further out.
- **Applied live, kept in sync**: `save()` now sets the `TerminalSettings`
  global before the write starts (it used to wait for the write), so a change
  shows in open terminals at once. The view observes that global, so a font
  or size changed elsewhere (another window's Settings, and `⌘+` / `⌘−` once
  `dt-terminal-font-zoom` lands, provided it sets the global) shows in the
  picker and the stepper. The stepper's text is left alone while you're
  typing in it.
- **Defaults aren't written**: Source Code Pro and 14 clear their keys, the
  same way Option's default isn't written, so `config.yaml` keeps only what
  you changed. A saved family that isn't installed is kept, not dropped.
- **Copy**: the helpers are the frame's, except that the font helper reads
  "The font family for terminals." and the size helper reads "In points."
  The "Blank uses …" clauses are gone, since both controls always have a
  value.
- **Palette**: a new role `form_text`, `#7b85a7` opaque in both modes. The
  Settings frame overrides `theme.gray` to this value (`get_variable_defs`
  on `2165:633` returns `#7b85a7` in dark too). It colors a field's value,
  an unselected choice and the form button's label.
- **Icons**: Heroicons `minus` (24/outline, v2.2.0) added for the stepper.

## How the values were pulled

The Figma MCP was available. I ran `get_metadata`, `get_design_context` and
`get_variable_defs` on the main content node `2165:633` (dark) and
`get_variable_defs` on the light frame `2220:21264`. Fills were then checked
against the reference PNGs with ImageMagick histogram reads. Our render
matches each one to the unit: the chip `#6c7387` vs `#6d7388`, the button
`#424758` vs `#414758`, the field edge `#2c3240` vs `#2c3140`, and the light
equivalents.

## For John (decisions and flags)

- **Contrast.** `form_text` (the Settings frame's gray) falls short of AA:
  dark 4.30 on the canvas and **2.53** for "Open keys.yaml" on its
  `theme.search` fill; light 3.65 and 3.29. I kept Figma's value and listed
  both under `docs/theme.md` "Exceptions" (the contrast test guards them).
  Before this task the button label was loud text, and the field values were
  `foreground` / `gray`.
- **The check mark** is still a text glyph (✓) in the 14pt box. The Figma
  "Icon / check" is a 12pt drawing, and the desktop's rule is that every
  icon is a 20pt Heroicon (a test guards `svg()`), so I didn't add a
  one-off 12pt icon.
- **Size stepper shape.** The task allowed either `[−] 14 [+]` or a field
  plus stepper. I chose the field with − / + inside its trailing end. It
  keeps the frame's 360x32 field and lines the section up with the others.
- **The field's value stays in Geist**, as the frame draws it. Only the list
  rows preview each font.
- **No missing backing settings.** Every control in the frame (theme, font,
  size, Option key, CLI path, keys.yaml, the three notification switches)
  already had a setting.
- **The size display follows `⌘+` / `⌘−`.** `dt-terminal-font-zoom`
  landed on the umbrella while this was in flight, and I rebased onto it.
  Its zoom sets the `TerminalSettings` global, and this view's
  `follow_terminal_settings` observer shows the new size (Actual Size shows
  14). Zoom had also added its own observer (`font_size_changed`) for the
  old text field. I folded it into this one, so the Settings view has a
  single observer.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings`
  (also with `--features visual-check`) and `cargo test`: clean, 449 tests pass on the umbrella at `82d78aad`.
  New tests cover the picker's order, filter and divider (`fonts.rs`), CoreText
  finding Menlo monospaced and Helvetica not, picking by keyboard (open, ↓,
  Return picks Geist Mono; typing filters; Escape closes without picking,
  `font_picker.rs`), and in `settings.rs` the value model (round trip,
  defaults not written, 8 to 32 clamp). No `crates/` change, so the root
  workspace checks don't apply.
- `--visual-views`: `settings.png` and a new `settings-font-picker.png`
  (the list open, after the installed families are read), both themes,
  0 checks failed.
- `--visual-figma` plus `scripts/figma-diff.sh`, Settings only:

| screen | before | after |
|---|---:|---:|
| settings-dark | 6.90% | 5.18% |
| settings-light | 4.91% | 3.97% |

  Most of what's left comes from the harness sidebar being 257pt wide
  against the frame's 280pt (the Navigation component's job). That shifts
  the whole main column 23pt left. Diffed with that shift taken out (main
  column only), the mismatch is 0.39% dark and 0.47% light. Text positions
  match row for row. The rest is
  the content difference (Source Code Pro / 14 / "Found automatically" vs the
  frame's older "Geist Mono" / 13 / a set CLI path) and glyph antialiasing.
