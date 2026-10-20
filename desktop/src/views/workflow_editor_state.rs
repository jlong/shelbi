//! The workflow editor's state, toolkit-free (`dt-wf-editor-form`): one
//! workflow's draft over `shelbi_state::workflow_config`, the changes the
//! Form and the Builder (`dt-wf-editor-builder`) make to it, and Save,
//! Discard and the leave prompt. Both views edit this one draft, so an edit
//! in either shows in the other.
//!
//! The editor holds two models: the one loaded (the files as on disk) and
//! the draft. Every edit is a [`Change`] applied to the draft through the
//! model's API, so it is validated as it is made: a change the model refuses
//! leaves the draft as it was and shows the model's message at the field or
//! row it concerns ([`Field`]). Nothing is written until Save. Save first
//! applies what the text fields hold (a field whose text the model refused
//! holds it back, its error showing), then the typed name (a rename reads
//! the board, so it waits for Save and runs with the write, off the UI
//! thread), then writes the draft; Discard puts the loaded model back. Back
//! (or Esc), Manage project statuses and Duplicate ask to save or discard
//! unsaved edits first ([`Leave`]).
//!
//! The config the form maps onto (the frame's fields where they differ):
//!
//! - **Branch name** is `git.branch`, the full branch-name template
//!   (`{{github_user}}/{{id}}`). A workflow that names its branches by
//!   `git.branch_prefix` shows `<prefix>/{{id}}`, which keeps the prefix when
//!   saved unchanged; any other text becomes a `git.branch`. Empty fields
//!   and the project's merge strategy mean no `git:` block: the workflow
//!   inherits the project's, whose values the fields show as placeholders.
//! - **Review mode** is `review:`: Diff only is no block (a diff-only
//!   review), Run the branch is a `review:` recipe (`serve`, `setup`,
//!   `ready`, `url`, `workdir`). `serve` is required, so Save refuses Run the
//!   branch without it.
//! - **Zen settings** is `zen:`: Use project settings is no block. A
//!   workflow with one shows Custom, which the form doesn't edit (its
//!   checks, CI timeout and danger paths stay in the YAML); choosing Use
//!   project settings removes it.
//! - **Required task fields** is `required_params:`, and a transition's
//!   **Details** are its `target`, `run`, `ready`, `ready_timeout` and
//!   `url`.

use std::time::Duration;

use shelbi_core::{
    GitConfig, MergeStrategy, Owner, ProjectStatus, ReviewServe, Transition, TransitionAction, Workflow,
    WorkflowStatus,
};
use shelbi_state::workflow_config::{ConfigError, Saved, WorkflowConfig};

use super::workflows_state::sentence;

/// The transition actions, in the order the action picker lists them.
pub const ACTIONS: [TransitionAction; 6] = [
    TransitionAction::PushBranch,
    TransitionAction::OpenPr,
    TransitionAction::Merge,
    TransitionAction::ClosePr,
    TransitionAction::DeleteBranch,
    TransitionAction::Restack,
];

/// An action's name, as its chip shows it.
pub fn action_label(action: TransitionAction) -> &'static str {
    match action {
        TransitionAction::PushBranch => "Push branch",
        TransitionAction::OpenPr => "Open PR",
        TransitionAction::Merge => "Merge",
        TransitionAction::ClosePr => "Close PR",
        TransitionAction::DeleteBranch => "Delete branch",
        TransitionAction::Restack => "Restack",
    }
}

/// The merge strategies `git.merge_strategy` accepts.
pub const STRATEGIES: [MergeStrategy; 3] = [MergeStrategy::Squash, MergeStrategy::Merge, MergeStrategy::Rebase];

pub fn strategy_label(strategy: MergeStrategy) -> &'static str {
    match strategy {
        MergeStrategy::Squash => "Squash",
        MergeStrategy::Merge => "Merge",
        MergeStrategy::Rebase => "Rebase",
    }
}

pub fn owner_label(owner: Owner) -> &'static str {
    match owner {
        Owner::User => "User",
        Owner::Agent => "Agent",
    }
}

/// The view the editor shows over the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Builder,
    Form,
}

/// How the Review agent shows the branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    /// No `review:` recipe: the diff only.
    DiffOnly,
    /// A `review:` recipe boots the branch.
    Serve,
}

impl ReviewMode {
    pub fn label(self) -> &'static str {
        match self {
            ReviewMode::DiffOnly => "Diff only",
            ReviewMode::Serve => "Run the branch",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            ReviewMode::DiffOnly => "Show code changes without starting a development server.",
            ReviewMode::Serve => "Start the branch with these commands so the change can be tried. $SLOT is the review port.",
        }
    }
}

/// Where an error shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Name,
    Description,
    InitialStatus,
    /// A status's row.
    Status(String),
    /// The Statuses header (Add status).
    Statuses,
    /// Transition `n`'s row.
    Transition(usize),
    /// The Transitions header (Add transition).
    Transitions,
    Git,
    Review,
    Zen,
    RequiredParams,
    /// The titlebar: a save, a duplicate or a delete that failed.
    Save,
}

/// The Run the branch fields, as typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewForm {
    pub serve: String,
    pub setup: String,
    pub ready: String,
    pub url: String,
    pub workdir: String,
}

impl ReviewForm {
    pub fn of(review: Option<&ReviewServe>) -> ReviewForm {
        let Some(r) = review else {
            return ReviewForm::default();
        };
        ReviewForm {
            serve: r.serve.clone(),
            setup: r.setup.clone().unwrap_or_default(),
            ready: r.ready.clone().unwrap_or_default(),
            url: r.url.clone().unwrap_or_default(),
            workdir: r.workdir.clone().unwrap_or_default(),
        }
    }

    /// The recipe, or `None` without a serve command.
    fn recipe(&self) -> Option<ReviewServe> {
        let serve = non_empty(&self.serve)?;
        Some(ReviewServe {
            workdir: non_empty(&self.workdir),
            setup: non_empty(&self.setup),
            serve,
            ready: non_empty(&self.ready),
            url: non_empty(&self.url),
        })
    }
}

