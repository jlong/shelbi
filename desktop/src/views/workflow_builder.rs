//! The workflow editor's Builder (`dt-wf-editor-builder`): the workflow as a
//! graph on a canvas, with an inspector beside it, as the Workflow Builder
//! frames draw it (2305:10965 nothing selected, 2303:5936 a status
//! selected, 2307:10112 drawing a transition, 2307:10655 a new transition,
//! 2307:11276 a transition just added).
//!
//! **Toolbar**: Add status (the catalog's statuses the workflow lacks), Add
//! transition (draws one from the selected status), the hint, then Fit and
//! the zoom picker.
//!
//! **Canvas**: each status a node (its icon, name, `Owner · agent`, and
//! Initial on the initial status), laid out by
//! [`super::workflow_graph::layout`]; status order dashed, transitions
//! solid with their actions; the legend under them. Scroll or drag the
//! background to pan, pinch or ⌘-scroll to zoom about the pointer. Hovering
//! a node shows its four handles (a 24pt target each at any zoom, which
//! keeps the node hovered, grows its dot and shows the crosshair); dragging
//! one draws a transition, dropped on the destination
//! ([`super::workflow_builder_state`]). A press on a hovered node's handle
//! wins over the edges leaving from under it, and over panning. Clicking a node
//! or an edge selects it. With the canvas focused, Tab and the arrow keys
//! move the focus ring between nodes, Enter selects, Delete removes the
//! selected status or transition, and Esc steps back.
//!
//! **Inspector** (when something is selected): a status (its picker, the
//! category, Owner, Agent, Workspace tags, Set as initial, its outgoing
//! transitions, Remove from workflow), the transition being drawn (From,
//! the destination under the pointer, Cancel connection), a new transition
//! (From, To, its actions, Cancel and Add transition) and a transition
//! (From, To, its ordered actions, Delete transition).
//!
//! Every edit goes into the editor's draft through the same
//! [`Change`]s as the Form, so each view shows the other's edits. Nothing
//! animates: pan, zoom and Fit move the canvas at once, so Reduce Motion
//! has nothing to change here.

use gpui::{
    canvas, div, point, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Hsla, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, PinchEvent, Pixels, Point, ScrollWheelEvent,
    SharedString, Stateful, Window,
};
use shelbi_core::{Owner, StatusCategory, TransitionAction, WorkflowStatus};

use super::task_panel;
use super::workflow_builder_state::{Removal, Selection};
use super::workflow_editor::{Menu, WorkflowEditor};
use super::workflow_editor_state::{action_label, owner_label, Change, EditorState, Field, ACTIONS};
use super::workflow_graph::{
    route_to_pointer, Edge, EdgeKind, Graph, Label, LabelAt, Pt, Rect, Side, HANDLE_D, HANDLE_HIT, HANDLE_HOT_D, LABEL_LINE_H, LABEL_PAD,
    ZOOMS,
};
use super::workflows_state::category_label;
use crate::assets::{heroicon, icons, Icon};
use crate::theme::motion::fill_layer;
use crate::theme::style::{self, ButtonKind, Type, Typography};
use crate::theme::Theme;

/// The content's inset in the window, and the gap between the toolbar,
/// the canvas and the inspector.
const INSET: f32 = 24.0;
const GAP: f32 = 16.0;
/// The toolbar's Base buttons and its Small ones.
const BASE_H: f32 = 32.0;
const SMALL_H: f32 = 28.0;
/// The inspector's width, and its controls' (its 16pt padding inside a
/// 1pt edge).
const INSPECTOR_W: f32 = 320.0;
const CONTROL_W: f32 = 286.0;
/// The inspector's heading and row buttons (close, an action's stepper and
/// remove).
const ICON_BUTTON: f32 = 24.0;
/// New transition's action rows: the delete button.
const TRASH_BUTTON: f32 = 28.0;
/// Corner radius of a bend, and an arrowhead's length and half width.
const BEND: f32 = 12.0;
const ARROW_LEN: f32 = 6.0;
const ARROW_HALF: f32 = 4.0;
/// A pointer that moves less than this between press and release clicked.
const CLICK_SLOP: f32 = 3.0;
/// ⌘-scroll: the zoom's change per scrolled point.
const SCROLL_ZOOM: f32 = 0.01;

/// The canvas's hint over the graph, and the toolbar's.
const HINT: &str = "Hover over a status, then drag a handle to add a transition.";
const DRAW_HINT: &str = "Choose a destination for the new transition.";

/// An action row in New transition being dragged by its handle.
#[derive(Debug, Clone)]
pub struct NewActionDrag {
    slot: usize,
    label: SharedString,
}

struct NewActionGhost {
    label: SharedString,
}

impl Render for NewActionGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        style::popover(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .h(px(SMALL_H))
                .type_style(Type::SmallMedium)
                .text_color(theme.text_loud),
            &theme,
        )
        .child(heroicon(icons::BARS_2).text_color(theme.icon))
        .child(self.label.clone())
    }
}

/// `el` in `ty` at `zoom`. The size steps by half points, so a zoom
/// gesture keeps meeting sizes gpui has already shaped and rasterized
/// rather than a new one each frame.
fn zoomed<E: Styled>(el: E, ty: Type, zoom: f32) -> E {
    let half = |v: f32| (v * 2.0).round() / 2.0;
    el.type_style(ty).text_size(px(half(ty.size() * zoom))).line_height(px(ty.line_height() * zoom))
}

/// A status's name, or its id when the catalog has lost it.
fn status_name(state: &EditorState, id: &str) -> String {
    state.status(id).map(|s| s.name.clone()).unwrap_or_else(|| id.to_string())
}

impl WorkflowEditor {
    // -----------------------------------------------------------------------
    // Geometry

    /// The draft's graph.
    pub(super) fn graph(&self) -> Option<Graph> {
        self.state.as_ref().map(|s| Graph::of(s.workflow()))
    }

    /// Window point `p` in graph coordinates.
    fn graph_point(&self, p: Point<Pixels>) -> Option<Pt> {
        let frame = self.frame_anchor.get()?;
        let z = self.builder.viewport.zoom;
        Some(Pt::new(f32::from(p.x - frame.origin.x) / z, f32::from(p.y - frame.origin.y) / z))
    }

    /// Window point `p` in the canvas.
    fn canvas_point(&self, p: Point<Pixels>) -> Option<Pt> {
        let c = self.canvas_anchor.get()?;
        Some(Pt::new(f32::from(p.x - c.origin.x), f32::from(p.y - c.origin.y)))
    }

    fn canvas_size(&self) -> Option<(f32, f32)> {
        let c = self.canvas_anchor.get()?;
        Some((f32::from(c.size.width), f32::from(c.size.height)))
    }

    // -----------------------------------------------------------------------
    // Pointer

