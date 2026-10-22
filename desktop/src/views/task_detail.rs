//! The task detail: an issue's frontmatter and its Markdown body, in a modal
//! over the board (the TUI's card popover), after the Figma Issue detail
//! frame.
//!
//! The modal is 80% of the main area across and 85% down (946 x 870 in the
//! 1440 x 1024 frame), on the canvas with a 1px divider edge and 8pt
//! corners: the title (18 / 28 bold, loud) and a quiet close button; the
//! field table (an 88pt muted key column, foreground values); the compact
//! action buttons, each with its key as a keycap; a divider; then the body.
//! Fields come from `shelbi_app::TaskDetailModel`; the body renders with
//! gpui-kit's Markdown text view in the frame's styles
//! ([`markdown::body`]: headings, lists, code, links, checkbox rows; links
//! open in the browser). The body edits in place, in a gpui-kit text area
//! saved through `EditIssue` (Cmd+S), or opens in the user's configured
//! editor. Keys are the TUI popover's (`keys.yaml` `popover:`) plus the
//! desktop's `s` / `a` / `e` / `E`.

use gpui::{div, prelude::*, px, relative, BoxShadow, ClickEvent, Context, Div, ElementId, Hsla, SharedString};
use gpui_kit::component::input::Textarea;
use shelbi_app::TaskDetailModel;
use shelbi_state::keymap::PopoverAction;

use super::issues::{backdrop, IssuesView};
use super::markdown;
use crate::assets::{heroicon, icons, Icon};
use crate::theme::style::{self, ButtonKind, IconButtonSize};
use crate::theme::Theme;
use crate::theme::style::{Type, Typography};

