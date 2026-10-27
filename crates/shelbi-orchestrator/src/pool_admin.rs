//! Machine and workspace lifecycle operations: add, edit and remove a
//! workspace, add and edit a machine. One implementation behind
//! `shelbi workspace add|rm` and the desktop's Settings screens.
//!
//! Each operation takes a structured request and returns a structured result
//! (the identities to refresh) or a [`PoolError`] a form can show: a
//! validation error names its [`PoolField`], a refusal says what to do about
//! it, and a hook merge the user has to make is its own variant
//! ([`PoolError::HooksMergeRequired`]) so the CLI keeps its distinct exit
//! code. Nothing here prints or exits.
//!
//! ## What every operation does
//!
//! 1. Checks the hub daemon's version, like every other mutation.
//! 2. Takes the project's pool lock ([`shelbi_state::lock_pool_config`]), then
//!    the per-workspace dispatch locks it touches (so a dispatch onto the
//!    workspace finishes first, or waits), and reloads the config.
//! 3. Validates against that fresh config before any side effect, and checks
//!    `expected` (the entry as the form loaded it) so an edit made elsewhere in
//!    between is a [`PoolError::Conflict`], not overwritten.
//! 4. Verifies the live state at execution time: active tasks, a live
//!    session, and whether the worktree holds work a change could lose.
//! 5. Provisions the destination first and verifies it, then writes the
//!    config ([`PoolConfig::save`]: atomic, comment-preserving, re-read and
//!    conflict-checked), and only then retires the old identity. A failure
//!    before the write undoes what this operation created; the config stays
//!    as it was, and the same request can be retried.
//!
//! Dispatch, routing, the sidebar and both clients read the project config
//! from disk on every use, so a committed change is picked up on their next
//! read, exactly like a hand edit.
//!
//! ## Decisions
//!
//! - A workspace's worktree is always `<machine.work_dir>/.shelbi/wt/<name>`;
//!   there's no per-workspace path. Renaming or moving a workspace is
//!   therefore a lifecycle change: it moves (same machine, `git worktree
//!   move`, which keeps every file) or re-materializes (new machine) the
//!   worktree. Both refuse an active workspace (an in-progress/review task,
//!   any open task assigned to it, or a live session) and a worktree with
//!   uncommitted changes, commits on no branch, or unpushed branch commits.
//! - Tags are trimmed, empties dropped, and exact duplicates removed; case is
//!   kept (`GPU` and `gpu` are different tags, as routing compares them). A
//!   tag can't contain whitespace or a comma.
//! - An empty slot is `None` (the zero-based declaration-order index among
//!   the machine's workspaces); any `u32`, zero included, is an explicit slot.
//! - Removal refuses an active task and a worktree with work at risk unless
//!   `force` (the CLI's `--force`; the desktop's normal path never sets it).
//!   It kills an idle session (a user shell) as the CLI always has. A
//!   worktree it couldn't remove is an error and the workspace stays
//!   declared; with `force` it's a warning instead, so a workspace on a
//!   machine that's gone can still be dropped.
//! - A rename on the same machine carries the workspace's tmux-cutover
//!   migration state to the new name. Hub-side status files keyed by the old
//!   name are left to go stale (nothing reads an undeclared name's status).
//! - Changing a machine's kind, host or directory is refused while any of its
//!   workspaces is active, and while any has a worktree at the old location
//!   that isn't also present at the new one (an alias for the same host, or a
//!   checkout the user already moved). Renaming a machine renames its
//!   workspaces' references in the same write. Machines can't be removed.
//! - A machine name is a free-form label ("Build server", as the design
//!   shows): trimmed, unique ignoring case and punctuation (it mustn't
//!   slug-collide with another machine's), no control characters. Nothing
//!   uses it as a path or a shell word except SSH's fallback to the name when
//!   `host:` is unset, so an SSH machine whose name isn't a usable host needs
//!   one. A host can't contain whitespace or start with `-`; a local machine
//!   has no host.
//! - A directory must be absolute. A local machine's `~` / `~/…` is expanded
//!   against this machine's home (nothing expands it later); a remote one has
//!   to be written out, since its home isn't known without connecting.

use std::path::PathBuf;
use std::time::Duration;

use shelbi_core::{Column, Machine, MachineKind, Project, WorkspaceSpec};
use shelbi_state::pool_config::{PoolConfig, PoolConfigError, PoolEntry};

use crate::workspace as ws;

/// How long one probe of a machine (a `test`, a `git status`) may take before
/// the machine counts as unreachable.
const PROBE_DEADLINE: Duration = Duration::from_secs(15);

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
    fn invalid(field: PoolField, message: impl Into<String>) -> Self {
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
// The live seam

/// Whether a workspace's session is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Dead,
    Alive { user_shell: bool },
    Unreachable(String),
}

/// What a workspace's worktree holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeState {
    /// Nothing at the path.
    Missing,
    /// A worktree with nothing a removal could lose.
    Clean,
    /// Something a removal would lose; the reason says what.
    AtRisk(String),
    /// The machine couldn't be checked.
    Unknown(String),
}

/// The outcome of wiring hooks into a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookWiring {
    Wired,
    SelfHealed,
    MergeRequired(String),
}

