# dt-figma-qa-issue-detail

Pixel QA of the issue detail (the card's modal over the board) against
John's Figma Issue detail frames: dark 2172:722 (overlay 2172:909, modal
2172:910) and light 2236:24534 (overlay 2236:24597, modal 2236:24598).
Values came from the Figma MCP (`get_design_context`, `get_metadata`,
`get_variable_defs`, and the icon SVGs it exports, which carry the true
stroke colors). Each element was then measured on the renders against the
1x reference PNGs with ImageMagick: ink bounding boxes, fill runs along a
row, and the peak ink color of each text run.

Images (not committed) are in
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-issue-detail/`:
`before/` and `after/` hold the whole-frame and modal-aligned side-by-side,
heatmap and overlay for each theme, and `states/` holds the undrawn states.

## Mismatch

Pixels that differ by more than 10% fuzz.

| Screen | Theme | Whole frame, before | Whole frame, after | Modal aligned, before | Modal aligned, after |
|---|---|---|---|---|---|
| issue-detail | dark | 5.17% | 5.06% | 0.71% | 0.71% |
| issue-detail | light | 5.53% | 5.48% | 0.89% | 0.89% |

The whole-frame figure is mostly not about this screen. The render's main
area starts at x=285, not Figma's 280, because the shell's sidebar resize
handle adds 4pt of sidebar fill and then its 1pt rule (`app.rs`,
`sidebar-resize`; flagged below). So the board and the modal behind the
scrim sit 5pt right of the frame. The modal-aligned figure crops the modal
on its own edge: 924 x 870 at the reference's (399, 77) and the render's
(400, 77).

The fixes below barely move the percentage, because icons and keycaps are
small areas. They were measured per element:

- **Residual, per screen:** 0.71% dark and 0.89% light, all text. Figma
  sets Geist 1.200 (John's installed copy); the app bundles 1.7.2, whose
  per-glyph advances differ, so long runs drift a pixel or two from where
  they started (the same residual `dt-figma-qa-activity` found). The other
  causes are Figma's half-point modal origin (x=398.5, so every edge in the
  reference is antialiased across two pixels) and the body paragraphs
  wrapping a word earlier in the narrower modal (see the departures below).

## Checked, matching

- **Rhythm** (every text line's ink rows, modal-relative): identical in
  both themes, from the title (y 24-39) through the fields (58-213), the
  action row (234-249), the body heading (286-303), the paragraphs, the
  checkbox rows (414-579, 38pt apart) and the notes (638-665). The divider
  sits on y=267 and runs from 20pt in.
- **Edges and fills:** the 1px `divider` border (`#2c3140` measured on
  canvas, light `#f6f7fa`, each within one level); the scrim
  (`#151820` / `#858a9b` over the board, matching); button rest fill
  `#383d4c` / `#f5f6f9`; the keycap is 16pt tall on the canvas, with its 1px
  drop row identical (`#1a1e24` / `#c9cee0`).
- **Type and color** (peak ink of each run, both themes): keys `muted`
  (`#777a86` / `#858a9b`), values and body `foreground` (`#b2b5bb` /
  `#4e4f4f`), title, headings and button labels `white` (`#f8f8fa` /
  `#020303`), and the keycap glyph `gray`.
- **Buttons:** Start, Assign, Edit and Editor are each within 1pt of
  Figma's 82 / 92 / 75 / 88. The 1pt is the Geist label width (Figma's
  "Start" is 29pt). Keycaps are 15pt wide, as Figma's Hotkey is.
- **Close mark:** its ink ends 22pt from the modal's right edge (Figma:
  22.5) and is centered on the title line, in `muted` (`#777a86` /
  `#858a9b` peak).
- **Checkboxes:** a 14pt box at the frame's 20.5pt (rendered at 21) with
  the 12pt check, as the earlier task measured.

## Fixes

- **Action icons at 18pt** (`src/views/task_detail.rs`, `src/assets.rs`,
  `assets/icons/heroicons-18/`). The frame draws the X-Small Button's icons
  at 18pt in the 14pt slot, but the detail drew them at 20pt, so each icon
  hung 1pt further out and was too large. Arrow left and right, user,
  pencil-square and arrow-up-right join the `@button` list (the 24/outline
  drawings, stroke widened to 1.25pt at 18pt, which the asset test checks
  against the originals). Start's filled play is now Figma's own 18pt
  drawing (`heroicons-18/play-solid.svg`, exported from the frame), where
  before it was scaled up to 20pt. The 20pt `figma/play-solid.svg` lost its
  last user and is gone. The 20pt `ARROW_RIGHT`, `ARROW_UP_RIGHT` and
  `USER` stay as the sources of the sized drawings (`#[allow(dead_code)]`,
  as `FUNNEL` already is).