/// A transition's Details, as typed: its `run` commands one per line, its
/// `ready_timeout` in seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetailsForm {
    pub target: String,
    pub run: String,
    pub ready: String,
    pub ready_timeout: String,
    pub url: String,
}

impl DetailsForm {
    pub fn of(t: &Transition) -> DetailsForm {
        DetailsForm {
            target: t.target.clone().unwrap_or_default(),
            run: t.run.join("\n"),
            ready: t.ready.clone().unwrap_or_default(),
            ready_timeout: t.ready_timeout.map(|d| d.as_secs().to_string()).unwrap_or_default(),
            url: t.url.clone().unwrap_or_default(),
        }
    }

    /// `t` with these details.
    fn apply(&self, t: &Transition) -> Result<Transition, String> {
        let ready_timeout = match self.ready_timeout.trim() {
            "" => None,
            secs => Some(Duration::from_secs(
                secs.parse::<u64>()
                    .map_err(|_| "The ready timeout is a whole number of seconds.".to_string())?,
            )),
        };
        Ok(Transition {
            target: non_empty(&self.target),
            run: self.run.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect(),
            ready: non_empty(&self.ready),
            ready_timeout,
            url: non_empty(&self.url),
            ..t.clone()
        })
    }
}

/// The Git fields, as typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitForm {
    pub base_branch: String,
    pub branch: String,
    pub merge_strategy: MergeStrategy,
}

/// The project's `git:` values a workflow without its own inherits: the
/// Git fields' placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectGit {
    pub base_branch: String,
    pub branch: String,
    pub merge_strategy: MergeStrategy,
}

impl Default for ProjectGit {
    /// A project with no `git:` block: `main`, the GitHub login, squash.
    fn default() -> Self {
        ProjectGit {
            base_branch: "main".into(),
            branch: "{{github_user}}/{{id}}".into(),
            merge_strategy: MergeStrategy::Squash,
        }
    }
}

impl ProjectGit {
    /// `project`'s, read from its YAML (the defaults when it can't be).
    /// Does file IO.
    pub fn load(project: &str) -> ProjectGit {
        let Ok(p) = shelbi_state::load_project(project) else {
            return ProjectGit::default();
        };
        ProjectGit {
            base_branch: p.base_branch().to_string(),
            branch: p
                .git
                .branch
                .clone()
                .or_else(|| p.git.branch_prefix.as_deref().map(prefix_template))
                .unwrap_or_else(|| ProjectGit::default().branch),
            merge_strategy: p.merge_strategy(),
        }
    }
}

/// The branch template a `branch_prefix` names branches by.
fn prefix_template(prefix: &str) -> String {
    format!("{prefix}/{{{{id}}}}")
}

/// One edit the Form makes to the draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Description(String),
    InitialStatus(String),
    /// Add catalog status `id` at the end.
    AddStatus(String),
    RemoveStatus(String),
    /// Move status `id` to `index`.
    MoveStatus { id: String, index: usize },
    Owner { id: String, owner: Owner },
    Agent { id: String, agent: Option<String> },
    /// The Workspace tags field's text (comma or space separated).
    Tags { id: String, text: String },
    /// Add a transition between the first two statuses that have none.
    AddTransition,
    /// Add this transition (the Builder's New transition).
    InsertTransition(Transition),
    RemoveTransition(usize),
    From { index: usize, status: String },
    To { index: usize, status: String },
    AddAction { index: usize, action: TransitionAction },
    /// Replace transition `index`'s action at `slot`.
    SetAction { index: usize, slot: usize, action: TransitionAction },
    RemoveAction { index: usize, slot: usize },
    /// Move transition `index`'s action at `slot` to `to`.
    MoveAction { index: usize, slot: usize, to: usize },
    Details { index: usize, form: DetailsForm },
    Git(GitForm),
    ReviewMode(ReviewMode),
    Review(ReviewForm),
    /// The Required task fields text (comma or space separated).
    RequiredParams(String),
    /// Drop the workflow's `zen:` override.
    InheritZen,
    SetDefault,
}

impl Change {
    /// Where its error shows.
    pub fn field(&self) -> Field {
        match self {
            Change::Description(_) => Field::Description,
            Change::InitialStatus(_) => Field::InitialStatus,
            Change::AddStatus(_) | Change::SetDefault => Field::Statuses,
            Change::RemoveStatus(id)
            | Change::MoveStatus { id, .. }
            | Change::Owner { id, .. }
            | Change::Agent { id, .. }
            | Change::Tags { id, .. } => Field::Status(id.clone()),
            Change::AddTransition | Change::InsertTransition(_) => Field::Transitions,
            Change::RemoveTransition(index)
            | Change::From { index, .. }
            | Change::To { index, .. }
            | Change::AddAction { index, .. }
            | Change::SetAction { index, .. }
            | Change::RemoveAction { index, .. }
            | Change::MoveAction { index, .. }
            | Change::Details { index, .. } => Field::Transition(*index),
            Change::Git(_) => Field::Git,
            Change::ReviewMode(_) | Change::Review(_) => Field::Review,
            Change::RequiredParams(_) => Field::RequiredParams,
            Change::InheritZen => Field::Zen,
        }
    }
}

/// What to do once unsaved edits are saved or discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    /// Back to Settings ▸ Workflows.
    Back,
    /// Back to the Workflows page's Statuses.
    ManageStatuses,
    /// Duplicate the workflow (as saved) and edit the copy.
    Duplicate,
}

/// A save to run off the UI thread ([`run_save`]).
#[derive(Debug)]
pub struct SaveJob {
    config: WorkflowConfig,
    name: String,
    rename: Option<String>,
}

/// What a save did: the model as saved and the workflow's name, or the error
/// and where it shows.
pub type SaveOutcome = Result<(Saved, String), (Field, String)>;

/// Rename (when the name changed) and save, off the UI thread.
pub fn run_save(job: SaveJob) -> SaveOutcome {
    let mut config = job.config;
    let mut name = job.name;
    if let Some(new) = job.rename {
        config = config.rename_workflow(&name, &new).map_err(|e| (Field::Name, message(&e)))?;
        name = new;
    }
    let saved = config.save().map_err(|e| (Field::Save, message(&e)))?;
    Ok((saved, name))
}

