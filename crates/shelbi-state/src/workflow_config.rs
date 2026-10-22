//! The editing model behind Settings ▸ Workflows: a project's status
//! catalog (`workflows/statuses.yaml`), its workflow files
//! (`workflows/<name>.yaml`) and its default workflow (`default_workflow:`
//! in the project YAML), loaded into one value that edits, validates and
//! saves them. Views never touch the YAML.
//!
//! [`WorkflowConfig::load`] reads everything. Each edit operation takes
//! `&self` and returns a validated new model or a [`ConfigError`] that says
//! what's wrong in the user's terms, so a view can hold the last good model
//! and show the error. Nothing is written until [`WorkflowConfig::save`],
//! which writes only the files whose meaning changed, through
//! [`yaml_edit`](crate::yaml_edit) so comments, help blocks, unknown keys
//! and the author's formatting survive. A save re-reads each file it is
//! about to write (or remove) and refuses, writing nothing, if any changed
//! on disk since the load.
//!
//! Validation matches the loader ([`crate::load_workflow`]): the catalog
//! needs unique non-empty ids and a terminal (done/archived) status; a
//! workflow's statuses must be in the catalog, unique, and `owner: agent`
//! ones need an `agent:`; transitions and `initial_status` must name the
//! workflow's statuses; agents must exist under `agents/` (when the project
//! has that directory, as the loader checks). An edit validates what it
//! touched, so one workflow that's already broken on disk doesn't block
//! edits to the others.
//!
//! The hub and the clients read workflow config from disk on every use
//! (`list_workflows` / `load_workflow` / `load_project_statuses`; nothing
//! caches it), so a save is picked up exactly like a hand edit: on the
//! next board refresh, dispatch, or move.
//!
//! ## The API, by screen
//!
//! - **Index.** [`workflow_summaries`](WorkflowConfig::workflow_summaries)
//!   (status and transition counts, default flag, description) and
//!   [`status_summaries`](WorkflowConfig::status_summaries) (with "Used
//!   in"). Edit YAML opens [`statuses_path`](WorkflowConfig::statuses_path).
//!   Files the model can't parse are in
//!   [`unreadable`](WorkflowConfig::unreadable), never written.
//! - **Statuses.** `add_status` / `rename_status` / `set_status_category` /
//!   `move_status` / `delete_status`.
//! - **Workflows.** `create_workflow` / `duplicate_workflow` /
//!   `rename_workflow` / `set_description` / `set_default_workflow` /
//!   `delete_workflow`, with [`check_rename`](WorkflowConfig::check_rename)
//!   and [`check_delete`](WorkflowConfig::check_delete) to disable those two
//!   actions up front.
//! - **Form and builder.** `add_workflow_status` / `remove_workflow_status`
//!   / `move_workflow_status`, `set_status_owner` / `set_status_agent` /
//!   `set_status_assignment` / `set_status_tags`, `set_initial_status`,
//!   `add_transition` / `update_transition` / `move_transition` /
//!   `delete_transition` (a [`Transition`] carries the details: `target`,
//!   `run`, `ready`, `ready_timeout`, `url`), `set_git`,
//!   `set_required_params`, `set_review` (`None` is a diff-only review), and
//!   `update_workflow` for anything else. [`agents`](WorkflowConfig::agents)
//!   feeds the agent picker.
//! - **Save / Discard.** [`is_dirty`](WorkflowConfig::is_dirty), then
//!   [`save`](WorkflowConfig::save) (which returns the reloaded model and
//!   any comments a file lost); discarding is dropping the edited value.
//!
//! ## Decisions
//!
//! - A status's id never changes; renaming edits `name:` only. A new
//!   status's id is the kebab-case of its name, suffixed `-2`, `-3`, … to be
//!   unique.
//! - A workflow's name is its file stem (the loader resolves by stem), so
//!   a rename moves the file and the project's `default_workflow:`, the only
//!   config that names a workflow. Issues name workflows too, so a rename or
//!   delete is refused while open issues use the name (they'd silently fall
//!   back to the default); terminal issues may keep it.
//! - Taking a status out of a workflow takes the transitions that touch it
//!   and clears an `initial_status` naming it. A status added to a workflow
//!   is `owner: user` with no agent until the user picks one.
//! - A new workflow lists every catalog status, with the shipped default's
//!   owner and agent where it has that status, and no transitions or git
//!   override. A duplicate starts from the source file's text, comments and
//!   unknown keys included.
//! - Save order keeps every intermediate state loadable: a catalog that
//!   only grows is written before the workflows, one that shrinks after
//!   them, and the default moves once the renamed file exists. Each file is
//!   written atomically; the set isn't one transaction.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_yaml::Value;
use shelbi_core::{
    default_project_statuses, default_workflow, validate_workflow_name, GitConfig, Owner,
    ProjectStatus, ProjectStatuses, ReviewServe, StatusCategory, Transition, Workflow,
    WorkflowStatus,
};

use crate::yaml_edit;

/// How many blocking issue ids a [`ConfigError::WorkflowInUse`] names.
const RENAME_BLOCKERS_SHOWN: usize = 10;

/// `tags:` may be spelled `tag:` in a workflow file.
const ALIASES: &[(&str, &str)] = &[("tags", "tag")];

