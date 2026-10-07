//! Shared sidebar helpers reused by the single-process shell's sidebar and the
//! review panel: nav-line math, the selection-bleed glyphs, and the palette
//! decoration-colour mapping.

use ratatui::style::Color;
use shelbi_palette::DecorationColor;

/// Rendered line count of the nav block: one item line per nav row plus a
/// separator line above the first, below the last, and between each pair —
/// `2n + 1`. Shared so click mapping and drawing agree on where the
/// rest-of-list begins.
pub fn nav_lines(nav_n: usize) -> usize {
    2 * nav_n + 1
}

/// Lower/upper half-block glyphs used to bleed the selection background half a
/// cell above and below the selected nav row. Drawn with their *foreground*
/// set to the selection background colour so the eye reads the fill as
/// continuing past the row's edges.
pub(crate) const BLEED_ABOVE: &str = "▄"; // U+2584 LOWER HALF BLOCK — sits above the row
pub(crate) const BLEED_BELOW: &str = "▀"; // U+2580 UPPER HALF BLOCK — sits below the row

/// Map the palette's ratatui-free [`DecorationColor`] to a ratatui
/// [`Color`]. Single conversion point so the sidebar and the palette
/// agree on what each decoration tint looks like on screen.
pub fn decoration_to_color(c: DecorationColor) -> Color {
    match c {
        DecorationColor::Default => Color::Reset,
        DecorationColor::Gray => Color::Gray,
        DecorationColor::DarkGray => Color::DarkGray,
        DecorationColor::Muted => crate::theme::PALETTE_MUTED,
        DecorationColor::Green => Color::Green,
        DecorationColor::Yellow => Color::Yellow,
        DecorationColor::Red => Color::Red,
        DecorationColor::Cyan => Color::Cyan,
        DecorationColor::Blue => Color::Blue,
    }
}
