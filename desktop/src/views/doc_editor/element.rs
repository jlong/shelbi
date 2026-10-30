//! The editor's element: lays the doc out at its width, paints it (block
//! chrome, selection, text, list gutters, the caret), and routes the
//! pointer and the input method to the [`DocEditor`].

use std::cell::RefCell;
use std::panic::Location;
use std::rc::Rc;

use gpui::{
    fill, point, px, relative, size, App, AvailableSpace, Bounds, CursorStyle, DispatchPhase, Element, ElementId,
    ElementInputHandler, Entity, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, SharedString, Style, TextAlign,
    TextRun, Window,
};
use shelbi_app::markdown_edit::RowKind;

use super::layout::{self, check_box, DocLayout, Field, Gutter};
use super::DocEditor;
use crate::theme::style::{Type, UI_FAMILY};
use crate::theme::Theme;

pub struct DocElement {
    editor: Entity<DocEditor>,
}

impl DocElement {
    pub fn new(editor: Entity<DocEditor>) -> Self {
        DocElement { editor }
    }
}

impl IntoElement for DocElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

type Shared = Rc<RefCell<Option<Rc<DocLayout>>>>;

pub struct Prepainted {
    hitbox: Hitbox,
    checks: Vec<Hitbox>,
}

impl Element for DocElement {
    type RequestLayoutState = Shared;
    type PrepaintState = Prepainted;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let e = self.editor.read(cx);
        let doc = e.model.doc().clone();
        let field = e.field;
        let marked = e.model.marked();
        let theme = e.theme(cx);
        let shared: Shared = Rc::new(RefCell::new(None));
        let out = shared.clone();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let layout_id = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let width = width.unwrap_or(px(600.0));
            let l = layout::layout(&doc, field, width, marked.clone(), &theme, window, cx);
            let h = l.height;
            *out.borrow_mut() = Some(Rc::new(l));
            size(width, h)
        });
        (layout_id, shared)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        shared: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let laid = shared.borrow().clone();
        let mut checks = Vec::new();
        if let Some(l) = &laid {
            for r in &l.rows {
                if let Some((Gutter::Check(_), g)) = &r.gutter {
                    let b = check_box(*g);
                    checks.push(
                        window.insert_hitbox(
                            Bounds {
                                origin: bounds.origin + b.origin,
                                size: b.size,
                            }
                            .dilate(px(4.0)),
                            HitboxBehavior::Normal,
                        ),
                    );
                }
            }
        }
        self.editor.update(cx, |e, _| {
            e.painted = laid.map(|layout| super::Painted { bounds, layout });
        });
        Prepainted { hitbox, checks }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        shared: &mut Self::RequestLayoutState,
        pre: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(l) = shared.borrow().clone() else {
            return;
        };
        let editor = self.editor.read(cx);
        let theme = editor.theme(cx);
        let focused = editor.focus.is_focused(window);
        let doc = editor.model.doc().clone();
        let sel = editor.model.selection();
        let head = editor.model.head();
        let show_caret = focused && editor.caret_on;
        let placeholder = editor.placeholder.clone();
        let empty = editor.model.text().is_empty();
        let field = editor.field;
        let o = bounds.origin;
        let at = |b: Bounds<Pixels>| Bounds {
            origin: o + b.origin,
            size: b.size,
        };

        for b in &l.blocks {
            window.paint_quad(fill(at(*b), theme.background).corner_radii(px(6.0)));
        }
        for b in &l.bars {
            window.paint_quad(fill(at(*b), theme.divider_strong).corner_radii(px(1.0)));
        }
        for b in &l.rules {
            window.paint_quad(fill(at(*b), theme.divider));
        }
        if focused {
            for b in l.selection(&doc, sel.clone()) {
                window.paint_quad(fill(at(b), theme.text_selection));
            }
        }
        for (ri, r) in l.rows.iter().enumerate() {
            let mut y = r.top;
            for (_, line) in &r.lines {
                let _ = line.paint(o + point(r.left, y), r.line_height, TextAlign::Left, None, window, cx);
                y += r.line_height * (line.wrap_boundaries().len() + 1) as f32;
            }
            if let Some((g, gb)) = &r.gutter {
                paint_gutter(g, at(*gb), r.line_height, &theme, doc.rows[ri].kind.clone(), window, cx);
            }
        }
        if empty {
            if let Some(r) = l.rows.first() {
                let ty = if field == Field::Title { Type::Title } else { Type::Base };
                let line = shape(&placeholder, ty, theme.muted, window);
                let _ = line.paint(
                    o + point(r.left, r.top),
                    px(ty.line_height()),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        }
        if show_caret && sel.is_empty() {
            let visuals = l.visuals();
            let (row, d) = doc.locate(head);
            let c = l.caret(&visuals, row, d);
            window.paint_quad(fill(at(c), theme.text_loud));
        }

        // Pointer.
        window.set_cursor_style(CursorStyle::IBeam, &pre.hitbox);
        for c in &pre.checks {
            window.set_cursor_style(CursorStyle::PointingHand, c);
        }
        let ed = self.editor.clone();
        let hb = pre.hitbox.clone();
        let checks = pre.checks.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || e.button != MouseButton::Left || !hb.is_hovered(window) {
                return;
            }
            let on_check = checks.iter().any(|c| c.is_hovered(window));
            ed.update(cx, |ed, cx| ed.mouse_down(e, on_check, window, cx));
            window.prevent_default();
            cx.stop_propagation();
        });
        let ed = self.editor.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && e.pressed_button == Some(MouseButton::Left) {
                ed.update(cx, |ed, cx| ed.mouse_drag(e.position, window, cx));
            }
        });
        let ed = self.editor.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                ed.update(cx, |ed, cx| ed.mouse_up(cx));
            }
        });
        if focused {
            let focus = self.editor.read(cx).focus.clone();
            window.handle_input(&focus, ElementInputHandler::new(bounds, self.editor.clone()), cx);
        }
        self.editor.update(cx, |e, cx| e.reveal_caret(window, cx));
    }
}

