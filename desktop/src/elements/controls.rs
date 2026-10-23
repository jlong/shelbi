//! The Figma form components (the Components page, `2196:10061`), styled in
//! the palette's roles. Settings composes them; any form can.
//!
//! - [`labeled_control`] (`2238:25403`): the label (Base, loud), the control,
//!   then an optional helper (X-Small, `form_hint`), 6px apart, at most
//!   [`LABELED_MAX_W`] wide;
//! - [`segmented_track`] and [`segment`] (`2238:25203`, item `2238:25185`):
//!   the `theme.search` track, 4px in, 10px corners; the selected item the
//!   canvas with the raised card's drops ([`selected_segment`]), which
//!   slides between items (a [`motion::SlideTrack`] probes them);
//! - [`textbox`] (`2238:25293`): 32px, a 1px `input_border` around the
//!   `textbox` well (5px corners inside 6), its text Base in `form_text`, or
//!   `form_hint` for a placeholder; focused, the edge is the
//!   `focus/default/ring` with the 3px `focus-glow` around it;
//! - [`picker`] (`2238:25517`): a textbox with a [`form_button`] after it,
//!   8px apart, [`CONTROL_W`] wide;
//! - [`form_button`] and [`form_icon_button`]: the Button component's
//!   Default style at the Base size (32px, 6px corners), and its 32px
//!   square Icon Button;
//! - [`check`] (`2238:25598`), [`checkbox`] (`2238:25621`) and
//!   [`checkbox_list`] (`2238:25702`): a 14px check (a 12px box, 2px
//!   corners, stroked 1px outside, as Figma strokes it) edged in
//!   `check_border` and checked in `check_fill` with Figma's 12pt mark,
//!   beside its label; the list stacks them 4px apart;
//! - [`Slider`]: a value along a track, its readout after it. The
//!   Components page draws no slider, so it is built from the tokens the
//!   drawn controls use: the segmented track's `theme.search` for the rail,
//!   `check_fill` (the forms' selected color) for the filled part, and a
//!   round thumb in the Button's fill over the canvas, with the textbox's
//!   edge and the raised drops; the focus ring and glow around the thumb
//!   while it has the keyboard. ← / → step it, ⇧← / ⇧→ step it further,
//!   Home / End go to the ends; a click or a drag on the track sets it.
//!
//! [`motion::SlideTrack`]: crate::theme::motion::SlideTrack

use std::cell::Cell;
use std::ops::RangeInclusive;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, relative, App, Bounds, Context, DispatchPhase, Div, EventEmitter,
    FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, SharedString, Stateful, Window,
};

use crate::assets::{check_mark, heroicon, Icon};
use crate::theme::motion::fill_layer;
use crate::theme::style::{button, focus_shadow, raised_drops, ButtonKind, ButtonSize, Type, Typography, BUTTON_H};
use crate::theme::Theme;

/// A labeled control's widest: its helper wraps at this.
pub const LABELED_MAX_W: f32 = 640.0;
/// A form control's width (a picker, a full-width textbox, the Theme
/// segmented control, the checkbox list).
pub const CONTROL_W: f32 = 400.0;
/// The gap between a picker's textbox and its button, and between a
/// textbox and the icon buttons after it.
pub const CONTROL_GAP: f32 = 8.0;

/// A labeled control: `label`, then `control`, then `help` when given.
/// Further lines (a warning under the helper) go in as children.
pub fn labeled_control(
    label: impl Into<SharedString>,
    control: impl IntoElement,
    help: Option<SharedString>,
    theme: &Theme,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .w_full()
        .max_w(px(LABELED_MAX_W))
        .child(
            div()
                .type_style(Type::Base)
                .text_color(theme.text_loud)
                .child(label.into()),
        )
        .child(div().flex().flex_row().items_start().w_full().child(control))
        .children(help.map(|help| helper(help, theme)))
}

/// A labeled control's helper line: X-Small in `form_hint`.
pub fn helper(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div().type_style(Type::XSmall).text_color(theme.form_hint).child(text.into())
}

