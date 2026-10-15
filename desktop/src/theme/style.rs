//! Taskpage's component shapes, in the palette's roles.
//!
//! One place for the shapes every view shares, so a button or a selected row
//! reads the same in the sidebar, a panel, the board and an overlay:
//!
//! - 8px corners everywhere ([`gpui::Styled::rounded_lg`]), 12px on the
//!   larger icon buttons;
//! - nav rows 40px tall with the content 8px inside, labels medium weight,
//!   the selected row a raised chip ([`nav_chip`]: white with Taskpage's
//!   `shadow-nav` in light, a lifted surface with a 1px ring in dark);
//! - buttons semibold with a 2px border ([`button`]): primary the brand fill,
//!   secondary slate, danger the error fill;
//! - quiet icon buttons ([`icon_button`]) 24/32/40px on a hover fill;
//! - menus, popovers and the palette raised with a ring and `shadow-lg`
//!   ([`popover`]);
//! - chips tinted from one hue ([`chip`]);
//! - the brand pill marking the keyboard-current row ([`selection_indicator`]).
//!
//! Spacing follows Taskpage's 8px scale (2, 4, 6, 8, 10, 12, 16, 20, 24...).

use gpui::{
    div, point, prelude::*, px, BoxShadow, Div, FontWeight, Hsla, SharedString, Stateful,
};

use super::{Hue, Theme};
use crate::assets::{heroicon, Icon};

/// A nav row's height (Taskpage's `h-5`). The chip inside is inset 2px top
/// and bottom.
pub const NAV_ROW_H: f32 = 40.0;
/// A list row's height in the leading column (workspaces, machine groups,
/// project rows): Taskpage's 32px control height, so a long list stays
/// scannable.
pub const LIST_ROW_H: f32 = 32.0;
/// A button's height at the medium size.
pub const BUTTON_H: f32 = 32.0;

/// Taskpage's `shadow-nav`: a 2px drop and a 1px hairline ring.
pub fn nav_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(2.0), theme.nav_shadow),
        ring_shadow(theme.nav_selected_ring),
    ]
}

/// Tailwind's `shadow-lg` plus a 1px ring (`ring-1 ring-black-600/5`).
pub fn popover_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(10.0), theme.shadow)
            .blur_radius(px(15.0))
            .spread_radius(px(-3.0)),
        BoxShadow::new(px(0.0), px(4.0), theme.shadow)
            .blur_radius(px(6.0))
            .spread_radius(px(-4.0)),
        ring_shadow(theme.ring),
    ]
}

/// A card's resting elevation: the ring and a 1px drop.
pub fn card_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(1.0), theme.nav_shadow),
        ring_shadow(theme.ring),
    ]
}

/// A 1px ring drawn outside the box, so it takes no layout room.
fn ring_shadow(color: Hsla) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(1.0),
        inset: false,
    }
}

/// Paint `el` as the selected nav row: the raised chip.
pub fn nav_chip<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.nav_selected).shadow(nav_shadow(theme))
}

/// Paint `el` as a raised surface: a menu, a popover, the palette, a dialog.
pub fn popover<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.surface_raised)
        .rounded_lg()
        .shadow(popover_shadow(theme))
        .text_color(theme.text)
}

/// The brand pill at a row's leading edge marking the keyboard-current row.
/// Absolutely positioned: the row needs `.relative()`.
pub fn selection_indicator(theme: &Theme) -> Div {
    div()
        .absolute()
        .left(px(-6.0))
        .top_0()
        .bottom_0()
        .flex()
        .items_center()
        .child(div().w(px(3.0)).h(px(16.0)).rounded_full().bg(theme.brand_text))
}

/// Paint a row in a nav list by its state: the raised chip while it is the
/// current page (`current`), the brand pill while it is the keyboard's row
/// (`keyboard`, over a fill when it isn't also current), else a hover fill.
pub fn row_state(d: Stateful<Div>, current: bool, keyboard: bool, theme: &Theme) -> Stateful<Div> {
    let d = d.relative();
    let d = if current {
        nav_chip(d, theme)
            .text_color(theme.text_loud)
            .font_weight(FontWeight::MEDIUM)
    } else if keyboard {
        d.bg(theme.fill)
    } else {
        d.hover(|s| s.bg(theme.fill))
    };
    d.when(keyboard, |d| d.child(selection_indicator(theme)))
}

