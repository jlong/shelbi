use std::cell::RefCell;

use shelbi_core::{task_workflow, StatusCategory, Workflow};

use super::*;
use crate::exec::Mutation;

/// A workflow `name` with `statuses` (`(id, category)`), starting in
/// `initial` when given (else its first status).
fn workflow(name: &str, statuses: &[(&str, StatusCategory)], initial: Option<&str>) -> Workflow {
    let mut wf = task_workflow();
    let template = wf.statuses[0].clone();
    wf.name = name.to_string();
    wf.statuses = statuses
        .iter()
        .map(|(id, category)| {
            let mut s = template.clone();
            s.id = id.to_string();
            s.name = title_case(id);
            s.category = *category;
            s
        })
        .collect();
    wf.initial_status = initial.map(str::to_string);
    wf.transitions = None;
    wf
}

/// `task` (backlog, todo, in-progress, review, done), `research` (idea,
/// todo, done; starts in idea) and `triage` (todo, review; starts in
/// review). The project's default is `task`.
fn workflows() -> Vec<Workflow> {
    vec![
        workflow(
            "task",
            &[
                ("backlog", StatusCategory::Backlog),
                ("todo", StatusCategory::Ready),
                ("in-progress", StatusCategory::Active),
                ("review", StatusCategory::Handoff),
                ("done", StatusCategory::Done),
            ],
            None,
        ),
        workflow(
            "research",
            &[
                ("idea", StatusCategory::Backlog),
                ("todo", StatusCategory::Ready),
                ("done", StatusCategory::Done),
            ],
            Some("idea"),
        ),
        workflow(
            "triage",
            &[("todo", StatusCategory::Ready), ("review", StatusCategory::Handoff)],
            Some("review"),
        ),
    ]
}

fn form() -> NewTaskForm {
    NewTaskForm::new(&workflows(), "task")
}

fn created(action: Action) -> CreateRequest {
    match action {
        Action::Create(req) => req,
        other => panic!("expected a create, got {other:?}"),
    }
}

#[test]
fn a_new_draft_defaults_to_the_project_workflow_its_initial_status_and_the_end() {
    let f = form();
    assert_eq!(f.workflow().name, "task");
    assert_eq!(f.status().id, "backlog");
    assert_eq!(f.position(), Position::End);
    assert!(!f.write_up(), "the switch starts off");
    assert_eq!(f.focus(), Field::Title);

    // The project's default isn't always the first workflow, and a declared
    // initial status wins over the first.
    let f = NewTaskForm::new(&workflows(), "research");
    assert_eq!(f.workflow().name, "research");
    assert_eq!(f.status().id, "idea");
    let f = NewTaskForm::new(&workflows(), "triage");
    assert_eq!(f.status().id, "review");

    // An unknown default falls back to the first; none loaded, the shipped
    // default stands in.
    assert_eq!(NewTaskForm::new(&workflows(), "nope").workflow().name, "task");
    let f = NewTaskForm::new(&[], "default");
    assert_eq!(f.workflow().name, "default");
    assert_eq!(f.status().id, "backlog");
}

#[test]
fn the_model_carries_the_values_rows_and_status_glyph() {
    let mut f = form();
    f.set_title("Write docs");
    let m = f.model();
    assert_eq!(m.title, "Write docs");
    assert_eq!(m.workflow, "Task");
    assert_eq!(
        m.workflows.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
        ["Task", "Research", "Triage"]
    );
    assert!(m.workflows[0].selected && !m.workflows[1].selected);
    assert_eq!(m.status, "Backlog");
    assert_eq!(m.status_category, StatusCategory::Backlog);
    assert_eq!(m.statuses.len(), 5);
    assert_eq!(m.statuses[3].category, Some(StatusCategory::Handoff));
    assert_eq!(m.priority, "End of queue");
    assert_eq!(
        m.priorities.iter().map(|o| (o.label.as_str(), o.selected)).collect::<Vec<_>>(),
        [("Top of queue", false), ("End of queue", true)]
    );
    assert!(m.can_create && !m.creating && m.menu.is_none());
}

#[test]
fn changing_the_workflow_keeps_a_status_it_declares_and_resets_one_it_doesnt() {
    let mut f = form();
    f.pick_status(1); // todo
    f.pick_workflow(1); // research declares todo: kept
    assert_eq!(f.status().id, "todo");

    f.pick_status(0); // idea
    f.pick_workflow(2); // triage has no idea: its initial, review
    assert_eq!(f.workflow().name, "triage");
    assert_eq!(f.status().id, "review");
    assert_eq!(f.model().statuses.len(), 2, "the Status picker follows the workflow");

    f.pick_workflow(0); // task declares review: kept
    assert_eq!(f.status().id, "review");
}

