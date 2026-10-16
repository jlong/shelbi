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
//! - buttons semibold with a 1px boundary ([`button`]): primary the brand
//!   fill, secondary the control fill (a visible border in dark), danger the
//!   error fill, quiet only a hover fill;
//! - quiet icon buttons ([`icon_button`]) 24/32/40px on a hover fill;
//! - controls that open something ([`dropdown`]), segmented controls
//!   ([`segment_track`], [`segment`]) and the sidebar's search box
//!   ([`search_box`], with Taskpage's [`keycaps`]);
//! - a project's initials badge ([`project_badge`]);
//! - every hover and press fades over about 200ms ([`motion::fill_layer`]);
//! - menus, popovers and the palette raised with a ring and `shadow-lg`
//!   ([`popover`]);
//! - chips tinted from one hue ([`chip`]);
//! - the brand pill marking the keyboard-current row ([`selection_indicator`]).
//!
//! Spacing follows Taskpage's 8px scale (2, 4, 6, 8, 10, 12, 16, 20, 24...).

use gpui::{
    div, point, prelude::*, px, AnyView, App, BoxShadow, Div, FontWeight, Hsla,
    SharedString, Stateful, Window,
};

use super::motion::fill_layer;
use super::{hsla, palette, Hue, Theme};
use crate::assets::{heroicon, icons, Icon};

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

/// Tailwind's `shadow-xl` plus the 1px ring: a dialog over the scrim (the
/// palette, confirmations).
pub fn dialog_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(20.0), theme.shadow)
            .blur_radius(px(25.0))
            .spread_radius(px(-5.0)),
        BoxShadow::new(px(0.0), px(8.0), theme.shadow)
            .blur_radius(px(10.0))
            .spread_radius(px(-6.0)),
        ring_shadow(theme.ring),
    ]
}

/// Paint `el` as a dialog: a raised surface with `shadow-xl`.
pub fn dialog<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.surface_raised)
        .rounded_lg()
        .shadow(dialog_shadow(theme))
        .text_color(theme.text)
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
/// (`keyboard`, over a fill when it isn't also current), else a fill that
/// fades in on hover. `key` names the row for its fade. `chip_inline: false`
/// leaves the current row's chip to a sliding one ([`motion::SlideTrack`])
/// while it moves, keeping the row's loud label.
pub fn row_state(
    d: Stateful<Div>,
    key: impl Into<SharedString>,
    current: bool,
    keyboard: bool,
    chip_inline: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let d = d.relative();
    let d = if current {
        let d = d.text_color(theme.text_loud).font_weight(FontWeight::MEDIUM);
        if chip_inline {
            nav_chip(d, theme)
        } else {
            d
        }
    } else if keyboard {
        d.bg(theme.fill)
    } else {
        let clear = theme.fill.opacity(0.0);
        fill_layer(d, key, px(8.0), [clear, theme.fill, theme.fill_strong])
    };
    d.when(keyboard, |d| d.child(selection_indicator(theme)))
}

/// The sliding chip's paint: [`nav_chip`]'s, with its corners.
pub fn nav_chip_layer(d: Div, theme: &Theme) -> Div {
    nav_chip(d.rounded_lg(), theme)
}

/// A row in a menu or popover list (the project switcher): 32px, 8px
/// inside, the keyboard's row on the selection fill, others fading a fill in
/// on hover.
pub fn menu_row(id: impl Into<SharedString>, selected: bool, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(LIST_ROW_H + 4.0))
        .px_2()
        .rounded_lg()
        .cursor_pointer()
        .text_color(theme.text);
    if selected {
        d.bg(theme.row_selected).text_color(theme.text_loud)
    } else {
        let clear = theme.fill_subtle.opacity(0.0);
        fill_layer(d, id, px(8.0), [clear, theme.fill_subtle, theme.fill])
    }
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
    /// A low-key action beside others: no fill until hovered.
    Quiet,
}