/// A section heading: Taskpage's `text-sm font-semibold`.
pub fn heading(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_loud)
        .child(text.into())
}

/// A button's look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// The brand fill with white text: the one main action.
    Primary,
    /// Slate fill with loud text: every other action.
    Secondary,
    /// A destructive action: the error fill.
    Danger,
}

/// A Taskpage button: semibold, 8px corners, a 2px border, 16px across.
/// `focused` draws the focus ring (keyboard focus in a dialog).
pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    focused: bool,
    theme: &Theme,
) -> Stateful<Div> {
    button_base(id, kind, focused, theme).child(label.into())
}

/// A button with an icon before its label, the icon in the label's color.
pub fn icon_label_button(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    theme: &Theme,
) -> Stateful<Div> {
    let (_, fg, _, _) = button_colors(kind, theme);
    button_base(id, kind, false, theme)
        .gap_1()
        .pl(px(10.0))
        .child(heroicon(icon).text_color(fg))
        .child(label.into())
}

/// A button's shape and colors, without its content.
pub fn button_base(
    id: impl Into<SharedString>,
    kind: ButtonKind,
    focused: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let (bg, fg, border, hover) = button_colors(kind, theme);
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(BUTTON_H))
        .px(px(14.0))
        .rounded_lg()
        .border_2()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .when(focused, |b| b.shadow(vec![focus_shadow(theme)]))
        .hover(move |s| s.bg(hover).border_color(if border == bg { hover } else { border }))
}

/// A button kind's fill, text, border and hover fill.
fn button_colors(kind: ButtonKind, theme: &Theme) -> (Hsla, Hsla, Hsla, Hsla) {
    match kind {
        ButtonKind::Primary => (theme.brand, theme.text_on_color, theme.brand, theme.brand_hover),
        ButtonKind::Secondary => (theme.fill, theme.text_loud, theme.fill, theme.fill_strong),
        ButtonKind::Danger => (
            theme.error_fill,
            theme.text_on_color,
            theme.error_fill,
            theme.error_fill,
        ),
    }
}

/// The keyboard-focus ring: 2px of the brand outside the control.
pub fn focus_shadow(theme: &Theme) -> BoxShadow {
    BoxShadow {
        color: theme.focus_ring,
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(2.0),
        inset: false,
    }
}

/// An icon button's square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconButtonSize {
    /// 24px, 8px corners.
    Small,
    /// 32px, 12px corners.
    Default,
    /// 40px, 12px corners.
    Large,
}

impl IconButtonSize {
    pub fn px(self) -> f32 {
        match self {
            IconButtonSize::Small => 24.0,
            IconButtonSize::Default => 32.0,
            IconButtonSize::Large => 40.0,
        }
    }
}

/// A quiet icon button: the icon role at rest, a fill and loud icon on hover,
/// the fill held while `active`.
pub fn icon_button(
    id: impl Into<SharedString>,
    icon: Icon,
    size: IconButtonSize,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let color = if active { theme.text_loud } else { theme.icon };
    let hover_icon = theme.text_loud;
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .group("icon-button")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size.px()))
        .map(|d| match size {
            IconButtonSize::Small => d.rounded_lg(),
            _ => d.rounded_xl(),
        })
        .cursor_pointer()
        .when(active, |d| d.bg(theme.fill))
        .hover(|s| s.bg(theme.fill_strong))
        .child(
            heroicon(icon)
                .text_color(color)
                .group_hover("icon-button", move |s| s.text_color(hover_icon)),
        )
}

/// A chip: a short label on a hue's tint, in the hue's text color.
pub fn chip(label: impl Into<SharedString>, hue: Hue, theme: &Theme) -> Div {
    let (fg, bg) = theme.hue(hue);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .h(px(20.0))
        .px(px(6.0))
        .rounded_lg()
        .bg(bg)
        .text_color(fg)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(label.into())
}
