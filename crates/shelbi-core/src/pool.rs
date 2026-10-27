//! Planning a workspace pool change, without touching disk: the requests
//! and results the pool's lifecycle operations
//! (`shelbi_orchestrator::pool_admin`) take and return, the errors a form
//! shows, and the checks a form runs on its draft before it sends one.
//!
//! These are pure functions of a loaded [`Project`], so a client's form
//! state (`shelbi_app::workspace_form`, which can't depend on the
//! orchestrator) checks a draft exactly as the operation will; the operation
//! runs the same checks again against the config as it is when it runs.
//!
//! - A workspace's worktree is always `<machine.work_dir>/.shelbi/wt/<name>`
//!   ([`workspace_worktree`]); there's no per-workspace path.
//! - A workspace name is a valid task id, free in the project both exactly
//!   and by slug.
//! - Tags are trimmed, empties dropped, and exact duplicates removed; case is
//!   kept (`GPU` and `gpu` are different tags, as routing compares them). A
//!   tag can't contain whitespace or a comma ([`normalize_tags`]).
//! - An empty slot is `None` (the zero-based declaration-order index among
//!   the machine's workspaces); any `u32`, zero included, is an explicit slot
//!   ([`parse_slot`]).

use std::path::PathBuf;

use crate::{Machine, MachineKind, Project, WorkspaceSpec};

/// The form field a validation error belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolField {
    Name,
    Machine,
    Tags,
    Slot,
    Kind,
    Host,
    WorkDir,
}

/// Why an operation was refused or failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolError {
    /// The request doesn't validate. `field` is the form field to mark, when
    /// there is one.
    #[error("{message}")]
    Invalid {
        field: Option<PoolField>,
        message: String,
    },
    /// The workspace or machine isn't declared (any more).
    #[error("{0}")]
    NotFound(String),
    /// The entry or the config changed since it was loaded. Nothing changed;
    /// reload and redo the edit.
    #[error("{0}")]
    Conflict(String),
    /// The workspace has an in-progress or review task.
    #[error("workspace `{workspace}` has an active task `{task}` — stop the workspace to release it first")]
    ActiveTask { workspace: String, task: String },
    /// The live state makes the change unsafe (a live session, work in the
    /// worktree a change could lose, a machine that can't be checked). The
    /// message says what to do.
    #[error("{0}")]
    Unsafe(String),
    /// The worktree's `.claude/settings.local.json` is user-authored and
    /// Shelbi won't merge its hooks in; `message` is the merge instruction.
    /// The workspace wasn't added; merge and retry.
    #[error("{message}")]
    HooksMergeRequired { workspace: String, message: String },
    /// Creating, moving or removing a worktree or session failed.
    #[error("{0}")]
    Provisioning(String),
    /// The config couldn't be written.
    #[error("{0}")]
    Persistence(String),
    /// The hub daemon runs a different version.
    #[error("{0}")]
    Version(String),
}

impl PoolError {
    pub fn invalid(field: PoolField, message: impl Into<String>) -> Self {
        PoolError::Invalid {
            field: Some(field),
            message: message.into(),
        }
    }
}

pub type Result<T, E = PoolError> = std::result::Result<T, E>;

// ---------------------------------------------------------------------------
// Requests and results

/// A new workspace. `machine: None` places it on the project's local machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddWorkspace {
    pub name: String,
    pub machine: Option<String>,
    /// As typed; normalized with [`normalize_tags`].
    pub tags: Vec<String>,
    pub slot: Option<u32>,
}

/// An edit of workspace `name`: every field as it should be afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateWorkspace {
    /// The workspace's current name.
    pub name: String,
    /// The workspace as the form loaded it; a different current value is a
    /// [`PoolError::Conflict`]. `None` skips the check.
    pub expected: Option<WorkspaceSpec>,
    pub new_name: String,
    pub machine: String,
    pub tags: Vec<String>,
    pub slot: Option<u32>,
}

/// Removing workspace `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveWorkspace {
    pub name: String,
    pub expected: Option<WorkspaceSpec>,
    /// Remove even with an active task or work at risk in the worktree.
    pub force: bool,
}

