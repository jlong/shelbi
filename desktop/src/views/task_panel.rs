//! The pieces the task panels share (`dt-figma-task-panels`): the Workspace
//! and Review panels that stand in the sidebar's column, drawn to the
//! Navigation component's State=Workspace (2220:18764) and State=Review
//! (2220:17158).
//!
//! Top to bottom: the header row, the back button beside the task's status
//! (its glyph and the status in caps) with Search and Toggle sidebar on the
//! right, the issue summary (the title, the description
//! clamped to five lines, More opening the task detail over the screen),
//! the info rows (the worktree with its Open button, the branch, the pull
//! request: the Workspace and Review frames 2787:33099 / 2787:33098),
//! the views (the sidebar's nav item with the sliding chip, numbered ⌃1 to
//! ⌃N), and, in the Review panel, its Actions. The window draws the
//! sidebar's titlebar and footer around a panel
//! ([`super::sidebar::render_panel_column`]).

use gpui::{div, prelude::*, px, AnyElement, Div, Hsla, SharedString, Stateful};
use shelbi_app::task_rows;
use shelbi_core::{IssuePr, PrCi, StatusCategory};

use super::sidebar::{self, nav_item};
use crate::assets::{glyph, glyphs, heroicon, icons, Icon};
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
        // Bands, not a shadow: a gpui shadow is a filled box under the
        // element and the fill is translucent, so the ring's blue would fill
        // the square. Focus adds the ring and nothing else.
        .when(keyboard, |d| d.children(style::focus_bands(NAV_RADIUS, theme)))
}

/// The panel's header row (the Navigation component's Toolbar in
/// State=Workspace 2220:18765 and State=Review): the back button, then the
/// status heading 12pt past it, 16pt in; then `search` and `toggle`
/// ([`super::sidebar::search_button`], [`super::sidebar::toggle_button`])
/// on the right, as the sidebar's header row draws them.
pub fn status_row(
    back: Stateful<Div>,
    status: Option<(StatusCategory, &str)>,
    search: Stateful<Div>,
    toggle: Stateful<Div>,
    theme: &Theme,
) -> Div {
    let lead = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(px(12.0))
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
        }));
    sidebar::header_row(lead, PANEL_HEADER_PAD_LEFT, search, toggle)
}

/// The task panels' header row starts 16pt in (the frames' back button).
const PANEL_HEADER_PAD_LEFT: f32 = 16.0;

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

/// A task's pull request as the task pane's PR property shows it: the link
/// and what's known of the PR, in words. (The panels' PR row is [`pr_row`].)
pub fn pr_value(link: Stateful<Div>, pr: &IssuePr, theme: &Theme) -> Div {
    let status = pr.state.map(|s| s.label());
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .min_w_0()
        .type_style(Type::Small)
        .child(link)
        .children(status.map(|s| {
            div()
                .debug_selector(|| "pr-state".into())
                .flex_none()
                .text_color(theme.muted)
                .child(s)
        }))
        .children(pr.ci.map(|ci| {
            let color = ci_color(ci, theme);
            div()
                .debug_selector(|| "pr-ci".into())
                .min_w_0()
                .truncate()
                .text_color(color)
                .child(format!("● {}", ci.label()))
        }))
}

/// A pull request's number as a link (`#2137 ↗`, the URL in its tooltip), for
/// the task pane's PR property; the caller adds the click. Without a URL it
/// reads as plain text, with nowhere to go.
pub fn pr_link(id: &'static str, pr: &IssuePr, keyboard: bool, theme: &Theme) -> Stateful<Div> {
    let d = div()
        .id(id)
        .debug_selector(move || id.into())
        .relative()
        .flex_none()
        .rounded(px(NAV_RADIUS));
    match &pr.url {
        Some(url) => d
            .text_color(theme.link)
            .hover(|s| s.underline())
            .cursor_pointer()
            .tooltip(style::tooltip(url.clone()))
            .child(format!("{} ↗", pr.number_label()))
            .when(keyboard, |d| d.children(style::focus_bands(NAV_RADIUS, theme))),
        None => d.text_color(theme.foreground).child(pr.number_label()),
    }
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

/// The info rows' block under the summary (Figma's "Review worktree"): the
/// worktree, the branch and the pull request, 12pt in, 8pt above and below,
/// 4pt apart.
pub fn info_rows(rows: impl IntoIterator<Item = AnyElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(4.0))
        .px(px(12.0))
        .py(px(8.0))
        .children(rows)
}

