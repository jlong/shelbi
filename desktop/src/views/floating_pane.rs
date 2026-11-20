//! The floating pane's chrome, shared by every screen that shows a record
//! beside its list without splitting the screen: the task pane over the
//! board ([`super::task_detail`]), the job sheet over the Jobs table
//! ([`super::jobs`]), and whatever comes next.
//!
//! ```text
//! ┌ main area ─────────────────────────────────────────┐
//! │ list …                        ┌──────────────────┐ │  EDGE in from the
//! │ (full width under the pane,   │ Header      ⋮  ✕ │ │  top, right, bottom
//! │  visible in its margins)      │ body             │ │
//! │                               │                  │ │
//! │                               └──────────────────┘ │
//! └────────────────────────────────────────────────────┘
//! ```
//!
//! The pane isn't modal. It sits [`EDGE`] in from the main area's top, right
//! and bottom, on an overlay layer over the host's content, which runs at
//! its full width under it. Its width is the user's ([`PaneFrame`]: 640pt
//! until they drag its left edge, narrower in a window that can't fit it;
//! see [`super::pane_width`]). It is a 12pt-cornered canvas, opaque in every
//! window (the host builds its header and body from `theme.opaque()`), with
//! a divider edge and Figma's `shadow/hard` drop over what it covers.
//!
//! It slides in from the right as it opens ([`PaneMotion::In`], [`SLIDE`] on
//! the executor's clock, so tests can stop it; Reduce Motion shows it in
//! place), and back out the way it came when it closes by any path
//! ([`PaneMotion::Out`]): the host keeps the closed pane in a [`SlideOut`]
//! and draws it, inert, until the slide is up.
//!
//! The Header ([`header`]) is 52pt: the host's title, then its buttons
//! ([`header_button`]: More, Close). The content sits [`INSET`] in, its
//! properties in [`property_row`]s (the muted [`LABEL_W`] label column, then
//! the value).

use gpui::{
    canvas, div, prelude::*, px, AnyElement, BoxShadow, Bounds, Context, CursorStyle, DispatchPhase, Div,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Stateful,
};

use super::column_swap::SLIDE;
use super::pane_width::PaneWidth;
use crate::app::RESIZE_GRAB;
use crate::assets::{heroicon, Icon};
use crate::elements::clocked_animation::clocked_animation;
use crate::theme::motion::{ease_out_expo, ease_out_expo_reversed, fill_layer};
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;

/// The pane's room from the main area's top, right and bottom edges.
pub const EDGE: f32 = 8.0;
/// The pane's corners.
pub const RADIUS: f32 = 12.0;
/// The Header's height.
pub const HEADER_H: f32 = 52.0;
/// A Header button (More, Close): 28pt, a 20pt icon.
const HEADER_BUTTON: f32 = 28.0;
/// The content's inset and the gap between its sections.
pub const INSET: f32 = 20.0;
/// A property's label column.
pub const LABEL_W: f32 = 108.0;

/// Which way the pane is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMotion {
    /// The open pane: it slides in from the right as it first draws.
    In,
    /// A closed pane, the open slide played backwards (same [`SLIDE`], the
    /// curve reversed): drawn inert, taking the pointer from nothing under
    /// it and never the keys, until the host drops it.
    Out,
}

/// The ids (and debug selectors) a host's pane draws under, so tests and
/// the visual check can tell one host's pane from another's.
#[derive(Debug, Clone, Copy)]
pub struct PaneIds {
    /// The open pane.
    pub pane: &'static str,
    /// A closed pane sliding out.
    pub closing: &'static str,
    /// The left edge's resize handle.
    pub resize: &'static str,
    /// The cover over a closing pane's controls.
    pub inert: &'static str,
    /// The slide in's animation.
    pub slide_in: &'static str,
    /// The slide out's animation.
    pub slide_out: &'static str,
}

/// The pane's width and a drag of its left edge: what a host keeps.
#[derive(Debug, Clone, Default)]
pub struct PaneFrame {
    width: PaneWidth,
    resize: Option<PaneResize>,
    /// The pane's layer (the main area inside its insets), measured for the
    /// drag.
    layer: Option<Bounds<Pixels>>,
}

/// A drag of the pane's left edge: the width the pane showed and where the
/// pointer pressed, and the width chosen before it (what decides whether
/// there's a new one to save).
#[derive(Debug, Clone, Copy)]
struct PaneResize {
    from: f32,
    press_x: f32,
    before: f32,
}

