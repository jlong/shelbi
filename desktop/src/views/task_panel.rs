//! The pieces the task panels share (`dt-figma-task-panels`): the Workspace
//! and Review panels that stand in the sidebar's column, drawn to the
//! Navigation component's State=Workspace (2220:18764) and State=Review
//! (2220:17158).
//!
//! Top to bottom: the back button beside the task's status (its glyph and
//! the status in caps), the issue summary (the title, the description
//! clamped to five lines, More opening the task detail over the screen),
//! the worktree with its Open button,
//! the views (the sidebar's nav item with the sliding chip, numbered ⌘1 to
//! ⌘N), and, in the Review panel, its Actions. The window draws the
//! sidebar's titlebar and footer around a panel
//! ([`super::sidebar::render_panel_column`]).

use gpui::{div, prelude::*, px, AnyElement, Div, SharedString, Stateful};
use shelbi_core::StatusCategory;

use super::sidebar::nav_item;
use crate::assets::{heroicon, icons, Icon};
use crate::elements::clamped_text::clamped_text;
use crate::keys::row_hotkey;
use crate::theme::motion::fill_layer;
use crate::theme::style::{self, Type, Typography, NAV_RADIUS};
use crate::theme::Theme;

/// The back button's square.
pub const BACK_BUTTON: f32 = 28.0;

/// How many lines of the description the panel shows (More opens the rest in
/// the task detail).
pub const DESCRIPTION_LINES: usize = 5;

/// The back button: a 28pt square on the overlay fill (deepening on hover
/// and press), 6pt corners, the left arrow. `keyboard` marks it the
/// keyboard's row with the focus ring and glow.
pub fn back_button(id: &'static str, keyboard: bool, theme: &Theme) -> Stateful<Div> {
    let radius = px(NAV_RADIUS);
    let d = div()
        .id(id)
        .debug_selector(move || id.into())
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(BACK_BUTTON))
        .rounded(radius)
        .cursor_pointer();
    fill_layer(d, id, radius, [theme.background, theme.background_hover, theme.background_active])
        .child(heroicon(icons::ARROW_LEFT).text_color(theme.button_icon))
        .when(keyboard, |d| d.shadow(style::focus_shadow(theme)))
}

/// The panel's first row (Figma's "Project and search"): the back button,
/// then the status heading, 16pt in, 8pt down, 12pt apart.
pub fn status_row(back: Stateful<Div>, status: Option<(StatusCategory, &str)>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(8.0))
        .child(back)
        .children(status.map(|(category, label)| {
            div()
                .debug_selector(|| "task-panel-status".into())
                .flex()
                .flex_row()
                .items_center()
                .h(px(24.0))
                .px(px(4.0))
                .min_w_0()
                .child(style::status_heading_in(
                    style::panel_status_icon(category, theme),
                    label.to_string(),
                    theme.foreground,
                ))
        }))
}

/// The issue summary: the title (Base/Medium, loud), the description
/// (X-Small on an 18pt line, clamped to [`DESCRIPTION_LINES`] with an
/// ellipsis), and the More link the caller makes ([`more_link`]); 18pt in,
/// 8pt apart.
pub fn summary(
    selectors: (&'static str, &'static str),
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    more: Option<Stateful<Div>>,
    theme: &Theme,
) -> Div {
    let (title_selector, description_selector) = selectors;
    let description: SharedString = description.into();
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(8.0))
        .px(px(18.0))
        .py(px(8.0))
        .child(
            div()
                .debug_selector(move || title_selector.into())
                .type_style(Type::BaseMedium)
                .text_color(theme.text_loud)
                .child(title.into()),
        )
        .when(!description.is_empty(), |d| {
            d.child(
                div()
                    .debug_selector(move || description_selector.into())
                    .type_style(Type::XSmall)
                    .line_height(px(18.0))
                    .text_color(theme.foreground)
                    .child(clamped_text(description, DESCRIPTION_LINES)),
            )
        })
        .children(more)
}

