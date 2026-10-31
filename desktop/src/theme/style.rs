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
//!   the Figma Button component's Default (`button/default/*`), Success
//!   (`button/success/*`) and Danger (`button/danger/*`) styles, quiet only
//!   a hover fill;
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
//! - every hover and press fades over about 200ms ([`motion::fill_layer`]);
//! - menus, popovers and the palette raised with a ring and a soft drop
//!   ([`popover`]);
//! - the keyboard focus as Figma draws it: a 1px `focus/default/ring` and a
//!   4px `focus/default/glow` ([`focus_bands`]); a Success or Danger
//!   button's in its own `focus/success/*` or `focus/danger/*`
//!   ([`ButtonColors::focus`]).
//!
//! Spacing follows Figma's scale (2, 4, 6, 8, 12, 16).

use gpui::{
    div, point, prelude::*, px, AnyView, App, BoxShadow, Div, FontWeight, Hsla, Pixels,
    SharedString, Stateful, Svg, Window,
};
use shelbi_core::StatusCategory;

use super::motion::fill_layer;
use super::Theme;
use crate::assets::{heroicon, icons, status_glyph, Icon, StatusGlyph, ICON_SIZE, ICON_SIZE_BUTTON};
use crate::elements::exact_text::exact_text;

/// The UI typeface every [`Type`] is set in.
pub const UI_FAMILY: &str = "Geist";

