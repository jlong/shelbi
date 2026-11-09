//! The window's floating toast ([`shelbi_app::toast`]): a small raised
//! surface over the main area's top-right corner. It is only painted: no
//! focus handle and no mouse listeners, so it never takes the keys and a
//! click lands on whatever is under it.

use gpui::{div, px, InteractiveElement, IntoElement, ParentElement, Styled};
use shelbi_app::toast::{Toast, ToastTone};

use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;

/// Its inset from the main area's top and right edges.
const INSET: f32 = 16.0;
/// The widest it grows before its text wraps.
const MAX_WIDTH: f32 = 420.0;

/// `toast`, positioned in its parent (the main area).
pub fn render(toast: &Toast, theme: &Theme) -> impl IntoElement {
    let text = match toast.tone {
        ToastTone::Info => theme.foreground,
        ToastTone::Error => theme.red,
    };
    style::popover(div(), theme)
        .debug_selector(|| "toast".into())
        .absolute()
        .top(px(INSET))
        .right(px(INSET))
        .max_w(px(MAX_WIDTH))
        .px_4()
        .py_2()
        .type_style(Type::Base)
        .text_color(text)
        .child(toast.text.clone())
}