#[test]
fn the_title_is_required() {
    let mut f = form();
    assert!(!f.can_create(), "blank: Create Task is disabled");
    f.set_title("   ");
    assert!(!f.can_create(), "whitespace is blank");
    f.set_focus(Field::Description);
    assert_eq!(f.key(Key::CmdEnter), Action::Stay, "nothing is created");
    assert_eq!(f.focus(), Field::Title, "the keyboard goes to the title");
    assert_eq!(f.press(Field::Create), Action::Stay);

    f.set_title("Ship it");
    assert!(f.can_create());
}

#[test]
fn the_create_request_carries_every_field_as_one_add_issue() {
    let mut f = form();
    f.set_title("  Restore the last project  ");
    f.set_description("Reopen it on launch.\n\n- And the picker\n\n");
    f.pick_workflow(1);
    f.pick_status(1);
    f.pick_position(Position::Top);
    f.toggle_write_up();
    let req = created(f.press(Field::Create));
    assert!(f.creating());
    assert_eq!(
        req.command,
        CommandKind::AddIssue {
            title: "Restore the last project".into(),
            body: Some("Reopen it on launch.\n\n- And the picker".into()),
            workflow: Some("research".into()),
            status: Some("todo".into()),
            top: true,
        }
    );
    assert!(req.write_up);
    // The registry turns it into the daemon's add.
    assert_eq!(
        req.mutation("alpha"),
        Mutation::AddIssue {
            project: "alpha".into(),
            title: "Restore the last project".into(),
            body: Some("Reopen it on launch.\n\n- And the picker".into()),
            workflow: Some("research".into()),
            status: Some("todo".into()),
            top: true,
        }
    );

    // The defaults: no body, the default workflow left to follow the
    // project's, the initial status, the end of the queue, no write-up.
    let mut f = form();
    f.set_title("Plain");
    let req = created(f.key(Key::Enter));
    assert_eq!(
        req.command,
        CommandKind::AddIssue {
            title: "Plain".into(),
            body: None,
            workflow: None,
            status: Some("backlog".into()),
            top: false,
        }
    );
    assert!(!req.write_up);
}

#[test]
fn while_the_create_is_out_nothing_acts_and_its_answer_closes_or_keeps_the_draft() {
    let mut f = form();
    f.set_title("Once");
    created(f.key(Key::CmdEnter));
    assert_eq!(f.key(Key::CmdEnter), Action::Stay, "no second create");
    assert_eq!(f.key(Key::Esc), Action::Stay, "can't cancel mid-create");
    f.toggle_write_up();
    assert!(!f.write_up());
    assert!(!f.model().can_create);

    assert_eq!(f.finished(Err("daemon down".into())), Action::Stay);
    assert_eq!(f.error(), Some("daemon down"));
    assert_eq!(f.title(), "Once", "the draft is kept");
    f.set_title("Once more");
    assert_eq!(f.error(), None, "editing clears it");
    created(f.key(Key::CmdEnter));
    assert_eq!(f.finished(Ok(())), Action::Close);
}

#[test]
fn keys_walk_the_controls_open_pickers_and_cancel() {
    let mut f = form();
    let mut seen = vec![f.focus()];
    for _ in 0..8 {
        f.key(Key::Tab);
        seen.push(f.focus());
    }
    assert_eq!(seen[..8], Field::ALL, "Tab walks in visual order");
    assert_eq!(seen[8], Field::Title, "and wraps");
    f.key(Key::BackTab);
    assert_eq!(f.focus(), Field::Create);

    // Return on a picker opens it on the current value; ↓ and Return pick.
    f.set_focus(Field::Status);
    f.key(Key::Enter);
    assert_eq!(f.menu(), Some((Menu::Status, 0)));
    f.key(Key::Down);
    f.key(Key::Down);
    f.key(Key::Enter);
    assert_eq!(f.status().id, "in-progress");
    assert_eq!(f.menu(), None);

    // Esc closes an open picker, not the dialog; then it cancels.
    f.key(Key::Space);
    assert!(f.menu().is_some());
    assert_eq!(f.key(Key::Esc), Action::Stay);
    assert_eq!(f.menu(), None);
    assert_eq!(f.key(Key::Esc), Action::Close);

    // Space toggles the switch; Return on Cancel cancels.
    let mut f = form();
    f.set_focus(Field::WriteUp);
    f.key(Key::Space);
    assert!(f.write_up());
    f.set_focus(Field::Cancel);
    assert_eq!(f.key(Key::Enter), Action::Close);
    // Return in the Description is its new line, not a create.
    let mut f = form();
    f.set_title("T");
    f.set_focus(Field::Description);
    assert_eq!(f.key(Key::Enter), Action::Stay);
}

