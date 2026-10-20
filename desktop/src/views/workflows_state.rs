//! Settings ▸ Workflows' state, toolkit-free (`dt-wf-settings-index`): the
//! project's workflows and status catalog as `shelbi_state::workflow_config`
//! loads them, the edits the page makes to them, and the guards that
//! disable an action up front.
//!
//! Every edit is an [`Edit`] applied to the model and saved at once (the
//! page has no Save button: a status renamed on blur is on disk a moment
//! later, as the other Settings pages save). Saves run one at a time off the
//! UI thread, each on the model the last one left ([`WorkflowsState::request`]
//! queues an edit made while one is in flight), so an edit never races its
//! own previous save into a conflict. An edit the model refuses, or a save
//! that fails, leaves the files as they were; the page reloads them and
//! shows the error where the edit was made ([`Target`]).

use std::collections::VecDeque;

use shelbi_core::StatusCategory;
use shelbi_state::workflow_config::{ConfigError, StatusSummary, WorkflowConfig, WorkflowSummary};

/// The six categories, in the order the picker and the legend list them.
pub const CATEGORIES: [StatusCategory; 6] = [
    StatusCategory::Backlog,
    StatusCategory::Ready,
    StatusCategory::Active,
    StatusCategory::Handoff,
    StatusCategory::Done,
    StatusCategory::Archived,
];

/// A category's name, as the picker shows it.
pub fn category_label(category: StatusCategory) -> &'static str {
    match category {
        StatusCategory::Backlog => "Backlog",
        StatusCategory::Ready => "Ready",
        StatusCategory::Active => "Active",
        StatusCategory::Handoff => "Handoff",
        StatusCategory::Done => "Done",
        StatusCategory::Archived => "Archived",
    }
}

/// What a category means (the frame's Status categories legend).
pub fn category_help(category: StatusCategory) -> &'static str {
    match category {
        StatusCategory::Backlog => "Waiting to be triaged.",
        StatusCategory::Ready => "Ready for the owner to pick up.",
        StatusCategory::Active => "Work is in progress.",
        StatusCategory::Handoff => "Waiting for another owner’s input.",
        StatusCategory::Done => "Accepted or shipped.",
        StatusCategory::Archived => "Closed without shipping, such as canceled or duplicate.",
    }
}

/// One change the page makes, applied through the model's API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    AddStatus { name: String, category: StatusCategory },
    RenameStatus { id: String, name: String },
    SetCategory { id: String, category: StatusCategory },
    /// Move status `id` to `index` in the catalog's order.
    MoveStatus { id: String, index: usize },
    DeleteStatus { id: String },
    CreateWorkflow { name: String },
    DuplicateWorkflow { from: String, name: String },
    SetDefault { name: String },
    DeleteWorkflow { name: String },
}

impl Edit {
    /// `config` with the edit made, or why the model refused it. Deleting a
    /// workflow reads the board (whether open issues use it), so this runs
    /// off the UI thread.
    pub fn apply(&self, config: &WorkflowConfig) -> Result<WorkflowConfig, ConfigError> {
        match self {
            Edit::AddStatus { name, category } => config.add_status(name, *category),
            Edit::RenameStatus { id, name } => config.rename_status(id, name),
            Edit::SetCategory { id, category } => config.set_status_category(id, *category),
            Edit::MoveStatus { id, index } => config.move_status(id, *index),
            Edit::DeleteStatus { id } => config.delete_status(id),
            Edit::CreateWorkflow { name } => config.create_workflow(name, None),
            Edit::DuplicateWorkflow { from, name } => config.duplicate_workflow(from, name),
            Edit::SetDefault { name } => config.set_default_workflow(name),
            Edit::DeleteWorkflow { name } => config.delete_workflow(name),
        }
    }

    /// Where its error shows.
    pub fn target(&self) -> Target {
        match self {
            Edit::AddStatus { .. } => Target::NewStatus,
            Edit::RenameStatus { id, .. }
            | Edit::SetCategory { id, .. }
            | Edit::MoveStatus { id, .. }
            | Edit::DeleteStatus { id } => Target::Status(id.clone()),
            Edit::CreateWorkflow { .. } | Edit::DuplicateWorkflow { .. } => Target::NewWorkflow,
            Edit::SetDefault { name } | Edit::DeleteWorkflow { name } => Target::Workflow(name.clone()),
        }
    }
}

