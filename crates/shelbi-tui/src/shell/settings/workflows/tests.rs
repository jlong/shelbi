//! The TUI's Workflows page against real files in a throwaway
//! `SHELBI_HOME`: keys, modals, guards, the save queue and what it draws.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use shelbi_app::settings::workflows::{commit, fixture, load, Target};
use shelbi_core::StatusCategory;
use shelbi_state::workflow_config::WorkflowConfig;

use super::*;

const PROJECT: &str = "my-project";

/// A throwaway `SHELBI_HOME` with the fixture project, held for the test
/// (`ENV_LOCK`: `SHELBI_HOME` is process-wide).
pub(crate) struct Home {
    pub root: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Home {
    pub(crate) fn new(tag: &str) -> Home {
        let lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("shb-tui-wf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SHELBI_HOME", &root);
        fixture::write_project(&shelbi_state::projects_dir().unwrap(), PROJECT);
        Home { root, _lock: lock }
    }

    pub(crate) fn workflows(&self) -> PathBuf {
        shelbi_state::project_dir(PROJECT).unwrap().join("workflows")
    }

    pub(crate) fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.workflows().join(file)).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The page, loaded for the fixture project.
fn page() -> WorkflowsPage {
    let mut page = WorkflowsPage::default();
    let ticket = page.begin_load(Some(PROJECT), false).expect("a first read");
    assert!(page.loaded(&ticket, load(PROJECT)));
    page
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Press `k` with the built-in nav (arrows, Enter).
fn press(page: &mut WorkflowsPage, k: KeyEvent) -> SettingsAction {
    let nav = Nav {
        up: k.code == KeyCode::Up && k.modifiers.is_empty(),
        down: k.code == KeyCode::Down && k.modifiers.is_empty(),
        activate: k.code == KeyCode::Enter,
    };
    page.handle_key(k, nav)
}

fn typed(page: &mut WorkflowsPage, text: &str) {
    for c in text.chars() {
        press(page, key(KeyCode::Char(c)));
    }
}

/// Run every save the page queued, as the shell does off the UI thread.
/// Returns how many saved.
fn drain(page: &mut WorkflowsPage) -> usize {
    let mut saved = 0;
    while let Some((ticket, project, config, edit)) = page.begin_commit() {
        let done = commit(&project, &config, edit);
        if page.committed(&ticket, done) {
            saved += 1;
        }
    }
    saved
}

fn render(page: &mut WorkflowsPage, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| {
        let area = f.area();
        // As the shell draws a page: inside its gutter, a table's lines out
        // in it.
        let pad = crate::panel::PAD.min(area.width / 2);
        let content = Rect { x: area.x + pad, width: area.width - 2 * pad, ..area };
        let rows = page.rows(content.width as usize, true, true);
        for (i, row) in rows.iter().take(area.height as usize).enumerate() {
            let rect = Rect { y: i as u16, height: 1, ..content };
            let rect = if row.table_row.is_some() { super::super::bleed_rect(rect, area) } else { rect };
            f.render_widget(Paragraph::new(row.line.clone()), rect);
        }
        page.render_modal(f, area);
    })
    .unwrap();
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn status_ids() -> Vec<String> {
    WorkflowConfig::load(PROJECT).unwrap().statuses().iter().map(|s| s.id.clone()).collect()
}

/// The text in each field on `screen`: the line under each `▄` cap,
/// trimmed.
fn boxed(screen: &str) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    lines
        .windows(2)
        .filter(|w| w[0].trim_start().starts_with('▄'))
        .map(|w| w[1].trim().to_string())
        .collect()
}

#[test]
fn the_page_lists_workflows_statuses_and_the_legend() {
    let _home = Home::new("rows");
    let mut page = page();
    let text = render(&mut page, 110, 60);
    for want in [
        "Workflows",
        "task  Default",
        "Branch work with a review gate.",
        "subtask",
        "Statuses",
        "Transitions",
        "New workflow",
        "● To Do",
        "in-progress",
        "Handoff",
        "task, subtask",
        "New status",
        "Edit YAML",
        "Renaming a status keeps its ID",
        "Status categories",
        "Archived  Closed without shipping",
    ] {
        assert!(text.contains(want), "{want:?}:\n{text}");
    }
    // Counts: task has six statuses and four transitions.
    let task = text.lines().find(|l| l.contains("task  Default")).unwrap();
    assert!(task.contains("│ 6 ") && task.contains("│ 4 "), "{task}");
    // The cursor's row is the selection's pill, capped each end.
    assert!(task.starts_with('▐') && task.trim_end().ends_with('▌'), "{task}");
    assert_eq!(
        page.items(),
        [
            Item::NewWorkflow,
            Item::Workflow("task".into()),
            Item::Workflow("subtask".into()),
            Item::EditStatuses,
            Item::NewStatus,
            Item::Status("backlog".into()),
            Item::Status("todo".into()),
            Item::Status("in-progress".into()),
            Item::Status("review".into()),
            Item::Status("done".into()),
            Item::Status("canceled".into()),
        ]
    );
}

#[test]
fn every_row_is_reachable_by_keys_and_enter_does_its_main_thing() {
    let _home = Home::new("walk");
    let mut page = page();
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::EditWorkflow { name: "task".into(), status: None });
    let n = page.items().len();
    for _ in 0..n + 2 {
        press(&mut page, key(KeyCode::Down));
    }
    assert!(matches!(page.cursor_item(), Some(Item::Status(_))), "stops at the bottom, the last status");
    // The Statuses heading's Edit YAML button, above its table.
    let edit = page.items().iter().position(|i| *i == Item::EditStatuses).unwrap();
    while page.cursor_item() != Some(Item::EditStatuses) {
        press(&mut page, key(KeyCode::Up));
    }
    assert_eq!(page.items()[edit], Item::EditStatuses);
    match press(&mut page, key(KeyCode::Enter)) {
        SettingsAction::EditYaml(file) => {
            assert!(file.path.ends_with("workflows/statuses.yaml"), "{file:?}");
            assert_eq!(file.statuses_of.as_deref(), Some(PROJECT));
            assert_eq!(file.target, Target::Page);
        }
        other => panic!("{other:?}"),
    }
    // Home is the New workflow button; the workflows follow it.
    press(&mut page, key(KeyCode::Home));
    press(&mut page, key(KeyCode::Down));
    press(&mut page, key(KeyCode::Down));
    match press(&mut page, key(KeyCode::Char('y'))) {
        SettingsAction::EditYaml(file) => {
            assert!(file.path.ends_with("workflows/subtask.yaml"));
            assert_eq!(file.target, Target::Workflow("subtask".into()));
            assert_eq!(file.statuses_of, None);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(press(&mut page, key(KeyCode::Char('r'))), SettingsAction::Reload);
    press(&mut page, key(KeyCode::End));
    assert_eq!(page.cursor_item(), Some(Item::Status("canceled".into())));
}

#[test]
fn n_types_a_new_workflow_and_enter_creates_it() {
    let home = Home::new("create");
    let mut page = page();
    press(&mut page, key(KeyCode::Char('n')));
    assert_eq!(page.cursor_item(), Some(Item::NewWorkflow));
    assert!(page.modal_open());
    typed(&mut page, "hotfix");
    let text = render(&mut page, 100, 40);
    assert!(boxed(&text).iter().any(|t| t == "hotfix"), "{text}");
    assert!(text.contains("Enter to create, Esc to cancel."), "{text}");
    // Digits and letters type, they don't run actions.
    typed(&mut page, "2");
    press(&mut page, key(KeyCode::Backspace));
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::Commit);
    assert_eq!(drain(&mut page), 1);
    assert!(home.workflows().join("hotfix.yaml").exists());
    assert!(!page.modal_open(), "the new row closes once it's created");
    assert_eq!(page.cursor_item(), Some(Item::Workflow("hotfix".into())), "on the new workflow");
}

#[test]
fn a_taken_name_keeps_the_new_row_open_with_the_reason() {
    let _home = Home::new("taken");
    let mut page = page();
    press(&mut page, key(KeyCode::Char('n')));
    typed(&mut page, "task");
    press(&mut page, key(KeyCode::Enter));
    assert_eq!(drain(&mut page), 0);
    assert!(page.modal_open(), "still typing");
    let text = render(&mut page, 100, 40);
    assert!(text.contains("A workflow named `task` already exists"), "{text}");
    press(&mut page, key(KeyCode::Esc));
    assert!(!page.modal_open());
    assert!(page.state().unwrap().draft.is_none());
}

#[test]
fn duplicate_offers_a_free_name_and_set_default_moves_the_badge() {
    let home = Home::new("dup");
    let mut page = page();
    press(&mut page, key(KeyCode::Char('c')));
    let text = render(&mut page, 100, 40);
    assert!(boxed(&text).iter().any(|t| t == "task-copy"), "{text}");
    assert!(text.contains("Duplicate of task."), "{text}");
    press(&mut page, key(KeyCode::Enter));
    drain(&mut page);
    assert!(home.read("task-copy.yaml").contains("## Statuses run left to right."));

    page.select(&Item::Workflow("subtask".into()));
    assert_eq!(press(&mut page, key(KeyCode::Char('s'))), SettingsAction::Commit);
    drain(&mut page);
    assert_eq!(WorkflowConfig::load(PROJECT).unwrap().default_workflow(), "subtask");
    assert_eq!(page.cursor_item(), Some(Item::Workflow("subtask".into())), "follows it to the top");
    assert!(render(&mut page, 100, 40).contains("subtask  Default"));
}

#[test]
fn delete_says_why_it_cant_or_asks_first() {
    let home = Home::new("delete-wf");
    let mut page = page();
    // The default: refused up front, with the reason at its row.
    assert_eq!(press(&mut page, key(KeyCode::Char('x'))), SettingsAction::Stay);
    assert!(!page.modal_open());
    let text = render(&mut page, 100, 40);
    assert!(text.contains("The default workflow can’t be deleted."), "{text}");

    page.select(&Item::Workflow("subtask".into()));
    press(&mut page, key(KeyCode::Delete));
    let text = render(&mut page, 100, 40);
    assert!(text.contains("Delete workflow"), "{text}");
    assert!(text.contains("workflows/subtask.yaml"), "{text}");
    // Enter alone presses Cancel.
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::Stay);
    assert!(!page.modal_open());
    assert!(home.workflows().join("subtask.yaml").exists());
    press(&mut page, key(KeyCode::Char('x')));
    press(&mut page, key(KeyCode::Left));
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::Commit);
    drain(&mut page);
    assert!(!home.workflows().join("subtask.yaml").exists());
}

