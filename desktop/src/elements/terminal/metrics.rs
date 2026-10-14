//! Cell geometry: the font, one cell's size in pixels, and the conversions
//! between pixels and cells.
//!
//! Everything except [`CellMetrics::measure`] is plain arithmetic over gpui's
//! geometry types, unit-tested without a window. The pixel-to-grid rule itself
//! (whole cells, at least 1x1) is `shelbi_term::Size::from_pixels`, shared and
//! tested in `shelbi-term`.

use gpui::{
    point, px, size, Bounds, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Pixels,
    Point, Window,
};
use shelbi_term::Size;

/// The default terminal font family: Geist Mono, Shelbi's monospace face,
/// bundled with the app (`assets::load_fonts`), so it is always available.
/// A configured family that isn't installed falls back to it.
pub const PRIMARY_FAMILY: &str = "Geist Mono";

/// The bundled symbol font for the private-use icons (Powerline, Devicons,
/// and the rest of the Nerd Fonts set) that prompts and agents print.
pub const SYMBOLS_FAMILY: &str = "Symbols Nerd Font Mono";

/// The system monospace face: the last explicit fallback, for anything else
/// Geist Mono lacks (box drawing, technical symbols). CJK and emoji fall
/// through to the system's own cascade after it.
pub const FALLBACK_FAMILY: &str = "Menlo";

/// The default terminal font size in logical pixels.
pub const DEFAULT_FONT_SIZE: f32 = 13.0;

/// The least line height, as a multiple of the font size, so rows never
/// crowd (some fonts report a tight ascent + descent).
pub const MIN_LINE_HEIGHT: f32 = 1.25;

/// One cell's size in logical pixels, plus the font it was measured from.
#[derive(Debug, Clone, PartialEq)]
pub struct CellMetrics {
    /// Cell width: the font's advance for one character.
    pub width: Pixels,
    /// Cell height: the line height (ascent + the descent's magnitude,
    /// at least [`MIN_LINE_HEIGHT`] x the font size, rounded up to whole
    /// pixels so rows never straddle a pixel boundary).
    pub height: Pixels,
    /// The font size the cell was measured at.
    pub font_size: Pixels,
    /// The regular-weight font. Bold and italic runs derive from it.
    pub font: Font,
}

impl CellMetrics {
    /// A cell of a given size (tests).
    #[cfg(test)]
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: px(width),
            height: px(height),
            font_size: px(DEFAULT_FONT_SIZE),
            font: terminal_font(FALLBACK_FAMILY),
        }
    }

    /// Measure the terminal font `family` at `size` in `window`. A family
    /// that isn't installed falls back to Geist Mono (bundled), and if even
    /// that is missing (a test without the app's fonts) to Menlo.
    pub fn measure(window: &Window, family: &str, size: f32) -> Self {
        let text = window.text_system();
        let names = text.all_font_names();
        let installed = |f: &str| names.iter().any(|n| n == f);
        let family = [family, PRIMARY_FAMILY]
            .into_iter()
            .find(|f| installed(f))
            .unwrap_or_else(|| {
                tracing::warn!(family, "terminal font not installed; using {FALLBACK_FAMILY}");
                FALLBACK_FAMILY
            });
        let font = terminal_font(family);
        let font_size = px(size);
        let id = text.resolve_font(&font);
        let width = text
            .advance(id, font_size, 'm')
            .map(|a| a.width)
            .unwrap_or(font_size * 0.6);
        // Font metrics report the descent signed (negative, below the
        // baseline), so take its magnitude.
        let natural =
            f32::from(text.ascent(id, font_size)) + f32::from(text.descent(id, font_size)).abs();
        let height = px(natural.max(size * MIN_LINE_HEIGHT).ceil());
        Self {
            width,
            height,
            font_size,
            font,
        }
    }

    /// The grid that fits an area of `area` pixels.
    pub fn grid_size(&self, area: gpui::Size<Pixels>) -> Size {
        Size::from_pixels(
            f32::from(area.width),
            f32::from(area.height),
            f32::from(self.width),
            f32::from(self.height),
        )
    }

    /// The viewer cell under `position`, relative to a grid drawn at `origin`
    /// with `cells` columns and rows. Points outside the grid clamp to its
    /// nearest edge cell, so a selection drag that leaves the pane keeps
    /// extending along the edge.
    pub fn cell_at(
        &self,
        position: Point<Pixels>,
        origin: Point<Pixels>,
        cells: Size,
    ) -> (u16, u16) {
        let axis = |offset: Pixels, cell: Pixels, count: u16| -> u16 {
            let n = (f32::from(offset) / f32::from(cell)).floor();
            if n.is_nan() || n < 0.0 {
                0
            } else {
                (n as u32).min(count.saturating_sub(1) as u32) as u16
            }
        };
        (
            axis(position.x - origin.x, self.width, cells.cols),
            axis(position.y - origin.y, self.height, cells.rows),
        )
    }

    /// The pixel bounds of `len` cells starting at viewer cell (`col`, `row`).
    pub fn cell_bounds(
        &self,
        origin: Point<Pixels>,
        col: u16,
        row: u16,
        len: u16,
    ) -> Bounds<Pixels> {
        Bounds {
            origin: point(
                origin.x + self.width * col as f32,
                origin.y + self.height * row as f32,
            ),
            size: size(self.width * len as f32, self.height),
        }
    }
}