/// The Figma text styles (`Desktop/...`), all Geist. Views name one of these
/// rather than a size, a line height and a weight apiece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// `Desktop/Title`: 24 / 32, semibold. A full-window step's title (the
    /// Add Project wizard).
    Title,
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
            Type::Title => 24.0,
            Type::Heading => 20.0,
            Type::Base | Type::BaseMedium | Type::BaseSemibold => 14.0,
            Type::Small | Type::SmallMedium => 13.0,
            Type::XSmall | Type::XSmallMedium | Type::XSmallSemibold => 12.0,
        }
    }

    /// The line height in points.
    pub fn line_height(self) -> f32 {
        match self {
            Type::Title => 32.0,
            Type::Heading => 28.0,
            Type::Base | Type::BaseMedium | Type::BaseSemibold | Type::Small | Type::SmallMedium => 18.0,
            Type::XSmall | Type::XSmallMedium | Type::XSmallSemibold => 16.0,
        }
    }

    pub fn weight(self) -> FontWeight {
        match self {
            Type::Base | Type::Small | Type::XSmall => FontWeight::NORMAL,
            Type::BaseMedium | Type::SmallMedium | Type::XSmallMedium => FontWeight::MEDIUM,
            Type::Title | Type::BaseSemibold | Type::XSmallSemibold => FontWeight::SEMIBOLD,
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
/// on hover. It never shrinks, so a capped menu scrolls its rows whole.
pub fn menu_row(id: impl Into<SharedString>, selected: bool, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let d = div()
        .id(id.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_row()
        .flex_none()
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

/// [`menu_row`] that can't be chosen: the row at the disabled opacity, with
/// no hover, press or pointer.
pub fn disabled_menu_row(id: impl Into<SharedString>, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_2()
        .h(px(LIST_ROW_H + 4.0))
        .px_2()
        .rounded_lg()
        .opacity(0.4)
        .type_style(Type::Base)
        .text_color(theme.foreground)
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
    /// A dialog's confirming action (Load, Save, Enable): the Button
    /// component's Primary style (a cyan tint, `button/primary/text`).
    Primary,
    /// A positive action beside a Danger one (the Review panel's Approve):
    /// the Success style (`button/success/*`: a green tint, `Green/400` /
    /// `Green/700` text).
    Success,
    /// Every other action: the Default style (`button/default/background`,
    /// `button/default/text`).
    Secondary,
    /// A destructive action: the Danger style (a pink tint).
    Danger,
    /// A low-key action beside others, and a value that opens a picker
    /// (the task pane's properties): the Ghost style (`2565:13261`), no fill
    /// until hovered, a loud label.
    Ghost,
    /// A set filter: the Select style (a cyan tint, `button/select/text`).
    Select,
}

/// The Button component's Size (`2220:17600`). Every size is Geist Medium
/// with 6px corners and its content centered; the presets differ in height,
/// type, padding, gap and icon slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    /// 24px, X-Small medium (12 / 16), 8px across, 4px between, an 18px
    /// glyph in a 14px slot (the task panels' Open, the issue detail's
    /// actions).
    XSmall,
    /// 28px, Small medium (13 / 18), 12px across, 4px between, a 20px glyph
    /// in a 12px slot (toolbars, headers, rows).
    Small,
    /// 32px, Base medium (14 / 18), 12px across, 6px between, a 20px glyph
    /// in a 14px slot (forms and dialogs).
    Base,
}

impl ButtonSize {
    pub const fn height(self) -> f32 {
        match self {
            ButtonSize::XSmall => 24.0,
            ButtonSize::Small => 28.0,
            ButtonSize::Base => 32.0,
        }
    }

    /// The padding either side.
    pub const fn padding(self) -> f32 {
        match self {
            ButtonSize::XSmall => 8.0,
            ButtonSize::Small | ButtonSize::Base => 12.0,
        }
    }

    /// The gap between its pieces of content.
    pub const fn gap(self) -> f32 {
        match self {
            ButtonSize::XSmall | ButtonSize::Small => 4.0,
            ButtonSize::Base => 6.0,
        }
    }

    /// The label's text style.
    pub const fn text(self) -> Type {
        match self {
            ButtonSize::XSmall => Type::XSmallMedium,
            ButtonSize::Small => Type::SmallMedium,
            ButtonSize::Base => Type::BaseMedium,
        }
    }

    /// An icon slot's width: narrower than the glyph, which hangs past the
    /// slot's outer edge.
    pub const fn slot(self) -> f32 {
        match self {
            ButtonSize::XSmall | ButtonSize::Base => 14.0,
            ButtonSize::Small => 12.0,
        }
    }

    /// The size an icon draws at in this button: the `_BUTTON` drawings at
    /// X-Small, the 20px ones otherwise.
    pub const fn glyph(self) -> Pixels {
        match self {
            ButtonSize::XSmall => ICON_SIZE_BUTTON,
            ButtonSize::Small | ButtonSize::Base => ICON_SIZE,
        }
    }
}

/// The chevron every dropdown trigger draws in its Right Icon slot (a
/// board filter, a picker, a menu button): Figma's `chevron-down-small`
/// (2342:37950). New dropdowns take it through [`ButtonContent::dropdown`]
/// or [`dropdown_slot`] rather than naming an icon.
pub const DROPDOWN_CHEVRON: Icon = icons::CHEVRON_DOWN_SMALL;

/// A button's height at the Base size: the height of a form's controls
/// (textboxes, pickers) beside it.
pub const BUTTON_H: f32 = ButtonSize::Base.height();

/// What a Button holds, in the component's order: the left icon, the label,
/// the Hotkey, the right icon. Anything left out takes no gap.
#[derive(Debug, Clone, Default)]
pub struct ButtonContent {
    left: Option<Icon>,
    label: Option<SharedString>,
    hotkey: Option<SharedString>,
    right: Option<Icon>,
}

impl ButtonContent {
    /// Just `label`.
    pub fn label(label: impl Into<SharedString>) -> Self {
        Self {
            label: Some(label.into()),
            ..Self::default()
        }
    }

    /// `Show Left Icon` (`None` hides it).
    pub fn left(mut self, icon: impl Into<Option<Icon>>) -> Self {
        self.left = icon.into();
        self
    }

    /// `Show Right Icon` (`None` hides it).
    pub fn right(mut self, icon: impl Into<Option<Icon>>) -> Self {
        self.right = icon.into();
        self
    }

    /// The Right Icon as a dropdown's chevron ([`DROPDOWN_CHEVRON`]).
    pub fn dropdown(self) -> Self {
        self.right(DROPDOWN_CHEVRON)
    }

    /// `Show Hotkey`: `key` as written (`E`, `⌘S`, `Esc`). Empty shows
    /// none. The Figma sheet draws it; no view does since the task pane's
    /// Done went with its edit mode.
    #[cfg_attr(not(any(test, feature = "visual-check")), allow(dead_code))]
    pub fn hotkey(mut self, key: impl Into<SharedString>) -> Self {
        let key = key.into();
        self.hotkey = (!key.is_empty()).then_some(key);
        self
    }
}

/// The Button component, label only: `kind` at `size`. `focused` draws the
/// style's focus ring and glow outside it (keyboard focus in a dialog) and
/// nothing else: the fill and label stay the style's.
pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    size: ButtonSize,
    focused: bool,
    theme: &Theme,
) -> Stateful<Div> {
    content_button(id, ButtonContent::label(label), kind, size, focused, theme)
}

/// [`button`] with Disable on.
pub fn disabled_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    size: ButtonSize,
    theme: &Theme,
) -> Stateful<Div> {
    disabled_content_button(id, ButtonContent::label(label), kind, size, theme)
}

/// The Button component holding `content`.
pub fn content_button(
    id: impl Into<SharedString>,
    content: ButtonContent,
    kind: ButtonKind,
    size: ButtonSize,
    focused: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    with_content(button_base(id.clone(), kind, size, focused, theme), &id, content, kind, size, theme)
}

/// The Button component holding `content`, with Disable on: the whole
/// control (fill, label, icons, hotkey) at `button/disabled/opacity`
/// (40%) once, at the enabled button's size, with no hover, press, focus
/// or pointer.
pub fn disabled_content_button(
    id: impl Into<SharedString>,
    content: ButtonContent,
    kind: ButtonKind,
    size: ButtonSize,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let c = button_colors(kind, theme);
    let d = button_shell(id.clone(), kind, size, theme).bg(c.fill).opacity(0.4);
    with_content(d, &id, content, kind, size, theme)
}

/// The Button component while its action runs: Disable on (no hover,
/// press, focus or pointer), `spinner` in the Left Icon slot before
/// `label` (Contact Us' "Sending…").
pub fn pending_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    spinner: impl IntoElement,
    kind: ButtonKind,
    size: ButtonSize,
    theme: &Theme,
) -> Stateful<Div> {
    let c = button_colors(kind, theme);
    button_shell(id, kind, size, theme)
        .bg(c.fill)
        .opacity(0.4)
        .child(left_slot(size, spinner))
        .child(label.into())
}