#[test]
fn a_workflow_open_issues_use_isnt_deleted_and_the_row_says_which() {
    let home = Home::new("in-use");
    let at = chrono::Utc::now();
    let issue = shelbi_core::Issue {
        id: "sub-1".into(),
        title: "sub".into(),
        column: shelbi_core::Column::todo(),
        priority: 1,
        assigned_to: None,
        workflow: Some("subtask".into()),
        branch: None,
        depends_on: vec![],
        prefers_machine: None,
        zen: None,
        launch: None,
        created_at: at,
        updated_at: at,
        params: Default::default(),
    };
    shelbi_state::save_task(PROJECT, &issue, "").unwrap();
    let mut page = page();
    page.select(&Item::Workflow("subtask".into()));
    press(&mut page, key(KeyCode::Char('x')));
    press(&mut page, key(KeyCode::Char('y')));
    assert_eq!(drain(&mut page), 0);
    let text = render(&mut page, 120, 40);
    assert!(text.contains("used by 1 open issue: sub-1"), "{text}");
    assert!(home.workflows().join("subtask.yaml").exists());
}

#[test]
fn a_status_renames_in_place_and_keeps_its_id() {
    let home = Home::new("rename");
    let mut page = page();
    page.select(&Item::Status("todo".into()));
    press(&mut page, key(KeyCode::Enter));
    press(&mut page, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    typed(&mut page, "Up next");
    let text = render(&mut page, 100, 40);
    assert!(boxed(&text).iter().any(|t| t == "Up next"), "typed in a field under its row:\n{text}");
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::Commit);
    drain(&mut page);
    assert!(home.read("statuses.yaml").contains("- id: todo\n  name: Up next\n"));
    assert!(home.read("statuses.yaml").starts_with("## The project's statuses."), "comments stay");
    // Esc cancels a rename; an unchanged name saves nothing.
    press(&mut page, key(KeyCode::Char('e')));
    typed(&mut page, "!!");
    press(&mut page, key(KeyCode::Esc));
    press(&mut page, key(KeyCode::Enter));
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::Stay);
    assert_eq!(WorkflowConfig::load(PROJECT).unwrap().status("todo").unwrap().name, "Up next");
}

