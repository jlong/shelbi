//! The task detail: an issue's frontmatter and its Markdown body, over the
//! board (the TUI's card popover, as a desktop panel).
//!
//! Fields come from `shelbi_app::TaskDetailModel`; the body renders with
//! gpui-kit's Markdown text view (headings, lists, code, links, task-list
//! checkboxes; links open in the browser). The body edits in place, in a
//! gpui-kit text area saved through `EditIssue` (Cmd+S), or opens in the
//! user's configured editor. Keys are the TUI popover's (`keys.yaml`
//! `popover:`) plus the desktop's `s` / `a` / `e` / `E`.

use gpui::{div, prelude::*, px, relative, ClickEvent, Context, ElementId, FontWeight, SharedString};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::text::TextView;
use shelbi_app::TaskDetailModel;
use shelbi_state::keymap::PopoverAction;

use super::issues::{backdrop, IssuesView};
use super::markdown;
use crate::assets::{heroicon, icons, Icon};
use crate::theme::Theme;

pub fn render(
    view: &IssuesView,
    d: &TaskDetailModel,
    theme: &Theme,
    cx: &mut Context<IssuesView>,
) -> impl IntoElement {
    let editing = view
        .editor
        .as_ref()
        .is_some_and(|(id, _)| *id == d.id);
    let pending = view.state.pending(&d.id).map(|p| p.label.clone());
    let notice = view.state.notice(&d.id).map(str::to_string);
    let km = view.state.keymaps();
    let close_hint = view.hint(km.popover.first_chord_for(PopoverAction::Close));
    let left_hint = view.hint(km.popover.first_chord_for(PopoverAction::MoveLeft));
    let right_hint = view.hint(km.popover.first_chord_for(PopoverAction::MoveRight));
    let checklist = markdown::checklist(&d.body);

    let header = div()
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .px_5()
        .pt_4()
        .pb_2()
        .child(
            div()
                .flex_1()
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.white)
                .child(d.title.clone()),
        )
        .child(
            icon_button(theme, "detail-close", icons::X_MARK, &format!("Close ({close_hint})"))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if this.editor.is_some() {
                        this.cancel_editor(cx);
                    }
                    this.state.close_overlay();
                    this.request_focus();
                    cx.notify();
                })),
        );

    let mut fields = div().flex().flex_col().gap_1().px_5().pb_3().text_sm();
    fields = fields.child(field(theme, "id", d.id.clone()));
    fields = fields.child(field(
        theme,
        "status",
        format!("{}   priority {}", d.status_label, d.priority),
    ));
    fields = fields.child(field(theme, "workflow", d.workflow_name.clone()));
    fields = fields.child(field(
        theme,
        "workspace",
        d.assignee
            .as_ref()
            .map_or_else(|| "unassigned".to_string(), |w| format!("@{w}")),
    ));
    if let Some(b) = &d.branch {
        fields = fields.child(field(theme, "branch", b.clone()));
    }
    if !d.depends_on.is_empty() {
        let deps = d
            .depends_on
            .iter()
            .map(dep_text)
            .collect::<Vec<_>>()
            .join(", ");
        let label = if d.blocked { "blocked by" } else { "depends on" };
        fields = fields.child(field(theme, label, deps));
    }
    if let Some(url) = d.pr_link.clone() {
        let open = url.clone();
        fields = fields.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(label(theme, "PR"))
                .child(
                    div()
                        .id("pr-link")
                        .underline()
                        .cursor_pointer()
                        .child(url)
                        .on_click(move |_, _, cx| cx.open_url(&open)),
                ),
        );
    }
    fields = fields.child(field(
        theme,
        "created",
        format!("{}   updated {}", d.created, d.updated),
    ));
    if let Some((done, total)) = checklist {
        fields = fields.child(field(theme, "checklist", format!("{done}/{total} done")));
    }

    let actions = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_2()
        .px_5()
        .pb_3()
        .child(
            text_button(theme, "detail-left", &format!("← Move ({left_hint})")).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    let intents = this.state.detail_move(false);
                    this.apply_intents(intents, window, cx);
                    cx.notify();
                }),
            ),
        )
        .child(
            text_button(theme, "detail-right", &format!("Move → ({right_hint})")).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    let intents = this.state.detail_move(true);
                    this.apply_intents(intents, window, cx);
                    cx.notify();
                }),
            ),
        )
        .child(
            icon_button(theme, "detail-start", icons::PLAY, "Start (s)").on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| {
                    if let Some(id) = this.state.detail_id().map(str::to_string) {
                        let intents = this.state.start(&id);
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    }
                },
            )),
        )
        .child(
            icon_button(theme, "detail-assign", icons::USER, "Assign (a)").on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| {
                    if let Some(id) = this.state.detail_id().map(str::to_string) {
                        let intents = this.state.begin_assign(&id);
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    }
                },
            )),
        )
        .when(!editing, |row| {
            row.child(
                icon_button(theme, "detail-edit", icons::PENCIL_SQUARE, "Edit (e)").on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        if let Some(id) = this.state.detail_id().map(str::to_string) {
                            let intents = this.state.begin_edit(&id);
                            this.apply_intents(intents, window, cx);
                            cx.notify();
                        }
                    }),
                ),
            )
        })
        .child(
            icon_button(
                theme,
                "detail-editor",
                icons::ARROW_TOP_RIGHT_ON_SQUARE,
                "Open in editor (E)",
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                if let Some(id) = this.state.detail_id().map(str::to_string) {
                    this.apply_intents(vec![super::board_state::Intent::OpenInEditor(id)], window, cx);
                    cx.notify();
                }
            })),
        )
        .when(editing, |row| {
            row.child(
                text_button(theme, "detail-save", "Save (⌘S)").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| this.save_editor(cx),
                )),
            )
            .child(
                text_button(theme, "detail-cancel", "Cancel (Esc)").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| this.cancel_editor(cx),
                )),
            )
        });

    let status_rows = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_5()
        .when_some(pending, |d, p| {
            d.child(div().text_xs().text_color(theme.muted).child(p))
        })
        .when_some(notice, |d, n| {
            d.child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.red)
                    .child(
                        heroicon(icons::EXCLAMATION_TRIANGLE)
                            .text_color(theme.red),
                    )
                    .child(n),
            )
        });

    let body = if let (true, Some((_, state))) = (editing, view.editor.as_ref()) {
        div()
            .flex_1()
            .min_h_0()
            .px_5()
            .pb_4()
            .child(Textarea::new(state).h_full())
            .into_any_element()
    } else {
        let text = if d.body.trim().is_empty() {
            "_No description._".to_string()
        } else {
            d.body.clone()
        };
        div()
            .id("detail-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&view.detail_scroll)
            .w_full()
            .min_w_0()
            .px_5()
            .pb_4()
            .child(
                TextView::markdown(
                    ElementId::Name(SharedString::from(format!("body-{}", d.id))),
                    text,
                )
                .w_full()
                .selectable(true),
            )
            .into_any_element()
    };

    let panel = div()
        .id("detail-panel")
        .flex()
        .flex_col()
        .w(relative(0.8))
        .h(relative(0.85))
        .min_w_0()
        .overflow_hidden()
        .rounded_lg()
        .border_1()
        .border_color(theme.divider)
        .bg(theme.canvas)
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(header)
        .child(fields)
        .child(actions)
        .child(status_rows)
        .child(div().h(px(1.0)).mx_5().mb_3().bg(theme.divider))
        .child(body);

    backdrop(theme, cx)
        .items_center()
        .justify_center()
        .child(panel)
}

fn label(theme: &Theme, text: &str) -> impl IntoElement {
    div()
        .w(px(88.0))
        .flex_none()
        .text_color(theme.muted)
        .child(text.to_string())
}

fn field(theme: &Theme, name: &str, value: String) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .child(label(theme, name))
        .child(div().text_color(theme.gray).child(value))
}

fn text_button(theme: &Theme, id: &'static str, text: &str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(theme.search)
        .text_xs()
        .text_color(theme.gray)
        .cursor_pointer()
        .child(text.to_string())
}

fn icon_button(
    theme: &Theme,
    id: &'static str,
    icon: Icon,
    text: &str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(theme.search)
        .text_xs()
        .text_color(theme.gray)
        .cursor_pointer()
        .child(heroicon(icon).text_color(theme.muted))
        .child(text.to_string())
}

/// One dependency as the detail lists it: `id [status]`, a `✓` once it's
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