/// A segmented control's track. Its items go in as children ([`segment`]),
/// after [`motion::origin_probe`] and, while the selection slides, a
/// [`motion::floating_chip`] painted by [`selected_segment`].
///
/// [`motion::origin_probe`]: crate::theme::motion::origin_probe
/// [`motion::floating_chip`]: crate::theme::motion::floating_chip
pub fn segmented_track(theme: &Theme) -> Div {
    div()
        .relative()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .p(px(4.0))
        .rounded(px(10.0))
        .bg(theme.search)
}

/// Paint `el` as a segmented control's selected item: the canvas, 6px
/// corners, with the raised card's drops. The canvas is the opaque one
/// whatever the window's translucency: the drops would show through it.
pub fn selected_segment<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.opaque().canvas).rounded(px(6.0)).shadow(raised_drops(theme))
}

/// One item of a [`segmented_track`]: 24px, 12px across, Base in
/// `form_text`; selected, Base medium and loud on the [`selected_segment`]
/// (unless `chip_inline` is false while a sliding chip stands in).
/// Unselected, a fill fades in on hover.
pub fn segment(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    selected: bool,
    chip_inline: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.0))
        .px(px(12.0))
        .rounded(px(6.0))
        .whitespace_nowrap()
        .cursor_pointer();
    let d = if selected {
        let d = d.type_style(Type::BaseMedium).text_color(theme.text_loud);
        if chip_inline {
            selected_segment(d, theme)
        } else {
            d
        }
    } else {
        let clear = theme.background_hover.opacity(0.0);
        fill_layer(
            d.type_style(Type::Base).text_color(theme.form_text),
            id,
            px(6.0),
            [clear, theme.background_hover, theme.background_active],
        )
    };
    d.child(label.into())
}

/// The focus glow around a focused control whose corners are `radius`: a
/// 3px band in `color` just outside it (the ring, the control's own edge,
/// keeps its color).
fn glow(radius: f32, color: Hsla) -> Div {
    div()
        .absolute()
        .top(px(-3.0))
        .left(px(-3.0))
        .right(px(-3.0))
        .bottom(px(-3.0))
        .rounded(px(radius + 3.0))
        .border(px(3.0))
        .border_color(color)
}

/// A textbox around `content` (an `Input` with its appearance off, or the
/// text itself): 32px tall, the caller sets its width. `focused` turns the
/// edge to the focus ring, with the glow around it.
pub fn textbox(content: impl IntoElement, focused: bool, theme: &Theme) -> Div {
    let edge = if focused {
        theme.focus_ring
    } else {
        theme.input_border
    };
    edged_textbox(content, edge, focused.then_some(theme.focus_glow), theme)
}

/// A [`textbox`] holding text it won't take (a color that isn't one): the
/// edge in `error`, and its glow while focused. The Components page draws
/// no error state for the Textbox, so this is the form's own.
pub fn invalid_textbox(content: impl IntoElement, focused: bool, theme: &Theme) -> Div {
    edged_textbox(content, theme.error, focused.then(|| theme.error.opacity(0.3)), theme)
}

fn edged_textbox(content: impl IntoElement, edge: Hsla, glow_color: Option<Hsla>, theme: &Theme) -> Div {
    // The edge is the box's fill, showing 1px around the well, so the well's
    // 5px corners sit inside the box's 6px as Figma nests them.
    div()
        .relative()
        .flex()
        .flex_row()
        .flex_none()
        .h(px(BUTTON_H))
        .p(px(1.0))
        .rounded(px(6.0))
        .bg(edge)
        .child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_w_0()
                .h_full()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .rounded(px(5.0))
                .bg(theme.textbox)
                .type_style(Type::Base)
                .text_color(theme.form_text)
                .child(content),
        )
        .children(glow_color.map(|color| glow(6.0, color)))
}

/// A picker: `textbox` (it grows) and `button` after it, [`CONTROL_W`]
/// wide.
pub fn picker(textbox: impl IntoElement, button: impl IntoElement) -> Div {
    div()
        .relative()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(CONTROL_GAP))
        .w(px(CONTROL_W))
        .child(textbox)
        .child(button)
}

/// A form's button (a picker's Browse): the Button component's Default
/// style at the Base size. Hover and press fade to the stronger fills.
pub fn form_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    button(id, label, ButtonKind::Secondary, ButtonSize::Base, false, theme).whitespace_nowrap()
}

