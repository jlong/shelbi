//! Box-drawing characters drawn as lines rather than font glyphs.
//!
//! A font's `─` or `│` is a stroke about one pixel thick placed wherever the
//! font put it, so at 1x it usually straddles a pixel boundary: a 2px smear
//! at half strength instead of a line (the Codex header box, the composer
//! rules), with seams where cells meet. Like other terminals, the pane draws
//! the common box-drawing characters itself: straight arms as quads snapped
//! to device pixels, rounded corners as stroked arcs that meet them, in the
//! cell's color. Light lines are 1pt at the default 14pt size (scaled with
//! the font), heavy lines twice that. Anything not listed here (double,
//! dashed, mixed-weight) still comes from the font.
//!
//! The block elements (`▀ ▄ █ ▌ ▐`, the eighths, the quadrants `▖ ▛ ▜ ...`)
//! are drawn too, as filled parts of the cell snapped to device pixels, so a
//! run of them is solid: from a fallback font they leave seams and stripes
//! (Claude Code's mascot, `▐▛███▜▌`). The shades (`░ ▒ ▓`) stay the font's.

use gpui::{point, px, size, Bounds, Pixels, Point};

/// How thick one arm of a box-drawing character is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Weight {
    None,
    Light,
    Heavy,
}

/// The four arms from the cell's center to its edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arms {
    pub left: Weight,
    pub right: Weight,
    pub up: Weight,
    pub down: Weight,
}

/// A rounded corner's two arms (`╭` is right and down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    RightDown,
    LeftDown,
    LeftUp,
    RightUp,
}

/// A part of a cell a block element fills, in eighths of the cell from its
/// top-left corner: `(left, top, right, bottom)`.
pub type Eighths = (u8, u8, u8, u8);

/// A character the pane draws itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxGlyph {
    Lines(Arms),
    Arc(Corner),
    /// A block element: the parts it fills (which never overlap).
    Block(&'static [Eighths]),
}

const UPPER_LEFT: Eighths = (0, 0, 4, 4);
const UPPER_RIGHT: Eighths = (4, 0, 8, 4);
const LOWER_LEFT: Eighths = (0, 4, 4, 8);
const LOWER_RIGHT: Eighths = (4, 4, 8, 8);

/// The drawing for `c`, or `None` when the font draws it.
pub fn builtin(c: char) -> Option<BoxGlyph> {
    use Weight::{Heavy as H, Light as L, None as N};
    let arms = |left, right, up, down| Some(BoxGlyph::Lines(Arms { left, right, up, down }));
    match c {
        '─' => arms(L, L, N, N),
        '━' => arms(H, H, N, N),
        '│' => arms(N, N, L, L),
        '┃' => arms(N, N, H, H),
        '┌' => arms(N, L, N, L),
        '┏' => arms(N, H, N, H),
        '┐' => arms(L, N, N, L),
        '┓' => arms(H, N, N, H),
        '└' => arms(N, L, L, N),
        '┗' => arms(N, H, H, N),
        '┘' => arms(L, N, L, N),
        '┛' => arms(H, N, H, N),
        '├' => arms(N, L, L, L),
        '┣' => arms(N, H, H, H),
        '┤' => arms(L, N, L, L),
        '┫' => arms(H, N, H, H),
        '┬' => arms(L, L, N, L),
        '┳' => arms(H, H, N, H),
        '┴' => arms(L, L, L, N),
        '┻' => arms(H, H, H, N),
        '┼' => arms(L, L, L, L),
        '╋' => arms(H, H, H, H),
        '╴' => arms(L, N, N, N),
        '╵' => arms(N, N, L, N),
        '╶' => arms(N, L, N, N),
        '╷' => arms(N, N, N, L),
        '╸' => arms(H, N, N, N),
        '╹' => arms(N, N, H, N),
        '╺' => arms(N, H, N, N),
        '╻' => arms(N, N, N, H),
        '╭' => Some(BoxGlyph::Arc(Corner::RightDown)),
        '╮' => Some(BoxGlyph::Arc(Corner::LeftDown)),
        '╯' => Some(BoxGlyph::Arc(Corner::LeftUp)),
        '╰' => Some(BoxGlyph::Arc(Corner::RightUp)),
        '▀'..='▟' => block(c).map(BoxGlyph::Block),
        _ => None,
    }
}

