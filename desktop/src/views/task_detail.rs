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
//! action row are in More ([`PaneAction`]) and keep their keys.
//!
//! Status, Workflow and Priority are edited in place, with no edit mode
//! (`dt-task-inline-properties`): each value is the Button at Small in the
//! Ghost style, and a click (or Tab to it, then Enter) opens a picker
//! anchored under it of what the task can be set to now
//! (`shelbi_app::task_properties`): the workflow's statuses with their
//! glyphs, the configured workflows, the places in the status's queue. A
//! choice is sent at once as the board's own command (a move runs the
//! workflow's gates), the value shows where it is headed while it settles,
//! and a refusal shows under the title. Branch is selectable text, and More
//! copies it.
//!
//! The body's in-place editor (More > Edit description) works on the issue's
//! draft (`shelbi_app::issue_edit`), which saves itself a moment after typing
//! stops (Cmd+S saves at once; Done, Esc and Close leave the editor, saving,
//! never reverting). A quiet line under the title says where the save
//! stands, with Retry for a failed one and both choices for a change made
//! elsewhere at the same place.
//!
//! A task an agent is working shows that agent's live session at the
//! pane's bottom (`dt-task-pane-session`, [`super::pane_session`]): a 17pt
//! resize handle under the details (a 32 x 4 grip, dragged, or ↑ / ↓ once
//! it has the keys, ⌥↑ / ⌥↓ from the pane), then the 356pt session area: a
//! 44pt header with the workspace's state glyph, its name and Open
//! Workspace, over the terminal itself. The details and the terminal
//! scroll on their own. An idle task's details fill the pane.
//!
//! Loading, a task no longer on the board, and a board that failed to read
//! show in the same shell.

use gpui::{
    canvas, div, prelude::*, px, Animation, AnimationExt, AnyElement, BoxShadow, ClickEvent, Context, CursorStyle,
    Div, ElementId, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollHandle, SharedString,
    Stateful, Window,
};
use gpui_kit::base::TextView;
use gpui_kit::component::input::Textarea;
use shelbi_app::issue_edit::{Field, Resolution, SaveStatus};
use shelbi_app::task_session::{TaskSession, TaskSessionState};
use shelbi_app::TaskDetailModel;
use shelbi_core::StatusCategory;
use shelbi_state::keymap::PopoverAction;

