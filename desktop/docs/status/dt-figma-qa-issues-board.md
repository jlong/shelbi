# dt-figma-qa-issues-board

Pixel QA of the Issues board (`dt-figma-issues-board`) against John's Figma
Issues frame (`Shelbi`, `pu3hxHgWoCDPCzJ4u6zTQK`; dark `2165:94`, light
`2220:20975`; main content `2165:185` / `2220:20977`), main area only
(x 280 to 1440), both themes. Values came from the Figma MCP
(`get_design_context`, `get_variable_defs`, `get_metadata` on the main
content, the Board toolbar `2220:20978`, the Button component set
`2220:17600` and its Focus / Active variants, the Issue card component
`2163:25`), plus the icon SVGs Figma exports (their stroke color and width),
checked pixel by pixel against the 1x reference PNGs.

Images (outside the repo):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-issues-board/`
`before/` and `after/` (whole-frame `*-side/heat/overlay.png` from
`figma-diff.sh`, aligned main-area `*-main-side/heat/overlay.png`, the
renders) and `states/` (the undrawn states, both themes).

## Method

`cargo run --features visual-check -- --visual-figma <dir>`, then
`scripts/figma-diff.sh`. The sidebar is still 256pt plus its 1pt rule here
(Figma: 280pt; `dt-figma-sidebar` hasn't merged), so the whole-frame number
is dominated by the board sitting 23pt left of Figma's. The aligned
main-area diff crops the reference at x 280 and the render at x 257
(1136×1024 each). Three numbers per frame:

- **max-channel**: `figma-diff.sh`'s measure (a pixel differs when any
  channel is off by more than 10%), on the aligned crop;
- **excl.**: the same with three regions left out that no styling change can
  fix: the Review card (the fixture's excerpt follows the issue detail's
  body, a mockup inconsistency the harness documents), New issue (right
  aligned, so 23pt off while the sidebar is narrow) and the 6pt the selected
  card overhangs in Figma (see Departures);
- **AE fuzz**: `magick compare -metric AE -fuzz 10%` (RGB distance), the
  measure `dt-figma-issues-board` reported.

`--2x` falls back to 1x here (no Retina display attached), so close
inspection used 600 to 800% nearest-neighbor crops of the 1x frames and
per-pixel samples.

## Before / after

| Frame | whole frame | main, max-channel | main, excl. | main, AE fuzz |
|---|---|---|---|---|
| issues-dark | 14.82% → 14.81% | 6.07% → 6.14% | 4.91% → 4.98% | 1.14% → 1.25% |
| issues-light | 9.01% → 8.96% | 7.00% → 6.90% | 6.31% → 6.20% | 1.86% → 1.84% |

Re-measured after each rebase onto the umbrella: with
`dt-figma-qa-settings` merged (`b229b3c7`), and again with
`dt-figma-qa-machines`, `dt-figma-qa-activity` and
`dt-figma-qa-terminal-screens` merged (`d586a867`). Both times the board
frames render pixel-identical to the reviewed renders (`magick compare
-metric AE`: 0 pixels, dark and light), and `figma-diff.sh` reports
14.81% / 8.96% whole frame, so every number above stands.

The totals barely move because what remains is text (next section), and
the fixes are small areas: card corners, the focus ring, the filter
buttons, two icons. Measured directly, those now match:

- **Cards**: every card's top, bottom, left and right edge in all five
  columns, corners included (a column of card-fill runs taken 4pt inside
  the left edge reads identically, 104..239 / 254..389 / ...).
- **Focus ring**: the selected card's edge pixels are exact in both themes
  (`1c3f59` ×3 then `52c7f8` dark, `d0efff` ×3 then `2baded` light).
- **Filter buttons**: fill rows 15 to 40 (26pt) and fill `#383d4c` /
  `#f5f6f9` as Figma; the funnel's stroke pixels match to within 1 unit.
- **New issue**: 28pt tall at y 14, the plus in Figma's gray.
- **Unchanged and already exact**: the toolbar (56pt, 12 / 16 across), both
  rules (`#3a404f` / `#eef0f5`, `#2c3140` / `#f6f7fa`), column headings
  (glyph at 16..31 × 73..88, label and count positions), the workflow badge
  (fill `#3a4258` / `#d3d8e4`, 4pt across, 2pt corners), title, excerpt and
  meta rows' vertical rhythm, the footer row.

