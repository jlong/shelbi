//! [`TerminalElement`]: paints a [`TerminalPane`]'s cell grid.
//!
//! - **Layout** fills the parent.
//! - **Prepaint** measures the cell (once), sizes the grid from the element's
//!   pixel bounds (reporting a new size to the session, debounced), rebuilds
//!   the damaged rows ([`super::layout::RowCache`]), and shapes each glyph
//!   run. Single-width runs are shaped with `force_width` = the cell width, so
//!   every base glyph lands on its cell whatever the font's own advances; a
//!   wide character is shaped alone at its cell.
//! - **Paint**, back to front: the default background, cell backgrounds,
//!   search and selection highlights, glyphs, the cursor (block, beam,
//!   underline, or hollow when unfocused) with the glyph under a block cursor
//!   repainted in the cursor's text color, and any IME composition. It also
//!   registers the pointer listeners and the input handler.
//!
//! The session grid is letterboxed (centered) or clipped (anchored top-left)
//! into the viewer by `shelbi_term::viewport`, the same placement the TUI uses
//! and the one mouse coordinates map back through.

use std::time::Instant;

use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use gpui::{
    fill, outline, point, px, relative, size, App, BorderStyle, Bounds, ContentMask,
    CursorStyle as PointerStyle, DispatchPhase, Element, ElementId, ElementInputHandler, Entity,
    FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollWheelEvent, ShapedLine, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle,
    Window,
};
use shelbi_term::render::{CursorShape, Underline};
use shelbi_term::Size;

use super::layout::{GlyphRun, Palette};
use super::metrics::CellMetrics;
use super::{Geometry, TerminalPane};
use crate::theme::{rgb_hsla, Theme};

/// The element. Cheap to build each render; all state lives in the pane.
pub struct TerminalElement {
    pane: Entity<TerminalPane>,
}

impl TerminalElement {
    /// An element painting `pane`.
    pub fn new(pane: Entity<TerminalPane>) -> Self {
        Self { pane }
    }
}

/// The top-left pixel of the session grid: the element's origin plus any
/// letterbox margin.
pub(crate) fn grid_origin(g: &Geometry, metrics: &CellMetrics) -> Point<Pixels> {
    point(
        g.bounds.origin.x + metrics.width * g.placement.cols.pad_before as f32,
        g.bounds.origin.y + metrics.height * g.placement.rows.pad_before as f32,
    )
}

/// The cursor as this frame paints it.
pub struct CursorPaint {
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    color: Hsla,
    /// The glyph under a block cursor, shaped in the cursor's text color.
    glyph: Option<(Point<Pixels>, ShapedLine)>,
}

