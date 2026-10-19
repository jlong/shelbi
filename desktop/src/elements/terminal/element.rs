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
//!   search and selection highlights ([`super::highlights`]), glyphs, a
//!   Cmd-hovered link's underline, the cursor (block, beam, underline, or
//!   hollow when unfocused) with the glyph under a block cursor repainted in
//!   the cursor's text color, any IME composition (over the cells at the
//!   cursor, underlined, its selected clause underlined thicker), and the
//!   scrollbar. It also registers the pointer listeners and the input
//!   handler. (Modifier changes reach the pane's own div: gpui sends them
//!   along the focus path, which ends at the focused pane, not this child.)
//!
//! The session grid is letterboxed (centered) or clipped into the viewer by
//! `TerminalView::placement`, the same placement the TUI uses and the one
//! mouse coordinates map back through. A clipped window keeps the bottom of a
//! taller session's content in view, so its rows start at viewport row
//! `placement.rows.src_start`, not 0 (`dt-terminal-bottom-clipped`).

use std::time::Instant;

use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use gpui::{
    fill, outline, point, px, relative, size, App, BorderStyle, Bounds, ContentMask,
    CursorStyle as PointerStyle, DispatchPhase, Element, ElementId, ElementInputHandler, Entity,
    FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Path, PathBuilder, Pixels, Point,
    ScrollWheelEvent, ShapedLine, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle,
    Window,
};
use shelbi_term::render::{CursorShape, Underline};
use shelbi_term::Size;

use super::{boxdraw, highlights};
use super::layout::{GlyphRun, Palette};
use super::metrics::CellMetrics;
use super::{scrollbar, Geometry, TerminalPane};
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

/// An IME composition as this frame paints it.
pub struct CompositionPaint {
    /// Covers the cells the composition is drawn over.
    background: Bounds<Pixels>,
    origin: Point<Pixels>,
    line: ShapedLine,
    /// The thick underline under the input method's selected clause.
    clause: Option<Bounds<Pixels>>,
}

