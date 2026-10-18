//! The Figma design system's text styles ([`Type`]) and the component
//! shapes every view shares, in the palette's roles.
//!
//! One place for the shapes every view shares, so a button or a selected row
//! reads the same in the sidebar, a panel, the board and an overlay:
//!
//! - text in the Figma styles ([`Type`], applied with
//!   [`Typography::type_style`]): Base 14/18, Small 13/18, X-Small 12/16,
//!   Heading 20/28, all Geist;
//! - 8px corners everywhere ([`gpui::Styled::rounded_lg`]), 12px on the
//!   larger icon buttons;
//! - nav rows 40px tall with the content 8px inside, labels medium weight,
//!   the selected row a raised chip ([`nav_chip`]: the canvas with Figma's
//!   `shadow/raised card`);
//! - buttons medium weight on the overlay fill, no boundary ([`button`]):
//!   the Figma Button component's Default (`theme.background`), Success
//!   (green tint, `text/green`) and Danger (pink tint) styles, quiet only a
//!   hover fill;
//! - quiet icon buttons ([`icon_button`]) 24/32/40px on a hover fill;
//! - toolbar actions ([`toolbar_button`]) and the icon slot Figma lays a
//!   button's icon in ([`icon_slot`]);
//! - a workflow status's glyph ([`status_icon`]) and heading
//!   ([`status_heading`]: the board's column headers, the task panels);
//! - controls that open something ([`dropdown`]), segmented controls
//!   ([`filter_track`], [`choice_track`]) and the sidebar's search box
//!   ([`search_box`], with the Hotkey [`keycaps`]);
//! - a project's initials badge ([`project_badge`]);
//! - every hover and press fades over about 200ms ([`motion::fill_layer`]);
//! - menus, popovers and the palette raised with a ring and a soft drop
//!   ([`popover`]);
//! - chips tinted from one hue ([`chip`]);
//! - the accent pill marking the keyboard-current row
//!   ([`selection_indicator`]);
//! - the keyboard focus as Figma draws it: a 1px `focus/default/ring` and a
//!   4px `focus/default/focus-glow` ([`focus_shadow`]).
//!
//! Spacing follows Figma's scale (2, 4, 6, 8, 12, 16).

use gpui::{
    div, point, prelude::*, px, AnyView, App, BoxShadow, Div, FontWeight, Hsla,
    SharedString, Stateful, Svg, Window,
};
use shelbi_core::StatusCategory;

use super::motion::fill_layer;
use super::{hsla, palette, Hue, Theme};
use crate::assets::{heroicon, icons, status_glyph, Icon, StatusGlyph};

/// The UI typeface every [`Type`] is set in.
pub const UI_FAMILY: &str = "Geist";

/// The Figma text styles (`Desktop/...`), all Geist. Views name one of these
/// rather than a size, a line height and a weight apiece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// `Desktop/Heading`: 20 / 28, bold. A page or dialog title.
    Heading,
    /// `Desktop/Base/Regular`: 14 / 18. Body text, nav labels, card titles'
    /// descriptions in a dialog.
    Base,
    /// `Desktop/Base/Medium`: 14 / 18, 500. The selected nav label, card
    /// titles.
    BaseMedium,
    /// `Desktop/Base/Semibold`: 14 / 18, 600. The palette's current row.
    BaseSemibold,
    /// `Desktop/Small/Regular`: 13 / 18. Sidebar rows.
    Small,
    /// `Desktop/Small/Medium`: 13 / 18, 500. Section headers, button labels.
    SmallMedium,
    /// `Desktop/X-Small/Regular`: 12 / 16. Metadata, hotkeys, hints.
    XSmall,
    /// `Desktop/X-Small/Medium`: 12 / 16, 500. Chips, compact buttons.
    XSmallMedium,
    /// `Desktop/X-Small/Semibold`: 12 / 16, 600. A workspace's agent.
    XSmallSemibold,
}

impl Type {
    /// The font size in points.
    pub fn size(self) -> f32 {
        match self {
            Type::Heading => 20.0,
            Type::Base | Type::BaseMedium | Type::BaseSemibold => 14.0,
            Type::Small | Type::SmallMedium => 13.0,
            Type::XSmall | Type::XSmallMedium | Type::XSmallSemibold => 12.0,
        }
    }

    /// The line height in points.
    pub fn line_height(self) -> f32 {
        match self {
            Type::Heading => 28.0,
            Type::Base | Type::BaseMedium | Type::BaseSemibold | Type::Small | Type::SmallMedium => 18.0,
            Type::XSmall | Type::XSmallMedium | Type::XSmallSemibold => 16.0,
        }
    }