/// The terminal font in `family`: regular weight, ligatures off. Ligatures
/// would merge glyphs ("->" into one arrow) and break the one-glyph-per-cell
/// grid the element paints.
///
/// A glyph the family lacks comes from [`fallbacks`] (Geist Mono, the Nerd
/// symbols, Menlo), then from the system's cascade (CJK, emoji), so nothing
/// renders as a missing-glyph box that any installed font can draw.
pub fn terminal_font(family: &str) -> Font {
    Font {
        family: family.to_string().into(),
        features: FontFeatures(std::sync::Arc::new(vec![
            ("calt".into(), 0),
            ("liga".into(), 0),
        ])),
        fallbacks: Some(FontFallbacks::from_fonts(fallbacks(family))),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

/// The explicit fallback chain for `family`, in order: Geist Mono (unless
/// it is the family), the Nerd symbols, then Menlo.
pub fn fallbacks(family: &str) -> Vec<String> {
    [PRIMARY_FAMILY, SYMBOLS_FAMILY, FALLBACK_FAMILY]
        .into_iter()
        .filter(|f| *f != family)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallbacks_put_geist_mono_first_then_symbols_then_menlo() {
        assert_eq!(fallbacks("Berkeley Mono"), vec!["Geist Mono", "Symbols Nerd Font Mono", "Menlo"]);
        assert_eq!(fallbacks("Geist Mono"), vec!["Symbols Nerd Font Mono", "Menlo"]);
    }

    #[test]
    fn grid_size_is_whole_cells_from_pixel_bounds() {
        let m = CellMetrics::new(8.0, 17.0);
        assert_eq!(m.grid_size(size(px(800.0), px(600.0))), Size::new(100, 35));
        // A fractional cell width (Geist Mono at 13px is about 7.8px).
        let m = CellMetrics::new(7.8, 17.0);
        assert_eq!(m.grid_size(size(px(780.0), px(170.0))), Size::new(100, 10));
        assert_eq!(m.grid_size(size(px(787.0), px(186.9))), Size::new(100, 10));
        // A collapsed pane is still a 1x1 grid.
        assert_eq!(m.grid_size(size(px(0.0), px(0.0))), Size::new(1, 1));
    }

    #[test]
    fn cell_at_maps_pixels_to_cells() {
        let m = CellMetrics::new(8.0, 16.0);
        let origin = point(px(100.0), px(50.0));
        let cells = Size::new(80, 24);
        assert_eq!(m.cell_at(point(px(100.0), px(50.0)), origin, cells), (0, 0));
        assert_eq!(m.cell_at(point(px(107.9), px(65.9)), origin, cells), (0, 0));
        assert_eq!(m.cell_at(point(px(108.0), px(66.0)), origin, cells), (1, 1));
        assert_eq!(
            m.cell_at(
                point(px(100.0 + 8.0 * 79.5), px(50.0 + 16.0 * 23.5)),
                origin,
                cells
            ),
            (79, 23)
        );
    }

    #[test]
    fn cell_at_clamps_outside_the_grid() {
        let m = CellMetrics::new(8.0, 16.0);
        let origin = point(px(100.0), px(50.0));
        let cells = Size::new(80, 24);
        assert_eq!(m.cell_at(point(px(0.0), px(0.0)), origin, cells), (0, 0));
        assert_eq!(
            m.cell_at(point(px(5000.0), px(5000.0)), origin, cells),
            (79, 23)
        );
    }

    #[test]
    fn cell_bounds_cover_the_cells() {
        let m = CellMetrics::new(8.0, 16.0);
        let b = m.cell_bounds(point(px(10.0), px(20.0)), 3, 2, 2);
        assert_eq!(b.origin, point(px(34.0), px(52.0)));
        assert_eq!(b.size, size(px(16.0), px(16.0)));
    }
}