impl PaneFrame {
    /// The width the user chose (the pane asks for it; a main area without
    /// room shrinks it for display only).
    pub fn wanted(&self) -> f32 {
        self.width.wanted()
    }

    /// The width the pane draws at: the chosen one, or the layer's room when
    /// that's less (the chosen one until the layer is first measured).
    pub fn shown(&self) -> f32 {
        let wanted = self.width.wanted();
        self.layer.map_or(wanted, |l| self.width.shown(l.size.width.as_f32()))
    }

    /// The window's saved width. Whether it changed.
    pub fn set_saved(&mut self, saved: f32) -> bool {
        let width = PaneWidth::new(saved);
        let changed = width != self.width;
        self.width = width;
        changed
    }

    /// Whether a drag of the edge is on.
    pub fn resizing(&self) -> bool {
        self.resize.is_some()
    }

    /// A press on the pane's left edge at `x`, in window coordinates, starts
    /// a drag from the width the pane shows.
    pub fn start_resize(&mut self, x: f32) {
        let wanted = self.width.wanted();
        self.resize = Some(PaneResize { from: self.shown(), press_x: x, before: wanted });
    }

    /// Follow a drag of the edge to `x`, in window coordinates. Whether the
    /// width changed.
    pub fn drag_to(&mut self, x: f32) -> bool {
        let (Some(drag), Some(layer)) = (self.resize, self.layer) else {
            return false;
        };
        self.width.drag(drag.from, x - drag.press_x, layer.size.width.as_f32())
    }

    /// The drag ended: the new width, when it differs from the one before
    /// it (for the window to save).
    pub fn end_resize(&mut self) -> Option<f32> {
        let drag = self.resize.take()?;
        (self.width.wanted() != drag.before).then(|| self.width.wanted())
    }

    /// Drop a drag without ending it (the pane closed under it).
    pub fn cancel_resize(&mut self) {
        self.resize = None;
    }
}

/// A view that shows a floating pane: where it keeps the pane's
/// [`PaneFrame`], and what it does when a drag of the edge chose a new width.
pub trait PaneHost: Sized + 'static {
    fn pane_frame(&mut self) -> &mut PaneFrame;
    /// A drag of the edge ended on `width`, a new one: hosts hand it to the
    /// window to save (`WindowState::task_pane_width`).
    fn pane_resized(&mut self, width: f32, cx: &mut Context<Self>);
}

/// A closed pane sliding back out: what it showed, kept for one [`SLIDE`].
pub struct SlideOut<T> {
    current: Option<(u64, T)>,
    /// Counts closes, so a slide's end drops only its own pane.
    closes: u64,
}

impl<T> Default for SlideOut<T> {
    fn default() -> Self {
        Self { current: None, closes: 0 }
    }
}