/// A Taskpage button: semibold, 8px corners, a 1px boundary, 14px across.
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
    let fg = button_colors(kind, theme).text;
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
    let c = button_colors(kind, theme);
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(BUTTON_H))
        .px(px(14.0))
        .rounded_lg()
        .border_1()
        .border_color(c.border)
        .text_color(c.text)
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .when(focused, |b| b.shadow(vec![focus_shadow(theme)]))
        .hover(move |s| s.border_color(c.border_hover));
    fill_layer(d, id, px(7.0), [c.fill, c.hover, c.pressed])
}

/// A button kind's colors, in each state.
#[derive(Debug, Clone, Copy)]
pub struct ButtonColors {
    pub fill: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub text: Hsla,
    pub border: Hsla,
    pub border_hover: Hsla,
}

/// A button kind's fill, text and boundary at rest, hovered and pressed.
pub fn button_colors(kind: ButtonKind, theme: &Theme) -> ButtonColors {
    match kind {
        ButtonKind::Primary => ButtonColors {
            fill: theme.brand,
            hover: theme.brand_hover,
            pressed: theme.brand_hover,
            text: theme.text_on_color,
            border: theme.brand,
            border_hover: theme.brand_hover_border,
        },
        ButtonKind::Secondary => ButtonColors {
            fill: theme.control_fill,
            hover: theme.control_fill_hover,
            pressed: theme.control_fill_pressed,
            text: theme.text_loud,
            border: theme.control_border,
            border_hover: theme.control_border_hover,
        },
        ButtonKind::Danger => ButtonColors {
            fill: theme.error_fill,
            hover: theme.error_fill.opacity(0.9),
            pressed: theme.error_fill.opacity(0.8),
            text: theme.text_on_color,
            border: theme.danger_border,
            border_hover: theme.danger_border,
        },
        ButtonKind::Quiet => ButtonColors {
            fill: theme.control_fill.opacity(0.0),
            hover: theme.control_fill_hover,
            pressed: theme.control_fill_pressed,
            text: theme.text,
            border: theme.control_border.opacity(0.0),
            border_hover: theme.control_border,
        },
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

/// A quiet icon button: the icon role at rest, a fill fading in and a loud
/// icon on hover, the fill held while `active`.
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
    let radius = match size {
        IconButtonSize::Small => px(8.0),
        _ => px(12.0),
    };
    let rest = if active {
        theme.control_fill
    } else {
        theme.control_fill.opacity(0.0)
    };
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .group(id.clone())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size.px()))
        .rounded(radius)
        .cursor_pointer();
    let group = id.clone();
    fill_layer(d, id, radius, [rest, theme.control_fill_hover, theme.control_fill_pressed]).child(
        heroicon(icon)
            .text_color(color)
            .group_hover(group, move |s| s.text_color(hover_icon)),
    )
}

/// A control that opens a menu (the board's filters): the control fill and
/// boundary, an icon, the label and a chevron; pressed while `open`.
pub fn dropdown(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let (rest, border) = if open {
        (theme.control_fill_pressed, theme.control_border_hover)
    } else {
        (theme.control_fill, theme.control_border)
    };
    let border_hover = theme.control_border_hover;
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .h(px(BUTTON_H))
        .pl(px(8.0))
        .pr(px(6.0))
        .rounded_lg()
        .border_1()
        .border_color(border)
        .hover(move |s| s.border_color(border_hover))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_loud)
        .cursor_pointer();
    fill_layer(d, id, px(7.0), [rest, theme.control_fill_hover, theme.control_fill_pressed])
        .child(heroicon(icon).text_color(theme.icon))
        .child(div().whitespace_nowrap().child(label.into()))
        .child(heroicon(icons::CHEVRON_DOWN).text_color(if open { theme.text_loud } else { theme.icon }))
}

/// A segmented control's track: the control fill, 2px inside. Its segments
/// go in as children ([`segment`]); give it `.relative()` children-first
/// order with [`motion::origin_probe`] and, while one slides,
/// [`motion::floating_chip`] painted by [`segment_chip`].
pub fn segment_track(theme: &Theme) -> Div {
    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .p(px(2.0))
        .rounded_lg()
        .bg(theme.control_fill)
        .border_1()
        .border_color(theme.control_border)
}