- **Icon colors per the frame's exported SVGs:** the two Move arrows are
  `button_icon` (`#acb4cf` at 60% dark, `theme.gray` light), and Start,
  Assign, Edit and Editor are `icon` (`theme.gray`, `#acb4cf` at 90% /
  `#7b85a7` at 90%). Before, every icon was `icon`. `action` now takes each
  icon with its color.
- **Close mark at 16pt in `muted`:** the frame's `Icon / x-mark` is 16pt
  with a 1.5pt stroke in `theme.muted`. Before, it was the 20pt mark in
  `icon`. The new `@close` list (`ICON_SIZE_CLOSE`, 16pt,
  `assets/icons/heroicons-16/x-mark.svg`: stroke 2.25 on the 24 grid, 1.5pt
  at 16) adds 16 to the sizes the icon guard allows (`ICON_SIZES` 20, 14,
  18, 16). The button is `style::tinted_icon_button` with `muted` at rest
  (hover still turns it loud on the fill), moved 2pt right (`mr(-6)`):
  Figma lays the title row 2pt past its 20pt inset, so the mark sits 18pt
  from the panel's inner edge.
- **No focus ring under the scrim** (`src/views/issues.rs`): the board
  drew the selected card's focus ring and glow behind the modal, and
  through the scrim it showed as a cyan outline beside the modal. The frame
  draws none, and the keyboard is in the modal, so the selected card wears
  its plain card shadow while the detail is open.
- **Harness** (`src/visual_figma.rs`): `--visual-figma` now also renders
  the states the frame doesn't draw:
  `issue-detail-hover-<action>-<theme>.png` for each of the six actions
  (`DETAIL_ACTIONS`; the pointer is moved onto each button, which is found
  by scanning the action row for runs of non-canvas pixels), and
  `issue-detail-scrolled-<theme>.png` (the umbrella's body four times over,
  paged down once with the popover's own key). `save` is split into
  `capture` (keep the window open) and `save` (capture and close).

## Undrawn states (checked by eye, both themes)

- **Each action hovered:** the hovered button's fill rises to
  `button/default/background-hover` (light `#acb4cf38` over white measures
  `#edeff4`, exact; dark measures `#464b5c`, the theme's derived
  `#acb4cf47`, since Figma defines the hover only in Light). The label,
  keycap and icon keep their rest colors, as the Button's Hover variant
  (2224:5682) does. Neighbors stay at rest.
- **Long body scrolled:** the header, fields, actions and divider stay put,
  and the body scrolls beneath the divider's 12pt gap. Headings, checkbox
  rows (16 of 20 checked: the field reads `16/20 done`) and paragraphs keep
  the 16pt block rhythm, and the body clips at the modal's edge.

## Departures, flagged for John

1. **Modal width (a Figma inconsistency).** The overlay frames (2172:909,
   2236:24597) are 1183pt wide, which is 1440 − 257, the old sidebar. So the
   frame's 946pt modal is 80% of a main area 23pt wider than the real 1160.
   The view keeps "80% across and 85% down, centered" (924 x 870 here, 928
   once the shell is 280pt), so the modal is 18-22pt narrower than drawn,
   and its body paragraphs wrap a word earlier ("workspaces," moves to line
   two). Matching 946 exactly would mean a fixed width, or centering on an
   area wider than the window. **John:** should the modal be a fixed 946pt
   (centered in the real main area), or should the frame's overlay be
   resized to 1160?
2. **The main area starts at x=285, not 280** (outside this screen). The
   shell's resize handle (`sidebar-resize` in `app.rs`) is 5pt: 4pt of
   sidebar fill and then the 1pt rule, beside the 280pt column. The
   Navigation component is 280pt with no rule. Every main-area screen sits
   5pt right of its frame. This is for `dt-figma-qa-final` (or the
   sidebar) to settle: either fold the grab room into the column or overlay
   it.
3. **Move keys read `H` / `L`, not the frame's `l` / `r`** (kept from
   `dt-figma-issue-detail`). The frame's letters aren't bound to anything,
   and the popover moves with ⇧H / ⇧L. The keycaps make those two buttons
   6pt and 3pt wider than drawn. **John:** keep the real keys?
4. **Text residual** is Geist 1.200 (Figma) against 1.7.2 (bundled), as
   on every screen.
5. **`--2x`:** no Retina display is attached on this machine, so the 2x
   render falls back to 1x (the harness says so). Close inspection used
   crops scaled 2x.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings` (with and
  without `--features visual-check`) and `cargo test` (491 passed after
  the rebase onto 1a4aa4fc, with and without `visual-check`), from
  `desktop/`. Re-rendered and re-measured on the rebased build: the main
  area still starts at x=285, and the numbers above are from it.
- `--visual-figma`: 0 checks failed in both themes, including the new
  "the 6 actions found on the row".
- No shared crate changed; the root workspace and `Cargo.lock` are
  untouched.