use super::board_state::{Mode, PaneAction, PaneProperties, Property, PropertyRow};
use super::column_swap::SLIDE;
use super::issues::IssuesView;
use super::markdown;
use super::pane_session::PaneSession;
use super::pane_split::{GRIP_H, GRIP_RADIUS, GRIP_W, HANDLE_H, SESSION_H, SESSION_HEADER_H};
use super::sidebar::{render_glyph, review_glyph, workspace_glyph};
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
        PaneContent::Task(d) => {
            let details = details(view, d, theme, cx);
            match view.pane_session.as_ref().and_then(|p| p.attached().map(|s| (p, s.clone()))) {
                Some((p, session)) => with_session(details, p, &session, theme, cx),
                None => details.into_any_element(),
            }
        }
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
    // A draft left unsaved (its save failed, or is still out) shows instead
    // of the read: that's the text the editor will open on.
    let shown_body = view.state.draft_text(&d.id, Field::Body).unwrap_or_else(|| d.body.clone());

    let title = div()
        .debug_selector(|| "pane-title".into())
        .w_full()
        .min_w_0()
        .type_style(Type::Title)
        .text_color(theme.text_loud)
        .child(d.title.clone());

    let save_row = save_row(view, &d.id, theme, cx);
    let status_rows = (pending.is_some() || notice.is_some() || save_row.is_some()).then(|| {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(save_row)
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

    let properties = properties(view, d, theme, cx);

    let body = if let (true, Some((_, state))) = (editing, view.editor.as_ref()) {
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(div().h(px(320.0)).w_full().child(Textarea::new(state).h_full()))
            // No Save: the draft saves itself. Done leaves the editor.
            .child(
                div().flex().flex_row().gap_2().child(
                    action(theme, "pane-done", ButtonContent::label("Done").hotkey("Esc"), ButtonKind::Ghost)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.finish_editor(cx))),
                ),
            )
            .into_any_element()
    } else if shown_body.trim().is_empty() {
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
                markdown::body(ElementId::Name(SharedString::from(format!("body-{}", d.id))), shown_body, theme)
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

/// The details over the task's live session: the details' scroll, the
/// resize handle, and the session area, in the column under the Header
/// (measured for the drag and the clamp).
fn with_session(
    details: Stateful<Div>,
    p: &PaneSession,
    session: &TaskSession,
    theme: &Theme,
    cx: &mut Context<IssuesView>,
) -> AnyElement {
    let session_h = p.bounds.map_or(SESSION_H, |b| p.split.session_height(b.size.height.as_f32()));
    let this = cx.entity().downgrade();
    let stop_drag = |this: &mut IssuesView, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<IssuesView>| {
        if let Some(p) = this.pane_session.as_mut().filter(|p| p.dragging) {
            p.dragging = false;
            cx.notify();
        }
    };
    div()
        .id("pane-split")
        .debug_selector(|| "pane-split".into())
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
            if let Some(p) = this.pane_session.as_mut() {
                if let (true, Some(b)) = (p.dragging, p.bounds) {
                    p.split.drag_to((ev.position.y - b.origin.y).as_f32(), b.size.height.as_f32());
                    cx.notify();
                }
            }
        }))
        .on_mouse_up(MouseButton::Left, cx.listener(stop_drag))
        .on_mouse_up_out(MouseButton::Left, cx.listener(stop_drag))
        .child(details)
        .child(split_handle(p, theme, cx))
        .child(
            div()
                .id("pane-session")
                .debug_selector(|| "pane-session".into())
                .flex()
                .flex_col()
                .flex_none()
                .h(px(session_h))
                .w_full()
                .overflow_hidden()
                // The terminal's own surface, the pane's canvas at the
                // default contrast, so the grid's insets match the grid.
                .bg(theme.terminal_canvas())
                .child(session_header(session, theme, cx))
                .child(div().flex().flex_1().min_h_0().w_full().child(p.view().clone())),
        )
        // Measures the column for the drag and the clamp; repaints once
        // when its size first becomes known or changes.
        .child(
            canvas(
                move |bounds, _, cx| {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(p) = this.pane_session.as_mut() {
                            if p.bounds != Some(bounds) {
                                p.bounds = Some(bounds);
                                cx.notify();
                            }
                        }
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .into_any_element()
}

/// The resize handle (Figma 2570:10583): a 17pt band with the 1pt divider
/// at its foot and the 32 x 4 grip centered over it. Drag it, or give it the
/// keys (a click) and step it with ↑ / ↓.
fn split_handle(p: &PaneSession, theme: &Theme, cx: &mut Context<IssuesView>) -> Stateful<Div> {
    let grip = if p.dragging { theme.icon } else { theme.divider };
    let hover = theme.icon;
    div()
        .id("pane-split-handle")
        .debug_selector(|| "pane-split-handle".into())
        .group("pane-split-handle")
        .track_focus(&p.handle_focus)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(HANDLE_H))
        .w_full()
        .border_b_1()
        .border_color(theme.divider)
        .cursor(CursorStyle::ResizeUpDown)
        .focus(|s| s.bg(theme.background_hover))
        .tooltip(style::tooltip("Drag to resize the session (⌥↑ ⌥↓)"))
        .child(
            div()
                .w(px(GRIP_W))
                .h(px(GRIP_H))
                .rounded(px(GRIP_RADIUS))
                .bg(grip)
                .group_hover("pane-split-handle", move |s| s.bg(hover)),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                if let Some(p) = this.pane_session.as_mut() {
                    p.dragging = true;
                    window.focus(&p.handle_focus, cx);
                }
                window.prevent_default();
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
            let m = &ev.keystroke.modifiers;
            if m.platform || m.control || m.shift {
                return;
            }
            let stepped = match ev.keystroke.key.as_str() {
                "up" => this.step_split(true, cx),
                "down" => this.step_split(false, cx),
                _ => false,
            };
            if stepped {
                cx.stop_propagation();
            }
        }))
}