/// The More link under the description, which opens the task detail:
/// X-Small / Medium in `text/link`, the hover color on hover. `keyboard`
/// marks it the keyboard's row with the selection fill.
pub fn more_link(id: &'static str, keyboard: bool, theme: &Theme) -> Stateful<Div> {
    let hover = theme.link_hover;
    div()
        .id(id)
        .debug_selector(move || id.into())
        .relative()
        .flex()
        .flex_none()
        .w_full()
        .cursor_pointer()
        .type_style(Type::XSmallMedium)
        .text_color(theme.link)
        .hover(move |s| s.text_color(hover))
        .when(keyboard, |d| {
            // The selection fill as the worktree row draws it, 12pt in: a
            // layer under the label, out of the layout so nothing moves.
            d.child(
                div()
                    .absolute()
                    .left(px(-6.0))
                    .right(px(-6.0))
                    .top(px(-4.0))
                    .bottom(px(-4.0))
                    .rounded(px(NAV_RADIUS))
                    .bg(theme.background),
            )
        })
        .child("More")
}

/// A worktree path as the panel shows it: its last four components after
/// `…/` (`…/shelbi/.shelbi/wt/alpha`, the repository, the worktrees and the
/// workspace), or the path itself when it is that short.
pub fn short_path(path: &str) -> String {
    const KEEP: usize = 4;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() <= KEEP {
        return path.to_string();
    }
    format!("…/{}", parts[parts.len() - KEEP..].join("/"))
}

/// The worktree row: the folder, the path ([`short_path`], the start
/// clipped first when even that doesn't fit) and the Open button the caller
/// makes, if any; 12pt in, `pad_y` above and below (State=Workspace draws
/// 8pt, State=Review none). `keyboard` marks it the keyboard's row.
pub fn worktree_row(
    selector: &'static str,
    path: &str,
    open: Option<Stateful<Div>>,
    keyboard: bool,
    pad_y: f32,
    theme: &Theme,
) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .px(px(12.0))
        .py(px(pad_y))
        .child(
            div()
                .debug_selector(move || selector.into())
                .relative()
                .flex()
                .flex_row()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(8.0))
                .p(px(6.0))
                .rounded(px(NAV_RADIUS))
                .when(keyboard, |d| d.bg(theme.background))
                .child(heroicon(icons::FOLDER).text_color(theme.icon))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_start()
                        .type_style(Type::XSmall)
                        .text_color(theme.foreground)
                        .child(short_path(path)),
                )
                .children(open),
        )
}

/// The worktree's Open button: the X-Small Default button.
pub fn open_button(id: &'static str, theme: &Theme) -> Stateful<Div> {
    style::xsmall_button(id, "Open", theme).tooltip(style::tooltip("Show the worktree in Finder"))
}

/// The views' block: 12pt in, 24pt above and below, the rows 4pt apart.
/// Relative, for the sliding chip.
pub fn views_block() -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(4.0))
        .px(px(12.0))
        .py(px(24.0))
}

/// One view: the sidebar's nav item with `icon`, `label` and its ⌘N hint
/// (or `trailing` in its place). `d` comes painted with its state.
pub fn view_row(
    d: Stateful<Div>,
    icon: Icon,
    label: impl Into<SharedString>,
    shortcut: Option<u8>,
    trailing: Option<AnyElement>,
    current: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let trailing = trailing.or_else(|| shortcut.map(|n| style::hotkey(&row_hotkey(n), theme).into_any_element()));
    nav_item(d, icon, label, trailing, current, theme)
}

/// A section header inside a panel (Actions): Small/Medium muted, 8pt above
/// and 4pt below, 20pt in.
pub fn section_header(label: &'static str, theme: &Theme) -> Div {
    div().flex().flex_none().px(px(12.0)).child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .px(px(8.0))
            .pt(px(8.0))
            .pb(px(4.0))
            .type_style(Type::SmallMedium)
            .text_color(theme.muted)
            .child(label),
    )
}

/// A note at the panel's foot (a failure, the status line): X-Small, 16pt in.
pub fn note(selector: &'static str, line: String, color: gpui::Hsla) -> Div {
    div()
        .debug_selector(move || selector.into())
        .flex_none()
        .px(px(16.0))
        .py(px(4.0))
        .type_style(Type::XSmall)
        .text_color(color)
        .child(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worktree_keeps_its_last_four_components() {
        assert_eq!(
            short_path("/Users/jlong/Workspaces/shelbi/.shelbi/wt/alpha"),
            "…/shelbi/.shelbi/wt/alpha"
        );
        assert_eq!(short_path("/srv/wt/alpha"), "/srv/wt/alpha");
        assert_eq!(short_path(""), "");
    }
}
