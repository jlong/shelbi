//! Contact Us (`dt-feedback-dialog`): the feedback dialog the sidebar
//! footer's megaphone and Help ▸ Send Feedback… open over the window,
//! modeled on ContextStore's. The Figma file draws the footer's button
//! (Navigation, `2220:17016`) and no frame for the dialog yet, so the panel
//! is built from the Components page's parts at the workspace dialog's
//! scale: the canvas panel (32pt in, 12pt corners) on the scrim; the
//! megaphone in the Primary button's tint and "Contact Us" (Desktop/Heading)
//! centered over the fields; Name and Email Textboxes; the kind's Segmented
//! Control (Bug Report, the default; Feature Request; Other) across the
//! form; Message, a multi-line Textbox; the "Include system info" Checkbox
//! (on); then Send Message (Primary) and Cancel (Default), full width, at
//! the Base size. A send that worked swaps the form for the thank-you:
//! the heart, "Thank you!", the line under it, and Close.
//!
//! [`FeedbackForm`] holds the draft and decides everything; this view
//! paints it, keeps the text fields in step with it and runs the send
//! through [`FeedbackService`] on the background executor. While it runs,
//! Send Message is the disabled "Sending…" with a spinner, and nothing can
//! send again or close the dialog. A failure shows under the form, the
//! draft kept for another try.
//!
//! Keys, taken ahead of the text fields: Tab / ⇧Tab walk the controls in
//! visual order and never leave the dialog; Return is Send Message (on
//! Cancel, Cancel; in Message, a new line, and ⌘Return sends from there);
//! Esc cancels; Space presses the focused button or toggles the checkbox;
//! ← / → pick the kind on the segmented control. The focus ring shows once
//! the keyboard is used.

use gpui::{
    div, prelude::*, px, App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, Role, Stateful,
    Subscription, Toggled, Window,
};
use gpui_kit::base::input::Textarea;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use shelbi_state::keymap::{Key as Code, KeyChord, Mods};

use super::feedback_state::{Action, FeedbackForm, Focus, Key};
use super::workflow_editor::text_field;
use crate::assets::{heroicon, icons, Icon};
use crate::elements::{controls, spinner};
use crate::keys::chord_from_keystroke;
use crate::platform::feedback::{FeedbackService, Kind, Submission};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The panel's width.
pub const DIALOG_W: f32 = 440.0;
/// The panel's inset and corners (the workspace dialog's).
const PADDING: f32 = 32.0;
const RADIUS: f32 = 12.0;
/// The title's size (Desktop/Heading, 18 / 28 semibold).
const TITLE_SIZE: f32 = 18.0;
/// The round tint behind the header's icon.
const BADGE: f32 = 40.0;
/// The Message field's height in lines: it grows from the first to the
/// second, then scrolls.
const MESSAGE_LINES: (usize, usize) = (5, 9);
/// What "Include system info" says it adds.
pub const SYSTEM_INFO_LABEL: &str = "Include system info (macOS version, app version)";
/// The thank-you's line.
pub const THANKS: &str = "Your input helps us build a better app.\nWe\u{2019}ll follow up if we have questions.";

/// What the dialog tells the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackEvent {
    /// Close it: cancelled (nothing sent) or after the thank-you.
    Closed,
}

/// The open dialog.
pub struct FeedbackDialog {
    form: FeedbackForm,
    /// The dialog's own focus: the segmented control, the checkbox and the
    /// buttons paint their ring from it.
    focus: FocusHandle,
    name_input: Entity<InputState>,
    email_input: Entity<InputState>,
    message_input: Entity<TextareaState>,
    /// The keyboard has moved focus: rings show from then on.
    keyboard: bool,
    service: FeedbackService,
    /// Every submission sent, in order (tests).
    #[cfg(test)]
    sent: Vec<Submission>,
    _subscriptions: Vec<Subscription>,
}