#[test]
fn the_category_picker_lists_the_categories_as_text() {
    let home = Home::new("category");
    let mut page = page();
    page.select(&Item::Status("review".into()));
    press(&mut page, key(KeyCode::Char('c')));
    let text = render(&mut page, 100, 40);
    for want in [" Category ", "Backlog", "Handoff", "current", "Archived"] {
        assert!(text.contains(want), "{want:?}:\n{text}");
    }
    press(&mut page, key(KeyCode::Esc));
    assert!(!page.modal_open(), "Esc cancels");
    press(&mut page, key(KeyCode::Char('c')));
    press(&mut page, key(KeyCode::Up));
    assert!(render(&mut page, 100, 40).contains("Work is in progress."), "the picked one's meaning shows");
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::Commit);
    drain(&mut page);
    assert_eq!(WorkflowConfig::load(PROJECT).unwrap().status("review").unwrap().category, StatusCategory::Active);
    assert!(home.read("statuses.yaml").contains("- id: review\n  name: Review\n  category: active"));
}

#[test]
fn shift_and_alt_arrows_reorder_statuses_and_the_cursor_follows() {
    let _home = Home::new("reorder");
    let mut page = page();
    page.select(&Item::Status("review".into()));
    assert_eq!(press(&mut page, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)), SettingsAction::Commit);
    drain(&mut page);
    assert_eq!(status_ids(), ["backlog", "todo", "review", "in-progress", "done", "canceled"]);
    assert_eq!(page.cursor_item(), Some(Item::Status("review".into())));
    press(&mut page, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
    press(&mut page, KeyEvent::new(KeyCode::Down, KeyModifiers::ALT));
    drain(&mut page);
    assert_eq!(status_ids(), ["backlog", "todo", "in-progress", "done", "review", "canceled"]);
    // The menu offers the same moves, for a terminal that sends no Shift.
    page.select(&Item::Status("backlog".into()));
    press(&mut page, key(KeyCode::Char('m')));
    let text = render(&mut page, 100, 40);
    assert!(text.contains("Move down") && !text.contains("Move up"), "the first can't move up:\n{text}");
    press(&mut page, key(KeyCode::Esc));
    assert_eq!(press(&mut page, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)), SettingsAction::Stay);
}