The dark AE number rose 0.11 points because of the excerpt width (see
Departures): two dark-frame excerpts now wrap one word earlier than the dark
frame, while two light-frame excerpts that didn't now match.

### The residual

What remains is text, in both themes:

- **Glyph advances.** Figma laid the text out with John's locally installed
  Geist (`~/Library/Fonts/GeistVF.ttf`, font version 1.2), which is wider
  than the Geist 1.7.2 the app bundles (`dt-figma-qa-activity` found this).
  `hb-shape` at 12pt Regular: the footer hints are 696.2pt in v1.2 and
  687.4pt in 1.7.2; "jlong/theme-docs" 100.5 and 98.7. A title or branch
  starts on the same pixel and drifts 1 to 3pt by its end, which lights up
  the right half of most text runs at a 10% threshold.
- **Wraps** follow from that: the light frame's excerpts wrap where v1.2 in
  a 244pt box wraps ("Track tokens and estimated cost for each agent run",
  "Reconnect to running sessions after the hub restarts"), a few of which
  1.7.2 in the same box doesn't.
- **Rasterization.** gpui draws Geist a touch heavier than Figma's export
  at 1x (CoreText's smoothing), so stems differ by a fraction of a pixel.
- The footer's `↵` and `⇧` aren't in Geist; each renderer falls back to a
  different face.

None of this is a styling value. Flagged for John: matching Figma's text
exactly means either bundling the v1.2 font or re-exporting the frames with
Geist 1.7.2 installed.

## Fixes

| File | Change |
|---|---|
| `src/views/issues.rs` | Cards (and the drag ghost, and the empty column's placeholder) take Figma's **12pt** corners (`CARD_RADIUS`), not 8pt. The card's radius token resolves to 12 in the card (`get_variable_defs`: `6` → 12); the PNG's corner arc confirms it. |
| `src/theme/style.rs` `focus_shadow` | Figma's **focus ring** stacks the 4px glow over the 1px ring, both outside the control: the ring pixel is the glow composited on the ring (`#52c7f8` dark, `#2baded` light) and 3px of glow show past it. The app drew a 5px glow under the ring (4px past it, ring at full `#62cdf6`). Now `[ring 1px, glow 4px]`, glow on top. Shared, so a dialog's keyboard button and text fields pick it up too (same Figma Focus Ring / Focus Glow layers in the Button component's Focus variant). |
| `src/theme/style.rs` `dropdown` | The board's filters are Figma's X-Small Button with an **18pt** funnel in its 14pt Icon slot, which makes them **26pt** tall (Figma `get_metadata`: 112×26 at y 15). Was a 20pt icon in a 24pt-tall button. The open (Active) state keeps the icon's color (Figma's Active variant does); it went loud before. |
| `src/assets.rs`, `assets/icons/heroicons-18/` | Figma's 18pt button icon (`icons::FUNNEL_BUTTON`) through `dt-figma-qa-activity`'s sized-icon mechanism: an `Icon` carries its size, and an `@button` list beside its `@small` one loads `heroicons-18/`, the outline drawing with its stroke widened to 1.25pt at 18 (Figma's exported funnel: 18pt, stroke 1.25). `ICON_SIZE_BUTTON` joins `ICON_SIZES` (now 20, 14, 18); its guards `every_icon_draws_at_a_figma_size` and the drawing check (each sized drawing is its 20pt original with only `stroke-width` changed) cover it. The 20pt `FUNNEL` stays vendored as that original; no view draws it any more, so the macro now takes attributes per row and it carries `#[allow(dead_code)]`. |
| `src/theme/palette.rs`, `docs/theme.md` | New role **`button_icon`**: the Button component's icon stroke, `#acb4cf` at 60% in Dark, `theme.gray` (`#7b85a7e5`) in Light (Figma's exported SVGs). The filters' funnel and New issue's plus use it; New issue's plus was in the loud label color. Contrast: a new `button_icon on rest button` pairing at 3:1 (non-text); it falls short (2.58 dark, 2.81 light) and is listed as an exception: the icon is decorative beside the button's label. |
| `src/views/issues.rs` | The card **excerpt is 244pt** wide (`SUMMARY_WIDTH`), Figma's description box, not the full 248pt content box. |
| `src/views/issues.rs` `drop_highlight` | **Bug (undrawn state):** a lit drop target painted the whole column solid cyan (dark) / blue (light). Its 1px ring was a drop shadow, which gpui paints as a filled box under the element, and the column's overlay fill is translucent. Now an inset shadow: the overlay fill and a 1px `focus_ring` ring just inside the column. |
| `src/views/issues.rs` `render_card` | **Bug (undrawn state):** the dragged card's placeholder (30% opacity) showed its focus glow through its fill as a solid tint. The placeholder now draws no shadow. |
| `src/visual_views.rs` | `--visual-views` renders the undrawn board states in both themes: `board-workspace-dropdown.png`, `board-empty.png` (a workspace filter empties columns), `board-drag.png` (a real mouse drag over the In progress column, released over the toolbar so nothing moves). |

## Undrawn states (both themes, by eye)

- **Card hovered**: no hover treatment, as before (the pointer cursor only).
  The Figma Issue card component has only `Selected=false` / `true`, so
  there is no hover state to match, and none was invented.
- **Drag in flight**: the target column takes the overlay fill
  (`background`) and a 1px `focus_ring` ring, its heading says "Drop to move
  here"; refused columns dim; the ghost is a card (12pt corners, the
  popover shadow). After the fix above, consistent with the tokens.
- **Empty column**: a column with no cards collapses to the 36pt strip: its
  status glyph (status colors), the count and the caps label down the strip
  in X-Small semibold, muted, loud on the keyboard's column. An expanded
  empty column's dashed placeholder uses `divider_strong`, X-Small `muted`,
  and now the card's 12pt corners.
- **Both filter menus open**: the raised surface (`raised`, the ring, the
  popover shadow), 6pt under the button, the title in X-Small, the current
  row on the overlay fill, counts in `muted`. Consistent.

## Departures, flagged for John

1. **The selected card is 278pt wide in Figma.** The Issue card component's
   `Selected=true` variant hugs its 254pt title plus padding (278pt) while
   `Selected=false` is fixed at 272pt, so in the frame the selected card
   runs 6pt into the gutter. The app keeps every card 272pt (the column's
   width). A Figma inconsistency.
2. **The dark and light frames wrap the excerpts differently.** Both say
   244pt in the export. The light frame's wraps are what Geist v1.2 gives in
   244pt; the dark frame's need about 248pt. `dt-figma-issues-board` matched
   the dark frame (248pt). This QA follows the stated 244pt, so the light
   frame matches more lines and two dark excerpts ("Keep the viewport
   anchored...", "Warn when an agent is still running...") wrap one word
   earlier than the dark frame draws them. Which frame is right?
3. **Text metrics** (the residual above): Figma used Geist v1.2, the app
   bundles 1.7.2.
4. **The button icon's contrast**: Figma's dark `#acb4cf` at 60% is 2.58:1
   on the button, under the 3:1 non-text minimum; kept as drawn (decorative
   beside a label) and listed in `theme.md`'s exceptions.
5. **README wording**: the README says the X-Small filter buttons draw their
   icons at 14pt. In the Issues frame the 14pt is the Icon slot; the funnel
   inside it is 18pt (and the frame's 26pt button height needs the 18pt
   icon). The Activity frame's pickers do draw a 14pt funnel
   (`dt-figma-qa-activity`).

## Notes

- **One icon-size mechanism with `dt-figma-qa-activity`**: the board's
  filters and the Activity pickers share `Icon.size`, `ICON_SIZES` and the
  same guards. They use different drawings because the frames draw
  different sizes: the Activity pickers a 14pt funnel (`FUNNEL_SMALL`,
  `style::filter_dropdown`), the board's X-Small Button an 18pt one in its
  14pt slot (`FUNNEL_BUTTON`, `style::dropdown`).
- The Review card's excerpt and the right-aligned New issue stay off until
  the fixture follows the card (a mockup inconsistency) and the 280pt
  sidebar lands.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings`
  (also with `--features visual-check`) and `cargo test`: clean, 459 tests
  pass (asset guard incl. the 18pt drawing check, contrast and theme-doc
  tests, smoke, drag and drop, board scroll axes).
- `--visual-figma`: 0 checks failed; `--visual-views`: 0 checks failed, the
  new board frames in both themes.
- No change outside `desktop/` (no root crate, no `Cargo.lock`).