/// Why an edit, load or save was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The edit would leave invalid config; the message is the loader's.
    #[error("{0}")]
    Invalid(String),
    #[error("no status `{0}` in the project")]
    UnknownStatus(String),
    #[error("no workflow `{0}`")]
    UnknownWorkflow(String),
    #[error("workflow `{workflow}` has no status `{status}`")]
    NotInWorkflow { workflow: String, status: String },
    #[error("workflow `{workflow}` already has status `{status}`")]
    AlreadyInWorkflow { workflow: String, status: String },
    #[error("workflow `{workflow}` has no transition {index}")]
    UnknownTransition { workflow: String, index: usize },
    #[error("a workflow named `{0}` already exists")]
    WorkflowExists(String),
    /// A status still referenced by workflows can't be deleted.
    #[error("status `{status}` is used in {}", .workflows.join(", "))]
    StatusInUse {
        status: String,
        workflows: Vec<String>,
    },
    /// Open issues name the workflow in their `workflow:` field, so it
    /// can't be renamed or deleted (they'd silently fall back to the
    /// default). Lists the first few ids.
    #[error(
        "workflow `{workflow}` is used by {total} open issue{}: {}{}; finish them or move them \
         to another workflow first",
        if *.total == 1 { "" } else { "s" },
        .issues.join(", "),
        if *.total > .issues.len() { format!(" and {} more", .total - .issues.len()) } else { String::new() }
    )]
    WorkflowInUse {
        workflow: String,
        issues: Vec<String>,
        total: usize,
    },
    /// The board couldn't be read, so whether open issues use the workflow
    /// is unknown.
    #[error("can't tell whether open issues use `{workflow}`: {reason}")]
    BoardUnavailable { workflow: String, reason: String },
    #[error("`{0}` is the default workflow; make another workflow the default first")]
    DeleteDefault(String),
    #[error("`{0}` is the only workflow")]
    DeleteLast(String),
    #[error("workflow `{workflow}`: status `{status}` uses agent `{agent}`, which doesn't exist (available: {})", list_or_none(.available))]
    UnknownAgent {
        workflow: String,
        status: String,
        agent: String,
        available: Vec<String>,
    },
    #[error("position {index} is out of range")]
    OutOfRange { index: usize },
    /// Files changed on disk since the model was loaded. Nothing was
    /// written; reload and redo the edit.
    #[error("changed on disk since it was loaded: {}", display_paths(.0))]
    Conflict(Vec<PathBuf>),
    /// A file couldn't be read, parsed or written.
    #[error("{path}: {message}")]
    File { path: PathBuf, message: String },
}

fn list_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub type Result<T, E = ConfigError> = std::result::Result<T, E>;

/// One row of the index's workflow table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowSummary {
    pub name: String,
    pub description: Option<String>,
    pub statuses: usize,
    pub transitions: usize,
    pub is_default: bool,
}

/// One row of the index's status table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSummary {
    pub id: String,
    pub name: String,
    pub category: StatusCategory,
    /// The workflows that reference the status, in index order.
    pub used_in: Vec<String>,
}

/// A workflow file the model can't edit (it doesn't parse, or names
/// statuses the catalog lacks). Shown so the user can fix it by hand; never
/// written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableWorkflow {
    /// The file stem.
    pub name: String,
    pub path: PathBuf,
    pub error: String,
    /// The status ids it lists, as far as they can be read, so a status it
    /// uses isn't deleted from under it.
    status_ids: Vec<String>,
}

/// What a save did.
#[derive(Debug, Clone)]
pub struct Saved {
    /// The model as now on disk, ready for the next edit.
    pub config: WorkflowConfig,
    pub written: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Comments a file lost: lines whose entry the edit removed, or the
    /// whole file's when its layout couldn't be followed and it had to be
    /// re-serialized.
    pub dropped_comments: Vec<(PathBuf, Vec<String>)>,
}

/// The editable statuses, workflows and default of one project.
#[derive(Debug, Clone)]
pub struct WorkflowConfig {
    project: String,
    statuses: Vec<ProjectStatus>,
    workflows: Vec<Entry>,
    default_workflow: String,
    /// `None` when the project has no `agents/` directory: like the loader,
    /// agent references aren't checked then.
    agents: Option<Vec<String>>,
    unreadable: Vec<UnreadableWorkflow>,
    disk: Arc<Disk>,
}

#[derive(Debug, Clone)]
struct Entry {
    workflow: Workflow,
    origin: Origin,
}

/// Where a workflow's text comes from on save.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// Loaded from `<stem>.yaml` (and saved there, or under its new name
    /// after a rename).
    File(String),
    /// A duplicate of the file loaded from `<stem>.yaml`: its text, comments
    /// included, is the starting point.
    Copy(String),
    /// Created here.
    New,
}

/// The files as loaded.
#[derive(Debug)]
struct Disk {
    dir: PathBuf,
    statuses_path: PathBuf,
    /// `None` when the file doesn't exist (the loader's default catalog
    /// applies).
    statuses_text: Option<String>,
    statuses: Vec<ProjectStatus>,
    /// Editable workflows by file stem.
    workflows: BTreeMap<String, Loaded>,
    /// The project YAML that holds `default_workflow:`.
    project_path: PathBuf,
    project_text: String,
    default_workflow: String,
}

#[derive(Debug)]
struct Loaded {
    text: String,
    workflow: Workflow,
}

