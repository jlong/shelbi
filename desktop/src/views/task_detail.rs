//! The task pane: an issue's properties and its Markdown body, floating over
//! the right of the main area, after the Figma Task pane frames (In progress
//! 2172:722, Ready to start 2573:10737).
//!
//! The pane isn't modal. It sits [`EDGE`] in from the main area's top, right
//! and bottom ([`PANE_W`] wide at most, narrower in a window that can't fit
//! it), on an overlay layer over the board, which keeps its width, scroll and
//! selection under it and stays usable where the pane doesn't cover it: a
//! click on another card shows that card here. It slides in from the right
//! as it opens ([`SLIDE`], gpui's animation, so Reduce Motion shows it in
//! place). The same pane shows over Review and Workspace (their More), drawn
//! by a floating board ([`IssuesView::open_task_over`]).
//!
//! Inside, a 12pt-cornered canvas with a divider edge and the frame's drop:
//! the Header (52pt, `Workflow / id`, More, Close), then a scroll of the
//! title (Desktop/Title), the Status / Workflow / Priority / Branch
//! properties (and, when the task has them, its tracker owner, dependencies
//! and pull request), a divider, the body ([`markdown::body`]), and the
//! created / updated dates. The task operations the old modal put on its
//! action row are in More ([`PaneAction`]) and keep their keys. The live
//! session (`dt-task-pane-session`) takes the pane's bottom, under the
//! scroll; an idle task's details fill the pane.
//!
//! Loading, a task no longer on the board, and a board that failed to read
//! show in the same shell.

use gpui::{
    div, prelude::*, px, Animation, AnimationExt, AnyElement, BoxShadow, ClickEvent, Context, Div, ElementId,
    SharedString, Stateful,
};
use gpui_kit::component::input::Textarea;
use shelbi_app::TaskDetailModel;
use shelbi_core::StatusCategory;
use shelbi_state::keymap::PopoverAction;

use super::board_state::{Mode, PaneAction};
use super::column_swap::SLIDE;
use super::issues::IssuesView;
use super::markdown;
use super::workflow_editor::{dropdown, MENU_W};
use crate::assets::{heroicon, icons, Icon};
use crate::elements::terminal::metrics::PRIMARY_FAMILY;
use crate::theme::motion::{ease_out_expo, fill_layer};
use crate::theme::style::{self, ButtonContent, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The pane's room from the main area's top, right and bottom edges.
pub const EDGE: f32 = 8.0;
/// The pane's width where the window has room for it.
pub const PANE_W: f32 = 640.0;
/// The pane's corners.
const RADIUS: f32 = 12.0;
/// The Header's height.
const HEADER_H: f32 = 52.0;
/// The content's inset and the gap between its sections.
const INSET: f32 = 20.0;
/// A property's label column.
const LABEL_W: f32 = 108.0;
/// A Header button (More, Close): 28pt, a 20pt icon.
const HEADER_BUTTON: f32 = 28.0;

/// What the pane shows.
pub enum PaneContent<'a> {
    Task(&'a TaskDetailModel),
    /// The board hasn't read the task yet.
    Loading,
    /// The board has read, and the task isn't on it.
    Missing,
    /// The board's read failed.
    Failed(String),
}

/// The pane for task `id`, on its overlay layer over the main area.
pub fn render(view: &IssuesView, id: &str, content: PaneContent<'_>, theme: &Theme, cx: &mut Context<IssuesView>) -> AnyElement {
    // Opaque over a translucent window.
    let theme = &theme.opaque();
    let title = match &content {
        PaneContent::Task(d) => format!("{} / {}", workflow_label(&d.workflow_name), d.id),
        _ => format!("Task / {id}"),
    };
    let body: AnyElement = match content {
        PaneContent::Task(d) => details(view, d, theme, cx).into_any_element(),
        PaneContent::Loading => state_note(theme, "Loading the task…", None).into_any_element(),
        PaneContent::Missing => {
            state_note(theme, "This task is no longer on the board.", Some("It may have been deleted or archived.".into()))
                .into_any_element()
        }
        PaneContent::Failed(e) => {
            state_note(theme, "The board couldn't be read.", Some(e)).into_any_element()
        }
    };
    let pane = div()
        .id("task-pane")
        .debug_selector(|| "task-pane".into())
        .relative()
        .flex()
        .flex_col()
        .h_full()
        .w(px(PANE_W))
        .min_w_0()
        .overflow_hidden()
        .bg(theme.canvas)
        .rounded(px(RADIUS))
        .shadow(pane_shadow(theme))
        .type_style(Type::Base)
        .text_color(theme.foreground)
        // The board under the pane gets none of its pointer.
        .occlude()
        .child(header(view, title, theme, cx))
        .child(body)
        // The edge, drawn over the content inside the pane's bounds (Figma
        // strokes inside), so it takes no room from the layout.
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(theme.divider),
        );
    let pane = view.pane_focus(pane, cx);
    div()
        .absolute()
        .top(px(EDGE))
        .right(px(EDGE))
        .bottom(px(EDGE))
        .left(px(EDGE))
        .flex()
        .flex_row()
        .justify_end()
        .child(pane.with_animation(
            "task-pane-in",
            Animation::new(SLIDE).with_easing(ease_out_expo),
            // In from past the window's right edge.
            |pane, t| pane.left(px((1.0 - t) * (PANE_W + EDGE))),
        ))
        .into_any_element()
}