/// The Icon Button beside a textbox: 32px square, 6px corners, `icon` in
/// `button_icon` on `theme.background`. `inert` dims it and drops the
/// pointer (a stepper at its end).
pub fn form_icon_button(id: impl Into<SharedString>, icon: Icon, inert: bool, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(BUTTON_H))
        .rounded(px(6.0))
        .when(inert, |d| d.opacity(0.4))
        .when(!inert, |d| d.cursor_pointer());
    fill_layer(d, id, px(6.0), [theme.background, theme.background_hover, theme.background_active])
        .child(heroicon(icon).text_color(theme.button_icon))
}

/// The check by itself: Figma's 12px box with 2px corners and a 1px stroke
/// outside it, so 14px edge to edge (3px corners). Unchecked, the
/// `textbox` well in a `check_border` edge (`checkbox/default/border`, its
/// own token: lighter than a textbox's edge in Dark); checked,
/// `check_fill` with the mark in white. `focused` turns the edge to the
/// ring (`check_fill` when checked) with the glow around it.
pub fn check(selected: bool, focused: bool, theme: &Theme) -> Div {
    let (fill, edge) = if selected {
        (theme.check_fill, theme.check_fill)
    } else if focused {
        (theme.textbox, theme.focus_ring)
    } else {
        (theme.textbox, theme.check_border)
    };
    let glow_color = if selected {
        theme.check_glow
    } else {
        theme.focus_glow
    };
    div()
        .relative()
        .flex_none()
        .size(px(14.0))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .rounded(px(3.0))
                .border_1()
                .border_color(edge)
                .bg(fill),
        )
        .when(selected, |d| {
            d.child(
                div()
                    .absolute()
                    .top(px(1.0))
                    .left(px(1.0))
                    .child(check_mark().text_color(theme.text_on_color)),
            )
        })
        .when(focused, |d| d.child(glow(3.0, glow_color)))
}

/// A checkbox: the [`check`] and its label (Base; loud when checked,
/// `form_text` when not), 8px apart, 2px in.
pub fn checkbox(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    selected: bool,
    focused: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(2.0))
        .cursor_pointer()
        .child(check(selected, focused, theme))
        .child(
            div()
                .type_style(Type::Base)
                .whitespace_nowrap()
                .text_color(if selected { theme.text_loud } else { theme.form_text })
                .child(label.into()),
        )
}

/// A checkbox list: its [`checkbox`]es stacked 4px apart, 4px in from top
/// and bottom, [`CONTROL_W`] wide.
pub fn checkbox_list() -> Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(4.0))
        .py(px(4.0))
        .w(px(CONTROL_W))
}

/// A slider's thumb, across.
pub const THUMB: f32 = 16.0;
/// A slider's rail, tall.
const RAIL_H: f32 = 4.0;
/// The slider's height: the thumb with its glow's room.
const SLIDER_H: f32 = 24.0;
/// The readout's column after the track.
const READOUT_W: f32 = 44.0;

/// `value` moved `delta` along `range`, stopping at its ends.
pub fn slider_step(value: i32, delta: i32, range: &RangeInclusive<i32>) -> i32 {
    value.saturating_add(delta).clamp(*range.start(), *range.end())
}

/// Where along the rail `value` sits, 0 to 1.
pub fn slider_fraction(value: i32, range: &RangeInclusive<i32>) -> f32 {
    let span = (range.end() - range.start()) as f32;
    if span <= 0.0 {
        return 0.0;
    }
    ((value - range.start()) as f32 / span).clamp(0.0, 1.0)
}

/// The value under `x` on a track spanning `left..right` (the thumb's
/// center travels it), to the nearest whole step.
pub fn slider_value_at(x: f32, left: f32, right: f32, range: &RangeInclusive<i32>) -> i32 {
    let t = if right > left { ((x - left) / (right - left)).clamp(0.0, 1.0) } else { 0.0 };
    let span = (range.end() - range.start()) as f32;
    range.start() + (t * span).round() as i32
}

/// What a [`Slider`] tells its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliderEvent {
    /// The value moved (each step of a drag): show it.
    Changed(i32),
    /// The user let go (the end of a drag or click, or a key step): keep it.
    Committed(i32),
}