impl WorkflowConfig {
    /// Load `project`'s statuses, workflows and default workflow.
    pub fn load(project: &str) -> Result<Self> {
        let dir = crate::workflows_dir(project).map_err(other)?;
        let statuses_path = crate::workflows::statuses_read_path(project).map_err(other)?;
        let statuses_text = read_optional(&statuses_path)?;
        let catalog = match &statuses_text {
            Some(text) => {
                ProjectStatuses::from_yaml_str(text).map_err(|e| file_err(&statuses_path, e))?
            }
            None => default_project_statuses(),
        };

        let mut workflows = BTreeMap::new();
        let mut unreadable = Vec::new();
        for (stem, path) in workflow_files(&dir)? {
            let text = read_optional(&path)?.unwrap_or_default();
            match parse_workflow(&text, &catalog) {
                Ok(mut workflow) => {
                    // Workflows load by file stem; the stem is the name.
                    workflow.name = stem.clone();
                    workflows.insert(stem, Loaded { text, workflow });
                }
                Err(error) => unreadable.push(UnreadableWorkflow {
                    status_ids: raw_status_ids(&text),
                    name: stem,
                    path,
                    error,
                }),
            }
        }

        let project_path = project_config_file(project)?;
        let project_text =
            fs::read_to_string(&project_path).map_err(|e| file_err(&project_path, e))?;
        let declared = yaml_edit::parse(&project_text)
            .ok()
            .and_then(|v| v.get("default_workflow")?.as_str().map(str::to_string))
            .unwrap_or_else(|| shelbi_core::DEFAULT_WORKFLOW_NAME.to_string());
        let default_workflow =
            crate::workflows::resolve_workflow_alias(project, &declared).map_err(other)?;

        let agents_dir = crate::agents_dir(project).map_err(other)?;
        let agents = if agents_dir.is_dir() {
            Some(crate::list_agents(project).map_err(other)?)
        } else {
            None
        };

        let entries = workflows
            .iter()
            .map(|(stem, loaded)| Entry {
                workflow: loaded.workflow.clone(),
                origin: Origin::File(stem.clone()),
            })
            .collect();
        Ok(WorkflowConfig {
            project: project.to_string(),
            statuses: catalog.statuses.clone(),
            workflows: entries,
            default_workflow: default_workflow.clone(),
            agents,
            unreadable,
            disk: Arc::new(Disk {
                dir,
                statuses_path,
                statuses_text,
                statuses: catalog.statuses,
                workflows,
                project_path,
                project_text,
                default_workflow,
            }),
        })
    }

    // -----------------------------------------------------------------------
    // Reading

    pub fn project(&self) -> &str {
        &self.project
    }

    /// The status catalog, in declared (column) order.
    pub fn statuses(&self) -> &[ProjectStatus] {
        &self.statuses
    }

    pub fn status(&self, id: &str) -> Option<&ProjectStatus> {
        self.statuses.iter().find(|s| s.id == id)
    }

    /// The editable workflows, in index order: loaded ones by name, then
    /// ones created since. Their statuses carry the catalog's names and
    /// categories.
    pub fn workflows(&self) -> impl Iterator<Item = &Workflow> {
        self.workflows.iter().map(|e| &e.workflow)
    }

    pub fn workflow(&self, name: &str) -> Option<&Workflow> {
        self.workflows().find(|w| w.name == name)
    }

    /// The project's default workflow.
    pub fn default_workflow(&self) -> &str {
        &self.default_workflow
    }

    /// The agents a status can name, or `None` when the project has no
    /// `agents/` directory (any name is accepted then).
    pub fn agents(&self) -> Option<&[String]> {
        self.agents.as_deref()
    }

    /// Workflow files that couldn't be loaded for editing.
    pub fn unreadable(&self) -> &[UnreadableWorkflow] {
        &self.unreadable
    }

    /// The index's workflow rows.
    pub fn workflow_summaries(&self) -> Vec<WorkflowSummary> {
        self.workflows()
            .map(|w| WorkflowSummary {
                name: w.name.clone(),
                description: w.description.clone(),
                statuses: w.statuses.len(),
                transitions: w.transitions.as_ref().map_or(0, Vec::len),
                is_default: w.name == self.default_workflow,
            })
            .collect()
    }

    /// The index's status rows.
    pub fn status_summaries(&self) -> Vec<StatusSummary> {
        self.statuses
            .iter()
            .map(|s| StatusSummary {
                id: s.id.clone(),
                name: s.name.clone(),
                category: s.category,
                used_in: self.used_in(&s.id),
            })
            .collect()
    }

    /// The workflows that reference status `id`: the editable ones in index
    /// order, then unreadable files that list it.
    pub fn used_in(&self, id: &str) -> Vec<String> {
        let editable = self
            .workflows()
            .filter(|w| w.status(id).is_some())
            .map(|w| w.name.clone());
        let unreadable = self
            .unreadable
            .iter()
            .filter(|u| u.status_ids.iter().any(|s| s == id))
            .map(|u| u.name.clone());
        editable.chain(unreadable).collect()
    }

