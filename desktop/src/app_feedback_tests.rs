//! Contact Us in the window (`dt-feedback-dialog`): the sidebar footer's
//! Settings and megaphone in the left corner, the dialog opened from the
//! megaphone and from Help ▸ Send Feedback… (both the registry's Send
//! Feedback, which the TUI's footer runs too), and a send end to end through
//! a fake transport (the request body, each error class, validation, no
//! duplicate sends, the thank-you, the name and email remembered).
//!
//! The window is the smoke test's headless one ([`Harness`]); every test
//! sets its own [`FeedbackService`], so nothing reaches the real endpoint.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::TestAppContext;
use serde_json::{json, Value};

use super::smoke::Harness;
use super::*;
use crate::menus::tree::{AppCommand, MenuAction};
use crate::platform::feedback::form::{Focus, NO_EMAIL, NO_MESSAGE};
use crate::platform::feedback::{Client, FeedbackGlobal, FeedbackService, HttpResponse, Os, Remembered, SystemInfo};

const NOW: &str = "2026-10-27T12:00:00Z";

/// What the fake endpoint saw and will answer.
#[derive(Clone, Default)]
struct Endpoint {
    bodies: Arc<Mutex<Vec<Value>>>,
    answers: Arc<Mutex<VecDeque<Result<HttpResponse, String>>>>,
}

impl Endpoint {
    fn answer(&self, status: u16, body: &str) {
        self.answers.lock().unwrap().push_back(Ok(HttpResponse {
            status,
            body: body.into(),
        }));
    }

    fn unreachable(&self, why: &str) {
        self.answers.lock().unwrap().push_back(Err(why.into()));
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
}

struct Feedback {
    h: Harness,
    endpoint: Endpoint,
    remembered: PathBuf,
}

fn temp_file(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shelbi-desktop-feedback-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(crate::platform::feedback::REMEMBERED_FILE)
}

impl Feedback {
    fn open(cx: &mut TestAppContext, tag: &str) -> Feedback {
        let mut h = Harness::open(cx);
        let endpoint = Endpoint::default();
        let remembered = temp_file(tag);
        let fake = endpoint.clone();
        let service = FeedbackService {
            client: Client::Desktop,
            transport: Arc::new(move |body: &str| {
                fake.bodies.lock().unwrap().push(serde_json::from_str(body).unwrap());
                fake.answers.lock().unwrap().pop_front().unwrap_or(Ok(HttpResponse {
                    status: 200,
                    body: "{}".into(),
                }))
            }),
            system: Arc::new(|| SystemInfo {
                app_version: "9.8.7".into(),
                os: Os::MacOs("15.1.0".into()),
            }),
            now: Arc::new(|| NOW.to_string()),
            remembered: Some(remembered.clone()),
        };
        h.cx.update(|_, cx| cx.set_global(FeedbackGlobal(service)));
        h.focus_window();
        Feedback { h, endpoint, remembered }
    }

    fn dialog(&self) -> Option<Entity<FeedbackDialog>> {
        self.h.window.read_with(&self.h.cx, |w, _| w.feedback())
    }

    fn open_dialog(&mut self) -> Entity<FeedbackDialog> {
        self.h.click_selector("sidebar-feedback");
        self.run_send_feedback("the megaphone");
        self.dialog().expect("Contact Us is open")
    }

    /// `from` ran the registry's Send Feedback (the tap holds it): run it.
    fn run_send_feedback(&mut self, from: &str) {
        let calls = self.h.take_calls();
        assert_eq!(calls, [HostCall::SendFeedback], "{from} runs the registry's Send Feedback");
        self.h.perform(calls);
    }

    fn read<R>(&self, f: impl FnOnce(&FeedbackDialog) -> R) -> R {
        let dialog = self.dialog().expect("Contact Us is open");
        self.h.cx.read(|cx| f(dialog.read(cx)))
    }

    /// Type `text` into `input` as the user would: focused, emptied, typed.
    fn type_into(&mut self, input: Entity<gpui_kit::component::input::InputState>, text: &str) {
        self.h.cx.update(|window, cx| {
            let focus = gpui::Focusable::focus_handle(input.read(cx), cx);
            window.focus(&focus, cx);
            input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        });
        self.h.cx.simulate_input(text);
        self.h.draw();
    }

