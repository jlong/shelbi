//! The reject-reason prompt, drawn: the TUI's `overlay/review_reject.rs` in a
//! red-bordered panel. "Reason for rejecting:", the multi-line text area (its
//! border cyan while it has focus, the caret a reversed cell), `[ Reject ]` /
//! `[ Cancel ]`, and the key hint.

use gpui::{div, prelude::*, px, AnyElement, Context};
use shelbi_app::overlay::review_reject::{
    RejectFocus, RejectPrompt, CANCEL_LABEL, HINT, LABEL, REJECT_LABEL, TITLE,
};

use crate::theme::style::ButtonKind;
use crate::theme::Theme;
use crate::views::overlay::{button, hint, panel, reject_event, title, with_overlay, ActiveOverlay, OverlayEvent, OverlayHost};

const WIDTH: f32 = 600.0;
/// The text area's visible lines (it scrolls to keep the caret in view).
pub const VISIBLE_LINES: usize = 9;
const LINE_H: f32 = 20.0;

/// The first line shown so the caret's row stays visible.
pub fn scroll_for(caret_row: usize) -> usize {
    caret_row.saturating_sub(VISIBLE_LINES - 1)
}

fn on_prompt<H: OverlayHost>(
    f: impl Fn(&mut RejectPrompt) -> Option<shelbi_app::overlay::review_reject::Step> + 'static,
) -> impl Fn(&mut H, &gpui::ClickEvent, &mut gpui::Window, &mut Context<H>) + 'static {
    move |this, _, window, cx| {
        with_overlay(this, window, cx, |o| match o {
            ActiveOverlay::RejectReason { task_id, prompt } => match f(prompt) {
                Some(step) => reject_event(task_id, prompt, step),
                None => OverlayEvent::Stay,
            },
            _ => OverlayEvent::Stay,
        })
    }
}

pub fn render<H: OverlayHost>(prompt: &RejectPrompt, theme: &Theme, cx: &mut Context<H>) -> AnyElement {
    let focused = prompt.focus() == RejectFocus::Text;
    let text = prompt.text();
    let (row, col) = text.cursor();
    let first = scroll_for(row);
    let lines = text.lines().iter().enumerate().skip(first).take(VISIBLE_LINES).map(|(i, line)| {
        let row_el = div()
            .debug_selector(move || format!("reject-line-{i}"))
            .h(px(LINE_H))
            .flex()
            .flex_row()
            .whitespace_nowrap()
            .text_color(theme.text_loud);
        let s = |chars: &[char]| chars.iter().collect::<String>().replace(' ', "\u{00a0}");
        if !(focused && i == row) {
            return row_el.child(s(line));
        }
        let under = line.get(col).map(|c| c.to_string()).unwrap_or_else(|| "\u{00a0}".into());
        row_el
            .child(s(&line[..col]))
            .child(
                div()
                    .debug_selector(|| "reject-caret".into())
                    .bg(theme.text_loud)
                    .text_color(theme.surface_raised)
                    .child(under.replace(' ', "\u{00a0}")),
            )
            .child(s(line.get(col + 1..).unwrap_or(&[])))
    });

    let area = div()
        .id("reject-text")
        .debug_selector(|| "reject-text".into())
        .h(px(LINE_H * VISIBLE_LINES as f32 + 16.0))
        .px_2()
        .py_2()
        .rounded_lg()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.rule_strong)
        .when(focused, |d| d.shadow(vec![crate::theme::style::focus_shadow(theme)]))
        .overflow_hidden()
        .cursor_text()
        .children(lines)
        .on_click(cx.listener(on_prompt(|p| {
            p.click_text();
            None
        })));

    panel("review-reject", theme)
        .w(px(WIDTH))
        .child(title(TITLE, theme))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .px_4()
                .pb_2()
                .child(div().text_color(theme.text).child(LABEL))
                .child(area)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .pt_1()
                        .child(
                            button(
                                "reject-submit",
                                REJECT_LABEL,
                                prompt.focus() == RejectFocus::Reject,
                                ButtonKind::Danger,
                                theme,
                            )
                            .on_click(cx.listener(on_prompt(|p| Some(p.click_reject())))),
                        )
                        .child(
                            button(
                                "reject-cancel",
                                CANCEL_LABEL,
                                prompt.focus() == RejectFocus::Cancel,
                                ButtonKind::Secondary,
                                theme,
                            )
                            .on_click(cx.listener(on_prompt(|p| Some(p.click_cancel())))),
                        ),
                ),
        )
        .child(hint(HINT, theme))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_area_scrolls_to_keep_the_caret_visible() {
        assert_eq!(scroll_for(0), 0);
        assert_eq!(scroll_for(VISIBLE_LINES - 1), 0);
        assert_eq!(scroll_for(VISIBLE_LINES), 1);
        assert_eq!(scroll_for(20), 20 - (VISIBLE_LINES - 1));
    }
}