/// A slider: a value in `range` along a track, its readout (the value and
/// `unit`) after it, [`CONTROL_W`] wide.
pub struct Slider {
    id: SharedString,
    value: i32,
    range: RangeInclusive<i32>,
    /// What ← / → move it by, and ⇧← / ⇧→.
    step: i32,
    big_step: i32,
    unit: &'static str,
    disabled: bool,
    focus: FocusHandle,
    /// The track's bounds as last painted (where a click or drag lands).
    track: Rc<Cell<Option<Bounds<Pixels>>>>,
    dragging: bool,
}

impl EventEmitter<SliderEvent> for Slider {}

impl Focusable for Slider {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Slider {
    pub fn new(
        id: impl Into<SharedString>,
        value: i32,
        range: RangeInclusive<i32>,
        (step, big_step): (i32, i32),
        unit: &'static str,
        cx: &mut Context<Self>,
    ) -> Self {
        Slider {
            id: id.into(),
            value: value.clamp(*range.start(), *range.end()),
            range,
            step,
            big_step,
            unit,
            disabled: false,
            focus: cx.focus_handle(),
            track: Rc::default(),
            dragging: false,
        }
    }

    #[cfg(test)]
    pub fn value(&self) -> i32 {
        self.value
    }

    /// Show `value` (clamped) without telling the owner: a value set
    /// elsewhere.
    pub fn set_value(&mut self, value: i32, cx: &mut Context<Self>) {
        let value = value.clamp(*self.range.start(), *self.range.end());
        if value != self.value {
            self.value = value;
            cx.notify();
        }
    }

