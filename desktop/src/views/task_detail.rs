//! The task detail: an issue's frontmatter and its Markdown body, over the
//! board (the TUI's card popover, as a desktop panel).
//!
//! Fields come from `shelbi_app::TaskDetailModel`; the body renders with
//! gpui-kit's Markdown text view (headings, lists, code, links, task-list
//! checkboxes; links open in the browser). The body edits in place, in a
//! gpui-kit text area saved through `EditIssue` (Cmd+S), or opens in the
//! user's configured editor. Keys are the TUI popover's (`keys.yaml`
//! `popover:`) plus the desktop's `s` / `a` / `e` / `E`.

use gpui::{div, prelude::*, px, relative, ClickEvent, Context, ElementId, SharedString};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::text::TextView;
use shelbi_app::TaskDetailModel;
use shelbi_state::keymap::PopoverAction;

use super::issues::{backdrop, IssuesView};
use super::markdown;
use crate::assets::{heroicon, icons, Icon};
use crate::theme::style::{self, ButtonKind, IconButtonSize};
use crate::theme::{Hue, Theme};
use crate::theme::style::{Type, Typography};

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
        .pl_5()
        .pr_3()
        .pt_4()
        .pb_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pt(px(4.0))
                .type_style(Type::Heading)
                .text_color(theme.text_loud)
                .child(d.title.clone()),
        )
        .child(
            style::icon_button("detail-close", icons::X_MARK, IconButtonSize::Default, false, theme)
                .tooltip(style::tooltip(format!("Close ({close_hint})")))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if this.editor.is_some() {
                        this.cancel_editor(cx);
                    }
                    this.state.close_overlay();
                    this.request_focus();
                    cx.notify();
                })),
        );

    let mut fields = div().flex().flex_col().gap(px(6.0)).px_5().pb_4().type_style(Type::Base);
    fields = fields.child(field(theme, "id", d.id.clone()));
    fields = fields.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(label(theme, "status"))
            .child(style::chip(d.status_label.clone(), Hue::Neutral, theme))
            .child(div().text_color(theme.muted).child(format!("priority {}", d.priority))),
    );
    fields = fields.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(label(theme, "workflow"))
            .child(style::chip(d.workflow_name.clone(), Hue::Blue, theme)),
    );
    fields = fields.child(match &d.assignee {
        Some(w) => field(theme, "workspace", format!("@{w}")).into_any_element(),
        None => div()
            .flex()
            .flex_row()
            .gap_2()
            .child(label(theme, "workspace"))
            .child(div().text_color(theme.muted).child("Unassigned"))
            .into_any_element(),
    });
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
                        .min_w_0()
                        .truncate()
                        .text_color(theme.link)
                        .hover(|s| s.underline())
                        .cursor_pointer()
                        .tooltip(style::tooltip(url.clone()))
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
            action(theme, "detail-left", Some(icons::ARROW_LEFT), "Move", &left_hint, ButtonKind::Secondary).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    let intents = this.state.detail_move(false);
                    this.apply_intents(intents, window, cx);
                    cx.notify();
                }),
            ),
        )
        .child(
            action(theme, "detail-right", Some(icons::ARROW_RIGHT), "Move", &right_hint, ButtonKind::Secondary).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    let intents = this.state.detail_move(true);
                    this.apply_intents(intents, window, cx);
                    cx.notify();
                }),
            ),
        )
        .child(
            action(theme, "detail-start", Some(icons::PLAY), "Start", "s", ButtonKind::Secondary).on_click(cx.listener(
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
            action(theme, "detail-assign", Some(icons::USER), "Assign", "a", ButtonKind::Secondary).on_click(cx.listener(
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
                action(theme, "detail-edit", Some(icons::PENCIL_SQUARE), "Edit", "e", ButtonKind::Secondary).on_click(
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
            action(
                theme,
                "detail-editor",
                Some(icons::ARROW_TOP_RIGHT_ON_SQUARE),
                "Open in editor",
                "⇧E",
                ButtonKind::Secondary,
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
                action(theme, "detail-save", None, "Save", "⌘S", ButtonKind::Primary).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| this.save_editor(cx),
                )),
            )
            .child(
                action(theme, "detail-cancel", None, "Cancel", "Esc", ButtonKind::Quiet).on_click(cx.listener(
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
            d.child(div().type_style(Type::XSmall).text_color(theme.muted).child(p))
        })
        .when_some(notice, |d, n| {
            d.child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_1()
                    .type_style(Type::XSmall)
                    .text_color(theme.error)
                    .child(
                        heroicon(icons::EXCLAMATION_TRIANGLE)
                            .text_color(theme.error),
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
    } else if d.body.trim().is_empty() {
        // An empty state with its action.
        div()
            .id("detail-body")
            .debug_selector(|| "detail-empty".into())
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .px_5()
            .pb_4()
            .child(div().text_color(theme.muted).child("This issue has no description yet."))
            .child(
                action(theme, "detail-add-description", Some(icons::PENCIL_SQUARE), "Write one", "e", ButtonKind::Secondary)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        if let Some(id) = this.state.detail_id().map(str::to_string) {
                            let intents = this.state.begin_edit(&id);
                            this.apply_intents(intents, window, cx);
                            cx.notify();
                        }
                    })),
            )
            .into_any_element()
    } else {
        let text = d.body.clone();
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
        .map(|d| style::dialog(d, theme))
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(header)
        .child(fields)
        .child(actions)
        .child(status_rows)
        .child(div().h(px(1.0)).mx_5().mt_1().mb_4().bg(theme.divider))
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
        .child(div().text_color(theme.foreground).child(value))
}

/// A compact action: a 28px Taskpage button, its icon, its label and the
/// key that does the same thing, as keycaps.
fn action(
    theme: &Theme,
    id: &'static str,
    icon: Option<Icon>,
    text: &str,
    key: &str,
    kind: ButtonKind,
) -> gpui::Stateful<gpui::Div> {
    let fg = style::button_colors(kind, theme).text;
    style::button_base(id, kind, false, theme)
        .debug_selector(move || id.into())
        .h(px(28.0))
        .px(px(10.0))
        .gap(px(6.0))
        .type_style(Type::XSmall)
        .when_some(icon, |b, icon| b.pl(px(8.0)).child(heroicon(icon).text_color(fg)))
        .child(text.to_string())
        .when(!key.is_empty(), |b| b.child(style::keycaps(key, theme)))
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