/// One segment: 24px, its label medium and normal; selected, it is lifted
/// ([`segment_chip`], unless `chip_inline` is false while a sliding chip
/// stands in) with a loud semibold label. Unselected, a fill fades in on
/// hover.
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
        .justify_center()
        .h(px(24.0))
        .px(px(10.0))
        .rounded(px(6.0))
        .text_xs()
        .cursor_pointer();
    let d = if selected {
        let d = d
            .text_color(theme.text_loud)
            .font_weight(FontWeight::SEMIBOLD);
        if chip_inline {
            segment_chip(d, theme)
        } else {
            d
        }
    } else {
        let clear = theme.control_fill_hover.opacity(0.0);
        fill_layer(
            d.text_color(theme.text).font_weight(FontWeight::MEDIUM),
            id,
            px(6.0),
            [clear, theme.control_fill_hover, theme.control_fill_pressed],
        )
    };
    d.child(label.into())
}

/// The selected segment's lift: the tab surface, its ring and Taskpage's
/// `shadow-nav`.
pub fn segment_chip<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.tab_selected).rounded(px(6.0)).shadow(vec![
        BoxShadow::new(px(0.0), px(1.0), theme.nav_shadow),
        ring_shadow(theme.tab_selected_ring),
    ])
}

/// Taskpage's `Shortcut`: one small keycap per key of `chord` (`⌘K` is ⌘
/// and K), 12px medium, a 1px bottom edge.
pub fn keycaps(chord: &str, theme: &Theme) -> Div {
    let keys = split_chord(chord);
    div().flex().flex_row().flex_none().gap(px(2.0)).children(keys.into_iter().map(|key| {
        div()
            .flex()
            .items_center()
            .justify_center()
            .min_w(px(18.0))
            .h(px(18.0))
            .px(px(4.0))
            .rounded(px(5.0))
            .bg(theme.keycap)
            .shadow(vec![BoxShadow::new(px(0.0), px(1.0), theme.keycap_edge)])
            .text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.text)
            .child(key)
    }))
}

/// A chord's keys as macOS spells them: each modifier glyph its own cap,
/// then the key (`⌃⇧P` → `⌃`, `⇧`, `P`).
pub fn split_chord(chord: &str) -> Vec<String> {
    const MODIFIERS: [char; 5] = ['⌘', '⌃', '⌥', '⇧', '⇪'];
    let mut keys = Vec::new();
    let mut rest = chord;
    while let Some(c) = rest.chars().next() {
        if MODIFIERS.contains(&c) {
            keys.push(c.to_string());
            rest = &rest[c.len_utf8()..];
        } else {
            break;
        }
    }
    if !rest.is_empty() {
        keys.push(rest.to_uppercase());
    }
    keys
}

/// Taskpage's `SearchBox`: the control fill (12px corners) fading to its
/// hover, a magnifying glass, "Search" growing to fill, and the shortcut's
/// keycaps.
pub fn search_box(id: impl Into<SharedString>, chord: &str, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .w_full()
        .h(px(32.0))
        .pl(px(8.0))
        .pr(px(6.0))
        .rounded_xl()
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .cursor_pointer();
    fill_layer(d, id, px(11.0), [theme.control_fill, theme.control_fill_hover, theme.control_fill_pressed])
        .child(heroicon(icons::MAGNIFYING_GLASS).text_color(theme.icon))
        .child(div().flex_1().child("Search"))
        .when(!chord.is_empty(), |d| d.child(keycaps(chord, theme)))
}

/// A project's badge: its two-letter initials, bold and white, on a hue
/// picked from its name ([`palette::badge_fill`]); `size` square with 6px
/// corners (8px at 32px and up).
pub fn project_badge(name: &str, size: f32) -> Div {
    let initials: String = initials(name);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(if size >= 32.0 { 8.0 } else { 6.0 }))
        .bg(hsla(palette::badge_fill(name)))
        .text_color(gpui::white())
        .text_size(px((size * 0.42).round()))
        .font_weight(FontWeight::BOLD)
        .child(initials)
}

