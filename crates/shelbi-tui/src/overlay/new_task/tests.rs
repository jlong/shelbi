use super::*;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use shelbi_app::CommandKind;
use shelbi_core::{subtask_workflow, task_workflow};

fn dialog() -> NewTaskDialog {
    NewTaskDialog::new("alpha", &[task_workflow(), subtask_workflow()], "task")
}

fn render(dialog: &mut NewTaskDialog, w: u16, h: u16) -> (Buffer, Rect) {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut rect = Rect::default();
    term.draw(|f| {
        rect = dialog.rect(f.area());
        dialog.render(f, rect);
    })
    .unwrap();
    (term.backend().buffer().clone(), rect)
}

/// The first cell of `needle`, scanning rows top to bottom.
fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    (0..buf.area.height).find_map(|y| {
        let cells: Vec<String> = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
        (0..cells.len()).find(|&x| cells[x..].concat().starts_with(needle)).map(|x| (x as u16, y))
    })
}

fn ch(c: char) -> KeyChord {
    KeyChord::new(Code::Char(c), Mods::NONE)
}

fn key(code: Code) -> KeyChord {
    KeyChord::new(code, Mods::NONE)
}

fn typed(d: &mut NewTaskDialog, text: &str) {
    for c in text.chars() {
        assert_eq!(d.key(ch(c)), Step::Stay);
    }
}

fn created(step: Step) -> CreateRequest {
    match step {
        Step::Create(req) => req,
        other => panic!("expected a create, got {other:?}"),
    }
}

#[test]
fn the_dialog_shows_the_desktops_fields_and_copy_in_its_order() {
    let mut d = dialog();
    let (buf, rect) = render(&mut d, 120, 50);
    let order = [
        model::TITLE,
        model::SUBTITLE,
        model::TITLE_LABEL,
        model::WORKFLOW_LABEL,
        model::DESCRIPTION_LABEL,
        model::WRITE_UP,
        "Expand this draft into a complete task",
        model::CREATE_HINT_TERMINAL,
    ];
    let ys: Vec<u16> = order.iter().map(|t| find(&buf, t).unwrap_or_else(|| panic!("{t} shows")).1).collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "top to bottom in the desktop's order: {ys:?}");
    // Workflow, Status and Priority share a row, in that order, with their
    // defaults.
    let xs: Vec<(u16, u16)> = [model::WORKFLOW_LABEL, model::STATUS_LABEL, model::PRIORITY_LABEL]
        .iter()
        .map(|t| find(&buf, t).unwrap())
        .collect();
    assert!(xs.iter().all(|(_, y)| *y == xs[0].1) && xs[0].0 < xs[1].0 && xs[1].0 < xs[2].0, "{xs:?}");
    let values = ["Task", "◌ Backlog", "End of queue"];
    let vs: Vec<(u16, u16)> = values.iter().map(|t| find(&buf, t).unwrap_or_else(|| panic!("{t} shows"))).collect();
    assert!(vs.iter().all(|(_, y)| *y == xs[0].1 + 2), "the values in the boxes: {vs:?}");
    // The switch is off; Cancel and Create Task sit on the hint's row.
    assert!(find(&buf, "( ○ )").is_some());
    let hint = find(&buf, model::CREATE_HINT_TERMINAL).unwrap();
    let cancel = find(&buf, model::CANCEL).unwrap();
    let create = find(&buf, model::CREATE).unwrap();
    assert!(cancel.1 == hint.1 && create.1 == hint.1 && hint.0 < cancel.0 && cancel.0 < create.0);
    assert_eq!(rect.width, PANEL_W);
    crate::overlay::assert_dialog_border(&buf, rect).unwrap();
}

#[test]
fn typing_fills_the_title_and_ctrl_return_creates_one_request() {
    let mut d = dialog();
    assert_eq!(d.key(KeyChord::new(Code::Enter, Mods::CONTROL)), Step::Stay, "no title: nothing created");
    typed(&mut d, "Write docs");
    assert_eq!(d.form().title(), "Write docs");
    // Tab to the Description; Return there is a new line.
    for _ in 0..4 {
        d.key(key(Code::Tab));
    }
    assert_eq!(d.form().focus(), Field::Description);
    typed(&mut d, "One");
    assert_eq!(d.key(key(Code::Enter)), Step::Stay);
    typed(&mut d, "Two");
    assert_eq!(d.form().description(), "One\nTwo");
    let req = created(d.key(KeyChord::new(Code::Enter, Mods::CONTROL)));
    assert_eq!(
        req.command,
        CommandKind::AddIssue {
            title: "Write docs".into(),
            body: Some("One\nTwo".into()),
            workflow: None,
            status: Some("backlog".into()),
            top: false,
        }
    );
    assert!(!req.write_up);
    assert!(d.form().creating());
    assert_eq!(d.finished(Ok(())), Step::Close);
}