    fn canvas_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.canvas_focus, cx);
        self.menu = None;
        let (Some(p), Some(at), Some(g)) = (self.graph_point(event.position), self.canvas_point(event.position), self.graph()) else {
            return;
        };
        self.press(p, at, &g);
        cx.notify();
    }

    /// A press at graph point `p` (canvas point `at`): a hovered node's
    /// handle starts a drawing; a node selects it (or, drawing, is the
    /// destination); an edge selects its transition; the background pans.
    pub(super) fn press(&mut self, p: Pt, at: Pt, g: &Graph) {
        let z = self.builder.viewport.zoom;
        let drawing = self.builder.drawing().is_some();
        // The hover as of this point (a scroll may have moved the graph
        // under a still pointer).
        self.builder.hover_to(p, g, z);
        if let Some(hovered) = self.builder.hover.clone() {
            if let Some(side) = g.handle_at(&hovered, p, z) {
                self.keep_status_scroll();
                self.builder.begin_draw(&hovered, Some(side), Some(p));
                return;
            }
        }
        if let Some(node) = g.node_at(p) {
            let id = node.id.clone();
            if drawing {
                self.builder.pick(&id);
            } else {
                self.builder.select_status(&id);
            }
            return;
        }
        if !drawing {
            if let Some(index) = g.transition_at(p, z) {
                self.builder.select_transition(index);
                return;
            }
        }
        self.builder.panning = Some(at);
        self.pressed_at = Some(at);
    }

    fn canvas_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let (Some(p), Some(at), Some(g)) = (self.graph_point(event.position), self.canvas_point(event.position), self.graph()) else {
            return;
        };
        let held = event.pressed_button == Some(MouseButton::Left);
        if self.pointer_moved(p, at, held, &g) {
            cx.notify();
        }
    }

    /// The pointer moved to `p` (`at` in the canvas), the button `held` or
    /// not. Returns whether anything shown changed.
    pub(super) fn pointer_moved(&mut self, p: Pt, at: Pt, held: bool, g: &Graph) -> bool {
        if let Some(last) = self.builder.panning {
            if !held {
                self.builder.panning = None;
            } else if let Some(canvas) = self.canvas_size() {
                self.builder.viewport.pan_by(at.x - last.x, at.y - last.y, canvas, (g.w, g.h));
                self.builder.panning = Some(at);
                return true;
            }
        }
        if self.builder.drawing().is_some_and(|d| d.pointer.is_some()) {
            if held {
                self.builder.draw_to(p, g);
            } else {
                // The release happened outside the canvas.
                self.builder.release();
            }
            return true;
        }
        self.builder.hover_to(p, g, self.builder.viewport.zoom)
    }

    fn canvas_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let at = self.canvas_point(event.position);
        self.released(at);
        cx.notify();
    }

    /// The button came up at canvas point `at` (`None` outside the canvas).
    pub(super) fn released(&mut self, at: Option<Pt>) {
        self.builder.release();
        self.builder.panning = None;
        // A click on the background (no drag) deselects.
        if let (Some(down), Some(at)) = (self.pressed_at.take(), at) {
            let still = (at.x - down.x).abs() < CLICK_SLOP && (at.y - down.y).abs() < CLICK_SLOP;
            if still && self.builder.drawing().is_none() {
                self.builder.deselect();
            }
        }
    }

    fn canvas_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(canvas), Some(at), Some(g)) = (self.canvas_size(), self.canvas_point(event.position), self.graph()) else {
            return;
        };
        let delta = event.delta.pixel_delta(window.line_height());
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let v = &mut self.builder.viewport;
        if event.modifiers.platform {
            let zoom = v.zoom * (1.0 + dy * SCROLL_ZOOM);
            v.zoom_at(zoom, at, canvas, (g.w, g.h));
        } else {
            v.pan_by(dx, dy, canvas, (g.w, g.h));
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn canvas_pinch(&mut self, event: &PinchEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let (Some(canvas), Some(at), Some(g)) = (self.canvas_size(), self.canvas_point(event.position), self.graph()) else {
            return;
        };
        let v = &mut self.builder.viewport;
        let zoom = v.zoom * (1.0 + event.delta);
        v.zoom_at(zoom, at, canvas, (g.w, g.h));
        cx.notify();
    }

    /// Fit: the whole graph in the canvas.
    pub(super) fn fit(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        if let (Some(canvas), Some(g)) = (self.canvas_size(), self.graph()) {
            self.builder.viewport.fit(canvas, (g.w, g.h));
        }
        cx.notify();
    }

    pub(super) fn zoom_to(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.menu = None;
        if let (Some(canvas), Some(g)) = (self.canvas_size(), self.graph()) {
            self.builder.viewport.zoom_to(zoom, canvas, (g.w, g.h));
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Keyboard

    fn canvas_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.modifiers.platform || k.modifiers.control || k.modifiers.alt {
            return;
        }
        if self.builder_key(&k.key, k.modifiers.shift, window, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// A key on the focused canvas. Returns whether it was the builder's.
    pub(super) fn builder_key(&mut self, key: &str, shift: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(g) = self.graph() else {
            return false;
        };
        let workflow = self.state.as_ref().expect("a graph has a draft").workflow().clone();
        match key {
            "tab" => self.builder.tab(&workflow, !shift),
            "up" => self.builder.arrow(&g, Side::Top),
            "down" => self.builder.arrow(&g, Side::Bottom),
            "left" => self.builder.arrow(&g, Side::Left),
            "right" => self.builder.arrow(&g, Side::Right),
            "enter" => self.builder.enter(),
            "backspace" | "delete" => {
                match self.builder.removal() {
                    Some(Removal::Status(id)) => self.remove_status(&id, window, cx),
                    Some(Removal::Transition(index)) => self.remove_transition(index, window, cx),
                    None => {}
                }
            }
            "escape" => self.back(cx),
            _ => return false,
        }
        true
    }

    // -----------------------------------------------------------------------
    // Edits

    /// Remove status `id` from the workflow (refused for the only one, its
    /// message in the inspector).
    fn remove_status(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let only = self.state.as_ref().is_some_and(|s| s.workflow().statuses.len() <= 1);
        if only {
            return;
        }
        self.change(Change::RemoveStatus(id.to_string()), window, cx);
        self.reconcile_builder();
    }

    fn remove_transition(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.change(Change::RemoveTransition(index), window, cx);
        self.builder.deselect();
    }

    /// Add catalog status `id` and select it.
    fn add_status(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.change(Change::AddStatus(id.to_string()), window, cx);
        if self.state.as_ref().is_some_and(|s| s.status(id).is_some()) {
            self.builder.select_status(id);
        }
    }

    /// Add transition: draw one from the selected status (else the
    /// keyboard's, else the first).
    pub(super) fn start_transition(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        let from = self
            .builder
            .selected_status()
            .map(str::to_string)
            .or_else(|| self.builder.focus.clone())
            .or_else(|| self.state.as_ref().and_then(|s| s.workflow().statuses.first().map(|s| s.id.clone())));
        if let Some(from) = from {
            self.keep_status_scroll();
            self.builder.begin_draw(&from, None, None);
        }
        cx.notify();
    }

    /// An Outgoing transitions row: open its transition, Back returning to
    /// the status.
    pub(super) fn open_transition(&mut self, index: usize, cx: &mut Context<Self>) {
        self.menu = None;
        self.keep_status_scroll();
        self.builder.open_transition(index);
        cx.notify();
    }

    /// Leaving the inspector's panel for a transition panel: a Status
    /// panel's scroll offset is kept for Back, and the new panel starts at
    /// the top.
    fn keep_status_scroll(&mut self) {
        self.back_scroll = self.builder.selected_status().map(|_| self.inspector_scroll.offset());
        if self.back_scroll.is_some() {
            self.inspector_scroll.set_offset(Point::default());
        }
    }

    /// Esc, or a transition panel's ←: back to the status it came from
    /// (dropping a pending drawing or new transition, and restoring the
    /// Status panel's scroll), else deselect. `false` when nothing was
    /// selected.
    pub(super) fn step_back(&mut self) -> bool {
        if self.builder.drawing().is_some() || self.builder.new_transition().is_some() {
            if let Some(state) = self.state.as_mut() {
                state.clear_error_at(&Field::Transitions);
            }
        }
        let back = self.builder.back.is_some();
        if !self.builder.escape() {
            return false;
        }
        if let Some(offset) = self.back_scroll.take().filter(|_| back) {
            self.inspector_scroll.set_offset(offset);
        }
        true
    }

    /// New transition's Add transition: into the draft, then selected.
    pub(super) fn add_new_transition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.builder.new_transition().map(|n| n.transition()) else {
            return;
        };
        self.change(Change::InsertTransition(t), window, cx);
        if let Some(state) = self.state.as_ref() {
            if state.error_at(&Field::Transitions).is_none() {
                self.builder.added(state.transitions().len() - 1);
            }
        }
        cx.notify();
    }

    fn cancel_new(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        self.step_back();
        cx.notify();
    }

    fn edit_new(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Vec<TransitionAction>)) {
        self.menu = None;
        if let Some(n) = self.builder.new_transition_mut() {
            edit(&mut n.actions);
        }
        cx.notify();
    }

    /// Drop selection and hover on what the draft no longer has.
    pub(super) fn reconcile_builder(&mut self) {
        if let Some(state) = self.state.as_ref() {
            self.builder.reconcile(state.workflow());
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering

/// The Button at Base: 32pt, Base medium, an optional 20pt icon in a 14pt
/// slot.
fn base_button(id: impl Into<SharedString>, icon: Option<Icon>, label: impl Into<SharedString>, kind: ButtonKind, theme: &Theme) -> Stateful<Div> {
    style::button_base(id, kind, false, theme)
        .h(px(BASE_H))
        .gap(px(6.0))
        .type_style(Type::BaseMedium)
        .whitespace_nowrap()
        .children(icon.map(|i| style::icon_slot(i, 14.0, theme.icon)))
        .child(label.into())
}

/// The Button at Small: 28pt, Small medium, an optional icon in a 12pt
/// slot.
fn small_button(id: impl Into<SharedString>, icon: Option<Icon>, label: impl Into<SharedString>, kind: ButtonKind, theme: &Theme) -> Stateful<Div> {
    style::button_base(id, kind, false, theme)
        .h(px(SMALL_H))
        .gap(px(4.0))
        .whitespace_nowrap()
        .children(icon.map(|i| style::icon_slot(i, 12.0, theme.icon)))
        .child(label.into())
}

/// A square icon button on the overlay fill (the inspector's close, an
/// action's remove and stepper): `size` across, `icon` centered.
fn square_button(id: impl Into<SharedString>, icon: Icon, size: f32, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(6.0))
        .cursor_pointer();
    fill_layer(d, id, px(6.0), [theme.background, theme.background_hover, theme.background_active])
        .child(heroicon(icon).text_color(theme.icon))
}

/// A picker's button at Base or Small: an optional status icon, the
/// choice, a chevron; it fills its row.
fn picker(id: impl Into<SharedString>, label: impl Into<SharedString>, icon: Option<StatusCategory>, small: bool, theme: &Theme) -> Stateful<Div> {
    let b = style::button_base(id, ButtonKind::Secondary, false, theme).w_full();
    let (b, slot) = if small {
        (b.h(px(SMALL_H)).gap(px(4.0)), 12.0)
    } else {
        (b.type_style(Type::BaseMedium).gap(px(6.0)), 14.0)
    };
    b.children(icon.map(|c| {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .w(px(14.0))
            .child(style::status_icon_large(c, theme))
    }))
    .child(div().flex_1().min_w_0().truncate().child(label.into()))
    .child(
        div()
            .flex()
            .flex_none()
            .items_center()
            .w(px(slot))
            .child(heroicon(icons::CHEVRON_DOWN).text_color(theme.icon)),
    )
}

/// A labeled control in the inspector: Base label, 6pt, the control, the
/// helper under it.
fn labeled(label: &'static str, control: impl IntoElement, help: Option<&'static str>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .w_full()
        .child(div().type_style(Type::Base).text_color(theme.text_loud).child(label))
        .child(control)
        .children(help.map(|h| div().type_style(Type::XSmall).text_color(theme.form_hint).child(h)))
}

fn rule(theme: &Theme) -> Div {
    div().flex_none().h(px(1.0)).w_full().bg(theme.divider)
}

fn error_line(selector: &'static str, text: &str, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.into())
        .type_style(Type::XSmall)
        .text_color(theme.error)
        .child(text.to_string())
}

/// A line drawn on the canvas.
struct Stroke {
    points: Vec<Pt>,
    color: Hsla,
    width: f32,
    /// The dash and the gap, at 100%.
    dash: Option<(f32, f32)>,
}

/// Paint `s` with its frame's origin at `origin`, at `zoom`: rounded bends
/// and an open arrowhead at its end.
fn paint_stroke(s: &Stroke, origin: Point<Pixels>, zoom: f32, window: &mut Window) {
    if s.points.len() < 2 {
        return;
    }
    let at = |p: Pt| point(origin.x + px(p.x * zoom), origin.y + px(p.y * zoom));
    let dashed = |b: PathBuilder| match s.dash {
        Some((on, off)) => b.dash_array(&[px(on * zoom.max(0.5)), px(off * zoom.max(0.5))]),
        None => b,
    };
    let width = px((s.width * zoom).max(1.0));
    let mut b = dashed(PathBuilder::stroke(width));
    let pts = &s.points;
    b.move_to(at(pts[0]));
    for i in 1..pts.len() - 1 {
        let (a, c, n) = (pts[i - 1], pts[i], pts[i + 1]);
        let len = |u: Pt, v: Pt| ((u.x - v.x).powi(2) + (u.y - v.y).powi(2)).sqrt();
        let r = BEND.min(len(a, c) / 2.0).min(len(c, n) / 2.0);
        let toward = |from: Pt, to: Pt, d: f32| {
            let l = len(from, to).max(0.001);
            Pt::new(from.x + (to.x - from.x) / l * d, from.y + (to.y - from.y) / l * d)
        };
        b.line_to(at(toward(c, a, r)));
        b.curve_to(at(toward(c, n, r)), at(c));
    }
    let (prev, tip) = (pts[pts.len() - 2], pts[pts.len() - 1]);
    b.line_to(at(tip));
    if let Ok(path) = b.build() {
        window.paint_path(path, s.color);
    }
    // The arrowhead: an open chevron at the tip.
    let l = ((tip.x - prev.x).powi(2) + (tip.y - prev.y).powi(2)).sqrt().max(0.001);
    let (dx, dy) = ((tip.x - prev.x) / l, (tip.y - prev.y) / l);
    let back = Pt::new(tip.x - dx * ARROW_LEN, tip.y - dy * ARROW_LEN);
    let mut head = PathBuilder::stroke(width);
    head.move_to(at(Pt::new(back.x - dy * ARROW_HALF, back.y + dx * ARROW_HALF)));
    head.line_to(at(tip));
    head.line_to(at(Pt::new(back.x + dy * ARROW_HALF, back.y - dx * ARROW_HALF)));
    if let Ok(path) = head.build() {
        window.paint_path(path, s.color);
    }
}

impl WorkflowEditor {
    /// The Builder: the toolbar over the canvas and, with something
    /// selected, the inspector.
    pub(super) fn render_builder(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        self.reconcile_builder();
        let graph = self.graph().expect("the builder draws a loaded workflow");
        let inspector = self.builder.selection.is_some().then(|| self.render_inspector(theme, window, cx).into_any_element());
        div()
            .debug_selector(|| "workflow-builder".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(GAP))
            .p(px(INSET))
            .child(self.render_toolbar(theme, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .gap(px(GAP))
                    .child(self.render_canvas(&graph, theme, window, cx))
                    .children(inspector),
            )
    }

    /// Add status, Add transition, the hint; Fit and the zoom picker.
    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let addable = state.addable_statuses();
        let add_status = self.dropdown(
            Menu::BuilderAddStatus,
            "builder-add-status-menu",
            base_button("builder-add-status", Some(icons::PLUS), "Add status", ButtonKind::Secondary, theme),
            |cx| {
                let theme = *cx.global::<Theme>();
                if addable.is_empty() {
                    return vec![div()
                        .debug_selector(|| "builder-add-status-none".into())
                        .px_2()
                        .py(px(6.0))
                        .type_style(Type::Small)
                        .text_color(theme.form_hint)
                        .child("Every project status is in this workflow. Manage project statuses in Form adds more.")
                        .into_any_element()];
                }
                addable
                    .iter()
                    .map(|s| {
                        let id = s.id.clone();
                        style::menu_row(format!("builder-add-status-{id}"), false, &theme)
                            .child(status_icon(s.category, &theme))
                            .child(s.name.clone())
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.add_status(&id, window, cx)))
                            .into_any_element()
                    })
                    .collect()
            },
            240.0,
            cx,
        );
        let add_transition = base_button("builder-add-transition", Some(icons::PLUS), "Add transition", ButtonKind::Secondary, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_transition(cx)));
        let fit = small_button("builder-fit", None, "Fit", ButtonKind::Secondary, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.fit(cx)));
        let current = self.builder.viewport.zoom;
        let zoom = self.dropdown(
            Menu::Zoom,
            "builder-zoom-menu",
            style::button_base("builder-zoom", ButtonKind::Secondary, false, theme)
                .h(px(SMALL_H))
                .gap(px(4.0))
                .child(self.builder.viewport.label())
                .child(div().flex().flex_none().items_center().w(px(12.0)).child(heroicon(icons::CHEVRON_DOWN).text_color(theme.icon))),
            move |cx| {
                let theme = *cx.global::<Theme>();
                ZOOMS
                    .into_iter()
                    .map(|z| {
                        let label = format!("{}%", (z * 100.0).round() as i32);
                        style::menu_row(format!("builder-zoom-{}", (z * 100.0).round() as i32), (z - current).abs() < 0.001, &theme)
                            .child(label)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.zoom_to(z, cx)))
                            .into_any_element()
                    })
                    .collect()
            },
            120.0,
            cx,
        );
        div()
            .debug_selector(|| "builder-toolbar".into())
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .h(px(BASE_H))
            .child(div().flex().flex_row().flex_none().gap(px(8.0)).child(add_status).child(add_transition))
            .child(div().flex_none().type_style(Type::Small).text_color(theme.form_hint).child(HINT))
            .child(div().flex_1())
            .child(div().flex().flex_row().flex_none().items_center().gap(px(8.0)).child(fit).child(zoom))
    }

    /// The canvas: the graph frame, centered and panned, zoomed.
    fn render_canvas(&self, graph: &Graph, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let v = self.builder.viewport;
        let z = v.zoom;
        let frame = div()
            .debug_selector(|| "builder-graph".into())
            .relative()
            .flex_none()
            .left(px(v.pan.x))
            .top(px(v.pan.y))
            .w(px(graph.w * z))
            .h(px(graph.h * z))
            .child(style::anchor_probe(&self.frame_anchor))
            .children(self.render_graph(graph, theme, window, cx));
        div()
            .id("builder-canvas")
            .debug_selector(|| "builder-canvas".into())
            .key_context("WorkflowBuilder")
            .track_focus(&self.canvas_focus)
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.divider)
            .bg(theme.canvas)
            .cursor_default()
            .child(style::anchor_probe(&self.canvas_anchor))
            .child(frame)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::canvas_down))
            .on_mouse_move(cx.listener(Self::canvas_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::canvas_up))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.released(None);
                    cx.notify();
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && this.builder.unhover() {
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(Self::canvas_scroll))
            .on_pinch(cx.listener(Self::canvas_pinch))
            .on_key_down(cx.listener(Self::canvas_key))
    }

    /// The graph frame's content: the edges, their labels, the nodes and
    /// their handles, the drawing, the hint and the legend.
    fn render_graph(&self, graph: &Graph, theme: &Theme, window: &Window, cx: &App) -> Vec<AnyElement> {
        let state = self.state.as_ref().expect("loaded");
        let workflow = state.workflow();
        let z = self.builder.viewport.zoom;
        let selected = self.builder.selected_transition();
        let accent = theme.focus_ring;
        let line = graph_line(theme);
        let mut strokes: Vec<Stroke> = Vec::new();
        let mut accents: Vec<Stroke> = Vec::new();
        let mut labels: Vec<AnyElement> = Vec::new();
        for e in &graph.edges {
            let on = matches!(e.kind, EdgeKind::Transition(i) if Some(i) == selected);
            let stroke = Stroke {
                points: e.points.clone(),
                color: if on { accent } else { line },
                width: if on { 2.0 } else { 1.5 },
                dash: (e.kind == EdgeKind::Order).then_some((5.5, 2.5)),
            };
            if on { accents.push(stroke) } else { strokes.push(stroke) }
            if let Some(label) = &e.label {
                labels.push(self.edge_label(e, label, if on { theme.link } else { theme.form_hint }, theme));
            }
        }
        // The line being drawn, or the new transition's.
        let mut drawn_label = None;
        if let Some(d) = self.builder.drawing() {
            let line = match (&d.over, d.pointer, d.handle) {
                (Some(to), _, _) => graph.route_between(&d.from, to),
                (None, Some(p), Some(side)) => graph.rect(&d.from).map(|r| route_to_pointer(r.side(side), side, p)),
                _ => None,
            };
            if let Some(points) = line {
                let text = d.over.as_deref().map(|to| format!("Connect to {}", status_name(state, to)));
                drawn_label = text.and_then(|t| Label::line(&points, &t));
                accents.push(Stroke { points, color: accent, width: 2.0, dash: Some((6.0, 2.0)) });
            }
        } else if let Some(n) = self.builder.new_transition() {
            if let Some(points) = graph.route_between(&n.from, &n.to) {
                drawn_label = Label::line(&points, "New transition");
                accents.push(Stroke { points, color: accent, width: 2.0, dash: Some((6.0, 2.0)) });
            }
        }
        if let Some(label) = drawn_label {
            labels.push(self.label_element("builder-drawing-label".into(), &label, theme.link, theme));
        }
        let edges = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                for s in strokes.iter().chain(&accents) {
                    paint_stroke(s, bounds.origin, z, window);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let initial = state.initial_status().map(|s| s.id.clone());
        let focused = self.canvas_focus.is_focused(window);
        let ringed = self.builder.ringed(workflow, focused);
        let with_handles = self.builder.with_handles();
        let hot = self.builder.hot_handle();
        let nodes = graph.nodes.iter().filter_map(|n| {
            let status = workflow.status(&n.id)?;
            Some(self.node_element(n.rect, status, initial.as_deref() == Some(&n.id), ringed.contains(&n.id), theme))
        });
        let handles: Vec<AnyElement> = graph
            .nodes
            .iter()
            .filter(|n| with_handles.contains(&n.id))
            .flat_map(|n| Side::ALL.map(|side| handle_element(&n.id, n.rect, side, hot == Some((n.id.as_str(), side)), z, theme)))
            .collect();
        let hint = if self.builder.drawing().is_some() {
            Some(DRAW_HINT)
        } else if self.builder.selection.is_none() {
            Some(HINT)
        } else {
            None
        };
        let hint = hint.map(|h| {
            zoomed(div(), Type::XSmall, z)
                .debug_selector(|| "builder-canvas-hint".into())
                .absolute()
                .left(px(24.0 * z))
                .top(px(20.0 * z))
                .whitespace_nowrap()
                .text_color(theme.form_hint)
                .child(h)
                .into_any_element()
        });
        let _ = cx;
        let mut out = vec![edges.into_any_element()];
        out.extend(labels);
        out.extend(nodes);
        out.extend(handles);
        out.extend(hint);
        out.push(self.legend(graph, theme));
        out
    }

    /// A transition's label: in `color` on the canvas's fill, over its line.
    fn edge_label(&self, e: &Edge, label: &Label, color: Hsla, theme: &Theme) -> AnyElement {
        let selector: SharedString = match e.kind {
            EdgeKind::Transition(i) => format!("builder-edge-label-{i}").into(),
            EdgeKind::Order => "builder-order-label".into(),
        };
        self.label_element(selector, label, color, theme)
    }

    fn label_element(&self, selector: SharedString, label: &Label, color: Hsla, theme: &Theme) -> AnyElement {
        let z = self.builder.viewport.zoom;
        let h = label.lines.len() as f32 * LABEL_LINE_H + 2.0 * LABEL_PAD;
        let chip = zoomed(div(), Type::XSmall, z)
            .debug_selector(move || selector.to_string())
            .flex()
            .flex_col()
            .flex_none()
            .p(px(LABEL_PAD * z))
            .bg(theme.canvas)
            .text_color(color)
            .whitespace_nowrap()
            .children(label.lines.iter().map(|l| div().child(l.clone())));
        // A wide, empty holder centers the chip on its line.
        const HOLD: f32 = 400.0;
        match label.at {
            LabelAt::Above { x, bottom } => div()
                .absolute()
                .left(px((x - HOLD / 2.0) * z))
                .top(px((bottom - h) * z))
                .w(px(HOLD * z))
                .flex()
                .justify_center()
                .child(chip)
                .into_any_element(),
            LabelAt::Beside { left, y } => div()
                .absolute()
                .left(px(left * z))
                .top(px((y - h / 2.0) * z))
                .child(chip)
                .into_any_element(),
        }
    }

    /// A status node (the Workflow Graph Node): its icon and name, `Owner ·
    /// agent`, and Initial; the focus ring when `ringed`.
    fn node_element(&self, r: Rect, s: &WorkflowStatus, initial: bool, ringed: bool, theme: &Theme) -> AnyElement {
        let z = self.builder.viewport.zoom;
        let id = s.id.clone();
        let border = if ringed { 2.0 } else { 1.0 };
        let assignment = format!("{} · {}", owner_label(s.owner), s.agent.as_deref().unwrap_or("None"));
        div()
            .debug_selector(move || format!("builder-node-{id}"))
            .absolute()
            .left(px(r.x * z))
            .top(px(r.y * z))
            .w(px(r.w * z))
            .h(px(r.h * z))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(8.0 * z))
            // Figma draws the edge inside the padding.
            .px(px((16.0 * z - border).max(0.0)))
            .rounded(px(6.0 * z))
            .border(px(border))
            .border_color(if ringed { theme.focus_ring } else { theme.divider })
            .bg(theme.search)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0 * z))
                    .overflow_hidden()
                    .child(style::status_icon_large(s.category, theme).size(px(20.0 * z)))
                    .child(zoomed(div(), Type::BaseSemibold, z).flex_1().min_w_0().truncate().text_color(theme.text_loud).child(s.name.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0 * z))
                    .overflow_hidden()
                    .child(zoomed(div(), Type::XSmall, z).flex_none().whitespace_nowrap().text_color(theme.form_hint).child(assignment))
                    .when(initial, |d| {
                        d.child(
                            zoomed(div(), Type::XSmallMedium, z)
                                .debug_selector(|| "builder-initial-badge".into())
                                .flex_none()
                                .text_color(theme.link)
                                .child("Initial"),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The legend under the graph: Status order dashed, Transition actions
    /// solid.
    fn legend(&self, graph: &Graph, theme: &Theme) -> AnyElement {
        let z = self.builder.viewport.zoom;
        let y = graph.h - 18.0;
        let color = graph_line(theme);
        let swatch = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let o = bounds.origin;
                // The frame's dashes: 2, 4 and 2 points with round caps.
                let mut lines: Vec<Stroke> = [(-0.75, 2.75), (5.25, 10.75), (13.25, 16.75)]
                    .into_iter()
                    .map(|(a, b)| Stroke { points: vec![Pt::new(a, 8.0), Pt::new(b, 8.0)], color, width: 1.5, dash: None })
                    .collect();
                lines.push(Stroke { points: vec![Pt::new(131.0, 8.0), Pt::new(147.0, 8.0)], color, width: 1.5, dash: None });
                for s in &lines {
                    paint_line(s, o, z, window);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let text = |left: f32, t: &'static str| {
            zoomed(div(), Type::XSmall, z).absolute().left(px(left * z)).top_0().whitespace_nowrap().text_color(theme.form_hint).child(t)
        };
        div()
            .debug_selector(|| "builder-legend".into())
            .absolute()
            .left(px(24.0 * z))
            .top(px((y - 8.0) * z))
            .w(px(300.0 * z))
            .h(px(16.0 * z))
            .child(swatch)
            .child(text(24.0, "Status order"))
            .child(text(155.0, "Transition actions"))
            .into_any_element()
    }
}

/// A plain line (the legend's swatches): no bends, no arrowhead.
fn paint_line(s: &Stroke, origin: Point<Pixels>, zoom: f32, window: &mut Window) {
    let at = |p: Pt| point(origin.x + px(p.x * zoom), origin.y + px(p.y * zoom));
    let mut b = PathBuilder::stroke(px((s.width * zoom).max(1.0)));
    if let Some((on, off)) = s.dash {
        b = b.dash_array(&[px(on * zoom), px(off * zoom)]);
    }
    b.move_to(at(s.points[0]));
    for p in &s.points[1..] {
        b.line_to(at(*p));
    }
    if let Ok(path) = b.build() {
        window.paint_path(path, s.color);
    }
}

/// The graph's lines: the frames' `theme.gray` at 60% (the `icon` role is
/// it at 90%).
fn graph_line(theme: &Theme) -> Hsla {
    theme.icon.opacity(0.6 / 0.9)
}

/// A node's connection handle on `side`: a 10pt circle on its edge, in its
/// 24pt target (the hit area, [`HANDLE_HIT`] around it), which shows the
/// crosshair. `hot` (under the pointer, or held): 14pt, in the accent.
fn handle_element(id: &str, r: Rect, side: Side, hot: bool, zoom: f32, theme: &Theme) -> AnyElement {
    let c = r.side(side);
    let target = 2.0 * HANDLE_HIT;
    let name = side.name();
    let selector = format!("builder-handle-{id}-{name}");
    // The line's color edges the same gray at 35%; hot, the drawing's
    // accent.
    let (d, border, fill) = if hot {
        (HANDLE_HOT_D, theme.focus_ring, theme.focus_ring.opacity(0.35))
    } else {
        (HANDLE_D, graph_line(theme), theme.icon.opacity(0.35 / 0.9))
    };
    let mut dot = div().size(px(d)).rounded_full().border_1().border_color(border).bg(fill);
    if hot {
        let selector = format!("builder-handle-hot-{id}-{name}");
        dot = dot.debug_selector(move || selector.clone());
    }
    div()
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(c.x * zoom - target / 2.0))
        .top(px(c.y * zoom - target / 2.0))
        .size(px(target))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_crosshair()
        .child(dot)
        .into_any_element()
}

/// A status's icon in a 20pt frame.
fn status_icon(category: StatusCategory, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(20.0))
        .child(style::status_icon_large(category, theme))
}