    /// The id [`add_status`](Self::add_status) gives a status named `name`:
    /// its kebab-case, made unique with a `-2`, `-3`, … suffix.
    pub fn status_id_for(&self, name: &str) -> String {
        let base = kebab(name);
        let base = if base.is_empty() {
            "status".to_string()
        } else {
            base
        };
        let mut id = base.clone();
        let mut n = 2;
        while self.status(&id).is_some() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    /// `statuses.yaml`, for the index's Edit YAML button.
    pub fn statuses_path(&self) -> &Path {
        &self.disk.statuses_path
    }

    /// Where workflow `name` is saved.
    pub fn workflow_path(&self, name: &str) -> PathBuf {
        self.disk.dir.join(format!("{name}.yaml"))
    }

    /// Whether a save would write anything.
    pub fn is_dirty(&self) -> bool {
        self.plan().map_or(true, |plan| !plan.is_empty())
    }

    // -----------------------------------------------------------------------
    // Status catalog

    /// Append a status named `name` to the catalog, with the id
    /// [`status_id_for`](Self::status_id_for) gives it.
    pub fn add_status(&self, name: &str, category: StatusCategory) -> Result<Self> {
        let name = name.trim();
        let id = self.status_id_for(name);
        let mut next = self.clone();
        next.statuses.push(ProjectStatus {
            id,
            name: name.to_string(),
            category,
        });
        next.checked_catalog()
    }

    /// Rename a status. Its id stays, so workflows keep referencing it.
    pub fn rename_status(&self, id: &str, name: &str) -> Result<Self> {
        let mut next = self.clone();
        next.catalog_status_mut(id)?.name = name.trim().to_string();
        next.checked_catalog()
    }

    pub fn set_status_category(&self, id: &str, category: StatusCategory) -> Result<Self> {
        let mut next = self.clone();
        next.catalog_status_mut(id)?.category = category;
        next.checked_catalog()
    }

    /// Move a status to `index` in the catalog's column order.
    pub fn move_status(&self, id: &str, index: usize) -> Result<Self> {
        let mut next = self.clone();
        let from = next.catalog_index(id)?;
        move_item(&mut next.statuses, from, index)?;
        next.checked_catalog()
    }

    /// Delete a status no workflow uses.
    pub fn delete_status(&self, id: &str) -> Result<Self> {
        let at = self.catalog_index(id)?;
        let workflows = self.used_in(id);
        if !workflows.is_empty() {
            return Err(ConfigError::StatusInUse {
                status: id.to_string(),
                workflows,
            });
        }
        let mut next = self.clone();
        next.statuses.remove(at);
        next.checked_catalog()
    }

    // -----------------------------------------------------------------------
    // Workflows

    /// Create workflow `name`: every catalog status, with the shipped
    /// default's owner and agent for the statuses it has (and `owner: user`,
    /// no agent, for the rest), and no transitions or git overrides.
    pub fn create_workflow(&self, name: &str, description: Option<&str>) -> Result<Self> {
        self.check_new_name(name)?;
        let starter = default_workflow();
        let statuses = self
            .statuses
            .iter()
            .map(|s| {
                let shipped = starter.status(&s.id);
                let agent = shipped
                    .and_then(|st| st.agent.clone())
                    .filter(|a| self.agents.as_ref().is_none_or(|known| known.contains(a)));
                let owner = match (shipped.map(|st| st.owner), &agent) {
                    (Some(Owner::Agent), Some(_)) => Owner::Agent,
                    _ => Owner::User,
                };
                WorkflowStatus {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    category: s.category,
                    owner,
                    agent,
                    tags: Vec::new(),
                }
            })
            .collect();
        let workflow = Workflow {
            name: name.to_string(),
            description: non_empty(description),
            statuses,
            initial_status: None,
            transitions: None,
            required_params: Vec::new(),
            git: None,
            zen: None,
            review: None,
        };
        let mut next = self.clone();
        next.workflows.push(Entry {
            workflow,
            origin: Origin::New,
        });
        next.checked_workflow(name)
    }

    /// Copy workflow `from` as `name`, everything included (its file's
    /// comments and unknown keys too).
    pub fn duplicate_workflow(&self, from: &str, name: &str) -> Result<Self> {
        self.check_new_name(name)?;
        let source = &self.workflows[self.workflow_index(from)?];
        let mut workflow = source.workflow.clone();
        workflow.name = name.to_string();
        let origin = match &source.origin {
            Origin::File(stem) | Origin::Copy(stem) => Origin::Copy(stem.clone()),
            Origin::New => Origin::New,
        };
        let mut next = self.clone();
        next.workflows.push(Entry { workflow, origin });
        next.checked_workflow(name)
    }

    /// Rename a workflow (and so its file). The project default follows; it
    /// is the only config that names a workflow. Refused while open issues
    /// name it ([`check_rename`](Self::check_rename)).
    pub fn rename_workflow(&self, from: &str, name: &str) -> Result<Self> {
        if from == name {
            self.workflow_index(from)?;
            return Ok(self.clone());
        }
        self.check_new_name(name)?;
        self.check_rename(from)?;
        let mut next = self.clone();
        let at = next.workflow_index(from)?;
        next.workflows[at].workflow.name = name.to_string();
        if next.default_workflow == from {
            next.default_workflow = name.to_string();
        }
        next.checked_workflow(name)
    }

    /// Whether `workflow` can be renamed: refused while any open (not
    /// done/archived) issue names it in `workflow:` (or names `default`
    /// when that resolves to it), since those issues would fall back to the
    /// default workflow. Reads the board the app and CLI already have
    /// (`read_board_report`: the daemon's index, never a live GitHub call)
    /// and refuses when that isn't available. Does file IO, so call it off
    /// the UI thread; the UI uses it to disable Rename with the reason.
    pub fn check_rename(&self, workflow: &str) -> Result<()> {
        self.workflow_index(workflow)?;
        self.check_unused_by_issues(workflow)
    }

    /// Whether `workflow` can be deleted: not the default, not the only
    /// workflow, and (as for [`check_rename`](Self::check_rename)) named by
    /// no open issue. Does file IO.
    pub fn check_delete(&self, workflow: &str) -> Result<()> {
        self.workflow_index(workflow)?;
        if self.default_workflow == workflow {
            return Err(ConfigError::DeleteDefault(workflow.to_string()));
        }
        if self.workflows.len() == 1 {
            return Err(ConfigError::DeleteLast(workflow.to_string()));
        }
        self.check_unused_by_issues(workflow)
    }

    fn check_unused_by_issues(&self, workflow: &str) -> Result<()> {
        if !self.disk.workflows.contains_key(workflow) {
            // Not on disk yet, so no issue can name it.
            return Ok(());
        }
        let unchecked = |reason: String| ConfigError::BoardUnavailable {
            workflow: workflow.to_string(),
            reason,
        };
        let report = crate::read_board_report(&self.project)
            .map_err(|e| unchecked(format!("couldn't read the board ({e})")))?;
        let issues = match report.state {
            crate::BoardState::Warm(issues) | crate::BoardState::Stale(issues) => issues,
            crate::BoardState::Cold => return Err(unchecked("the board hasn't loaded yet".into())),
        };
        let via_default = workflow != shelbi_core::DEFAULT_WORKFLOW_NAME
            && crate::workflows::resolve_workflow_alias(
                &self.project,
                shelbi_core::DEFAULT_WORKFLOW_NAME,
            )
            .is_ok_and(|w| w == workflow);
        let terminal = |status: &str| {
            self.disk.statuses.iter().any(|s| {
                s.id == status
                    && matches!(s.category, StatusCategory::Done | StatusCategory::Archived)
            })
        };
        let blocking: Vec<String> = issues
            .iter()
            .map(|f| &f.task)
            .filter(|t| match t.workflow.as_deref() {
                Some(w) => {
                    w == workflow || (via_default && w == shelbi_core::DEFAULT_WORKFLOW_NAME)
                }
                None => false,
            })
            .filter(|t| !terminal(t.column.as_str()))
            .map(|t| t.id.clone())
            .collect();
        if blocking.is_empty() {
            return Ok(());
        }
        Err(ConfigError::WorkflowInUse {
            workflow: workflow.to_string(),
            total: blocking.len(),
            issues: blocking.into_iter().take(RENAME_BLOCKERS_SHOWN).collect(),
        })
    }

    pub fn set_description(&self, workflow: &str, description: Option<&str>) -> Result<Self> {
        self.edit(workflow, |w| {
            w.description = non_empty(description);
            Ok(())
        })
    }

    /// Make `workflow` the one tasks without a `workflow:` use.
    pub fn set_default_workflow(&self, workflow: &str) -> Result<Self> {
        self.workflow_index(workflow)?;
        let mut next = self.clone();
        next.default_workflow = workflow.to_string();
        Ok(next)
    }

    /// Delete a workflow other than the default or the only one, that no
    /// open issue uses ([`check_delete`](Self::check_delete)).
    pub fn delete_workflow(&self, workflow: &str) -> Result<Self> {
        self.check_delete(workflow)?;
        let at = self.workflow_index(workflow)?;
        let mut next = self.clone();
        next.workflows.remove(at);
        Ok(next)
    }

    // -----------------------------------------------------------------------
    // Inside a workflow

    /// Add catalog status `status` to `workflow`, at `index` (the end when
    /// `None`), owned by the user with no agent.
    pub fn add_workflow_status(
        &self,
        workflow: &str,
        status: &str,
        index: Option<usize>,
    ) -> Result<Self> {
        let known = self
            .status(status)
            .ok_or_else(|| ConfigError::UnknownStatus(status.to_string()))?
            .clone();
        self.edit(workflow, |w| {
            if w.status(status).is_some() {
                return Err(ConfigError::AlreadyInWorkflow {
                    workflow: w.name.clone(),
                    status: status.to_string(),
                });
            }
            let index = index.unwrap_or(w.statuses.len());
            if index > w.statuses.len() {
                return Err(ConfigError::OutOfRange { index });
            }
            w.statuses.insert(
                index,
                WorkflowStatus {
                    id: known.id,
                    name: known.name,
                    category: known.category,
                    owner: Owner::User,
                    agent: None,
                    tags: Vec::new(),
                },
            );
            Ok(())
        })
    }

    /// Take `status` out of `workflow`, with the transitions to and from it.
    /// An `initial_status` naming it is cleared (the first status applies).
    pub fn remove_workflow_status(&self, workflow: &str, status: &str) -> Result<Self> {
        self.edit(workflow, |w| {
            let at = status_index(w, status)?;
            w.statuses.remove(at);
            if let Some(ts) = &mut w.transitions {
                ts.retain(|t| t.from != status && t.to != status);
            }
            if w.initial_status.as_deref() == Some(status) {
                w.initial_status = None;
            }
            Ok(())
        })
    }

    pub fn move_workflow_status(&self, workflow: &str, status: &str, index: usize) -> Result<Self> {
        self.edit(workflow, |w| {
            let from = status_index(w, status)?;
            move_item(&mut w.statuses, from, index)
        })
    }

    /// Who owns `status` when Zen is off. `owner: agent` needs an agent
    /// set first (or use [`set_status_assignment`](Self::set_status_assignment)).
    pub fn set_status_owner(&self, workflow: &str, status: &str, owner: Owner) -> Result<Self> {
        self.edit_status(workflow, status, |s| s.owner = owner)
    }

    /// Which agent runs `status` under Zen; `None` for no automation.
    pub fn set_status_agent(
        &self,
        workflow: &str,
        status: &str,
        agent: Option<&str>,
    ) -> Result<Self> {
        self.edit_status(workflow, status, |s| s.agent = non_empty(agent))
    }

    /// Owner and agent together, for a change that's only valid as a pair.
    pub fn set_status_assignment(
        &self,
        workflow: &str,
        status: &str,
        owner: Owner,
        agent: Option<&str>,
    ) -> Result<Self> {
        self.edit_status(workflow, status, |s| {
            s.owner = owner;
            s.agent = non_empty(agent);
        })
    }

    /// The workspace tags `status` needs; empty for any workspace. Tags are
    /// trimmed and deduplicated.
    pub fn set_status_tags(&self, workflow: &str, status: &str, tags: &[String]) -> Result<Self> {
        let mut clean: Vec<String> = Vec::new();
        for tag in tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            if !clean.iter().any(|c| c == tag) {
                clean.push(tag.to_string());
            }
        }
        self.edit_status(workflow, status, |s| s.tags = clean)
    }

