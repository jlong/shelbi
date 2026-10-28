//! Contact Us in the shell (`tui-feedback-dialog`): the footer's `📣` and
//! the palette's Send Feedback open it over the window, it takes the keys,
//! clicks and pastes ahead of the session under it, and a send runs off the
//! UI thread through the shell's [`FeedbackService`] (a fake here: no test
//! reaches the network) and lands back in the dialog.

use super::*;
use shelbi_app::feedback::{form, Client, FeedbackService, HttpResponse, Os, Remembered, SystemInfo};
use std::sync::Mutex;

/// The requests the fake endpoint saw, and its answers in order (then 200).
#[derive(Clone, Default)]
struct Endpoint {
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    answers: Arc<Mutex<Vec<Result<HttpResponse, String>>>>,
}

impl Endpoint {
    fn service(&self, remembered: PathBuf) -> FeedbackService {
        let me = self.clone();
        FeedbackService {
            client: Client::Tui,
            transport: Arc::new(move |body: &str| {
                me.bodies.lock().unwrap().push(serde_json::from_str(body).unwrap());
                let mut answers = me.answers.lock().unwrap();
                if answers.is_empty() {
                    Ok(HttpResponse { status: 200, body: "{}".into() })
                } else {
                    answers.remove(0)
                }
            }),
            system: Arc::new(|| SystemInfo { app_version: "9.8.7".into(), os: Os::Other("Linux 6.8.0".into()) }),
            now: Arc::new(|| "2026-10-28T12:00:00Z".into()),
            remembered: Some(remembered),
        }
    }

    fn bodies(&self) -> Vec<serde_json::Value> {
        self.bodies.lock().unwrap().clone()
    }
}

fn dialog(st: &ShellState) -> &crate::overlay::feedback::FeedbackDialog {
    match &st.overlay {
        Some(ActiveOverlay::Feedback(d)) => d,
        _ => panic!("Contact Us is open"),
    }
}

fn press(st: &mut ShellState, code: KeyCode, mods: KeyModifiers) {
    st.handle_event(Event::Key(KeyEvent::new(code, mods)));
}

fn type_text(st: &mut ShellState, text: &str) {
    for c in text.chars() {
        type_key(st, c);
    }
}

/// Poll the send until it lands (it runs on its own thread).
fn land(st: &mut ShellState) {
    for _ in 0..400 {
        if st.poll_feedback_job() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the send never came back");
}

fn temp_remembered() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(shelbi_app::feedback::REMEMBERED_FILE);
    (dir, path)
}

#[test]
fn the_footer_and_the_palette_open_contact_us() {
    let mut st = settings_state();
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    draw_frame(&mut term, &mut st).unwrap();
    let (fx, fy) = find_on_screen(&term, "📣").expect("the footer's Send Feedback");
    st.handle_event(Event::Mouse(left_click(fx, fy)));
    assert!(matches!(st.overlay, Some(ActiveOverlay::Feedback(_))), "the 📣 opens it");
    draw_frame(&mut term, &mut st).unwrap();
    assert!(find_on_screen(&term, form::TITLE).is_some(), "Contact Us is drawn");
    press(&mut st, KeyCode::Esc, KeyModifiers::NONE);
    assert!(st.overlay.is_none(), "Esc cancels");

    // The palette: Ctrl+Space, its name, Return.
    st.handle_event(Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)));
    type_text(&mut st, "Send Feedback");
    press(&mut st, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(st.overlay, Some(ActiveOverlay::Feedback(_))), "the palette opens it");
}

#[test]
fn a_send_goes_off_the_ui_thread_says_its_the_tui_and_remembers_the_name() {
    let mut st = settings_state();
    let endpoint = Endpoint::default();
    let (_dir, path) = temp_remembered();
    st.feedback_service = endpoint.service(path.clone());
    st.dispatch_effect(Effect::SendFeedback);
    assert_eq!(dialog(&st).form().focus(), form::Focus::Name, "nothing remembered: Name first");
    type_text(&mut st, "Ada");
    press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut st, "ada@example.com");
    press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    press(&mut st, KeyCode::Right, KeyModifiers::NONE);
    press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    // A paste lands in Message, not the session under the dialog.
    st.handle_event(Event::Paste("The board froze\r\nafter sleep.".into()));
    assert_eq!(dialog(&st).form().message(), "The board froze\nafter sleep.");
    press(&mut st, KeyCode::Enter, KeyModifiers::CONTROL);
    assert!(dialog(&st).form().sending(), "the dialog waits on the send");
    land(&mut st);
    assert_eq!(
        endpoint.bodies(),
        [serde_json::json!({
            "appName": "Shelbi TUI",
            "name": "Ada",
            "email": "ada@example.com",
            "type": "feature",
            "message": "The board froze\nafter sleep.",
            "timestamp": "2026-10-28T12:00:00Z",
            "appVersion": "9.8.7",
            "osVersion": "Linux 6.8.0",
        })]
    );
    assert!(dialog(&st).form().sent(), "the thank-you shows");
    assert_eq!(Remembered::load(&path), Remembered { name: "Ada".into(), email: "ada@example.com".into() });
    press(&mut st, KeyCode::Enter, KeyModifiers::NONE);
    assert!(st.overlay.is_none(), "Return closes the thank-you");

    // Next time the name and email are filled in and Message has the keyboard.
    st.dispatch_effect(Effect::SendFeedback);
    let form = dialog(&st).form();
    assert_eq!((form.name(), form.email(), form.focus()), ("Ada", "ada@example.com", form::Focus::Message));
}

#[test]
fn a_failure_shows_in_the_dialog_and_keeps_the_draft() {
    let mut st = settings_state();
    let endpoint = Endpoint::default();
    endpoint.answers.lock().unwrap().push(Err("Could not resolve host: 32pixels.co".into()));
    let (_dir, path) = temp_remembered();
    st.feedback_service = endpoint.service(path);
    st.dispatch_effect(Effect::SendFeedback);
    type_text(&mut st, "Ada");
    press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut st, "ada@example.com");
    for _ in 0..2 {
        press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    }
    type_text(&mut st, "Retry me");
    press(&mut st, KeyCode::Esc, KeyModifiers::NONE);
    assert!(st.overlay.is_none(), "Esc cancels unsent");
    assert!(endpoint.bodies().is_empty());

    st.dispatch_effect(Effect::SendFeedback);
    assert_eq!(dialog(&st).form().name(), "", "a cancelled draft remembers nothing");
    type_text(&mut st, "Ada");
    press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut st, "ada@example.com");
    for _ in 0..2 {
        press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    }
    type_text(&mut st, "Retry me");
    press(&mut st, KeyCode::Enter, KeyModifiers::SUPER);
    press(&mut st, KeyCode::Esc, KeyModifiers::NONE);
    assert!(st.overlay.is_some(), "a send that's out can't be cancelled");
    land(&mut st);
    let form = dialog(&st).form();
    assert_eq!(form.error(), Some("Couldn't send your message: Could not resolve host: 32pixels.co"));
    assert_eq!(form.message(), "Retry me", "the draft is kept");
    assert!(!form.sending() && !form.sent());
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(100, 45)).unwrap();
    draw_frame(&mut term, &mut st).unwrap();
    assert!(find_on_screen(&term, "Couldn't send your message").is_some(), "shown inline");
    press(&mut st, KeyCode::Enter, KeyModifiers::SUPER);
    land(&mut st);
    assert!(dialog(&st).form().sent(), "the retry went");
    assert_eq!(endpoint.bodies().len(), 2);
}