/// A button with an icon before its label (Load older, Refresh).
pub fn icon_label_button(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    size: ButtonSize,
    theme: &Theme,
) -> Stateful<Div> {
    content_button(id, ButtonContent::label(label).left(icon), kind, size, false, theme)
}

/// `content` laid into button `id` of `size`, in the component's order,
/// each icon in the style's icon color (not the label's). Each piece is
/// `<id>-left`, `-label`, `-hotkey` or `-right` to a test.
fn with_content(
    b: Stateful<Div>,
    id: &SharedString,
    content: ButtonContent,
    kind: ButtonKind,
    size: ButtonSize,
    theme: &Theme,
) -> Stateful<Div> {
    let icon = button_colors(kind, theme).icon;
    let part = |name: &'static str| {
        let selector = format!("{id}-{name}");
        move || selector.clone()
    };
    b.children(content.left.map(|i| left_icon_slot(i, size, icon).debug_selector(part("left"))))
        .children(content.label.map(|l| div().debug_selector(part("label")).child(l)))
        .children(content.hotkey.map(|k| button_hotkey(k, theme).debug_selector(part("hotkey"))))
        .children(content.right.map(|i| right_icon_slot(i, size, icon).debug_selector(part("right"))))
}

/// A button's shape, type and colors at `size`, without its content: hover
/// and press fade its fill, and `focused` adds the style's ring and glow
/// outside it. Views that hold more than [`ButtonContent`] (a status glyph,
/// a truncating choice) add their own children, sized from `size`.
pub fn button_base(
    id: impl Into<SharedString>,
    kind: ButtonKind,
    size: ButtonSize,
    focused: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let c = button_colors(kind, theme);
    let d = button_shell(id.clone(), kind, size, theme).cursor_pointer();
    // Bands, not `focus_shadow`: a gpui shadow is a filled box under the
    // button and its tinted fill is translucent, so the glow would fill it.
    let rest = if focused { c.focus_fill } else { c.fill };
    fill_layer(d, id, px(6.0), [rest, c.hover, c.pressed])
        .when(focused, |b| b.children(focus_bands_in(6.0, 0.0, c.focus)))
}

/// [`button_base`] with Disable on (see [`disabled_content_button`]), for
/// views that add their own children.
pub fn disabled_button_base(id: impl Into<SharedString>, kind: ButtonKind, size: ButtonSize, theme: &Theme) -> Stateful<Div> {
    let c = button_colors(kind, theme);
    button_shell(id, kind, size, theme).bg(c.fill).opacity(0.4)
}

/// The geometry and type every state of a size shares.
fn button_shell(id: impl Into<SharedString>, kind: ButtonKind, size: ButtonSize, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(size.height()))
        .px(px(size.padding()))
        .gap(px(size.gap()))
        .rounded(px(6.0))
        .text_color(button_colors(kind, theme).text)
        .type_style(size.text())
}

/// The Button's left Icon slot: `icon` in a box [`ButtonSize::slot`] wide,
/// flush right, so the glyph hangs past the slot's left edge and the label
/// sits where Figma puts it.
pub fn left_icon_slot(icon: Icon, size: ButtonSize, color: Hsla) -> Div {
    left_slot(size, button_glyph(icon, size, color))
}

/// The Button's Right Icon slot: [`left_icon_slot`] mirrored, flush left so
/// the glyph hangs past its right edge.
pub fn right_icon_slot(icon: Icon, size: ButtonSize, color: Hsla) -> Div {
    right_slot(size, button_glyph(icon, size, color))
}

/// A dropdown trigger's Right Icon slot: [`DROPDOWN_CHEVRON`] in `color`.
pub fn dropdown_slot(size: ButtonSize, color: Hsla) -> Div {
    right_icon_slot(DROPDOWN_CHEVRON, size, color)
}