    /// Dim the slider and stop it taking input.
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if disabled != self.disabled {
            self.disabled = disabled;
            self.dragging = false;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Move to `value` (clamped) as the user did, telling the owner it
    /// changed.
    fn change(&mut self, value: i32, cx: &mut Context<Self>) {
        let value = value.clamp(*self.range.start(), *self.range.end());
        if value != self.value {
            self.value = value;
            cx.emit(SliderEvent::Changed(value));
            cx.notify();
        }
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        cx.emit(SliderEvent::Committed(self.value));
    }

    /// The keys: ← / → a step, with ⇧ a big step, Home / End the ends.
    /// Returns whether the key was the slider's.
    pub fn key(&mut self, key: &str, shift: bool, cx: &mut Context<Self>) -> bool {
        if self.disabled {
            return false;
        }
        let by = if shift { self.big_step } else { self.step };
        let next = match key {
            "left" => slider_step(self.value, -by, &self.range),
            "right" => slider_step(self.value, by, &self.range),
            "home" => *self.range.start(),
            "end" => *self.range.end(),
            _ => return false,
        };
        self.change(next, cx);
        self.commit(cx);
        true
    }

    /// The value under window `x` on the track as last painted.
    fn value_at(&self, x: Pixels) -> Option<i32> {
        let b = self.track.get()?;
        let left = b.left().as_f32() + THUMB / 2.0;
        let right = b.right().as_f32() - THUMB / 2.0;
        Some(slider_value_at(x.as_f32(), left, right, &self.range))
    }

    fn press(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        window.focus(&self.focus, cx);
        self.dragging = true;
        if let Some(value) = self.value_at(x) {
            self.change(value, cx);
        }
        cx.notify();
    }

    fn drag(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if let Some(value) = self.value_at(x).filter(|_| self.dragging) {
            self.change(value, cx);
        }
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.dragging) {
            self.commit(cx);
            cx.notify();
        }
    }
}

impl Render for Slider {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let fraction = slider_fraction(self.value, &self.range);
        let focused = !self.disabled && self.focus.is_focused(window);
        let track = self.track.clone();
        let weak = cx.entity().downgrade();
        let dragging = self.dragging;
        // Measure the track, and while a drag is on follow the pointer
        // anywhere in the window until the button comes up.
        let probe = canvas(
            move |bounds, _, _| track.set(Some(bounds)),
            move |_, _, window, _| {
                if !dragging {
                    return;
                }
                let moved = weak.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = moved.update(cx, |s, cx| s.drag(e.position.x, cx));
                    }
                });
                let up = weak.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = up.update(cx, |s, cx| s.release(cx));
                    }
                });
            },
        )
        .absolute()
        .size_full();
        let mut shadows = raised_drops(&theme);
        if focused {
            shadows.extend(focus_shadow(&theme));
        }
        let thumb_id = SharedString::from(format!("{}-thumb", self.id));
        let thumb = div()
            .absolute()
            .top(px((SLIDER_H - THUMB) / 2.0))
            .left(px(-THUMB / 2.0))
            .size(px(THUMB))
            .rounded_full()
            .bg(theme.opaque().canvas)
            .border_1()
            .border_color(theme.input_border)
            .shadow(shadows)
            .child(fill_layer(
                div().id(thumb_id.clone()).debug_selector(move || thumb_id.to_string()).size_full().rounded_full(),
                format!("{}-thumb", self.id),
                px(THUMB / 2.0),
                [theme.background, theme.background_hover, theme.background_active],
            ));
        let rail = |d: Div| d.absolute().top(px((SLIDER_H - RAIL_H) / 2.0)).h(px(RAIL_H)).rounded(px(RAIL_H / 2.0));
        // The thumb's center travels the track inset by half a thumb at
        // each end, so it never overhangs the control.
        let travel = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(THUMB / 2.0))
            .right(px(THUMB / 2.0))
            .child(rail(div()).left_0().right_0().bg(theme.search))
            .child(rail(div()).left_0().w(relative(fraction)).bg(theme.check_fill))
            .child(div().absolute().top_0().left(relative(fraction)).child(thumb));
        let selector = self.id.clone();
        let track = div()
            .id(self.id.clone())
            .debug_selector(move || selector.to_string())
            .track_focus(&self.focus)
            .relative()
            .flex_1()
            .h(px(SLIDER_H))
            .when(!self.disabled, |d| d.cursor_pointer())
            .child(probe)
            .child(travel)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if this.key(e.keystroke.key.as_str(), e.keystroke.modifiers.shift, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    this.press(e.position.x, window, cx);
                    cx.stop_propagation();
                }),
            )
            // A click lets go before the drag's window-wide listener is
            // painted.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, cx| this.release(cx)));
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(12.0))
            .w(px(CONTROL_W))
            .when(self.disabled, |d| d.opacity(0.4))
            .child(track)
            .child(
                div()
                    .flex_none()
                    .w(px(READOUT_W))
                    .text_right()
                    .type_style(Type::Base)
                    .text_color(theme.form_text)
                    .child(format!("{}{}", self.value, self.unit)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_stops_at_the_ends() {
        let r = 30..=100;
        assert_eq!((slider_step(85, 1, &r), slider_step(85, -1, &r)), (86, 84));
        assert_eq!((slider_step(85, 10, &r), slider_step(35, -10, &r)), (95, 30));
        assert_eq!((slider_step(100, 1, &r), slider_step(30, -1, &r)), (100, 30));
        assert_eq!(slider_step(i32::MAX, 1, &r), 100, "no overflow");
    }

    #[test]
    fn the_thumb_sits_by_fraction_and_a_position_maps_back() {
        let r = 0..=50;
        assert_eq!((slider_fraction(0, &r), slider_fraction(25, &r), slider_fraction(50, &r)), (0.0, 0.5, 1.0));
        assert_eq!(slider_fraction(80, &r), 1.0);
        assert_eq!(slider_fraction(3, &(5..=5)), 0.0, "an empty range");
        // A 200pt track from x 100: its ends, its middle, a pointer past it.
        assert_eq!(slider_value_at(100.0, 100.0, 300.0, &r), 0);
        assert_eq!(slider_value_at(200.0, 100.0, 300.0, &r), 25);
        assert_eq!(slider_value_at(203.0, 100.0, 300.0, &r), 26, "to the nearest step");
        assert_eq!((slider_value_at(20.0, 100.0, 300.0, &r), slider_value_at(900.0, 100.0, 300.0, &r)), (0, 50));
        let o = 30..=100;
        assert_eq!(slider_value_at(200.0, 100.0, 300.0, &o), 65);
        for v in o.clone() {
            let x = 100.0 + slider_fraction(v, &o) * 200.0;
            assert_eq!(slider_value_at(x, 100.0, 300.0, &o), v, "round trip at {v}");
        }
    }
}
