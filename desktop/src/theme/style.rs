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
//! - nav rows painted by state ([`row_state_rounded`]), the selected row a
//!   raised chip ([`nav_chip`]: the canvas with Figma's `shadow/raised
//!   card`);
//! - buttons medium weight on the overlay fill, no boundary ([`button`]):
//!   the Figma Button component's Default (`theme.background`), Success
//!   (green tint, `text/green`) and Danger (pink tint) styles, quiet only a
//!   hover fill;
//! - quiet icon buttons ([`icon_button`]) 24/32px on a hover fill;
//! - toolbar actions ([`toolbar_button`], [`small_icon_button`]), the
//!   X-Small button ([`xsmall_button`]) and the icon slot Figma lays a
//!   button's icon in ([`icon_slot`]);
//! - a workflow status's glyph ([`status_icon`]) and heading
//!   ([`status_heading`]: the board's column headers, the task panels);
//! - controls that open something ([`dropdown`]), the Activity filter's
//!   segments ([`filter_track`]); the form controls (segmented control,
//!   textbox, picker, checkbox) are `elements::controls`;
//! - the sidebar's (the Navigation component's) 6px corners
//!   ([`NAV_RADIUS`]) and plain-text shortcut hints ([`hotkey`]);
//! - a project's initials badge ([`project_badge`]);
//! - every hover and press fades over about 200ms ([`motion::fill_layer`]);
//! - menus, popovers and the palette raised with a ring and a soft drop
//!   ([`popover`]);
//! - the keyboard focus as Figma draws it: a 1px `focus/default/ring` and a
//!   4px `focus/default/focus-glow` ([`focus_shadow`]).
//!
//! Spacing follows Figma's scale (2, 4, 6, 8, 12, 16).

use gpui::{
    div, point, prelude::*, px, AnyView, App, BoxShadow, Div, FontWeight, Hsla, Pixels,
    SharedString, Stateful, Svg, Window,
};
use shelbi_core::StatusCategory;

use super::motion::fill_layer;
use super::{hsla, palette, Theme};
use crate::assets::{heroicon, icons, status_glyph, Icon, StatusGlyph};
use crate::elements::exact_text::exact_text;

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

/// A list row's height in the leading column (workspaces, machine groups,
/// project rows): a 32px control height, so a long list stays
/// scannable.
pub const LIST_ROW_H: f32 = 32.0;
/// A button's height at the medium size.
pub const BUTTON_H: f32 = 32.0;

/// A Figma (or CSS) blur as gpui's `blur_radius`: Figma's blur is twice the
/// Gaussian's sigma, and gpui's `blur_radius` is the sigma itself, so a
/// Figma value passed straight through draws twice as soft.
pub fn figma_blur(blur: f32) -> Pixels {
    px(blur / 2.0)
}

/// Figma's `shadow/raised card`: three soft drops (0 4 4, 0 11 0 -8,
/// 0 3 0 -1) and a 1px ring. The selected nav chip and the board's cards.
pub fn raised_card_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let mut shadow = raised_drops(theme);
    shadow.push(ring_shadow(theme.shadow_ring));
    shadow
}

/// `shadow/raised card`'s three drops without its ring: a segmented
/// control's selected item.
pub fn raised_drops(theme: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(4.0), theme.shadow).blur_radius(figma_blur(4.0)),
        BoxShadow::new(px(0.0), px(11.0), theme.shadow).spread_radius(px(-8.0)),
        BoxShadow::new(px(0.0), px(3.0), theme.shadow).spread_radius(px(-1.0)),
    ]
}

/// A soft drop plus a 1px ring, for a menu or popover (CSS's `shadow-lg`
/// values, whose blurs are Figma's kind).
pub fn popover_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(10.0), theme.shadow)
            .blur_radius(figma_blur(15.0))
            .spread_radius(px(-3.0)),
        BoxShadow::new(px(0.0), px(4.0), theme.shadow)
            .blur_radius(figma_blur(6.0))
            .spread_radius(px(-4.0)),
        ring_shadow(theme.ring),
    ]
}