    /// The status new tasks land in; `None` for the first status.
    pub fn set_initial_status(&self, workflow: &str, status: Option<&str>) -> Result<Self> {
        self.edit(workflow, |w| {
            w.initial_status = non_empty(status);
            Ok(())
        })
    }

    /// Append a transition.
    pub fn add_transition(&self, workflow: &str, transition: Transition) -> Result<Self> {
        self.edit(workflow, |w| {
            w.transitions.get_or_insert_with(Vec::new).push(transition);
            Ok(())
        })
    }

    /// Replace transition `index` (its from, to, actions and details).
    pub fn update_transition(
        &self,
        workflow: &str,
        index: usize,
        transition: Transition,
    ) -> Result<Self> {
        self.edit(workflow, |w| {
            *transition_mut(w, index)? = transition;
            Ok(())
        })
    }

    pub fn delete_transition(&self, workflow: &str, index: usize) -> Result<Self> {
        self.edit(workflow, |w| {
            transition_mut(w, index)?;
            let ts = w.transitions.as_mut().expect("checked above");
            ts.remove(index);
            if ts.is_empty() {
                w.transitions = None;
            }
            Ok(())
        })
    }

    pub fn move_transition(&self, workflow: &str, index: usize, to: usize) -> Result<Self> {
        self.edit(workflow, |w| {
            transition_mut(w, index)?;
            move_item(w.transitions.as_mut().expect("checked above"), index, to)
        })
    }