/// Everything prepaint computed for paint.
pub struct Frame {
    hitbox: Hitbox,
    started: Instant,
    rows_rebuilt: usize,
    background: Hsla,
    quads: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<CursorPaint>,
    marked: Option<(Point<Pixels>, ShapedLine)>,
    line_height: Pixels,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Frame {
        let started = Instant::now();
        let theme = cx.global::<Theme>().terminal;
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        // Geometry and damage: the only mutation of the pane in a frame.
        let (metrics, rows, rows_rebuilt, palette) = self.pane.update(cx, |pane, cx| {
            let metrics = pane
                .metrics
                .get_or_insert_with(|| CellMetrics::measure(window))
                .clone();
            let viewer = metrics.grid_size(bounds.size);
            pane.begin_frame(bounds, viewer, cx);
            let placement = pane
                .geometry
                .as_ref()
                .map(|g| g.placement)
                .expect("set by begin_frame");
            let palette = Palette::resolve(&theme, pane.view().emulator());
            let (rows, rebuilt) = pane.layout_rows(placement, &palette);
            (metrics, rows, rebuilt, palette)
        });

        let pane = self.pane.read(cx);
        let geometry = pane.geometry.clone().expect("set by begin_frame");
        let origin = grid_origin(&geometry, &metrics);
        let shown = Size::new(geometry.placement.cols.len, geometry.placement.rows.len);
        let view = pane.view();
        let emu = view.emulator();
        let offset = emu.display_offset() as i32;
        let text = window.text_system().clone();

        // Cell backgrounds, then highlights over them.
        let mut quads = Vec::new();
        for (row, layout) in rows.iter().enumerate() {
            for bg in &layout.backgrounds {
                quads.push((
                    metrics.cell_bounds(origin, bg.col, row as u16, bg.len),
                    bg.color,
                ));
            }
        }
        let search_color = rgb_hsla(theme.search_match);
        let current_color = rgb_hsla(theme.search_current);
        let current = view.search_current();
        for m in view.search_matches() {
            let row = m.line + offset;
            if row < 0 || row >= shown.rows as i32 || m.start_col >= shown.cols {
                continue;
            }
            let len = m.end_col.min(shown.cols) - m.start_col;
            let color = if Some(*m) == current {
                current_color
            } else {
                search_color
            };
            quads.push((
                metrics.cell_bounds(origin, m.start_col, row as u16, len),
                color,
            ));
        }
        if let Some(sel) = view.selection().filter(|s| !s.is_empty()) {
            let (start, end) = sel.range();
            let selection_color = rgb_hsla(theme.selection);
            for row in 0..shown.rows {
                let line = row as i32 - offset;
                if line < start.line || line > end.line {
                    continue;
                }
                let first = if line == start.line { start.col } else { 0 };
                let last = if line == end.line {
                    end.col
                } else {
                    shown.cols.saturating_sub(1)
                };
                let last = last.min(shown.cols.saturating_sub(1));
                if first <= last {
                    quads.push((
                        metrics.cell_bounds(origin, first, row, last - first + 1),
                        selection_color,
                    ));
                }
            }
        }

        // Glyphs.
        let mut lines = Vec::new();
        for (row, layout) in rows.iter().enumerate() {
            for run in &layout.glyphs {
                let shaped = shape_run(&text, &metrics, run, run.style.color);
                let at = metrics.cell_bounds(origin, run.col, row as u16, 1).origin;
                lines.push((at, shaped));
            }
        }

        // The cursor: hidden until a session is attached, while scrolled away,
        // or on the blink's off phase; an outline while the pane isn't focused.
        let focused = pane.is_focused();
        let cursor = emu
            .cursor()
            .filter(|c| c.row < shown.rows && c.col < shown.cols)
            .filter(|_| pane.is_live() && (pane.blink_on || !focused))
            .map(|c| {
                let cell_origin = |col, row| metrics.cell_bounds(origin, col, row, 1).origin;
                let cell = metrics.cell_bounds(origin, c.col, c.row, 1);
                let shape = if focused {
                    c.shape
                } else {
                    CursorShape::HollowBlock
                };
                let color = palette.cursor();
                let glyph = (shape == CursorShape::Block)
                    .then(|| {
                        // The cell's own text (combining marks included), in
                        // its run's weight and slant.
                        let grid = emu.grid();
                        let cell = &grid[Line(c.row as i32 - offset)][Column(c.col as usize)];
                        if matches!(cell.c, ' ' | '\0')
                            || cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                        {
                            return None;
                        }
                        let mut ch = cell.c.to_string();
                        if let Some(marks) = cell.zerowidth() {
                            ch.extend(marks);
                        }
                        let style = rows
                            .get(c.row as usize)?
                            .glyphs
                            .iter()
                            .find(|r| r.col <= c.col && c.col < r.col + r.cells)?
                            .style;
                        let wide = cell.flags.contains(Flags::WIDE_CHAR);
                        let single = GlyphRun {
                            col: c.col,
                            cells: 1,
                            text: ch,
                            style,
                            wide,
                        };
                        Some((
                            cell_origin(c.col, c.row),
                            shape_run(&text, &metrics, &single, rgb_hsla(theme.cursor_text)),
                        ))
                    })
                    .flatten();
                let bounds = match shape {
                    CursorShape::Beam => Bounds {
                        origin: cell.origin,
                        size: size(px(2.0), cell.size.height),
                    },
                    CursorShape::Underline => Bounds {
                        origin: point(cell.origin.x, cell.origin.y + cell.size.height - px(2.0)),
                        size: size(cell.size.width, px(2.0)),
                    },
                    CursorShape::Block | CursorShape::HollowBlock => cell,
                };
                CursorPaint {
                    bounds,
                    shape,
                    color,
                    glyph,
                }
            });

        // An IME composition, drawn at the cursor cell and underlined.
        let marked = pane.marked_text.as_ref().and_then(|t| {
            let c = emu.cursor()?;
            let at = metrics.cell_bounds(origin, c.col, c.row, 1).origin;
            let color = palette.foreground();
            let run = TextRun {
                len: t.len(),
                font: metrics.font.clone(),
                color,
                background_color: Some(palette.background()),
                underline: Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(color),
                    wavy: false,
                }),
                strikethrough: None,
            };
            Some((
                at,
                text.shape_line(t.clone().into(), metrics.font_size, &[run], None),
            ))
        });

        Frame {
            hitbox,
            started,
            rows_rebuilt,
            background: palette.background(),
            quads,
            lines,
            cursor,
            marked,
            line_height: metrics.height,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, frame.background));
            for (b, color) in &frame.quads {
                window.paint_quad(fill(*b, *color));
            }
            for (at, line) in &frame.lines {
                let _ = line.paint(*at, frame.line_height, TextAlign::Left, None, window, cx);
            }
            if let Some(cursor) = &frame.cursor {
                match cursor.shape {
                    CursorShape::HollowBlock => {
                        window.paint_quad(outline(cursor.bounds, cursor.color, BorderStyle::Solid))
                    }
                    _ => window.paint_quad(fill(cursor.bounds, cursor.color)),
                }
                if let Some((at, glyph)) = &cursor.glyph {
                    let _ = glyph.paint(*at, frame.line_height, TextAlign::Left, None, window, cx);
                }
            }
            if let Some((at, line)) = &frame.marked {
                let _ = line.paint(*at, frame.line_height, TextAlign::Left, None, window, cx);
            }
        });

        self.register_listeners(&frame.hitbox, window, cx);
        window.set_cursor_style(PointerStyle::IBeam, &frame.hitbox);
        let focus = self.pane.read(cx).focus_handle_ref().clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.pane.clone()),
            cx,
        );

        let took = frame.started.elapsed();
        let rebuilt = frame.rows_rebuilt;
        self.pane
            .update(cx, |pane, _| pane.stats.record_frame(took, rebuilt));
    }
}