/// Copy workflow `name` (as saved) as `copy` and save it, off the UI thread.
pub fn run_duplicate(base: WorkflowConfig, name: &str, copy: &str) -> Result<WorkflowConfig, String> {
    base.duplicate_workflow(name, copy)
        .and_then(|c| c.save())
        .map(|s| s.config)
        .map_err(|e| message(&e))
}

/// Delete workflow `name` and save, off the UI thread (it reads the board).
pub fn run_delete(base: WorkflowConfig, name: &str) -> Result<(), String> {
    base.delete_workflow(name).and_then(|c| c.save()).map(|_| ()).map_err(|e| message(&e))
}

fn message(e: &ConfigError) -> String {
    match e {
        ConfigError::Conflict(_) => format!("{} Discard to reload them.", sentence(&e.to_string())),
        e => sentence(&e.to_string()),
    }
}

/// The editor's state.
#[derive(Debug)]
pub struct EditorState {
    /// The workflow's name in the loaded model (its file stem).
    name: String,
    /// What the Name field holds; applied at Save.
    typed_name: String,
    /// The files as loaded, and the draft over them.
    base: WorkflowConfig,
    draft: WorkflowConfig,
    /// Whether a save would write anything (kept, as working it out
    /// patches the files' text).
    draft_dirty: bool,
    pub mode: Mode,
    review_mode: ReviewMode,
    /// The Run the branch fields as last typed (kept across Diff only).
    review_form: ReviewForm,
    project_git: ProjectGit,
    errors: Vec<(Field, String)>,
    /// Comments the last save dropped.
    notice: Option<String>,
    /// The leave prompt, while it asks.
    prompt: Option<Leave>,
    /// What to do once the save in flight lands.
    after_save: Option<Leave>,
    /// A save, a duplicate or a delete is running.
    busy: bool,
    /// The files changed on disk under the last save: Discard reloads.
    stale: bool,
}