/// The session's header (Figma 2570:10586): 44pt, 12pt in, the workspace's
/// state glyph and name, then Open Workspace (Small, Default) at the
/// trailing edge, over a divider.
fn session_header(session: &TaskSession, theme: &Theme, cx: &mut Context<IssuesView>) -> Div {
    let glyph = match session.state {
        TaskSessionState::Workspace(badge) => workspace_glyph(badge, theme),
        TaskSessionState::Review(state) => review_glyph(state, theme),
    };
    let open = style::content_button(
        "pane-open-workspace",
        ButtonContent::label("Open Workspace"),
        ButtonKind::Secondary,
        ButtonSize::Small,
        false,
        theme,
    )
    .tooltip(style::tooltip(if session.is_review() { "Open the review" } else { "Open the workspace" }))
    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_session_workspace(cx)));
    div()
        .debug_selector(|| "pane-session-header".into())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .h(px(SESSION_HEADER_H))
        .w_full()
        .px(px(12.0))
        .border_b_1()
        .border_color(theme.divider)
        .child(div().flex().flex_none().items_center().child(render_glyph(glyph)))
        .child(
            div()
                .debug_selector(|| "pane-session-name".into())
                .min_w_0()
                .truncate()
                .type_style(Type::BaseMedium)
                .text_color(theme.text_loud)
                .child(session.workspace.clone()),
        )
        .child(div().flex_1())
        .child(open)
}

