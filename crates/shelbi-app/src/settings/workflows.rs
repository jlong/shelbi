//! Settings ▸ Workflows' state, toolkit-free: the project's workflows and
//! status catalog as `shelbi_state::workflow_config` loads them, the edits
//! the page makes to them, and the guards that refuse an action up front.
//! The desktop app (`dt-wf-settings-index`) and the TUI
//! (`tui-settings-workflows`) draw it.
//!
//! Every edit is an [`Edit`] applied to the model and saved at once (the
//! page has no Save button: a status renamed is on disk a moment later).
//! Saves run one at a time off the UI thread, each on the model the last one
//! left ([`WorkflowsState::request`] queues an edit made while one is in
//! flight), so an edit never races its own previous save into a conflict. An
//! edit the model refuses, or a save that fails, leaves the files as they
//! were; the page reloads them and shows the error where the edit was made
//! ([`Target`]). Each target keeps its own error, so a queued edit that saves
//! doesn't hide an earlier one that failed.

use std::collections::VecDeque;
use std::path::Path;

use shelbi_core::{StatusCategory, TrackerName};

use super::store::ConfigStore;
use shelbi_state::workflow_config::{
    ConfigError, StatusSummary, UnreadableWorkflow, WorkflowConfig, WorkflowSummary,
};

/// The Workflows section's helper.
pub const WORKFLOWS_HELP: &str =
    "Configure the statuses, agents, and transitions for each kind of work.";

/// The Statuses section's helper.
pub const STATUSES_HELP: &str = "Work transitions through statuses in workflows. Configure the \
     statuses, agents, and transitions for each kind of work.";

/// The note under the status table.
pub const RENAME_NOTE: &str =
    "Renaming a status keeps its ID, so existing workflow references stay intact.";

/// "Sync now": read the tracker's statuses into the catalog again.
pub const SYNC_NOW: &str = "Sync now";
/// While it runs.
pub const SYNCING: &str = "Syncing…";

/// The banner over a catalog a tracker manages (`managed_by:` in
/// `statuses.yaml`): the statuses come from there, and only each one's
/// category is edited here.
pub fn managed_banner(tracker: TrackerName, team: Option<&str>) -> String {
    let name = tracker_label(tracker);
    let whose = match team {
        Some(team) => format!("{name} (team {team})"),
        None => name.to_string(),
    };
    format!(
        "Statuses are managed by {whose}. Rename, reorder, add or remove them in {name}; Shelbi syncs \
         automatically. Each status's category is still yours to set."
    )
}

/// A tracker's name in prose.
pub fn tracker_label(tracker: TrackerName) -> &'static str {
    match tracker {
        TrackerName::Linear => "Linear",
    }
}

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

