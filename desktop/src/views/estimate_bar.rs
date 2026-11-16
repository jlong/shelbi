//! The estimate bar along a board card's bottom edge: elapsed / estimate
//! for an issue started with one ([`shelbi_app::estimate`]).
//!
//! The bar is flush with the card's bottom edge and spans its full width,
//! clipped to the card's rounded corners. gpui's content masks are
//! rectangular, so the clip can't come from the card's `overflow_hidden`:
//! the bar paints itself as a path, a band of the card's bottom with its
//! ends following the corner curves. The fill is the same band painted under
//! a mask cut at the elapsed share, over the track; past the estimate it is
//! full and takes the warning color.

use gpui::{
    canvas, point, prelude::*, px, size, Bounds, ContentMask, Hsla, PathBuilder, Pixels,
};
use shelbi_app::estimate::EstimateProgress;

use crate::theme::Theme;

/// The bar's thickness.
pub const BAR_H: f32 = 3.0;

/// How far in from the card's side the band's top edge starts: where a line
/// `thickness` above the bottom meets a corner of `radius`. Zero when the
/// band is as tall as the corner.
pub fn band_inset(radius: f32, thickness: f32) -> f32 {
    if radius <= 0.0 || thickness >= radius {
        return 0.0;
    }
    let rise = radius - thickness;
    radius - (radius * radius - rise * rise).sqrt()
}

/// The card's bottom band in `bounds` (the card's bottom `radius` rows, its
/// full width): a strip `thickness` tall whose ends follow the corners.
fn band_path(bounds: Bounds<Pixels>, radius: f32, thickness: f32) -> Option<gpui::Path<Pixels>> {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    let r = radius.min(w / 2.0).min(h);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let o = bounds.origin;
    let at = |x: f32, y: f32| point(o.x + px(x), o.y + px(y));
    let top = h - thickness.min(h);
    let inset = band_inset(r, thickness);
    let radii = point(px(r), px(r));
    let mut b = PathBuilder::fill();
    b.move_to(at(inset, top));
    b.line_to(at(w - inset, top));
    // Down the bottom-right corner, along the bottom, up the bottom-left.
    b.arc_to(radii, px(0.0), false, true, at(w - r, h));
    b.line_to(at(r, h));
    b.arc_to(radii, px(0.0), false, true, at(inset, top));
    b.close();
    b.build().ok()
}

/// The fill's color: the progress role, the warning color once overdue.
pub fn fill_color(progress: &EstimateProgress, theme: &Theme) -> Hsla {
    if progress.overdue() {
        theme.warning
    } else {
        theme.progress
    }
}

/// The bar for a card of corner `radius`, filling its parent: the caller
/// places that over the card's bottom `radius` rows (inside its padding, so
/// it covers no content). It takes no pointer events.
pub fn estimate_bar(progress: EstimateProgress, radius: f32, theme: &Theme) -> impl IntoElement {
    let fraction = progress.fraction();
    let fill = fill_color(&progress, theme);
    // The segmented control's track: it reads on the card in both themes
    // (`divider` all but vanishes on the dark card).
    let track = theme.search;
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let Some(band) = band_path(bounds, radius, BAR_H) else {
                return;
            };
            window.paint_path(band, track);
            if fraction <= 0.0 {
                return;
            }
            let Some(band) = band_path(bounds, radius, BAR_H) else {
                return;
            };
            let filled = Bounds {
                origin: bounds.origin,
                size: size(bounds.size.width * fraction, bounds.size.height),
            };
            window.with_content_mask(Some(ContentMask { bounds: filled }), |window| {
                window.paint_path(band, fill);
            });
        },
    )
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_starts_where_its_top_meets_the_corner() {
        // A 3pt band on a 12pt corner: its top edge is 9pt from the corner's
        // center, so it meets the curve sqrt(144 - 81) = 7.94pt out.
        let inset = band_inset(12.0, 3.0);
        assert!((inset - (12.0 - 63f32.sqrt())).abs() < 1e-4, "{inset}");
        assert_eq!(band_inset(12.0, 12.0), 0.0);
        assert_eq!(band_inset(0.0, 3.0), 0.0);
    }

    #[test]
    fn the_fill_takes_the_warning_color_once_overdue() {
        let theme = Theme::resolve(
            shelbi_app::theme::ThemePreference::Dark,
            gpui::WindowAppearance::Dark,
        );
        let on_time = EstimateProgress {
            estimate: std::time::Duration::from_secs(3600),
            elapsed: std::time::Duration::from_secs(600),
        };
        let overdue = EstimateProgress {
            estimate: std::time::Duration::from_secs(3600),
            elapsed: std::time::Duration::from_secs(4000),
        };
        assert_eq!(fill_color(&on_time, &theme), theme.progress);
        assert_eq!(fill_color(&overdue, &theme), theme.warning);
        assert_ne!(theme.progress, theme.warning);
    }
}