    fn type_message(&mut self, text: &str) {
        let (_, _, message) = self.read(|d| d.inputs());
        self.h.cx.update(|window, cx| {
            let focus = gpui::Focusable::focus_handle(message.read(cx), cx);
            window.focus(&focus, cx);
            message.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        });
        self.h.cx.simulate_input(text);
        self.h.draw();
    }

    /// Fill Name, Email and Message.
    fn fill(&mut self, name: &str, email: &str, message: &str) {
        let (name_input, email_input, _) = self.read(|d| d.inputs());
        self.type_into(name_input, name);
        self.type_into(email_input, email);
        self.type_message(message);
    }

    fn error(&self) -> Option<String> {
        self.read(|d| d.form().error().map(str::to_string))
    }

    fn message_text(&self) -> String {
        let (_, _, message) = self.read(|d| d.inputs());
        self.h.cx.read(|cx| message.read(cx).value().to_string())
    }
}

fn expected(name: Option<&str>, kind: &str, message: &str, system: bool) -> Value {
    let mut body = json!({
        "appName": "Shelbi",
        "email": "ada@example.com",
        "type": kind,
        "message": message,
        "timestamp": NOW,
    });
    if let Some(name) = name {
        body["name"] = name.into();
    }
    if system {
        body["appVersion"] = "9.8.7".into();
        body["macOSVersion"] = "15.1.0".into();
    }
    body
}

#[gpui::test]
fn the_footer_puts_settings_and_feedback_together_in_the_left_corner(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "footer");
    f.h.draw();
    let settings = f.h.cx.debug_bounds("sidebar-settings").expect("Settings is drawn");
    let feedback = f.h.cx.debug_bounds("sidebar-feedback").expect("the megaphone is drawn");
    let zen = f.h.cx.debug_bounds("sidebar-zen").expect("the Zen pill stays");
    assert_eq!(settings.left(), zen.left(), "Settings starts the row, at the footer's inset");
    assert_eq!(feedback.left() - settings.right(), px(4.0), "the megaphone sits 4pt after it");
    assert_eq!(settings.center().y, feedback.center().y);
    assert!(feedback.bottom() <= zen.top(), "both above the Zen pill");
    assert_eq!(settings.size, feedback.size);

    // Settings still opens Settings.
    f.h.click_selector("sidebar-settings");
    assert!(f.h.window.read_with(&f.h.cx, |w, _| matches!(w.route, Route::Settings(_))));
}

#[gpui::test]
fn the_megaphone_opens_contact_us_and_esc_or_cancel_close_it_unsent(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "cancel");
    f.open_dialog();
    assert!(f.h.cx.debug_bounds("feedback-dialog").is_some());
    assert_eq!(f.read(|d| d.form().focus()), Focus::Name, "nothing remembered: Name first");
    f.fill("Ada", "ada@example.com", "Hello");
    f.h.key("escape");
    assert!(f.dialog().is_none(), "Esc cancels");

    f.open_dialog();
    f.fill("Ada", "ada@example.com", "Hello");
    f.h.click_selector("feedback-cancel");
    assert!(f.dialog().is_none(), "Cancel closes it");
    assert!(f.endpoint.bodies().is_empty(), "closing never sends");
    assert!(!f.remembered.exists(), "nor remembers");
}

#[gpui::test]
fn help_send_feedback_opens_it_over_the_palette_and_cmd_w_cancels_it(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "menu");
    f.h.key("ctrl-space");
    assert!(f.h.window.read_with(&f.h.cx, |w, _| w.overlay.is_some()), "the palette is up");
    f.h.cx.dispatch_action(menus::MenuInvoke {
        target: MenuAction::Command("action:send-feedback".into()),
        generation: 0,
    });
    f.h.cx.run_until_parked();
    assert!(f.h.window.read_with(&f.h.cx, |w, _| w.overlay.is_none()), "the palette closed first");
    f.run_send_feedback("Help ▸ Send Feedback…");
    let first = f.dialog().expect("Help ▸ Send Feedback… opened it");
    // It opens once; the window's chords wait behind it.
    f.h.cx.dispatch_action(menus::MenuInvoke {
        target: MenuAction::Command("action:send-feedback".into()),
        generation: 0,
    });
    f.h.cx.run_until_parked();
    assert!(f.h.take_calls().is_empty(), "the menus' commands wait behind the dialog");
    assert_eq!(f.dialog(), Some(first));
    f.h.cx.dispatch_action(menus::MenuInvoke {
        target: MenuAction::App(AppCommand::CloseWindow),
        generation: 0,
    });
    f.h.cx.run_until_parked();
    assert!(f.dialog().is_none(), "⌘W cancels the dialog, not the window");
    assert!(f.endpoint.bodies().is_empty());
}