/// Everything the operations do to the world outside the config file. The
/// production implementation is [`LiveOps`]; tests script it.
pub trait PoolOps {
    fn check_version(&self) -> Result<(), String>;
    /// The in-progress or review task assigned to `workspace`, read from the
    /// issue store.
    fn active_task(&self, project: &Project, workspace: &str) -> Result<Option<String>, String>;
    /// Every open task assigned to `workspace`.
    fn open_assignments(&self, project: &Project, workspace: &str) -> Result<Vec<String>, String>;
    fn session(&self, project: &Project, workspace: &WorkspaceSpec) -> SessionState;
    fn worktree(&self, machine: &Machine, workspace: &WorkspaceSpec) -> WorktreeState;
    /// Create the idle worktree (a no-op when one is there).
    fn materialize(&self, project: &Project, machine: &Machine, workspace: &WorkspaceSpec)
        -> Result<(), String>;
    fn wire_hooks(&self, project: &Project, machine: &Machine, workspace: &WorkspaceSpec)
        -> Result<HookWiring, String>;
    /// Move a worktree to another name on the same machine.
    fn move_worktree(&self, project: &Project, machine: &Machine, from: &WorkspaceSpec, to: &WorkspaceSpec)
        -> Result<(), String>;
    /// Remove a worktree; an error when it's still there afterwards.
    fn remove_worktree(&self, project: &Project, machine: &Machine, workspace: &WorkspaceSpec)
        -> Result<(), String>;
    fn kill_session(&self, project: &Project, workspace: &WorkspaceSpec) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Workspaces

/// Add a workspace: validate, materialize its worktree, wire Shelbi's Claude
/// hooks, then declare it. The worktree comes first so a
/// [`PoolError::HooksMergeRequired`] leaves the name free for the retry (the
/// materialize is idempotent, so retrying is cheap).
pub fn add_workspace(project: &str, req: &AddWorkspace) -> Result<WorkspaceChange> {
    add_workspace_with(&LiveOps, project, req)
}

pub fn add_workspace_with(
    ops: &dyn PoolOps,
    project: &str,
    req: &AddWorkspace,
) -> Result<WorkspaceChange> {
    ops.check_version().map_err(PoolError::Version)?;
    let _pool = lock(shelbi_state::lock_pool_config(project))?;
    let (p, pool) = load(project)?;
    let spec = plan_workspace_add(&p, req)?;
    let _ws = lock(shelbi_state::lock_workspace(project, &spec.name))?;
    let machine = machine_of(&p, &spec.machine)?.clone();
    let candidate = with_workspaces(&p, {
        let mut all = p.workspaces.clone();
        all.push(spec.clone());
        all
    });
    ops.materialize(&candidate, &machine, &spec)
        .map_err(PoolError::Provisioning)?;
    let hooks = wire(ops, &candidate, &machine, &spec)?;
    let mut workspaces = kept(pool.workspaces());
    workspaces.push(PoolEntry::new(spec.clone()));
    let saved = pool
        .save(&kept(pool.machines()), &workspaces)
        .map_err(|e| {
            persistence(
                e,
                &format!(
                    "the worktree at {} was kept; retrying the add reuses it",
                    ws::workspace_worktree(&machine, &spec).display()
                ),
            )
        })?;
    Ok(change(&saved, &p, &spec.name, None, None, hooks, Vec::new()))
}

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

/// Edit a workspace. Tags and slot alone are a config edit; a new name or
/// machine is a lifecycle change (see the module docs).
pub fn update_workspace(project: &str, req: &UpdateWorkspace) -> Result<WorkspaceChange> {
    update_workspace_with(&LiveOps, project, req)
}

pub fn update_workspace_with(
    ops: &dyn PoolOps,
    project: &str,
    req: &UpdateWorkspace,
) -> Result<WorkspaceChange> {
    ops.check_version().map_err(PoolError::Version)?;
    let _pool = lock(shelbi_state::lock_pool_config(project))?;
    let (p, pool) = load(project)?;
    let current = find_workspace(&p, &req.name)?.clone();
    if let Some(expected) = &req.expected {
        if *expected != current {
            return Err(changed_elsewhere("workspace", &req.name));
        }
    }
    if req.new_name != current.name {
        check_workspace_name(&p, &req.new_name, Some(&current.name))?;
    }
    machine_of(&p, &req.machine)
        .map_err(|_| PoolError::invalid(PoolField::Machine, format!("unknown machine `{}`", req.machine)))?;
    let spec = WorkspaceSpec {
        name: req.new_name.clone(),
        machine: req.machine.clone(),
        tags: normalize_tags(&req.tags)?,
        slot: req.slot,
    };
    let replaced: Vec<WorkspaceSpec> = p
        .workspaces
        .iter()
        .map(|w| if w.name == current.name { spec.clone() } else { w.clone() })
        .collect();
    let candidate = with_workspaces(&p, replaced);
    candidate
        .validate_workspaces()
        .map_err(|e| PoolError::invalid(PoolField::Machine, e.to_string()))?;
    let entries: Vec<PoolEntry<WorkspaceSpec>> = pool
        .workspaces()
        .iter()
        .map(|w| {
            if w.name == current.name {
                PoolEntry {
                    was: Some(current.name.clone()),
                    value: spec.clone(),
                }
            } else {
                PoolEntry::kept(w.clone())
            }
        })
        .collect();

    let renamed = spec.name != current.name;
    let moved = spec.machine != current.machine;
    if !renamed && !moved {
        let saved = pool
            .save(&kept(pool.machines()), &entries)
            .map_err(|e| persistence(e, ""))?;
        return Ok(change(&saved, &candidate, &spec.name, None, None, HooksOutcome::Skipped, Vec::new()));
    }

    let _old = lock(shelbi_state::lock_workspace(project, &current.name))?;
    let _new = if renamed {
        Some(lock(shelbi_state::lock_workspace(project, &spec.name))?)
    } else {
        None
    };
    check_idle(ops, &p, &current, "renaming or moving it")?;
    let old_machine = machine_of(&p, &current.machine)?.clone();
    let new_machine = machine_of(&p, &spec.machine)?.clone();
    let old_state = ops.worktree(&old_machine, &current);
    match &old_state {
        WorktreeState::Unknown(reason) => return Err(unreachable(&old_machine, &current, reason)),
        WorktreeState::AtRisk(reason) => return Err(at_risk(&old_machine, &current, reason, "renaming or moving it")),
        WorktreeState::Missing | WorktreeState::Clean => {}
    }
    let new_state = ops.worktree(&new_machine, &spec);
    if let WorktreeState::Unknown(reason) = &new_state {
        return Err(unreachable(&new_machine, &spec, reason));
    }
    let new_path = ws::workspace_worktree(&new_machine, &spec);
    let previous_name = renamed.then(|| current.name.clone());
    let previous_machine = moved.then(|| current.machine.clone());

    if !moved {
        // Same machine: move the worktree, keeping every file in it. A
        // destination that's already there while the source is gone is an
        // earlier interrupted rename: adopt it.
        let did_move = match (&old_state, &new_state) {
            (WorktreeState::Clean, WorktreeState::Missing) => {
                ops.move_worktree(&candidate, &old_machine, &current, &spec)
                    .map_err(PoolError::Provisioning)?;
                true
            }
            (WorktreeState::Clean, _) => {
                return Err(PoolError::Unsafe(format!(
                    "something is already at {} — remove it or pick another name",
                    new_path.display()
                )))
            }
            _ => false,
        };
        let saved = match pool.save(&kept(pool.machines()), &entries) {
            Ok(saved) => saved,
            Err(e) => {
                let note = if did_move {
                    match ops.move_worktree(&candidate, &old_machine, &spec, &current) {
                        Ok(()) => "the worktree was moved back".to_string(),
                        Err(back) => format!(
                            "the worktree stays at {} (moving it back failed: {back}); retrying \
                             the rename adopts it",
                            new_path.display()
                        ),
                    }
                } else {
                    String::new()
                };
                return Err(persistence(e, &note));
            }
        };
        let mut warnings = Vec::new();
        if renamed {
            carry_migration_state(project, &current.name, &spec.name, &mut warnings);
        }
        return Ok(change(&saved, &candidate, &spec.name, previous_name, None, HooksOutcome::Skipped, warnings));
    }

    // A new machine: provision there first, then declare, then retire the
    // old worktree.
    let created = new_state == WorktreeState::Missing;
    let undo = |note: &str| -> String {
        if !created {
            return note.to_string();
        }
        match ops.remove_worktree(&candidate, &new_machine, &spec) {
            Ok(()) => note.to_string(),
            Err(e) => format!(
                "{note}{}the new worktree at {} on `{}` couldn't be removed: {e}",
                if note.is_empty() { "" } else { "; " },
                new_path.display(),
                new_machine.name
            ),
        }
    };
    if let Err(e) = ops.materialize(&candidate, &new_machine, &spec) {
        return Err(PoolError::Provisioning(join(&e, &undo(""))));
    }
    let hooks = match wire(ops, &candidate, &new_machine, &spec) {
        Ok(hooks) => hooks,
        Err(e) => {
            undo("");
            return Err(e);
        }
    };
    let saved = match pool.save(&kept(pool.machines()), &entries) {
        Ok(saved) => saved,
        Err(e) => return Err(persistence(e, &undo(""))),
    };
    let mut warnings = Vec::new();
    if old_state == WorktreeState::Clean {
        let old_path = ws::workspace_worktree(&old_machine, &current);
        match ops.worktree(&old_machine, &current) {
            WorktreeState::Clean => {
                if let Err(e) = ops.remove_worktree(&candidate, &old_machine, &current) {
                    warnings.push(format!(
                        "couldn't remove the old worktree at {} on `{}`: {e} — remove it by hand",
                        old_path.display(),
                        old_machine.name
                    ));
                }
            }
            WorktreeState::Missing => {}
            WorktreeState::AtRisk(reason) | WorktreeState::Unknown(reason) => warnings.push(format!(
                "left the old worktree at {} on `{}` in place: {reason}",
                old_path.display(),
                old_machine.name
            )),
        }
    }
    Ok(change(&saved, &candidate, &spec.name, previous_name, previous_machine, hooks, warnings))
}

/// Remove a workspace: refuse an active task and work at risk (unless
/// `force`), stop its session, remove its worktree, then drop it from the
/// config. A worktree that can't be removed is an error and the workspace
/// stays declared; a config write that fails after the worktree is gone is
/// an error whose retry finishes the removal.
pub fn remove_workspace(project: &str, req: &RemoveWorkspace) -> Result<WorkspaceRemoved> {
    remove_workspace_with(&LiveOps, project, req)
}

pub fn remove_workspace_with(
    ops: &dyn PoolOps,
    project: &str,
    req: &RemoveWorkspace,
) -> Result<WorkspaceRemoved> {
    ops.check_version().map_err(PoolError::Version)?;
    let _pool = lock(shelbi_state::lock_pool_config(project))?;
    let (p, pool) = load(project)?;
    let current = find_workspace(&p, &req.name)?.clone();
    if let Some(expected) = &req.expected {
        if *expected != current {
            return Err(changed_elsewhere("workspace", &req.name));
        }
    }
    let _ws = lock(shelbi_state::lock_workspace(project, &current.name))?;
    let machine = machine_of(&p, &current.machine)?.clone();
    if !req.force {
        match ops.active_task(&p, &current.name) {
            Ok(None) => {}
            Ok(Some(task)) => {
                return Err(PoolError::ActiveTask {
                    workspace: current.name.clone(),
                    task,
                })
            }
            Err(reason) => {
                return Err(PoolError::Unsafe(format!(
                    "can't tell whether workspace `{}` has an active task: {reason}",
                    current.name
                )))
            }
        }
        match ops.worktree(&machine, &current) {
            WorktreeState::Missing | WorktreeState::Clean => {}
            WorktreeState::AtRisk(reason) => return Err(at_risk(&machine, &current, &reason, "removing it")),
            WorktreeState::Unknown(reason) => return Err(unreachable(&machine, &current, &reason)),
        }
    }
    let mut warnings = Vec::new();
    if let Err(e) = ops.kill_session(&p, &current) {
        warnings.push(format!("killing pane for `{}`: {e}", current.name));
    }
    if let Err(e) = ops.remove_worktree(&p, &machine, &current) {
        let path = ws::workspace_worktree(&machine, &current);
        if !req.force {
            return Err(PoolError::Provisioning(format!(
                "couldn't remove workspace `{}`'s worktree at {}: {e}; the workspace is still declared",
                current.name,
                path.display()
            )));
        }
        warnings.push(format!(
            "couldn't remove the worktree at {} on `{}`: {e} — remove it by hand",
            path.display(),
            machine.name
        ));
    }
    let workspaces: Vec<_> = kept(pool.workspaces())
        .into_iter()
        .filter(|w| w.value.name != current.name)
        .collect();
    pool.save(&kept(pool.machines()), &workspaces).map_err(|e| {
        persistence(
            e,
            "its worktree was already removed; retry to finish removing it from the config",
        )
    })?;
    Ok(WorkspaceRemoved {
        name: current.name,
        machine: current.machine,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Machines

/// Declare a new machine. Nothing is provisioned: SSH readiness and the
/// remote `shelbi` install are their own steps.
pub fn add_machine(project: &str, fields: &MachineFields) -> Result<MachineChange> {
    add_machine_with(&LiveOps, project, fields)
}

pub fn add_machine_with(
    ops: &dyn PoolOps,
    project: &str,
    fields: &MachineFields,
) -> Result<MachineChange> {
    ops.check_version().map_err(PoolError::Version)?;
    let _pool = lock(shelbi_state::lock_pool_config(project))?;
    let (p, pool) = load(project)?;
    let machine = plan_machine(&p, fields, None)?;
    let mut machines = kept(pool.machines());
    machines.push(PoolEntry::new(machine.clone()));
    let saved = pool
        .save(&machines, &kept(pool.workspaces()))
        .map_err(|e| persistence(e, ""))?;
    Ok(machine_change(&saved, &machine.name, None))
}

/// Edit a machine. A rename carries its workspaces along; a new kind, host
/// or directory is checked against their worktrees and sessions first.
pub fn update_machine(project: &str, req: &UpdateMachine) -> Result<MachineChange> {
    update_machine_with(&LiveOps, project, req)
}

pub fn update_machine_with(
    ops: &dyn PoolOps,
    project: &str,
    req: &UpdateMachine,
) -> Result<MachineChange> {
    ops.check_version().map_err(PoolError::Version)?;
    let _pool = lock(shelbi_state::lock_pool_config(project))?;
    let (p, pool) = load(project)?;
    let current = p
        .machine(&req.name)
        .ok_or_else(|| {
            PoolError::NotFound(format!(
                "machine `{}` not declared in project `{}` (known: {})",
                req.name,
                p.name,
                names(p.machines.iter().map(|m| m.name.as_str()))
            ))
        })?
        .clone();
    if let Some(expected) = &req.expected {
        if *expected != current {
            return Err(changed_elsewhere("machine", &req.name));
        }
    }
    let machine = plan_machine(&p, &req.fields, Some(&current))?;
    let renamed = machine.name != current.name;
    let relocated = machine.host() != current.host() || machine.work_dir != current.work_dir;
    let on_machine: Vec<WorkspaceSpec> = p
        .workspaces
        .iter()
        .filter(|w| w.machine == current.name)
        .cloned()
        .collect();

    let mut _locks = Vec::new();
    if relocated {
        for w in &on_machine {
            _locks.push(lock(shelbi_state::lock_workspace(project, &w.name))?);
        }
        let mut present = Vec::new();
        for w in &on_machine {
            check_idle(ops, &p, w, "changing its machine's host or directory")?;
            match ops.worktree(&current, w) {
                WorktreeState::Missing => {}
                WorktreeState::Unknown(reason) => return Err(unreachable(&current, w, &reason)),
                WorktreeState::Clean | WorktreeState::AtRisk(_) => present.push(w),
            }
        }
        for w in present {
            match ops.worktree(&machine, w) {
                WorktreeState::Clean | WorktreeState::AtRisk(_) => {}
                WorktreeState::Unknown(reason) => return Err(unreachable(&machine, w, &reason)),
                WorktreeState::Missing => {
                    return Err(PoolError::Unsafe(format!(
                        "workspace `{}` has a worktree at {} on {}; the new location has none, so the \
                         change would orphan it — remove the workspace, or move its checkout to {} first",
                        w.name,
                        ws::workspace_worktree(&current, w).display(),
                        host_label(&current),
                        ws::workspace_worktree(&machine, w).display(),
                    )))
                }
            }
        }
    }

    let machines: Vec<PoolEntry<Machine>> = pool
        .machines()
        .iter()
        .map(|m| {
            if m.name == current.name {
                PoolEntry {
                    was: Some(current.name.clone()),
                    value: machine.clone(),
                }
            } else {
                PoolEntry::kept(m.clone())
            }
        })
        .collect();
    let workspaces: Vec<PoolEntry<WorkspaceSpec>> = pool
        .workspaces()
        .iter()
        .map(|w| {
            let mut value = w.clone();
            if renamed && value.machine == current.name {
                value.machine = machine.name.clone();
            }
            PoolEntry {
                was: Some(w.name.clone()),
                value,
            }
        })
        .collect();
    let saved = pool
        .save(&machines, &workspaces)
        .map_err(|e| persistence(e, ""))?;
    Ok(machine_change(&saved, &machine.name, renamed.then(|| current.name.clone())))
}

/// Validate machine fields into a [`Machine`]. `current` is the machine
/// being edited (its name may stay as it is; its forwarding override is
/// kept).
pub fn plan_machine(p: &Project, fields: &MachineFields, current: Option<&Machine>) -> Result<Machine> {
    let name = fields.name.trim();
    if current.map(|c| c.name.as_str()) != Some(name) {
        if name.is_empty() {
            return Err(PoolError::invalid(PoolField::Name, "enter a name for the machine"));
        }
        if name.chars().any(char::is_control) {
            return Err(PoolError::invalid(
                PoolField::Name,
                format!("machine name `{}` can't contain control characters", name.escape_debug()),
            ));
        }
        let slug = shelbi_core::normalize_project_name(name).ok();
        for m in &p.machines {
            if current.is_some_and(|c| c.name == m.name) {
                continue;
            }
            if m.name.eq_ignore_ascii_case(name) {
                return Err(PoolError::invalid(PoolField::Name, format!("machine `{}` already exists", m.name)));
            }
            if slug.is_some() && shelbi_core::normalize_project_name(&m.name).ok() == slug {
                return Err(PoolError::invalid(
                    PoolField::Name,
                    format!("machine name `{name}` is too close to existing machine `{}`", m.name),
                ));
            }
        }
    }
    let host = match fields.kind {
        MachineKind::Local => None,
        MachineKind::Ssh => {
            let host = fields.host.as_deref().map(str::trim).filter(|h| !h.is_empty());
            match host {
                Some(h) if !usable_ssh_host(h) => {
                    return Err(PoolError::invalid(
                        PoolField::Host,
                        format!("`{h}` isn't a valid SSH host: no spaces, and it can't start with `-`"),
                    ))
                }
                // Without a host, SSH connects to the machine's name.
                None if !usable_ssh_host(name) => {
                    return Err(PoolError::invalid(PoolField::Host, "enter the machine's SSH host"))
                }
                _ => {}
            }
            host.map(str::to_string)
        }
    };
    let work_dir = fields.work_dir.trim();
    if work_dir.is_empty() {
        return Err(PoolError::invalid(
            PoolField::WorkDir,
            "enter the project's directory on the machine",
        ));
    }
    if work_dir.chars().any(char::is_control) {
        return Err(PoolError::invalid(
            PoolField::WorkDir,
            format!("`{}` isn't a valid path", work_dir.escape_debug()),
        ));
    }
    // Nothing expands `~` at run time (a local command gets the path as is;
    // a remote one gets it shell-quoted), so it's resolved here, where only
    // this machine's home is known.
    let tilde = work_dir == "~" || work_dir.starts_with("~/");
    let work_dir = match fields.kind {
        MachineKind::Local if tilde => shelbi_state::expand_tilde_str(work_dir),
        _ => PathBuf::from(work_dir),
    };
    if !work_dir.is_absolute() {
        let hint = if tilde {
            " — write the remote home directory out in full"
        } else {
            ""
        };
        return Err(PoolError::invalid(
            PoolField::WorkDir,
            format!("`{}` isn't an absolute path{hint}", work_dir.display()),
        ));
    }
    Ok(Machine {
        name: name.to_string(),
        kind: fields.kind,
        work_dir,
        host,
        tags: normalize_tags(&fields.tags)?,
        forward: current.and_then(|c| c.forward),
    })
}

/// A string `ssh` takes as its destination: no whitespace or control
/// characters, and no leading `-` (which `ssh` would read as an option).
fn usable_ssh_host(h: &str) -> bool {
    !h.is_empty() && !h.starts_with('-') && !h.chars().any(|c| c.is_whitespace() || c.is_control())
}

// ---------------------------------------------------------------------------
// Field helpers

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
    shelbi_core::validate_task_id(name)
        .map_err(|e| PoolError::invalid(PoolField::Name, format!("invalid workspace name `{name}`: {e}")))?;
    let slug = shelbi_core::normalize_project_name(name).map_err(|_| {
        PoolError::invalid(PoolField::Name, format!("workspace name `{name}` does not reduce to a valid slug"))
    })?;
    for w in p.workspaces.iter().filter(|w| Some(w.name.as_str()) != except) {
        if w.name == name {
            return Err(PoolError::invalid(PoolField::Name, format!("workspace `{name}` already exists")));
        }
        if shelbi_core::normalize_project_name(&w.name).is_ok_and(|s| s == slug) {
            return Err(PoolError::invalid(
                PoolField::Name,
                format!("workspace name `{name}` slug-collides with existing workspace `{}`", w.name),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internals

fn lock<T>(r: shelbi_core::Result<T>) -> Result<T> {
    r.map_err(|e| PoolError::Persistence(format!("couldn't take the config lock: {e}")))
}

/// The project (for runners, base branch and validation) with its pool as
/// [`PoolConfig`] read it, which is what a save is checked against.
fn load(project: &str) -> Result<(Project, PoolConfig)> {
    let p = shelbi_state::load_project(project).map_err(|e| PoolError::Persistence(e.to_string()))?;
    let pool = PoolConfig::load(project).map_err(|e| persistence(e, ""))?;
    let mut p = p;
    p.machines = pool.machines().to_vec();
    p.workspaces = pool.workspaces().to_vec();
    Ok((p, pool))
}

fn with_workspaces(p: &Project, workspaces: Vec<WorkspaceSpec>) -> Project {
    let mut c = p.clone();
    c.workspaces = workspaces;
    c
}

fn kept<T: Clone + shelbi_state::pool_config::Named>(items: &[T]) -> Vec<PoolEntry<T>> {
    items.iter().cloned().map(PoolEntry::kept).collect()
}

fn find_workspace<'a>(p: &'a Project, name: &str) -> Result<&'a WorkspaceSpec> {
    p.workspace(name).ok_or_else(|| {
        PoolError::NotFound(format!(
            "workspace `{name}` not declared in project `{}` (known: {})",
            p.name,
            names(p.workspaces.iter().map(|w| w.name.as_str()))
        ))
    })
}

fn machine_of<'a>(p: &'a Project, name: &str) -> Result<&'a Machine> {
    p.machine(name)
        .ok_or_else(|| PoolError::NotFound(format!("workspace references unknown machine `{name}`")))
}

fn names<'a>(it: impl Iterator<Item = &'a str>) -> String {
    it.collect::<Vec<_>>().join(", ")
}

fn changed_elsewhere(kind: &str, name: &str) -> PoolError {
    PoolError::Conflict(format!(
        "{kind} `{name}` changed since it was loaded; reload and try again"
    ))
}

fn persistence(e: PoolConfigError, note: &str) -> PoolError {
    match e {
        PoolConfigError::Conflict { .. } => PoolError::Conflict(join(&e.to_string(), note)),
        PoolConfigError::Invalid(m) => PoolError::Invalid {
            field: None,
            message: join(&m, note),
        },
        PoolConfigError::File { .. } => PoolError::Persistence(join(&e.to_string(), note)),
    }
}

fn join(message: &str, note: &str) -> String {
    if note.is_empty() {
        message.to_string()
    } else {
        format!("{message}; {note}")
    }
}

fn host_label(m: &Machine) -> String {
    match m.host() {
        shelbi_core::Host::Local => "this machine".to_string(),
        shelbi_core::Host::Ssh { host } => format!("`{host}`"),
    }
}

fn unreachable(m: &Machine, w: &WorkspaceSpec, reason: &str) -> PoolError {
    PoolError::Unsafe(format!(
        "can't check workspace `{}`'s worktree on machine `{}`: {reason}",
        w.name, m.name
    ))
}

fn at_risk(m: &Machine, w: &WorkspaceSpec, reason: &str, doing: &str) -> PoolError {
    PoolError::Unsafe(format!(
        "workspace `{}`'s worktree at {} {reason} — commit and push it, or discard it, before {doing}",
        w.name,
        ws::workspace_worktree(m, w).display()
    ))
}

/// Refuse a workspace that's in use: an active task, open tasks assigned to
/// it, or a live session.
fn check_idle(ops: &dyn PoolOps, p: &Project, w: &WorkspaceSpec, doing: &str) -> Result<()> {
    match ops.active_task(p, &w.name) {
        Ok(None) => {}
        Ok(Some(task)) => {
            return Err(PoolError::ActiveTask {
                workspace: w.name.clone(),
                task,
            })
        }
        Err(reason) => {
            return Err(PoolError::Unsafe(format!(
                "can't tell whether workspace `{}` has an active task: {reason}",
                w.name
            )))
        }
    }
    match ops.open_assignments(p, &w.name) {
        Ok(tasks) if tasks.is_empty() => {}
        Ok(tasks) => {
            return Err(PoolError::Unsafe(format!(
                "workspace `{}` is assigned open task{} {} — unassign {} before {doing}",
                w.name,
                if tasks.len() == 1 { "" } else { "s" },
                tasks.join(", "),
                if tasks.len() == 1 { "it" } else { "them" },
            )))
        }
        Err(reason) => {
            return Err(PoolError::Unsafe(format!(
                "can't tell which tasks are assigned to workspace `{}`: {reason}",
                w.name
            )))
        }
    }
    match ops.session(p, w) {
        SessionState::Dead => Ok(()),
        SessionState::Alive { user_shell } => Err(PoolError::Unsafe(format!(
            "workspace `{}` has a live {} — stop it before {doing}",
            w.name,
            if user_shell { "shell" } else { "session" }
        ))),
        SessionState::Unreachable(reason) => Err(PoolError::Unsafe(format!(
            "can't check workspace `{}`'s session: {reason}",
            w.name
        ))),
    }
}

/// A renamed workspace keeps its tmux-cutover migration state, so one still
/// pending stays gated under its new name.
fn carry_migration_state(project: &str, from: &str, to: &str, warnings: &mut Vec<String>) {
    let carried = shelbi_state::workspace_migration_state(project, from).and_then(|state| match state {
        Some(state) => shelbi_state::set_workspace_migration_state(project, to, state).map(|_| ()),
        None => Ok(()),
    });
    if let Err(e) = carried {
        warnings.push(format!("couldn't carry the migration state of `{from}` over to `{to}`: {e}"));
    }
}

/// Wire hooks when the project runs Claude Code.
fn wire(ops: &dyn PoolOps, p: &Project, machine: &Machine, w: &WorkspaceSpec) -> Result<HooksOutcome> {
    if !uses_claude(p) {
        return Ok(HooksOutcome::Skipped);
    }
    match ops.wire_hooks(p, machine, w).map_err(PoolError::Provisioning)? {
        HookWiring::Wired => Ok(HooksOutcome::Wired),
        HookWiring::SelfHealed => Ok(HooksOutcome::SelfHealed),
        HookWiring::MergeRequired(message) => Err(PoolError::HooksMergeRequired {
            workspace: w.name.clone(),
            message,
        }),
    }
}

/// Only Claude Code reads `settings.local.json`; other runners skip the
/// wiring.
fn uses_claude(p: &Project) -> bool {
    p.default_runner_spec()
        .map(|r| shelbi_agent::RunnerAdapter::for_command(&r.command).is_claude())
        .unwrap_or(false)
}

fn change(
    saved: &PoolConfig,
    p: &Project,
    name: &str,
    previous_name: Option<String>,
    previous_machine: Option<String>,
    hooks: HooksOutcome,
    warnings: Vec<String>,
) -> WorkspaceChange {
    let mut now = p.clone();
    now.machines = saved.machines().to_vec();
    now.workspaces = saved.workspaces().to_vec();
    let workspace = now
        .workspace(name)
        .cloned()
        .expect("the saved pool holds the workspace just written");
    let worktree = now
        .machine(&workspace.machine)
        .map(|m| ws::workspace_worktree(m, &workspace))
        .unwrap_or_default();
    WorkspaceChange {
        slot: now.workspace_slot(&workspace),
        workspace,
        previous_name,
        previous_machine,
        worktree,
        hooks,
        warnings,
    }
}

fn machine_change(saved: &PoolConfig, name: &str, previous_name: Option<String>) -> MachineChange {
    MachineChange {
        machine: saved
            .machine(name)
            .cloned()
            .expect("the saved pool holds the machine just written"),
        previous_name,
        workspaces: saved
            .workspaces()
            .iter()
            .filter(|w| w.machine == name)
            .map(|w| w.name.clone())
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// The live implementation

/// [`PoolOps`] against the real issue store, sessions, git and SSH.
pub struct LiveOps;

impl PoolOps for LiveOps {
    fn check_version(&self) -> Result<(), String> {
        shelbi_state::ensure_daemon_matches_for_mutation().map_err(|e| e.to_string())
    }

    fn active_task(&self, project: &Project, workspace: &str) -> Result<Option<String>, String> {
        let store = shelbi_state::issue_store_for(&project.name).map_err(|e| e.to_string())?;
        for column in [Column::in_progress(), Column::review()] {
            let tasks = store.list_in_status(&column).map_err(|e| e.to_string())?;
            if let Some(tf) = tasks
                .into_iter()
                .find(|tf| tf.task.assigned_to.as_deref() == Some(workspace))
            {
                return Ok(Some(tf.task.id));
            }
        }
        Ok(None)
    }

    fn open_assignments(&self, project: &Project, workspace: &str) -> Result<Vec<String>, String> {
        let board = shelbi_state::read_board_with_cfg(&project.name, &project.issue_tracker)
            .map_err(|e| e.to_string())?;
        if board.is_cold() {
            return Err("the board hasn't loaded yet".to_string());
        }
        let mut issues = board.into_issues();
        shelbi_state::fold_assignment_overlay(&project.name, &project.issue_tracker, &mut issues);
        Ok(issues
            .into_iter()
            .filter(|tf| tf.task.assigned_to.as_deref() == Some(workspace))
            .map(|tf| tf.task.id)
            .collect())
    }

    fn session(&self, project: &Project, workspace: &WorkspaceSpec) -> SessionState {
        let Some(machine) = project.machine(&workspace.machine) else {
            return SessionState::Dead;
        };
        let target = match ws::workspace_target(project, workspace) {
            Ok(t) => t,
            Err(e) => return SessionState::Unreachable(e.to_string()),
        };
        match ws::probe_workspace_slot(&machine.host(), &target, ws::probe_deadline()) {
            ws::SlotProbe::Dead => SessionState::Dead,
            ws::SlotProbe::Alive { user_shell } => SessionState::Alive { user_shell },
            ws::SlotProbe::Unreachable { reason } => SessionState::Unreachable(reason),
        }
    }

    fn worktree(&self, machine: &Machine, workspace: &WorkspaceSpec) -> WorktreeState {
        worktree_state(machine, workspace)
    }

    fn materialize(&self, project: &Project, machine: &Machine, workspace: &WorkspaceSpec) -> Result<(), String> {
        ws::materialize_idle_worktree(&project.name, machine, workspace, project.base_branch())
            .map_err(|e| e.to_string())
    }

    fn wire_hooks(&self, project: &Project, machine: &Machine, workspace: &WorkspaceSpec) -> Result<HookWiring, String> {
        let block = ws::render_workspace_settings_preferring_agent(project, None).map_err(|e| e.to_string())?;
        let worktree = ws::workspace_worktree(machine, workspace);
        match ws::wire_settings_local(&machine.host(), &worktree, &workspace.name, &block)
            .map_err(|e| e.to_string())?
        {
            ws::SettingsWireResult::Wired => Ok(HookWiring::Wired),
            ws::SettingsWireResult::SelfHealed => {
                ws::record_settings_selfheal(&workspace.name);
                Ok(HookWiring::SelfHealed)
            }
            ws::SettingsWireResult::MergeRequired { message } => Ok(HookWiring::MergeRequired(message)),
        }
    }

    fn move_worktree(&self, project: &Project, machine: &Machine, from: &WorkspaceSpec, to: &WorkspaceSpec) -> Result<(), String> {
        let host = machine.host();
        let _git = shelbi_state::lock_git_worktrees(&project.name).map_err(|e| e.to_string())?;
        let repo = machine.work_dir.to_string_lossy().into_owned();
        let src = ws::workspace_worktree(machine, from).to_string_lossy().into_owned();
        let dst = ws::workspace_worktree(machine, to).to_string_lossy().into_owned();
        let out = shelbi_ssh::run_with_deadline(&host, ["git", "-C", &repo, "worktree", "move", &src, &dst], PROBE_DEADLINE)
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(shelbi_ssh::describe_failure(&host, &out));
        }
        Ok(())
    }

    fn remove_worktree(&self, project: &Project, machine: &Machine, workspace: &WorkspaceSpec) -> Result<(), String> {
        ws::remove_workspace_worktree(&project.name, machine, workspace).map_err(|e| e.to_string())?;
        let path = ws::workspace_worktree(machine, workspace);
        match path_exists(&machine.host(), &path.to_string_lossy()) {
            Ok(false) => Ok(()),
            Ok(true) => Err(format!("{} is still there", path.display())),
            Err(e) => Err(format!("couldn't confirm {} is gone: {e}", path.display())),
        }
    }

    fn kill_session(&self, project: &Project, workspace: &WorkspaceSpec) -> Result<(), String> {
        let machine = project
            .machine(&workspace.machine)
            .ok_or_else(|| format!("unknown machine `{}`", workspace.machine))?;
        let target = ws::workspace_target(project, workspace).map_err(|e| e.to_string())?;
        ws::kill_workspace_pane(&machine.host(), &target, &workspace.name).map_err(|e| e.to_string())
    }
}

/// Probe a workspace's worktree for work a removal would lose: uncommitted
/// changes (Shelbi's own `.claude/` / `.shelbi/` footprint aside), commits a
/// detached HEAD holds that no branch, tag or remote has, and branch commits
/// not on any remote (when the repo has a remote). A directory that isn't a
/// worktree counts unless it's empty.
pub fn worktree_state(machine: &Machine, workspace: &WorkspaceSpec) -> WorktreeState {
    let host = machine.host();
    let wt = ws::workspace_worktree(machine, workspace).to_string_lossy().into_owned();
    let run = |argv: &[&str]| shelbi_ssh::run_with_deadline(&host, argv, PROBE_DEADLINE);
    let capture = |argv: &[&str]| -> Result<String, String> {
        let out = run(argv).map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(shelbi_ssh::describe_failure(&host, &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    match path_exists(&host, &wt) {
        Ok(false) => return WorktreeState::Missing,
        Ok(true) => {}
        Err(e) => return WorktreeState::Unknown(e),
    }
    match path_exists(&host, &format!("{wt}/.git")) {
        Ok(true) => {}
        Ok(false) => {
            return match capture(&["ls", "-A", &wt]) {
                Ok(listing) if listing.trim().is_empty() => WorktreeState::Clean,
                Ok(_) => WorktreeState::AtRisk("isn't a git worktree but holds files".to_string()),
                Err(e) => WorktreeState::Unknown(e),
            }
        }
        Err(e) => return WorktreeState::Unknown(e),
    }
    let porcelain = match capture(&["git", "-C", &wt, "status", "--porcelain", "-z"]) {
        Ok(s) => s,
        Err(e) => return WorktreeState::Unknown(e),
    };
    let dirty = ws::user_dirty_porcelain_lines(&porcelain);
    if !dirty.is_empty() {
        return WorktreeState::AtRisk(format!(
            "has uncommitted changes ({})",
            dirty.iter().take(5).copied().collect::<Vec<_>>().join(", ")
        ));
    }
    let head = match capture(&["git", "-C", &wt, "rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(s) => s.trim().to_string(),
        Err(e) => return WorktreeState::Unknown(e),
    };
    let unsaved = if head == "HEAD" {
        capture(&["git", "-C", &wt, "rev-list", "--count", "HEAD", "--not", "--branches", "--tags", "--remotes"])
            .map(|n| (n.trim().parse::<u64>().unwrap_or(0), "commits on no branch"))
    } else {
        match capture(&["git", "-C", &wt, "remote"]) {
            Ok(remotes) if remotes.trim().is_empty() => Ok((0, "")),
            Ok(_) => capture(&["git", "-C", &wt, "rev-list", "--count", &head, "--not", "--remotes"])
                .map(|n| (n.trim().parse::<u64>().unwrap_or(0), "unpushed commits")),
            Err(e) => Err(e),
        }
    };
    match unsaved {
        Ok((0, _)) => WorktreeState::Clean,
        Ok((n, what)) => WorktreeState::AtRisk(format!("has {n} {what}")),
        Err(e) => WorktreeState::Unknown(e),
    }
}

fn path_exists(host: &shelbi_core::Host, path: &str) -> Result<bool, String> {
    let out = shelbi_ssh::run_with_deadline(host, ["test", "-e", path], PROBE_DEADLINE).map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(shelbi_ssh::describe_failure(host, &out)),
    }
}

#[cfg(test)]
mod tests;