/// The left Icon slot holding something other than an icon (a picker's
/// status glyph).
pub fn left_slot(size: ButtonSize, content: impl IntoElement) -> Div {
    slot(size).justify_end().child(content)
}

/// The Right Icon slot holding something other than an icon.
pub fn right_slot(size: ButtonSize, content: impl IntoElement) -> Div {
    slot(size).justify_start().child(content)
}

fn slot(size: ButtonSize) -> Div {
    div().flex().flex_none().items_center().w(px(size.slot()))
}

/// `icon` as a `size` button draws it: an X-Small button's at 18px (a
/// `_BUTTON` drawing), the others' at 20px.
fn button_glyph(icon: Icon, size: ButtonSize, color: Hsla) -> Svg {
    debug_assert_eq!(icon.size(), size.glyph(), "{} in a {size:?} button", icon.path());
    heroicon(icon).text_color(color)
}

/// The Button component's Hotkey: the key as written (`s`, `E`, `⌘S`),
/// X-Small in gray on the canvas, 16px tall with 4px across and corners,
/// and a 1px drop.
pub fn button_hotkey(key: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(16.0))
        .px(px(4.0))
        .rounded(px(4.0))
        .bg(theme.keycap)
        .shadow(vec![BoxShadow::new(px(0.0), px(1.0), theme.button_keycap_edge)])
        .type_style(Type::XSmall)
        .text_color(theme.gray)
        .child(key.into())
}

/// A button kind's colors, in each state.
#[derive(Debug, Clone, Copy)]
pub struct ButtonColors {
    pub fill: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub text: Hsla,
    /// Its fill at rest while focused: the rest fill, but for the Ghost
    /// style, whose Focus variant (`2565:13282`) rests on its hover fill so
    /// the ring has something to hold.
    pub focus_fill: Hsla,
    /// Its icons (`button/<style>/icon`): the Default style's own gray, the
    /// tinted styles' their label's color.
    pub icon: Hsla,
    /// The ring and glow it wears while focused, over whichever fill.
    pub focus: FocusColors,
}

/// A focus treatment: the 1px ring and the 4px glow over it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusColors {
    pub ring: Hsla,
    pub glow: Hsla,
}

impl FocusColors {
    /// `focus/default/*`: every control's focus but a Success or Danger
    /// button's.
    pub fn default(theme: &Theme) -> Self {
        Self {
            ring: theme.focus_ring,
            glow: theme.focus_glow,
        }
    }
}

/// A button kind's fill and text at rest, hovered and pressed: the Button
/// component's `background`, `background-hover` and `background-active`;
/// and its focus. The Success and Danger styles have their own
/// (`focus/success/*`, `focus/danger/*`); the rest wear the default cyan
/// (Select has no Figma focus variant; Ghost's is the default ring over
/// its hover fill).
pub fn button_colors(kind: ButtonKind, theme: &Theme) -> ButtonColors {
    let default_focus = FocusColors::default(theme);
    match kind {
        ButtonKind::Primary => ButtonColors {
            fill: theme.primary_fill,
            hover: theme.primary_fill_hover,
            pressed: theme.primary_fill_active,
            text: theme.primary_label,
            focus_fill: theme.primary_fill,
            icon: theme.primary_label,
            focus: default_focus,
        },
        ButtonKind::Success => ButtonColors {
            fill: theme.success_fill,
            hover: theme.success_fill_hover,
            pressed: theme.success_fill_active,
            text: theme.success_label,
            focus_fill: theme.success_fill,
            icon: theme.success_label,
            focus: FocusColors {
                ring: theme.success_focus_ring,
                glow: theme.success_focus_glow,
            },
        },
        ButtonKind::Secondary => ButtonColors {
            fill: theme.background,
            hover: theme.background_hover,
            pressed: theme.background_active,
            text: theme.text_loud,
            focus_fill: theme.background,
            icon: theme.button_icon,
            focus: default_focus,
        },
        ButtonKind::Danger => ButtonColors {
            fill: theme.danger_fill,
            hover: theme.danger_fill_hover,
            pressed: theme.danger_fill_active,
            text: theme.error,
            focus_fill: theme.danger_fill,
            icon: theme.error,
            focus: FocusColors {
                ring: theme.danger_focus_ring,
                glow: theme.danger_focus_glow,
            },
        },
        ButtonKind::Select => ButtonColors {
            fill: theme.select_fill,
            hover: theme.select_fill_hover,
            pressed: theme.select_fill_active,
            text: theme.select_label,
            focus_fill: theme.select_fill,
            icon: theme.select_label,
            focus: default_focus,
        },
        // Ghost (`2565:13261`..`13298`): Hover on `theme.background`, Active
        // on `button/default/background-hover`, the label `theme.text_loud`
        // (`#020303` Light), the Default style's icon gray.
        ButtonKind::Ghost => ButtonColors {
            fill: theme.background.opacity(0.0),
            hover: theme.background,
            pressed: theme.background_hover,
            text: theme.text_loud,
            focus_fill: theme.background,
            icon: theme.button_icon,
            focus: default_focus,
        },
    }
}