/// A deeper drop plus the 1px ring: a dialog over the scrim (the palette,
/// confirmations).
pub fn dialog_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(20.0), theme.shadow)
            .blur_radius(figma_blur(25.0))
            .spread_radius(px(-5.0)),
        BoxShadow::new(px(0.0), px(8.0), theme.shadow)
            .blur_radius(figma_blur(10.0))
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

/// Paint `el` as the selected nav row: the canvas chip, lifted with the
/// raised-card shadow in Light (the Navigation component draws the dark chip
/// flat).
pub fn nav_chip<E: Styled>(el: E, theme: &Theme) -> E {
    let el = el.bg(theme.nav_selected);
    if theme.is_dark() {
        el
    } else {
        el.shadow(raised_card_shadow(theme))
    }
}

/// Paint `el` as a raised surface: a menu, a popover, the palette, a dialog.
pub fn popover<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(theme.raised)
        .rounded_lg()
        .shadow(popover_shadow(theme))
        .text_color(theme.foreground)
}

/// Paint a row in a nav list by its state: the raised chip while it is the
/// current page (`current`), the selection fill while it is the keyboard's
/// row (`keyboard`) and not also current, else a fill that fades in on hover. `key` names the row for its fade, `radius` its corners
/// (the Navigation component's are [`NAV_RADIUS`]). `chip_inline: false`
/// leaves the current row's chip to a sliding one ([`motion::SlideTrack`])
/// while it moves, keeping the row's loud label.
pub fn row_state_rounded(
    d: Stateful<Div>,
    key: impl Into<SharedString>,
    current: bool,
    keyboard: bool,
    chip_inline: bool,
    radius: Pixels,
    theme: &Theme,
) -> Stateful<Div> {
    let d = d.relative();
    if current {
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
        fill_layer(d, key, radius, [clear, theme.background, theme.background_hover])
    }
}

/// The Navigation component's corners: its rows, chip and buttons.
pub const NAV_RADIUS: f32 = 6.0;

/// The Navigation component's Hotkey: a chord's keys as plain text glyphs
/// (`⌘O` is ⌘ and O, 2px apart), X-Small in gray, 4px either side. No
/// keycaps. Each key is laid out at its exact width ([`exact_text`]), as
/// Figma does; the modifier glyphs, which Geist lacks, are the system
/// font's, as in the frames (CoreText's own fallback for Geist draws a
/// wider, heavier ⌘).
pub fn hotkey(chord: &str, theme: &Theme) -> Div {
    hotkey_keys(split_chord(chord), theme).px(px(4.0))
}

/// [`hotkey`] for keys already split (each modifier glyph, then the key),
/// without its 4px either side.
fn hotkey_keys(keys: Vec<String>, theme: &Theme) -> Div {
    hotkey_row(theme).children(keys.into_iter().map(|key| {
        let modifier = key.chars().all(|c| MODIFIERS.contains(&c));
        let text = exact_text(key);
        if modifier {
            text.family(SYSTEM_FAMILY)
        } else {
            text
        }
    }))
}

/// A Keybindings keycap's label (frame 2252:28172): [`hotkey_keys`] with
/// each modifier glyph in the face Figma's fallback draws it in there. `⌃`
/// and `⌥` are Apple Symbols' (SF's are wider, and set a keycap 4pt wider
/// than the frame's), `⇧` Geist's own fallback, `⌘` the system font's, as
/// the sidebar's.
pub fn keycap_label(keys: Vec<String>, theme: &Theme) -> Div {
    hotkey_row(theme).children(keys.into_iter().map(|key| {
        let text = exact_text(key.clone());
        match key.as_str() {
            "⌃" | "⌥" => text.family(SYMBOLS_FAMILY),
            "⌘" => text.family(SYSTEM_FAMILY),
            _ => text,
        }
    }))
}