    pub fn weight(self) -> FontWeight {
        match self {
            Type::Base | Type::Small | Type::XSmall => FontWeight::NORMAL,
            Type::BaseMedium | Type::SmallMedium | Type::XSmallMedium => FontWeight::MEDIUM,
            Type::BaseSemibold | Type::XSmallSemibold => FontWeight::SEMIBOLD,
            Type::Heading => FontWeight::BOLD,
        }
    }
}

/// Set an element's text in a [`Type`]: Geist at the style's size, line
/// height and weight.
pub trait Typography: Styled + Sized {
    fn type_style(self, ty: Type) -> Self {
        self.font_family(UI_FAMILY)
            .text_size(px(ty.size()))
            .line_height(px(ty.line_height()))
            .font_weight(ty.weight())
    }
}

impl<E: Styled> Typography for E {}

/// A nav row's height. The chip inside is inset 2px top
/// and bottom.
pub const NAV_ROW_H: f32 = 40.0;
/// A list row's height in the leading column (workspaces, machine groups,
/// project rows): a 32px control height, so a long list stays
/// scannable.
pub const LIST_ROW_H: f32 = 32.0;
/// A button's height at the medium size.
pub const BUTTON_H: f32 = 32.0;

/// Figma's `shadow/raised card`: three soft drops (0 4 4, 0 11 0 -8,
/// 0 3 0 -1) and a 1px ring. The selected nav chip and the board's cards.
pub fn raised_card_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(4.0), theme.shadow).blur_radius(px(4.0)),
        BoxShadow::new(px(0.0), px(11.0), theme.shadow).spread_radius(px(-8.0)),
        BoxShadow::new(px(0.0), px(3.0), theme.shadow).spread_radius(px(-1.0)),
        ring_shadow(theme.shadow_ring),
    ]
}

/// A soft drop plus a 1px ring, for a menu or popover.
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

/// A deeper drop plus the 1px ring: a dialog over the scrim (the palette,
/// confirmations).
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

/// Paint `el` as a dialog: a raised surface with the deeper drop.
pub fn dialog<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.raised)
        .rounded_lg()
        .shadow(dialog_shadow(theme))
        .text_color(theme.foreground)
}

/// A card's resting elevation: Figma's `shadow/raised card`.
pub fn card_shadow(theme: &Theme) -> Vec<BoxShadow> {
    raised_card_shadow(theme)
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
    el.bg(theme.nav_selected).shadow(raised_card_shadow(theme))
}

/// Paint `el` as a raised surface: a menu, a popover, the palette, a dialog.
pub fn popover<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.raised)
        .rounded_lg()
        .shadow(popover_shadow(theme))
        .text_color(theme.foreground)
}

/// The accent pill at a row's leading edge marking the keyboard-current row.
/// Absolutely positioned: the row needs `.relative()`.
pub fn selection_indicator(theme: &Theme) -> Div {
    div()
        .absolute()
        .left(px(-6.0))
        .top_0()
        .bottom_0()
        .flex()
        .items_center()
        .child(div().w(px(3.0)).h(px(16.0)).rounded_full().bg(theme.accent))
}

/// Paint a row in a nav list by its state: the raised chip while it is the
/// current page (`current`), the accent pill while it is the keyboard's row
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
        d.bg(theme.background)
    } else {
        let clear = theme.background.opacity(0.0);
        fill_layer(d, key, px(8.0), [clear, theme.background, theme.background_hover])
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
        .type_style(Type::Base)
        .text_color(theme.foreground);
    if selected {
        d.bg(theme.row_selected).text_color(theme.text_loud)
    } else {
        let clear = theme.background.opacity(0.0);
        fill_layer(d, id, px(8.0), [clear, theme.background, theme.background_hover])
    }
}

/// A section heading: Base medium, loud.
pub fn heading(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .type_style(Type::BaseMedium)
        .text_color(theme.text_loud)
        .child(text.into())
}

/// A button's look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one main action: the Button component's Success style (a green
    /// tint, `text/green`). Figma draws no solid brand button.
    Primary,
    /// Every other action: the Default style (`theme.background`, loud
    /// text).
    Secondary,
    /// A destructive action: the Danger style (a pink tint).
    Danger,
    /// A low-key action beside others: no fill until hovered.
    Quiet,
}

/// A Figma button (Small): 13 / 18 medium, 6px corners, 12px across, the
/// style's fill and no boundary. `focused` draws the focus ring and glow
/// (keyboard focus in a dialog).
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
        .px(px(12.0))
        .rounded(px(6.0))
        .text_color(c.text)
        .type_style(Type::SmallMedium)
        .cursor_pointer()
        .when(focused, |b| b.shadow(focus_shadow(theme)));
    fill_layer(d, id, px(6.0), [c.fill, c.hover, c.pressed])
}

/// A button kind's colors, in each state.
#[derive(Debug, Clone, Copy)]
pub struct ButtonColors {
    pub fill: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub text: Hsla,
}