/// The keyboard focus as Figma draws it: the 1px `focus/default/ring` and,
/// over it, the 4px `focus/default/glow`, both outside the control
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

/// [`focus_shadow`] painted inside a fill that is a tint of the glass (a
/// board card in a translucent window): a drop shadow is a filled box under
/// the element, and the ring's would show through the tint, so the ring and
/// the glow step inside the edge instead.
pub fn focus_shadow_inset(theme: &Theme) -> Vec<BoxShadow> {
    focus_shadow(theme).into_iter().map(|s| BoxShadow { inset: true, ..s }).collect()
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

/// A toolbar action (the board's New issue): the Button at Small, Default
/// style, with a left icon.
pub fn toolbar_button(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    small_icon_button(id, icon, label, ButtonKind::Secondary, false, theme)
}

/// The Button at Small in any style with a left icon (the board's New
/// issue, the Review panel's Approve and Reject).
pub fn small_icon_button(
    id: impl Into<SharedString>,
    icon: Icon,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    focused: bool,
    theme: &Theme,
) -> Stateful<Div> {
    content_button(id, ButtonContent::label(label).left(icon), kind, ButtonSize::Small, focused, theme)
}

/// The Button at X-Small, Default style, label only (the task panels'
/// Open).
pub fn xsmall_button(id: impl Into<SharedString>, label: impl Into<SharedString>, theme: &Theme) -> Stateful<Div> {
    button(id, label, ButtonKind::Secondary, ButtonSize::XSmall, false, theme)
}

/// A board filter (the Issues Header's chips): the Figma Button at Small,
/// 28px tall, its label then a down chevron in the Right Icon slot, in the
/// Select style (the cyan tint, label and chevron) while its filter is set
/// (`selected`) or its menu is up (`open`), and the Default style
/// otherwise ([`filter_chip_kind`]). Each piece is `<id>-label` or
/// `-right` to a test.
pub fn filter_chip(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    selected: bool,
    open: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let kind = filter_chip_kind(selected, open);
    let c = button_colors(kind, theme);
    let part = |name: &'static str| {
        let selector = format!("{id}-{name}");
        move || selector.clone()
    };
    let d = button_shell(id.clone(), kind, ButtonSize::Small, theme).cursor_pointer();
    fill_layer(d, id.clone(), px(6.0), [c.fill, c.hover, c.pressed])
        .child(div().debug_selector(part("label")).whitespace_nowrap().child(label.into()))
        .child(dropdown_slot(ButtonSize::Small, c.icon).debug_selector(part("right")))
}

/// The style a board filter chip draws in: Select while its filter is set
/// or its menu is open, Default (Secondary) otherwise.
pub fn filter_chip_kind(selected: bool, open: bool) -> ButtonKind {
    if selected || open {
        ButtonKind::Select
    } else {
        ButtonKind::Secondary
    }
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
/// 1px ring, then the 4px glow over it, both outside the control, so
/// neither moves its layout nor its hit area.
///
/// The widths are the Button component's focus variants' (a 1px and a 4px
/// outside stroke), not `focus/ring-width`, which says 2 and which no
/// variant binds.
pub fn focus_bands(radius: f32, theme: &Theme) -> [Div; 2] {
    focus_bands_past(radius, 0.0, theme)
}

/// [`focus_bands`] for a control with a `border` of its own: an absolute
/// child is placed from inside the border, so the bands step out past it.
pub fn focus_bands_past(radius: f32, border: f32, theme: &Theme) -> [Div; 2] {
    focus_bands_in(radius, border, FocusColors::default(theme))
}

/// [`focus_bands_past`] in `colors` (a Success or Danger button's focus).
pub fn focus_bands_in(radius: f32, border: f32, colors: FocusColors) -> [Div; 2] {
    let band = |width: f32, color: Hsla| {
        let out = px(-(width + border));
        div()
            .absolute()
            .top(out)
            .left(out)
            .right(out)
            .bottom(out)
            .rounded(px(radius + width))
            .border(px(width))
            .border_color(color)
    };
    [band(1.0, colors.ring), band(4.0, colors.glow)]
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
/// sliding chip stands in). Unselected, no fill: the pointer changes only
/// the cursor.
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
        .id(id)
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
        d.text_color(theme.muted)
    };
    d.child(label.into())
}