#[gpui::test]
fn sending_posts_the_payload_with_system_info_shows_thanks_and_remembers(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "send");
    f.open_dialog();
    f.fill("  Ada ", " ada@example.com ", "  The board froze.\n");
    f.h.click_selector("feedback-send");
    assert_eq!(f.endpoint.bodies(), [expected(Some("Ada"), "bug", "The board froze.", true)]);
    assert!(f.h.cx.debug_bounds("feedback-thanks").is_some(), "the thank-you shows");
    assert!(f.h.cx.debug_bounds("feedback-dialog").is_none());
    assert_eq!(
        Remembered::load(&f.remembered),
        Remembered {
            name: "Ada".into(),
            email: "ada@example.com".into()
        }
    );
    f.h.click_selector("feedback-close");
    assert!(f.dialog().is_none());

    // Next time the name and email are filled in and Message has the keyboard.
    let dialog = f.open_dialog();
    let (name, email, _) = f.read(|d| d.inputs());
    f.h.cx.read(|cx| {
        assert_eq!(name.read(cx).value(), "Ada");
        assert_eq!(email.read(cx).value(), "ada@example.com");
        assert_eq!(dialog.read(cx).form().focus(), Focus::Message);
    });
}

#[gpui::test]
fn the_kind_and_the_system_info_checkbox_shape_the_payload(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "kind");
    f.open_dialog();
    f.fill("", "ada@example.com", "Dark dock icon, please");
    f.h.click_selector("feedback-kind-feature");
    f.h.click_selector("feedback-system-info");
    assert!(!f.read(|d| d.form().include_system_info()));
    f.h.click_selector("feedback-send");
    assert_eq!(f.endpoint.bodies(), [expected(None, "feature", "Dark dock icon, please", false)]);
}

#[gpui::test]
fn the_keyboard_walks_picks_and_sends(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "keys");
    f.open_dialog();
    f.fill("Ada", "ada@example.com", "Keys");
    // From Email, Tab reaches the segmented control; → picks Other.
    let (_, email, _) = f.read(|d| d.inputs());
    f.type_into(email, "ada@example.com");
    f.h.key("tab");
    assert_eq!(f.read(|d| d.form().focus()), Focus::Kind);
    f.h.key("right");
    f.h.key("right");
    // Tab to Message: Return is its new line, not a send.
    f.h.key("tab");
    assert_eq!(f.read(|d| d.form().focus()), Focus::Message);
    f.h.key("enter");
    assert!(f.endpoint.bodies().is_empty(), "Return in Message types a new line");
    // Tab on to the checkbox, Space unchecks it; Return sends.
    f.h.key("tab");
    assert_eq!(f.read(|d| d.form().focus()), Focus::SystemInfo);
    f.h.key("space");
    f.h.key("enter");
    assert_eq!(f.endpoint.bodies(), [expected(Some("Ada"), "other", "Keys", false)]);
    f.h.key("enter");
    assert!(f.dialog().is_none(), "Return closes the thank-you");
}

#[gpui::test]
fn cmd_return_sends_from_the_message(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "cmd-return");
    f.open_dialog();
    f.fill("", "ada@example.com", "Quick one");
    f.h.key("cmd-enter");
    assert_eq!(f.endpoint.bodies(), [expected(None, "bug", "Quick one", true)]);
}

