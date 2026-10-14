//! The error log, drawn: the TUI's `overlay/error_log.rs` rows (newest first,
//! a bold `●` on the entries unread when it opened, the local
//! `YYYY-MM-DD HH:MM` timestamp, the message wrapping under its own column) in
//! a yellow-bordered panel 80% × 60% of the window, with Copy and Clear.
//!
//! Opening it reads the log and marks it read off the UI thread (the window
//! does that); until the read lands it says "Loading…".

use gpui::{
    div, prelude::*, px, AnyElement, ClipboardItem, Context, FontWeight, Pixels, ScrollHandle,
    Size,
};
use shelbi_app::overlay::error_log::{format_ts, Viewer, EMPTY, HINT};

use crate::theme::Theme;
use crate::views::overlay::{hint, panel, title, with_overlay, ActiveOverlay, OverlayEvent, OverlayHost};

/// One scroll step (a key's line) in pixels.
pub const LINE_H: f32 = 20.0;

/// The panel's size in a window of `size` (the TUI's 80% × 60%, with floors).
pub fn panel_size(size: Size<Pixels>) -> (f32, f32) {
    let w = (size.width.as_f32() * 0.8).max(360.0).min(size.width.as_f32());
    let h = (size.height.as_f32() * 0.6).max(200.0).min(size.height.as_f32());
    (w, h)
}

/// The copy text, or `None` before the log is read.
fn copy_text(o: &ActiveOverlay) -> Option<String> {
    match o {
        ActiveOverlay::ErrorLog {
            viewer: Some(viewer),
            ..
        } => Some(viewer.text()),
        _ => None,
    }
}

pub fn render<H: OverlayHost>(
    viewer: Option<&Viewer>,
    scroll: &ScrollHandle,
    size: Size<Pixels>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let (w, h) = panel_size(size);
    let body = match viewer {
        None => div()
            .id("error-log-body")
            .text_color(theme.muted)
            .child("Loading…"),
        Some(viewer) if viewer.rows.is_empty() => div()
            .id("error-log-body")
            .debug_selector(|| "error-log-empty".into())
            .text_color(theme.ansi_dark_gray)
            .child(EMPTY),
        Some(viewer) => div()
            .id("error-log-body")
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scroll()
            .track_scroll(scroll)
            .children(viewer.rows.iter().enumerate().map(|(i, (entry, unread))| {
                div()
                    .debug_selector(move || format!("error-log-row-{i}"))
                    .flex()
                    .flex_row()
                    .items_start()
                    .child(
                        div()
                            .w(px(16.0))
                            .flex_none()
                            .when(*unread, |m| {
                                m.debug_selector(move || format!("error-log-unread-{i}"))
                                    .text_color(theme.error_marker)
                                    .font_weight(FontWeight::BOLD)
                                    .child("●")
                            }),
                    )
                    .child(
                        div()
                            .w(px(132.0))
                            .flex_none()
                            .text_color(theme.ansi_dark_gray)
                            .child(format_ts(entry)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(theme.ansi_white)
                            .child(entry.message.clone()),
                    )
            })),
    };
    let small_button = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .px_2()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .text_color(theme.muted)
            .hover(|s| s.bg(theme.selection).text_color(theme.foreground))
            .child(label)
    };
    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .pr_3()
        .child(title("Error log", theme.ansi_yellow))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .pt_2()
                .child(small_button("error-log-copy", "Copy").on_click(cx.listener(
                    |this: &mut H, _, window, cx| {
                        with_overlay(this, window, cx, |o| match copy_text(o) {
                            Some(text) => OverlayEvent::Copy(text),
                            None => OverlayEvent::Stay,
                        })
                    },
                )))
                .child(small_button("error-log-clear", "Clear").on_click(cx.listener(
                    |this: &mut H, _, window, cx| {
                        with_overlay(this, window, cx, |o| match o {
                            ActiveOverlay::ErrorLog {
                                project,
                                viewer: Some(_),
                                ..
                            } => OverlayEvent::ClearErrors {
                                project: project.clone(),
                            },
                            _ => OverlayEvent::Stay,
                        })
                    },
                ))),
        );
    panel("error-log", theme.ansi_yellow, theme)
        .w(px(w))
        .h(px(h))
        .child(header)
        .child(div().flex_1().min_h_0().flex().flex_col().px_4().py_2().child(body.flex_1().min_h_0()))
        .child(hint(format!("{HINT} · ⌘C copy"), theme))
        .into_any_element()
}

/// Put `text` on the clipboard.
pub fn copy(text: String, cx: &mut gpui::App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}