fn shape(text: &SharedString, ty: Type, color: gpui::Hsla, window: &mut Window) -> gpui::ShapedLine {
    let run = TextRun {
        len: text.len(),
        font: gpui::Font {
            weight: ty.weight(),
            ..gpui::font(UI_FAMILY)
        },
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text.clone(), px(ty.size()), &[run], None)
}

/// A list item's marker: a bullet, its number, or the frame's checkbox.
fn paint_gutter(
    g: &Gutter,
    gb: Bounds<Pixels>,
    lh: Pixels,
    theme: &Theme,
    _kind: RowKind,
    window: &mut Window,
    cx: &mut App,
) {
    match g {
        Gutter::Bullet => {
            let d = px(5.0);
            let b = Bounds {
                origin: point(gb.origin.x + px(5.0), gb.origin.y + (lh - d) / 2.0),
                size: size(d, d),
            };
            window.paint_quad(fill(b, theme.muted).corner_radii(d / 2.0));
        }
        Gutter::Number(n) => {
            let line = shape(&n.clone().into(), Type::Base, theme.muted, window);
            let x = gb.origin.x + gb.size.width - px(6.0) - line.width;
            let _ = line.paint(
                point(x.max(gb.origin.x), gb.origin.y),
                lh,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        Gutter::Check(checked) => {
            let b = check_box(gb);
            let (fill_c, edge) = if *checked {
                (theme.check_fill, theme.check_fill)
            } else {
                (theme.textbox, theme.check_border)
            };
            window.paint_quad(
                fill(b, fill_c)
                    .corner_radii(px(3.0))
                    .border_widths(px(1.0))
                    .border_color(edge),
            );
            if *checked {
                let p = |x: f32, y: f32| point(b.origin.x + px(x), b.origin.y + px(y));
                let mut path = PathBuilder::stroke(px(1.5));
                path.move_to(p(3.5, 7.2));
                path.line_to(p(6.0, 9.6));
                path.line_to(p(10.6, 4.6));
                if let Ok(path) = path.build() {
                    window.paint_path(path, theme.text_on_color);
                }
            }
        }
    }
}