/// Where an error shows: under the row it's about, or under the table's
/// new row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Workflow(String),
    NewWorkflow,
    Status(String),
    NewStatus,
    /// Loading, or Edit YAML: under the page's header.
    Page,
}

/// What one save did, handed back to the UI thread.
#[derive(Debug)]
pub struct Committed {
    pub edit: Edit,
    /// The model as now on disk (`None` if it couldn't be reloaded after a
    /// failure: the page keeps the one it had).
    pub config: Option<WorkflowConfig>,
    pub error: Option<String>,
    /// Comment lines a file lost to the edit (each `path: line`).
    pub dropped_comments: Vec<String>,
}

/// Apply `edit` to `config` and save it, off the UI thread. A refusal or a
/// failed save writes nothing; the model is reloaded from disk then, so the
/// page shows the files as they are (a conflict means they changed under
/// it).
pub fn commit(project: &str, config: &WorkflowConfig, edit: Edit) -> Committed {
    let saved = edit.apply(config).and_then(|next| next.save());
    match saved {
        Ok(saved) => Committed {
            dropped_comments: saved
                .dropped_comments
                .iter()
                .flat_map(|(path, lines)| {
                    let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    lines.iter().map(move |l| format!("{file}: {}", l.trim()))
                })
                .collect(),
            edit,
            config: Some(saved.config),
            error: None,
        },
        Err(e) => Committed {
            edit,
            config: WorkflowConfig::load(project).ok(),
            error: Some(sentence(&e.to_string())),
            dropped_comments: Vec::new(),
        },
    }
}

/// `text` as a sentence: a capital first letter (the model's messages start
/// lower case, for embedding).
pub fn sentence(text: &str) -> String {
    shelbi_app::view::title_case(text)
}

/// A name being typed into a table's new row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draft {
    /// New workflow, or (`from`) a duplicate of one.
    Workflow { from: Option<String> },
    /// New status, in `category`.
    Status { category: StatusCategory },
}

/// The page's state.
#[derive(Debug)]
pub struct WorkflowsState {
    project: String,
    /// `None` until the first load lands.
    config: Option<WorkflowConfig>,
    /// The last error, where it shows.
    error: Option<(Target, String)>,
    /// Comments the last save dropped, shown until the next edit.
    notice: Option<String>,
    /// An edit is being saved.
    saving: bool,
    /// Edits made while one was saving, in order.
    queue: VecDeque<Edit>,
    /// The new row being typed into, if any.
    pub draft: Option<Draft>,
}