/// The frame's elevation: a 32pt drop 8pt down, in the shadow role.
fn pane_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.0), px(8.0), theme.shadow).blur_radius(style::figma_blur(32.0))]
}

/// A workflow's name as the pane spells it (`task` → `Task`).
fn workflow_label(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Task".to_string(),
    }
}

/// The Header: `Workflow / id`, then More and Close.
fn header(view: &IssuesView, title: String, theme: &Theme, cx: &mut Context<IssuesView>) -> Div {
    let km = view.state.keymaps();
    let close_hint = view.hint(km.popover.first_chord_for(PopoverAction::Close));
    let cursor = match view.state.mode {
        Mode::PaneMenu { cursor } => Some(cursor),
        _ => None,
    };
    let menu_open = cursor.is_some();
    // What the menu lists, read now (its rows are built while this view
    // renders, so they can't read it then).
    let rows: Vec<(PaneAction, String)> = if menu_open {
        view.state.pane_actions().into_iter().map(|a| (a, action_hint(view, a))).collect()
    } else {
        Vec::new()
    };
    let more = dropdown(
        menu_open,
        &view.pane_menu_anchor,
        "pane-menu",
        header_button("pane-more", icons::ELLIPSIS_VERTICAL, menu_open, theme).tooltip(style::tooltip("More")),
        move |cx| menu_items(rows, cursor, cx),
        MENU_W,
        true,
        |this: &mut IssuesView, cx| {
            this.state.toggle_pane_menu();
            cx.notify();
        },
        |this: &mut IssuesView, _| {
            if matches!(this.state.mode, Mode::PaneMenu { .. }) {
                this.state.close_overlay();
            }
        },
        cx,
    );
    let close = header_button("pane-close", icons::X_MARK, false, theme)
        .tooltip(style::tooltip(format!("Close ({close_hint})")))
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_pane(cx)));
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .h(px(HEADER_H))
        .px(px(12.0))
        .bg(theme.toolbar)
        // The Header's 1px canvas edge on its left.
        .border_l_1()
        .border_color(theme.canvas)
        .child(
            div()
                .debug_selector(|| "pane-heading".into())
                .flex_1()
                .min_w_0()
                .truncate()
                .type_style(Type::BaseSemibold)
                .text_color(theme.text_loud)
                .child(title),
        )
        .child(div().flex().flex_row().flex_none().items_center().gap(px(4.0)).child(more).child(close))
}