// ---------------------------------------------------------------------------
// The inspector

impl WorkflowEditor {
    fn render_inspector(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        // The status inspector centers its heading on the close button;
        // the transition inspectors set it at the top, unless a back button
        // (taller than the close) leads it.
        let centered = matches!(self.builder.selection, Some(Selection::Status(_)));
        let (title, body): (&'static str, Vec<AnyElement>) = match self.builder.selection.clone() {
            Some(Selection::Status(id)) => ("Status", self.status_panel(&id, theme, window, cx)),
            Some(Selection::Transition { index, added }) => ("Transition", self.transition_panel(index, added, theme, cx)),
            Some(Selection::Draw(_)) => ("Draw transition", self.draw_panel(theme, cx)),
            Some(Selection::New(_)) => ("New transition", self.new_panel(theme, cx)),
            None => ("", Vec::new()),
        };
        let close = square_button("builder-inspector-close", icons::X_MARK_BUTTON, ICON_BUTTON, theme)
            .tooltip(style::tooltip("Close"))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.menu = None;
                if let Some(state) = this.state.as_mut() {
                    state.clear_error_at(&Field::Transitions);
                }
                this.builder.deselect();
                cx.notify();
            }));
        // A transition panel opened from a status goes back to it.
        let back = self.builder.back.as_deref().zip(self.state.as_ref()).map(|(id, state)| {
            task_panel::back_button("builder-inspector-back", false, theme)
                .tooltip(style::tooltip(format!("Back to {}", status_name(state, id))))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.menu = None;
                    this.step_back();
                    cx.notify();
                }))
        });
        let centered = centered || back.is_some();
        div()
            .id("builder-inspector")
            .debug_selector(|| "builder-inspector".into())
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(GAP))
            .w(px(INSPECTOR_W))
            .h_full()
            .p(px(GAP))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.divider)
            .bg(theme.search)
            .overflow_y_scroll()
            .track_scroll(&self.inspector_scroll)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .when(centered, |d| d.items_center())
                    .when(!centered, |d| d.items_start())
                    .gap(px(8.0))
                    .w_full()
                    .children(back)
                    .child(
                        div()
                            .debug_selector(|| "builder-inspector-title".into())
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud)
                            .child(title),
                    )
                    .child(div().flex_1())
                    .child(close),
            )
            .children(body)
    }

    /// A picker over the workflow's statuses in the inspector.
    fn status_picker(
        &self,
        menu: Menu,
        id: &'static str,
        current: &str,
        pick: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + Clone + 'static,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let state = self.state.as_ref().expect("loaded");
        let statuses = state.workflow().statuses.clone();
        let status = state.status(current).cloned();
        let current = current.to_string();
        self.dropdown(
            menu,
            "builder-status-menu",
            picker(
                id,
                status.as_ref().map(|s| s.name.clone()).unwrap_or_else(|| current.clone()),
                status.as_ref().map(|s| s.category),
                false,
                theme,
            ),
            move |cx| {
                let theme = *cx.global::<Theme>();
                statuses
                    .iter()
                    .map(|s| {
                        let sid = s.id.clone();
                        let pick = pick.clone();
                        style::menu_row(format!("{id}-{sid}"), s.id == current, &theme)
                            .child(status_icon(s.category, &theme))
                            .child(s.name.clone())
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.menu = None;
                                pick(this, sid.clone(), window, cx);
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect()
            },
            CONTROL_W,
            cx,
        )
        .w_full()
    }

    /// A status: its picker, category, Owner, Agent, Workspace tags,
    /// Initial status, outgoing transitions, Remove from workflow.
    fn status_panel(&self, id: &str, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.state.as_ref().expect("loaded");
        let Some(s) = state.status(id).cloned() else {
            return Vec::new();
        };
        let picker_el = self.status_picker(
            Menu::InspectStatus,
            "builder-status",
            id,
            |this, sid, _, _| this.builder.select_status(&sid),
            theme,
            cx,
        );
        let category = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(div().type_style(Type::Small).text_color(theme.form_hint).child("Category"))
            .child(
                div()
                    .debug_selector(|| "builder-status-category".into())
                    .type_style(Type::BaseMedium)
                    .text_color(theme.form_text)
                    .child(category_label(s.category)),
            );
        let owner = {
            let current = s.owner;
            let row = s.id.clone();
            self.dropdown(
                Menu::Owner(s.id.clone()),
                "builder-owner-menu",
                picker(format!("builder-owner-{}", s.id), owner_label(current), None, false, theme),
                move |cx| {
                    let theme = *cx.global::<Theme>();
                    [Owner::User, Owner::Agent]
                        .into_iter()
                        .map(|o| {
                            let row = row.clone();
                            style::menu_row(format!("builder-owner-{row}-{}", owner_label(o).to_lowercase()), o == current, &theme)
                                .child(owner_label(o))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.change(Change::Owner { id: row.clone(), owner: o }, window, cx)
                                }))
                                .into_any_element()
                        })
                        .collect()
                },
                CONTROL_W,
                cx,
            )
            .w_full()
        };
        let agent = {
            let current = s.agent.clone();
            let row = s.id.clone();
            let agents = state.agents();
            self.dropdown(
                Menu::Agent(s.id.clone()),
                "builder-agent-menu",
                picker(format!("builder-agent-{}", s.id), current.clone().unwrap_or_else(|| "None".into()), None, false, theme),
                move |cx| {
                    let theme = *cx.global::<Theme>();
                    agents
                        .into_iter()
                        .map(Some)
                        .chain([None])
                        .map(|a| {
                            let row = row.clone();
                            let key = a.clone().unwrap_or_else(|| "none".into());
                            style::menu_row(format!("builder-agent-{row}-{key}"), a == current, &theme)
                                .child(a.clone().unwrap_or_else(|| "None".into()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.change(Change::Agent { id: row.clone(), agent: a.clone() }, window, cx)
                                }))
                                .into_any_element()
                        })
                        .collect()
                },
                CONTROL_W,
                cx,
            )
            .w_full()
        };
        let tags = self.tags.get(&s.id).map(|(input, _)| {
            self.text_field(input, format!("builder-tags-{}", s.id), Some("Any workspace".into()), false, window, cx)
        });
        let is_initial = state.initial_status().is_some_and(|i| i.id == s.id);
        let set_initial = if is_initial {
            style::disabled_button("builder-set-initial", "Set as initial", ButtonKind::Secondary, theme)
                .h(px(SMALL_H))
                .tooltip(style::tooltip("New tasks start here already"))
        } else {
            let sid = s.id.clone();
            small_button("builder-set-initial", None, "Set as initial", ButtonKind::Secondary, theme).on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| this.change(Change::InitialStatus(sid.clone()), window, cx)),
            )
        };
        let outgoing: Vec<AnyElement> = state
            .transitions()
            .iter()
            .enumerate()
            .filter(|(_, t)| t.from == s.id)
            .map(|(i, t)| {
                let actions: Vec<&str> = t.actions.iter().map(|&a| action_label(a)).collect();
                let label = if actions.is_empty() {
                    status_name(state, &t.to)
                } else {
                    format!("{} · {}", status_name(state, &t.to), actions.join(", "))
                };
                small_button(format!("builder-outgoing-{i}"), None, label, ButtonKind::Secondary, theme)
                    .w_full()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_transition(i, cx)))
                    .into_any_element()
            })
            .collect();
        let add = small_button("builder-status-add-transition", Some(icons::PLUS), "Add transition", ButtonKind::Secondary, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_transition(cx)));
        let only = state.workflow().statuses.len() <= 1;
        let remove = if only {
            style::disabled_button("builder-remove-status", "Remove from workflow", ButtonKind::Danger, theme)
                .h(px(SMALL_H))
                .tooltip(style::tooltip("A workflow needs at least one status"))
        } else {
            let sid = s.id.clone();
            small_button("builder-remove-status", None, "Remove from workflow", ButtonKind::Danger, theme)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.remove_status(&sid, window, cx)))
        };
        let error = state.error_at(&Field::Status(s.id.clone())).map(|e| error_line("builder-status-error", e, theme));
        vec![
            labeled("Status", picker_el, None, theme).into_any_element(),
            category.into_any_element(),
            rule(theme).into_any_element(),
            labeled("Owner", owner, None, theme).into_any_element(),
            labeled("Agent", agent, None, theme).into_any_element(),
            labeled("Workspace tags", div().w_full().children(tags), Some("Only workspaces with these tags."), theme).into_any_element(),
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .w_full()
                .child(div().type_style(Type::Small).text_color(theme.form_text).child("Initial status"))
                .child(div().flex_1())
                .child(set_initial)
                .into_any_element(),
        ]
        .into_iter()
        .chain(error.map(IntoElement::into_any_element))
        .chain([
            rule(theme).into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .w_full()
                .child(div().type_style(Type::SmallMedium).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme.text_loud).child("Outgoing transitions"))
                .children(outgoing)
                .child(div().flex().flex_row().child(add))
                .into_any_element(),
            div().flex_1().into_any_element(),
            div().flex().flex_row().child(remove).into_any_element(),
        ])
        .collect()
    }

    /// Drawing a transition: From, the hint, the destination under the
    /// pointer, Cancel connection.
    fn draw_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.state.as_ref().expect("loaded");
        let Some(d) = self.builder.drawing().cloned() else {
            return Vec::new();
        };
        let from = self.status_picker(Menu::DrawFrom, "builder-draw-from", &d.from, |this, sid, _, _| this.builder.set_from(&sid), theme, cx);
        let destination = match d.over.as_deref().and_then(|id| state.status(id)) {
            Some(s) => style::button_base("builder-destination", ButtonKind::Secondary, false, theme)
                .type_style(Type::BaseMedium)
                .gap(px(6.0))
                .child(div().flex().flex_none().items_center().justify_end().w(px(14.0)).child(style::status_icon_large(s.category, theme)))
                .child(s.name.clone())
                .into_any_element(),
            None => div()
                .debug_selector(|| "builder-destination-none".into())
                .type_style(Type::Base)
                .text_color(theme.form_hint)
                .child("None yet. Click or drop on a status.")
                .into_any_element(),
        };
        let cancel = small_button("builder-cancel-connection", None, "Cancel connection", ButtonKind::Secondary, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_new(cx)));
        vec![
            labeled("From", from, None, theme).into_any_element(),
            div()
                .type_style(Type::Base)
                .text_color(theme.form_hint)
                .child("Drop the connection on a status node to choose where this transition goes.")
                .into_any_element(),
            rule(theme).into_any_element(),
            div().type_style(Type::SmallMedium).text_color(theme.form_hint).child("Destination").into_any_element(),
            div().flex().flex_row().child(destination).into_any_element(),
            div().flex().flex_row().child(cancel).into_any_element(),
        ]
    }

    /// A new transition: From, To, its actions, Cancel and Add transition.
    fn new_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.state.as_ref().expect("loaded");
        let Some(n) = self.builder.new_transition().cloned() else {
            return Vec::new();
        };
        let from = self.status_picker(Menu::NewFrom, "builder-new-from", &n.from, |this, sid, _, _| this.builder.set_from(&sid), theme, cx);
        let to = self.status_picker(
            Menu::NewTo,
            "builder-new-to",
            &n.to,
            |this, sid, _, _| {
                if let Some(n) = this.builder.new_transition_mut() {
                    n.to = sid;
                }
            },
            theme,
            cx,
        );
        let rows: Vec<AnyElement> = n
            .actions
            .iter()
            .enumerate()
            .map(|(slot, &action)| {
                let others: Vec<TransitionAction> = ACTIONS.into_iter().filter(|a| *a == action || !n.actions.contains(a)).collect();
                let handle = div()
                    .id(SharedString::from(format!("builder-new-action-handle-{slot}")))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(20.0))
                    .cursor_grab()
                    .child(heroicon(icons::BARS_2).text_color(theme.icon))
                    .on_drag(
                        NewActionDrag { slot, label: action_label(action).into() },
                        |drag: &NewActionDrag, _, _, cx: &mut App| {
                            let label = drag.label.clone();
                            cx.new(|_| NewActionGhost { label })
                        },
                    );
                let pick = self.dropdown(
                    Menu::NewAction(slot),
                    "builder-new-action-menu",
                    picker(format!("builder-new-action-{slot}"), action_label(action), None, true, theme),
                    move |cx| {
                        let theme = *cx.global::<Theme>();
                        others
                            .iter()
                            .map(|&a| {
                                style::menu_row(format!("builder-new-action-{slot}-{a}"), a == action, &theme)
                                    .child(action_label(a))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.edit_new(cx, |actions| actions[slot] = a)
                                    }))
                                    .into_any_element()
                            })
                            .collect()
                    },
                    CONTROL_W,
                    cx,
                )
                .flex_1()
                .min_w_0();
                let trash = square_button(format!("builder-new-action-remove-{slot}"), icons::TRASH, TRASH_BUTTON, theme)
                    .tooltip(style::tooltip("Remove action"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.edit_new(cx, |actions| {
                            actions.remove(slot);
                        })
                    }));
                let hover = *theme;
                div()
                    .id(SharedString::from(format!("builder-new-action-row-{slot}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .w_full()
                    .rounded(px(6.0))
                    .drag_over::<NewActionDrag>(move |st, _, _, _| st.bg(hover.background))
                    .on_drop(cx.listener(move |this, drag: &NewActionDrag, _, cx| {
                        let from = drag.slot;
                        this.edit_new(cx, |actions| {
                            if from < actions.len() && slot < actions.len() && from != slot {
                                let a = actions.remove(from);
                                actions.insert(slot, a);
                            }
                        })
                    }))
                    .child(handle)
                    .child(pick)
                    .child(trash)
                    .into_any_element()
            })
            .collect();
        let missing: Vec<TransitionAction> = ACTIONS.into_iter().filter(|a| !n.actions.contains(a)).collect();
        let add_action = self.add_action_menu(Menu::NewAddAction, "builder-new-add-action", missing, |this, a, _, cx| this.edit_new(cx, |actions| actions.push(a)), theme, cx);
        let cancel = base_button("builder-new-cancel", None, "Cancel", ButtonKind::Secondary, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_new(cx)));
        let add = base_button("builder-new-add", None, "Add transition", ButtonKind::Select, theme)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.add_new_transition(window, cx)));
        let error = state.error_at(&Field::Transitions).map(|e| error_line("builder-new-error", e, theme).into_any_element());
        let mut out = vec![
            labeled("From", from, None, theme).into_any_element(),
            labeled("To", to, None, theme).into_any_element(),
            rule(theme).into_any_element(),
            self.actions_block(&status_name(state, &n.from), &status_name(state, &n.to), rows, add_action, theme),
            div().flex_1().into_any_element(),
        ];
        out.extend(error);
        out.push(div().flex().flex_row().items_center().w_full().child(cancel).child(div().flex_1()).child(add).into_any_element());
        out
    }

    /// The Actions block: its title, the help naming both ends, the rows,
    /// Add action.
    fn actions_block(&self, from: &str, to: &str, rows: Vec<AnyElement>, add: Div, theme: &Theme) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .w_full()
            .child(div().type_style(Type::SmallMedium).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme.text_loud).child("Actions"))
            .child(
                div()
                    .type_style(Type::Small)
                    .text_color(theme.form_hint)
                    .child(format!("Run in this order when a task moves from {from} to {to}.")),
            )
            .children(rows)
            .child(div().flex().flex_row().child(add))
            .into_any_element()
    }

    /// Add action and its menu of the actions not yet on the transition.
    fn add_action_menu(
        &self,
        menu: Menu,
        id: &'static str,
        missing: Vec<TransitionAction>,
        add: impl Fn(&mut Self, TransitionAction, &mut Window, &mut Context<Self>) + Clone + 'static,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let button = small_button(id, Some(icons::PLUS), "Add action", ButtonKind::Secondary, theme);
        if missing.is_empty() {
            return div().child(button.opacity(0.4).cursor_default().tooltip(style::tooltip("Every action is already here")));
        }
        self.dropdown(
            menu,
            "builder-add-action-menu",
            button,
            move |cx| {
                let theme = *cx.global::<Theme>();
                missing
                    .iter()
                    .map(|&a| {
                        let add = add.clone();
                        style::menu_row(format!("{id}-{a}"), false, &theme)
                            .child(action_label(a))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.menu = None;
                                add(this, a, window, cx);
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect()
            },
            200.0,
            cx,
        )
    }

    /// A transition: From, To, its ordered actions, Add action, the note
    /// when it was just added, Delete transition.
    fn transition_panel(&self, index: usize, added: bool, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = self.state.as_ref().expect("loaded");
        let Some(t) = state.transitions().get(index).cloned() else {
            return Vec::new();
        };
        let from = self.status_picker(
            Menu::From(index),
            "builder-transition-from",
            &t.from,
            move |this, sid, window, cx| this.change(Change::From { index, status: sid }, window, cx),
            theme,
            cx,
        );
        let to = self.status_picker(
            Menu::To(index),
            "builder-transition-to",
            &t.to,
            move |this, sid, window, cx| this.change(Change::To { index, status: sid }, window, cx),
            theme,
            cx,
        );
        let last = t.actions.len().saturating_sub(1);
        let rows: Vec<AnyElement> = t
            .actions
            .iter()
            .enumerate()
            .map(|(slot, &action)| {
                let others: Vec<TransitionAction> = ACTIONS.into_iter().filter(|a| *a == action || !t.actions.contains(a)).collect();
                // The stepper: its top half runs the action earlier, its
                // bottom half later.
                let half = |up: bool, cx: &mut Context<Self>| {
                    let to = if up { slot.checked_sub(1) } else { (slot < last).then_some(slot + 1) };
                    div()
                        .id(SharedString::from(format!("builder-action-{index}-{slot}-{}", if up { "earlier" } else { "later" })))
                        .debug_selector(move || format!("builder-action-{index}-{slot}-{}", if up { "earlier" } else { "later" }))
                        .absolute()
                        .left_0()
                        .w_full()
                        .h(px(ICON_BUTTON / 2.0))
                        .when(up, |d| d.top_0())
                        .when(!up, |d| d.bottom_0())
                        .when_some(to, |d, to| {
                            d.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.change(Change::MoveAction { index, slot, to }, window, cx)
                            }))
                        })
                };
                let stepper = square_button(format!("builder-action-{index}-{slot}-stepper"), icons::CHEVRON_UP_DOWN_BUTTON, ICON_BUTTON, theme)
                    .tooltip(style::tooltip("Run earlier or later"))
                    .child(half(true, cx))
                    .child(half(false, cx));
                let pick = self.dropdown(
                    Menu::BuilderAction(index, slot),
                    "builder-action-menu",
                    picker(format!("builder-action-{index}-{slot}"), action_label(action), None, false, theme),
                    move |cx| {
                        let theme = *cx.global::<Theme>();
                        others
                            .iter()
                            .map(|&a| {
                                style::menu_row(format!("builder-action-{index}-{slot}-{a}"), a == action, &theme)
                                    .child(action_label(a))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        this.change(Change::SetAction { index, slot, action: a }, window, cx)
                                    }))
                                    .into_any_element()
                            })
                            .collect()
                    },
                    CONTROL_W,
                    cx,
                )
                .flex_1()
                .min_w_0();
                let remove = square_button(format!("builder-action-{index}-{slot}-remove"), icons::X_MARK_BUTTON, ICON_BUTTON, theme)
                    .tooltip(style::tooltip("Remove action"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.change(Change::RemoveAction { index, slot }, window, cx)));
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .w_full()
                    .child(stepper)
                    .child(pick)
                    .child(remove)
                    .into_any_element()
            })
            .collect();
        let missing: Vec<TransitionAction> = ACTIONS.into_iter().filter(|a| !t.actions.contains(a)).collect();
        let add_action = self.add_action_menu(
            Menu::AddAction(index),
            "builder-add-action",
            missing,
            move |this, a, window, cx| this.change(Change::AddAction { index, action: a }, window, cx),
            theme,
            cx,
        );
        let delete = base_button("builder-delete-transition", None, "Delete transition", ButtonKind::Danger, theme)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.remove_transition(index, window, cx)));
        let error = state.error_at(&Field::Transition(index)).map(|e| error_line("builder-transition-error", e, theme).into_any_element());
        let mut out = vec![
            labeled("From", from, None, theme).into_any_element(),
            labeled("To", to, None, theme).into_any_element(),
        ];
        out.extend(error);
        out.push(rule(theme).into_any_element());
        out.push(self.actions_block(&status_name(state, &t.from), &status_name(state, &t.to), rows, add_action, theme));
        if added {
            out.push(rule(theme).into_any_element());
            out.push(
                div()
                    .debug_selector(|| "builder-transition-added".into())
                    .type_style(Type::Small)
                    .text_color(theme.form_hint)
                    .child("Transition added. Save changes to apply it to this project.")
                    .into_any_element(),
            );
        }
        out.push(div().flex_1().into_any_element());
        out.push(div().flex().flex_row().child(delete).into_any_element());
        out
    }
}
