//! The Contact Us form's state, toolkit-free (`dt-feedback-dialog`, moved
//! here for `tui-feedback-dialog`): the draft, where the keyboard is, what
//! Return / Esc / Tab do, the validation, and the one send in flight. Each
//! client paints it and runs the send (the desktop's `views::feedback`, the
//! TUI's `overlay::feedback`), and both show the words below.
//!
//! The controls, in visual (and Tab) order: Name, Email, the kind's
//! segmented control, Message, "Include system info", Send Message, Cancel.
//! Once sent, the thank-you view has one control, Close.

use super::{FeedbackError, Kind, Remembered, Submission};

/// The dialog's title.
pub const TITLE: &str = "Contact Us";
/// The fields' placeholders (they have no label over them).
pub const NAME: &str = "Name";
pub const EMAIL: &str = "Email";
pub const MESSAGE: &str = "Message";
/// The default action, and what it says while a send is out.
pub const SEND: &str = "Send Message";
pub const SENDING: &str = "Sending\u{2026}";
pub const CANCEL: &str = "Cancel";
/// The thank-you's title, its line and its button.
pub const THANKS_TITLE: &str = "Thank you!";
pub const THANKS: &str = "Your input helps us build a better app.\nWe\u{2019}ll follow up if we have questions.";
pub const CLOSE: &str = "Close";

/// What "Include system info" says it adds: the OS this build sends
/// ([`super::Os`]).
pub fn system_info_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Include system info (macOS version, app version)"
    } else {
        "Include system info (OS version, app version)"
    }
}

/// The message under the form when Email is empty.
pub const NO_EMAIL: &str = "Please enter your email";
/// The message under the form when Message is empty.
pub const NO_MESSAGE: &str = "Please enter a message";

/// A control the keyboard can be on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Name,
    Email,
    Kind,
    Message,
    SystemInfo,
    Send,
    Cancel,
    /// The thank-you view's button.
    Close,
}

impl Focus {
    /// The form's controls in Tab order.
    pub const FORM: [Focus; 7] = [
        Focus::Name,
        Focus::Email,
        Focus::Kind,
        Focus::Message,
        Focus::SystemInfo,
        Focus::Send,
        Focus::Cancel,
    ];

    /// Whether it's a text field (which takes typing, Space and the arrows).
    pub fn is_text(self) -> bool {
        matches!(self, Focus::Name | Focus::Email | Focus::Message)
    }
}

/// A key the dialog handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Tab,
    BackTab,
    /// Return: the default action (Send Message), or the focused button.
    Enter,
    /// ⌘Return: Send Message from anywhere, the Message field included
    /// (where Return starts a new line).
    CmdEnter,
    Esc,
    Space,
    Left,
    Right,
}

/// What the view does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Stay,
    /// Send this (the form is now busy until [`FeedbackForm::finished`]).
    Send(Submission),
    /// Close the dialog: cancelled, or after the thank-you.
    Close,
}

/// The form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackForm {
    name: String,
    email: String,
    kind: Kind,
    message: String,
    include_system_info: bool,
    focus: Focus,
    error: Option<String>,
    sending: bool,
    sent: bool,
}

