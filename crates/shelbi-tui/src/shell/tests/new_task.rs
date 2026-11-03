//! New task in the shell (`new-task-dialog`): the board's `n` and its
//! `+ New issue`, and the palette's New Issue, open it over the view; it
//! takes the keys and pastes ahead of what's under it; Create Task runs
//! off the UI thread through the shell's runner (a recorder here: no test
//! reaches the daemon or a session) and its outcome lands on the status
//! line.

use super::*;
use shelbi_app::new_task::{self, Created, CreateRequest, WriteUp};

fn dialog(st: &ShellState) -> &crate::overlay::new_task::NewTaskDialog {
    match &st.overlay {
        Some(ActiveOverlay::NewTask(d)) => d,
        _ => panic!("New task is open"),
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

/// Poll the create until it lands (it runs on its own thread).
fn land(st: &mut ShellState) {
    for _ in 0..400 {
        if st.poll_new_task_job() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the create never came back");
}

/// A runner that records each request and answers `write_up`.
fn recorder(seen: Arc<Mutex<Vec<(String, CreateRequest)>>>, write_up: WriteUp) -> NewTaskRunner {
    Arc::new(move |project: &str, request: &CreateRequest| {
        seen.lock().unwrap().push((project.to_string(), request.clone()));
        Ok(Created {
            id: Some("write-docs".into()),
            title: request.title().to_string(),
            write_up: if request.write_up { write_up.clone() } else { WriteUp::NotAsked },
        })
    })
}

#[test]
fn the_board_n_its_new_issue_and_the_palette_open_new_task() {
    let mut st = settings_state();
    st.kanban.workflows = vec![shelbi_core::task_workflow()];
    st.kanban.default_workflow_name = "task".into();
    st.main_view = MainView::Native(View::Issues);
    st.client.focus_main();
    press(&mut st, KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(matches!(st.overlay, Some(ActiveOverlay::NewTask(_))), "`n` opens it");
    assert_eq!(dialog(&st).form().workflow().name, "task", "on the board's workflows");
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(120, 50)).unwrap();
    draw_frame(&mut term, &mut st).unwrap();
    assert!(find_on_screen(&term, new_task::SUBTITLE).is_some(), "New task is drawn");
    press(&mut st, KeyCode::Esc, KeyModifiers::NONE);
    assert!(st.overlay.is_none(), "Esc cancels");

    // The board's title row: `+ New issue`.
    draw_frame(&mut term, &mut st).unwrap();
    let (x, y) = find_on_screen(&term, "+ New issue").expect("the board shows New issue");
    st.handle_event(Event::Mouse(left_click(x + 2, y)));
    assert!(matches!(st.overlay, Some(ActiveOverlay::NewTask(_))), "a click opens it");
    press(&mut st, KeyCode::Esc, KeyModifiers::NONE);

    // The palette: Ctrl+Space, its name, Return.
    st.handle_event(Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)));
    type_text(&mut st, "New Issue");
    press(&mut st, KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(st.overlay, Some(ActiveOverlay::NewTask(_))), "the palette opens it");
}

#[test]
fn create_task_runs_one_create_off_the_ui_thread_and_reports_on_the_status_line() {
    let mut st = settings_state();
    let seen = Arc::new(Mutex::new(Vec::new()));
    st.new_task_runner = recorder(seen.clone(), WriteUp::Requested);
    st.dispatch_effect(Effect::NewIssue { project: "proj".into() });
    type_text(&mut st, "Write docs");
    // A paste into the Description, past the pickers.
    for _ in 0..4 {
        press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    }
    st.handle_event(Event::Paste("First line\r\nsecond".into()));
    assert_eq!(dialog(&st).form().description(), "First line\nsecond");
    // The switch on, then Ctrl+Return.
    press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    press(&mut st, KeyCode::Char(' '), KeyModifiers::NONE);
    press(&mut st, KeyCode::Enter, KeyModifiers::CONTROL);
    assert!(dialog(&st).form().creating(), "the dialog waits on the create");
    land(&mut st);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one create");
    assert_eq!(seen[0].0, "proj");
    assert!(seen[0].1.write_up);
    assert!(matches!(&seen[0].1.command, CommandKind::AddIssue { title, body, .. }
        if title == "Write docs" && body.as_deref() == Some("First line\nsecond")));
    assert!(st.overlay.is_none(), "created: the dialog closes");
    assert_eq!(st.status.as_deref(), Some("Created write-docs; the Orchestrator is writing it up"));
}

#[test]
fn an_orchestrator_that_isnt_running_still_creates_and_says_so() {
    let mut st = settings_state();
    let seen = Arc::new(Mutex::new(Vec::new()));
    st.new_task_runner = recorder(seen.clone(), WriteUp::NotRunning);
    st.dispatch_effect(Effect::NewIssue { project: "proj".into() });
    type_text(&mut st, "Write docs");
    for _ in 0..5 {
        press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    }
    press(&mut st, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(dialog(&st).form().write_up());
    press(&mut st, KeyCode::Enter, KeyModifiers::SUPER);
    land(&mut st);
    assert_eq!(seen.lock().unwrap().len(), 1);
    let status = st.status.clone().unwrap_or_default();
    assert!(status.starts_with("Created write-docs."), "{status}");
    assert!(status.contains("isn\u{2019}t running") && status.contains("wasn\u{2019}t requested"), "{status}");
}

#[test]
fn a_failed_create_keeps_the_dialog_and_the_draft() {
    let mut st = settings_state();
    st.new_task_runner = Arc::new(|_: &str, _: &CreateRequest| Err("Couldn\u{2019}t create the task: daemon down".to_string()));
    st.dispatch_effect(Effect::NewIssue { project: "proj".into() });
    type_text(&mut st, "Write docs");
    press(&mut st, KeyCode::Enter, KeyModifiers::NONE);
    land(&mut st);
    let form = dialog(&st).form();
    assert_eq!(form.error(), Some("Couldn\u{2019}t create the task: daemon down"));
    assert_eq!(form.title(), "Write docs");
    assert!(!form.creating());
}