impl WorkflowsState {
    pub fn new(project: &str) -> Self {
        WorkflowsState {
            project: project.to_string(),
            config: None,
            error: None,
            notice: None,
            saving: false,
            queue: VecDeque::new(),
            draft: None,
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn config(&self) -> Option<&WorkflowConfig> {
        self.config.as_ref()
    }

    /// The first load (or a reload) landed.
    pub fn loaded(&mut self, config: Result<WorkflowConfig, String>) {
        match config {
            Ok(config) => self.config = Some(config),
            Err(e) => self.error = Some((Target::Page, format!("Couldn't read the workflows: {e}"))),
        }
    }

    /// The error shown at `target`.
    pub fn error_at(&self, target: &Target) -> Option<&str> {
        self.error.as_ref().filter(|(t, _)| t == target).map(|(_, e)| e.as_str())
    }

    /// Show `message` at `target` (an Edit YAML that couldn't open).
    pub fn set_error(&mut self, target: Target, message: String) {
        self.error = Some((target, message));
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn saving(&self) -> bool {
        self.saving
    }

    /// Make `edit`: the save to start now (the model to apply it to), or
    /// `None` while another save runs (it is queued and starts when that
    /// one lands) or before the model has loaded.
    pub fn request(&mut self, edit: Edit) -> Option<(WorkflowConfig, Edit)> {
        self.error = None;
        self.notice = None;
        let config = self.config.clone()?;
        if self.saving {
            self.queue.push_back(edit);
            return None;
        }
        self.saving = true;
        Some((config, edit))
    }

    /// A save landed: take its model and error, and hand back the next
    /// queued save, if any.
    pub fn committed(&mut self, done: Committed) -> Option<(WorkflowConfig, Edit)> {
        if let Some(config) = done.config {
            self.config = Some(config);
        }
        if let Some(e) = done.error {
            self.error = Some((done.edit.target(), e));
        } else if matches!(done.edit, Edit::CreateWorkflow { .. } | Edit::DuplicateWorkflow { .. } | Edit::AddStatus { .. }) {
            self.draft = None;
        }
        if !done.dropped_comments.is_empty() {
            self.notice = Some(format!(
                "The save dropped these comments: {}",
                done.dropped_comments.join("; ")
            ));
        }
        self.saving = false;
        let next = self.queue.pop_front()?;
        self.saving = true;
        Some((self.config.clone()?, next))
    }

    /// The workflow table's rows: the default first (as the frame lists
    /// it), then the rest by name.
    pub fn workflows(&self) -> Vec<WorkflowSummary> {
        let mut rows = self.config.as_ref().map(WorkflowConfig::workflow_summaries).unwrap_or_default();
        rows.sort_by_key(|r| !r.is_default);
        rows
    }

    /// The status table's rows, each "Used in" in the workflow table's
    /// order.
    pub fn statuses(&self) -> Vec<StatusSummary> {
        let order: Vec<String> = self.workflows().into_iter().map(|w| w.name).collect();
        let mut rows = self.config.as_ref().map(WorkflowConfig::status_summaries).unwrap_or_default();
        for row in &mut rows {
            // Unreadable files, not in the table, go last.
            row.used_in.sort_by_key(|w| order.iter().position(|o| o == w).unwrap_or(usize::MAX));
        }
        rows
    }

    /// Why workflow `name` can't be deleted, known without reading the
    /// board (the default, or the only one); `None` lets the menu offer it,
    /// and open issues using it are checked when it's chosen.
    pub fn delete_workflow_blocked(&self, name: &str) -> Option<String> {
        let config = self.config.as_ref()?;
        if config.default_workflow() == name {
            Some("The default workflow can’t be deleted. Make another the default first.".into())
        } else if config.workflows().count() <= 1 {
            Some("The only workflow can’t be deleted.".into())
        } else {
            None
        }
    }

    /// Why status `id` can't be deleted: the workflows that use it.
    pub fn delete_status_blocked(&self, id: &str) -> Option<String> {
        let used = self.config.as_ref()?.used_in(id);
        (!used.is_empty()).then(|| format!("Used in {}. Remove it from them first.", used.join(", ")))
    }

    /// The name a duplicate of `from` is offered: `<from>-copy`, then
    /// `-copy-2`, `-copy-3`, … to be unique.
    pub fn duplicate_name(&self, from: &str) -> String {
        let base = format!("{from}-copy");
        let taken = |name: &str| self.config.as_ref().is_some_and(|c| c.workflow(name).is_some());
        let mut name = base.clone();
        let mut n = 2;
        while taken(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }

    /// The edit a status dropped on status `onto`'s row makes: it takes
    /// that row's place. `None` when it's dropped on itself.
    pub fn drop_status(&self, dragged: &str, onto: &str) -> Option<Edit> {
        let statuses = self.config.as_ref()?.statuses();
        let index = statuses.iter().position(|s| s.id == onto)?;
        (dragged != onto).then(|| Edit::MoveStatus {
            id: dragged.to_string(),
            index,
        })
    }
}

/// The Workflows frames' content (`--visual-figma`, and the tests): the
/// shipped catalog (To Do as the frame names it), Task (six statuses, four
/// transitions, owners, agents, tags and git as the Workflow Form frame
/// draws them) and Subtask (five, two), and the agents they name.
#[cfg(any(test, feature = "visual-check"))]
pub(crate) mod fixture {
    use std::path::Path;

    /// Write the frame's `workflows/` and `agents/` into `project_dir`.
    pub fn write(project_dir: &Path) {
        let workflows = project_dir.join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        for agent in ["developer", "orchestrator", "review"] {
            let dir = project_dir.join("agents").join(agent);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("instructions.md"), "# agent\n").unwrap();
        }
        std::fs::write(workflows.join("statuses.yaml"), STATUSES).unwrap();
        std::fs::write(workflows.join("task.yaml"), TASK).unwrap();
        std::fs::write(workflows.join("subtask.yaml"), SUBTASK).unwrap();
    }

    pub const STATUSES: &str = "\
## The project's statuses.
statuses:
- id: backlog
  name: Backlog
  category: backlog
- id: todo
  name: To Do
  category: ready
- id: in-progress
  name: In Progress
  category: active
- id: review
  name: Review
  category: handoff
- id: done
  name: Done
  category: done
- id: canceled
  name: Canceled
  category: archived
";

    pub const TASK: &str = "\
name: task
description: Branch work with a review gate.
## Statuses run left to right.
statuses:
- id: backlog
  owner: user
  agent: orchestrator
- id: todo
  owner: agent
  agent: orchestrator
- id: in-progress
  owner: agent
  agent: developer
- id: review
  owner: user
  agent: review
  tags: [review]
- id: done
  owner: user
- id: canceled
  owner: user
transitions:
- from: in-progress
  to: review
  actions: [push_branch, open_pr]
- from: review
  to: done
  actions: [push_branch, merge]
- from: in-progress
  to: canceled
  actions: [close_pr, delete_branch]
- from: review
  to: canceled
  actions: [close_pr, delete_branch]
git:
  base_branch: main
  branch: '{{github_user}}/{{id}}'
  merge_strategy: squash
";

    pub const SUBTASK: &str = "\
name: subtask
description: Focused work that merges back into a parent task.
statuses:
- id: backlog
  owner: user
- id: todo
  owner: user
- id: in-progress
  owner: agent
  agent: developer
- id: done
  owner: user
- id: canceled
  owner: user
transitions:
- from: todo
  to: in-progress
- from: in-progress
  to: done
  actions: [merge]
";
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};

    use super::fixture::{self, STATUSES};
    use super::*;

    /// A throwaway `SHELBI_HOME` holding project `my-project` (its YAML, an
    /// `agents/` for the developer and review agents, and the frame's two
    /// workflows over the default catalog), set for the test's length. Tests
    /// that set it run one at a time ([`crate::test_env::lock`]).
    pub(crate) struct Home {
        pub root: PathBuf,
        project: String,
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    pub(crate) const PROJECT: &str = "my-project";

    impl Home {
        pub(crate) fn new(tag: &str) -> Home {
            Home::with_project(tag, PROJECT)
        }

        /// [`Home::new`] with the fixture project named `project`.
        pub(crate) fn with_project(tag: &str, project: &str) -> Home {
            let lock = crate::test_env::lock();
            let root = PathBuf::from(format!("/tmp/shb-wf-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let prev = std::env::var_os("SHELBI_HOME");
            std::env::set_var("SHELBI_HOME", &root);
            write_project(&root, project);
            Home {
                root,
                project: project.to_string(),
                prev,
                _lock: lock,
            }
        }

        pub(crate) fn workflows(&self) -> PathBuf {
            self.root.join("projects").join(&self.project).join("workflows")
        }

        pub(crate) fn read(&self, file: &str) -> String {
            std::fs::read_to_string(self.workflows().join(file)).unwrap()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The fixture project: its YAML (Task the default), and the frame's
    /// workflows ([`fixture::write`]).
    pub(crate) fn write_project(root: &Path, project: &str) {
        let projects = root.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join(format!("{project}.yaml")),
            "name: My project\nrepo: ''\ndefault_branch: main\ndefault_workflow: task\n\
             orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n\
             machines:\n  - name: hub\n    kind: local\n    work_dir: /tmp\nworkspaces:\n  - name: alpha\n    machine: hub\n",
        )
        .unwrap();
        fixture::write(&projects.join(project));
    }

    fn loaded() -> WorkflowsState {
        let mut state = WorkflowsState::new(PROJECT);
        state.loaded(WorkflowConfig::load(PROJECT).map_err(|e| e.to_string()));
        state
    }

    /// Make `edit` the way the page does: request it, commit it (here,
    /// rather than off the UI thread), and take the result.
    fn make(state: &mut WorkflowsState, edit: Edit) {
        let mut job = state.request(edit);
        while let Some((config, edit)) = job {
            let done = commit(PROJECT, &config, edit);
            job = state.committed(done);
        }
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
        make(&mut state, Edit::DeleteStatus { id: "spare".into() });
        assert!(WorkflowConfig::load(PROJECT).unwrap().status("spare").is_none());
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

        make(&mut state, Edit::SetDefault { name: "subtask".into() });
        let rows = state.workflows();
        assert!(rows.iter().find(|r| r.name == "subtask").unwrap().is_default);
        let project = std::fs::read_to_string(home.root.join("projects").join(format!("{PROJECT}.yaml"))).unwrap();
        assert!(project.contains("default_workflow: subtask"), "{project}");
    }

    #[test]
    fn a_bad_name_shows_on_the_new_row_and_keeps_it_open() {
        let _home = Home::new("bad-name");
        let mut state = loaded();
        state.draft = Some(Draft::Workflow { from: None });
        make(&mut state, Edit::CreateWorkflow { name: "task".into() });
        assert_eq!(
            state.error_at(&Target::NewWorkflow),
            Some("A workflow named `task` already exists")
        );
        assert!(state.draft.is_some());
    }

    #[test]
    fn the_default_and_the_only_workflow_cant_be_deleted() {
        let home = Home::new("delete-workflow");
        let mut state = loaded();
        assert!(state.delete_workflow_blocked("task").is_some_and(|e| e.contains("default")));
        assert_eq!(state.delete_workflow_blocked("subtask"), None);
        // The model refuses it too, should the menu ever offer it.
        make(&mut state, Edit::DeleteWorkflow { name: "task".into() });
        assert!(state.error_at(&Target::Workflow("task".into())).is_some());
        assert!(home.workflows().join("task.yaml").exists());

        // Deleting reads the board: an open issue using the workflow
        // blocks it (it would fall back to the default).
        save_issue("sub-1", "subtask", shelbi_core::Column::todo());
        make(&mut state, Edit::DeleteWorkflow { name: "subtask".into() });
        let error = state.error_at(&Target::Workflow("subtask".into())).map(str::to_string);
        assert!(error.as_deref().is_some_and(|e| e.contains("used by 1 open issue: sub-1")), "{error:?}");
        assert!(home.workflows().join("subtask.yaml").exists());

        // Once it's done, it goes.
        save_issue("sub-1", "subtask", shelbi_core::Column::done());
        make(&mut state, Edit::DeleteWorkflow { name: "subtask".into() });
        assert_eq!(state.error_at(&Target::Workflow("subtask".into())), None);
        assert!(!home.workflows().join("subtask.yaml").exists());
        assert!(state.delete_workflow_blocked("task").is_some_and(|e| e.contains("default")));
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
    fn an_edit_made_while_one_saves_waits_its_turn() {
        let _home = Home::new("queue");
        let mut state = loaded();
        let (config, first) = state.request(Edit::RenameStatus { id: "todo".into(), name: "A".into() }).unwrap();
        assert!(state.saving());
        assert!(state.request(Edit::RenameStatus { id: "done".into(), name: "B".into() }).is_none());
        let (config2, second) = state.committed(commit(PROJECT, &config, first)).expect("the queued one");
        assert_eq!(config2.status("todo").unwrap().name, "A", "builds on the first save");
        assert_eq!(state.committed(commit(PROJECT, &config2, second)).map(|_| ()), None);
        assert!(!state.saving());
        let back = WorkflowConfig::load(PROJECT).unwrap();
        assert_eq!((back.status("todo").unwrap().name.as_str(), back.status("done").unwrap().name.as_str()), ("A", "B"));
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
}
