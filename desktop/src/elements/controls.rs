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
//!   corners, stroked 1px outside, as Figma strokes it) checked in
//!   `check_fill` with Figma's 12pt mark, beside its label; the list
//!   stacks them 4px apart.
//!
//! [`motion::SlideTrack`]: crate::theme::motion::SlideTrack

use gpui::{div, prelude::*, px, Div, Hsla, SharedString, Stateful};

use crate::assets::{check_mark, heroicon, Icon};
use crate::theme::motion::fill_layer;
use crate::theme::style::{button_base, raised_drops, ButtonKind, Type, Typography, BUTTON_H};
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
/// corners, with the raised card's drops.
pub fn selected_segment<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.canvas).rounded(px(6.0)).shadow(raised_drops(theme))
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

/// A form's button: the Button component's Default style at the Base size
/// (Base medium, loud, on `theme.background`, 32px, 12px across, 6px
/// corners). Hover and press fade to the stronger fills.
pub fn form_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    button_base(id, ButtonKind::Secondary, false, theme)
        .type_style(Type::BaseMedium)
        .whitespace_nowrap()
        .child(label.into())
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
/// `textbox` well in an `input_border` edge; checked, `check_fill` with
/// the mark in white. `focused` turns the edge to the ring (`check_fill`
/// when checked) with the glow around it.
pub fn check(selected: bool, focused: bool, theme: &Theme) -> Div {
    let (fill, edge) = if selected {
        (theme.check_fill, theme.check_fill)
    } else if focused {
        (theme.textbox, theme.focus_ring)
    } else {
        (theme.textbox, theme.input_border)
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