#[test]
fn a_new_status_shows_its_id_cycles_its_category_and_lands_selected() {
    let home = Home::new("new-status");
    let mut page = page();
    page.select(&Item::Status("done".into()));
    press(&mut page, key(KeyCode::Char('n')));
    assert_eq!(page.cursor_item(), Some(Item::NewStatus));
    typed(&mut page, "QA check");
    press(&mut page, key(KeyCode::Tab));
    press(&mut page, key(KeyCode::Tab));
    press(&mut page, key(KeyCode::Tab));
    press(&mut page, key(KeyCode::BackTab));
    let text = render(&mut page, 100, 40);
    assert!(text.contains("id qa-check"), "{text}");
    assert!(text.contains("category Active"), "{text}");
    press(&mut page, key(KeyCode::Enter));
    drain(&mut page);
    let back = WorkflowConfig::load(PROJECT).unwrap();
    assert_eq!(back.status("qa-check").unwrap().category, StatusCategory::Active);
    assert!(home.read("statuses.yaml").contains("name: QA check"));
    assert_eq!(page.cursor_item(), Some(Item::Status("qa-check".into())));
}

#[test]
fn a_status_in_use_isnt_deleted_and_an_unused_one_is_after_confirming() {
    let _home = Home::new("delete-status");
    let mut page = page();
    page.select(&Item::Status("review".into()));
    press(&mut page, key(KeyCode::Char('x')));
    assert!(!page.modal_open());
    assert!(render(&mut page, 100, 40).contains("Used in task. Remove it from them first."));
    // The menu shows Delete dimmed, and choosing it says the same.
    press(&mut page, key(KeyCode::Char('m')));
    press(&mut page, key(KeyCode::Char('x')));
    assert!(!page.modal_open());

    page.select(&Item::NewStatus);
    press(&mut page, key(KeyCode::Enter));
    typed(&mut page, "Spare");
    press(&mut page, key(KeyCode::Enter));
    drain(&mut page);
    assert_eq!(page.cursor_item(), Some(Item::Status("spare".into())));
    press(&mut page, key(KeyCode::Char('x')));
    let text = render(&mut page, 100, 40);
    assert!(text.contains("Delete status Spare (spare)?"), "{text}");
    press(&mut page, key(KeyCode::Char('y')));
    drain(&mut page);
    assert!(!status_ids().contains(&"spare".to_string()));
}