/// The parts block element `c` fills (U+2580 to U+259F), or `None` for the
/// shades, which the font draws.
fn block(c: char) -> Option<&'static [Eighths]> {
    Some(match c {
        '▀' => &[(0, 0, 8, 4)],
        '▁' => &[(0, 7, 8, 8)],
        '▂' => &[(0, 6, 8, 8)],
        '▃' => &[(0, 5, 8, 8)],
        '▄' => &[(0, 4, 8, 8)],
        '▅' => &[(0, 3, 8, 8)],
        '▆' => &[(0, 2, 8, 8)],
        '▇' => &[(0, 1, 8, 8)],
        '█' => &[(0, 0, 8, 8)],
        '▉' => &[(0, 0, 7, 8)],
        '▊' => &[(0, 0, 6, 8)],
        '▋' => &[(0, 0, 5, 8)],
        '▌' => &[(0, 0, 4, 8)],
        '▍' => &[(0, 0, 3, 8)],
        '▎' => &[(0, 0, 2, 8)],
        '▏' => &[(0, 0, 1, 8)],
        '▐' => &[(4, 0, 8, 8)],
        '▔' => &[(0, 0, 8, 1)],
        '▕' => &[(7, 0, 8, 8)],
        '▖' => &[LOWER_LEFT],
        '▗' => &[LOWER_RIGHT],
        '▘' => &[UPPER_LEFT],
        '▙' => &[UPPER_LEFT, LOWER_LEFT, LOWER_RIGHT],
        '▚' => &[UPPER_LEFT, LOWER_RIGHT],
        '▛' => &[UPPER_LEFT, UPPER_RIGHT, LOWER_LEFT],
        '▜' => &[UPPER_LEFT, UPPER_RIGHT, LOWER_RIGHT],
        '▝' => &[UPPER_RIGHT],
        '▞' => &[UPPER_RIGHT, LOWER_LEFT],
        '▟' => &[UPPER_RIGHT, LOWER_LEFT, LOWER_RIGHT],
        _ => return None,
    })
}

/// The light line's thickness at `font_size`: 1pt at the default 14pt.
pub fn light_thickness(font_size: Pixels) -> Pixels {
    px((font_size.as_f32() / 14.0).max(0.5))
}

/// A rounded corner as a stroke: from `from`, a straight line to the arc's
/// start, a quarter circle of `radius`, then a straight line to `to`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArcStroke {
    pub from: Point<Pixels>,
    pub arc_start: Point<Pixels>,
    pub arc_end: Point<Pixels>,
    pub to: Point<Pixels>,
    pub radius: Pixels,
    /// The arc turns clockwise (SVG's sweep flag).
    pub clockwise: bool,
    pub width: Pixels,
}

/// What one cell draws.
#[derive(Debug, Clone, PartialEq)]
pub enum Drawing {
    Quads(Vec<Bounds<Pixels>>),
    Arc(ArcStroke),
}

/// Round `v` to the nearest device pixel at `scale`.
fn snap(v: f32, scale: f32) -> f32 {
    (v * scale).round() / scale
}

/// A thickness in whole device pixels, at least one.
fn snap_width(w: f32, scale: f32) -> f32 {
    (w * scale).round().max(1.0) / scale
}

/// The band a line of `width` centered on `center` covers, in device pixels.
fn band(center: f32, width: f32, scale: f32) -> (f32, f32) {
    let w = snap_width(width, scale);
    let start = snap(center - w / 2.0, scale);
    (start, start + w)
}