#[test]
fn esc_cancels_and_a_failed_create_keeps_the_draft() {
    let mut d = dialog();
    typed(&mut d, "T");
    created(d.key(KeyChord::new(Code::Enter, Mods::SUPER)));
    assert_eq!(d.finished(Err("daemon down".into())), Step::Stay);
    let (buf, _) = render(&mut d, 120, 50);
    assert!(find(&buf, "daemon down").is_some(), "the error shows");
    assert_eq!(d.key(key(Code::Esc)), Step::Close);
}

#[test]
fn the_pickers_open_by_keyboard_and_mouse_and_status_follows_the_workflow() {
    let mut d = dialog();
    d.key(key(Code::Tab));
    assert_eq!(d.form().focus(), Field::Workflow);
    // Return opens it; ↓ Return picks the second workflow.
    d.key(key(Code::Enter));
    let (buf, _) = render(&mut d, 120, 50);
    assert!(find(&buf, "✓ Task").is_some(), "the open picker checks the chosen row");
    d.key(key(Code::Down));
    d.key(key(Code::Enter));
    assert_eq!(d.form().workflow().name, "subtask");

    // Click Status, then its second row.
    let (buf, _) = render(&mut d, 120, 50);
    let (x, y) = find(&buf, "◌ Backlog").unwrap();
    d.handle_click(x, y);
    assert_eq!(d.form().menu().map(|(m, _)| m), Some(Menu::Status));
    let (buf, _) = render(&mut d, 120, 50);
    let (x, y) = find(&buf, "○ Todo").expect("the Todo row");
    d.handle_click(x, y);
    assert_eq!(d.form().status().id, "todo");
    assert_eq!(d.form().menu(), None);

    // Priority by click: Top of queue.
    let (buf, _) = render(&mut d, 120, 50);
    let (x, y) = find(&buf, "End of queue").unwrap();
    d.handle_click(x, y);
    let (buf, _) = render(&mut d, 120, 50);
    let (x, y) = find(&buf, "Top of queue").unwrap();
    d.handle_click(x, y);
    assert_eq!(d.form().position(), model::Position::Top);
}

#[test]
fn clicks_toggle_the_switch_and_press_the_buttons() {
    let mut d = dialog();
    let (buf, _) = render(&mut d, 120, 50);
    let (x, y) = find(&buf, model::WRITE_UP).unwrap();
    assert_eq!(d.handle_click(x, y), Step::Stay);
    assert!(d.form().write_up());
    let (buf, _) = render(&mut d, 120, 50);
    assert!(find(&buf, "( ● )").is_some(), "the switch shows on");

    // Create Task is inert with no title; with one, it creates.
    let (x, y) = find(&buf, model::CREATE).unwrap();
    assert_eq!(d.handle_click(x, y), Step::Stay);
    let (tx, ty) = find(&buf, model::TITLE_PLACEHOLDER).unwrap();
    d.handle_click(tx, ty);
    typed(&mut d, "Ship");
    let req = created(d.handle_click(x, y));
    assert!(req.write_up);

    let mut d = dialog();
    let (buf, _) = render(&mut d, 120, 50);
    let (x, y) = find(&buf, model::CANCEL).unwrap();
    assert_eq!(d.handle_click(x, y), Step::Close);
}

#[test]
fn a_narrow_terminal_stacks_the_pickers() {
    let mut d = dialog();
    let (buf, rect) = render(&mut d, 44, 70);
    assert!(rect.width <= 44);
    let w = find(&buf, model::WORKFLOW_LABEL).unwrap();
    let s = find(&buf, model::STATUS_LABEL).unwrap();
    assert!(s.1 > w.1, "stacked: {w:?} {s:?}");
}