/// One info row's frame: `icon` (20pt, `icon/default`), 8pt, then the
/// caller's content; 6pt padding, 6pt corners. `keyboard` marks it the
/// keyboard's row with the selection fill.
fn info_row(d: Div, icon: Icon, keyboard: bool, theme: &Theme) -> Div {
    d.relative()
        .flex()
        .flex_row()
        .w_full()
        .min_w_0()
        .items_center()
        .gap(px(8.0))
        .p(px(6.0))
        .rounded(px(NAV_RADIUS))
        .type_style(Type::XSmall)
        .text_color(theme.foreground)
        .when(keyboard, |d| d.bg(theme.background))
        .child(heroicon(icon).text_color(theme.icon))
}

/// A clickable info row (the branch, the pull request): the row is the
/// button, deepening to the selection fill on hover.
fn clickable(id: &'static str, d: Div, theme: &Theme) -> Stateful<Div> {
    let hover = theme.background;
    d.id(id)
        .debug_selector(move || id.into())
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
}

/// The worktree row: the folder, the path ([`short_path`], the start
/// clipped first when even that doesn't fit) and the Open button the caller
/// makes, if any. The 24pt button overhangs the row's 20pt line by 2pt each
/// way, so the row keeps the frame's 32pt like the branch and PR rows.
/// `keyboard` marks it the keyboard's row.
pub fn worktree_row(selector: &'static str, path: &str, open: Option<Stateful<Div>>, keyboard: bool, theme: &Theme) -> Div {
    info_row(div().debug_selector(move || selector.into()), icons::FOLDER, keyboard, theme)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .child(short_path(path)),
        )
        .children(open.map(|button| div().flex().flex_none().my(px(-2.0)).child(button)))
}

/// The branch row: the git branch and the branch's name, cut at the end when
/// it doesn't fit. A click (the caller's) copies the name.
pub fn branch_row(id: &'static str, branch: &str, keyboard: bool, theme: &Theme) -> Stateful<Div> {
    let row = info_row(div(), icons::GIT_BRANCH, keyboard, theme).child(
        div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(branch.to_string()),
    );
    clickable(id, row, theme).tooltip(style::tooltip(format!("Copy {branch}")))
}

/// The pull request row: the pull request's icon, `PR #1234`, and one dot in
/// the CI's color (none while the CI is unknown). The state and CI are in the
/// tooltip with the URL, not words in the row. With a URL the row is a button
/// (the caller's click opens it); without one it has nowhere to go.
pub fn pr_row(id: &'static str, pr: &IssuePr, keyboard: bool, theme: &Theme) -> Stateful<Div> {
    let row = info_row(div(), icons::PULL_REQUEST, keyboard, theme)
        .child(div().flex_none().whitespace_nowrap().child(task_rows::pr_label(pr)))
        .children(pr.ci.map(|ci| {
            div()
                .debug_selector(|| "pr-ci-dot".into())
                .flex()
                .flex_none()
                .child(glyph(glyphs::CIRCLE_SOLID).text_color(ci_color(ci, theme)))
        }));
    let tip = task_rows::pr_tooltip(pr);
    if pr.url.is_some() {
        clickable(id, row, theme).tooltip(style::tooltip(tip))
    } else {
        row.id(id).debug_selector(move || id.into()).tooltip(style::tooltip(tip))
    }
}

/// A PR's CI as a color: passing green, pending yellow, failing red.
pub fn ci_color(ci: PrCi, theme: &Theme) -> Hsla {
    match ci {
        PrCi::Pending => theme.warning,
        PrCi::Passing => theme.success,
        PrCi::Failing => theme.error,
    }
}

/// The worktree's Open button: the X-Small Default button.
pub fn open_button(id: &'static str, theme: &Theme) -> Stateful<Div> {
    style::xsmall_button(id, "Open", theme).tooltip(style::tooltip("Show the worktree in Finder"))
}

/// The worktree row's button for a worktree on another machine, which
/// Finder can't show: it copies the worktree's `host:path` (`address`).
pub fn copy_button(id: &'static str, address: &str, theme: &Theme) -> Stateful<Div> {
    style::xsmall_button(id, "Copy", theme).tooltip(style::tooltip(format!("Copy {address}")))
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

/// One view: the sidebar's nav item with `icon`, `label` and its ⌃N hint
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