impl TerminalElement {
    fn register_listeners(&self, hitbox: &Hitbox, window: &mut Window, _cx: &mut App) {
        let pane = self.pane.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.is_hovered(window) {
                pane.update(cx, |p, cx| {
                    p.mouse_down(e.button, e.position, e.modifiers, window, cx)
                });
                cx.stop_propagation();
            }
        });
        let pane = self.pane.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && e.pressed_button == Some(gpui::MouseButton::Left) {
                pane.update(cx, |p, cx| p.mouse_drag(e.position, e.modifiers, cx));
            }
        });
        let pane = self.pane.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hb.is_hovered(window);
                pane.update(cx, |p, cx| {
                    p.mouse_up(e.button, e.position, e.modifiers, hovered, cx)
                });
            }
        });
        let pane = self.pane.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.is_hovered(window) {
                pane.update(cx, |p, cx| {
                    p.scroll_wheel(e.delta, e.position, e.modifiers, cx)
                });
                cx.stop_propagation();
            }
        });
    }
}

/// Shape one glyph run in `color`. A single-width run is pinned to the cell
/// grid (`force_width`); a wide character keeps its own advance.
fn shape_run(
    text: &gpui::WindowTextSystem,
    metrics: &CellMetrics,
    run: &GlyphRun,
    color: Hsla,
) -> ShapedLine {
    let s = run.style;
    let mut font = metrics.font.clone();
    if s.bold {
        font.weight = FontWeight::BOLD;
    }
    if s.italic {
        font.style = FontStyle::Italic;
    }
    let underline = (s.underline != Underline::None).then_some(UnderlineStyle {
        thickness: px(1.0),
        color: Some(color),
        wavy: s.underline == Underline::Curly,
    });
    let strikethrough = s.strikethrough.then_some(StrikethroughStyle {
        thickness: px(1.0),
        color: Some(color),
    });
    let runs = [TextRun {
        len: run.text.len(),
        font,
        color,
        background_color: None,
        underline,
        strikethrough,
    }];
    let force_width = (!run.wide).then_some(metrics.width);
    text.shape_line(
        run.text.clone().into(),
        metrics.font_size,
        &runs,
        force_width,
    )
}