pub fn render(
    view: &IssuesView,
    d: &TaskDetailModel,
    theme: &Theme,
    cx: &mut Context<IssuesView>,
) -> impl IntoElement {
    // A modal over the board: opaque over a translucent window.
    let theme = &theme.opaque();
    let editing = view
        .editor
        .as_ref()
        .is_some_and(|(id, _)| *id == d.id);
    let pending = view.state.pending(&d.id).map(|p| p.label.clone());
    let notice = view.state.notice(&d.id).map(str::to_string);
    let km = view.state.keymaps();
    let close_hint = view.hint(km.popover.first_chord_for(PopoverAction::Close));
    let left_hint = keycap_label(&view.hint(km.popover.first_chord_for(PopoverAction::MoveLeft)));
    let right_hint = keycap_label(&view.hint(km.popover.first_chord_for(PopoverAction::MoveRight)));
    let checklist = markdown::checklist(&d.body);

    let header = div()
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .px(px(INSET))
        .pt(px(16.0))
        .pb(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                // The frame's title: Geist Bold 18 / 28 (no named style).
                .type_style(Type::Heading)
                .text_size(px(18.0))
                .text_color(theme.text_loud)
                .child(d.title.clone()),
        )
        .child(
            // Centered on the title's first line, the 16pt x-mark in muted
            // where the frame draws it (18pt in from the panel's inner
            // edge: Figma lays the title row 2pt past its 20pt inset).
            style::tinted_icon_button(
                "detail-close",
                icons::X_MARK_CLOSE,
                IconButtonSize::Small,
                false,
                theme.muted,
                theme,
            )
            .mt(px(2.0))
            .mr(px(-6.0))
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

    let mut fields = div()
        .flex()
        .flex_col()
        .gap_1()
        .px(px(INSET))
        .pb(px(12.0))
        .type_style(Type::Base);
    fields = fields.child(field(theme, "id", d.id.clone()));
    fields = fields.child(field(theme, "status", format!("{}   priority {}", d.status_name, d.priority)));
    fields = fields.child(field(theme, "workflow", d.workflow_name.clone()));
    fields = fields.child(match &d.assignee {
        Some(w) => field(theme, "workspace", format!("@{w}")),
        None => row(theme, "workspace").child(div().text_color(theme.muted).child("Unassigned")),
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
            row(theme, "PR").child(
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
        format!("{}   updated {}", date(&d.created), date(&d.updated)),
    ));
    if let Some((done, total)) = checklist {
        fields = fields.child(field(theme, "checklist", format!("{done}/{total} done")));
    }

    let actions = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap_2()
        .px(px(INSET))
        .pb(px(12.0))
        .child(
            action(theme, "detail-left", Some((icons::ARROW_LEFT_BUTTON, theme.button_icon)), "Move", &left_hint, None, ButtonKind::Secondary)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let intents = this.state.detail_move(false);
                    this.apply_intents(intents, window, cx);
                    cx.notify();
                })),
        )
        .child(
            action(theme, "detail-right", None, "Move", &right_hint, Some((icons::ARROW_RIGHT_BUTTON, theme.button_icon)), ButtonKind::Secondary)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    let intents = this.state.detail_move(true);
                    this.apply_intents(intents, window, cx);
                    cx.notify();
                })),
        )
        .child(
            action(theme, "detail-start", Some((icons::PLAY_SOLID_BUTTON, theme.icon)), "Start", "s", None, ButtonKind::Secondary).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    if let Some(id) = this.state.detail_id().map(str::to_string) {
                        let intents = this.state.start(&id);
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    }
                }),
            ),
        )
        .child(
            action(theme, "detail-assign", Some((icons::USER_BUTTON, theme.icon)), "Assign", "a", None, ButtonKind::Secondary).on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    if let Some(id) = this.state.detail_id().map(str::to_string) {
                        let intents = this.state.begin_assign(&id);
                        this.apply_intents(intents, window, cx);
                        cx.notify();
                    }
                }),
            ),
        )
        .when(!editing, |row| {
            row.child(
                action(theme, "detail-edit", Some((icons::PENCIL_SQUARE_BUTTON, theme.icon)), "Edit", "e", None, ButtonKind::Secondary)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        if let Some(id) = this.state.detail_id().map(str::to_string) {
                            let intents = this.state.begin_edit(&id);
                            this.apply_intents(intents, window, cx);
                            cx.notify();
                        }
                    })),
            )
        })
        .child(
            action(
                theme,
                "detail-editor",
                Some((icons::ARROW_UP_RIGHT_BUTTON, theme.icon)),
                "Editor",
                "E",
                None,
                ButtonKind::Secondary,
            )
            .tooltip(style::tooltip("Open in your editor"))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                if let Some(id) = this.state.detail_id().map(str::to_string) {
                    this.apply_intents(vec![super::board_state::Intent::OpenInEditor(id)], window, cx);
                    cx.notify();
                }
            })),
        )
        .when(editing, |row| {
            row.child(
                action(theme, "detail-save", None, "Save", "⌘S", None, ButtonKind::Success)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_editor(cx))),
            )
            .child(
                action(theme, "detail-cancel", None, "Cancel", "Esc", None, ButtonKind::Quiet)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_editor(cx))),
            )
        });

    let status_rows = div()
        .flex()
        .flex_col()
        .gap_1()
        .px(px(INSET))
        .when(pending.is_some() || notice.is_some(), |d| d.pb(px(12.0)))
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
            .px(px(INSET))
            .pb(px(INSET))
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
            .px(px(INSET))
            .pb(px(INSET))
            .type_style(Type::Base)
            .child(div().text_color(theme.muted).child("This issue has no description yet."))
            .child(
                action(theme, "detail-add-description", Some((icons::PENCIL_SQUARE_BUTTON, theme.icon)), "Write one", "e", None, ButtonKind::Secondary)
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
            .px(px(INSET))
            .pb(px(INSET))
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(
                markdown::body(
                    ElementId::Name(SharedString::from(format!("body-{}", d.id))),
                    text,
                    theme,
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
        .bg(theme.canvas)
        .border_1()
        .border_color(theme.divider)
        .rounded_lg()
        .text_color(theme.foreground)
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(header)
        .child(fields)
        .child(actions)
        .child(status_rows)
        .child(div().flex_none().h(px(1.0)).mx(px(INSET)).mb(px(12.0)).bg(theme.divider))
        .child(body);

    backdrop(theme, cx)
        .items_center()
        .justify_center()
        .child(panel)
}

/// The modal's side inset (the frame's 20pt).
const INSET: f32 = 20.0;

/// A field row: 20pt tall, the key in its 88pt muted column, 8pt before
/// the value.
fn row(theme: &Theme, name: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .min_h(px(20.0))
        .child(
            div()
                .w(px(88.0))
                .flex_none()
                .text_color(theme.muted)
                .child(name.to_string()),
        )
}

fn field(theme: &Theme, name: &str, value: String) -> Div {
    row(theme, name).child(div().min_w_0().text_color(theme.foreground).child(value))
}

/// A `YYYY-MM-DD HH:MM UTC` stamp's date, as the frame shows `created` and
/// `updated`.
fn date(stamp: &str) -> &str {
    stamp.split_whitespace().next().unwrap_or(stamp)
}

/// A popover chord as the action's keycap spells it: a shifted letter as the
/// capital (`⇧H` → `H`, as the frame's Editor button shows `E`), any other
/// chord as it formats.
fn keycap_label(chord: &str) -> String {
    match chord.strip_prefix('⇧') {
        Some(key) if key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphabetic()) => {
            key.to_ascii_uppercase()
        }
        _ => chord.to_string(),
    }
}

/// A compact action, the Figma Button at X-Small: the style's fill, 6pt
/// corners, 26pt tall and 8pt across, an icon slot before (or after) the
/// X-Small medium label, and the key that does the same thing as one small
/// keycap. Each icon is 18pt, in its color: the frame draws the arrows in
/// `button_icon` and the rest in `icon`.
fn action(
    theme: &Theme,
    id: &'static str,
    icon: Option<(Icon, Hsla)>,
    text: &str,
    key: &str,
    trailing: Option<(Icon, Hsla)>,
    kind: ButtonKind,
) -> gpui::Stateful<gpui::Div> {
    style::button_base(id, kind, false, theme)
        .debug_selector(move || id.into())
        .h(px(26.0))
        .px(px(8.0))
        .gap_1()
        .type_style(Type::XSmallMedium)
        .when_some(icon, |b, (icon, color)| b.child(style::icon_slot(icon, 14.0, color)))
        .child(text.to_string())
        .when(!key.is_empty(), |b| b.child(keycap(key, theme)))
        .when_some(trailing, |b, (icon, color)| {
            // The trailing slot is flush left: the icon hangs past its right
            // edge, as the leading one hangs past its left.
            b.child(div().flex().flex_none().items_center().w(px(14.0)).child(heroicon(icon).text_color(color)))
        })
}

/// The Button component's Hotkey inside a button: the key as written (`s`,
/// `E`, `⌘S`), X-Small in gray on the canvas, 16pt tall with 4pt corners and
/// a 1px drop.
fn keycap(key: &str, theme: &Theme) -> Div {
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
        .child(key.to_string())
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