    /// The workflow's `git:` override; `None` inherits the project's.
    pub fn set_git(&self, workflow: &str, git: Option<GitConfig>) -> Result<Self> {
        self.edit(workflow, |w| {
            w.git = git;
            Ok(())
        })
    }

    /// The task fields every task on the workflow must carry.
    pub fn set_required_params(&self, workflow: &str, params: Vec<String>) -> Result<Self> {
        self.edit(workflow, |w| {
            w.required_params = params;
            Ok(())
        })
    }

    /// How the Review agent serves the branch; `None` is a diff-only review.
    pub fn set_review(&self, workflow: &str, review: Option<ReviewServe>) -> Result<Self> {
        self.edit(workflow, |w| {
            w.review = review;
            Ok(())
        })
    }

    /// Any other edit to a workflow's fields, validated like the rest. The
    /// name isn't edited here; use [`rename_workflow`](Self::rename_workflow).
    pub fn update_workflow(
        &self,
        workflow: &str,
        edit: impl FnOnce(&mut Workflow),
    ) -> Result<Self> {
        self.edit(workflow, |w| {
            let name = w.name.clone();
            edit(w);
            w.name = name;
            Ok(())
        })
    }

    // -----------------------------------------------------------------------
    // Saving

    /// Write every changed file, keeping its comments and the keys the
    /// model doesn't edit, and return the model as now on disk. Refuses,
    /// writing nothing, if a file to be written or removed changed on disk
    /// since the load.
    pub fn save(&self) -> Result<Saved> {
        let plan = self.plan()?;
        let conflicts: Vec<PathBuf> = plan
            .iter()
            .filter(|op| read_optional(&op.path).map_or(true, |now| now != op.expected))
            .map(|op| op.path.clone())
            .collect();
        if !conflicts.is_empty() {
            return Err(ConfigError::Conflict(conflicts));
        }
        let mut written = Vec::new();
        let mut removed = Vec::new();
        let mut dropped_comments = Vec::new();
        for op in &plan {
            match &op.text {
                Some(text) => {
                    crate::atomic_write(&op.path, text.as_bytes())
                        .map_err(|e| file_err(&op.path, e))?;
                    written.push(op.path.clone());
                }
                None => {
                    match fs::remove_file(&op.path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == ErrorKind::NotFound => {}
                        Err(e) => return Err(file_err(&op.path, e)),
                    }
                    removed.push(op.path.clone());
                }
            }
            if !op.dropped_comments.is_empty() {
                dropped_comments.push((op.path.clone(), op.dropped_comments.clone()));
            }
        }
        Ok(Saved {
            config: WorkflowConfig::load(&self.project)?,
            written,
            removed,
            dropped_comments,
        })
    }

