//! New task in the window (`new-task-dialog`, Figma `2730:10845`): the
//! palette's New Issue, File ▸ New Issue…, the board's New issue and `n`
//! open it over the window; its layout follows the frame; Create Task makes
//! one create through the runner (a recorder here: nothing reaches the
//! daemon or a session), the window flashes its outcome, the orchestrator
//! hand-off's included; Esc, Cancel and ⌘W discard.
//!
//! The window is the smoke test's headless one ([`Harness`]).

use std::sync::{Arc, Mutex};

use gpui::TestAppContext;
use shelbi_app::new_task::{CreateRequest, Created, Field, WriteUp};

use super::smoke::Harness;
use super::*;
use crate::menus::tree::{AppCommand, MenuAction};
use crate::views::new_task::NewTaskGlobal;

struct NewTask {
    h: Harness,
    seen: Arc<Mutex<Vec<CreateRequest>>>,
}

impl NewTask {
    /// The window, its runner recording each create and answering with
    /// `write_up` for one that asked for the write-up.
    fn open(cx: &mut TestAppContext, write_up: WriteUp) -> NewTask {
        let mut h = Harness::open(cx);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let runner = Arc::new(move |_: &shelbi_app::source::ProjectLink, request: &CreateRequest| {
            record.lock().unwrap().push(request.clone());
            Ok(Created {
                id: Some("write-docs".into()),
                title: request.title().to_string(),
                write_up: if request.write_up { write_up.clone() } else { WriteUp::NotAsked },
            })
        });
        h.cx.update(|_, cx| cx.set_global(NewTaskGlobal(runner)));
        h.focus_window();
        NewTask { h, seen }
    }

    fn dialog(&self) -> Option<Entity<NewTaskDialog>> {
        self.h.window.read_with(&self.h.cx, |w, _| w.new_task())
    }

    fn read<R>(&self, f: impl FnOnce(&NewTaskDialog) -> R) -> R {
        let dialog = self.dialog().expect("New task is open");
        self.h.cx.read(|cx| f(dialog.read(cx)))
    }

    /// Run the registry's New Issue from the menu (as the palette does).
    fn open_dialog(&mut self) -> Entity<NewTaskDialog> {
        self.h.cx.dispatch_action(menus::MenuInvoke {
            target: MenuAction::Command("action:new-issue".into()),
            generation: 0,
        });
        self.h.cx.run_until_parked();
        let calls = self.h.take_calls();
        assert_eq!(calls, [HostCall::NewIssue], "File ▸ New Issue… runs the registry's New Issue");
        self.h.perform(calls);
        self.h.draw();
        self.dialog().expect("New task is open")
    }

    fn type_title(&mut self, text: &str) {
        let (title, _) = self.read(|d| d.inputs());
        self.h.cx.update(|window, cx| {
            let focus = gpui::Focusable::focus_handle(title.read(cx), cx);
            window.focus(&focus, cx);
            title.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        });
        self.h.cx.simulate_input(text);
        self.h.draw();
    }

    fn type_description(&mut self, text: &str) {
        let (_, description) = self.read(|d| d.inputs());
        self.h.cx.update(|window, cx| {
            let focus = gpui::Focusable::focus_handle(description.read(cx), cx);
            window.focus(&focus, cx);
            description.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        });
        self.h.cx.simulate_input(text);
        self.h.draw();
    }

    fn seen(&self) -> Vec<CreateRequest> {
        self.seen.lock().unwrap().clone()
    }

    fn flashed(&self) -> Option<String> {
        self.h.window.read_with(&self.h.cx, |w, _| w.status.as_ref().map(|s| s.to_string()))
    }
}