fn hotkey_row(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(2.0))
        .type_style(Type::XSmall)
        .text_color(theme.gray)
}

/// macOS's symbol font, whose `⌃` and `⌥` match the Figma keycaps'.
const SYMBOLS_FAMILY: &str = "Apple Symbols";

/// gpui's name for the system UI font (SF on macOS).
const SYSTEM_FAMILY: &str = ".SystemUIFont";

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
    /// A set filter: the Select style (a cyan tint, `button/select/text`).
    Select,
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

/// The Button component with Disabled on: its fill and label at
/// `button/disabled/opacity` (40%), no hover or press, no pointer.
pub fn disabled_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let c = button_colors(kind, theme);
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(BUTTON_H))
        .px(px(12.0))
        .rounded(px(6.0))
        .bg(c.fill)
        .text_color(c.text)
        .type_style(Type::SmallMedium)
        .opacity(0.4)
        .child(label.into())
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
            text: theme.success_label,
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
        ButtonKind::Select => ButtonColors {
            fill: theme.select_fill,
            hover: theme.select_fill_hover,
            pressed: theme.select_fill_active,
            text: theme.select_label,
        },
        ButtonKind::Quiet => ButtonColors {
            fill: theme.background.opacity(0.0),
            hover: theme.background_hover,
            pressed: theme.background_active,
            text: theme.foreground,
        },
    }
}

/// The keyboard focus as Figma draws it: the 1px `focus/default/ring` and,
/// over it, the 4px `focus/default/focus-glow`, both outside the control
/// (Figma stacks the glow over the ring, so the ring reads a touch paler
/// than its token, and 3px of glow show past it).
pub fn focus_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let around = |color: Hsla, spread: f32| BoxShadow {
        color,
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(spread),
        inset: false,
    };
    vec![around(theme.focus_ring, 1.0), around(theme.focus_glow, 4.0)]
}

/// An icon button's square.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconButtonSize {
    /// 24px, 8px corners.
    Small,
    /// 32px, 12px corners.
    Default,
}