impl FeedbackDialog {
    /// A new draft from the name and email remembered, sent through the
    /// [`FeedbackService`] global when one is set (a test's), else the live
    /// one.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A test that set no service gets the offline one, never the live.
        #[cfg(not(test))]
        let fallback = FeedbackService::live;
        #[cfg(test)]
        let fallback = FeedbackService::offline;
        let service = cx.try_global::<FeedbackService>().cloned().unwrap_or_else(fallback);
        let form = FeedbackForm::new(service.remembered());
        let focus = cx.focus_handle();
        let name = form.name().to_string();
        let email = form.email().to_string();
        let name_input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let email_input = cx.new(|cx| InputState::new(window, cx).default_value(email));
        let message_input =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(MESSAGE_LINES.0, MESSAGE_LINES.1));
        let this_window = window.window_handle();
        let me = cx.weak_entity();
        let mut subscriptions = vec![
            // Ahead of the text fields' own bindings: Tab, Return and Esc
            // are the dialog's everywhere in it.
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != this_window {
                    return;
                }
                let Some(chord) = chord_from_keystroke(&event.keystroke) else {
                    return;
                };
                if me.update(cx, |this, cx| this.key(chord, window, cx)).unwrap_or(false) {
                    cx.stop_propagation();
                }
            }),
        ];
        for (input, focus) in [(&name_input, Focus::Name), (&email_input, Focus::Email)] {
            subscriptions.push(cx.subscribe_in(input, window, move |this, input, event: &InputEvent, _, cx| {
                match event {
                    InputEvent::Change => {
                        let text = input.read(cx).value().to_string();
                        match focus {
                            Focus::Name => this.form.set_name(&text),
                            _ => this.form.set_email(&text),
                        }
                    }
                    InputEvent::Focus => this.form.set_focus(focus),
                    _ => return,
                }
                cx.notify();
            }));
        }
        subscriptions.push(cx.subscribe_in(&message_input, window, |this, input, event: &InputEvent, _, cx| {
            match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.form.set_message(&text);
                }
                InputEvent::Focus => this.form.set_focus(Focus::Message),
                _ => return,
            }
            cx.notify();
        }));
        let mut dialog = FeedbackDialog {
            form,
            focus,
            name_input,
            email_input,
            message_input,
            keyboard: false,
            service,
            #[cfg(test)]
            sent: Vec::new(),
            _subscriptions: subscriptions,
        };
        dialog.sync_focus(window, cx);
        dialog
    }

    #[cfg(test)]
    pub fn form(&self) -> &FeedbackForm {
        &self.form
    }

    /// The submissions sent so far (tests).
    #[cfg(test)]
    pub fn sent(&self) -> &[Submission] {
        &self.sent
    }

    /// The text fields (tests type into them as the user would).
    #[cfg(test)]
    pub fn inputs(&self) -> (Entity<InputState>, Entity<InputState>, Entity<TextareaState>) {
        (self.name_input.clone(), self.email_input.clone(), self.message_input.clone())
    }

    /// Fill the fields (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_fill(&mut self, name: &str, email: &str, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name_input.update(cx, |s, cx| s.set_value(name.to_string(), window, cx));
        self.email_input.update(cx, |s, cx| s.set_value(email.to_string(), window, cx));
        self.message_input.update(cx, |s, cx| s.set_value(message.to_string(), window, cx));
        self.form.set_name(name);
        self.form.set_email(email);
        self.form.set_message(message);
        cx.notify();
    }

    /// Press Send Message (`--visual-figma`). `hold`: the send stays out
    /// (the form is sending, no request is made), for its frame.
    #[cfg(feature = "visual-check")]
    pub fn offscreen_send(&mut self, hold: bool, window: &mut Window, cx: &mut Context<Self>) {
        if hold {
            let _ = self.form.press(Focus::Send);
            cx.notify();
        } else {
            self.press(Focus::Send, window, cx);
        }
    }

    /// Close without sending (the window's ⌘W), unless a send is out.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        if !self.form.sending() {
            cx.emit(FeedbackEvent::Closed);
        }
    }

    /// A key the dialog takes. `false` lets it through to the focused text
    /// field.
    pub(crate) fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let bare = chord.mods == Mods::NONE;
        let in_text = !self.form.sent() && self.form.focus().is_text();
        let key = match chord.code {
            Code::Tab if bare => Key::Tab,
            Code::Tab | Code::BackTab if chord.mods == Mods::SHIFT || chord.code == Code::BackTab => Key::BackTab,
            Code::Enter if chord.mods == Mods::SUPER => Key::CmdEnter,
            // Return in the message is its new line.
            Code::Enter if bare && in_text && self.form.focus() == Focus::Message => return false,
            Code::Enter if bare => Key::Enter,
            Code::Esc if bare => Key::Esc,
            Code::Char(' ') if bare && !in_text => Key::Space,
            Code::Left if bare && !in_text => Key::Left,
            Code::Right if bare && !in_text => Key::Right,
            _ => return false,
        };
        if matches!(key, Key::Tab | Key::BackTab | Key::Space | Key::Left | Key::Right) {
            self.keyboard = true;
        }
        let action = self.form.key(key);
        self.act(action, window, cx);
        true
    }

    /// A click on `focus`'s button, without a pointer (tests).
    #[cfg(test)]
    pub fn press_for_test(&mut self, focus: Focus, window: &mut Window, cx: &mut Context<Self>) {
        self.press(focus, window, cx);
    }

    fn press(&mut self, focus: Focus, window: &mut Window, cx: &mut Context<Self>) {
        let action = self.form.press(focus);
        self.act(action, window, cx);
    }

    fn act(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Stay => {}
            Action::Send(submission) => self.send(submission, window, cx),
            Action::Close => {
                cx.emit(FeedbackEvent::Closed);
                return;
            }
        }
        self.sync_focus(window, cx);
        cx.notify();
    }

    /// Send `submission` off the UI thread; the answer comes back to the
    /// form. A dialog closed meanwhile (its window gone) drops it.
    fn send(&mut self, submission: Submission, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.sent.push(submission.clone());
        let service = self.service.clone();
        let work = cx.background_executor().spawn(async move { service.send(&submission) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| {
                if let Err(e) = &result {
                    tracing::warn!("feedback wasn't sent: {e}");
                }
                this.form.finished(result);
                this.sync_focus(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Give the keyboard to the focused control: a text field's input, else
    /// the dialog (which paints the ring on the rest).
    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match self.form.focus() {
            _ if self.form.sent() => self.focus.clone(),
            Focus::Name => self.name_input.read(cx).focus_handle(cx),
            Focus::Email => self.email_input.read(cx).focus_handle(cx),
            Focus::Message => self.message_input.read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        };
        if !handle.is_focused(window) {
            window.focus(&handle, cx);
        }
    }

    /// Follow the keyboard into a text field however it got there (a
    /// click, the window handing focus back): the form's focus is where
    /// Tab goes on from.
    fn follow_focus(&mut self, window: &Window, cx: &App) {
        let fields = [
            (Focus::Name, self.name_input.read(cx).focus_handle(cx)),
            (Focus::Email, self.email_input.read(cx).focus_handle(cx)),
            (Focus::Message, self.message_input.read(cx).focus_handle(cx)),
        ];
        if let Some((focus, _)) = fields.iter().find(|(_, h)| h.is_focused(window)) {
            self.form.set_focus(*focus);
        }
    }

    /// Whether `focus`'s ring shows.
    fn ring(&self, focus: Focus, window: &Window) -> bool {
        self.keyboard && self.focus.is_focused(window) && self.form.focus() == focus
    }
}

impl EventEmitter<FeedbackEvent> for FeedbackDialog {}

/// The header's icon on its round tint.
fn badge(icon: Icon, fill: gpui::Hsla, color: gpui::Hsla) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(BADGE))
        .rounded_full()
        .bg(fill)
        .child(heroicon(icon).text_color(color))
}

fn title(text: &'static str, theme: &Theme) -> Div {
    div()
        .type_style(Type::Heading)
        .text_size(px(TITLE_SIZE))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_loud)
        .child(text)
}