/// A machine's editable fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineFields {
    pub name: String,
    pub kind: MachineKind,
    /// The SSH host; empty or `None` uses the machine name. Ignored for a
    /// local machine.
    pub host: Option<String>,
    /// The project's directory on the machine.
    pub work_dir: String,
    pub tags: Vec<String>,
}

/// An edit of machine `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateMachine {
    pub name: String,
    pub expected: Option<Machine>,
    pub fields: MachineFields,
}

/// What became of the hook wiring in the workspace's worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HooksOutcome {
    /// Nothing to wire: the project's runner isn't Claude Code, or the
    /// worktree didn't change.
    Skipped,
    Wired,
    /// Shelbi's hooks were merged into a user-authored
    /// `settings.local.json`, keeping the user's content. Recorded on
    /// `events.log`; callers say so.
    SelfHealed,
}

/// A workspace as now configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceChange {
    pub workspace: WorkspaceSpec,
    /// Its name before the change, when it was renamed.
    pub previous_name: Option<String>,
    /// Its machine before the change, when it moved.
    pub previous_machine: Option<String>,
    /// The effective `$SLOT` (the explicit slot, else the fallback index).
    pub slot: u32,
    /// `<machine.work_dir>/.shelbi/wt/<name>` on its machine.
    pub worktree: PathBuf,
    pub hooks: HooksOutcome,
    /// Things done or left that don't fail the change (an old worktree that
    /// couldn't be cleaned up).
    pub warnings: Vec<String>,
}

/// A removed workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRemoved {
    pub name: String,
    pub machine: String,
    pub warnings: Vec<String>,
}

/// A machine as now configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineChange {
    pub machine: Machine,
    pub previous_name: Option<String>,
    /// The workspaces on the machine, in declaration order.
    pub workspaces: Vec<String>,
}


// ---------------------------------------------------------------------------
// Planning


/// Validate and resolve an [`AddWorkspace`] against `p` without touching
/// disk: an invalid name, an exact or slug collision with an existing
/// workspace, an unknown (or undefaultable) machine, and bad tags are
/// refused.
pub fn plan_workspace_add(p: &Project, req: &AddWorkspace) -> Result<WorkspaceSpec> {
    check_workspace_name(p, &req.name, None)?;
    let spec = WorkspaceSpec {
        name: req.name.clone(),
        machine: resolve_add_machine(p, req.machine.as_deref())?,
        tags: normalize_tags(&req.tags)?,
        slot: req.slot,
    };
    let mut all = p.workspaces.clone();
    all.push(spec.clone());
    with_workspaces(p, all)
        .validate_workspaces()
        .map_err(|e| PoolError::invalid(PoolField::Machine, e.to_string()))?;
    Ok(spec)
}

/// The machine for a new workspace: the named one, else the first local
/// machine.
pub fn resolve_add_machine(p: &Project, machine: Option<&str>) -> Result<String> {
    if let Some(name) = machine {
        let m = p
            .machine(name)
            .ok_or_else(|| PoolError::invalid(PoolField::Machine, format!("unknown machine `{name}`")))?;
        return Ok(m.name.clone());
    }
    p.machines
        .iter()
        .find(|m| m.host().is_local())
        .map(|m| m.name.clone())
        .ok_or_else(|| {
            PoolError::invalid(
                PoolField::Machine,
                "project declares no local machine to host the workspace",
            )
        })
}

/// Validate and resolve an [`UpdateWorkspace`] of `current` against `p`
/// without touching disk: an invalid or colliding new name, an unknown
/// machine, and bad tags are refused. A form checks its draft with this
/// before it sends the edit; `pool_admin::update_workspace` runs it again
/// against the config as it is then.
pub fn plan_workspace_update(p: &Project, current: &WorkspaceSpec, req: &UpdateWorkspace) -> Result<WorkspaceSpec> {
    if req.new_name != current.name {
        check_workspace_name(p, &req.new_name, Some(&current.name))?;
    }
    machine_of(p, &req.machine)
        .map_err(|_| PoolError::invalid(PoolField::Machine, format!("unknown machine `{}`", req.machine)))?;
    let spec = WorkspaceSpec {
        name: req.new_name.clone(),
        machine: req.machine.clone(),
        tags: normalize_tags(&req.tags)?,
        slot: req.slot,
    };
    with_replaced(p, &current.name, &spec)
        .validate_workspaces()
        .map_err(|e| PoolError::invalid(PoolField::Machine, e.to_string()))?;
    Ok(spec)
}