impl IconButtonSize {
    pub fn px(self) -> f32 {
        match self {
            IconButtonSize::Small => 24.0,
            IconButtonSize::Default => 32.0,
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
    tinted_icon_button(id, icon, size, active, theme.icon, theme)
}

/// [`icon_button`] with the icon in `rest` at rest instead of the icon role
/// (Settings' fields draw theirs in `form_text`, the frame's gray).
pub fn tinted_icon_button(
    id: impl Into<SharedString>,
    icon: Icon,
    size: IconButtonSize,
    active: bool,
    rest_icon: Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let color = if active { theme.text_loud } else { rest_icon };
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

/// Figma's Icon slot: `icon` in a box `width` wide, flush right, so the
/// icon hangs past the slot's left edge and the label after it sits where
/// Figma puts it (an 18px icon in a 14px slot in an X-Small button, 20px in
/// 12px in a Small one).
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
    small_icon_button(id, icon, label, ButtonKind::Secondary, false, theme)
}

/// The Figma Button at Small in any style, 28px tall, with a left icon (the
/// board's New issue, the Review panel's Approve and Reject): the Default
/// style's icon is `button_icon`, a tinted style's the label's color.
pub fn small_icon_button(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    focused: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let icon_color = match kind {
        ButtonKind::Secondary => theme.button_icon,
        _ => button_colors(kind, theme).text,
    };
    button_base(id, kind, focused, theme)
        .h(px(28.0))
        .gap(px(4.0))
        .child(icon_slot(icon, 12.0, icon_color))
        .child(label.into())
}

/// The Figma Button at X-Small, Default style, label only: 8px across, 4px
/// down, X-Small/Medium loud on the overlay fill (the task panels' Open).
pub fn xsmall_button(id: impl Into<SharedString>, label: impl Into<SharedString>, theme: &Theme) -> Stateful<Div> {
    button_base(id, ButtonKind::Secondary, false, theme)
        .h(px(24.0))
        .px(px(8.0))
        .type_style(Type::XSmallMedium)
        .child(label.into())
}

/// A board filter (the Issues Header's chips): the Figma Button at Small,
/// label only, 28px tall, in the Select style while its filter is set
/// (`selected`: the cyan tint and label) and the Default style while it
/// isn't; pressed while `open` (its menu is up).
pub fn filter_chip(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    selected: bool,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let kind = if selected {
        ButtonKind::Select
    } else {
        ButtonKind::Secondary
    };
    let c = button_colors(kind, theme);
    let rest = if open { c.pressed } else { c.fill };
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(28.0))
        .px(px(12.0))
        .rounded(px(6.0))
        .type_style(Type::SmallMedium)
        .text_color(c.text)
        .cursor_pointer();
    fill_layer(d, id, px(6.0), [rest, c.hover, c.pressed])
        .child(div().whitespace_nowrap().child(label.into()))
}

/// The Searchbox component at Small (the Issues Header's search): 28px tall,
/// 6px corners, the overlay fill (the hover fill under the pointer), the
/// 20px magnifying glass in `icon` 6px in, then `content` 6px after it.
/// `focused` draws the focus ring and glow around it ([`focus_bands`]). The
/// caller sets its width (Figma: 180px), and adds a [`search_clear`] after
/// it while the query is non-empty (the field shrinks to make room).
pub fn search_box(id: impl Into<SharedString>, content: impl IntoElement, focused: bool, theme: &Theme) -> Stateful<Div> {
    search_box_sized(id, content, focused, 28.0, 6.0, theme)
}

/// The Searchbox at Base (the Keybindings page's): [`search_box`] 32px tall
/// with 8px corners.
pub fn search_box_base(id: impl Into<SharedString>, content: impl IntoElement, focused: bool, theme: &Theme) -> Stateful<Div> {
    search_box_sized(id, content, focused, 32.0, 8.0, theme)
}

fn search_box_sized(
    id: impl Into<SharedString>,
    content: impl IntoElement,
    focused: bool,
    height: f32,
    radius: f32,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .h(px(height))
        .px(px(6.0))
        .rounded(px(radius))
        .cursor_text();
    fill_layer(d, id, px(radius), [theme.background, theme.background_hover, theme.background_hover])
        .child(heroicon(icons::MAGNIFYING_GLASS).text_color(theme.icon))
        .child(div().relative().flex().flex_1().min_w_0().h_full().items_center().child(content))
        .when(focused, |d| d.children(focus_bands(radius, theme)))
}

/// A search box's clear button (`dt-search-clear-button`): the 20px x-mark
/// as a 24px [`icon_button`] at the box's right end, the icon 6px in from
/// the edge as the magnifying glass is from the left (the button hangs 2px
/// into the box's padding). The caller adds it after the field while the
/// query is non-empty, as a macOS search field shows its cancel button, and
/// gives it the click that clears the query. The Figma Searchbox draws no
/// clear button, so this follows the app's icon buttons.
pub fn search_clear(id: impl Into<SharedString>, theme: &Theme) -> Stateful<Div> {
    icon_button(id, icons::X_MARK, IconButtonSize::Small, false, theme)
        .mr(px(-2.0))
        .tooltip(tooltip("Clear search"))
}

/// [`focus_shadow`] drawn as bands around a control whose corners are
/// `radius`, for a translucent control: gpui paints a drop shadow as a
/// filled box under the element, which would show through its fill. The
/// 1px ring, then the 4px glow over it, both outside the control.
pub fn focus_bands(radius: f32, theme: &Theme) -> [Div; 2] {
    let band = |width: f32, color: Hsla| {
        div()
            .absolute()
            .top(px(-width))
            .left(px(-width))
            .right(px(-width))
            .bottom(px(-width))
            .rounded(px(radius + width))
            .border(px(width))
            .border_color(color)
    };
    [band(1.0, theme.focus_ring), band(4.0, theme.focus_glow)]
}

/// A workflow status's glyph ([`StatusGlyph`]) in its Figma color: backlog
/// and to do in the icon gray, in progress in the accent (`icon/select`),
/// review purple, done green, canceled muted.
pub fn status_icon(category: StatusCategory, theme: &Theme) -> Svg {
    let (glyph, color) = status_glyph_for(category, theme);
    status_glyph(glyph).text_color(color)
}

/// [`status_icon`] at 20pt, as Settings ▸ Workflows' status rows draw it.
pub fn status_icon_large(category: StatusCategory, theme: &Theme) -> Svg {
    let (glyph, color) = status_glyph_for(category, theme);
    crate::assets::status_glyph_large(glyph).text_color(color)
}

/// A category's glyph and its color.
fn status_glyph_for(category: StatusCategory, theme: &Theme) -> (StatusGlyph, Hsla) {
    match category {
        StatusCategory::Backlog => (StatusGlyph::Backlog, theme.icon),
        StatusCategory::Ready => (StatusGlyph::Todo, theme.icon),
        StatusCategory::Active => (StatusGlyph::InProgress, theme.accent),
        StatusCategory::Handoff => (StatusGlyph::Review, theme.icon_purple),
        StatusCategory::Done => (StatusGlyph::Done, theme.icon_green),
        StatusCategory::Archived => (StatusGlyph::Canceled, theme.muted),
    }
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
    let color = if current { theme.text_loud } else { theme.muted };
    status_heading_in(status_icon(category, theme), label, color)
}

/// [`status_heading`] with its glyph and label colors chosen by the caller:
/// the task panels draw the label in `theme.foreground` and In progress's
/// glyph in `icon/cyan` ([`panel_status_icon`]).
pub fn status_heading_in(icon: Svg, label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .child(icon)
        .child(
            div()
                .whitespace_nowrap()
                .type_style(Type::XSmallSemibold)
                .text_color(color)
                .child(label.into().to_uppercase()),
        )
}

/// A status's glyph as the task panels draw it (the Navigation component's
/// State=Workspace and State=Review): [`status_icon`]'s, except In progress
/// is `icon/cyan` in both themes where the board uses `icon/select`.
pub fn panel_status_icon(category: StatusCategory, theme: &Theme) -> Svg {
    match category {
        StatusCategory::Active => status_glyph(StatusGlyph::InProgress).text_color(theme.icon_cyan),
        other => status_icon(other, theme),
    }
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

/// The Activity frame's pickers: `theme.search`'s fill, a 14px icon (pass
/// a small one, `icons::FUNNEL_SMALL`) and an X-Small label, 24px tall and
/// no chevron; pressed while `open`. Icon and label are `log_text`: the
/// frame sets `theme.gray` to the log's opaque `#7b85a7` in both modes.
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
        .text_color(theme.log_text)
        .cursor_pointer();
    fill_layer(d, id, px(6.0), [rest, theme.background_active, theme.selection])
        .child(heroicon(icon).text_color(theme.log_text))
        .child(div().whitespace_nowrap().child(label.into()))
}

/// The modifier glyphs a chord starts with.
const MODIFIERS: [char; 5] = ['⌘', '⌃', '⌥', '⇧', '⇪'];

/// A chord's keys as macOS spells them: each modifier glyph its own cap,
/// then the key (`⌃⇧P` → `⌃`, `⇧`, `P`).
pub fn split_chord(chord: &str) -> Vec<String> {
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
    fn chords_split_into_keys() {
        assert_eq!(split_chord("⌃P"), ["⌃", "P"]);
        assert_eq!(split_chord("⌘⇧k"), ["⌘", "⇧", "K"]);
        assert_eq!(split_chord("Esc"), ["ESC"]);
    }
}
