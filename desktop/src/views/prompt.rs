//! The window's confirmations (Close Project, Close All Other Projects,
//! Quit Shelbi) drawn by the app, for a platform with no native alert.
//!
//! On macOS `window.prompt` is an `NSAlert` sheet. Linux has none, so gpui
//! falls back to its own renderer: a white box with unstyled text that
//! clips a long detail line. [`build`] replaces that fallback
//! (`App::set_prompt_builder`, installed in `main` off macOS) with the
//! app's dialog: the overlays' scrim and panel ([`overlay::panel`]), the
//! heading and detail in the theme's type, and the Button component.
//!
//! Keys: Return takes the focused button (the first, as an alert's default
//! does), Esc takes Cancel, Tab / Shift+Tab and ←/→ move between buttons.

use gpui::{
    div, prelude::*, px, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    PromptButton, PromptHandle, PromptLevel, PromptResponse, RenderablePromptHandle, SharedString,
    Window,
};

use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;
use crate::views::overlay;

/// The dialog's width, as the Zen intro's confirmation.
const WIDTH: f32 = 440.0;

/// gpui's prompt builder: the app's dialog in place of gpui's fallback.
pub fn build(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    actions: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let view = cx.new(|cx| ConfirmPrompt::new(level, message, detail, actions, cx));
    handle.with_view(view, window, cx)
}

/// One confirmation: its words, its buttons and which has the keyboard.
pub struct ConfirmPrompt {
    level: PromptLevel,
    message: SharedString,
    detail: Option<SharedString>,
    actions: Vec<PromptButton>,
    focused: usize,
    focus: FocusHandle,
}

impl ConfirmPrompt {
    fn new(
        level: PromptLevel,
        message: &str,
        detail: Option<&str>,
        actions: &[PromptButton],
        cx: &mut Context<Self>,
    ) -> Self {
        ConfirmPrompt {
            level,
            message: message.to_string().into(),
            detail: detail.map(|d| d.to_string().into()),
            actions: actions.to_vec(),
            focused: 0,
            focus: cx.focus_handle(),
        }
    }

    fn answer(&mut self, ix: usize, cx: &mut Context<Self>) {
        cx.emit(PromptResponse(ix));
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match key_action(&keystroke.key, keystroke.modifiers.shift, self.focused, &self.actions) {
            KeyAction::Answer(ix) => self.answer(ix, cx),
            KeyAction::Focus(ix) => {
                self.focused = ix;
                cx.notify();
            }
            KeyAction::None => return,
        }
        cx.stop_propagation();
    }
}

/// What a key does in a prompt with `actions`, `focused` holding the keyboard.
#[derive(Debug, PartialEq, Eq)]
enum KeyAction {
    Answer(usize),
    Focus(usize),
    None,
}

fn key_action(key: &str, shift: bool, focused: usize, actions: &[PromptButton]) -> KeyAction {
    let n = actions.len();
    if n == 0 {
        return KeyAction::None;
    }
    match key {
        "enter" | "space" => KeyAction::Answer(focused.min(n - 1)),
        "escape" => KeyAction::Answer(cancel_index(actions)),
        "tab" if shift => KeyAction::Focus((focused + n - 1) % n),
        "tab" | "right" => KeyAction::Focus((focused + 1) % n),
        "left" => KeyAction::Focus((focused + n - 1) % n),
        _ => KeyAction::None,
    }
}

/// The answer Esc gives: the Cancel button, else the last one (an alert's
/// dismissing answer).
fn cancel_index(actions: &[PromptButton]) -> usize {
    actions.iter().position(PromptButton::is_cancel).unwrap_or(actions.len().saturating_sub(1))
}

/// A button's style: Cancel is Secondary; the first answer of a warning or
/// critical prompt (Close Project, Quit Shelbi) is Danger, of any other
/// Primary; the rest Secondary.
fn kind(level: PromptLevel, ix: usize, action: &PromptButton) -> ButtonKind {
    if action.is_cancel() || ix > 0 {
        return ButtonKind::Secondary;
    }
    match level {
        PromptLevel::Info => ButtonKind::Primary,
        _ => ButtonKind::Danger,
    }
}

impl EventEmitter<PromptResponse> for ConfirmPrompt {}

impl Focusable for ConfirmPrompt {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConfirmPrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Opaque over a translucent window, as the overlays are.
        let theme = cx.global::<Theme>().opaque();
        let buttons = self.actions.iter().enumerate().map(|(ix, action)| {
            let id = SharedString::from(format!("prompt-button-{ix}"));
            style::button(id, action.label().clone(), kind(self.level, ix, action), ButtonSize::Base, ix == self.focused, &theme)
                .on_click(cx.listener(move |this, _, _, cx| this.answer(ix, cx)))
        });
        let panel = overlay::panel("confirm-prompt", &theme)
            .w(px(WIDTH))
            .max_w_full()
            .px_6()
            .py_4()
            .gap_3()
            .child(div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(self.message.clone()))
            .children(self.detail.clone().map(|detail| div().text_color(theme.foreground).child(detail)))
            .child(div().flex().flex_row().flex_wrap().justify_end().gap_2().pt_1().children(buttons));
        div()
            .id("confirm-prompt-layer")
            .debug_selector(|| "confirm-prompt-layer".into())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .bg(theme.scrim)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p_6()
            .font_family("Geist")
            .type_style(Type::Base)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions() -> Vec<PromptButton> {
        ["Close Project", "Cancel"].map(PromptButton::from).to_vec()
    }

    #[test]
    fn return_takes_the_focused_button_and_esc_takes_cancel() {
        assert_eq!(key_action("enter", false, 0, &actions()), KeyAction::Answer(0));
        assert_eq!(key_action("enter", false, 1, &actions()), KeyAction::Answer(1));
        assert_eq!(key_action("escape", false, 0, &actions()), KeyAction::Answer(1));
        let no_cancel = ["Quit", "Stay"].map(PromptButton::from).to_vec();
        assert_eq!(key_action("escape", false, 0, &no_cancel), KeyAction::Answer(1), "the last answer");
    }

    #[test]
    fn tab_and_the_arrows_move_between_buttons() {
        assert_eq!(key_action("tab", false, 0, &actions()), KeyAction::Focus(1));
        assert_eq!(key_action("tab", false, 1, &actions()), KeyAction::Focus(0));
        assert_eq!(key_action("tab", true, 0, &actions()), KeyAction::Focus(1));
        assert_eq!(key_action("right", false, 0, &actions()), KeyAction::Focus(1));
        assert_eq!(key_action("left", false, 0, &actions()), KeyAction::Focus(1));
        assert_eq!(key_action("a", false, 0, &actions()), KeyAction::None);
        assert_eq!(key_action("enter", false, 0, &[]), KeyAction::None);
    }

    #[test]
    fn a_warnings_first_answer_is_danger_and_cancel_is_secondary() {
        let a = actions();
        assert_eq!(kind(PromptLevel::Warning, 0, &a[0]), ButtonKind::Danger);
        assert_eq!(kind(PromptLevel::Info, 0, &a[0]), ButtonKind::Primary);
        assert_eq!(kind(PromptLevel::Warning, 1, &a[1]), ButtonKind::Secondary);
        assert_eq!(kind(PromptLevel::Warning, 0, &PromptButton::cancel("Cancel")), ButtonKind::Secondary);
    }
}
