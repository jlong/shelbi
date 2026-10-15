//! The "Load for review" dialog, drawn: the TUI's `overlay/review_confirm.rs`
//! in a cyan-bordered panel. The task's title, then one of its three shapes:
//! the no-slots report with `[ Dismiss ]`, the one-slot "Load onto review
//! workspace `review`?" confirm, or the slot picker (`> review-1  (free)`);
//! `[ Load ]` / `[ Cancel ]` under it.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight};
use shelbi_app::overlay::review_confirm::{
    ConfirmFocus, Dialog, CANCEL_LABEL, DISMISS_LABEL, LOAD_LABEL, TITLE,
};

use crate::theme::style::{self, ButtonKind};
use crate::theme::Theme;
use crate::views::overlay::{button, confirm_event, panel, title, with_overlay, ActiveOverlay, OverlayEvent, OverlayHost};

/// The panel's width (the TUI's 60 columns).
const WIDTH: f32 = 520.0;

fn on_dialog<H: OverlayHost>(
    f: impl Fn(&mut Dialog) -> shelbi_app::overlay::review_confirm::Step + 'static,
) -> impl Fn(&mut H, &gpui::ClickEvent, &mut gpui::Window, &mut Context<H>) + 'static {
    move |this, _, window, cx| {
        with_overlay(this, window, cx, |o| match o {
            ActiveOverlay::ReviewConfirm { task_id, dialog } => {
                let step = f(dialog);
                confirm_event(task_id, step)
            }
            _ => OverlayEvent::Stay,
        })
    }
}

pub fn render<H: OverlayHost>(dialog: &Dialog, theme: &Theme, cx: &mut Context<H>) -> AnyElement {
    let accent = theme.info;
    let mut body = div()
        .flex()
        .flex_col()
        .gap_2()
        .px_4()
        .pt_1()
        .pb_4()
        .child(
            div()
                .text_color(theme.text_loud)
                .font_weight(FontWeight::MEDIUM)
                .child(dialog.title().to_string()),
        );

    if !dialog.has_slots() {
        body = body
            .child(
                div()
                    .debug_selector(|| "review-confirm-message".into())
                    .text_color(theme.warning)
                    .child(dialog.empty_message().to_string()),
            )
            .child(
                div().flex().justify_center().pt_2().child(
                    button("review-confirm-dismiss", DISMISS_LABEL, true, ButtonKind::Secondary, theme)
                        .on_click(cx.listener(on_dialog(|d| d.click_load()))),
                ),
            );
    } else {
        if dialog.is_picker() {
            for (i, slot) in dialog.slots().iter().enumerate() {
                let selected = i == dialog.selected();
                let name = slot.name.clone();
                body = body.child(
                    div()
                        .id(("review-confirm-slot", i))
                        .debug_selector(move || format!("review-confirm-slot-{name}"))
                        .relative()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_lg()
                        .cursor_pointer()
                        .when(selected && dialog.focus() == ConfirmFocus::List, |r| {
                            r.bg(theme.row_selected).child(style::selection_indicator(theme))
                        })
                        .when(!selected, |r| r.hover(|s| s.bg(theme.fill_subtle)))
                        .child(
                            div()
                                .when(selected, |n| n.text_color(theme.text_loud).font_weight(FontWeight::SEMIBOLD))
                                .when(!selected, |n| n.text_color(theme.text_loud))
                                .child(slot.name.clone()),
                        )
                        .child(div().text_color(theme.text_quiet).child(slot.state_note()))
                        .on_click(cx.listener(on_dialog(move |d| d.click_slot(i)))),
                );
            }
        } else {
            let slot = &dialog.slots()[0];
            body = body.child(
                div()
                    .debug_selector(|| "review-confirm-message".into())
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .text_color(theme.text)
                    .child("Load onto review workspace\u{00a0}")
                    .child(
                        div()
                            .text_color(accent)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(slot.name.clone()),
                    )
                    .child("?"),
            );
            if let Some(occupant) = &slot.occupant {
                body = body.child(
                    div()
                        .text_color(theme.text_quiet)
                        .child(format!("(returns \"{occupant}\" to the queue)")),
                );
            }
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_2()
                .pt_2()
                .child(
                    button(
                        "review-confirm-load",
                        LOAD_LABEL,
                        dialog.focus() == ConfirmFocus::Load,
                        ButtonKind::Primary,
                        theme,
                    )
                    .on_click(cx.listener(on_dialog(|d| d.click_load()))),
                )
                .child(
                    button(
                        "review-confirm-cancel",
                        CANCEL_LABEL,
                        dialog.focus() == ConfirmFocus::Cancel,
                        ButtonKind::Secondary,
                        theme,
                    )
                    .on_click(cx.listener(on_dialog(|d| d.click_cancel()))),
                ),
        );
    }

    panel("review-confirm", theme)
        .w(px(WIDTH))
        .child(title(TITLE, theme))
        .child(body)
        .into_any_element()
}
