//! The reject-reason prompt, drawn: the TUI's `overlay/review_reject.rs` in a
//! red-bordered panel. "Reason for rejecting:", the multi-line text area (its
//! border cyan while it has focus, the caret a reversed cell), `[ Reject ]` /
//! `[ Cancel ]`, and the key hint.
//!
//! The reason soft-wraps at the text area's width, measured in the panel's
//! font ([`wrap`]): rows break between words, the caret, ↑ / ↓ and a click
//! work on the wrapped rows, and the area scrolls to keep the caret's row in
//! view. Wrapping never adds a newline to the reason.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{canvas, div, prelude::*, px, AnyElement, App, Bounds, Context, MouseButton, MouseDownEvent, Pixels};
use shelbi_app::overlay::review_reject::{
    RejectFocus, RejectPrompt, Wrap, CANCEL_LABEL, HINT, LABEL, REJECT_LABEL, TITLE,
};

use crate::theme::style::{ButtonKind, Type};
use crate::theme::Theme;
use crate::views::overlay::{button, hint, panel, reject_event, title, with_overlay, ActiveOverlay, OverlayEvent, OverlayHost};

const WIDTH: f32 = 600.0;
/// The text area's visible lines (it scrolls to keep the caret in view).
pub const VISIBLE_LINES: usize = 9;
const LINE_H: f32 = 20.0;
/// The text's room across: the panel less its 16pt sides, the area's 8pt
/// padding and 1pt border each side.
pub const TEXT_W: f32 = WIDTH - 2.0 * 16.0 - 2.0 * 8.0 - 2.0;

/// Where the last draw put the reason's text, for placing the caret on a
/// click.
pub type TextBounds = Rc<Cell<Bounds<Pixels>>>;

/// The first line shown so the caret's row stays visible.
pub fn scroll_for(caret_row: usize) -> usize {
    caret_row.saturating_sub(VISIBLE_LINES - 1)
}

/// How the reason wraps: [`TEXT_W`] across, each char as wide as the
/// overlay's font (Geist, Base) sets it. A point short of the room, since a
/// shaped row can run a hair past its chars' summed advances.
pub fn wrap(cx: &App) -> Wrap {
    let text = cx.text_system().clone();
    let font = text.resolve_font(&gpui::font("Geist"));
    let size = px(Type::Base.size());
    Wrap::new(TEXT_W - 1.0, move |c| {
        let c = if c == '\u{00a0}' || c.is_control() { ' ' } else { c };
        text.advance(font, size, c).map_or(0.0, |s| s.width.as_f32())
    })
}

fn on_prompt<H: OverlayHost>(
    f: impl Fn(&mut RejectPrompt) -> Option<shelbi_app::overlay::review_reject::Step> + 'static,
) -> impl Fn(&mut H, &gpui::ClickEvent, &mut gpui::Window, &mut Context<H>) + 'static {
    move |this, _, window, cx| {
        with_overlay(this, window, cx, |o| match o {
            ActiveOverlay::RejectReason { task_id, prompt, .. } => match f(prompt) {
                Some(step) => reject_event(task_id, prompt, step),
                None => OverlayEvent::Stay,
            },
            _ => OverlayEvent::Stay,
        })
    }
}

/// A press in the text area: focus it and put the caret where it landed, on
/// the wrapped row under it.
fn place_caret<H: OverlayHost>(this: &mut H, ev: &MouseDownEvent, window: &mut gpui::Window, cx: &mut Context<H>) {
    with_overlay(this, window, cx, |o| {
        if let ActiveOverlay::RejectReason { prompt, text, .. } = o {
            let at = text.get();
            let (_, (caret_row, _)) = prompt.text().layout();
            let y = (ev.position.y - at.origin.y).as_f32().max(0.0);
            let row = scroll_for(caret_row) + (y / LINE_H) as usize;
            prompt.click_text_at(row, (ev.position.x - at.origin.x).as_f32().max(0.0));
        }
        OverlayEvent::Stay
    })
}

pub fn render<H: OverlayHost>(prompt: &RejectPrompt, bounds: &TextBounds, theme: &Theme, cx: &mut Context<H>) -> AnyElement {
    let focused = prompt.focus() == RejectFocus::Text;
    let text = prompt.text();
    let (rows, (caret_row, caret_col)) = text.layout();
    let first = scroll_for(caret_row);
    let s = |chars: &[char]| chars.iter().collect::<String>().replace(' ', "\u{00a0}");
    let lines = rows.iter().enumerate().skip(first).take(VISIBLE_LINES).map(|(v, r)| {
        let row_el = div()
            .debug_selector(move || format!("reject-line-{v}"))
            .h(px(LINE_H))
            .flex()
            .flex_row()
            .whitespace_nowrap()
            .text_color(theme.text_loud);
        let line = &text.lines()[r.line][r.start..r.end];
        if !(focused && v == caret_row) {
            return row_el.child(s(line));
        }
        let col = caret_col.min(line.len());
        let under = line.get(col).map(|c| c.to_string()).unwrap_or_else(|| "\u{00a0}".into());
        row_el
            .child(s(&line[..col]))
            .child(
                div()
                    .debug_selector(|| "reject-caret".into())
                    .bg(theme.text_loud)
                    .text_color(theme.raised)
                    .child(under.replace(' ', "\u{00a0}")),
            )
            .child(s(line.get(col + 1..).unwrap_or(&[])))
    });
    // The rows over a canvas that notes where they are, for clicks.
    let noted = bounds.clone();
    let lines = div()
        .relative()
        .flex()
        .flex_col()
        .child(
            canvas(move |b, _, _| noted.set(b), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
        .children(lines);

    let area = div()
        .id("reject-text")
        .debug_selector(|| "reject-text".into())
        .h(px(LINE_H * VISIBLE_LINES as f32 + 16.0))
        .px_2()
        .py_2()
        .rounded_lg()
        .bg(theme.canvas)
        .border_1()
        .border_color(theme.input_border)
        .overflow_hidden()
        .cursor_text()
        .child(lines)
        .on_mouse_down(MouseButton::Left, cx.listener(place_caret));
    // The focus ring and glow as bands around the area, not a shadow: the
    // canvas is translucent while the window is, and a shadow would fill
    // it. They sit on a holder because the area clips what overflows it.
    let area = div()
        .relative()
        .flex()
        .flex_col()
        .child(area)
        .when(focused, |d| d.children(crate::theme::style::focus_bands(8.0, theme)));

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
                .child(div().text_color(theme.foreground).child(LABEL))
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