/// The Activity frame's pickers: `surface/depth-30`'s fill, a 14px icon
/// (pass a small one, `icons::FUNNEL_SMALL`) and an X-Small label, 24px
/// tall and no chevron; pressed while `open`. Icon and label are
/// `log_text`: the frame's `text/gray`, the log's opaque `#7b85a7` in both
/// modes.
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
    // A flex column that can shrink, so a capped menu (`placed`) scrolls.
    div()
        .relative()
        .flex()
        .flex_col()
        .min_h_0()
        .child(menu)
        .with_animation(
            id,
            Animation::new(super::motion::MENU_IN).with_easing(super::motion::ease_out_expo),
            |d, t| d.top(px((1.0 - t) * super::motion::SLIDE_PX)).opacity(t),
        )
}

/// `menu` 6px under the control `anchor` measured, its left edges aligned,
/// flipped above it, capped or shifted to stay inside the window
/// ([`placed`](crate::elements::placed)); where the control hasn't painted
/// yet, against `fallback` (window coordinates).
pub fn below(anchor: &Anchor, fallback: gpui::Point<gpui::Pixels>, menu: impl IntoElement) -> impl IntoElement {
    crate::elements::placed::placed(anchor, menu).gap(6.0).fallback(fallback)
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
    fn a_filter_chip_is_select_while_set_or_open() {
        assert_eq!(filter_chip_kind(false, false), ButtonKind::Secondary);
        assert_eq!(filter_chip_kind(true, false), ButtonKind::Select, "a set filter");
        assert_eq!(filter_chip_kind(false, true), ButtonKind::Select, "its menu is open");
        assert_eq!(filter_chip_kind(true, true), ButtonKind::Select);
    }


    fn themes() -> [Theme; 2] {
        use gpui::WindowAppearance;
        use shelbi_app::theme::ThemePreference;
        [WindowAppearance::Dark, WindowAppearance::Light].map(|a| Theme::resolve(ThemePreference::Auto, a))
    }

    fn rgba8(c: Hsla) -> u32 {
        let c = c.to_rgb();
        let q = |v: f32| (v * 255.0).round() as u32;
        q(c.r) << 24 | q(c.g) << 16 | q(c.b) << 8 | q(c.a)
    }

    /// The Primary style is the Figma Button's Primary: `button/primary/*`,
    /// `Cyan/400` at 18 / 28 / 40% in Dark and 12 / 22 / 32% in Light under
    /// `Cyan/300` / `Cyan/600`. Success keeps the green it had as Primary.
    #[test]
    fn the_primary_button_is_the_figma_primary_style() {
        let [dark, light] = themes();
        let colors = |theme: &Theme, kind| {
            let c = button_colors(kind, theme);
            [c.fill, c.hover, c.pressed, c.text].map(rgba8)
        };
        assert_eq!(colors(&dark, ButtonKind::Primary), [0x62cdf62e, 0x62cdf647, 0x62cdf666, 0xb0e6fbff]);
        assert_eq!(colors(&light, ButtonKind::Primary), [0x62cdf61f, 0x62cdf638, 0x62cdf652, 0x1989c4ff]);
        assert_eq!(colors(&dark, ButtonKind::Success), [0x5ac3512e, 0x5ac35147, 0x5ac35166, 0x86d580ff]);
        assert_eq!(colors(&light, ButtonKind::Success), [0x5ac3511f, 0x5ac35138, 0x5ac35152, 0x00753dff]);
    }

    /// Each style's focus, as the Button component's Focus variants draw it
    /// (`2224:5758` Default, `2224:5770` Success, `2224:5782` Danger):
    /// Success green and Danger pink in both themes, every other style the
    /// default cyan the search boxes and fields wear too.
    #[test]
    fn success_and_danger_buttons_focus_in_their_own_colors() {
        let [dark, light] = themes();
        let focus = |theme: &Theme, kind| {
            let f = button_colors(kind, theme).focus;
            [rgba8(f.ring), rgba8(f.glow)]
        };
        for kind in [ButtonKind::Primary, ButtonKind::Secondary, ButtonKind::Select, ButtonKind::Ghost] {
            assert_eq!(focus(&dark, kind), [0x62cdf6ff, 0x12adff33], "{kind:?} Dark");
            assert_eq!(focus(&light, kind), [0x31ade9ff, 0x12adff33], "{kind:?} Light");
            assert_eq!(button_colors(kind, &dark).focus, FocusColors::default(&dark));
        }
        assert_eq!(focus(&dark, ButtonKind::Success), [0x5ac351ff, 0x86d58033]);
        assert_eq!(focus(&light, ButtonKind::Success), [0x5ac351ff, 0x86d58026]);
        assert_eq!(focus(&dark, ButtonKind::Danger), [0xf21478ff, 0xff66ab40]);
        assert_eq!(focus(&light, ButtonKind::Danger), [0xf21478ff, 0xff66ab1a]);
    }

    /// Focus is the ring and glow and nothing else: a focused button keeps
    /// its label color, sets no fill of its own (the fill is the hover layer
    /// under it, the same three colors either way), and draws no shadow,
    /// which gpui paints as a filled box that shows through the translucent
    /// fills and turned the focused Load button solid cyan.
    #[test]
    fn focus_never_changes_a_buttons_fill_or_label() {
        const KINDS: [ButtonKind; 6] = [
            ButtonKind::Primary,
            ButtonKind::Success,
            ButtonKind::Secondary,
            ButtonKind::Danger,
            ButtonKind::Ghost,
            ButtonKind::Select,
        ];
        for theme in themes() {
            for kind in KINDS {
                let mut rest = button("b", "Load", kind, ButtonSize::Base, false, &theme);
                let mut focused = button("b", "Load", kind, ButtonSize::Base, true, &theme);
                let (rest, focused) = (rest.style(), focused.style());
                assert_eq!(focused.text.color, rest.text.color, "{kind:?} label");
                assert_eq!(focused.text.color, Some(button_colors(kind, &theme).text), "{kind:?} label");
                assert!(rest.background.is_none() && focused.background.is_none(), "{kind:?} fill");
                assert!(focused.box_shadow.is_none(), "{kind:?} draws its focus as a shadow");
                for mut small in [
                    small_icon_button("s", icons::CHECK, "Approve", kind, true, &theme),
                    icon_label_button("i", icons::CHECK, "Refresh", kind, ButtonSize::Base, &theme),
                ] {
                    assert!(small.style().box_shadow.is_none(), "{kind:?} icon button");
                }
            }
        }
    }

    /// Every button of `size` holding each mix of content, enabled and
    /// disabled, Default style.
    struct SizeSheet(Theme);

    const CONTENTS: [&str; 5] = ["label", "left", "right", "both", "hotkey"];

    fn sheet_content(name: &str, size: ButtonSize) -> ButtonContent {
        let (left, right) = match size {
            ButtonSize::XSmall => (icons::ARROW_LEFT_BUTTON, icons::ARROW_RIGHT_BUTTON),
            _ => (icons::PLUS, DROPDOWN_CHEVRON),
        };
        let c = ButtonContent::label("Button");
        match name {
            "left" => c.left(left),
            "right" => c.right(right),
            "both" => c.left(left).right(right),
            "hotkey" => c.left(left).hotkey("E").right(right),
            _ => c,
        }
    }

    const SIZES: [ButtonSize; 3] = [ButtonSize::XSmall, ButtonSize::Small, ButtonSize::Base];

    impl gpui::Render for SizeSheet {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            let theme = self.0;
            div().flex().flex_col().gap_4().p_4().children(SIZES.iter().flat_map(move |&size| {
                CONTENTS.iter().map(move |&name| {
                    div()
                        .flex()
                        .flex_row()
                        .gap_4()
                        .child(content_button(format!("{size:?}-{name}"), sheet_content(name, size), ButtonKind::Secondary, size, false, &theme))
                        .child(disabled_content_button(
                            format!("{size:?}-{name}-off"),
                            sheet_content(name, size),
                            ButtonKind::Secondary,
                            size,
                            &theme,
                        ))
                })
            }))
        }
    }

    /// The Figma size contract (`2220:17545` X-Small, `2220:17601` Small,
    /// `2220:17603` Base): each size's height and padding, its slots' widths
    /// on the outer sides, the gap between pieces (none for a hidden one),
    /// and a disabled button laid out exactly as its enabled twin.
    #[gpui::test]
    fn buttons_lay_out_to_the_figma_size_contract(cx: &mut gpui::TestAppContext) {
        let [dark, _] = themes();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| cx.new(|_| SizeSheet(dark))).unwrap()
        });
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        let at = |vcx: &mut gpui::VisualTestContext, s: String| {
            let s: &'static str = Box::leak(s.into_boxed_str());
            vcx.debug_bounds(s)
        };
        let close = |a: Pixels, b: f32, what: &str| assert!((f32::from(a) - b).abs() < 0.01, "{what}: {a:?} vs {b}");
        for size in SIZES {
            let (h, pad, gap, slot) = (size.height(), size.padding(), size.gap(), size.slot());
            assert_eq!((h, pad, gap, slot), match size {
                ButtonSize::XSmall => (24.0, 8.0, 4.0, 14.0),
                ButtonSize::Small => (28.0, 12.0, 4.0, 12.0),
                ButtonSize::Base => (32.0, 12.0, 6.0, 14.0),
            });
            for name in CONTENTS {
                let id = format!("{size:?}-{name}");
                let what = |part: &str| format!("{id} {part}");
                let b = at(&mut vcx, id.clone()).unwrap_or_else(|| panic!("{id} drawn"));
                let off = at(&mut vcx, format!("{id}-off")).unwrap();
                close(b.size.height, h, &what("height"));
                assert_eq!(b.size, off.size, "{id}: disabled shares the enabled geometry");
                let label = at(&mut vcx, format!("{id}-label")).unwrap();
                let left = at(&mut vcx, format!("{id}-left"));
                let hotkey = at(&mut vcx, format!("{id}-hotkey"));
                let right = at(&mut vcx, format!("{id}-right"));
                // Left to right, each piece `gap` after the one before.
                let mut edge = b.left() + px(pad);
                for (part, bounds) in [("left", left), ("label", Some(label)), ("hotkey", hotkey), ("right", right)] {
                    let Some(bounds) = bounds else { continue };
                    close(bounds.left() - edge, 0.0, &what(part));
                    if part == "left" || part == "right" {
                        close(bounds.size.width, slot, &what("slot width"));
                    }
                    edge = bounds.right() + px(gap);
                }
                close(b.right() - (edge - px(gap)), pad, &what("right padding"));
                assert_eq!(left.is_some(), matches!(name, "left" | "both" | "hotkey"), "{id} left");
                assert_eq!(right.is_some(), matches!(name, "right" | "both" | "hotkey"), "{id} right");
                assert_eq!(hotkey.is_some(), name == "hotkey", "{id} hotkey");
                if let Some(k) = hotkey {
                    close(k.size.height, 16.0, &what("hotkey height"));
                }
            }
        }
    }

    /// An icon in a button draws at its size's glyph: 18px (the `_BUTTON`
    /// drawings) at X-Small, 20px at Small and Base, hanging past the
    /// slot's outer edge.
    #[test]
    fn button_glyphs_draw_at_the_sizes_glyph() {
        assert_eq!(ButtonSize::XSmall.glyph(), px(18.0));
        assert_eq!(ButtonSize::Small.glyph(), px(20.0));
        assert_eq!(ButtonSize::Base.glyph(), px(20.0));
        for size in SIZES {
            for name in CONTENTS {
                let c = sheet_content(name, size);
                for icon in c.left.into_iter().chain(c.right) {
                    assert_eq!(icon.size(), size.glyph(), "{size:?} {name}");
                    assert!(f32::from(icon.size()) > size.slot(), "{size:?}: the glyph hangs past its slot");
                }
            }
        }
    }

    /// Icons take the style's icon role, not the label's: the Default
    /// style's own `button/default/icon` (`#acb4cf` Dark, `#7b85a7` Light,
    /// opaque), the tinted styles' their label's color.
    #[test]
    fn button_icons_take_the_styles_icon_role() {
        let [dark, light] = themes();
        assert_eq!(rgba8(button_colors(ButtonKind::Secondary, &dark).icon), 0xacb4cfff);
        assert_eq!(rgba8(button_colors(ButtonKind::Secondary, &light).icon), 0x7b85a7ff);
        for theme in [dark, light] {
            assert_ne!(button_colors(ButtonKind::Secondary, &theme).icon, button_colors(ButtonKind::Secondary, &theme).text);
            for kind in [ButtonKind::Primary, ButtonKind::Success, ButtonKind::Danger, ButtonKind::Select] {
                let c = button_colors(kind, &theme);
                assert_eq!(c.icon, c.text, "{kind:?}");
            }
        }
    }

    /// A disabled button dims once, as a whole: the control at 40% and
    /// nothing inside it dimmed again; no hover layer, pointer or focus.
    #[test]
    fn disabled_buttons_dim_once_and_take_no_pointer() {
        for theme in themes() {
            for size in SIZES {
                let mut b = disabled_content_button("d", sheet_content("hotkey", size), ButtonKind::Secondary, size, &theme);
                let style = b.style();
                assert_eq!(style.opacity, Some(0.4));
                assert!(style.mouse_cursor.is_none(), "{size:?} takes no pointer");
                assert_eq!(style.background, Some(button_colors(ButtonKind::Secondary, &theme).fill.into()));
                let mut on = content_button("e", sheet_content("hotkey", size), ButtonKind::Secondary, size, true, &theme);
                assert!(on.style().opacity.is_none(), "{size:?} enabled is opaque");
            }
        }
    }

    #[test]
    fn chords_split_into_keys() {
        assert_eq!(split_chord("⌃P"), ["⌃", "P"]);
        assert_eq!(split_chord("⌘⇧k"), ["⌘", "⇧", "K"]);
        assert_eq!(split_chord("Esc"), ["ESC"]);
    }
}