/// A Header button: 28pt with 8pt corners, the 20pt icon in the icon role,
/// a fill fading in (and a loud icon) on hover, held while `active`.
fn header_button(id: &'static str, icon: Icon, active: bool, theme: &Theme) -> Stateful<Div> {
    let radius = px(8.0);
    let loud = theme.text_loud;
    let rest = if active { theme.background_hover } else { theme.background.opacity(0.0) };
    let d = div()
        .id(id)
        .debug_selector(move || id.into())
        .group(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(HEADER_BUTTON))
        .rounded(radius)
        .cursor_pointer();
    fill_layer(d, id, radius, [rest, theme.background_hover, theme.background_active])
        .child(heroicon(icon).text_color(theme.icon).group_hover(id, move |s| s.text_color(loud)))
}

/// The More menu's rows: each action and the key that does the same.
fn menu_items(rows: Vec<(PaneAction, String)>, cursor: Option<usize>, cx: &mut Context<IssuesView>) -> Vec<AnyElement> {
    let theme = cx.global::<Theme>().opaque();
    rows.into_iter()
        .enumerate()
        .map(|(ix, (action, hint))| {
            style::menu_row(format!("pane-menu-{}", action.slug()), cursor == Some(ix), &theme)
                .child(div().flex_1().min_w_0().truncate().child(action.label()))
                .child(div().flex_none().type_style(Type::XSmall).text_color(theme.muted).child(hint))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    let intents = this.state.choose_pane_action(ix);
                    this.apply_intents(intents, window, cx);
                    cx.notify();
                }))
                .into_any_element()
        })
        .collect()
}

/// A loading, missing or failed pane's note, at the content's inset.
fn state_note(theme: &Theme, title: &'static str, detail: Option<String>) -> Div {
    div()
        .debug_selector(|| "pane-state".into())
        .flex()
        .flex_col()
        .gap(px(8.0))
        .p(px(INSET))
        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(title))
        .children(detail.map(|d| div().type_style(Type::Small).text_color(theme.muted).child(d)))
}

/// A loaded task: the scrolling details.
fn details(view: &IssuesView, d: &TaskDetailModel, theme: &Theme, cx: &mut Context<IssuesView>) -> Stateful<Div> {
    let editing = view.editor.as_ref().is_some_and(|(id, _)| *id == d.id);
    let pending = view.state.pending(&d.id).map(|p| p.label.clone());
    let notice = view.state.notice(&d.id).map(str::to_string);

    let title = div()
        .debug_selector(|| "pane-title".into())
        .w_full()
        .min_w_0()
        .type_style(Type::Title)
        .text_color(theme.text_loud)
        .child(d.title.clone());

    let status_rows = (pending.is_some() || notice.is_some()).then(|| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .when_some(pending, |d, p| d.child(div().type_style(Type::XSmall).text_color(theme.muted).child(p)))
            .when_some(notice, |d, n| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap_1()
                        .type_style(Type::XSmall)
                        .text_color(theme.error)
                        .child(heroicon(icons::EXCLAMATION_TRIANGLE).text_color(theme.error))
                        .child(n),
                )
            })
    });

    let properties = properties(d, theme);

    let body = if let (true, Some((_, state))) = (editing, view.editor.as_ref()) {
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(div().h(px(320.0)).w_full().child(Textarea::new(state).h_full()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        action(theme, "pane-save", ButtonContent::label("Save").hotkey("⌘S"), ButtonKind::Success)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_editor(cx))),
                    )
                    .child(
                        action(theme, "pane-discard", ButtonContent::label("Discard"), ButtonKind::Quiet)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_editor(cx))),
                    ),
            )
            .into_any_element()
    } else if d.body.trim().is_empty() {
        div()
            .debug_selector(|| "pane-empty".into())
            .type_style(Type::Base)
            .text_color(theme.muted)
            .child("No description yet.")
            .into_any_element()
    } else {
        div()
            .w_full()
            .min_w_0()
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(
                markdown::body(ElementId::Name(SharedString::from(format!("body-{}", d.id))), d.body.clone(), theme)
                    .w_full()
                    .selectable(true),
            )
            .into_any_element()
    };

    let dates = div()
        .debug_selector(|| "pane-dates".into())
        .type_style(Type::XSmall)
        .text_color(theme.muted)
        .child(format!("Created {}  ·  Updated {}", short_date(&d.created), short_date(&d.updated)));

    div()
        .id("pane-scroll")
        .debug_selector(|| "pane-scroll".into())
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .track_scroll(&view.detail_scroll)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(INSET))
                .p(px(INSET))
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(16.0))
                        .w_full()
                        .min_w_0()
                        .child(title)
                        .children(status_rows)
                        .child(properties),
                )
                .child(div().flex_none().h(px(1.0)).w_full().bg(theme.divider))
                .child(body)
                .child(dates),
        )
}