#[test]
fn a_picker_opened_by_click_closes_on_a_second_click_or_another_field() {
    let mut f = form();
    f.toggle_menu(Menu::Priority);
    assert_eq!(f.menu(), Some((Menu::Priority, 1)), "the cursor on End of queue");
    assert_eq!(f.focus(), Field::Priority);
    f.hover_menu(0);
    assert_eq!(f.menu(), Some((Menu::Priority, 0)));
    f.toggle_menu(Menu::Priority);
    assert_eq!(f.menu(), None);
    f.toggle_menu(Menu::Workflow);
    f.set_focus(Field::Title);
    assert_eq!(f.menu(), None);
    f.toggle_menu(Menu::Workflow);
    f.choose(Menu::Workflow, 2);
    assert_eq!(f.workflow().name, "triage");
}

/// Records what it was asked to send, and answers as configured.
struct Recorder {
    answer: Result<(), HandoffError>,
    sent: RefCell<Vec<(String, String)>>,
}

impl Recorder {
    fn answering(answer: Result<(), HandoffError>) -> Self {
        Recorder {
            answer,
            sent: RefCell::new(Vec::new()),
        }
    }
}

impl OrchestratorMessenger for Recorder {
    fn send(&self, project: &str, text: &str) -> Result<(), HandoffError> {
        self.sent.borrow_mut().push((project.to_string(), text.to_string()));
        self.answer.clone()
    }
}

fn request(write_up: bool) -> CreateRequest {
    let mut f = form();
    f.set_title("Write docs");
    if write_up {
        f.toggle_write_up();
    }
    created(f.key(Key::CmdEnter))
}

#[test]
fn create_runs_one_add_and_asks_the_orchestrator_only_when_switched_on() {
    let mut adds = Vec::new();
    let messenger = Recorder::answering(Ok(()));
    let out = create(
        "alpha",
        &request(false),
        |m| {
            adds.push(m.clone());
            Ok(Some("write-docs".into()))
        },
        &messenger,
    )
    .unwrap();
    assert_eq!(adds.len(), 1, "one mutation");
    assert!(matches!(&adds[0], Mutation::AddIssue { project, title, .. } if project == "alpha" && title == "Write docs"));
    assert_eq!(out.write_up, WriteUp::NotAsked);
    assert!(messenger.sent.borrow().is_empty(), "switch off: nothing sent");
    assert_eq!(out.message(), "Created write-docs");

    let out = create("alpha", &request(true), |_| Ok(Some("write-docs".into())), &messenger).unwrap();
    assert_eq!(out.write_up, WriteUp::Requested);
    let sent = messenger.sent.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "alpha");
    assert_eq!(sent[0].1, write_up_message("write-docs"));
    assert!(sent[0].1.contains("write-docs"), "the message names the new id");
    assert!(sent[0].1.contains("acceptance criteria") && sent[0].1.contains("task templates"));
    assert_eq!(out.message(), "Created write-docs; the Orchestrator is writing it up");
}

#[test]
fn the_task_is_created_even_when_the_orchestrator_isnt_running_and_the_user_is_told() {
    let messenger = Recorder::answering(Err(HandoffError::NotRunning));
    let out = create("alpha", &request(true), |_| Ok(Some("write-docs".into())), &messenger).unwrap();
    assert_eq!(out.id.as_deref(), Some("write-docs"));
    assert_eq!(out.write_up, WriteUp::NotRunning);
    assert!(out.message().contains("isn\u{2019}t running"), "{}", out.message());
    assert!(out.message().contains("wasn\u{2019}t requested"));

    let failing = Recorder::answering(Err(HandoffError::Failed("broken pipe".into())));
    let out = create("alpha", &request(true), |_| Ok(Some("w".into())), &failing).unwrap();
    assert_eq!(out.write_up, WriteUp::Failed("broken pipe".into()));

    // No id reported: nothing to name, so nothing is sent.
    let quiet = Recorder::answering(Ok(()));
    let out = create("alpha", &request(true), |_| Ok(None), &quiet).unwrap();
    assert!(matches!(out.write_up, WriteUp::Failed(_)));
    assert!(quiet.sent.borrow().is_empty());
    assert_eq!(out.message().split('.').next(), Some("Created \u{201c}Write docs\u{201d}"));
}

#[test]
fn a_failed_add_is_an_error_and_sends_nothing() {
    let messenger = Recorder::answering(Ok(()));
    let err = create(
        "alpha",
        &request(true),
        |_| Err(ExecError::Backend("daemon down".into())),
        &messenger,
    )
    .unwrap_err();
    assert!(err.contains("daemon down"), "{err}");
    assert!(messenger.sent.borrow().is_empty());
}
