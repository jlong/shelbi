//! The pieces the palette-only list screens share (Jobs, Reminders): the
//! title row (the view's name, loud, then its count, muted, on the Machines
//! toolbar's 37pt row), the scrolling bordered table whose rows sit under
//! section headings, the clickable row, the empty-state line, and a sheet's
//! danger action. The TUI's screens share the same parts in `shelbi-tui`'s
//! `list_screen`. (The sheets themselves are the floating pane's, shared
//! separately.)
//!
//! Debug selectors are named from the screen (`jobs-title`, `jobs-list`,
//! `jobs-finished-heading`) and its entry (`job-row-<key>`,
//! `reminder-sheet-cancel`), so each screen's tests find their own.

use gpui::{div, prelude::*, px, AnyElement, App, ClickEvent, Div, ScrollHandle, SharedString, Stateful, Window};

use super::floating_pane::INSET;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// A row's corner radius (a running job's estimate bar follows it).
pub(crate) const ROW_RADIUS: f32 = 6.0;

/// The gap between columns.
pub(crate) const COLUMN_GAP: f32 = 20.0;

/// The title row: `title`, loud, then `summary`, muted. Selectors
/// `<list>-title` and `<list>-summary`.
pub(crate) fn title_row(list: &'static str, title: &'static str, summary: Option<String>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_2()
        .h(px(37.0))
        .px_4()
        .child(
            div()
                .debug_selector(move || format!("{list}-title"))
                .type_style(Type::BaseMedium)
                .text_color(theme.text_loud)
                .child(title),
        )
        .when_some(summary, |d, summary| {
            d.child(
                div()
                    .debug_selector(move || format!("{list}-summary"))
                    .type_style(Type::XSmall)
                    .text_color(theme.muted)
                    .child(summary),
            )
        })
}

/// The line in place of the table (loading, empty, unavailable), centered.
pub(crate) fn message(selector: &'static str, text: String, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .child(style::empty_state(selector, text, None, theme).max_w_full())
        .into_any_element()
}

/// The table's column headings row; the caller adds one child per column.
pub(crate) fn headings_row() -> Div {
    div().flex().flex_row().gap(px(COLUMN_GAP)).px(px(12.0)).py(px(6.0))
}

/// `rows` with a divider between each two, and each `(index, text)`
/// heading above the row at that index, ruled off from the rows above it.
/// A heading's selector is `<list>-<text, lowercased>-heading`.
pub(crate) fn sectioned(
    list: &'static str,
    rows: Vec<AnyElement>,
    headings: &[(usize, &'static str)],
    theme: &Theme,
) -> Vec<AnyElement> {
    let mut out = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if let Some((_, text)) = headings.iter().find(|(at, _)| *at == i) {
            if i > 0 {
                out.push(div().h(px(1.0)).bg(theme.divider_strong).into_any_element());
            }
            out.push(section_heading(list, text, theme));
            out.push(div().h(px(1.0)).bg(theme.divider).into_any_element());
        } else if i > 0 {
            out.push(div().h(px(1.0)).bg(theme.divider).into_any_element());
        }
        out.push(row);
    }
    out
}

/// A section row (`Finished`, `Pending`), in the headings' type.
fn section_heading(list: &'static str, text: &'static str, theme: &Theme) -> AnyElement {
    div()
        .debug_selector(move || format!("{list}-{}-heading", text.to_lowercase()))
        .px(px(12.0))
        .py(px(6.0))
        .type_style(Type::SmallMedium)
        .text_color(theme.form_hint)
        .child(text)
        .into_any_element()
}

/// The scrolling list holding the bordered table (Settings' table look).
/// Selectors `<list>-list` and `<list>-table`.
pub(crate) fn list(list: &'static str, scroll: &ScrollHandle, headings: Div, rows: Vec<AnyElement>, theme: &Theme) -> AnyElement {
    div()
        .id(SharedString::from(format!("{list}-list")))
        .debug_selector(move || format!("{list}-list"))
        .track_scroll(scroll)
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .px_4()
        .py_4()
        .child(super::workflows::table(headings, rows, theme).debug_selector(move || format!("{list}-table")))
        .into_any_element()
}

/// One entry's clickable row, `<item>-row-<key>`: highlighted while its
/// sheet is open and on hover; the caller adds the cells and the click.
pub(crate) fn row(item: &'static str, key: &str, open: bool, theme: &Theme) -> Stateful<Div> {
    let selector = format!("{item}-row-{key}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .relative()
        .cursor_pointer()
        .rounded(px(ROW_RADIUS))
        .when(open, |d| d.bg(theme.background_hover))
        .hover(|d| d.bg(theme.background_hover))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(COLUMN_GAP))
        .px(px(12.0))
        .py(px(10.0))
        .type_style(Type::Base)
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
