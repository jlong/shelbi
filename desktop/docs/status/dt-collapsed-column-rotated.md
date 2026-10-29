# dt-collapsed-column-rotated: the collapsed column's label, turned

A collapsed column on the desktop board is now Figma's (node `2165:94`):
the column heading turned 90° clockwise down a 24pt strip. The status glyph
sits at the top, where the expanded heading draws it, and the caps label and
the count read down the strip as one line ("BACKLOG 10"). It replaces the
stacked letters the TUI draws, which the TUI keeps.

## Layout

The strip (`IssuesView::render_collapsed`, `src/views/issues.rs`) is 24pt
wide (`COLLAPSED_WIDTH`, was 36) and keeps the heading's own measures: 4pt
in from the top, the 16pt glyph, 8pt, the label in X-Small/Semibold, 8pt,
the count in X-Small. The label is loud on the keyboard's column and muted
otherwise, like the expanded heading. The count is always muted.

The label is the **full** status name. It takes the strip's real height
(the column's leftover room after the glyph, the count and the gaps). Only
a name that runs past that height is cut, with an ellipsis, at the longest
start that fits. Trailing spaces and punctuation are dropped before the
ellipsis. The count is never cut. Figma's turned heading frame is 272pt
long (the expanded column's width), but that width is not the limit here.

The glyph stays upright. Figma turns the whole heading, glyph included, but
the dashed Backlog ring looks the same either way, and an upright glyph
reads as the same status icon as the expanded heading's (the In progress
pie keeps its fill direction).

The click target, hover fill, keyboard focus, drop target and refused-drop
dimming are the strip's, unchanged. Collapse and expand stay an instant
swap: the board has no column animation.

## Drawing turned text

gpui can't rotate text: glyphs go to the scene as axis-aligned sprites, and
only an SVG takes a transformation. `elements::rotated_text` (new):

1. shapes the line with gpui's shaper (CoreText), so the label's length
   and where the ellipsis cuts agree with how the same text lays out
   unrotated;
2. traces each shaped glyph's outline from its font file (`ttf-parser`;
   `assets::ui_face` gives the bundled Geist face for a weight), turned
   into an SVG path at gpui's baseline;
3. paints it with `window.paint_svg` as a monochrome sprite in the text's
   color, the primitive gpui paints glyphs and icons with, so it takes
   theme, contrast and translucency like any text.

A character Geist lacks (CoreText's fallback drew it) is traced from the
first system face that has it (`fontdb`, loaded only the first time one is
needed). Traced labels are cached by text and style, so a frame doesn't
re-trace. The sprite atlas caches the raster per size.

`ttf-parser` and `fontdb` were already in the desktop's tree (resvg's).
They're now direct dependencies, and the desktop `Cargo.lock` gains only
the two edges.

## Checks

- Against the dark Figma frame, the strip's ink box is identical (16×101
  at the same offset) and 1.5 pixels of 3,300 differ at a 10% fuzz.
- The light frame (`2220:20975`) draws Backlog expanded. Its heading shows
  the same muted label and count roles the light strip uses.
- Tests: `elements::rotated_text::tests` (the turn's direction, the trace
  cache, whole vs cut vs no room, a traced path).

## Captures

`SHELBI_VISUAL_ONLY=issues shelbi-desktop --visual-figma <dir>` renders
`state-issues-collapsed{,-long,-cut}-{dark,light}.png`. Captures and
side-by-sides with the Figma frames are in
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-collapsed-column-rotated/`.