#[test]
fn the_menu_lists_every_action_with_its_key_and_runs_them() {
    let _home = Home::new("menu");
    let mut page = page();
    page.select(&Item::Workflow("subtask".into()));
    press(&mut page, key(KeyCode::Char('m')));
    let text = render(&mut page, 100, 40);
    for want in ["Edit", "Edit YAML", "Duplicate", "Set as default", "Delete", "New workflow", "⏎ e"] {
        assert!(text.contains(want), "{want:?}:\n{text}");
    }
    // Down to Edit YAML, Enter.
    press(&mut page, key(KeyCode::Down));
    assert!(matches!(press(&mut page, key(KeyCode::Enter)), SettingsAction::EditYaml(_)));
    assert!(!page.modal_open());
    // An entry's own key runs it from the menu.
    press(&mut page, key(KeyCode::Char('.')));
    assert_eq!(press(&mut page, key(KeyCode::Char('s'))), SettingsAction::Commit);
    // The default has no Set as default.
    drain(&mut page);
    press(&mut page, key(KeyCode::Char('m')));
    assert!(!render(&mut page, 100, 40).contains("Set as default"));
}

#[test]
fn edits_made_while_one_saves_queue_and_all_land() {
    let _home = Home::new("queue");
    let mut page = page();
    page.select(&Item::Status("todo".into()));
    press(&mut page, key(KeyCode::Char('e')));
    typed(&mut page, "!");
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::Commit);
    let first = page.begin_commit().expect("the first save starts");
    // Two more while it runs: they wait.
    page.select(&Item::Status("done".into()));
    press(&mut page, key(KeyCode::Char('e')));
    typed(&mut page, "!");
    assert_eq!(press(&mut page, key(KeyCode::Enter)), SettingsAction::Stay, "queued");
    press(&mut page, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    assert!(page.begin_commit().is_none(), "one save at a time");
    assert!(render(&mut page, 100, 40).contains("saving… (2 more queued)"));
    let (ticket, project, config, edit) = first;
    assert!(page.committed(&ticket, commit(&project, &config, edit)));
    assert_eq!(drain(&mut page), 2);
    let back = WorkflowConfig::load(PROJECT).unwrap();
    assert_eq!(back.status("todo").unwrap().name, "To Do!");
    assert_eq!(back.status("done").unwrap().name, "Done!");
    assert_eq!(status_ids()[3..5], ["done".to_string(), "review".to_string()]);
}

#[test]
fn a_save_for_a_project_no_longer_shown_is_dropped() {
    let home = Home::new("stale");
    let mut page = page();
    press(&mut page, key(KeyCode::Char('s')));
    page.select(&Item::Workflow("subtask".into()));
    press(&mut page, key(KeyCode::Char('s')));
    let (ticket, project, config, edit) = page.begin_commit().unwrap();
    // The user switches project while it saves.
    page.project_changed();
    let ticket2 = page.begin_load(Some("beta"), false).unwrap();
    let done = commit(&project, &config, edit);
    assert!(!page.committed(&ticket, done), "dropped");
    assert_eq!(page.dropped(), 1);
    assert!(page.begin_commit().is_none(), "nothing of the old project's queue runs");
    // The old project's late read is dropped too.
    assert!(!page.loaded(&ticket, load(PROJECT)));
    assert!(page.state().unwrap().project() == "beta");
    let _ = (ticket2, home);
}

#[test]
fn a_file_changed_on_disk_shows_the_conflict_at_the_row_and_the_hand_edit_stands() {
    let home = Home::new("conflict");
    let mut page = page();
    let edited = fixture::STATUSES.replace("name: Done", "name: Shipped");
    std::fs::write(home.workflows().join("statuses.yaml"), &edited).unwrap();
    page.select(&Item::Status("todo".into()));
    press(&mut page, key(KeyCode::Char('e')));
    typed(&mut page, "!");
    press(&mut page, key(KeyCode::Enter));
    assert_eq!(drain(&mut page), 0);
    let text = render(&mut page, 120, 40);
    assert!(text.contains("Changed on disk since it was loaded"), "{text}");
    assert!(text.contains("● Shipped"), "the page shows the files as they are:\n{text}");
    assert_eq!(home.read("statuses.yaml"), edited);
}