/// Everything prepaint computed for paint.
pub struct Frame {
    hitbox: Hitbox,
    started: Instant,
    rows_rebuilt: usize,
    background: Hsla,
    quads: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    /// Box-drawing lines ([`boxdraw`]): straight arms, then rounded corners.
    box_quads: Vec<(Bounds<Pixels>, Hsla)>,
    box_arcs: Vec<(Path<Pixels>, Hsla)>,
    /// Quads painted over the glyphs (a hovered link's underline).
    over: Vec<(Bounds<Pixels>, Hsla)>,
    cursor: Option<CursorPaint>,
    composition: Option<CompositionPaint>,
    /// The scrollbar thumb.
    scrollbar: Option<(Bounds<Pixels>, Hsla)>,
    background_fill: Hsla,
    line_height: Pixels,
    pointer: PointerStyle,
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
        // The theme's terminal colors, with the ANSI colors the user changed.
        let theme = cx.global::<Theme>().terminal.with_ansi(&self.pane.read(cx).settings.ansi);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let held = super::ResizeHold::holds(window, cx);
        // Geometry and damage: the only mutation of the pane in a frame.
        let (metrics, rows, rows_rebuilt, palette) = self.pane.update(cx, |pane, cx| {
            let (family, size) = (pane.settings.font_family.clone(), pane.settings.font_size);
            let metrics = pane
                .metrics
                .get_or_insert_with(|| CellMetrics::cached(window, &family, size))
                .clone();
            let viewer = metrics.grid_size(bounds.size);
            pane.begin_frame(bounds, viewer, held, cx);
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
        let offset = emu.display_offset();
        // The grid line on the first row shown, and the viewport row it is.
        let top = view.first_shown_line(&geometry.placement);
        let first = geometry.placement.rows.src_start;
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
        for (span, current) in
            highlights::match_spans(view.search_matches(), view.search_current(), top, shown)
        {
            let color = if current { current_color } else { search_color };
            quads.push((metrics.cell_bounds(origin, span.col, span.row, span.len), color));
        }
        if let Some(sel) = view.selection().filter(|s| !s.is_empty()) {
            let selection_color = rgb_hsla(theme.selection);
            for span in highlights::selection_spans(&sel, top, shown) {
                quads.push((
                    metrics.cell_bounds(origin, span.col, span.row, span.len),
                    selection_color,
                ));
            }
        }

        // A Cmd-hovered link's underline, along the bottom of its cells.
        let mut over = Vec::new();
        if let Some(link) = pane.hovered_link.as_ref() {
            for span in highlights::link_spans(link, top, shown) {
                let cells = metrics.cell_bounds(origin, span.col, span.row, span.len);
                over.push((
                    Bounds {
                        origin: point(cells.left(), cells.bottom() - px(2.0)),
                        size: size(cells.size.width, px(1.0)),
                    },
                    palette.foreground(),
                ));
            }
        }
        let offset = offset as i32;

        // Glyphs.
        let mut lines = Vec::new();
        for (row, layout) in rows.iter().enumerate() {
            for run in &layout.glyphs {
                let shaped = shape_run(&text, &metrics, run, run.style.color);
                let at = metrics.cell_bounds(origin, run.col, row as u16, 1).origin;
                lines.push((at, shaped));
            }
        }

        // Box drawing, as lines snapped to device pixels.
        let (mut box_quads, mut box_arcs) = (Vec::new(), Vec::new());
        let scale = window.scale_factor();
        let light = boxdraw::light_thickness(metrics.font_size);
        for (row, layout) in rows.iter().enumerate() {
            for b in &layout.boxes {
                let cell = metrics.cell_bounds(origin, b.col, row as u16, 1);
                match boxdraw::draw(b.glyph, cell, light, scale) {
                    boxdraw::Drawing::Quads(quads) => {
                        box_quads.extend(quads.into_iter().map(|q| (q, b.color)))
                    }
                    boxdraw::Drawing::Arc(arc) => {
                        let mut path = PathBuilder::stroke(arc.width);
                        path.move_to(arc.from);
                        path.line_to(arc.arc_start);
                        path.arc_to(
                            point(arc.radius, arc.radius),
                            px(0.0),
                            false,
                            arc.clockwise,
                            arc.arc_end,
                        );
                        path.line_to(arc.to);
                        if let Ok(path) = path.build() {
                            box_arcs.push((path, b.color));
                        }
                    }
                }
            }
        }

        // The cursor: hidden until a session is attached, while scrolled away,
        // on the blink's off phase, or under a composition; an outline while
        // the pane isn't focused.
        let focused = pane.is_focused();
        let composing_here = pane.composition.is_some() && !pane.search_open();
        // The cursor's viewport row, as a row of the window.
        let cursor = emu
            .cursor()
            .and_then(|c| {
                let row = c.row.checked_sub(first)?;
                (row < shown.rows && c.col < shown.cols).then_some((c, row))
            })
            .filter(|_| pane.is_live() && (pane.blink_on || !focused))
            .filter(|_| !composing_here)
            .map(|(c, row)| {
                let cell_origin = |col, row| metrics.cell_bounds(origin, col, row, 1).origin;
                let cell = metrics.cell_bounds(origin, c.col, row, 1);
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
                            .get(row as usize)?
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
                            cell_origin(c.col, row),
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

        // An IME composition, drawn over the cells at the cursor and
        // underlined, its selected clause underlined thicker. (In the find
        // bar while that is open; the bar draws it.)
        let composition = pane
            .composition
            .as_ref()
            .filter(|_| !pane.search_open())
            .and_then(|comp| {
                let c = emu.cursor()?;
                let at = metrics.cell_bounds(origin, c.col, c.row.checked_sub(first)?, 1).origin;
                let color = palette.foreground();
                let run = TextRun {
                    len: comp.text.len(),
                    font: metrics.font.clone(),
                    color,
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(color),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                let line = text.shape_line(comp.text.clone().into(), metrics.font_size, &[run], None);
                let x_of = |utf16: usize| {
                    let byte = super::utf16_to_byte_range(&comp.text, 0..utf16).end;
                    line.x_for_index(byte)
                };
                let clause = (comp.selected.start < comp.selected.end).then(|| {
                    let (x0, x1) = (x_of(comp.selected.start), x_of(comp.selected.end));
                    Bounds {
                        origin: point(at.x + x0, at.y + metrics.height - px(2.0)),
                        size: size(x1 - x0, px(2.0)),
                    }
                });
                let width = line.width.max(metrics.width);
                Some(CompositionPaint {
                    background: Bounds {
                        origin: at,
                        size: size(width, metrics.height),
                    },
                    origin: at,
                    line,
                    clause,
                })
            });

        // The scrollbar thumb, over the right edge.
        let scrollbar = pane.scrollbar_thumb().map(|thumb| {
            let color = if pane.scrollbar_active() || pane.scrollbar_hovered {
                cx.global::<Theme>().icon
            } else {
                cx.global::<Theme>().muted.opacity(0.6)
            };
            (
                Bounds {
                    origin: point(
                        bounds.right() - px(scrollbar::WIDTH + scrollbar::INSET),
                        bounds.top() + px(thumb.top),
                    ),
                    size: size(px(scrollbar::WIDTH), px(thumb.height)),
                },
                color,
            )
        });

        let pointer = if pane.hovered_link.is_some() {
            PointerStyle::PointingHand
        } else if pane.scrollbar_hovered || pane.scrollbar_active() {
            PointerStyle::Arrow
        } else {
            PointerStyle::IBeam
        };

        Frame {
            hitbox,
            started,
            rows_rebuilt,
            background: palette.background(),
            quads,
            lines,
            box_quads,
            box_arcs,
            over,
            cursor,
            composition,
            scrollbar,
            background_fill: palette.background(),
            line_height: metrics.height,
            pointer,
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
            for (b, color) in &frame.box_quads {
                window.paint_quad(fill(*b, *color));
            }
            for (path, color) in frame.box_arcs.drain(..) {
                window.paint_path(path, color);
            }
            for (b, color) in &frame.over {
                window.paint_quad(fill(*b, *color));
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
            if let Some(comp) = &frame.composition {
                window.paint_quad(fill(comp.background, frame.background_fill));
                let _ = comp
                    .line
                    .paint(comp.origin, frame.line_height, TextAlign::Left, None, window, cx);
                if let Some(clause) = comp.clause {
                    window.paint_quad(fill(clause, frame.cursor_color()));
                }
            }
            if let Some((b, color)) = &frame.scrollbar {
                window.paint_quad(fill(*b, *color).corner_radii(px(scrollbar::WIDTH / 2.0)));
            }
        });

        self.register_listeners(&frame.hitbox, window, cx);
        window.set_cursor_style(frame.pointer, &frame.hitbox);
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

impl Frame {
    /// The color the composition's clause underline takes: the cursor's.
    fn cursor_color(&self) -> Hsla {
        self.cursor.as_ref().map_or(self.background_fill.opacity(0.0), |c| c.color)
    }
}

impl TerminalElement {
    fn register_listeners(&self, hitbox: &Hitbox, window: &mut Window, _cx: &mut App) {
        let pane = self.pane.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.is_hovered(window) {
                pane.update(cx, |p, cx| {
                    p.mouse_down(e.button, e.position, e.modifiers, e.click_count, window, cx)
                });
                cx.stop_propagation();
            }
        });
        let pane = self.pane.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hb.is_hovered(window);
                pane.update(cx, |p, cx| {
                    p.mouse_move(e.position, e.pressed_button, e.modifiers, hovered, cx)
                });
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