#[gpui::test]
fn the_dialog_lays_out_the_frame(cx: &mut TestAppContext) {
    let mut t = NewTask::open(cx, WriteUp::NotAsked);
    t.open_dialog();
    let mut bounds = |sel: &'static str| t.h.cx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel} is drawn"));
    let panel = bounds("new-task-dialog");
    assert_eq!(panel.size.width, px(680.0), "the frame's 680pt panel");
    let title = bounds("new-task-title-field");
    assert_eq!(title.size.height, px(40.0), "the 40pt title field");
    assert_eq!(title.left() - panel.left(), px(33.0), "32pt in, past the 1pt edge");
    assert_eq!(title.size.width, px(680.0 - 66.0), "the content's 616pt");
    // Workflow, Status and Priority share the row, 12pt apart, equal widths.
    let w = bounds("new-task-workflow");
    let s = bounds("new-task-status");
    let p = bounds("new-task-priority");
    assert!(w.top() == s.top() && s.top() == p.top());
    assert_eq!(s.left() - w.right(), px(12.0));
    assert_eq!(p.left() - s.right(), px(12.0));
    assert!((w.size.width - s.size.width).abs() < px(1.0) && (s.size.width - p.size.width).abs() < px(1.0));
    assert_eq!(w.size.height, px(32.0), "Base buttons");
    assert!(w.top() > title.bottom());
    let description = bounds("new-task-description");
    assert_eq!(description.size.height, px(152.0));
    assert!(description.top() > w.bottom());
    let write_up = bounds("new-task-write-up");
    assert!(write_up.top() > description.bottom());
    let switch = bounds("switch-off");
    assert_eq!((switch.size.width, switch.size.height), (px(36.0), px(22.0)), "the Switch, off");
    assert_eq!(write_up.right() - switch.right(), px(16.0), "16pt in from the card's edge");
    let cancel = bounds("new-task-cancel");
    let create = bounds("new-task-create");
    assert_eq!(cancel.size.height, px(40.0), "Large buttons");
    assert_eq!(create.left() - cancel.right(), px(12.0));
    assert_eq!(create.right(), title.right(), "flush with the content's right edge");
    assert!(cancel.top() > write_up.bottom());
    assert!(t.h.cx.debug_bounds("new-task-hint").is_some(), "⌘ Return to create");
    // Defaults: the keyboard in Title, the end of the queue, the switch off.
    t.read(|d| {
        let f = d.form();
        assert_eq!(f.focus(), Field::Title);
        assert_eq!(f.position(), shelbi_app::new_task::Position::End);
        assert!(!f.write_up());
        assert!(!f.can_create(), "no title: Create Task is disabled");
    });
}

#[gpui::test]
fn esc_cancel_and_cmd_w_discard_it(cx: &mut TestAppContext) {
    let mut t = NewTask::open(cx, WriteUp::NotAsked);
    t.open_dialog();
    t.type_title("Write docs");
    t.h.key("escape");
    assert!(t.dialog().is_none(), "Esc cancels");

    t.open_dialog();
    t.h.click_selector("new-task-cancel");
    assert!(t.dialog().is_none(), "Cancel closes it");

    t.open_dialog();
    t.h.cx.dispatch_action(menus::MenuInvoke {
        target: MenuAction::App(AppCommand::CloseWindow),
        generation: 0,
    });
    t.h.cx.run_until_parked();
    assert!(t.dialog().is_none(), "⌘W cancels the dialog, not the window");
    assert!(t.seen().is_empty(), "nothing was created");
}

#[gpui::test]
fn create_task_makes_one_create_and_the_window_flashes_it(cx: &mut TestAppContext) {
    let mut t = NewTask::open(cx, WriteUp::NotAsked);
    t.open_dialog();
    // Disabled while the title is blank.
    t.h.click_selector("new-task-create");
    assert!(t.seen().is_empty());
    assert!(t.dialog().is_some());

    t.type_title("  Write docs ");
    t.type_description("Line one\n\nLine two");
    t.h.key("cmd-enter");
    t.h.cx.run_until_parked();
    let seen = t.seen();
    assert_eq!(seen.len(), 1, "one create");
    assert_eq!(
        seen[0].command,
        CommandKind::AddIssue {
            title: "Write docs".into(),
            body: Some("Line one\n\nLine two".into()),
            workflow: None,
            status: seen[0].command_status(),
            top: false,
        }
    );
    assert!(!seen[0].write_up);
    assert!(t.dialog().is_none(), "created: it closes");
    assert_eq!(t.flashed().as_deref(), Some("Created write-docs"));
}