/// What a category means (the Status categories legend).
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
    /// Move status `id` one row down (or up), from wherever it is when the
    /// edit is applied: the keyboard's reorder, so moves queued behind a
    /// save each take one step.
    StepStatus { id: String, down: bool },
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
            Edit::StepStatus { id, down } => {
                let at = config
                    .statuses()
                    .iter()
                    .position(|s| s.id == *id)
                    .ok_or_else(|| ConfigError::UnknownStatus(id.clone()))?;
                let index = if *down { at + 1 } else { at.wrapping_sub(1) };
                if index >= config.statuses().len() {
                    // Already at that end: nothing changes.
                    return Ok(config.clone());
                }
                config.move_status(id, index)
            }
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
            | Edit::StepStatus { id, .. }
            | Edit::DeleteStatus { id } => Target::Status(id.clone()),
            Edit::CreateWorkflow { .. } | Edit::DuplicateWorkflow { .. } => Target::NewWorkflow,
            Edit::SetDefault { name } | Edit::DeleteWorkflow { name } => {
                Target::Workflow(name.clone())
            }
        }
    }

    /// Whether a catalog a tracker manages takes the edit: only a status's
    /// category (and the workflow edits, which aren't the catalog's).
    pub fn allowed_when_managed(&self) -> bool {
        !matches!(
            self,
            Edit::AddStatus { .. }
                | Edit::RenameStatus { .. }
                | Edit::MoveStatus { .. }
                | Edit::StepStatus { .. }
                | Edit::DeleteStatus { .. }
        )
    }

    /// Whether the edit removes something (a client confirms it first).
    pub fn is_destructive(&self) -> bool {
        matches!(self, Edit::DeleteStatus { .. } | Edit::DeleteWorkflow { .. })
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

/// Apply `edit` to `config` and save it through `store` (the window's
/// project, wherever it lives), off the UI thread. A refusal or a failed
/// save writes nothing; the model is read back then, so the page shows the
/// files as they are (a conflict means they changed under it).
pub fn commit(store: &dyn ConfigStore, config: &WorkflowConfig, edit: Edit) -> Committed {
    let saved = edit
        .apply(config)
        .and_then(|next| super::store::save_workflows(store, &next));
    match saved {
        Ok(saved) => Committed {
            dropped_comments: saved
                .dropped_comments
                .iter()
                .flat_map(|(path, lines)| {
                    let file = path
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    lines.iter().map(move |l| format!("{file}: {}", l.trim()))
                })
                .collect(),
            edit,
            config: Some(saved.config),
            error: None,
        },
        Err(e) => Committed {
            edit,
            config: store.workflows().ok(),
            error: Some(sentence(&e.to_string())),
            dropped_comments: Vec::new(),
        },
    }
}

/// Read `project`'s workflow config, off the UI thread, for
/// [`WorkflowsState::loaded`].
pub fn load(project: &str) -> Result<WorkflowConfig, String> {
    WorkflowConfig::load(project).map_err(|e| e.to_string())
}

/// Get `statuses.yaml` (at `path`) ready for the user's editor, off the UI
/// thread: a project with none yet gets the shipped catalog written first.
pub fn prepare_statuses_yaml(project: &str, path: &Path) -> Result<(), String> {
    if !path.exists() {
        shelbi_state::scaffold_project_statuses(project)
            .map_err(|e| format!("Couldn't write statuses.yaml: {e}"))?;
    }
    Ok(())
}

/// Check workflow `name`'s file (at `path`) is there to open in the user's
/// editor; one that can't be read opens too, since that's how it gets fixed.
pub fn prepare_workflow_yaml(name: &str, path: &Path) -> Result<(), String> {
    if path.exists() {
        Ok(())
    } else {
        Err(format!("Couldn't find {name}.yaml."))
    }
}

/// `text` as a sentence: a capital first letter (the model's messages start
/// lower case, for embedding).
pub fn sentence(text: &str) -> String {
    crate::view::title_case(text)
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
    /// The errors shown, one per target, oldest first.
    errors: Vec<(Target, String)>,
    /// Comments the last save dropped, shown until the next edit.
    notice: Option<String>,
    /// An edit is being saved.
    saving: bool,
    /// Edits made while one was saving, in order.
    queue: VecDeque<Edit>,
    /// The new row being typed into, if any.
    pub draft: Option<Draft>,
    /// A "Sync now" is running.
    syncing: bool,
    /// What the last one said: `Ok` a line saying what changed, `Err` why
    /// it failed.
    synced: Option<Result<String, String>>,
}

impl WorkflowsState {
    pub fn new(project: &str) -> Self {
        WorkflowsState {
            project: project.to_string(),
            config: None,
            errors: Vec::new(),
            notice: None,
            saving: false,
            queue: VecDeque::new(),
            draft: None,
            syncing: false,
            synced: None,
        }
    }

    /// The tracker that manages the status catalog, if one does: the page
    /// hides adding, renaming, reordering and deleting statuses, and shows
    /// [`managed_banner`] and [`SYNC_NOW`].
    pub fn read_only(&self) -> Option<TrackerName> {
        self.config.as_ref().and_then(WorkflowConfig::statuses_read_only)
    }

    /// The banner over a managed catalog (`None` when it isn't managed).
    pub fn banner(&self) -> Option<String> {
        let tracker = self.read_only()?;
        let team = self.config.as_ref().and_then(|c| c.issue_tracker().target.clone());
        Some(managed_banner(tracker, team.as_deref()))
    }

    /// Start "Sync now" (the host runs the tracker service's
    /// `sync_statuses` off the UI thread and answers [`Self::synced`]).
    /// False while one runs, or when the catalog isn't managed.
    pub fn begin_sync(&mut self) -> bool {
        if self.syncing || self.read_only().is_none() {
            return false;
        }
        self.syncing = true;
        self.synced = None;
        true
    }

    /// The sync finished: the host reads the files again next.
    pub fn synced(&mut self, result: crate::tracker_connect::StatusSync) {
        use crate::tracker_connect::StatusSync;
        if !std::mem::take(&mut self.syncing) {
            return;
        }
        self.synced = Some(match result {
            StatusSync::Synced(line) => Ok(format!("Synced: {line}.")),
            StatusSync::NotNeeded => Ok("Nothing to sync: the project isn't on Linear.".into()),
            StatusSync::Failed(why) => Err(format!("Couldn't sync the statuses: {why}")),
        });
    }

    pub fn syncing(&self) -> bool {
        self.syncing
    }

    /// The last sync's line, until the next.
    pub fn sync_note(&self) -> Option<&Result<String, String>> {
        self.synced.as_ref()
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn config(&self) -> Option<&WorkflowConfig> {
        self.config.as_ref()
    }

    /// A load (the first, or a reload to pick up hand edits) landed. Returns
    /// whether it was taken: while a save runs it isn't, since that save
    /// lands its own fresh model. A failed read keeps the model the page has
    /// and says why under the header.
    pub fn loaded(&mut self, config: Result<WorkflowConfig, String>) -> bool {
        if self.saving {
            return false;
        }
        match config {
            Ok(config) => {
                self.config = Some(config);
                self.clear_error(&Target::Page);
            }
            Err(e) => self.set_error(Target::Page, format!("Couldn't read the workflows: {e}")),
        }
        true
    }

    /// The error shown at `target`.
    pub fn error_at(&self, target: &Target) -> Option<&str> {
        self.errors.iter().find(|(t, _)| t == target).map(|(_, e)| e.as_str())
    }

    /// Every error shown, oldest first (a client shows one whose row is gone
    /// under the header).
    pub fn errors(&self) -> &[(Target, String)] {
        &self.errors
    }

    /// Show `message` at `target` (an Edit YAML that couldn't open, a guard
    /// that refused an action), in place of any error there.
    pub fn set_error(&mut self, target: Target, message: String) {
        self.clear_error(&target);
        self.errors.push((target, message));
    }

    /// Stop showing the error at `target`.
    pub fn clear_error(&mut self, target: &Target) {
        self.errors.retain(|(t, _)| t != target);
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn saving(&self) -> bool {
        self.saving
    }

    /// Edits waiting behind the save in flight.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Make `edit`: the save to start now (the model to apply it to), or
    /// `None` while another save runs (it is queued and starts when that
    /// one lands) or before the model has loaded. The edit's row (and the
    /// header) stop showing their errors; other rows keep theirs.
    pub fn request(&mut self, edit: Edit) -> Option<(WorkflowConfig, Edit)> {
        if let Some(tracker) = self.read_only().filter(|_| !edit.allowed_when_managed()) {
            // The page hides these; a key that slips through says why.
            self.set_error(edit.target(), sentence(&shelbi_state::workflow_config::ConfigError::ReadOnlyStatus(tracker).to_string()));
            return None;
        }
        self.clear_error(&edit.target());
        self.clear_error(&Target::Page);
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
    /// queued save, if any. A completion with no save in flight (one a
    /// client should have dropped) is ignored.
    pub fn committed(&mut self, done: Committed) -> Option<(WorkflowConfig, Edit)> {
        if !self.saving {
            return None;
        }
        if let Some(config) = done.config {
            self.config = Some(config);
        }
        let target = done.edit.target();
        if let Some(e) = done.error {
            self.set_error(target, e);
        } else {
            self.clear_error(&target);
            if matches!(
                done.edit,
                Edit::CreateWorkflow { .. } | Edit::DuplicateWorkflow { .. } | Edit::AddStatus { .. }
            ) {
                self.draft = None;
            }
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

    /// The workflow table's rows: the default first, then the rest by name.
    pub fn workflows(&self) -> Vec<WorkflowSummary> {
        let mut rows = self
            .config
            .as_ref()
            .map(WorkflowConfig::workflow_summaries)
            .unwrap_or_default();
        rows.sort_by_key(|r| !r.is_default);
        rows
    }

    /// The workflow files the model can't edit, listed with their errors
    /// (never written).
    pub fn unreadable(&self) -> &[UnreadableWorkflow] {
        self.config.as_ref().map(WorkflowConfig::unreadable).unwrap_or_default()
    }

    /// The status table's rows, each "Used in" in the workflow table's
    /// order.
    pub fn statuses(&self) -> Vec<StatusSummary> {
        let order: Vec<String> = self.workflows().into_iter().map(|w| w.name).collect();
        let mut rows = self
            .config
            .as_ref()
            .map(WorkflowConfig::status_summaries)
            .unwrap_or_default();
        for row in &mut rows {
            // Unreadable files, not in the table, go last.
            row.used_in
                .sort_by_key(|w| order.iter().position(|o| o == w).unwrap_or(usize::MAX));
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
        if let Some(tracker) = self.read_only() {
            return Some(format!("Delete it in {}.", tracker_label(tracker)));
        }
        let used = self.config.as_ref()?.used_in(id);
        (!used.is_empty()).then(|| format!("Used in {}. Remove it from them first.", used.join(", ")))
    }

    /// The question a client asks before a destructive `edit`; `None` for
    /// one that removes nothing.
    pub fn confirmation(&self, edit: &Edit) -> Option<String> {
        match edit {
            Edit::DeleteWorkflow { name } => Some(format!(
                "Delete workflow {name}? Its file, workflows/{name}.yaml, is removed. Issues \
                 that finished on it keep its name."
            )),
            Edit::DeleteStatus { id } => {
                let name = self
                    .config
                    .as_ref()
                    .and_then(|c| c.status(id))
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| id.clone());
                Some(format!(
                    "Delete status {name} ({id})? It is removed from statuses.yaml."
                ))
            }
            _ => None,
        }
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

    /// The id a new status named `name` gets (it never changes after).
    /// `None` until a name is typed.
    pub fn new_status_id(&self, name: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        self.config.as_ref().map(|c| c.status_id_for(name))
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

    /// The edit moving status `id` one row up (`down` false) or down: the
    /// keyboard's reorder ([`Edit::StepStatus`]). `None` when the shown
    /// model has it at that end already.
    pub fn move_status_by(&self, id: &str, down: bool) -> Option<Edit> {
        let statuses = self.config.as_ref()?.statuses();
        let at = statuses.iter().position(|s| s.id == id)?;
        let movable = if down { at + 1 < statuses.len() } else { at > 0 };
        movable.then(|| Edit::StepStatus {
            id: id.to_string(),
            down,
        })
    }
}

/// The Workflows frames' content (the desktop's `--visual-figma`, and both
/// clients' tests): the shipped catalog (To Do as the frame names it), Task
/// (six statuses, four transitions, owners, agents, tags and git as the
/// Workflow Form frame draws them) and Subtask (five, two), and the agents
/// they name.
#[doc(hidden)]
pub mod fixture {
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

    /// The fixture project's YAML (Task the default), as a project named
    /// anything loads it.
    pub const PROJECT_YAML: &str = "name: My project\nrepo: ''\ndefault_branch: main\n\
         default_workflow: task\norchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    \
         command: claude\n    flags: []\nmachines:\n  - name: hub\n    kind: local\n    work_dir: \
         /tmp\nworkspaces:\n  - name: alpha\n    machine: hub\n";

    /// Write project `project` under `projects` (its YAML and
    /// [`write`]'s files).
    pub fn write_project(projects: &Path, project: &str) {
        std::fs::create_dir_all(projects).unwrap();
        std::fs::write(projects.join(format!("{project}.yaml")), PROJECT_YAML).unwrap();
        write(&projects.join(project));
    }

    /// Project `project` under `projects` as a Linear project (team ENG)
    /// whose catalog Linear manages ([`MANAGED_STATUSES`]): the Statuses
    /// section's read-only state.
    pub fn write_linear_project(projects: &Path, project: &str) {
        write_project(projects, project);
        let yaml = format!("{PROJECT_YAML}issue_tracker:\n  backend: linear\n  linear:\n    team: ENG\n");
        std::fs::write(projects.join(format!("{project}.yaml")), yaml).unwrap();
        std::fs::write(projects.join(project).join("workflows").join("statuses.yaml"), MANAGED_STATUSES).unwrap();
    }

    /// [`STATUSES`] as Linear's team ENG manages them.
    pub const MANAGED_STATUSES: &str = "\
managed_by: linear
statuses:
- id: backlog
  name: Backlog
  category: backlog
  external_id: state-backlog
- id: todo
  name: To Do
  category: ready
  external_id: state-todo
- id: in-progress
  name: In Progress
  category: active
  external_id: state-started
- id: review
  name: Review
  category: handoff
  external_id: state-review
- id: done
  name: Done
  category: done
  external_id: state-done
- id: canceled
  name: Canceled
  category: archived
  external_id: state-canceled
";

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