/// A button kind's fill and text at rest, hovered and pressed: the Button
/// component's `background`, `background-hover` and `background-active`.
pub fn button_colors(kind: ButtonKind, theme: &Theme) -> ButtonColors {
    match kind {
        ButtonKind::Primary => ButtonColors {
            fill: theme.success_fill,
            hover: theme.success_fill_hover,
            pressed: theme.success_fill_active,
            text: theme.success,
        },
        ButtonKind::Secondary => ButtonColors {
            fill: theme.background,
            hover: theme.background_hover,
            pressed: theme.background_active,
            text: theme.text_loud,
        },
        ButtonKind::Danger => ButtonColors {
            fill: theme.danger_fill,
            hover: theme.danger_fill_hover,
            pressed: theme.danger_fill_active,
            text: theme.error,
        },
        ButtonKind::Quiet => ButtonColors {
            fill: theme.background.opacity(0.0),
            hover: theme.background_hover,
            pressed: theme.background_active,
            text: theme.foreground,
        },
    }
}

/// The keyboard focus as Figma draws it: a 4px `focus/default/focus-glow`
/// and, over it, the 1px `focus/default/ring`, both outside the control.
pub fn focus_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let around = |color: Hsla, spread: f32| BoxShadow {
        color,
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(spread),
        inset: false,
    };
    vec![around(theme.focus_glow, 5.0), around(theme.focus_ring, 1.0)]
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
        theme.background
    } else {
        theme.background.opacity(0.0)
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
    fill_layer(d, id, radius, [rest, theme.background_hover, theme.background_active]).child(
        heroicon(icon)
            .text_color(color)
            .group_hover(group, move |s| s.text_color(hover_icon)),
    )
}

/// Figma's Icon slot: `icon` in a box `width` wide, flush right, so a 20px
/// icon hangs past the slot's left edge and the label after it sits where
/// Figma puts it (the slot is 14px in an X-Small button, 12px in a Small
/// one).
pub fn icon_slot(icon: Icon, width: f32, color: Hsla) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .w(px(width))
        .child(heroicon(icon).text_color(color))
}

/// A toolbar action (the board's New issue): the Figma Button at Small,
/// Default style, 28px tall, with a left icon.
pub fn toolbar_button(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let fg = button_colors(ButtonKind::Secondary, theme).text;
    button_base(id, ButtonKind::Secondary, false, theme)
        .h(px(28.0))
        .gap(px(4.0))
        .child(icon_slot(icon, 12.0, fg))
        .child(label.into())
}

/// A control that opens a menu (the board's and the feed's filters): the
/// Figma Button at X-Small with a left icon (24px tall, 8px across, the
/// overlay fill, X-Small/Medium loud); pressed while `open`.
pub fn dropdown(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let rest = if open {
        theme.background_active
    } else {
        theme.background
    };
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.0))
        .h(px(24.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .type_style(Type::XSmallMedium)
        .text_color(theme.text_loud)
        .cursor_pointer();
    fill_layer(d, id, px(6.0), [rest, theme.background_hover, theme.background_active])
        .child(icon_slot(icon, 14.0, if open { theme.text_loud } else { theme.icon }))
        .child(div().whitespace_nowrap().child(label.into()))
}

/// A workflow status's glyph ([`StatusGlyph`]) in its Figma color: backlog
/// and to do in the icon gray, in progress in the accent (`icon/select`),
/// review purple, done green, canceled muted.
pub fn status_icon(category: StatusCategory, theme: &Theme) -> Svg {
    let (glyph, color) = match category {
        StatusCategory::Backlog => (StatusGlyph::Backlog, theme.icon),
        StatusCategory::Ready => (StatusGlyph::Todo, theme.icon),
        StatusCategory::Active => (StatusGlyph::InProgress, theme.accent),
        StatusCategory::Handoff => (StatusGlyph::Review, theme.icon_purple),
        StatusCategory::Done => (StatusGlyph::Done, theme.icon_green),
        StatusCategory::Archived => (StatusGlyph::Canceled, theme.muted),
    };
    status_glyph(glyph).text_color(color)
}

/// A status heading, as the board's column headers and the task panels draw
/// it: the status glyph, then the label in caps at X-Small/Semibold, loud
/// when `current` (the keyboard's column) and muted otherwise.
pub fn status_heading(
    category: StatusCategory,
    label: impl Into<SharedString>,
    current: bool,
    theme: &Theme,
) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .child(status_icon(category, theme))
        .child(
            div()
                .whitespace_nowrap()
                .type_style(Type::XSmallSemibold)
                .text_color(if current { theme.text_loud } else { theme.muted })
                .child(label.into().to_uppercase()),
        )
}

/// The selected segment: `theme.selection` (Activity's and Settings'
/// segmented controls).
pub fn segment_chip<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.selection).rounded(px(6.0))
}