    /// The writes a save makes, in order.
    fn plan(&self) -> Result<Vec<FileOp>> {
        let disk = &self.disk;
        let mut workflow_ops: BTreeMap<PathBuf, FileOp> = BTreeMap::new();
        // Files whose workflow is gone, or renamed away.
        for (stem, loaded) in &disk.workflows {
            let stays = self
                .workflows
                .iter()
                .any(|e| e.origin == Origin::File(stem.clone()) && e.workflow.name == *stem);
            if !stays {
                let path = self.workflow_path(stem);
                workflow_ops.insert(path.clone(), FileOp::remove(path, &loaded.text));
            }
        }
        for entry in &self.workflows {
            let wf = &entry.workflow;
            let path = self.workflow_path(&wf.name);
            let patched = match &entry.origin {
                Origin::File(stem) | Origin::Copy(stem) => {
                    let loaded = &disk.workflows[stem];
                    if entry.origin == Origin::File(wf.name.clone()) && same(&loaded.workflow, wf)?
                    {
                        continue;
                    }
                    edit_text(&loaded.text, &loaded.workflow, wf, &path)?
                }
                Origin::New => yaml_edit::Patched {
                    text: shelbi_core::scaffold::new_workflow_yaml(wf)
                        .map_err(|e| file_err(&path, e))?,
                    dropped_comments: Vec::new(),
                },
            };
            // What's at the path now: a loaded file (this one, or one that
            // was renamed or deleted away), or nothing.
            workflow_ops.remove(&path);
            let expected = disk.workflows.get(&wf.name).map(|l| l.text.clone());
            workflow_ops.insert(
                path.clone(),
                FileOp {
                    path,
                    expected,
                    text: Some(patched.text),
                    dropped_comments: patched.dropped_comments,
                },
            );
        }

        let mut statuses_op = None;
        let catalog_changed = self.statuses != disk.statuses;
        let needs_file = disk.statuses_text.is_none() && !self.workflows.is_empty();
        if catalog_changed || needs_file {
            let base_text = match &disk.statuses_text {
                Some(text) => text.clone(),
                None => shelbi_core::scaffold::default_statuses_yaml()
                    .map_err(|e| file_err(&disk.statuses_path, e))?,
            };
            let base = ProjectStatuses {
                statuses: disk.statuses.clone(),
            };
            let new = ProjectStatuses {
                statuses: self.statuses.clone(),
            };
            let patched = edit_text(&base_text, &base, &new, &disk.statuses_path)?;
            statuses_op = Some(FileOp {
                path: disk.statuses_path.clone(),
                expected: disk.statuses_text.clone(),
                text: Some(patched.text),
                dropped_comments: patched.dropped_comments,
            });
        }

        let mut project_op = None;
        if self.default_workflow != disk.default_workflow {
            let raw = yaml_edit::parse(&disk.project_text)
                .map_err(|e| file_err(&disk.project_path, e))?;
            let mut target = raw.clone();
            let map = target
                .as_mapping_mut()
                .ok_or_else(|| file_err(&disk.project_path, "isn't a mapping"))?;
            map.insert(
                "default_workflow".into(),
                self.default_workflow.clone().into(),
            );
            let patched = yaml_edit::patch(&disk.project_text, &target)
                .map_err(|e| file_err(&disk.project_path, e))?;
            project_op = Some(FileOp {
                path: disk.project_path.clone(),
                expected: Some(disk.project_text.clone()),
                text: Some(patched.text),
                dropped_comments: patched.dropped_comments,
            });
        }

        // A catalog that only grows goes first, so no workflow ever names a
        // status it lacks; one that loses statuses goes after the workflows
        // that stopped using them. The default moves once its file exists.
        let removes_statuses = disk.statuses.iter().any(|s| self.status(&s.id).is_none());
        let mut plan = Vec::new();
        if !removes_statuses {
            plan.extend(statuses_op.take());
        }
        let (writes, removes): (Vec<FileOp>, Vec<FileOp>) =
            workflow_ops.into_values().partition(|op| op.text.is_some());
        plan.extend(writes);
        plan.extend(project_op);
        plan.extend(removes);
        plan.extend(statuses_op);
        Ok(plan)
    }

    // -----------------------------------------------------------------------
    // Helpers

    fn catalog_index(&self, id: &str) -> Result<usize> {
        self.statuses
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| ConfigError::UnknownStatus(id.to_string()))
    }

    fn catalog_status_mut(&mut self, id: &str) -> Result<&mut ProjectStatus> {
        let at = self.catalog_index(id)?;
        Ok(&mut self.statuses[at])
    }

    fn workflow_index(&self, name: &str) -> Result<usize> {
        self.workflows
            .iter()
            .position(|e| e.workflow.name == name)
            .ok_or_else(|| ConfigError::UnknownWorkflow(name.to_string()))
    }

    /// `name` is free and valid for a new or renamed workflow.
    fn check_new_name(&self, name: &str) -> Result<()> {
        validate_workflow_name(name).map_err(|_| {
            ConfigError::Invalid(format!(
                "`{name}` isn't a valid workflow name: use lowercase letters, digits, `-` and `_`, \
                 starting with a letter or digit"
            ))
        })?;
        if self.workflow(name).is_some() || self.unreadable.iter().any(|u| u.name == name) {
            return Err(ConfigError::WorkflowExists(name.to_string()));
        }
        Ok(())
    }

    /// Apply `edit` to a copy of `workflow` and validate it.
    fn edit(&self, workflow: &str, edit: impl FnOnce(&mut Workflow) -> Result<()>) -> Result<Self> {
        let mut next = self.clone();
        let at = next.workflow_index(workflow)?;
        edit(&mut next.workflows[at].workflow)?;
        next.checked_workflow(workflow)
    }

    fn edit_status(
        &self,
        workflow: &str,
        status: &str,
        edit: impl FnOnce(&mut WorkflowStatus),
    ) -> Result<Self> {
        self.edit(workflow, |w| {
            let at = status_index(w, status)?;
            edit(&mut w.statuses[at]);
            Ok(())
        })
    }

    /// Validate the catalog and carry its names and categories into the
    /// workflows.
    fn checked_catalog(mut self) -> Result<Self> {
        let catalog = ProjectStatuses {
            statuses: self.statuses.clone(),
        };
        catalog.validate().map_err(invalid)?;
        for entry in &mut self.workflows {
            for st in &mut entry.workflow.statuses {
                let known = catalog
                    .get(&st.id)
                    .ok_or_else(|| ConfigError::UnknownStatus(st.id.clone()))?;
                st.name = known.name.clone();
                st.category = known.category;
            }
        }
        Ok(self)
    }

    /// Validate workflow `name` as the loader would.
    fn checked_workflow(mut self, name: &str) -> Result<Self> {
        let at = self.workflow_index(name)?;
        let catalog = ProjectStatuses {
            statuses: self.statuses.clone(),
        };
        let wf = &mut self.workflows[at].workflow;
        for st in &wf.statuses {
            if catalog.get(&st.id).is_none() {
                return Err(ConfigError::UnknownStatus(st.id.clone()));
            }
        }
        *wf = wf.clone().resolve_against(&catalog).map_err(invalid)?;
        if let Some(known) = &self.agents {
            for st in &wf.statuses {
                if let Some(agent) = &st.agent {
                    if !known.contains(agent) {
                        return Err(ConfigError::UnknownAgent {
                            workflow: wf.name.clone(),
                            status: st.id.clone(),
                            agent: agent.clone(),
                            available: known.clone(),
                        });
                    }
                }
            }
        }
        Ok(self)
    }
}

