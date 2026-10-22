//! The add-project form, drawn: the TUI's `overlay/add_project.rs` in a
//! cyan-bordered panel 70% × 60% of the window. The Name and Repo path fields
//! (the focused one's label bold and underlined, a cyan caret after its
//! value), the Config radio row, the "Detected:" line, the "Folder / id:"
//! preview, a row reserved for the inline error, and the key hints.
//!
//! The "Detected:" check runs git, so the window runs it off the UI thread
//! whenever [`Form::detection_due`] says the repo path changed.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, Pixels, Size};
use shelbi_app::overlay::add_project::{Field, Form, HINT, TITLE};
use shelbi_core::ConfigMode;

use crate::theme::style;
use crate::theme::Theme;
use crate::views::overlay::{hint, panel, title, with_overlay, ActiveOverlay, OverlayEvent, OverlayHost};

/// The label column (the TUI's 11 columns).
const LABEL_W: f32 = 100.0;

/// The panel's size in a window of `size` (the TUI's 70% × 60%, with floors).
pub fn panel_size(size: Size<Pixels>) -> (f32, f32) {
    let w = (size.width.as_f32() * 0.7).max(432.0).min(size.width.as_f32());
    let h = (size.height.as_f32() * 0.6).max(300.0).min(size.height.as_f32());
    (w, h)
}

fn on_field<H: OverlayHost>(
    field: Field,
) -> impl Fn(&mut H, &gpui::ClickEvent, &mut gpui::Window, &mut Context<H>) + 'static {
    move |this, _, window, cx| {
        with_overlay(this, window, cx, |o| {
            if let ActiveOverlay::AddProject { form, busy: false } = o {
                form.click_field(field);
            }
            OverlayEvent::Stay
        })
    }
}

fn label(text: &'static str, focused: bool, theme: &Theme) -> gpui::Div {
    div()
        .w(px(LABEL_W))
        .flex_none()
        .when(focused, |l| {
            l.text_color(theme.text_loud).font_weight(FontWeight::SEMIBOLD)
        })
        .when(!focused, |l| l.text_color(theme.foreground))
        .child(text)
}

pub fn render<H: OverlayHost>(
    form: &Form,
    busy: bool,
    size: Size<Pixels>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let (w, h) = panel_size(size);
    let accent = theme.focus_ring;
    let text_field = |id: &'static str, name: &'static str, value: &str, field: Field| {
        let focused = form.focus() == field;
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .flex_row()
            .items_center()
            .cursor_text()
            .child(label(name, focused, theme))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .relative()
                    .h(px(32.0))
                    .px_2()
                    .rounded_lg()
                    .bg(theme.canvas)
                    .border_1()
                    .border_color(theme.input_border)
                    // Bands, not a shadow: the canvas is translucent while
                    // the window is, and a shadow would fill the field.
                    .when(focused, |d| d.children(style::focus_bands_past(8.0, 1.0, theme)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(value.replace(' ', "\u{00a0}")),
                    )
                    .when(focused, |f| f.child(div().text_color(accent).child("▏"))),
            )
            .on_click(cx.listener(on_field(field)))
    };
    let option = |text: &'static str, on: bool| {
        div()
            .text_color(if on { theme.text_loud } else { theme.muted })
            .child(format!("({}) {text}", if on { "•" } else { "\u{00a0}" }))
    };
    let config_focused = form.focus() == Field::Config;
    let config = div()
        .id("add-project-config")
        .debug_selector(|| "add-project-config".into())
        .flex()
        .flex_row()
        .gap_4()
        .cursor_pointer()
        .child(label("Config", config_focused, theme))
        .child(option("In repo", form.mode() == ConfigMode::InRepo))
        .child(option("Global (~/.shelbi)", form.mode() == ConfigMode::Global))
        .on_click(cx.listener(on_field(Field::Config)));

    let detected = form.detected().map(|d| {
        div()
            .debug_selector(|| "add-project-detected".into())
            .text_color(if d.is_ok() { theme.success } else { theme.warning })
            .child(d.message())
    });
    let preview = match form.preview() {
        Some((slug, path)) => div()
            .flex()
            .flex_row()
            .child(div().text_color(theme.foreground).child("Folder / id:\u{00a0}"))
            .child(div().text_color(theme.text_loud).font_weight(FontWeight::SEMIBOLD).child(slug))
            .child(div().text_color(theme.muted).child(format!("\u{00a0}\u{00a0}\u{00a0}({path})"))),
        None => div()
            .text_color(theme.muted)
            .child("Folder / id: (type a name to see the derived id)"),
    };
    // The error row is always reserved so the hints never reflow.
    let status = div()
        .debug_selector(|| "add-project-status".into())
        .min_h(px(20.0));
    let status = match (busy, form.error()) {
        (true, _) => status.text_color(theme.muted).child("Creating project…"),
        (false, Some(msg)) => status
            .text_color(theme.error)
            .font_weight(FontWeight::MEDIUM)
            .child(format!("✗ {msg}")),
        (false, None) => status,
    };

    panel("add-project", theme)
        .w(px(w))
        .h(px(h))
        .child(title(TITLE, theme))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap_2()
                .px_4()
                .pt_3()
                .child(text_field("add-project-name", "Name", form.name(), Field::Name))
                .child(text_field("add-project-root", "Repo path", form.root(), Field::Root))
                .child(config)
                .child(div().h(px(8.0)))
                .children(detected)
                .child(preview)
                .child(div().h(px(8.0)))
                .child(status),
        )
        .child(hint(HINT, theme))
        .into_any_element()
}
