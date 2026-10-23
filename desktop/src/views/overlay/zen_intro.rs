//! The first-run Zen intro, drawn: the TUI's `overlay/zen_intro.rs`. The cyan
//! heading, the gray body copy, the "Don't show this again" checkbox, and
//! `[ Cancel ]` left of the green `[ Enable Zen ]`. Like the TUI's, it draws no
//! accent border of its own.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight};
use shelbi_app::overlay::zen_intro::{self as zen, IntroFocus, IntroState};

use crate::theme::style::{self, ButtonKind, ButtonSize};
use crate::theme::Theme;
use crate::views::overlay::{panel, with_overlay, zen_event, ActiveOverlay, OverlayEvent, OverlayHost};
use crate::theme::style::{Type, Typography};

const WIDTH: f32 = 560.0;

fn on_control<H: OverlayHost>(
    control: IntroFocus,
) -> impl Fn(&mut H, &gpui::ClickEvent, &mut gpui::Window, &mut Context<H>) + 'static {
    move |this, _, window, cx| {
        with_overlay(this, window, cx, |o| match o {
            ActiveOverlay::ZenIntro { project, state } => {
                let outcome = zen::click(state, control);
                zen_event(project, state, outcome)
            }
            _ => OverlayEvent::Stay,
        })
    }
}

pub fn render<H: OverlayHost>(state: &IntroState, theme: &Theme, cx: &mut Context<H>) -> AnyElement {
    let paragraphs = zen::BODY.split("\n\n").map(|para| {
        div().flex().flex_col().children(
            para.lines()
                .map(|line| div().child(line.trim_end().replace("  ", "\u{00a0}\u{00a0}"))),
        )
    });
    let checkbox_focused = state.focus == IntroFocus::Checkbox;
    let checkbox = div()
        .id("zen-checkbox")
        .debug_selector(|| "zen-checkbox".into())
        .cursor_pointer()
        .when(checkbox_focused, |c| {
            c.text_color(theme.text_loud).font_weight(FontWeight::MEDIUM).underline()
        })
        .when(!checkbox_focused, |c| c.text_color(theme.foreground))
        .child(format!(
            "{} {}",
            if state.dont_show_again { "[x]" } else { "[ ]" },
            zen::CHECKBOX
        ))
        .on_click(cx.listener(on_control(IntroFocus::Checkbox)));

    let button = |id: &'static str, label: &str, focused: bool, kind| {
        style::button(id, label.to_string(), kind, ButtonSize::Base, focused, theme)
    };

    panel("zen-intro", theme)
        .w(px(WIDTH))
        .px_6()
        .py_4()
        .gap_3()
        .child(
            div()
                .type_style(Type::BaseSemibold)
                .text_color(theme.text_loud)
                .child(zen::HEADING),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .text_color(theme.foreground)
                .children(paragraphs),
        )
        .child(checkbox)
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_2()
                .child(
                    button("zen-cancel", zen::CANCEL, state.focus == IntroFocus::Cancel, ButtonKind::Secondary)
                        .on_click(cx.listener(on_control(IntroFocus::Cancel))),
                )
                .child(
                    button("zen-enable", zen::ENABLE, state.focus == IntroFocus::Enable, ButtonKind::Primary)
                        .on_click(cx.listener(on_control(IntroFocus::Enable))),
                ),
        )
        .into_any_element()
}