impl FeedbackForm {
    /// A new draft, starting from the name and email remembered: the
    /// keyboard on Name when it's empty, else Email when that is, else
    /// Message.
    pub fn new(remembered: Remembered) -> FeedbackForm {
        let focus = if remembered.name.is_empty() {
            Focus::Name
        } else if remembered.email.is_empty() {
            Focus::Email
        } else {
            Focus::Message
        };
        FeedbackForm {
            name: remembered.name,
            email: remembered.email,
            kind: Kind::Bug,
            message: String::new(),
            include_system_info: true,
            focus,
            error: None,
            sending: false,
            sent: false,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn include_system_info(&self) -> bool {
        self.include_system_info
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The message under the form, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether a send is out: nothing can send again or close the dialog.
    pub fn sending(&self) -> bool {
        self.sending
    }

    /// Whether it went: the thank-you view shows.
    pub fn sent(&self) -> bool {
        self.sent
    }

    /// Editing a field clears the error shown.
    pub fn set_name(&mut self, text: &str) {
        if self.name != text {
            self.name = text.to_string();
            self.error = None;
        }
    }

    pub fn set_email(&mut self, text: &str) {
        if self.email != text {
            self.email = text.to_string();
            self.error = None;
        }
    }

    pub fn set_message(&mut self, text: &str) {
        if self.message != text {
            self.message = text.to_string();
            self.error = None;
        }
    }

    /// A text field took the keyboard (a click into it).
    pub fn set_focus(&mut self, focus: Focus) {
        if !self.sent && Focus::FORM.contains(&focus) {
            self.focus = focus;
        }
    }

    /// Pick `kind` (a click on its segment, which takes the keyboard too).
    pub fn pick(&mut self, kind: Kind) {
        if !self.sending && !self.sent {
            self.kind = kind;
            self.focus = Focus::Kind;
        }
    }

    /// Toggle "Include system info" (a click, which takes the keyboard too).
    pub fn toggle_system_info(&mut self) {
        if !self.sending && !self.sent {
            self.include_system_info = !self.include_system_info;
            self.focus = Focus::SystemInfo;
        }
    }

    /// A click on a button: what pressing it does.
    pub fn press(&mut self, focus: Focus) -> Action {
        match focus {
            Focus::Send => self.submit(),
            Focus::Cancel | Focus::Close => self.close(),
            _ => Action::Stay,
        }
    }

    /// A key.
    pub fn key(&mut self, key: Key) -> Action {
        if self.sent {
            return match key {
                Key::Enter | Key::CmdEnter | Key::Esc | Key::Space => self.close(),
                _ => Action::Stay,
            };
        }
        match key {
            Key::Tab | Key::BackTab => {
                let at = Focus::FORM.iter().position(|f| *f == self.focus).unwrap_or(0);
                let n = Focus::FORM.len();
                let next = if key == Key::Tab { (at + 1) % n } else { (at + n - 1) % n };
                self.focus = Focus::FORM[next];
                Action::Stay
            }
            Key::Esc => self.close(),
            Key::CmdEnter => self.submit(),
            Key::Enter => match self.focus {
                Focus::Cancel => self.close(),
                // Return starts a new line in the message (the view lets it
                // through); ⌘Return sends from there.
                Focus::Message => Action::Stay,
                _ => self.submit(),
            },
            Key::Space => match self.focus {
                Focus::SystemInfo => {
                    self.toggle_system_info();
                    Action::Stay
                }
                Focus::Send => self.submit(),
                Focus::Cancel => self.close(),
                _ => Action::Stay,
            },
            Key::Left | Key::Right if self.focus == Focus::Kind && !self.sending => {
                let at = Kind::ALL.iter().position(|k| *k == self.kind).unwrap_or(0);
                let next = if key == Key::Right { (at + 1).min(Kind::ALL.len() - 1) } else { at.saturating_sub(1) };
                self.kind = Kind::ALL[next];
                Action::Stay
            }
            Key::Left | Key::Right => Action::Stay,
        }
    }

    /// Cancel / Esc / Close: never sends, and waits for a send that's out.
    fn close(&mut self) -> Action {
        if self.sending {
            Action::Stay
        } else {
            Action::Close
        }
    }

    /// Send Message: check the draft and, when it holds, send it trimmed. A
    /// second press while one is out does nothing.
    fn submit(&mut self) -> Action {
        if self.sending || self.sent {
            return Action::Stay;
        }
        let email = self.email.trim();
        let message = self.message.trim();
        if email.is_empty() {
            self.error = Some(NO_EMAIL.to_string());
            self.focus = Focus::Email;
            return Action::Stay;
        }
        if message.is_empty() {
            self.error = Some(NO_MESSAGE.to_string());
            self.focus = Focus::Message;
            return Action::Stay;
        }
        let name = self.name.trim();
        let submission = Submission {
            name: (!name.is_empty()).then(|| name.to_string()),
            email: email.to_string(),
            kind: self.kind,
            message: message.to_string(),
            include_system_info: self.include_system_info,
        };
        self.error = None;
        self.sending = true;
        Action::Send(submission)
    }

    /// The send came back: the thank-you, or the error under the form with
    /// the draft kept for another try.
    pub fn finished(&mut self, result: Result<(), FeedbackError>) {
        self.sending = false;
        match result {
            Ok(()) => {
                self.sent = true;
                self.error = None;
                self.focus = Focus::Close;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remembered(name: &str, email: &str) -> Remembered {
        Remembered {
            name: name.into(),
            email: email.into(),
        }
    }

    fn filled() -> FeedbackForm {
        let mut f = FeedbackForm::new(remembered("  Ada ", " ada@example.com "));
        f.set_message("  It crashed.\n");
        f
    }

    #[test]
    fn the_keyboard_starts_on_the_first_empty_field() {
        assert_eq!(FeedbackForm::new(remembered("", "")).focus(), Focus::Name);
        assert_eq!(FeedbackForm::new(remembered("", "a@b.c")).focus(), Focus::Name);
        assert_eq!(FeedbackForm::new(remembered("Ada", "")).focus(), Focus::Email);
        assert_eq!(FeedbackForm::new(remembered("Ada", "a@b.c")).focus(), Focus::Message);
    }

    #[test]
    fn a_new_form_is_a_bug_report_with_system_info() {
        let f = FeedbackForm::new(Remembered::default());
        assert_eq!(f.kind(), Kind::Bug);
        assert!(f.include_system_info());
        assert!(!f.sending() && !f.sent() && f.error().is_none());
    }

    #[test]
    fn tab_walks_the_controls_in_visual_order_and_wraps() {
        let mut f = FeedbackForm::new(Remembered::default());
        let mut seen = vec![f.focus()];
        for _ in 0..7 {
            f.key(Key::Tab);
            seen.push(f.focus());
        }
        assert_eq!(
            seen,
            [
                Focus::Name,
                Focus::Email,
                Focus::Kind,
                Focus::Message,
                Focus::SystemInfo,
                Focus::Send,
                Focus::Cancel,
                Focus::Name
            ]
        );
        f.key(Key::BackTab);
        assert_eq!(f.focus(), Focus::Cancel);
    }

    #[test]
    fn the_arrows_pick_the_kind_on_the_segmented_control() {
        let mut f = FeedbackForm::new(Remembered::default());
        f.key(Key::Right);
        assert_eq!(f.kind(), Kind::Bug, "the arrows are the text field's off the control");
        f.set_focus(Focus::Kind);
        f.key(Key::Right);
        assert_eq!(f.kind(), Kind::Feature);
        f.key(Key::Right);
        f.key(Key::Right);
        assert_eq!(f.kind(), Kind::Other, "it stops at the end");
        f.key(Key::Left);
        assert_eq!(f.kind(), Kind::Feature);
        f.pick(Kind::Bug);
        assert_eq!((f.kind(), f.focus()), (Kind::Bug, Focus::Kind));
    }

    #[test]
    fn space_toggles_system_info() {
        let mut f = FeedbackForm::new(Remembered::default());
        f.set_focus(Focus::SystemInfo);
        f.key(Key::Space);
        assert!(!f.include_system_info());
        f.toggle_system_info();
        assert!(f.include_system_info());
    }

    #[test]
    fn email_and_message_are_required_and_editing_clears_the_error() {
        let mut f = FeedbackForm::new(Remembered::default());
        f.set_message("hi");
        f.set_email("   ");
        assert_eq!(f.key(Key::Enter), Action::Stay);
        assert_eq!((f.error(), f.focus()), (Some(NO_EMAIL), Focus::Email));
        f.set_email("a@b.c");
        assert_eq!(f.error(), None);
        f.set_message(" \n ");
        assert_eq!(f.press(Focus::Send), Action::Stay);
        assert_eq!((f.error(), f.focus()), (Some(NO_MESSAGE), Focus::Message));
        f.set_message("hi");
        assert_eq!(f.error(), None);
        assert!(!f.sending());
    }

    #[test]
    fn send_trims_the_fields_and_a_second_press_does_nothing() {
        let mut f = filled();
        f.set_focus(Focus::Kind);
        f.key(Key::Right);
        let Action::Send(sub) = f.key(Key::Enter) else {
            panic!("Return sends");
        };
        assert_eq!(
            sub,
            Submission {
                name: Some("Ada".into()),
                email: "ada@example.com".into(),
                kind: Kind::Feature,
                message: "It crashed.".into(),
                include_system_info: true,
            }
        );
        assert!(f.sending());
        assert_eq!(f.key(Key::Enter), Action::Stay);
        assert_eq!(f.key(Key::CmdEnter), Action::Stay);
        assert_eq!(f.press(Focus::Send), Action::Stay);
        assert_eq!(f.key(Key::Esc), Action::Stay, "a send that's out can't be left");
        assert_eq!(f.press(Focus::Cancel), Action::Stay);
    }

    #[test]
    fn an_empty_name_is_left_out() {
        let mut f = FeedbackForm::new(remembered("  ", "a@b.c"));
        f.set_message("hi");
        let Action::Send(sub) = f.key(Key::CmdEnter) else {
            panic!("⌘Return sends");
        };
        assert_eq!(sub.name, None);
    }

    #[test]
    fn return_in_the_message_is_a_new_line_and_on_cancel_cancels() {
        let mut f = filled();
        f.set_focus(Focus::Message);
        assert_eq!(f.key(Key::Enter), Action::Stay);
        assert!(!f.sending());
        assert!(matches!(f.key(Key::CmdEnter), Action::Send(_)));
        let mut f = filled();
        f.set_focus(Focus::Cancel);
        assert_eq!(f.key(Key::Enter), Action::Close);
    }

    #[test]
    fn cancel_and_esc_close_without_sending() {
        let mut f = filled();
        assert_eq!(f.key(Key::Esc), Action::Close);
        assert_eq!(f.press(Focus::Cancel), Action::Close);
        assert!(!f.sending());
    }

    #[test]
    fn a_failure_keeps_the_draft_and_allows_a_retry() {
        let mut f = filled();
        assert!(matches!(f.press(Focus::Send), Action::Send(_)));
        f.finished(Err(FeedbackError::RateLimited));
        assert_eq!(f.error(), Some("Too many submissions. Please try again later."));
        assert!(!f.sending() && !f.sent());
        assert_eq!(f.message(), "  It crashed.\n");
        assert!(matches!(f.press(Focus::Send), Action::Send(_)), "it can try again");
    }

    #[test]
    fn success_shows_the_thank_you_whose_keys_close_it() {
        let mut f = filled();
        f.press(Focus::Send);
        f.finished(Ok(()));
        assert!(f.sent() && !f.sending());
        assert_eq!(f.focus(), Focus::Close);
        assert_eq!(f.key(Key::Tab), Action::Stay);
        assert_eq!(f.focus(), Focus::Close);
        assert_eq!(f.press(Focus::Send), Action::Stay, "it never sends again");
        assert_eq!(f.key(Key::Enter), Action::Close);
        assert_eq!(f.key(Key::Esc), Action::Close);
    }
}