/// The Activity frame's kind filter: its segments in a bare row, 4px apart,
/// with no track behind them. Its segments go in as children, after
/// [`motion::origin_probe`] and, while one slides, a
/// [`motion::floating_chip`] painted by [`segment_chip`].
pub fn filter_track() -> Div {
    div().relative().flex().flex_row().items_center().gap(px(4.0))
}

/// One kind filter: X-Small, 8px in, 24px tall; muted at rest, loud on the
/// selected chip ([`segment_chip`], unless `chip_inline` is false while a
/// sliding chip stands in). Unselected, a fill fades in on hover.
pub fn filter_segment(
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
        .px(px(8.0))
        .rounded(px(6.0))
        .type_style(Type::XSmall)
        .cursor_pointer();
    let d = if selected {
        let d = d.text_color(theme.text_loud);
        if chip_inline {
            segment_chip(d, theme)
        } else {
            d
        }
    } else {
        let clear = theme.background_hover.opacity(0.0);
        fill_layer(
            d.text_color(theme.muted),
            id,
            px(6.0),
            [clear, theme.background_hover, theme.background_active],
        )
    };
    d.child(label.into())
}

/// The Activity frame's pickers: `theme.search`'s fill, the icon and an
/// X-Small gray label, 24px tall and no chevron; pressed while `open`.
pub fn filter_dropdown(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let rest = if open {
        theme.background_active
    } else {
        theme.search
    };
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .h(px(24.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .type_style(Type::XSmall)
        .text_color(theme.gray)
        .cursor_pointer();
    fill_layer(d, id, px(6.0), [rest, theme.background_active, theme.selection])
        .child(heroicon(icon).text_color(theme.icon))
        .child(div().whitespace_nowrap().child(label.into()))
}

/// Settings' segmented control, as the Settings frame draws it: no track,
/// the choices 4px apart. Its choices go in as children ([`choice`]), with
/// the same sliding-chip probes as a [`filter_track`].
pub fn choice_track() -> Div {
    div().relative().flex().flex_row().items_center().gap(px(4.0))
}

/// One choice of a [`choice_track`]: 26px, 12px across, 6px corners, Base
/// in `form_text`; selected, Base medium and loud on the [`segment_chip`]
/// (unless `chip_inline` is false while a sliding chip stands in).
/// Unselected, a fill fades in on hover.
pub fn choice(
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
        .h(px(26.0))
        .px(px(12.0))
        .rounded(px(6.0))
        .cursor_pointer();
    let d = if selected {
        let d = d.type_style(Type::BaseMedium).text_color(theme.text_loud);
        if chip_inline {
            segment_chip(d, theme)
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

/// The width of a settings field (the Settings frame's text fields).
pub const FIELD_W: f32 = 360.0;

/// Paint `el` as a settings field: 360 x 32, 10px across, 6px corners, the
/// canvas inside a 1px divider, its text Base in `form_text`. A gpui-kit
/// `Input` takes it too (it refines its own style with this).
pub fn field<E: Styled>(el: E, theme: &Theme) -> E {
    el.w(px(FIELD_W))
        .h(px(BUTTON_H))
        .px(px(10.0))
        .rounded(px(6.0))
        .bg(theme.well)
        .border_1()
        .border_color(theme.input_border)
        .type_style(Type::Base)
        .text_color(theme.form_text)
}

/// A button in a settings form, as the Settings frame draws "Open
/// keys.yaml": `theme.search`, 26px, 12px across, 6px corners, Base in
/// `form_text`. Hover and press fade to the stronger overlay fills.
pub fn form_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .h(px(26.0))
        .px(px(12.0))
        .rounded(px(6.0))
        .type_style(Type::Base)
        .text_color(theme.form_text)
        .cursor_pointer();
    fill_layer(d, id, px(6.0), [theme.search, theme.background_active, theme.selection])
        .child(label.into())
}

/// The Button component's Hotkey: one small keycap per key of `chord` (`⌘K`
/// is ⌘ and K), X-Small in gray, on the canvas with a 1px bottom edge.
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
            .rounded(px(4.0))
            .bg(theme.keycap)
            .shadow(vec![BoxShadow::new(px(0.0), px(1.0), theme.keycap_edge)])
            .type_style(Type::XSmall)
            .text_color(theme.gray)
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

/// The sidebar's search box: the overlay fill (12px corners) fading to its
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
        .type_style(Type::Base)
        .text_color(theme.foreground)
        .cursor_pointer();
    fill_layer(d, id, px(12.0), [theme.background, theme.background_hover, theme.background_active])
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
                .text_color(theme.muted)
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
/// on the motion curve, each time it mounts (a menu is drawn only while
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
            .px_2()
            .py_1()
            .type_style(Type::XSmall)
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
        .type_style(Type::XSmallMedium)
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