#[gpui::test]
fn email_and_message_are_required(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "validation");
    f.open_dialog();
    f.fill("Ada", "  ", "Hello");
    f.h.click_selector("feedback-send");
    assert_eq!(f.error().as_deref(), Some(NO_EMAIL));
    assert!(f.h.cx.debug_bounds("feedback-error").is_some(), "it shows under the form");
    assert_eq!(f.read(|d| d.form().focus()), Focus::Email);
    let (_, email, _) = f.read(|d| d.inputs());
    f.type_into(email, "ada@example.com");
    assert_eq!(f.error(), None, "editing a field clears it");
    f.type_message("   ");
    f.h.click_selector("feedback-send");
    assert_eq!(f.error().as_deref(), Some(NO_MESSAGE));
    assert!(f.endpoint.bodies().is_empty(), "nothing invalid is sent");
}

#[gpui::test]
fn each_failure_shows_inline_keeps_the_draft_and_allows_a_retry(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "errors");
    f.endpoint.answer(422, r#"{"error":"Email is invalid"}"#);
    f.endpoint.answer(429, "");
    f.endpoint.answer(500, "");
    f.endpoint.unreachable("Could not resolve host: 32pixels.co");
    f.endpoint.answer(200, "{}");
    f.open_dialog();
    f.fill("Ada", "ada@example.com", "Retry me");
    let expect = [
        "Email is invalid",
        "Too many submissions. Please try again later.",
        "Server error (HTTP 500)",
        "Couldn't send your message: Could not resolve host: 32pixels.co",
    ];
    for (n, message) in expect.iter().enumerate() {
        f.h.click_selector("feedback-send");
        assert_eq!(f.error().as_deref(), Some(*message));
        assert!(f.h.cx.debug_bounds("feedback-error").is_some());
        assert!(f.h.cx.debug_bounds("feedback-dialog").is_some(), "the form stays");
        assert_eq!(f.message_text(), "Retry me", "the draft is kept");
        assert_eq!(f.endpoint.bodies().len(), n + 1);
    }
    f.h.click_selector("feedback-send");
    assert!(f.h.cx.debug_bounds("feedback-thanks").is_some(), "the retry went");
    assert_eq!(f.endpoint.bodies().len(), 5);
}

#[gpui::test]
fn a_double_press_sends_once_and_shows_sending_meanwhile(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "double");
    let dialog = f.open_dialog();
    f.fill("Ada", "ada@example.com", "Once");
    let enter = shelbi_state::keymap::KeyChord::parse("enter").unwrap();
    // Two presses and a click before the send can come back.
    f.h.cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.key(enter, window, cx);
            d.key(enter, window, cx);
            d.press_for_test(Focus::Send, window, cx);
            assert!(d.form().sending());
        });
        window.draw(cx).clear(cx);
    });
    assert!(f.h.cx.debug_bounds("feedback-send").is_some(), "Sending… stands in for Send Message");
    f.h.cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.dismiss(cx);
            d.key(shelbi_state::keymap::KeyChord::parse("esc").unwrap(), window, cx);
        });
    });
    assert!(f.dialog().is_some(), "a send that's out can't be cancelled");
    f.h.cx.run_until_parked();
    assert_eq!(f.endpoint.bodies().len(), 1, "one request");
    assert_eq!(f.read(|d| d.sent().len()), 1);
    f.h.draw();
    assert!(f.h.cx.debug_bounds("feedback-thanks").is_some());
}

#[gpui::test]
fn an_unpicked_kind_takes_no_fill_under_the_pointer_and_send_still_lights(cx: &mut TestAppContext) {
    let mut f = Feedback::open(cx, "hover");
    f.open_dialog();
    f.fill("Ada", "ada@example.com", "Hover");
    let point_at = |f: &mut Feedback, selector: &'static str| {
        f.h.draw();
        let at = f.h.cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
        f.h.cx.simulate_mouse_move(at, None, gpui::Modifiers::none());
        // A hover found while painting shows in the next frame.
        f.h.draw();
        f.h.draw();
    };
    point_at(&mut f, "feedback-kind-feature");
    assert!(!crate::theme::motion::is_hovered("feedback-kind-feature"), "an unpicked segment has no hover fill");
    point_at(&mut f, "feedback-send");
    assert!(crate::theme::motion::is_hovered("feedback-send"), "a button still lights");
}