impl<T: 'static> SlideOut<T> {
    /// The pane sliding out, while one is.
    pub fn get(&self) -> Option<&T> {
        self.current.as_ref().map(|(_, t)| t)
    }

    /// Drop it now (a pane opened over it, or what it covered went away).
    pub fn clear(&mut self) {
        self.current = None;
    }

    /// Keep `item` drawn as its pane slides out, then drop it after one
    /// [`SLIDE`] (unless a later close replaced it) and call `done`. With
    /// Reduce Motion the pane goes at once, as it opens in place.
    pub fn start<V: 'static>(
        &mut self,
        item: T,
        field: fn(&mut V) -> &mut SlideOut<T>,
        done: fn(&mut V, &mut Context<V>),
        cx: &mut Context<V>,
    ) {
        if cx.reduce_motion() {
            self.current = None;
            return;
        }
        self.closes += 1;
        let close = self.closes;
        self.current = Some((close, item));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SLIDE).await;
            let _ = this.update(cx, |this, cx| {
                let slide = field(this);
                if slide.current.as_ref().is_some_and(|(c, _)| *c == close) {
                    slide.current = None;
                    done(this, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// The pane, on its overlay layer over the main area: `header` over `body`
/// in the canvas, at the frame's width, sliding as `motion` says. `theme` is
/// the opaque theme the host built the header and body from. `focus` wires
/// the open pane's pointer and keys (a closing pane never takes either).
#[allow(clippy::too_many_arguments)]
pub fn render<V: PaneHost>(
    frame: &PaneFrame,
    ids: PaneIds,
    motion: PaneMotion,
    header: Div,
    body: AnyElement,
    focus: impl FnOnce(Stateful<Div>, &mut Context<V>) -> Stateful<Div>,
    theme: &Theme,
    cx: &mut Context<V>,
) -> AnyElement {
    let closing = motion == PaneMotion::Out;
    // The chosen width; the row it sits in shrinks it in a main area too
    // narrow for it.
    let width = frame.wanted();
    let selector = if closing { ids.closing } else { ids.pane };
    let pane = div()
        .id(selector)
        .debug_selector(move || selector.into())
        .relative()
        .flex()
        .flex_col()
        .h_full()
        .w(px(width))
        .min_w_0()
        .overflow_hidden()
        .bg(theme.canvas)
        .rounded(px(RADIUS))
        .shadow(pane_shadow(theme))
        .type_style(Type::Base)
        .text_color(theme.foreground)
        // What's under the pane gets none of its pointer.
        .occlude()
        .child(header)
        .child(body)
        // The edge, drawn over the content inside the pane's bounds (Figma
        // strokes inside), so it takes no room from the layout.
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(theme.divider),
        )
        .child(resize_handle(frame, ids.resize, theme, cx))
        // Sliding out, the pane's controls are past use: a cover over them
        // takes the pointer, and the pane never takes the keys.
        .when(closing, |pane| pane.child(div().id(ids.inert).absolute().top_0().left_0().size_full().occlude()));
    let pane = if closing { pane } else { focus(pane, cx) };
    let travel = width + EDGE;
    let pane = match motion {
        // In from past the window's right edge.
        PaneMotion::In => clocked_animation(ids.slide_in, pane, SLIDE, ease_out_expo, move |pane, t| {
            pane.left(px((1.0 - t) * travel))
        })
        .into_any_element(),
        // Back out the way it came, along the same curve backwards.
        PaneMotion::Out => clocked_animation(ids.slide_out, pane, SLIDE, ease_out_expo_reversed, move |pane, t| {
            pane.left(px(t * travel))
        })
        .into_any_element(),
    };
    div()
        .absolute()
        .top(px(EDGE))
        .right(px(EDGE))
        .bottom(px(EDGE))
        .left(px(EDGE))
        .flex()
        .flex_row()
        .justify_end()
        .child(pane)
        .when(!closing, |layer| layer.child(resize_probe(frame, cx)))
        .into_any_element()
}

/// The pane's left edge: the sidebar edge's grab room and cursor, laid over
/// the pane's first points. A press starts a drag ([`resize_probe`] follows
/// it); while it's on, a rule lights the edge between the corners, as the
/// sidebar's edge lights.
fn resize_handle<V: PaneHost>(frame: &PaneFrame, id: &'static str, theme: &Theme, cx: &mut Context<V>) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .w(px(RESIZE_GRAB))
        .cursor(CursorStyle::ResizeLeftRight)
        .when(frame.resizing(), |d| {
            d.child(div().absolute().top(px(RADIUS)).bottom(px(RADIUS)).left_0().w(px(1.0)).bg(theme.icon))
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this: &mut V, ev: &MouseDownEvent, window, cx| {
                this.pane_frame().start_resize(ev.position.x.as_f32());
                cx.notify();
                // The keys stay where they were (an editor keeps its caret).
                window.prevent_default();
                cx.stop_propagation();
            }),
        )
}

/// Measures the pane's layer for the drag (and for the width the pane
/// shows, which picks its host's layout: a change repaints), and while a drag of the pane's
/// edge is on follows the pointer anywhere in the window until the button
/// comes up (the pointer leaves the pane, and the layer, as it goes).
fn resize_probe<V: PaneHost>(frame: &PaneFrame, cx: &mut Context<V>) -> impl IntoElement {
    let measured = cx.entity().downgrade();
    let weak = cx.entity().downgrade();
    let resizing = frame.resizing();
    canvas(
        move |bounds, _, cx| {
            let _ = measured.update(cx, |this, cx| {
                let frame = this.pane_frame();
                if frame.layer != Some(bounds) {
                    frame.layer = Some(bounds);
                    cx.notify();
                }
            });
        },
        move |_, _, window, _| {
            if !resizing {
                return;
            }
            let moved = weak.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    let _ = moved.update(cx, |this, cx| {
                        if this.pane_frame().drag_to(e.position.x.as_f32()) {
                            cx.notify();
                        }
                    });
                }
            });
            let up = weak.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    let _ = up.update(cx, |this, cx| {
                        if !this.pane_frame().resizing() {
                            return;
                        }
                        if let Some(width) = this.pane_frame().end_resize() {
                            this.pane_resized(width, cx);
                        }
                        cx.notify();
                    });
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The frame's elevation over what it covers: Figma's `0 8 32` drop in
/// `shadow/hard` (the `pane_shadow` role).
fn pane_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.0), px(8.0), theme.pane_shadow).blur_radius(style::figma_blur(32.0))]
}

