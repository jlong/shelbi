# rt-command-palette-gray-border-top-aligned-height-fits-its-content-while-typing

Done. The command palette now draws a muted `#7a7a7a` single-line border around
its `#292929` panel, opens near the top of the window (horizontally centered),
and sizes its height to its current content — growing and shrinking on every
keystroke as the list filters, capped to the window with the list scrolling
inside when it overflows.

Notes:
- All geometry lives in `palette_rect` (palette.rs) so it's unit-testable; the
  shell's `ActiveOverlay::rect` delegates to `Palette::overlay_rect`.
- Empty results show a single dim "No matching commands" row rather than
  collapsing to nothing.
- The shrink leaves no leftover cells: the shell redraws the full dimmed main
  area each frame before painting the (now smaller) overlay, so cells outside
  the new rect show the view underneath. Verified.