/// `glyph` in `cell`, with light lines `light` thick, snapped to device
/// pixels at `scale`. Arms meet without overlapping, so a translucent (dim)
/// color doesn't darken where they join.
pub fn draw(glyph: BoxGlyph, cell: Bounds<Pixels>, light: Pixels, scale: f32) -> Drawing {
    let (left, top) = (snap(cell.left().as_f32(), scale), snap(cell.top().as_f32(), scale));
    let (right, bottom) = (snap(cell.right().as_f32(), scale), snap(cell.bottom().as_f32(), scale));
    let cx = cell.left().as_f32() + cell.size.width.as_f32() / 2.0;
    let cy = cell.top().as_f32() + cell.size.height.as_f32() / 2.0;
    let thickness = |w: Weight| match w {
        Weight::None | Weight::Light => light.as_f32(),
        Weight::Heavy => light.as_f32() * 2.0,
    };
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Bounds {
        origin: point(px(x0), px(y0)),
        size: size(px(x1 - x0), px(y1 - y0)),
    };
    match glyph {
        BoxGlyph::Lines(a) => {
            let horizontal = a.left.max(a.right);
            let vertical = a.up.max(a.down);
            let mut quads = Vec::new();
            // The horizontal line's band (a light one's when there is none:
            // where a lone vertical line splits).
            let (hy0, hy1) = band(cy, thickness(horizontal), scale);
            if horizontal != Weight::None {
                // Through the center, as wide as the vertical line it crosses
                // (or itself, for a half line), so it covers where the arms
                // meet.
                let across = if vertical == Weight::None { horizontal } else { vertical };
                let (vx0, vx1) = band(cx, thickness(across), scale);
                let x0 = if a.left == Weight::None { vx0 } else { left };
                let x1 = if a.right == Weight::None { vx1 } else { right };
                quads.push(rect(x0, hy0, x1, hy1));
            }
            // Vertical arms stop at the horizontal line; without one, two
            // arms meet at its band's top and a lone arm runs past the center.
            let up_end = if horizontal != Weight::None || a.down != Weight::None { hy0 } else { hy1 };
            let down_start = if horizontal != Weight::None { hy1 } else { hy0 };
            if a.up != Weight::None {
                let (x0, x1) = band(cx, thickness(a.up), scale);
                quads.push(rect(x0, top, x1, up_end));
            }
            if a.down != Weight::None {
                let (x0, x1) = band(cx, thickness(a.down), scale);
                quads.push(rect(x0, down_start, x1, bottom));
            }
            Drawing::Quads(quads)
        }
        BoxGlyph::Arc(corner) => {
            let (hy0, hy1) = band(cy, light.as_f32(), scale);
            let (vx0, vx1) = band(cx, light.as_f32(), scale);
            // The stroke runs down the middle of the bands the straight
            // lines next to it fill.
            let (my, mx) = ((hy0 + hy1) / 2.0, (vx0 + vx1) / 2.0);
            let radius = (cell.size.width.as_f32().min(cell.size.height.as_f32()) / 2.0).max(1.0);
            let (horizontal_edge, dx) = match corner {
                Corner::RightDown | Corner::RightUp => (right, 1.0),
                Corner::LeftDown | Corner::LeftUp => (left, -1.0),
            };
            let (vertical_edge, dy) = match corner {
                Corner::RightDown | Corner::LeftDown => (bottom, 1.0),
                Corner::RightUp | Corner::LeftUp => (top, -1.0),
            };
            Drawing::Arc(ArcStroke {
                from: point(px(horizontal_edge), px(my)),
                arc_start: point(px(mx + dx * radius), px(my)),
                arc_end: point(px(mx), px(my + dy * radius)),
                to: point(px(mx), px(vertical_edge)),
                radius: px(radius),
                clockwise: matches!(corner, Corner::LeftDown | Corner::RightUp),
                width: px(hy1 - hy0),
            })
        }
        BoxGlyph::Block(parts) => {
            // Each eighth's edge snapped on its own, so a cell's edge is the
            // same pixel as its neighbor's and the parts tile the cell.
            let (w, h) = (cell.size.width.as_f32(), cell.size.height.as_f32());
            let x = |e: u8| snap(cell.left().as_f32() + w * e as f32 / 8.0, scale);
            let y = |e: u8| snap(cell.top().as_f32() + h * e as f32 / 8.0, scale);
            Drawing::Quads(
                parts
                    .iter()
                    .map(|&(x0, y0, x1, y1)| rect(x(x0), y(y0), x(x1), y(y1)))
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(16.0 + 8.4 * 3.0), px(12.0 + 18.0 * 2.0)),
            size: size(px(8.4), px(18.0)),
        }
    }

    fn quads(c: char, scale: f32) -> Vec<Bounds<Pixels>> {
        match draw(builtin(c).unwrap(), cell(), px(1.0), scale) {
            Drawing::Quads(q) => q,
            other => panic!("{c}: {other:?}"),
        }
    }

    fn on_device_pixels(b: &Bounds<Pixels>, scale: f32) -> bool {
        [b.left().as_f32(), b.top().as_f32(), b.right().as_f32(), b.bottom().as_f32()]
            .iter()
            .all(|v| ((v * scale) - (v * scale).round()).abs() < 1e-3)
    }

    #[test]
    fn a_horizontal_line_is_one_crisp_pixel_across_the_cell() {
        for scale in [1.0, 2.0] {
            let q = quads('─', scale);
            assert_eq!(q.len(), 1);
            let b = q[0];
            assert!(on_device_pixels(&b, scale), "{b:?} at {scale}x");
            assert_eq!(b.size.height, px(1.0));
            // The whole cell, edge to edge (to the device pixel), so a row
            // of them is one unbroken line.
            assert!((b.left().as_f32() - cell().left().as_f32()).abs() <= 0.5 / scale);
            assert!((b.right().as_f32() - cell().right().as_f32()).abs() <= 0.5 / scale);
            // Through the middle.
            assert!((b.top().as_f32() + 0.5 - (cell().top().as_f32() + 9.0)).abs() <= 0.5);
        }
    }

    #[test]
    fn neighbors_meet_on_the_same_pixel() {
        let next = Bounds { origin: point(cell().right(), cell().top()), ..cell() };
        let a = quads('─', 1.0)[0];
        let Drawing::Quads(b) = draw(builtin('─').unwrap(), next, px(1.0), 1.0) else {
            unreachable!()
        };
        assert_eq!(a.right(), b[0].left());
    }

    #[test]
    fn a_vertical_line_runs_top_to_bottom() {
        let q = quads('│', 1.0);
        assert_eq!(q.len(), 2, "two arms");
        assert_eq!(q[0].top(), cell().top());
        assert_eq!(q[1].bottom(), cell().bottom());
        assert_eq!(q[0].bottom(), q[1].top(), "the arms meet without a gap");
        assert!(q.iter().all(|b| b.size.width == px(1.0)));
    }

    #[test]
    fn joined_arms_never_overlap() {
        for c in ['┼', '├', '┤', '┬', '┴', '└', '┘', '┌', '┐', '╋', '┣'] {
            let q = quads(c, 1.0);
            for (i, a) in q.iter().enumerate() {
                for b in &q[i + 1..] {
                    assert!(!a.intersects(b), "{c}: {a:?} overlaps {b:?}");
                }
            }
        }
    }

    #[test]
    fn heavy_lines_are_twice_as_thick() {
        assert_eq!(quads('━', 1.0)[0].size.height, px(2.0));
        assert!(quads('┃', 1.0).iter().all(|b| b.size.width == px(2.0)));
    }

    #[test]
    fn a_rounded_corner_meets_its_straight_neighbors() {
        let Drawing::Arc(arc) = draw(builtin('╭').unwrap(), cell(), px(1.0), 1.0) else {
            panic!("an arc")
        };
        let line = quads('─', 1.0)[0];
        let rule = quads('│', 1.0)[0];
        // The arc's horizontal end is at the line's middle, on the cell's
        // right edge; its vertical end at the rule's middle, on the bottom.
        assert_eq!(arc.from.y.as_f32(), (line.top().as_f32() + line.bottom().as_f32()) / 2.0);
        assert_eq!(arc.from.x, line.right());
        assert_eq!(arc.to.x.as_f32(), (rule.left().as_f32() + rule.right().as_f32()) / 2.0);
        assert_eq!(arc.to.y, cell().bottom());
        assert_eq!(arc.width, px(1.0));
    }

    #[test]
    fn block_elements_fill_their_parts_and_neighbors_join() {
        let c = cell();
        let full = quads('█', 1.0);
        assert_eq!(full.len(), 1);
        assert!(on_device_pixels(&full[0], 1.0));
        assert!((full[0].size.height.as_f32() - 18.0).abs() < 1e-3);
        // ▐ is the right half, ▄ the bottom half.
        let right = quads('▐', 1.0)[0];
        assert_eq!(right.right(), full[0].right());
        assert!((right.left().as_f32() - (c.left().as_f32() + 4.2)).abs() <= 0.5);
        let lower = quads('▄', 1.0)[0];
        assert_eq!((lower.top(), lower.bottom()), (px(c.top().as_f32() + 9.0), full[0].bottom()));
        // A quadrant set: ▛ is three quarters that tile without overlapping
        // and leave the lower right empty.
        let q = quads('▛', 2.0);
        assert_eq!(q.len(), 3);
        for (i, a) in q.iter().enumerate() {
            assert!(on_device_pixels(a, 2.0));
            for b in &q[i + 1..] {
                assert!(!a.intersects(b), "{a:?} overlaps {b:?}");
            }
        }
        let lr = point(c.right() - px(1.0), c.bottom() - px(1.0));
        assert!(!q.iter().any(|b| b.contains(&lr)));
        // Side by side, two full blocks share an edge: no seam.
        let next = Bounds { origin: point(c.right(), c.top()), ..c };
        let Drawing::Quads(b) = draw(builtin('█').unwrap(), next, px(1.0), 1.0) else {
            unreachable!()
        };
        assert_eq!(full[0].right(), b[0].left());
        // The shades stay the font's.
        for shade in ['░', '▒', '▓'] {
            assert_eq!(builtin(shade), None);
        }
    }

    #[test]
    fn other_characters_come_from_the_font() {
        for c in ['═', '║', '╔', '┄', 'a', ' ', '•'] {
            assert_eq!(builtin(c), None, "{c}");
        }
    }
}