/// `p` with workspace `name` replaced by `spec`, in its place.
pub fn with_replaced(p: &Project, name: &str, spec: &WorkspaceSpec) -> Project {
    let replaced = p
        .workspaces
        .iter()
        .map(|w| if w.name == name { spec.clone() } else { w.clone() })
        .collect();
    with_workspaces(p, replaced)
}

// ---------------------------------------------------------------------------
// Field helpers

/// A string `ssh` takes as its destination: no whitespace or control
/// characters, and no leading `-` (which `ssh` would read as an option).
pub fn usable_ssh_host(h: &str) -> bool {
    !h.is_empty() && !h.starts_with('-') && !h.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Tags as entered → tags as stored: trimmed, empties dropped, exact
/// duplicates removed (first one kept), case unchanged. A tag with
/// whitespace or a comma inside is refused.
pub fn normalize_tags(input: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for tag in input.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if tag.chars().any(|c| c.is_whitespace() || c == ',' || c.is_control()) {
            return Err(PoolError::invalid(
                PoolField::Tags,
                format!("tag `{tag}` can't contain spaces or commas"),
            ));
        }
        if !out.iter().any(|t| t == tag) {
            out.push(tag.to_string());
        }
    }
    Ok(out)
}

/// A slot as typed: empty is `None` (automatic), otherwise a whole number
/// from 0 to `u32::MAX`.
pub fn parse_slot(input: &str) -> Result<Option<u32>> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if !s.chars().all(|c| c.is_ascii_digit()) {
        return Err(PoolError::invalid(
            PoolField::Slot,
            "slot must be a whole number (0 or more), or empty for automatic",
        ));
    }
    s.parse::<u32>().map(Some).map_err(|_| {
        PoolError::invalid(
            PoolField::Slot,
            format!("slot `{s}` is too large (the largest is {})", u32::MAX),
        )
    })
}

/// `name` is a valid workspace name, free in `p` (ignoring `except`, the
/// workspace being renamed) both exactly and by slug.
fn check_workspace_name(p: &Project, name: &str, except: Option<&str>) -> Result<()> {
    crate::validate_task_id(name)
        .map_err(|e| PoolError::invalid(PoolField::Name, format!("invalid workspace name `{name}`: {e}")))?;
    let slug = crate::normalize_project_name(name).map_err(|_| {
        PoolError::invalid(PoolField::Name, format!("workspace name `{name}` does not reduce to a valid slug"))
    })?;
    for w in p.workspaces.iter().filter(|w| Some(w.name.as_str()) != except) {
        if w.name == name {
            return Err(PoolError::invalid(PoolField::Name, format!("workspace `{name}` already exists")));
        }
        if crate::normalize_project_name(&w.name).is_ok_and(|s| s == slug) {
            return Err(PoolError::invalid(
                PoolField::Name,
                format!("workspace name `{name}` slug-collides with existing workspace `{}`", w.name),
            ));
        }
    }
    Ok(())
}


// ---------------------------------------------------------------------------
// Helpers

/// Where `workspace`'s worktree is on `machine`:
/// `<machine.work_dir>/.shelbi/wt/<name>`.
pub fn workspace_worktree(machine: &Machine, workspace: &WorkspaceSpec) -> PathBuf {
    machine
        .work_dir
        .join(".shelbi")
        .join("wt")
        .join(&workspace.name)
}

/// `p` with its workspaces replaced by `workspaces`.
pub fn with_workspaces(p: &Project, workspaces: Vec<WorkspaceSpec>) -> Project {
    let mut c = p.clone();
    c.workspaces = workspaces;
    c
}

/// Machine `name` of `p`; a workspace naming an undeclared one is
/// [`PoolError::NotFound`].
pub fn machine_of<'a>(p: &'a Project, name: &str) -> Result<&'a Machine> {
    p.machine(name)
        .ok_or_else(|| PoolError::NotFound(format!("workspace references unknown machine `{name}`")))
}
