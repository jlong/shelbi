//! The pieces the palette-only list screens share (Jobs, Reminders, the two
//! segments of one screen under its toolbar, `super::jobs_reminders`): the
//! centered, scrolling column of cards (the Changelog's column, at most [`CONTENT_MAX_W`] wide, so a wide window
//! doesn't stretch a row into a scan across the screen), a section's heading
//! over its cards, the card that holds a group of compact rows, the empty
//! state (an icon, a heading, a sentence and the command that fills the
//! screen, ready to copy), the line in place of the list (loading,
//! unavailable), and a sheet's danger action. The TUI's screens share their
//! own parts in `shelbi-tui`'s `list_screen`. (The sheets themselves are the
//! floating pane's, shared separately.)
//!
//! Debug selectors are named from the screen (`jobs-list`,
//! `reminders-empty`) and its entry (`job-row-<key>`,
//! `reminder-sheet-cancel`), so each screen's tests find their own.

use gpui::{div, prelude::*, px, AnyElement, App, ClickEvent, Div, Hsla, ScrollHandle, SharedString, Stateful, Window};
use shelbi_app::theme::Tone;

use super::floating_pane::INSET;
use crate::assets::{heroicon, Icon};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The content column's widest; a wider window centers it.
pub(crate) const CONTENT_MAX_W: f32 = 960.0;
/// The column's room from the view's edges.
const CONTENT_PAD_X: f32 = 24.0;
/// A card's corners: the board cards'.
pub(crate) const CARD_RADIUS: f32 = 12.0;
/// A compact row's height in a group card, and its corners.
pub(crate) const COMPACT_ROW_H: f32 = 36.0;
const COMPACT_ROW_RADIUS: f32 = 8.0;

/// The line in place of the list (loading, unavailable, failed), centered.
pub(crate) fn message(selector: &'static str, text: String, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .child(style::empty_state(selector, text, None, theme).max_w_full())
        .into_any_element()
}

/// The column the screen's sections stack in; the caller adds them and
/// hands it to [`scrolling`].
pub(crate) fn content() -> Div {
    div().flex().flex_col().w_full().max_w(px(CONTENT_MAX_W)).gap(px(8.0))
}

/// `content` centered in the view, scrolling, `<list>-list`.
pub(crate) fn scrolling(list: &'static str, scroll: &ScrollHandle, content: Div) -> AnyElement {
    div()
        .id(SharedString::from(format!("{list}-list")))
        .debug_selector(move || format!("{list}-list"))
        .track_scroll(scroll)
        .flex()
        .flex_col()
        .items_center()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .px(px(CONTENT_PAD_X))
        .pt(px(12.0))
        .pb(px(24.0))
        .child(content)
        .into_any_element()
}

/// A section's heading over its cards (`Running 2`, `Today`), in the
/// sidebar sections' type; `spaced` sets it off from the section above.
pub(crate) fn section_heading(
    selector: impl Into<String>,
    text: String,
    count: Option<usize>,
    spaced: bool,
    theme: &Theme,
) -> AnyElement {
    let selector = selector.into();
    div()
        .debug_selector(move || selector)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(4.0))
        .when(spaced, |d| d.pt(px(12.0)))
        .type_style(Type::SmallMedium)
        .text_color(theme.muted)
        .child(text)
        .when_some(count, |d, n| {
            d.child(
                div()
                    .px(px(5.0))
                    .rounded(px(4.0))
                    .bg(theme.background)
                    .type_style(Type::XSmallMedium)
                    .text_color(theme.foreground)
                    .child(n.to_string()),
            )
        })
        .into_any_element()
}

/// The card holding a group of compact rows (a day's finished jobs).
pub(crate) fn group_card(selector: String, rows: impl IntoIterator<Item = AnyElement>, theme: &Theme) -> AnyElement {
    div()
        .debug_selector(move || selector)
        .flex()
        .flex_col()
        .p(px(4.0))
        .rounded(px(CARD_RADIUS))
        .bg(theme.card)
        .shadow(style::card_shadow(theme))
        .children(rows)
        .into_any_element()
}

/// One entry's compact row in a [`group_card`], id and selector `id`:
/// highlighted while its sheet is open and on hover; the caller adds the
/// cells and the click.
pub(crate) fn compact_row(id: String, open: bool, theme: &Theme) -> Stateful<Div> {
    let highlight = theme.fill_on(theme.background_hover, theme.card);
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .h(px(COMPACT_ROW_H))
        .min_w_0()
        .px(px(10.0))
        .rounded(px(COMPACT_ROW_RADIUS))
        .cursor_pointer()
        .when(open, |d| d.bg(highlight))
        .hover(move |d| d.bg(highlight))
}

/// The tint behind a chip or a mark in `tone`, laid on a card.
pub(crate) fn tint(tone: Tone, theme: &Theme) -> Hsla {
    let fill = match tone {
        Tone::Green => theme.green_tint,
        Tone::Yellow => theme.yellow_tint,
        Tone::Red | Tone::LightRed => theme.red_tint,
        Tone::Blue => theme.blue_tint,
        Tone::Cyan => theme.cyan_tint,
        Tone::Magenta => theme.magenta_tint,
        _ => theme.background,
    };
    theme.fill_on(fill, theme.card)
}

/// Nothing to list: the screen's `icon` in a disc, `title`, a sentence on
/// what the screen holds, and the `command` that adds one, ready to copy
/// (the bootstrap screen's command box). Selectors `<selector>` and
/// `<selector>-icon`, `<selector>-command`.
pub(crate) fn empty_state(
    selector: &'static str,
    icon: Icon,
    title: &'static str,
    detail: &'static str,
    command: &'static str,
    theme: &Theme,
) -> AnyElement {
    div()
        .debug_selector(move || selector.into())
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .items_center()
        .justify_center()
        .gap(px(12.0))
        .p_6()
        .child(
            div()
                .debug_selector(move || format!("{selector}-icon"))
                .flex()
                .items_center()
                .justify_center()
                .size(px(44.0))
                .rounded_full()
                .bg(theme.background)
                .child(heroicon(icon).text_color(theme.icon)),
        )
        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(title))
        .child(
            div()
                .max_w(px(400.0))
                .text_center()
                .type_style(Type::Small)
                .text_color(theme.muted)
                .child(detail),
        )
        .child(
            div()
                .debug_selector(move || format!("{selector}-command"))
                .mt(px(4.0))
                .max_w_full()
                .child(super::bootstrap::command_box(0, command, theme)),
        )
        .into_any_element()
}

/// A sheet's danger action at the floating pane's trailing edge, under its
/// content, `<item>-sheet-cancel`: `label`, disabled while `busy`, with why
/// it last failed before it.
pub(crate) fn action(
    item: &'static str,
    label: &'static str,
    busy: bool,
    error: Option<String>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    theme: &Theme,
) -> AnyElement {
    let id = format!("{item}-sheet-cancel");
    let button = if busy {
        style::disabled_button(id, label, ButtonKind::Danger, ButtonSize::Small, theme)
    } else {
        style::button(id, label, ButtonKind::Danger, ButtonSize::Small, false, theme).on_click(on_click)
    };
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .justify_end()
        .gap_3()
        .px(px(INSET))
        .pt(px(16.0))
        .when_some(error, |d, e| {
            d.child(div().flex_1().min_w_0().truncate().type_style(Type::Small).text_color(theme.red).child(e))
        })
        .child(button)
        .into_any_element()
}