/// The property table: Status, Workflow, Priority and Branch, then the
/// task's tracker owner, dependencies and pull request where it has them.
fn properties(d: &TaskDetailModel, theme: &Theme) -> Div {
    let mut rows = div()
        .debug_selector(|| "pane-properties".into())
        .flex()
        .flex_col()
        .gap(px(6.0))
        .w_full()
        .min_w_0();
    rows = rows.child(property(theme, "Status", value_button("pane-status", Some(d.category), d.status_name.clone(), theme)));
    rows = rows.child(property(
        theme,
        "Workflow",
        value_button("pane-workflow", None, workflow_label(&d.workflow_name), theme),
    ));
    rows = rows.child(property(theme, "Priority", value_button("pane-priority", None, format!("{} in queue", d.priority), theme)));
    rows = rows.child(property(
        theme,
        "Branch",
        match &d.branch {
            Some(b) => value_text(theme)
                .debug_selector(|| "pane-branch".into())
                .font_family(PRIMARY_FAMILY)
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(theme.foreground)
                .child(b.clone()),
            None => value_text(theme).text_color(theme.muted).child("Created when work starts"),
        },
    ));
    // Tracker ownership, on a remote tracker only: whose the issue is, and for
    // one that isn't the user's, how to claim it (Start isn't offered).
    if let Some(owner) = &d.tracker_owner {
        let text = if owner.is_mine() {
            "Me".to_string()
        } else {
            format!("{}  ·  not yours: `shelbi issue start {} --take` claims it", owner.label(), d.id)
        };
        rows = rows.child(property(theme, "Owner", value_text(theme).child(text)));
    }
    if !d.depends_on.is_empty() {
        let deps = d.depends_on.iter().map(dep_text).collect::<Vec<_>>().join(", ");
        let label = if d.blocked { "Blocked by" } else { "Depends on" };
        rows = rows.child(property(theme, label, value_text(theme).debug_selector(|| "pane-deps".into()).child(deps)));
    }
    if let Some(url) = d.pr_link.clone() {
        let open = url.clone();
        rows = rows.child(property(
            theme,
            "Pull request",
            value_text(theme).child(
                div()
                    .id("pr-link")
                    .debug_selector(|| "pr-link".into())
                    .min_w_0()
                    .truncate()
                    .text_color(theme.link)
                    .hover(|s| s.underline())
                    .cursor_pointer()
                    .tooltip(style::tooltip(url.clone()))
                    .child(url)
                    .on_click(move |_, _, cx| cx.open_url(&open)),
            ),
        ));
    }
    rows
}

/// A property row: 28pt, the label in its muted column, 12pt before the
/// value.
fn property(theme: &Theme, label: &'static str, value: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .min_h(px(28.0))
        .w_full()
        .min_w_0()
        .child(
            div()
                .w(px(LABEL_W))
                .flex_none()
                .type_style(Type::Small)
                .text_color(theme.muted)
                .child(label),
        )
        .child(div().flex().flex_row().items_center().flex_1().min_w_0().child(value))
}

/// A text value, 8pt in so it lines up with the buttons' labels' edge.
fn value_text(theme: &Theme) -> Div {
    div()
        .min_w_0()
        .px(px(8.0))
        .truncate()
        .type_style(Type::Small)
        .text_color(theme.foreground)
}