/// One file a save writes (`text`) or removes (`None`), and the text it
/// must still have on disk (`None`: must not exist).
#[derive(Debug)]
struct FileOp {
    path: PathBuf,
    expected: Option<String>,
    text: Option<String>,
    dropped_comments: Vec<String>,
}

impl FileOp {
    fn remove(path: PathBuf, text: &str) -> FileOp {
        FileOp {
            path,
            expected: Some(text.to_string()),
            text: None,
            dropped_comments: Vec::new(),
        }
    }
}

/// `text` (a file holding `base`) rewritten to hold `new`, changing only
/// what differs.
fn edit_text<T: serde::Serialize>(
    text: &str,
    base: &T,
    new: &T,
    path: &Path,
) -> Result<yaml_edit::Patched> {
    let to_value = |t: &T| serde_yaml::to_value(t).map_err(|e| file_err(path, e));
    let raw = yaml_edit::parse(text).map_err(|e| file_err(path, e))?;
    let target = yaml_edit::merge(&raw, &to_value(base)?, &to_value(new)?, ALIASES);
    yaml_edit::patch(text, &target).map_err(|e| file_err(path, e))
}

/// Whether two workflows write the same file (names and categories, which
/// live in the catalog, aside).
fn same(a: &Workflow, b: &Workflow) -> Result<bool> {
    let to_value =
        |w: &Workflow| serde_yaml::to_value(w).map_err(|e| ConfigError::Invalid(e.to_string()));
    Ok(to_value(a)? == to_value(b)?)
}

/// Parse and resolve a workflow file as the loader does (agent references
/// aside: those are checked when the workflow is edited).
fn parse_workflow(text: &str, catalog: &ProjectStatuses) -> Result<Workflow, String> {
    let inline = Workflow::inline_identity_fields(text).map_err(|e| e.to_string())?;
    if !inline.is_empty() {
        return Err("carries inline status identity (`name:` / `category:`); \
                    statuses.yaml is the source of truth for those"
            .to_string());
    }
    Workflow::from_yaml_str(text)
        .and_then(|wf| wf.resolve_against(catalog))
        .map_err(|e| e.to_string())
}

/// The `id`s under a workflow file's `statuses:`, read leniently.
fn raw_status_ids(text: &str) -> Vec<String> {
    let Ok(value) = serde_yaml::from_str::<Value>(text) else {
        return Vec::new();
    };
    value
        .get("statuses")
        .and_then(Value::as_sequence)
        .map(|items| {
            items
                .iter()
                .filter_map(|s| {
                    s.get("id")
                        .or_else(|| s.get("name"))?
                        .as_str()
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `(stem, path)` of each workflow file in `dir`, sorted by stem.
pub(crate) fn workflow_files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(file_err(dir, e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| file_err(dir, e))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") || !path.is_file() {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if stem == "statuses" {
            continue;
        }
        out.push((stem, path));
    }
    out.sort();
    Ok(out)
}

/// The project YAML that holds `default_workflow:`: the global
/// `projects/<name>.yaml`, or an in-repo project's shared
/// `<repo>/.shelbi/project.yaml`. Same precedence as the project loader.
pub(crate) fn project_config_file(project: &str) -> Result<PathBuf> {
    let projects = crate::projects_dir().map_err(other)?;
    let local = projects.join(project).join("local.yaml");
    if local.is_file() {
        let text = fs::read_to_string(&local).map_err(|e| file_err(&local, e))?;
        let repo = crate::migrate::extract_repo_from_local_yaml(&text, &local).map_err(other)?;
        return Ok(crate::expand_tilde_str(&repo)
            .join(".shelbi")
            .join("project.yaml"));
    }
    Ok(projects.join(format!("{project}.yaml")))
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(file_err(path, e)),
    }
}

fn status_index(w: &Workflow, status: &str) -> Result<usize> {
    w.statuses
        .iter()
        .position(|s| s.id == status)
        .ok_or_else(|| ConfigError::NotInWorkflow {
            workflow: w.name.clone(),
            status: status.to_string(),
        })
}

fn transition_mut(w: &mut Workflow, index: usize) -> Result<&mut Transition> {
    let name = w.name.clone();
    w.transitions
        .as_mut()
        .and_then(|ts| ts.get_mut(index))
        .ok_or(ConfigError::UnknownTransition {
            workflow: name,
            index,
        })
}

fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) -> Result<()> {
    if to >= items.len() {
        return Err(ConfigError::OutOfRange { index: to });
    }
    let item = items.remove(from);
    items.insert(to, item);
    Ok(())
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `In Progress` → `in-progress`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn invalid(e: shelbi_core::Error) -> ConfigError {
    ConfigError::Invalid(match e {
        shelbi_core::Error::InvalidWorkflow(m) | shelbi_core::Error::InvalidProjectStatuses(m) => m,
        e => e.to_string(),
    })
}

fn other(e: shelbi_core::Error) -> ConfigError {
    ConfigError::Invalid(e.to_string())
}

fn file_err(path: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::File {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests;