/// A full-width dialog button with its role, its label as the accessible
/// name, and the keyboard's ring when `focused`.
fn button(id: &'static str, label: &'static str, kind: ButtonKind, enabled: bool, focused: bool, theme: &Theme) -> Stateful<Div> {
    let b = if enabled {
        style::button(id, label, kind, ButtonSize::Base, focused, theme)
    } else {
        style::disabled_button(id, label, kind, ButtonSize::Base, theme)
    };
    b.w_full().role(Role::Button).aria_label(label)
}

impl FeedbackDialog {
    fn render_form(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let form = &self.form;
        let busy = form.sending();
        let header = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(badge(icons::MEGAPHONE, theme.primary_fill, theme.primary_label))
            .child(title("Contact Us", theme));
        let name = text_field(&self.name_input, "feedback-name", Some("Name".into()), false, window, cx);
        let email_invalid = form.error() == Some(super::feedback_state::NO_EMAIL);
        let email = text_field(&self.email_input, "feedback-email", Some("Email".into()), email_invalid, window, cx);
        let kind = self.render_kinds(theme, window, cx);
        let message_focused = self.message_input.read(cx).focus_handle(cx).is_focused(window);
        let message = controls::textarea_box(
            div()
                .relative()
                .child(Textarea::new(&self.message_input))
                .when(form.message().is_empty(), |d| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .type_style(Type::Base)
                            .text_color(theme.form_hint)
                            .child("Message"),
                    )
                }),
            message_focused,
            theme,
        )
        .debug_selector(|| "feedback-message".into())
        .w_full();
        let on = form.include_system_info();
        let system_info = controls::checkbox(
            "feedback-system-info",
            SYSTEM_INFO_LABEL,
            on,
            self.ring(Focus::SystemInfo, window),
            theme,
        )
        .role(Role::CheckBox)
        .aria_label(SYSTEM_INFO_LABEL)
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .when(!busy, |d| {
            d.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.form.toggle_system_info();
                this.act(Action::Stay, window, cx);
            }))
        });
        let error = form.error().map(|e| {
            div()
                .debug_selector(|| "feedback-error".into())
                .type_style(Type::Small)
                .text_color(theme.error)
                .child(e.to_string())
        });
        let send = if busy {
            style::pending_button(
                "feedback-send",
                "Sending\u{2026}",
                spinner::spinner_sized("feedback-sending", theme.primary_label, 16.0),
                ButtonKind::Primary,
                ButtonSize::Base,
                theme,
            )
            .w_full()
            .role(Role::Button)
            .aria_label("Sending\u{2026}")
        } else {
            button("feedback-send", "Send Message", ButtonKind::Primary, true, self.ring(Focus::Send, window), theme)
                .aria_keyshortcuts("Return")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.press(Focus::Send, window, cx)))
        };
        let cancel = button("feedback-cancel", "Cancel", ButtonKind::Secondary, !busy, self.ring(Focus::Cancel, window), theme)
            .aria_keyshortcuts("Escape")
            .when(!busy, |b| b.on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.press(Focus::Cancel, window, cx))));
        panel("feedback-dialog", "Contact Us", theme)
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .child(name)
                    .child(email)
                    .child(kind)
                    .child(message)
                    .child(system_info)
                    .children(error),
            )
            .child(div().flex().flex_col().gap(px(12.0)).child(send).child(cancel))
    }

    /// Bug Report / Feature Request / Other: the Segmented Control across
    /// the form, its items sharing the width.
    fn render_kinds(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let current = self.form.kind();
        let busy = self.form.sending();
        let items = Kind::ALL.map(|kind| {
            let id = format!("feedback-kind-{}", kind.wire());
            let selected = kind == current;
            controls::segment(id, kind.label(), selected, true, theme)
                .flex_1()
                .justify_center()
                .role(Role::RadioButton)
                .aria_label(kind.label())
                .aria_selected(selected)
                .aria_toggled(if selected { Toggled::True } else { Toggled::False })
                .when(!busy, |d| {
                    d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.form.pick(kind);
                        this.act(Action::Stay, window, cx);
                    }))
                })
        });
        let ring = self.ring(Focus::Kind, window);
        let track = controls::segmented_track(theme)
            .w_full()
            .children(items)
            .when(ring, |d| d.children(style::focus_bands(10.0, theme)));
        div()
            .id("feedback-kind")
            .debug_selector(|| "feedback-kind".into())
            .role(Role::RadioGroup)
            .aria_label("Feedback type")
            .w_full()
            .child(track)
    }

    fn render_thanks(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let close = button("feedback-close", "Close", ButtonKind::Secondary, true, self.ring(Focus::Close, window), theme)
            .aria_keyshortcuts("Return")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.press(Focus::Close, window, cx)));
        panel("feedback-thanks", "Thank you!", theme)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.0))
                    .child(badge(icons::HEART, theme.magenta_tint, theme.magenta))
                    .child(title("Thank you!", theme))
                    .child(
                        div()
                            .debug_selector(|| "feedback-thanks-line".into())
                            .type_style(Type::Small)
                            .text_color(theme.form_hint)
                            .text_center()
                            .child(THANKS),
                    ),
            )
            .child(close)
    }
}

/// The dialog's panel: the canvas, its 1pt edge, 32pt in, 24pt between its
/// parts.
fn panel(id: &'static str, label: &'static str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(Role::Dialog)
        .aria_label(label)
        .flex()
        .flex_col()
        .gap(px(24.0))
        .w(px(DIALOG_W))
        .p(px(PADDING))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(theme.divider)
        .bg(theme.opaque().canvas)
        .text_color(theme.form_text)
        .occlude()
}

impl Render for FeedbackDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        self.follow_focus(window, cx);
        let panel = if self.form.sent() {
            self.render_thanks(&theme, window, cx)
        } else {
            self.render_form(&theme, window, cx)
        };
        div()
            .id("feedback-layer")
            .track_focus(&self.focus)
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .font_family("Geist")
            .type_style(Type::Base)
            .child(
                div()
                    .id("feedback-scrim")
                    .debug_selector(|| "feedback-scrim".into())
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.scrim)
                    .occlude()
                    .child(panel),
            )
    }
}