#[gpui::test]
fn the_write_up_switch_asks_the_orchestrator_and_says_when_it_isnt_running(cx: &mut TestAppContext) {
    let mut t = NewTask::open(cx, WriteUp::NotRunning);
    t.open_dialog();
    t.type_title("Write docs");
    t.h.click_selector("new-task-write-up");
    assert!(t.read(|d| d.form().write_up()), "a click on the card toggles it");
    t.h.draw();
    assert!(t.h.cx.debug_bounds("switch-on").is_some(), "the Switch shows on");
    t.h.click_selector("new-task-create");
    t.h.cx.run_until_parked();
    let seen = t.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].write_up, "the create asks for the write-up");
    let flashed = t.flashed().unwrap_or_default();
    assert!(flashed.starts_with("Created write-docs."), "{flashed}");
    assert!(flashed.contains("isn\u{2019}t running") && flashed.contains("wasn\u{2019}t requested"), "{flashed}");
}

#[gpui::test]
fn the_pickers_open_from_the_keyboard_and_pick(cx: &mut TestAppContext) {
    let mut t = NewTask::open(cx, WriteUp::NotAsked);
    t.open_dialog();
    // Tab, Tab, Tab to Priority; Return opens it; ↑ Return picks the top.
    for _ in 0..3 {
        t.h.key("tab");
    }
    assert_eq!(t.read(|d| d.form().focus()), Field::Priority);
    t.h.key("enter");
    t.h.draw();
    assert!(t.h.cx.debug_bounds("new-task-priority-menu").is_some(), "the menu opens under it");
    t.h.key("up");
    t.h.key("enter");
    t.h.draw();
    assert!(t.h.cx.debug_bounds("new-task-priority-menu").is_none(), "picked: it closes");
    assert_eq!(t.read(|d| d.form().position()), shelbi_app::new_task::Position::Top);
    // Esc closes an open menu, not the dialog.
    t.h.key("enter");
    t.h.key("escape");
    assert!(t.dialog().is_some());
    assert_eq!(t.read(|d| d.form().menu()), None);
}

#[gpui::test]
fn the_pickers_open_and_pick_with_the_mouse(cx: &mut TestAppContext) {
    let mut t = NewTask::open(cx, WriteUp::NotAsked);
    t.open_dialog();
    // The Title has the keyboard; a click on Priority opens its menu and
    // keeps it open (the keyboard moves to the menu).
    t.h.click_selector("new-task-priority");
    t.h.draw();
    assert_eq!(t.read(|d| d.form().menu().map(|(m, _)| m)), Some(shelbi_app::new_task::Menu::Priority));
    assert!(t.h.cx.debug_bounds("new-task-priority-menu").is_some(), "the menu stays open");
    t.h.click_selector("new-task-priority-0");
    assert_eq!(t.read(|d| d.form().position()), shelbi_app::new_task::Position::Top);
    assert_eq!(t.read(|d| d.form().menu()), None);
    // A click back into Title takes the keyboard there.
    t.h.click_selector("new-task-title-field");
    t.h.draw();
    assert_eq!(t.read(|d| d.form().focus()), Field::Title);
}

#[gpui::test]
fn the_boards_new_issue_and_n_run_the_registrys_new_issue(cx: &mut TestAppContext) {
    let mut t = NewTask::open(cx, WriteUp::NotAsked);
    t.h.perform(vec![HostCall::ShowView(View::Issues)]);
    t.h.draw();
    t.h.click_selector("new-issue");
    t.h.cx.run_until_parked();
    assert_eq!(t.h.take_calls(), [HostCall::NewIssue], "the toolbar's New issue");
    t.h.key("n");
    t.h.cx.run_until_parked();
    assert_eq!(t.h.take_calls(), [HostCall::NewIssue], "the board's `n`");
}

trait CommandStatus {
    fn command_status(&self) -> Option<String>;
}

impl CommandStatus for CreateRequest {
    /// The status the create carries (the default workflow's initial one,
    /// whatever the sandbox's workflows are).
    fn command_status(&self) -> Option<String> {
        match &self.command {
            CommandKind::AddIssue { status, .. } => status.clone(),
            _ => None,
        }
    }
}