impl EditorState {
    /// Edit workflow `name` of `config` (which has it).
    pub fn new(config: WorkflowConfig, name: &str) -> EditorState {
        let review = config.workflow(name).and_then(|w| w.review.clone());
        EditorState {
            name: name.to_string(),
            typed_name: name.to_string(),
            base: config.clone(),
            draft: config,
            draft_dirty: false,
            mode: Mode::Builder,
            review_mode: if review.is_some() { ReviewMode::Serve } else { ReviewMode::DiffOnly },
            review_form: ReviewForm::of(review.as_ref()),
            project_git: ProjectGit::default(),
            errors: Vec::new(),
            notice: None,
            prompt: None,
            after_save: None,
            busy: false,
            stale: false,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The draft of the workflow.
    pub fn workflow(&self) -> &Workflow {
        self.draft.workflow(&self.name).expect("the edited workflow is in its model")
    }

    pub fn set_project_git(&mut self, git: ProjectGit) {
        self.project_git = git;
    }

    pub fn project_git(&self) -> &ProjectGit {
        &self.project_git
    }

    pub fn is_default(&self) -> bool {
        self.draft.default_workflow() == self.name
    }

    /// Whether there is anything to save.
    pub fn is_dirty(&self) -> bool {
        self.draft_dirty || self.typed_name.trim() != self.name || self.review_pending()
    }

    /// Run the branch is chosen without a recipe in the draft yet.
    fn review_pending(&self) -> bool {
        self.review_mode == ReviewMode::Serve && self.workflow().review.is_none()
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn error_at(&self, field: &Field) -> Option<&str> {
        self.errors.iter().find(|(f, _)| f == field).map(|(_, e)| e.as_str())
    }

    #[cfg(test)]
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    fn set_error(&mut self, field: Field, message: String) {
        self.errors.retain(|(f, _)| *f != field);
        self.errors.push((field, message));
    }

    fn clear_error(&mut self, field: &Field) {
        self.errors.retain(|(f, _)| f != field);
    }

    /// Drop the error showing at `field` (the Builder's New transition,
    /// cancelled).
    pub fn clear_error_at(&mut self, field: &Field) {
        self.clear_error(field);
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn typed_name(&self) -> &str {
        &self.typed_name
    }

    /// The Name field changed.
    pub fn type_name(&mut self, text: &str) {
        self.typed_name = text.to_string();
        self.clear_error(&Field::Name);
    }

    // -----------------------------------------------------------------------
    // Reading the draft

    /// The agents the Agent picker offers: the project's, or (a project
    /// with no `agents/` directory, where any name goes) those its
    /// workflows name.
    pub fn agents(&self) -> Vec<String> {
        if let Some(known) = self.draft.agents() {
            return known.to_vec();
        }
        let mut names: Vec<String> = self
            .draft
            .workflows()
            .flat_map(|w| w.statuses.iter().filter_map(|s| s.agent.clone()))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The catalog statuses the workflow doesn't have (Add status).
    pub fn addable_statuses(&self) -> Vec<ProjectStatus> {
        let w = self.workflow();
        self.draft.statuses().iter().filter(|s| w.status(&s.id).is_none()).cloned().collect()
    }

    /// The status new tasks land in.
    pub fn initial_status(&self) -> Option<&WorkflowStatus> {
        let w = self.workflow();
        w.resolved_initial_status().and_then(|id| w.status(id))
    }

    pub fn status(&self, id: &str) -> Option<&WorkflowStatus> {
        self.workflow().status(id)
    }

    pub fn transitions(&self) -> &[Transition] {
        self.workflow().transitions.as_deref().unwrap_or(&[])
    }

    /// Status `id`'s Workspace tags field text.
    pub fn tags_text(&self, id: &str) -> String {
        self.status(id).map(|s| s.tags.join(", ")).unwrap_or_default()
    }

    pub fn params_text(&self) -> String {
        self.workflow().required_params.join(", ")
    }

    pub fn git_form(&self) -> GitForm {
        let git = self.workflow().git.as_ref();
        GitForm {
            base_branch: git.and_then(|g| g.base_branch.clone()).unwrap_or_default(),
            branch: git
                .and_then(|g| g.branch.clone().or_else(|| g.branch_prefix.as_deref().map(prefix_template)))
                .unwrap_or_default(),
            merge_strategy: git.map_or(self.project_git.merge_strategy, |g| g.merge_strategy),
        }
    }

    pub fn review_mode(&self) -> ReviewMode {
        self.review_mode
    }

    pub fn review_form(&self) -> &ReviewForm {
        &self.review_form
    }

    pub fn details_form(&self, index: usize) -> DetailsForm {
        self.transitions().get(index).map(DetailsForm::of).unwrap_or_default()
    }

    /// Whether transition `index` sets anything Details shows.
    pub fn has_details(&self, index: usize) -> bool {
        self.transitions().get(index).is_some_and(|t| DetailsForm::of(t) != DetailsForm::default())
    }

    /// The Zen settings picker's choice: `None` inherits the project's,
    /// else what the workflow's `zen:` overrides.
    pub fn zen_override(&self) -> Option<String> {
        let zen = self.workflow().zen.as_ref().filter(|z| !z.is_empty())?;
        let mut parts = Vec::new();
        if zen.checks.is_some() {
            parts.push("checks");
        }
        if zen.ci_timeout.is_some() {
            parts.push("CI timeout");
        }
        if zen.danger_paths.is_some() {
            parts.push("protected paths");
        }
        Some(parts.join(", "))
    }

    /// Why the workflow can't be deleted, known without reading the board.
    pub fn delete_blocked(&self) -> Option<String> {
        if self.base.default_workflow() == self.name {
            Some("The default workflow can’t be deleted. Make another the default first.".into())
        } else if self.base.workflows().count() <= 1 {
            Some("The only workflow can’t be deleted.".into())
        } else {
            None
        }
    }

    /// The name a duplicate gets: `<name>-copy`, then `-copy-2`, … .
    pub fn duplicate_name(&self) -> String {
        let base = format!("{}-copy", self.name);
        let taken = |name: &str| self.base.workflow(name).is_some();
        let mut name = base.clone();
        let mut n = 2;
        while taken(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }

    // -----------------------------------------------------------------------
    // Editing

    /// Make `change` to the draft. A refusal leaves the draft as it was
    /// and shows at the change's field. Returns whether it was made.
    pub fn change(&mut self, change: Change) -> bool {
        let field = change.field();
        self.clear_error(&field);
        self.notice = None;
        match &change {
            Change::ReviewMode(mode) => self.review_mode = *mode,
            Change::Review(form) => self.review_form = form.clone(),
            _ => {}
        }
        match self.apply(&change) {
            Ok(next) => {
                self.draft = next;
                self.draft_dirty = self.draft.is_dirty();
                true
            }
            Err(e) => {
                self.set_error(field, e);
                false
            }
        }
    }

    fn apply(&self, change: &Change) -> Result<WorkflowConfig, String> {
        let (c, w) = (&self.draft, self.name.as_str());
        let made = match change {
            Change::Description(text) => c.set_description(w, Some(text)),
            Change::InitialStatus(id) => c.set_initial_status(w, Some(id)),
            Change::AddStatus(id) => c.add_workflow_status(w, id, None),
            Change::RemoveStatus(id) => c.remove_workflow_status(w, id),
            Change::MoveStatus { id, index } => c.move_workflow_status(w, id, *index),
            Change::Owner { id, owner } => c.set_status_owner(w, id, *owner),
            Change::Agent { id, agent } => c.set_status_agent(w, id, agent.as_deref()),
            Change::Tags { id, text } => c.set_status_tags(w, id, &split_list(text)),
            Change::AddTransition => {
                let t = self.next_transition().ok_or("Every pair of statuses already has a transition.")?;
                c.add_transition(w, t)
            }
            Change::InsertTransition(t) => c.add_transition(w, t.clone()),
            Change::RemoveTransition(index) => c.delete_transition(w, *index),
            Change::From { index, status } => return self.edit_transition(*index, |t| t.from = status.clone()),
            Change::To { index, status } => return self.edit_transition(*index, |t| t.to = status.clone()),
            Change::AddAction { index, action } => return self.edit_transition(*index, |t| t.actions.push(*action)),
            Change::SetAction { index, slot, action } => {
                return self.edit_transition(*index, |t| {
                    if let Some(a) = t.actions.get_mut(*slot) {
                        *a = *action;
                    }
                })
            }
            Change::RemoveAction { index, slot } => {
                return self.edit_transition(*index, |t| {
                    if *slot < t.actions.len() {
                        t.actions.remove(*slot);
                    }
                })
            }
            Change::MoveAction { index, slot, to } => {
                return self.edit_transition(*index, |t| {
                    if *slot < t.actions.len() && *to < t.actions.len() {
                        let a = t.actions.remove(*slot);
                        t.actions.insert(*to, a);
                    }
                })
            }
            Change::Details { index, form } => {
                let t = self.transitions().get(*index).ok_or("No such transition.")?;
                let t = form.apply(t)?;
                c.update_transition(w, *index, t)
            }
            Change::Git(form) => c.set_git(w, self.git_from(form)),
            Change::ReviewMode(_) | Change::Review(_) => {
                let recipe = match self.review_mode {
                    ReviewMode::DiffOnly => None,
                    ReviewMode::Serve => self.review_form.recipe(),
                };
                c.set_review(w, recipe)
            }
            Change::RequiredParams(text) => c.set_required_params(w, split_list(text)),
            Change::InheritZen => c.update_workflow(w, |wf| wf.zen = None),
            Change::SetDefault => c.set_default_workflow(w),
        };
        made.map_err(|e| message(&e))
    }

    /// The draft with transition `index` edited.
    fn edit_transition(&self, index: usize, edit: impl FnOnce(&mut Transition)) -> Result<WorkflowConfig, String> {
        let mut t = self.transitions().get(index).ok_or("No such transition.")?.clone();
        edit(&mut t);
        self.draft.update_transition(&self.name, index, t).map_err(|e| message(&e))
    }

    /// A transition for Add transition: the first pair of neighboring
    /// statuses without one, else the first pair of any.
    fn next_transition(&self) -> Option<Transition> {
        let w = self.workflow();
        let ids: Vec<&str> = w.statuses.iter().map(|s| s.id.as_str()).collect();
        let taken = |from: &str, to: &str| self.transitions().iter().any(|t| t.from == from && t.to == to);
        let neighbors = ids.windows(2).map(|p| (p[0], p[1]));
        let any = ids.iter().flat_map(|&a| ids.iter().filter(move |&&b| b != a).map(move |&b| (a, b)));
        let (from, to) = neighbors.chain(any).find(|(a, b)| !taken(a, b))?;
        Some(Transition {
            from: from.to_string(),
            to: to.to_string(),
            actions: Vec::new(),
            target: None,
            run: Vec::new(),
            ready: None,
            ready_timeout: None,
            url: None,
        })
    }

    /// The `git:` block the Git fields make: none when they're empty and
    /// the strategy is the project's.
    fn git_from(&self, form: &GitForm) -> Option<GitConfig> {
        let prefix = self.workflow().git.as_ref().and_then(|g| g.branch_prefix.clone());
        let (branch, branch_prefix) = match non_empty(&form.branch) {
            None => (None, None),
            Some(t) => match prefix {
                Some(p) if t == prefix_template(&p) => (None, Some(p)),
                _ => (Some(t), None),
            },
        };
        let git = GitConfig {
            base_branch: non_empty(&form.base_branch),
            branch,
            branch_prefix,
            merge_strategy: form.merge_strategy,
        };
        let inherits = git.base_branch.is_none()
            && git.branch.is_none()
            && git.branch_prefix.is_none()
            && git.merge_strategy == self.project_git.merge_strategy;
        (!inherits).then_some(git)
    }

    // -----------------------------------------------------------------------
    // Save, Discard, leaving

    /// Start a save: apply `texts` (what the text fields hold, each made
    /// only if it differs from the draft), then hand back the save to run.
    /// `None` when there's nothing to save, one is running, or a field's
    /// text was refused (its error shows).
    pub fn begin_save(&mut self, texts: Vec<Change>) -> Option<SaveJob> {
        if self.busy {
            return None;
        }
        self.clear_error(&Field::Save);
        let mut refused = false;
        for change in texts {
            if self.differs(&change) && !self.change(change) {
                refused = true;
            }
        }
        if self.review_pending() {
            self.set_error(
                Field::Review,
                "Enter the command that starts the server, or choose Diff only.".into(),
            );
            refused = true;
        }
        let typed = self.typed_name.trim().to_string();
        if refused || !self.is_dirty() {
            self.after_save = None;
            return None;
        }
        self.busy = true;
        Some(SaveJob {
            config: self.draft.clone(),
            name: self.name.clone(),
            rename: (typed != self.name).then_some(typed),
        })
    }

    /// Whether a text field's change would change the draft.
    pub fn differs(&self, change: &Change) -> bool {
        let w = self.workflow();
        match change {
            Change::Description(text) => w.description.as_deref().unwrap_or("") != text.trim(),
            Change::Tags { id, text } => self.status(id).is_some_and(|s| s.tags != split_list(text)),
            Change::Details { index, form } => self.transitions().get(*index).map(DetailsForm::of).as_ref() != Some(form),
            Change::Git(form) => self.git_form() != *form,
            Change::Review(form) => self.review_form != *form,
            Change::RequiredParams(text) => w.required_params != split_list(text),
            _ => true,
        }
    }

    /// A save landed: take its model (the draft and the loaded model are
    /// now the files), or show its error. Hands back what to do next, if
    /// the leave prompt's Save started it.
    pub fn saved(&mut self, outcome: SaveOutcome) -> Option<Leave> {
        self.busy = false;
        match outcome {
            Ok((saved, name)) => {
                let dropped: Vec<String> = saved
                    .dropped_comments
                    .iter()
                    .flat_map(|(path, lines)| {
                        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                        lines.iter().map(move |l| format!("{file}: {}", l.trim()))
                    })
                    .collect();
                let then = self.after_save.take();
                self.reset(saved.config, &name);
                if !dropped.is_empty() {
                    self.notice = Some(format!("The save dropped these comments: {}", dropped.join("; ")));
                }
                then
            }
            Err((field, e)) => {
                self.stale = e.starts_with("Changed on disk");
                self.set_error(field, e);
                self.after_save = None;
                None
            }
        }
    }

    /// Edit `name` of `config` from here on, as loaded.
    fn reset(&mut self, config: WorkflowConfig, name: &str) {
        let mode = self.mode;
        let git = std::mem::take(&mut self.project_git);
        *self = EditorState::new(config, name);
        self.mode = mode;
        self.project_git = git;
    }

    /// Put the loaded model back. Returns whether the files must be read
    /// again (they changed under the last save): [`EditorState::reloaded`].
    pub fn discard(&mut self) -> bool {
        let stale = self.stale;
        let base = self.base.clone();
        let name = self.name.clone();
        self.reset(base, &name);
        stale
    }

    /// The files, read again after a discard. `false` when the workflow is
    /// gone from them.
    pub fn reloaded(&mut self, config: WorkflowConfig) -> bool {
        if config.workflow(&self.name).is_none() {
            return false;
        }
        let name = self.name.clone();
        self.reset(config, &name);
        true
    }

    /// Ask to leave for `then`: `Some(then)` to go now, or `None` while the
    /// prompt asks to save or discard the unsaved edits first.
    pub fn leave(&mut self, then: Leave) -> Option<Leave> {
        if self.is_dirty() {
            self.prompt = Some(then);
            None
        } else {
            Some(then)
        }
    }

    pub fn prompt(&self) -> Option<Leave> {
        self.prompt
    }

    pub fn cancel_prompt(&mut self) {
        self.prompt = None;
    }

    /// The prompt's Discard: drop the edits and go.
    pub fn discard_and_leave(&mut self) -> Option<Leave> {
        let then = self.prompt.take();
        self.discard();
        then
    }

    /// The prompt's Save: save, then go once it lands ([`EditorState::saved`]).
    pub fn save_and_leave(&mut self, texts: Vec<Change>) -> Option<SaveJob> {
        self.after_save = self.prompt.take();
        self.begin_save(texts)
    }

    /// Start a duplicate or a delete (on the model as saved). `None` while
    /// one is running.
    pub fn begin_job(&mut self) -> Option<WorkflowConfig> {
        if self.busy {
            return None;
        }
        self.clear_error(&Field::Save);
        self.busy = true;
        Some(self.base.clone())
    }

    /// A duplicate or a delete failed.
    pub fn job_failed(&mut self, e: String) {
        self.busy = false;
        self.set_error(Field::Save, e);
    }
}

/// `text`'s items: split on commas and white space.
pub fn split_list(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn non_empty(text: &str) -> Option<String> {
    let t = text.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::workflows_state::tests::{Home, PROJECT};

    fn editor(name: &str) -> EditorState {
        EditorState::new(WorkflowConfig::load(PROJECT).unwrap(), name)
    }

    /// Save as the view does, here rather than off the UI thread.
    fn save(state: &mut EditorState, texts: Vec<Change>) -> Option<Leave> {
        let job = state.begin_save(texts)?;
        state.saved(run_save(job))
    }

    fn on_disk(name: &str) -> Workflow {
        WorkflowConfig::load(PROJECT).unwrap().workflow(name).unwrap().clone()
    }

    #[test]
    fn edits_stay_in_the_draft_until_save_and_discard_reverts_them() {
        let home = Home::new("ed-draft");
        let before = home.read("task.yaml");
        let mut state = editor("task");
        assert!(!state.is_dirty());
        assert!(state.change(Change::Description("Shipped work.".into())));
        assert!(state.is_dirty());
        assert_eq!(home.read("task.yaml"), before, "nothing written yet");

        state.discard();
        assert!(!state.is_dirty());
        assert_eq!(state.workflow().description.as_deref(), Some("Branch work with a review gate."));

        state.change(Change::Description("Shipped work.".into()));
        save(&mut state, vec![]);
        assert!(!state.is_dirty());
        assert_eq!(on_disk("task").description.as_deref(), Some("Shipped work."));
        let text = home.read("task.yaml");
        assert!(text.contains("## Statuses run left to right."), "comments kept:\n{text}");
    }

    #[test]
    fn undoing_an_edit_by_hand_leaves_nothing_to_save() {
        let _home = Home::new("ed-undo");
        let mut state = editor("task");
        state.change(Change::Owner { id: "backlog".into(), owner: Owner::Agent });
        assert!(state.is_dirty());
        state.change(Change::Owner { id: "backlog".into(), owner: Owner::User });
        assert!(!state.is_dirty());
        assert!(state.begin_save(vec![]).is_none(), "nothing to save");
    }

    #[test]
    fn status_edits_add_remove_reorder_and_assign() {
        let home = Home::new("ed-statuses");
        std::fs::write(
            home.workflows().join("statuses.yaml"),
            format!("{}- id: qa\n  name: QA\n  category: handoff\n", crate::views::workflows_state::fixture::STATUSES),
        )
        .unwrap();
        let mut state = editor("task");
        assert_eq!(state.addable_statuses().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["qa"]);
        assert!(state.change(Change::AddStatus("qa".into())));
        assert!(state.change(Change::MoveStatus { id: "qa".into(), index: 4 }));
        assert!(state.change(Change::Agent { id: "qa".into(), agent: Some("review".into()) }));
        assert!(state.change(Change::Owner { id: "qa".into(), owner: Owner::Agent }));
        assert!(state.change(Change::Tags { id: "qa".into(), text: "review, gpu  mac".into() }));
        assert!(state.change(Change::RemoveStatus("canceled".into())));
        save(&mut state, vec![]);

        let w = on_disk("task");
        let ids: Vec<_> = w.statuses.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["backlog", "todo", "in-progress", "review", "qa", "done"]);
        let qa = w.status("qa").unwrap();
        assert_eq!((qa.owner, qa.agent.as_deref()), (Owner::Agent, Some("review")));
        assert_eq!(qa.tags, ["review", "gpu", "mac"]);
        let transitions = w.transitions.as_ref().unwrap();
        assert!(transitions.iter().all(|t| t.to != "canceled"), "removing a status takes its transitions");
    }

    #[test]
    fn an_owner_without_an_agent_is_refused_at_its_row() {
        let _home = Home::new("ed-owner");
        let mut state = editor("task");
        assert!(!state.change(Change::Owner { id: "done".into(), owner: Owner::Agent }));
        let error = state.error_at(&Field::Status("done".into())).unwrap();
        assert!(error.contains("owner: agent but no agent"), "{error}");
        assert_eq!(state.status("done").unwrap().owner, Owner::User, "the draft is as it was");
        assert!(!state.is_dirty());
        // An agent from outside the project's is refused too.
        assert!(!state.change(Change::Agent { id: "done".into(), agent: Some("nobody".into()) }));
        assert!(state.error_at(&Field::Status("done".into())).unwrap().contains("doesn't exist"));
    }

    #[test]
    fn transition_edits_and_their_actions() {
        let _home = Home::new("ed-transitions");
        let mut state = editor("task");
        assert_eq!(state.transitions().len(), 4);
        assert!(state.change(Change::AddTransition));
        let added = state.transitions()[4].clone();
        assert_eq!((added.from.as_str(), added.to.as_str()), ("backlog", "todo"), "the first free neighbors");
        assert!(state.change(Change::To { index: 4, status: "in-progress".into() }));
        assert!(state.change(Change::AddAction { index: 4, action: TransitionAction::PushBranch }));
        assert!(state.change(Change::AddAction { index: 4, action: TransitionAction::OpenPr }));
        assert!(state.change(Change::MoveAction { index: 4, slot: 1, to: 0 }));
        assert!(state.change(Change::SetAction { index: 4, slot: 1, action: TransitionAction::Restack }));
        assert!(state.change(Change::RemoveAction { index: 0, slot: 1 }));
        assert!(state.change(Change::RemoveTransition(3)));
        // The same move twice is refused.
        assert!(!state.change(Change::To { index: 0, status: "canceled".into() }));
        assert!(state.error_at(&Field::Transition(0)).unwrap().contains("duplicate transition"));
        save(&mut state, vec![]);

        let ts = on_disk("task").transitions.unwrap();
        assert_eq!(ts.len(), 4);
        assert_eq!(ts[0].actions, [TransitionAction::PushBranch]);
        assert_eq!((ts[3].from.as_str(), ts[3].to.as_str()), ("backlog", "in-progress"));
        assert_eq!(ts[3].actions, [TransitionAction::OpenPr, TransitionAction::Restack]);
    }

    #[test]
    fn the_builders_new_transition_goes_in_whole_and_a_duplicate_is_refused() {
        let _home = Home::new("ed-insert");
        let mut state = editor("task");
        let t = Transition {
            from: "in-progress".into(),
            to: "done".into(),
            actions: vec![TransitionAction::PushBranch, TransitionAction::Merge],
            target: None,
            run: Vec::new(),
            ready: None,
            ready_timeout: None,
            url: None,
        };
        assert!(state.change(Change::InsertTransition(t.clone())));
        assert_eq!(state.transitions().last(), Some(&t));
        assert!(!state.change(Change::InsertTransition(t)));
        assert!(state.error_at(&Field::Transitions).unwrap().contains("duplicate transition"));
        state.clear_error_at(&Field::Transitions);
        assert!(!state.has_errors());
        save(&mut state, vec![]);
        assert_eq!(on_disk("task").transitions.unwrap().len(), 5);
    }

    #[test]
    fn details_write_the_transitions_other_keys() {
        let _home = Home::new("ed-details");
        let mut state = editor("task");
        assert!(!state.has_details(0));
        let form = DetailsForm {
            target: "develop".into(),
            run: "npm ci\n\nnpm run dev -- -p $SLOT &\n".into(),
            ready: "curl -sf localhost:$SLOT".into(),
            ready_timeout: "soon".into(),
            url: "http://localhost:$SLOT".into(),
        };
        assert!(!state.change(Change::Details { index: 0, form: form.clone() }));
        assert!(state.error_at(&Field::Transition(0)).unwrap().contains("whole number of seconds"));
        let form = DetailsForm { ready_timeout: "120".into(), ..form };
        assert!(state.change(Change::Details { index: 0, form }));
        assert!(state.has_details(0));
        save(&mut state, vec![]);
        let t = on_disk("task").transitions.unwrap()[0].clone();
        assert_eq!(t.target.as_deref(), Some("develop"));
        assert_eq!(t.run, ["npm ci", "npm run dev -- -p $SLOT &"]);
        assert_eq!(t.ready_timeout, Some(Duration::from_secs(120)));
        assert_eq!(t.url.as_deref(), Some("http://localhost:$SLOT"));
    }

    #[test]
    fn git_fields_map_to_the_git_block() {
        let home = Home::new("ed-git");
        let mut state = editor("task");
        let form = state.git_form();
        assert_eq!((form.base_branch.as_str(), form.branch.as_str()), ("main", "{{github_user}}/{{id}}"));
        assert!(state.change(Change::Git(GitForm { merge_strategy: MergeStrategy::Rebase, ..form })));
        save(&mut state, vec![]);
        let git = on_disk("task").git.unwrap();
        assert_eq!(git.merge_strategy, MergeStrategy::Rebase);
        assert_eq!(git.branch.as_deref(), Some("{{github_user}}/{{id}}"));

        // Emptied, with the project's strategy: no block, so the project's
        // apply.
        assert!(state.change(Change::Git(GitForm {
            base_branch: String::new(),
            branch: String::new(),
            merge_strategy: MergeStrategy::Squash,
        })));
        save(&mut state, vec![]);
        assert_eq!(on_disk("task").git, None);
        assert!(!home.read("task.yaml").contains("git:"));

        // A placeholder in the base branch needs a required field.
        let templated = GitForm {
            base_branch: "feature/{{feature}}".into(),
            branch: String::new(),
            merge_strategy: MergeStrategy::Squash,
        };
        assert!(!state.change(Change::Git(templated.clone())));
        assert!(state.error_at(&Field::Git).unwrap().contains("required task field"));
        assert!(state.change(Change::RequiredParams("feature".into())));
        assert!(state.change(Change::Git(templated)));
    }

    #[test]
    fn a_branch_prefix_round_trips_through_the_branch_name() {
        let home = Home::new("ed-prefix");
        let text = home.read("task.yaml").replace("  branch: '{{github_user}}/{{id}}'\n", "  branch_prefix: team\n");
        std::fs::write(home.workflows().join("task.yaml"), text).unwrap();
        let mut state = editor("task");
        let form = state.git_form();
        assert_eq!(form.branch, "team/{{id}}");
        assert!(state.change(Change::Git(GitForm { base_branch: "develop".into(), ..form.clone() })));
        save(&mut state, vec![]);
        let git = on_disk("task").git.unwrap();
        assert_eq!((git.branch_prefix.as_deref(), git.branch.as_deref()), (Some("team"), None));
        assert!(state.change(Change::Git(GitForm { branch: "{{id}}".into(), ..state.git_form() })));
        save(&mut state, vec![]);
        let git = on_disk("task").git.unwrap();
        assert_eq!((git.branch_prefix.as_deref(), git.branch.as_deref()), (None, Some("{{id}}")));
    }

    #[test]
    fn review_mode_needs_a_serve_command_at_save() {
        let _home = Home::new("ed-review");
        let mut state = editor("task");
        assert_eq!(state.review_mode(), ReviewMode::DiffOnly);
        state.change(Change::ReviewMode(ReviewMode::Serve));
        assert!(state.is_dirty(), "a mode to save");
        assert!(state.begin_save(vec![]).is_none());
        assert!(state.error_at(&Field::Review).unwrap().contains("starts the server"));

        let form = ReviewForm {
            serve: "npm run dev -- -p $SLOT".into(),
            url: "http://localhost:$SLOT".into(),
            ..ReviewForm::default()
        };
        save(&mut state, vec![Change::Review(form)]);
        assert!(!state.has_errors());
        let review = on_disk("task").review.unwrap();
        assert_eq!(review.serve, "npm run dev -- -p $SLOT");
        assert_eq!(review.setup, None);

        state.change(Change::ReviewMode(ReviewMode::DiffOnly));
        save(&mut state, vec![]);
        assert_eq!(on_disk("task").review, None);
    }

    #[test]
    fn initial_status_params_default_and_zen() {
        let home = Home::new("ed-misc");
        let text = format!("{}zen:\n  ci_timeout: 600\nx-notes: keep me\n", home.read("subtask.yaml"));
        std::fs::write(home.workflows().join("subtask.yaml"), text).unwrap();
        let mut state = editor("subtask");
        assert_eq!(state.initial_status().unwrap().id, "backlog");
        assert_eq!(state.zen_override().as_deref(), Some("CI timeout"));
        assert!(!state.is_default());
        state.change(Change::InitialStatus("todo".into()));
        state.change(Change::SetDefault);
        assert!(state.is_default());
        save(&mut state, vec![Change::RequiredParams("feature, team".into())]);
        let w = on_disk("subtask");
        assert_eq!(w.initial_status.as_deref(), Some("todo"));
        assert_eq!(w.required_params, ["feature", "team"]);
        assert_eq!(WorkflowConfig::load(PROJECT).unwrap().default_workflow(), "subtask");
        let text = home.read("subtask.yaml");
        assert!(text.contains("x-notes: keep me") && text.contains("ci_timeout: 600"), "keys the form doesn't show stay:\n{text}");

        state.change(Change::InheritZen);
        save(&mut state, vec![]);
        assert_eq!(on_disk("subtask").zen, None);
        assert!(home.read("subtask.yaml").contains("x-notes: keep me"));
    }

    #[test]
    fn a_rename_waits_for_save_and_a_bad_name_shows_at_the_field() {
        let home = Home::new("ed-rename");
        let mut state = editor("subtask");
        state.type_name("task");
        assert!(state.is_dirty());
        save(&mut state, vec![]);
        assert!(state.error_at(&Field::Name).unwrap().contains("already exists"));
        assert_eq!(state.name(), "subtask");

        state.type_name("child");
        save(&mut state, vec![]);
        assert_eq!(state.name(), "child");
        assert!(!state.is_dirty());
        assert!(home.workflows().join("child.yaml").exists());
        assert!(!home.workflows().join("subtask.yaml").exists());
    }

    #[test]
    fn a_refused_text_field_holds_the_save_back() {
        let home = Home::new("ed-held");
        let before = home.read("task.yaml");
        let mut state = editor("task");
        state.change(Change::Description("New words.".into()));
        let detail = DetailsForm { ready_timeout: "x".into(), ..DetailsForm::default() };
        assert!(state.begin_save(vec![Change::Details { index: 1, form: detail }]).is_none());
        assert!(state.error_at(&Field::Transition(1)).is_some());
        assert_eq!(home.read("task.yaml"), before, "nothing written");
        assert!(state.is_dirty(), "the description is still to save");
    }

    #[test]
    fn leaving_with_edits_asks_first() {
        let _home = Home::new("ed-leave");
        let mut state = editor("task");
        assert_eq!(state.leave(Leave::Back), Some(Leave::Back), "nothing to lose");
        state.change(Change::Description("Changed.".into()));
        assert_eq!(state.leave(Leave::Back), None);
        assert_eq!(state.prompt(), Some(Leave::Back));
        state.cancel_prompt();
        assert_eq!(state.prompt(), None);
        assert!(state.is_dirty(), "Cancel keeps the edits");

        state.leave(Leave::ManageStatuses);
        assert_eq!(state.discard_and_leave(), Some(Leave::ManageStatuses));
        assert!(!state.is_dirty());

        state.change(Change::Description("Saved this time.".into()));
        state.leave(Leave::Back);
        let job = state.save_and_leave(vec![]).unwrap();
        assert_eq!(state.saved(run_save(job)), Some(Leave::Back));
        assert_eq!(on_disk("task").description.as_deref(), Some("Saved this time."));
    }

    #[test]
    fn a_file_changed_under_the_save_is_kept_and_discard_reloads() {
        let home = Home::new("ed-conflict");
        let mut state = editor("task");
        let edited = home.read("task.yaml").replace("Branch work", "Hand-edited work");
        std::fs::write(home.workflows().join("task.yaml"), &edited).unwrap();
        state.change(Change::Description("Mine.".into()));
        save(&mut state, vec![]);
        assert!(state.error_at(&Field::Save).unwrap().starts_with("Changed on disk"));
        assert_eq!(home.read("task.yaml"), edited, "the hand edit stands");
        assert!(state.discard(), "the files must be read again");
        assert!(state.reloaded(WorkflowConfig::load(PROJECT).unwrap()));
        assert!(state.workflow().description.as_deref().unwrap().starts_with("Hand-edited"));
    }

    #[test]
    fn duplicate_and_delete_run_on_the_saved_model() {
        let home = Home::new("ed-dup");
        let mut state = editor("subtask");
        assert_eq!(state.duplicate_name(), "subtask-copy");
        let base = state.begin_job().unwrap();
        let copy = run_duplicate(base, "subtask", "subtask-copy").unwrap();
        assert!(copy.workflow("subtask-copy").is_some());
        assert!(home.workflows().join("subtask-copy.yaml").exists());

        let mut state = EditorState::new(copy, "subtask-copy");
        assert_eq!(state.delete_blocked(), None);
        let base = state.begin_job().unwrap();
        run_delete(base, "subtask-copy").unwrap();
        assert!(!home.workflows().join("subtask-copy.yaml").exists());
        assert!(editor("task").delete_blocked().is_some_and(|e| e.contains("default")));
    }
}