/// A name's initials: the first letters of its first two words (split at
/// separators and camel case), else its first two letters
/// (`context-store` and `ContextStore` → `CS`, `shelbi` → `SH`).
pub fn initials(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            prev_lower = false;
            words.push(String::new());
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(String::new());
        }
        match words.last_mut() {
            Some(w) => w.push(c),
            None => words.push(c.to_string()),
        }
        prev_lower = c.is_lowercase();
    }
    let words: Vec<&str> = words.iter().map(String::as_str).filter(|w| !w.is_empty()).collect();
    let picked: String = match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    picked.to_uppercase()
}

/// An empty state: a quiet sentence saying why there's nothing here, and
/// the action that would change it, centered in the space.
pub fn empty_state(
    selector: &'static str,
    message: impl Into<SharedString>,
    action: Option<Stateful<Div>>,
    theme: &Theme,
) -> Div {
    div()
        .debug_selector(move || selector.into())
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_3()
        .p_6()
        .child(
            div()
                .max_w(px(360.0))
                .text_center()
                .text_color(theme.text_quiet)
                .child(message.into()),
        )
        .children(action)
}

/// Where a control that opens a menu last painted, so its menu can sit
/// under it.
pub type Anchor = std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>;

/// An invisible element filling its parent that records the parent's bounds
/// into `anchor` each frame. The parent needs `.relative()`.
pub fn anchor_probe(anchor: &Anchor) -> impl IntoElement {
    let anchor = anchor.clone();
    gpui::canvas(move |bounds, _, _| anchor.set(Some(bounds)), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// `menu` sliding up 8px into place and fading in over [`motion::MENU_IN`]
/// on Taskpage's curve, each time it mounts (a menu is drawn only while
/// open, so every open slides). `id` names the menu.
pub fn menu_entrance(id: &'static str, menu: impl IntoElement + 'static) -> impl IntoElement {
    use gpui::{Animation, AnimationExt};
    div()
        .relative()
        .child(menu)
        .with_animation(
            id,
            Animation::new(super::motion::MENU_IN).with_easing(super::motion::ease_out_expo),
            |d, t| d.top(px((1.0 - t) * super::motion::SLIDE_PX)).opacity(t),
        )
}

/// `menu` 6px under the control `anchor` measured, its left edges aligned,
/// kept 8px inside the window; where the control hasn't painted yet, at
/// `fallback` (window coordinates).
pub fn below(anchor: &Anchor, fallback: gpui::Point<gpui::Pixels>, menu: impl IntoElement) -> impl IntoElement {
    let at = anchor
        .get()
        .map(|b| point(b.left(), b.bottom() + px(6.0)))
        .unwrap_or(fallback);
    gpui::anchored()
        .position(at)
        .snap_to_window_with_margin(px(8.0))
        .child(menu)
}

/// A tooltip with `text`, for gpui's `.tooltip(...)`: a small popover.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| Tooltip(text.clone())).into()
}

struct Tooltip(SharedString);

impl gpui::Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        popover(div(), &theme)
            .font_family("Geist")
            .px_2()
            .py_1()
            .text_xs()
            .max_w(px(360.0))
            .child(self.0.clone())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_two_words_or_two_letters() {
        assert_eq!(initials("shelbi"), "SH");
        assert_eq!(initials("Shelbi"), "SH");
        assert_eq!(initials("ContextStore"), "CS");
        assert_eq!(initials("context-store"), "CS");
        assert_eq!(initials("my cool project"), "MC");
        assert_eq!(initials("x"), "X");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn chords_split_into_keycaps() {
        assert_eq!(split_chord("⌃P"), ["⌃", "P"]);
        assert_eq!(split_chord("⌘⇧k"), ["⌘", "⇧", "K"]);
        assert_eq!(split_chord("Esc"), ["ESC"]);
    }
}