/// A property's value as the frame draws it: the Button at Small in the
/// Quiet style, an optional status glyph, the value and a chevron. Choosing
/// a value belongs to the pane's property editors (a later task), so this
/// is the face only: no hover, no press.
fn value_button(id: &'static str, icon: Option<StatusCategory>, label: impl Into<SharedString>, theme: &Theme) -> Div {
    let size = ButtonSize::Small;
    let c = style::button_colors(ButtonKind::Quiet, theme);
    div()
        .debug_selector(move || id.into())
        .flex()
        .flex_row()
        .flex_none()
        .min_w_0()
        .items_center()
        .h(px(size.height()))
        .px(px(size.padding()))
        .gap(px(size.gap()))
        .rounded(px(6.0))
        .type_style(size.text())
        .text_color(c.text)
        .children(icon.map(|cat| style::left_slot(size, style::status_icon_large(cat, theme))))
        .child(div().min_w_0().truncate().child(label.into()))
        .child(style::dropdown_slot(size, c.icon))
}

/// A compact action under the body editor.
fn action(theme: &Theme, id: &'static str, content: ButtonContent, kind: ButtonKind) -> Stateful<Div> {
    style::content_button(id, content, kind, ButtonSize::XSmall, false, theme)
}

/// A `YYYY-MM-DD HH:MM UTC` stamp as the pane shows it: `Oct 26`, with the
/// year when it isn't this one. A stamp that doesn't parse shows as it is.
pub fn short_date(stamp: &str) -> String {
    short_date_in(stamp, this_year())
}

fn short_date_in(stamp: &str, this_year: i32) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let date = stamp.split_whitespace().next().unwrap_or(stamp);
    let mut parts = date.split('-');
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return stamp.to_string();
    };
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i32>(), m.parse::<usize>(), d.parse::<u32>()) else {
        return stamp.to_string();
    };
    let Some(month) = m.checked_sub(1).and_then(|i| MONTHS.get(i)) else {
        return stamp.to_string();
    };
    if y == this_year {
        format!("{month} {d}")
    } else {
        format!("{month} {d}, {y}")
    }
}

/// The current UTC year.
fn this_year() -> i32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    // Days since the epoch to a civil year (Howard Hinnant's algorithm).
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2)) as i32
}

/// One dependency as the pane lists it: `id [status]`, a `✓` once it's
/// done (the TUI colors a done dependency green; the desktop is
/// monochrome, so the mark carries it), `[missing]` for a dependency the
/// board doesn't have.
fn dep_text(dep: &shelbi_app::view::DepBadge) -> String {
    match (&dep.status, dep.done) {
        (Some(status), true) => format!("{} [{status}] ✓", dep.id),
        (Some(status), false) => format!("{} [{status}]", dep.id),
        (None, _) => format!("{} [missing]", dep.id),
    }
}

/// The hint the More menu shows beside `action`.
pub(super) fn action_hint(view: &IssuesView, action: PaneAction) -> String {
    let km = view.state.keymaps();
    match action {
        PaneAction::MoveLeft => view.hint(km.popover.first_chord_for(PopoverAction::MoveLeft)),
        PaneAction::MoveRight => view.hint(km.popover.first_chord_for(PopoverAction::MoveRight)),
        PaneAction::OpenWorkspace => view.hint(km.popover.first_chord_for(PopoverAction::OpenWorkspace)),
        PaneAction::Start => "s".into(),
        PaneAction::Assign => "a".into(),
        PaneAction::EditDescription => "e".into(),
        PaneAction::OpenInEditor => "⇧E".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_shows_its_month_and_day_and_an_old_one_its_year() {
        assert_eq!(short_date_in("2026-10-26 14:02 UTC", 2026), "Oct 26");
        assert_eq!(short_date_in("2026-01-03 00:00 UTC", 2026), "Jan 3");
        assert_eq!(short_date_in("2025-12-31 23:59 UTC", 2026), "Dec 31, 2025");
        assert_eq!(short_date_in("yesterday", 2026), "yesterday");
        assert_eq!(short_date_in("2026-13-01 00:00 UTC", 2026), "2026-13-01 00:00 UTC");
    }

    #[test]
    fn this_year_is_a_plausible_year() {
        assert!((2024..2200).contains(&this_year()));
    }

    #[test]
    fn a_workflow_name_is_capitalized_for_the_header() {
        assert_eq!(workflow_label("task"), "Task");
        assert_eq!(workflow_label("Subtask"), "Subtask");
        assert_eq!(workflow_label(""), "Task");
    }
}