#[test]
fn an_unreadable_workflow_is_listed_with_its_error_and_opens_its_yaml() {
    let home = Home::new("unreadable");
    let broken = "name: broken\nstatuses: [oops\n";
    std::fs::write(home.workflows().join("broken.yaml"), broken).unwrap();
    let mut page = page();
    let text = render(&mut page, 120, 40);
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| l.contains("⚠ broken")).expect(&text);
    assert!(lines[at + 1].contains("Can’t be edited here:"), "its error under its name:\n{text}");
    page.select(&Item::Unreadable("broken".into()));
    match press(&mut page, key(KeyCode::Enter)) {
        SettingsAction::EditYaml(file) => assert!(file.path.ends_with("broken.yaml")),
        other => panic!("{other:?}"),
    }
    // Other edits still save; the broken file is never written.
    page.select(&Item::Status("todo".into()));
    press(&mut page, key(KeyCode::Char('e')));
    typed(&mut page, "!");
    press(&mut page, key(KeyCode::Enter));
    assert_eq!(drain(&mut page), 1);
    assert_eq!(home.read("broken.yaml"), broken);
}

#[test]
fn a_save_that_drops_comments_says_so() {
    let home = Home::new("comments");
    let text = format!("{}- id: spare\n  # Keep for Q3.\n  name: Spare\n  category: backlog\n", fixture::STATUSES);
    std::fs::write(home.workflows().join("statuses.yaml"), text).unwrap();
    let mut page = page();
    page.select(&Item::Status("spare".into()));
    press(&mut page, key(KeyCode::Char('x')));
    press(&mut page, key(KeyCode::Char('y')));
    drain(&mut page);
    let screen = render(&mut page, 120, 40);
    assert!(screen.contains("The save dropped these comments: statuses.yaml: # Keep for Q3."), "{screen}");
}

#[test]
fn a_paste_types_into_the_name_and_nowhere_else() {
    let _home = Home::new("paste");
    let mut page = page();
    page.paste("ignored");
    assert!(!page.modal_open());
    press(&mut page, key(KeyCode::Char('n')));
    page.paste("from\nclipboard");
    assert!(boxed(&render(&mut page, 100, 40)).iter().any(|t| t == "fromclipboard"));
}

#[test]
fn a_failed_read_says_why_and_no_project_says_so() {
    let mut page = WorkflowsPage::default();
    let ticket = page.begin_load(Some("gone"), false).unwrap();
    page.loaded(&ticket, Err("no such project".into()));
    let text = render(&mut page, 80, 20);
    assert!(text.contains("Couldn't read the workflows: no such project"), "{text}");
    assert!(text.contains("Press r to try again."), "{text}");
    assert!(page.items().is_empty());
    assert!(page.begin_load(None, false).is_none());
    let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
    term.draw(|f| {
        let rows = page.rows(80, true, false);
        let text: String = rows.iter().map(|r| r.line.to_string()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("No project is open."), "{text}");
        let _ = f;
    })
    .unwrap();
}

#[test]
fn narrow_and_tiny_widths_draw_without_panicking() {
    let _home = Home::new("narrow");
    let mut page = page();
    let text = render(&mut page, 52, 60);
    assert!(text.contains("task  Default"), "{text}");
    assert!(text.contains("● To Do"), "{text}");
    page.select(&Item::Status("todo".into()));
    press(&mut page, key(KeyCode::Char('m')));
    for (w, h) in [(1, 1), (8, 3), (20, 6), (40, 12), (52, 60), (200, 80)] {
        render(&mut page, w, h);
    }
    press(&mut page, key(KeyCode::Esc));
    press(&mut page, key(KeyCode::Char('x')));
    for (w, h) in [(1, 1), (14, 4), (30, 8)] {
        render(&mut page, w, h);
    }
}

#[test]
fn the_page_and_its_modals_read_in_light() {
    use shelbi_app::theme::Appearance;
    let _home = Home::new("light");
    let mut page = page();
    page.select(&Item::Status("review".into()));
    crate::theme::with_appearance(Appearance::Light, || {
        for open in [None, Some('m'), Some('c')] {
            if let Some(c) = open {
                press(&mut page, key(KeyCode::Char(c)));
            }
            let mut term = Terminal::new(TestBackend::new(110, 60)).unwrap();
            term.draw(|f| {
                let area = f.area();
                let rows = page.rows(area.width as usize, true, true);
                for (i, row) in rows.iter().take(area.height as usize).enumerate() {
                    f.render_widget(Paragraph::new(row.line.clone()), Rect { y: i as u16, height: 1, ..area });
                }
                page.render_modal(f, area);
            })
            .unwrap();
            crate::theme::assert_light_readable("settings-workflows", term.backend().buffer());
            press(&mut page, key(KeyCode::Esc));
        }
    });
}
