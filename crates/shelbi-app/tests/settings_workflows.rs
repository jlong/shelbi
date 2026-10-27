//! `shelbi_app::settings::workflows` against real files: the Workflows
//! page's rows, its edits through `shelbi_state::workflow_config`, the
//! guards, the save queue, and what a failure leaves showing. Each test runs
//! in its own throwaway `SHELBI_HOME`, one at a time.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use shelbi_app::settings::workflows::fixture::{self, STATUSES};
use shelbi_app::settings::workflows::{commit, Draft, Edit, Target, WorkflowsState};
use shelbi_core::StatusCategory;
use shelbi_state::workflow_config::{WorkflowConfig, WorkflowSummary};

const PROJECT: &str = "my-project";

/// Serializes the tests: `SHELBI_HOME` is process-wide.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A throwaway `SHELBI_HOME` holding the fixture project, set for the
/// test's length.
struct Home {
    root: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

impl Home {
    fn new(tag: &str) -> Home {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("shb-app-wf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("SHELBI_HOME", &root);
        fixture::write_project(&shelbi_state::projects_dir().unwrap(), PROJECT);
        Home { root, _lock: lock }
    }

    fn workflows(&self) -> PathBuf {
        shelbi_state::project_dir(PROJECT).unwrap().join("workflows")
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.workflows().join(file)).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn loaded() -> WorkflowsState {
    let mut state = WorkflowsState::new(PROJECT);
    assert!(state.loaded(shelbi_app::settings::workflows::load(PROJECT)));
    state
}

/// Make `edit` the way a page does: request it, commit it (here, rather
/// than off the UI thread), and take the result.
fn make(state: &mut WorkflowsState, edit: Edit) {
    let mut job = state.request(edit);
    while let Some((config, edit)) = job {
        let done = commit(PROJECT, &config, edit);
        job = state.committed(done);
    }
}

/// Issue `id` on `workflow`, in `column`.
fn save_issue(id: &str, workflow: &str, column: shelbi_core::Column) {
    let at = chrono::Utc::now();
    let issue = shelbi_core::Issue {
        id: id.into(),
        title: id.into(),
        column,
        priority: 1,
        assigned_to: None,
        workflow: Some(workflow.into()),
        branch: None,
        depends_on: vec![],
        prefers_machine: None,
        zen: None,
        launch: None,
        created_at: at,
        updated_at: at,
        params: Default::default(),
    };
    shelbi_state::save_task(PROJECT, &issue, "").expect("save an issue");
}

#[test]
fn the_tables_show_the_projects_workflows_and_statuses() {
    let _home = Home::new("rows");
    let state = loaded();
    let rows = state.workflows();
    let summary = |r: &WorkflowSummary| (r.name.clone(), r.statuses, r.transitions, r.is_default);
    assert_eq!(
        rows.iter().map(summary).collect::<Vec<_>>(),
        [("task".to_string(), 6, 4, true), ("subtask".to_string(), 5, 2, false)],
        "the default first"
    );
    assert_eq!(rows[0].description.as_deref(), Some("Branch work with a review gate."));
    let statuses = state.statuses();
    assert_eq!(statuses.len(), 6);
    assert_eq!((statuses[1].id.as_str(), statuses[1].name.as_str()), ("todo", "To Do"));
    assert_eq!(statuses[3].used_in, ["task"]);
    assert_eq!(statuses[0].used_in, ["task", "subtask"], "in the table's order");
    assert!(state.unreadable().is_empty());
}

#[test]
fn renaming_a_status_keeps_its_id_and_the_files_comments() {
    let home = Home::new("rename");
    let mut state = loaded();
    make(&mut state, Edit::RenameStatus { id: "todo".into(), name: "Ready to go".into() });
    assert_eq!(state.error_at(&Target::Status("todo".into())), None);
    let text = home.read("statuses.yaml");
    assert!(text.starts_with("## The project's statuses.\n"), "{text}");
    assert!(text.contains("- id: todo\n  name: Ready to go\n"), "{text}");
    assert_eq!(state.statuses()[1].name, "Ready to go");
    assert_eq!(state.statuses()[1].id, "todo");
    assert!(home.read("task.yaml").contains("- id: todo"), "workflows keep the id");
}

#[test]
fn category_reorder_and_new_status_persist() {
    let home = Home::new("status-edits");
    let mut state = loaded();
    make(&mut state, Edit::SetCategory { id: "review".into(), category: StatusCategory::Active });
    let edit = state.drop_status("canceled", "backlog").expect("a move");
    assert_eq!(edit, Edit::MoveStatus { id: "canceled".into(), index: 0 });
    make(&mut state, edit);
    assert_eq!(state.new_status_id("QA check").as_deref(), Some("qa-check"));
    assert_eq!(state.new_status_id("Backlog").as_deref(), Some("backlog-2"), "ids stay unique");
    assert_eq!(state.new_status_id("  "), None);
    make(&mut state, Edit::AddStatus { name: "QA check".into(), category: StatusCategory::Handoff });
    let ids: Vec<_> = state.statuses().into_iter().map(|s| s.id).collect();
    assert_eq!(ids, ["canceled", "backlog", "todo", "in-progress", "review", "done", "qa-check"]);
    let back = WorkflowConfig::load(PROJECT).unwrap();
    assert_eq!(back.status("review").unwrap().category, StatusCategory::Active);
    assert_eq!(back.status("qa-check").unwrap().name, "QA check");
    assert!(home.read("statuses.yaml").contains("category: handoff"));
    assert_eq!(state.drop_status("todo", "todo"), None, "dropped on itself");
}

#[test]
fn the_keys_move_a_status_one_row_and_stop_at_the_ends() {
    let _home = Home::new("move-by");
    let mut state = loaded();
    assert_eq!(state.move_status_by("backlog", false), None, "already first");
    assert_eq!(state.move_status_by("canceled", true), None, "already last");
    let edit = state.move_status_by("review", false).unwrap();
    assert_eq!(edit, Edit::StepStatus { id: "review".into(), down: false });
    make(&mut state, edit);
    let down = state.move_status_by("backlog", true).unwrap();
    make(&mut state, down);
    let ids: Vec<_> = state.statuses().into_iter().map(|s| s.id).collect();
    assert_eq!(ids, ["todo", "backlog", "review", "in-progress", "done", "canceled"]);
    let back = WorkflowConfig::load(PROJECT).unwrap();
    assert_eq!(back.statuses()[2].id, "review", "on disk");
}

#[test]
fn moves_queued_behind_a_save_each_take_a_step() {
    let _home = Home::new("step-queue");
    let mut state = loaded();
    let (config, first) = state.request(state.move_status_by("todo", true).unwrap()).unwrap();
    // Two more Down presses while the first saves: computed on the old model,
    // applied to the new one.
    for _ in 0..2 {
        let down = state.move_status_by("todo", true).unwrap();
        assert!(state.request(down).is_none());
    }
    let mut job = state.committed(commit(PROJECT, &config, first));
    while let Some((config, edit)) = job {
        job = state.committed(commit(PROJECT, &config, edit));
    }
    let ids: Vec<_> = state.statuses().into_iter().map(|s| s.id).collect();
    assert_eq!(ids, ["backlog", "in-progress", "review", "done", "todo", "canceled"]);
    // A step past the end saves nothing and says nothing.
    make(&mut state, Edit::StepStatus { id: "canceled".into(), down: true });
    assert!(state.errors().is_empty());
}

#[test]
fn a_status_in_use_cant_be_deleted_and_an_unused_one_can() {
    let _home = Home::new("delete-status");
    let mut state = loaded();
    assert_eq!(
        state.delete_status_blocked("review").as_deref(),
        Some("Used in task. Remove it from them first.")
    );
    make(&mut state, Edit::DeleteStatus { id: "review".into() });
    assert!(state.error_at(&Target::Status("review".into())).is_some_and(|e| e.contains("used in task")));
    assert!(state.statuses().iter().any(|s| s.id == "review"), "still there");

    make(&mut state, Edit::AddStatus { name: "Spare".into(), category: StatusCategory::Backlog });
    assert_eq!(state.delete_status_blocked("spare"), None);
    let delete = Edit::DeleteStatus { id: "spare".into() };
    assert!(delete.is_destructive());
    assert_eq!(
        state.confirmation(&delete).as_deref(),
        Some("Delete status Spare (spare)? It is removed from statuses.yaml.")
    );
    make(&mut state, delete);
    assert!(WorkflowConfig::load(PROJECT).unwrap().status("spare").is_none());
    assert_eq!(state.confirmation(&Edit::SetDefault { name: "task".into() }), None);
}

#[test]
fn the_last_terminal_status_keeps_its_category() {
    let _home = Home::new("terminal");
    let mut state = loaded();
    // Done is one of two terminal statuses; Canceled the other.
    make(&mut state, Edit::SetCategory { id: "done".into(), category: StatusCategory::Active });
    assert_eq!(state.error_at(&Target::Status("done".into())), None);
    make(&mut state, Edit::SetCategory { id: "canceled".into(), category: StatusCategory::Backlog });
    let error = state.error_at(&Target::Status("canceled".into())).map(str::to_string);
    assert!(error.is_some(), "the model keeps a terminal status");
    assert_eq!(
        WorkflowConfig::load(PROJECT).unwrap().status("canceled").unwrap().category,
        StatusCategory::Archived
    );
}

#[test]
fn new_duplicate_and_default_workflows_persist() {
    let home = Home::new("workflows");
    let mut state = loaded();
    state.draft = Some(Draft::Workflow { from: None });
    make(&mut state, Edit::CreateWorkflow { name: "hotfix".into() });
    assert_eq!(state.draft, None, "the new row closes once it's created");
    assert!(home.workflows().join("hotfix.yaml").exists());

    assert_eq!(state.duplicate_name("task"), "task-copy");
    make(&mut state, Edit::DuplicateWorkflow { from: "task".into(), name: "task-copy".into() });
    assert_eq!(state.duplicate_name("task"), "task-copy-2");
    assert!(home.read("task-copy.yaml").contains("Branch work with a review gate."));
    assert!(home.read("task-copy.yaml").contains("## Statuses run left to right."), "comments come along");

    make(&mut state, Edit::SetDefault { name: "subtask".into() });
    let rows = state.workflows();
    assert_eq!(rows[0].name, "subtask", "the new default lists first");
    let project = std::fs::read_to_string(shelbi_state::projects_dir().unwrap().join(format!("{PROJECT}.yaml"))).unwrap();
    assert!(project.contains("default_workflow: subtask"), "{project}");
}

#[test]
fn a_bad_name_shows_on_the_new_row_and_keeps_it_open() {
    let _home = Home::new("bad-name");
    let mut state = loaded();
    state.draft = Some(Draft::Workflow { from: None });
    make(&mut state, Edit::CreateWorkflow { name: "task".into() });
    assert_eq!(state.error_at(&Target::NewWorkflow), Some("A workflow named `task` already exists"));
    assert!(state.draft.is_some());
}

#[test]
fn the_default_the_only_and_a_workflow_open_issues_use_cant_be_deleted() {
    let home = Home::new("delete-workflow");
    let mut state = loaded();
    assert!(state.delete_workflow_blocked("task").is_some_and(|e| e.contains("default")));
    assert_eq!(state.delete_workflow_blocked("subtask"), None);
    // The model refuses it too, should a client ever offer it.
    make(&mut state, Edit::DeleteWorkflow { name: "task".into() });
    assert!(state.error_at(&Target::Workflow("task".into())).is_some());
    assert!(home.workflows().join("task.yaml").exists());

    // Deleting reads the board: an open issue using the workflow blocks it.
    save_issue("sub-1", "subtask", shelbi_core::Column::todo());
    let delete = Edit::DeleteWorkflow { name: "subtask".into() };
    assert!(state.confirmation(&delete).is_some_and(|q| q.contains("workflows/subtask.yaml")));
    make(&mut state, delete.clone());
    let error = state.error_at(&Target::Workflow("subtask".into())).map(str::to_string);
    assert!(error.as_deref().is_some_and(|e| e.contains("used by 1 open issue: sub-1")), "{error:?}");
    assert!(home.workflows().join("subtask.yaml").exists());

    // Once it's done, it goes.
    save_issue("sub-1", "subtask", shelbi_core::Column::done());
    make(&mut state, delete);
    assert_eq!(state.error_at(&Target::Workflow("subtask".into())), None);
    assert!(!home.workflows().join("subtask.yaml").exists());
    assert_eq!(
        state.delete_workflow_blocked("task").as_deref(),
        Some("The default workflow can’t be deleted. Make another the default first.")
    );
}

#[test]
fn an_edit_made_while_one_saves_waits_its_turn() {
    let _home = Home::new("queue");
    let mut state = loaded();
    let (config, first) = state.request(Edit::RenameStatus { id: "todo".into(), name: "A".into() }).unwrap();
    assert!(state.saving());
    assert!(state.request(Edit::RenameStatus { id: "done".into(), name: "B".into() }).is_none());
    assert!(state.request(Edit::SetCategory { id: "review".into(), category: StatusCategory::Active }).is_none());
    assert_eq!(state.queued(), 2);
    let (config2, second) = state.committed(commit(PROJECT, &config, first)).expect("the queued one");
    assert_eq!(config2.status("todo").unwrap().name, "A", "builds on the first save");
    let (config3, third) = state.committed(commit(PROJECT, &config2, second)).expect("the last one");
    assert_eq!(state.committed(commit(PROJECT, &config3, third)).map(|_| ()), None);
    assert!(!state.saving());
    let back = WorkflowConfig::load(PROJECT).unwrap();
    assert_eq!(
        (back.status("todo").unwrap().name.as_str(), back.status("done").unwrap().name.as_str()),
        ("A", "B")
    );
    assert_eq!(back.status("review").unwrap().category, StatusCategory::Active);
}

#[test]
fn a_queued_edit_that_saves_doesnt_hide_the_one_that_failed() {
    let _home = Home::new("pending-failure");
    let mut state = loaded();
    let (config, first) = state.request(Edit::DeleteStatus { id: "review".into() }).unwrap();
    state.request(Edit::RenameStatus { id: "todo".into(), name: "Next".into() });
    let (config2, second) = state.committed(commit(PROJECT, &config, first)).expect("the queued rename");
    assert!(state.error_at(&Target::Status("review".into())).is_some(), "the delete was refused");
    assert_eq!(state.committed(commit(PROJECT, &config2, second)).map(|_| ()), None);
    assert!(
        state.error_at(&Target::Status("review".into())).is_some(),
        "the rename saving doesn't clear the delete's error"
    );
    assert_eq!(state.statuses()[1].name, "Next");
    // Another row's edit keeps it too; an edit to its own row clears it.
    make(&mut state, Edit::RenameStatus { id: "done".into(), name: "Shipped".into() });
    assert_eq!(state.errors().len(), 1);
    make(&mut state, Edit::RenameStatus { id: "review".into(), name: "In review".into() });
    assert!(state.errors().is_empty());
}

#[test]
fn a_late_completion_or_load_never_overrides_the_save_in_flight() {
    let _home = Home::new("late");
    let mut state = loaded();
    let (config, edit) = state.request(Edit::RenameStatus { id: "todo".into(), name: "Up next".into() }).unwrap();
    // A reload landing mid-save is ignored: the save brings its own model.
    assert!(!state.loaded(Err("stale".into())));
    assert!(state.error_at(&Target::Page).is_none());
    let done = commit(PROJECT, &config, edit);
    assert_eq!(state.committed(done).map(|_| ()), None);
    // A second completion with nothing in flight is dropped.
    let again = commit(PROJECT, &config, Edit::RenameStatus { id: "todo".into(), name: "Old".into() });
    assert!(again.error.is_some(), "the stale model conflicts with the save on disk");
    state.committed(again);
    assert!(state.error_at(&Target::Status("todo".into())).is_none());
    assert_eq!(state.statuses()[1].name, "Up next");
}

#[test]
fn a_file_changed_on_disk_is_reloaded_not_overwritten() {
    let home = Home::new("conflict");
    let mut state = loaded();
    let edited = STATUSES.replace("name: Done", "name: Shipped");
    std::fs::write(home.workflows().join("statuses.yaml"), &edited).unwrap();
    make(&mut state, Edit::RenameStatus { id: "todo".into(), name: "Next".into() });
    assert!(state
        .error_at(&Target::Status("todo".into()))
        .is_some_and(|e| e.starts_with("Changed on disk")));
    assert_eq!(home.read("statuses.yaml"), edited, "the hand edit stands");
    assert_eq!(state.statuses()[4].name, "Shipped", "the page shows the file as it is");
}

#[test]
fn an_unreadable_workflow_is_listed_untouched_and_other_edits_still_save() {
    let home = Home::new("unreadable");
    let broken = "name: broken\nstatuses: [oops\n";
    std::fs::write(home.workflows().join("broken.yaml"), broken).unwrap();
    let mut state = loaded();
    assert_eq!(state.workflows().len(), 2, "it isn't an editable row");
    let unreadable = state.unreadable();
    assert_eq!(unreadable.len(), 1);
    assert_eq!(unreadable[0].name, "broken");
    assert!(!unreadable[0].error.is_empty());
    make(&mut state, Edit::RenameStatus { id: "todo".into(), name: "Next".into() });
    make(&mut state, Edit::CreateWorkflow { name: "hotfix".into() });
    assert!(state.errors().is_empty(), "{:?}", state.errors());
    assert_eq!(home.read("broken.yaml"), broken, "never rewritten");
}

#[test]
fn a_failed_reload_keeps_the_model_and_says_why() {
    let home = Home::new("reload");
    let mut state = loaded();
    std::fs::write(home.workflows().join("statuses.yaml"), "statuses: [\n").unwrap();
    assert!(state.loaded(shelbi_app::settings::workflows::load(PROJECT)));
    assert!(state.error_at(&Target::Page).is_some_and(|e| e.starts_with("Couldn't read the workflows")));
    assert_eq!(state.statuses().len(), 6, "the last good model stays");
    std::fs::write(home.workflows().join("statuses.yaml"), STATUSES).unwrap();
    assert!(state.loaded(shelbi_app::settings::workflows::load(PROJECT)));
    assert!(state.errors().is_empty());
}

#[test]
fn a_save_that_drops_comments_says_which() {
    let home = Home::new("comments");
    let text = format!("{STATUSES}- id: spare\n  # Keep for the Q3 experiment.\n  name: Spare\n  category: backlog\n");
    std::fs::write(home.workflows().join("statuses.yaml"), text).unwrap();
    let mut state = loaded();
    make(&mut state, Edit::DeleteStatus { id: "spare".into() });
    assert!(state.errors().is_empty(), "{:?}", state.errors());
    let notice = state.notice().map(str::to_string);
    assert!(
        notice.as_deref().is_some_and(|n| n.contains("statuses.yaml: # Keep for the Q3 experiment.")),
        "{notice:?}"
    );
    // The next edit clears it.
    make(&mut state, Edit::RenameStatus { id: "todo".into(), name: "Next".into() });
    assert_eq!(state.notice(), None);
}

#[test]
fn edit_yaml_writes_the_shipped_catalog_when_there_is_none() {
    let home = Home::new("edit-yaml");
    let path = home.workflows().join("statuses.yaml");
    std::fs::remove_file(&path).unwrap();
    shelbi_app::settings::workflows::prepare_statuses_yaml(PROJECT, &path).unwrap();
    assert!(home.read("statuses.yaml").contains("statuses:"));
    let missing = home.workflows().join("gone.yaml");
    assert_eq!(
        shelbi_app::settings::workflows::prepare_workflow_yaml("gone", &missing),
        Err("Couldn't find gone.yaml.".to_string())
    );
}