/// The Header: `title` (debug selector `heading`), loud and cut to one line,
/// then the host's `buttons` at the trailing edge.
pub fn header(heading: &'static str, title: impl IntoElement, buttons: Vec<AnyElement>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .h(px(HEADER_H))
        .px(px(12.0))
        // The pane's top corners: its overflow clip is square, so the band
        // rounds its own fill (and its edge) to the pane's.
        .rounded_t(px(RADIUS))
        .bg(theme.toolbar)
        // The Header's 1px canvas edge on its left.
        .border_l_1()
        .border_color(theme.canvas)
        .child(
            div()
                .debug_selector(move || heading.into())
                .flex_1()
                .min_w_0()
                .truncate()
                .type_style(Type::BaseSemibold)
                .text_color(theme.text_loud)
                .child(title),
        )
        .child(div().flex().flex_row().flex_none().items_center().gap(px(4.0)).children(buttons))
}

/// A Header button: 28pt with 8pt corners, the 20pt icon in the icon role,
/// a fill fading in (and a loud icon) on hover, held while `active`.
pub fn header_button(id: &'static str, icon: Icon, active: bool, theme: &Theme) -> Stateful<Div> {
    let radius = px(8.0);
    let loud = theme.text_loud;
    let rest = if active { theme.background_hover } else { theme.background.opacity(0.0) };
    let d = div()
        .id(id)
        .debug_selector(move || id.into())
        .group(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(HEADER_BUTTON))
        .rounded(radius)
        .cursor_pointer();
    fill_layer(d, id, radius, [rest, theme.background_hover, theme.background_active])
        .child(heroicon(icon).text_color(theme.icon).group_hover(id, move |s| s.text_color(loud)))
}

/// A property row: 28pt, the label in its muted column, 12pt before the
/// value.
pub fn property_row(theme: &Theme, label: &'static str, value: impl IntoElement) -> Div {
    property_row_at(theme, label, LABEL_W, value)
}

/// [`property_row`] with its label column `label_w` wide (the task pane's
/// side column is narrower than the frame's).
pub fn property_row_at(theme: &Theme, label: &'static str, label_w: f32, value: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .min_h(px(28.0))
        .w_full()
        .min_w_0()
        .child(
            div()
                .w(px(label_w))
                .flex_none()
                .truncate()
                .type_style(Type::Small)
                .text_color(theme.muted)
                .child(label),
        )
        .child(div().flex().flex_row().items_center().flex_1().min_w_0().child(value))
}

/// A text value, 8pt in so it lines up with the buttons' labels' edge.
pub fn value_text(theme: &Theme) -> Div {
    div()
        .min_w_0()
        .px(px(8.0))
        .truncate()
        .type_style(Type::Small)
        .text_color(theme.foreground)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_in(room: f32) -> PaneFrame {
        PaneFrame {
            layer: Some(Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(px(room), px(800.0)))),
            ..PaneFrame::default()
        }
    }

    #[test]
    fn a_drag_follows_the_pointer_from_where_it_pressed_and_reports_a_new_width() {
        let mut f = frame_in(1144.0);
        assert!(!f.drag_to(100.0), "no drag, no change");
        f.start_resize(500.0);
        assert!(f.resizing());
        assert!(f.drag_to(400.0));
        assert_eq!(f.wanted(), 740.0);
        assert_eq!(f.end_resize(), Some(740.0));
        assert!(!f.resizing());
        f.start_resize(500.0);
        assert_eq!(f.end_resize(), None, "a press without a move chose nothing new");
    }

    #[test]
    fn a_narrow_layer_starts_the_drag_from_the_width_it_shows() {
        let mut f = frame_in(560.0);
        f.start_resize(300.0);
        f.drag_to(290.0);
        // It shows 560 (all the room), not the 640 chosen: 10pt wider than
        // that leaves too little beside it, so it holds the minimum.
        assert_eq!(f.wanted(), super::super::pane_width::MIN_W);
    }

    #[test]
    fn a_saved_width_is_taken_and_junk_is_the_default() {
        let mut f = PaneFrame::default();
        assert!(f.set_saved(820.0));
        assert!(!f.set_saved(820.0));
        assert_eq!(f.wanted(), 820.0);
        f.set_saved(f32::NAN);
        assert_eq!(f.wanted(), super::super::pane_width::DEFAULT_W);
    }
}