/// The property table: Status, Workflow, Priority and Branch, then the
/// task's tracker owner, dependencies and pull request where it has them.
fn properties(view: &IssuesView, d: &TaskDetailModel, theme: &Theme, cx: &mut Context<IssuesView>) -> Div {
    let mut rows = div()
        .debug_selector(|| "pane-properties".into())
        .flex()
        .flex_col()
        .gap(px(6.0))
        .w_full()
        .min_w_0();
    let shown = view.state.pane_properties(&d.id).unwrap_or_else(|| PaneProperties {
        status_name: d.status_name.clone(),
        category: d.category,
        workflow_name: d.workflow_name.clone(),
        queue: None,
        pending: None,
    });
    let priority = match shown.queue {
        Some(q) => format!("{} in queue", q.position),
        None => "Done".to_string(),
    };
    let values = [
        (Property::Status, "Status", Some(shown.category), shown.status_name.clone()),
        (Property::Workflow, "Workflow", None, workflow_label(&shown.workflow_name)),
        (Property::Priority, "Priority", None, priority),
    ];
    for (property, label, icon, value) in values {
        let pending = shown.pending == Some(property);
        rows = rows.child(property_row(theme, label, value_picker(view, property, icon, value, pending, theme, cx)));
    }
    rows = rows.child(property_row(
        theme,
        "Branch",
        match &d.branch {
            // Selectable and copyable (More copies it too); not an editor:
            // the branch is derived, and made when work starts.
            Some(b) => value_text(theme)
                .debug_selector(|| "pane-branch".into())
                .font_family(PRIMARY_FAMILY)
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(theme.foreground)
                .child(TextView::markdown(ElementId::Name(SharedString::from(format!("branch-{}", d.id))), plain(b)).selectable(true)),
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
        rows = rows.child(property_row(theme, "Owner", value_text(theme).child(text)));
    }
    if !d.depends_on.is_empty() {
        let deps = d.depends_on.iter().map(dep_text).collect::<Vec<_>>().join(", ");
        let label = if d.blocked { "Blocked by" } else { "Depends on" };
        rows = rows.child(property_row(theme, label, value_text(theme).debug_selector(|| "pane-deps".into()).child(deps)));
    }
    if let Some(url) = d.pr_link.clone() {
        let open = url.clone();
        rows = rows.child(property_row(
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
fn property_row(theme: &Theme, label: &'static str, value: impl IntoElement) -> Div {
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

/// A property's value as the frames draw it (`2565:13093`): the Button at
/// Small in the Ghost style, the status glyph for Status, the value and the
/// chevron, opening its picker under it. It draws Select while the picker is
/// open, the focus ring while the keyboard is on it, and Disable when the
/// property can't change now (a change still unanswered, a done queue), with
/// the reason on hover. `pending` dims the value while a choice settles.
fn value_picker(
    view: &IssuesView,
    property: Property,
    icon: Option<StatusCategory>,
    value: String,
    pending: bool,
    theme: &Theme,
    cx: &mut Context<IssuesView>,
) -> AnyElement {
    let size = ButtonSize::Small;
    let id = SharedString::from(format!("pane-{}", property.slug()));
    let cursor = match view.state.mode {
        Mode::PropertyPicker { property: open, cursor } if open == property => Some(cursor),
        _ => None,
    };
    let open = cursor.is_some();
    let refusal = view.state.property_refusal(property).filter(|_| !open);
    let kind = if open { ButtonKind::Select } else { ButtonKind::Ghost };
    let c = style::button_colors(kind, theme);
    let face = |b: Stateful<Div>| {
        b.min_w_0()
            .children(icon.map(|cat| style::left_slot(size, style::status_icon_large(cat, theme))))
            .child(div().min_w_0().truncate().when(pending, |d| d.opacity(0.6)).child(value))
            .child(style::dropdown_slot(size, c.icon))
    };
    if let Some(why) = refusal {
        return face(style::disabled_button_base(id, kind, size, theme))
            .tooltip(style::tooltip(why))
            .into_any_element();
    }
    let focused = !open && view.state.pane_focus() == Some(property);
    let trigger = face(style::button_base(id, kind, size, focused, theme));
    // The rows, read now (they're built while this view renders).
    let rows: Vec<(PropertyRow, String, Option<String>)> = if open {
        view.state
            .property_rows(property)
            .into_iter()
            .map(|row| {
                let (label, hint) = row_text(&row);
                (row, label, hint)
            })
            .collect()
    } else {
        Vec::new()
    };
    let width = match property {
        Property::Priority => PRIORITY_MENU_W,
        Property::Status | Property::Workflow => MENU_W,
    };
    let scroll = view.picker_scroll.clone();
    dropdown(
        open,
        &view.property_anchor,
        "property-picker",
        trigger,
        move |cx| picker_rows(property, rows, cursor, scroll, cx),
        width,
        false,
        move |this: &mut IssuesView, cx| {
            this.state.toggle_property(property);
            this.scroll_picker_to_cursor();
            cx.notify();
        },
        move |this: &mut IssuesView, _| {
            if matches!(this.state.mode, Mode::PropertyPicker { property: p, .. } if p == property) {
                this.state.close_overlay();
            }
        },
        cx,
    )
    .into_any_element()
}

/// A picker's widest: the Priority picker names the task at each place.
const PRIORITY_MENU_W: f32 = 320.0;
/// The tallest a picker grows before it scrolls (ten rows).
const PICKER_MAX_H: f32 = 10.0 * 36.0 + 8.0;

/// A picker row's label and its muted hint.
fn row_text(row: &PropertyRow) -> (String, Option<String>) {
    match row {
        PropertyRow::Status(c) => (c.name.clone(), c.merge.then(|| "merges".to_string())),
        PropertyRow::Workflow(c) => (workflow_label(&c.name), c.default.then(|| "Default".to_string())),
        PropertyRow::Priority(c) => (c.position.to_string(), Some(c.occupant.clone().unwrap_or_else(|| "this task".to_string()))),
    }
}

/// A picker's rows in one scroll: the keyboard's row on the selection fill,
/// the current value checked, a row that can't be chosen dimmed with why on
/// hover.
fn picker_rows(
    property: Property,
    rows: Vec<(PropertyRow, String, Option<String>)>,
    cursor: Option<usize>,
    scroll: ScrollHandle,
    cx: &mut Context<IssuesView>,
) -> Vec<AnyElement> {
    let theme = cx.global::<Theme>().opaque();
    let items: Vec<AnyElement> = rows
        .into_iter()
        .enumerate()
        .map(|(ix, (row, label, hint))| {
            let id = format!("picker-{}-{ix}", property.slug());
            let refusal = row.refusal().map(str::to_string);
            let base = match &refusal {
                Some(why) => style::disabled_menu_row(id, &theme).tooltip(style::tooltip(why.clone())),
                None => style::menu_row(id, cursor == Some(ix), &theme).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        let intents = this.state.choose_property(property, ix);
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    },
                )),
            };
            let glyph = match &row {
                PropertyRow::Status(c) => Some(style::status_icon_large(c.category, &theme)),
                _ => None,
            };
            let check = row
                .current()
                .then(|| heroicon(icons::CHECK).text_color(theme.accent).debug_selector(move || format!("picker-current-{ix}")));
            base.children(glyph)
                .child(div().flex_none().child(label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
                        .children(hint),
                )
                .children(check)
                .into_any_element()
        })
        .collect();
    vec![div()
        .id("property-picker-scroll")
        .flex()
        .flex_col()
        .max_h(px(PICKER_MAX_H))
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .children(items)
        .into_any_element()]
}

/// `text` as Markdown that shows it as typed: every ASCII punctuation mark
/// escaped (a branch name's `_`, `*` and `#`).
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Where task `id`'s draft save stands (none while nothing is unsaved), with
/// Retry for a failed save and both choices for a conflicting change.
fn save_row(view: &IssuesView, id: &str, theme: &Theme, cx: &mut Context<IssuesView>) -> Option<Div> {
    let save = view.state.save_status(id);
    let label = save.label()?;
    let alarm = matches!(save, SaveStatus::Failed(_) | SaveStatus::Invalid { .. } | SaveStatus::Conflict(_));
    let id = id.to_string();
    let row = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_2()
        .type_style(Type::XSmall)
        .text_color(if alarm { theme.error } else { theme.muted })
        .debug_selector(|| "pane-save-status".into())
        .child(label)
        .when(matches!(save, SaveStatus::Failed(_)), |row| {
            let id = id.clone();
            row.child(
                action(theme, "pane-save-retry", ButtonContent::label("Retry"), ButtonKind::Ghost).on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        let intents = this.state.retry_save(&id);
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    }),
                ),
            )
        })
        .when(matches!(save, SaveStatus::Conflict(Field::Body | Field::Title)), |row| {
            let (mine, theirs) = (id.clone(), id.clone());
            row.child(
                action(theme, "pane-keep-mine", ButtonContent::label("Keep mine"), ButtonKind::Ghost)
                    .tooltip(style::tooltip("Save your version over the other change"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.state.resolve_conflict(&mine, Resolution::KeepMine);
                        cx.notify();
                    })),
            )
            .child(
                action(theme, "pane-take-theirs", ButtonContent::label("Use theirs"), ButtonKind::Ghost)
                    .tooltip(style::tooltip("Show the saved version; yours is set aside"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.state.resolve_conflict(&theirs, Resolution::TakeTheirs);
                        cx.notify();
                    })),
            )
        });
    Some(row)
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
        PaneAction::CopyBranch => String::new(),
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
    fn a_branch_shows_as_typed() {
        assert_eq!(plain("jlong/my_task-1"), "jlong\\/my\\_task\\-1");
    }

    #[test]
    fn a_workflow_name_is_capitalized_for_the_header() {
        assert_eq!(workflow_label("task"), "Task");
        assert_eq!(workflow_label("Subtask"), "Subtask");
        assert_eq!(workflow_label(""), "Task");
    }
}
