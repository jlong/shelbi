//! Shared sidebar helpers reused by the single-process shell's sidebar and the
//! review panel: nav-line math, the selection-bleed glyphs, and the palette
//! decoration-colour mapping.

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
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

/// Interleave caller-built `content` lines (one per item, already styled, with
/// the selected item's solid fill baked in) with the half-block bleed
/// separators that make the selected row read as a taller pill. Returns
/// `nav_lines(content.len())` (`2n + 1`) lines: a separator before the first
/// item, between each pair, and after the last. The selected item's adjacent
/// separators carry the half-block bleed glyphs in `fill`; every other
/// separator is blank. The content line for item `p` lands at offset `2p + 1`,
/// its bleed-above at `2p` and bleed-below at `2p + 2` — the one shared trick
/// the sidebar nav, the review panel nav, and the command palette all draw.
pub fn bleed_block_lines<'a>(
    content: Vec<Line<'a>>,
    selected: Option<usize>,
    width: usize,
    fill: Color,
) -> Vec<Line<'a>> {
    let n = content.len();
    let mut out: Vec<Line<'a>> = Vec::with_capacity(nav_lines(n));
    let mut content = content.into_iter();
    for p in 0..=n {
        let glyph = if selected == Some(p) {
            Some(BLEED_ABOVE)
        } else if p > 0 && selected == Some(p - 1) {
            Some(BLEED_BELOW)
        } else {
            None
        };
        out.push(match glyph {
            Some(g) => Line::from(Span::styled(g.repeat(width), Style::default().fg(fill))),
            None => Line::raw(""),
        });
        if let Some(line) = content.next() {
            out.push(line);
        }
    }
    out
}

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
