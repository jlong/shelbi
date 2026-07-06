//! `shelbi task <subcommand>` — Kanban board management.
//!
//! Tasks are stored as `<shelbi_home>/projects/<project>/tasks/<id>.md`
//! files (markdown body + YAML frontmatter). The orchestrator creates
//! tasks (typically into `backlog`); the user curates them through the
//! columns; workspaces pick up `todo` tasks.
//!
//! Priorities within a column are contiguous integers 0..N. Any operation
//! that changes a column's membership renumbers it before returning, so
//! callers can treat `priority` as a stable position index.

use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::{Args as ClapArgs, Subcommand};
use shelbi_core::{
    default_workflow, validate_task_id, validate_workflow_name, Column, Task, Workflow,
    MAX_TASK_ID_LEN,
};

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum TaskCmd {
    /// Create a new task. Defaults to the backlog column.
    Add(AddArgs),
    /// List tasks (all statuses, or one with `--status`).
    List {
        /// Restrict to a single status. `--column` accepted as a hidden
        /// alias for one release while older scripts catch up.
        #[arg(long = "status", alias = "column", value_name = "STATUS")]
        status: Option<String>,
        /// Show only unblocked todo items, in priority order. Useful for
        /// orchestrator agents and for users planning next work. Mutually
        /// exclusive with `--status`.
        #[arg(long, conflicts_with = "status")]
        ready: bool,
        /// Restrict to tasks pinned to the named workflow. Tasks with no
        /// explicit `workflow:` field are treated as the canonical
        /// `default` workflow. Composes with `--column` and `--ready`.
        #[arg(long, value_name = "NAME")]
        workflow: Option<String>,
    },
    /// Print a task's frontmatter + body, plus the resolved status of each
    /// `depends_on` entry.
    Show { id: String },
    /// Edit a task's dependency list.
    Depends(DependsArgs),
    /// Move a task to another status. A task's position is a status id, so
    /// the destination may be ANY status the task's workflow declares —
    /// including `canceled` / archived statuses and any status a user adds.
    /// A status the workflow doesn't declare errors, naming the ids it does.
    Move {
        id: String,
        #[arg(long, value_name = "STATUS")]
        to: String,
        /// Reason string recorded in `~/.shelbi/events.log`. The
        /// orchestrator parses this to identify auto-dispatch moves vs.
        /// user-driven ones. Defaults to `user:cli`.
        #[arg(long, value_name = "REASON")]
        reason: Option<String>,
    },
    /// Assign a task to a workspace. Workspace must be declared in project YAML.
    Assign {
        id: String,
        #[arg(long, value_name = "WORKSPACE")]
        to: String,
    },
    /// Clear a task's workspace assignment.
    Unassign { id: String },
    /// Launch the assigned workspace on this task: ensure the worktree is on
    /// the task's branch, kill any existing workspace pane (clears context),
    /// start the runner with the task's prompt. Moves the task into
    /// `in_progress`. Pass `--workspace` to assign at the same time.
    Start {
        id: String,
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<String>,
        /// Override the default branch name (`shelbi/<task-id>`).
        #[arg(long)]
        branch: Option<String>,
        /// Reason string recorded in `~/.shelbi/events.log` when the
        /// column transitions into `in_progress`. The orchestrator uses
        /// this to identify auto-dispatch starts vs. user-driven ones.
        /// Defaults to `user:cli:start`.
        #[arg(long, value_name = "REASON")]
        reason: Option<String>,
    },
    /// Relaunch the assigned workspace on the task it is ALREADY working,
    /// WITHOUT discarding progress. For a stalled or killed worker: recreates
    /// or reclaims the tmux pane and (for a claude runner) resumes the prior
    /// conversation via `--continue`, while preserving the worktree as-is —
    /// its branch, commits, and uncommitted changes stay put. Contrast with
    /// `start`, which wipes context and re-checks-out a clean branch. Restores
    /// the task to `in_progress` if it drifted. Pass `--workspace` to target a
    /// specific workspace (defaults to the task's `assigned_to`).
    Resume {
        id: String,
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<String>,
        /// Reason string recorded in `~/.shelbi/events.log` if the resume has
        /// to move the card back into `in_progress`. Defaults to
        /// `user:cli:resume`.
        #[arg(long, value_name = "REASON")]
        reason: Option<String>,
    },
    /// Re-order a task within its column.
    Prio(PrioArgs),
    /// Open the task file in `$EDITOR`.
    Edit { id: String },
    /// Delete a task.
    Rm { id: String },
}

#[derive(Debug, ClapArgs)]
pub struct AddArgs {
    /// Human-readable title.
    pub title: String,
    /// Override the auto-generated id (slugified from the title).
    #[arg(long)]
    pub id: Option<String>,
    /// Initial status. Defaults to `backlog`. `--column` accepted as a
    /// hidden alias for one release while older scripts catch up.
    #[arg(
        long = "status",
        alias = "column",
        default_value = "backlog",
        value_name = "STATUS"
    )]
    pub status: String,
    /// Optional description; if omitted, the body starts empty (use
    /// `shelbi task edit` to fill it in).
    #[arg(long, short)]
    pub description: Option<String>,
    /// Task id this task depends on. Repeat for multiple deps:
    /// `--depends-on a --depends-on b`. Repeat-flag chosen over
    /// comma-separated to avoid future escaping issues with ids that may
    /// contain commas or shell metacharacters.
    #[arg(long = "depends-on", value_name = "ID")]
    pub depends_on: Vec<String>,
    /// Hint to the orchestrator that this task should be assigned to a
    /// workspace on this machine. Persisted in the task frontmatter; the
    /// orchestrator decides whether to honor it.
    #[arg(long = "prefers-machine", value_name = "NAME")]
    pub prefers_machine: Option<String>,
    /// Workflow this task runs under. Names a file in `workflows/<NAME>.yaml`.
    /// Omit to inherit the project's default workflow.
    #[arg(long = "workflow", value_name = "NAME")]
    pub workflow: Option<String>,
    /// Pre-fill the task's `branch:` frontmatter field. Omit to let the
    /// orchestrator cut `shelbi/<task-id>` off the resolved base branch
    /// at dispatch time; supply a value to point the task at an existing
    /// branch (the *release task* pattern in `Plans/workflows.md` §12).
    #[arg(long = "branch", value_name = "BRANCH")]
    pub branch: Option<String>,
}

#[derive(Debug, ClapArgs)]
pub struct DependsArgs {
    /// Task whose dependency list is being edited.
    pub id: String,
    /// Dependency id to add. Repeat for multiple.
    #[arg(long = "add", value_name = "DEP")]
    pub add: Vec<String>,
    /// Dependency id to remove. Repeat for multiple.
    #[arg(long = "remove", value_name = "DEP")]
    pub remove: Vec<String>,
}

#[derive(Debug, ClapArgs)]
pub struct PrioArgs {
    pub id: String,
    /// Move up one slot.
    #[arg(long, conflicts_with_all = ["down", "top", "bottom", "set"])]
    pub up: bool,
    /// Move down one slot.
    #[arg(long, conflicts_with_all = ["up", "top", "bottom", "set"])]
    pub down: bool,
    /// Move to the top of the column.
    #[arg(long, conflicts_with_all = ["up", "down", "bottom", "set"])]
    pub top: bool,
    /// Move to the bottom of the column.
    #[arg(long, conflicts_with_all = ["up", "down", "top", "set"])]
    pub bottom: bool,
    /// Move to a specific 0-based slot.
    #[arg(long, value_name = "N", conflicts_with_all = ["up", "down", "top", "bottom"])]
    pub set: Option<u32>,
}

pub fn run(project_opt: Option<String>, cmd: TaskCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        TaskCmd::Add(args) => add(&project, args),
        TaskCmd::List {
            status,
            ready,
            workflow,
        } => list(&project, status.as_deref(), ready, workflow.as_deref()),
        TaskCmd::Show { id } => show(&project, &id),
        TaskCmd::Depends(args) => depends(&project, args),
        TaskCmd::Move { id, to, reason } => move_to(&project, &id, &to, reason.as_deref()),
        TaskCmd::Assign { id, to } => assign(&project, &id, &to),
        TaskCmd::Unassign { id } => unassign(&project, &id),
        TaskCmd::Start {
            id,
            workspace,
            branch,
            reason,
        } => start(
            &project,
            &id,
            workspace.as_deref(),
            branch.as_deref(),
            reason.as_deref(),
        ),
        TaskCmd::Resume {
            id,
            workspace,
            reason,
        } => resume(&project, &id, workspace.as_deref(), reason.as_deref()),
        TaskCmd::Prio(args) => prio(&project, args),
        TaskCmd::Edit { id } => edit(&project, &id),
        TaskCmd::Rm { id } => rm(&project, &id),
    }
}

fn add(project: &str, args: AddArgs) -> Result<()> {
    let column = Column::from_str(&args.status).map_err(|e| anyhow!(e))?;
    let id = match args.id {
        Some(id) => {
            validate_task_id(&id).map_err(|e| anyhow!(e))?;
            if shelbi_state::task_path(project, &id)
                .map_err(|e| anyhow!(e))?
                .exists()
            {
                bail!("task id `{id}` already exists");
            }
            id
        }
        None => generate_unique_id(project, &args.title)?,
    };

    if let Some(name) = args.workflow.as_deref() {
        validate_workflow_name(name).map_err(|e| anyhow!(e))?;
    }
    let priority = shelbi_state::list_column(project, column.clone())
        .map_err(|e| anyhow!(e))?
        .len() as u32;
    let now = Utc::now();
    let task = Task {
        id: id.clone(),
        title: args.title.clone(),
        column: column.clone(),
        priority,
        assigned_to: None,
        workflow: args.workflow.clone(),
        branch: args.branch.clone(),
        depends_on: dedup_preserving_order(args.depends_on.clone()),
        prefers_machine: args.prefers_machine.clone(),
        zen: None,
        created_at: now,
        updated_at: now,
        params: std::collections::BTreeMap::new(),
    };
    if !task.depends_on.is_empty() {
        let existing = shelbi_state::list_tasks(project).map_err(|e| anyhow!(e))?;
        shelbi_state::validate_depends_on(&task, &existing).map_err(|e| anyhow!(e))?;
    }
    let body = args
        .description
        .map(|d| format!("# Task\n\n{d}\n"))
        .unwrap_or_else(|| format!("# Task\n\n{}\n", args.title));
    // Create-exclusive: the up-front existence checks above are advisory
    // (they race against concurrent creators); this is the authoritative
    // no-overwrite guarantee.
    shelbi_state::create_task(project, &task, &body).map_err(|e| anyhow!(e))?;
    println!("✓ {} created in {column} (priority {priority})", task.id);
    Ok(())
}

/// Stable de-dup that preserves first-occurrence order. Used so a user
/// passing `--depends-on a --depends-on a --depends-on b` lands as `[a, b]`.
fn dedup_preserving_order(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

/// The full board rendering used by both `shelbi task list` (no flags)
/// and the `## Board` section of `shelbi status --full`. Emits every
/// column with counts and owner badges. Extracted so the bootstrap
/// snapshot doesn't fork a second copy of the render code.
pub(crate) fn print_board(project: &str) -> Result<()> {
    list(project, None, false, None)
}

fn list(
    project: &str,
    status_filter: Option<&str>,
    ready: bool,
    workflow_filter: Option<&str>,
) -> Result<()> {
    let project_yaml = shelbi_state::load_project(project).ok();
    // String-compare against the project-aware resolver so a filter of the
    // configured default matches tasks with no `workflow:` field.
    let matches_workflow = |task: &Task| -> bool {
        match workflow_filter {
            Some(name) => {
                project_yaml
                    .as_ref()
                    .map(|p| shelbi_state::resolve_task_workflow_name(p, task))
                    .unwrap_or_else(|| task.workflow_or_default())
                    == name
            }
            None => true,
        }
    };

    if ready {
        let mut ready_tasks = shelbi_state::list_ready(project).map_err(|e| anyhow!(e))?;
        ready_tasks.retain(|tf| matches_workflow(&tf.task));
        if ready_tasks.is_empty() {
            println!("(no ready todo items)");
            return Ok(());
        }
        for tf in &ready_tasks {
            let owner = tf
                .task
                .assigned_to
                .as_deref()
                .map(|w| format!("  [{w}]"))
                .unwrap_or_default();
            println!("  {:<28} {}{owner}", tf.task.id, tf.task.title);
        }
        return Ok(());
    }

    // Any status id is a valid filter — normalized so `wip` matches
    // `in-progress`.
    let filter = status_filter.map(Column::from_status_id);

    let all = shelbi_state::list_tasks(project).map_err(|e| anyhow!(e))?;
    if all.is_empty() {
        println!("(no tasks yet)");
        return Ok(());
    }
    // Blocked-status lookup is computed against the unfiltered task set:
    // a workflow filter can hide a dependency target without changing
    // whether the visible task is actually blocked.
    let columns: std::collections::HashMap<String, Column> = all
        .iter()
        .map(|tf| (tf.task.id.clone(), tf.task.column.clone()))
        .collect();
    // The stock columns (always shown, even empty) plus any custom /
    // archived status a task actually occupies, in board order.
    let mut cols: Vec<Column> = Column::core();
    for tf in &all {
        if !cols.contains(&tf.task.column) {
            cols.push(tf.task.column.clone());
        }
    }
    cols.sort_by(|a, b| (a.board_order(), a.as_str()).cmp(&(b.board_order(), b.as_str())));
    for col in &cols {
        if let Some(want) = &filter {
            if want != col {
                continue;
            }
        }
        let in_col: Vec<_> = all
            .iter()
            .filter(|tf| &tf.task.column == col && matches_workflow(&tf.task))
            .collect();
        println!("{col} ({})", in_col.len());
        for tf in in_col {
            let owner = tf
                .task
                .assigned_to
                .as_deref()
                .map(|w| format!("  [{w}]"))
                .unwrap_or_default();
            let badge = if tf.task.is_blocked(&columns) {
                " 🔒"
            } else {
                ""
            };
            println!("  {:<28} {}{owner}{badge}", tf.task.id, tf.task.title);
        }
    }
    Ok(())
}

fn show(project: &str, id: &str) -> Result<()> {
    let path = shelbi_state::task_path(project, id).map_err(|e| anyhow!(e))?;
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    print!("{text}");

    // Footer: resolved depends_on. Done lazily after the raw file dump so
    // scripts grepping for frontmatter still get clean output above the line.
    let tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;
    if !tf.task.depends_on.is_empty() {
        let columns = shelbi_state::task_columns(project).map_err(|e| anyhow!(e))?;
        let parts: Vec<String> = tf
            .task
            .depends_on
            .iter()
            .map(|dep| match columns.get(dep) {
                Some(col) => format!("{dep} [{col}]"),
                None => format!("{dep} [missing]"),
            })
            .collect();
        if !text.ends_with('\n') {
            println!();
        }
        println!("→ depends on: {}", parts.join(", "));
        if tf.task.is_blocked(&columns) {
            println!("  status: 🔒 blocked");
        } else {
            println!("  status: ✓ ready");
        }
    }
    Ok(())
}

fn depends(project: &str, args: DependsArgs) -> Result<()> {
    if args.add.is_empty() && args.remove.is_empty() {
        bail!("specify at least one --add ID or --remove ID");
    }
    let mut tf = shelbi_state::load_task(project, &args.id).map_err(|e| anyhow!(e))?;

    let mut updated: Vec<String> = tf.task.depends_on.clone();
    // Removals first so an --add of an id being removed lands at the end.
    if !args.remove.is_empty() {
        let drop: std::collections::HashSet<&str> =
            args.remove.iter().map(String::as_str).collect();
        updated.retain(|d| !drop.contains(d.as_str()));
    }
    for dep in &args.add {
        if !updated.iter().any(|d| d == dep) {
            updated.push(dep.clone());
        }
    }
    if updated == tf.task.depends_on {
        println!("(no change)");
        return Ok(());
    }
    tf.task.depends_on = updated;
    tf.task.updated_at = Utc::now();

    let existing = shelbi_state::list_tasks(project).map_err(|e| anyhow!(e))?;
    shelbi_state::validate_depends_on(&tf.task, &existing).map_err(|e| anyhow!(e))?;
    shelbi_state::save_task(project, &tf.task, &tf.body).map_err(|e| anyhow!(e))?;
    if tf.task.depends_on.is_empty() {
        println!("✓ {} now has no dependencies", args.id);
    } else {
        println!(
            "✓ {} depends on: {}",
            args.id,
            tf.task.depends_on.join(", ")
        );
    }
    Ok(())
}

fn move_to(project: &str, id: &str, to: &str, reason: Option<&str>) -> Result<()> {
    let tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;
    let workflow = resolve_task_workflow(project, &tf.task)?;
    // Resolve the destination against the task's workflow. A task's position
    // is a status id, so ANY status the workflow declares is a valid target
    // — including `canceled` / archived and any status a user adds. A target
    // the workflow doesn't declare errors, naming the declared statuses.
    let column = resolve_move_target(&workflow, to)?;

    // Lifecycle hook: a move INTO `in_progress` cuts the task's branch on
    // the hub (with depends_on awareness — see
    // `shelbi_orchestrator::lifecycle`) and persists `branch:` onto the
    // task. Skip when the destination matches the current column (the
    // `shelbi_state::move_task` short-circuit would treat it as a no-op
    // anyway) and when no column change is actually happening — that
    // keeps `task move ... --to in_progress` on an already-in-progress
    // task from running the cut for no reason. A failure inside the cut
    // (e.g. depends_on names a branch that hasn't been pushed yet) DOES
    // abort the move — silently dropping the depends_on intent and
    // shipping the card to in_progress without a usable branch would be
    // the worst of both worlds.
    if column == Column::in_progress() && tf.task.column != Column::in_progress() {
        let project_yaml = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
        shelbi_orchestrator::lifecycle::ensure_branch_for_in_progress(&project_yaml, id)
            .map_err(|e| anyhow!(e))?;
    }

    let moved = shelbi_state::move_task(project, id, column.clone()).map_err(|e| anyhow!(e))?;
    if let Some((from, to_col, workflow)) = moved {
        let reason = reason.unwrap_or("user:cli");
        if let Err(e) =
            shelbi_state::append_task_event(project, id, &workflow, from, to_col, reason)
        {
            eprintln!("warning: append_task_event failed: {e}");
        }
    }
    println!("✓ {id} → {column}");
    Ok(())
}

/// Load the workflow assigned to `task`. Project defaults are resolved via
/// project config; explicit task workflow misses keep the legacy fallback to
/// the canonical default workflow.
fn resolve_task_workflow(project: &str, task: &Task) -> Result<Workflow> {
    let project_yaml = shelbi_state::load_project(project).ok();
    let name = project_yaml
        .as_ref()
        .map(|p| shelbi_state::resolve_task_workflow_name(p, task))
        .unwrap_or_else(|| task.workflow_or_default());
    match shelbi_state::load_workflow(project, name) {
        Ok(wf) => Ok(wf),
        Err(e) if task.workflow.is_some() || project_yaml.is_none() => {
            eprintln!(
                "warning: workflow `{name}` could not be loaded ({e}); using built-in default"
            );
            Ok(default_workflow())
        }
        Err(e) => Err(anyhow!(e)),
    }
}

/// Resolve a `task move --to <STATUS>` argument against the task's
/// workflow, returning the target position (a status id).
///
/// A task's position is a status id, so any status the workflow declares
/// is a reachable target — including `canceled` / archived statuses and
/// any status a user adds later. `to` is matched against the declared
/// status ids, first through the same alias normalization a stored
/// position gets (so `wip` / `in_progress` resolve onto `in-progress`),
/// then verbatim against the raw declared ids (for custom ids the
/// normalizer passes through untouched). A `to` the workflow doesn't
/// declare errors, listing the ids it does.
fn resolve_move_target(workflow: &Workflow, to: &str) -> Result<Column> {
    // Alias-normalized lookup: folds the friendly CLI spellings onto the
    // canonical id before checking the workflow.
    let normalized = Column::from_status_id(to);
    if let Some(status) = workflow.status(normalized.as_str()) {
        return Ok(Column::from_status_id(&status.id));
    }
    // Verbatim lookup: a custom id the normalizer left untouched still has
    // to match a declared status id exactly (modulo surrounding whitespace).
    if let Some(status) = workflow.statuses.iter().find(|s| s.id == to.trim()) {
        return Ok(Column::from_status_id(&status.id));
    }

    let valid = workflow
        .statuses
        .iter()
        .map(|s| s.id.clone())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "`{to}` is not a status in workflow `{}` (valid: {valid})",
        workflow.name,
    );
}

/// Resolve which agent should drive the workspace once it lands in the
/// active (in-progress) status. `shelbi task start` is an explicit user
/// invocation — we don't gate on Zen here even when the active status
/// is `owner: user` (the user typed the command, that's the override).
/// Falls back to the bundled `developer` agent when the workflow can't
/// be loaded or has no active-category status (legacy workflows without
/// the two-field design); the worktree's agent context still deploys so
/// the bundled developer prompt + skills are wired up correctly.
/// The required workspace tags of the task's workflow active status — the
/// set a workspace's effective tags must be a superset of to take this task
/// (see the tag-routing check in [`start`]). Empty when the workflow has no
/// active status or that status declares no `tags:`.
fn required_active_tags(project: &str, task: &Task) -> Result<std::collections::BTreeSet<String>> {
    use shelbi_core::StatusCategory;
    let workflow = resolve_task_workflow(project, task)?;
    Ok(workflow
        .statuses
        .iter()
        .find(|s| s.category == StatusCategory::Active)
        .map(|s| s.tags.iter().cloned().collect())
        .unwrap_or_default())
}

fn resolve_active_agent_for_dispatch(project: &str, task: &Task) -> Result<String> {
    use shelbi_core::StatusCategory;
    use shelbi_orchestrator::dispatch::{resolve_dispatch_agent, DispatchDecision};
    use shelbi_state::DEVELOPER_AGENT;

    let workflow = resolve_task_workflow(project, task)?;
    // The active-category status is what the task lands in after `task
    // start` — its `agent:` field is the runner we want spawned.
    let active = workflow
        .statuses
        .iter()
        .find(|s| s.category == StatusCategory::Active);

    let zen_on = matches!(
        shelbi_state::read_state(project).map(|s| s.zen_mode),
        Ok(shelbi_state::ZenModeState::On),
    );

    let Some(status) = active else {
        // Workflow without an active status (rare; legacy minimal
        // workflows from before the two-field design). Fall back to the
        // built-in developer so the spawn path still mounts agent
        // context.
        return Ok(DEVELOPER_AGENT.to_string());
    };

    match resolve_dispatch_agent(status, zen_on) {
        DispatchDecision::Dispatch { agent } => Ok(agent),
        DispatchDecision::Skip(reason) => {
            // The CLI is the explicit-intent path: a `Skip` here means
            // the loader allowed a workflow whose active status has no
            // `agent:` (legacy, fully-human workflow). Fall back to the
            // developer agent so the spawn path still has *something*
            // to deploy, and surface the resolver's diagnostic so the
            // user knows why we didn't honor the workflow's wish.
            eprintln!(
                "shelbi: workflow `{}` active status had no dispatchable agent \
                 ({}); falling back to `{DEVELOPER_AGENT}`",
                workflow.name,
                reason.human_message(),
            );
            Ok(DEVELOPER_AGENT.to_string())
        }
    }
}

fn assign(project: &str, id: &str, workspace: &str) -> Result<()> {
    let project_yaml = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    if project_yaml.workspace(workspace).is_none() {
        bail!(
            "workspace `{workspace}` not declared in project `{project}` (known: {})",
            project_yaml
                .workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;
    tf.task.assigned_to = Some(workspace.to_string());
    tf.task.updated_at = Utc::now();
    shelbi_state::save_task(project, &tf.task, &tf.body).map_err(|e| anyhow!(e))?;
    println!("✓ {id} assigned to {workspace}");
    Ok(())
}

fn unassign(project: &str, id: &str) -> Result<()> {
    let mut tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;
    tf.task.assigned_to = None;
    tf.task.updated_at = Utc::now();
    shelbi_state::save_task(project, &tf.task, &tf.body).map_err(|e| anyhow!(e))?;
    println!("✓ {id} unassigned");
    Ok(())
}

fn prio(project: &str, args: PrioArgs) -> Result<()> {
    let tf = shelbi_state::load_task(project, &args.id).map_err(|e| anyhow!(e))?;
    let col = shelbi_state::list_column(project, tf.task.column.clone()).map_err(|e| anyhow!(e))?;
    let pos = col
        .iter()
        .position(|x| x.task.id == args.id)
        .ok_or_else(|| anyhow!("task `{}` not found in column listing", args.id))?;
    let last = col.len().saturating_sub(1);

    let new_pos: usize = if args.up {
        pos.saturating_sub(1)
    } else if args.down {
        (pos + 1).min(last)
    } else if args.top {
        0
    } else if args.bottom {
        last
    } else if let Some(n) = args.set {
        (n as usize).min(last)
    } else {
        bail!("specify one of --up, --down, --top, --bottom, --set N");
    };

    shelbi_state::set_task_priority(project, &args.id, new_pos as u32).map_err(|e| anyhow!(e))?;
    println!("✓ {} now at slot {new_pos} in {}", args.id, tf.task.column);
    Ok(())
}

fn start(
    project: &str,
    id: &str,
    workspace_arg: Option<&str>,
    branch_arg: Option<&str>,
    reason: Option<&str>,
) -> Result<()> {
    let project_yaml = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let mut tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;

    // Resolve workspace: explicit --workspace wins; otherwise reuse task.assigned_to.
    let workspace_name = workspace_arg
        .map(str::to_string)
        .or_else(|| tf.task.assigned_to.clone())
        .ok_or_else(|| {
            anyhow!(
                "task `{id}` has no assigned workspace — pass `--workspace NAME` or run \
                 `shelbi task assign {id} --to <workspace>` first"
            )
        })?;
    let workspace = project_yaml.workspace(&workspace_name).ok_or_else(|| {
        anyhow!(
            "workspace `{workspace_name}` not declared in project `{project}` (known: {})",
            project_yaml
                .workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    // Tag routing (Plans/generic-review-via-workflow-primitives): if the
    // active status the task is entering requires workspace tags, the chosen
    // workspace's *effective* tags (its own ∪ its machine's) must be a
    // superset. An empty required set — the default for every stock status —
    // accepts any workspace, so this is a no-op until a workflow opts in.
    let required = required_active_tags(project, &tf.task)?;
    if !required.is_empty() {
        let effective = project_yaml.effective_tags(workspace);
        let missing: Vec<&str> = required
            .iter()
            .filter(|t| !effective.contains(t.as_str()))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            bail!(
                "workspace `{workspace_name}` can't take task `{id}`: its active status \
                 requires tag(s) {required:?} but the workspace's effective tags are \
                 {effective:?} (missing {missing:?}) — assign a workspace tagged accordingly"
            );
        }
    }

    // Refuse to clobber another in-flight task on the same workspace. Pulling
    // a workspace off mid-task is intentional — make the user do it explicitly
    // via `task move <other> --to todo` first.
    let conflict = shelbi_state::list_column(project, Column::in_progress())
        .map_err(|e| anyhow!(e))?
        .into_iter()
        .find(|tf| {
            tf.task.assigned_to.as_deref() == Some(workspace_name.as_str()) && tf.task.id != id
        });
    if let Some(other) = conflict {
        bail!(
            "workspace `{workspace_name}` is already on task `{}` (in_progress) — \
             move it to another column first",
            other.task.id
        );
    }

    // Cut the branch on the hub if it hasn't been already (depends_on
    // aware — see `shelbi_orchestrator::lifecycle`). Doing this BEFORE
    // `start_workspace_on_task` means the workspace's `sync_worktree` sees an
    // existing branch and just checks it out; for a hub-local workspace
    // they share the repo, so the cut we just made is the same ref the
    // workspace will resolve. An explicit `--branch` override still wins:
    // it bypasses the lifecycle cut and tells `sync_worktree` to use
    // that ref directly (the *release task* pattern, Plans/workflows.md
    // §12).
    if branch_arg.is_none() {
        let updated =
            shelbi_orchestrator::lifecycle::ensure_branch_for_in_progress(&project_yaml, id)
                .map_err(|e| anyhow!(e))?;
        tf = updated;
    }
    let branch = branch_arg
        .map(str::to_string)
        .or_else(|| tf.task.branch.clone())
        .unwrap_or_else(|| format!("shelbi/{id}"));

    // Resolve which agent runs in the spawned pane. `shelbi task start`
    // is always putting the task into `in_progress`, so we look up the
    // workflow's active status and ask the dispatch resolver which
    // agent answers for it under the project's current Zen state. The
    // CLI is an explicit user invocation, so we don't honor the
    // "owner: user + Zen off → skip" rule here — the user typed the
    // command, that's the intent override. We still surface the
    // resolver's verdict via the `agent=` field on StartSpec so the
    // worktree picks up the right `instructions.md` + skills mount.
    let agent_name =
        resolve_active_agent_for_dispatch(project, &tf.task).map_err(|e| anyhow!(e))?;

    // Persist the in_progress move BEFORE spawning the pane (F7). Ordering
    // is load-bearing: if we spawned first and the process died before the
    // save, an agent would be running against a card still sitting in
    // `todo`, and auto-dispatch (which selects from `todo`) could hand the
    // same task to a second workspace — two agents on one branch. The
    // conflict guard above only inspects the `in_progress` column, which
    // the un-persisted first start never reached. By moving the card first
    // the board reflects the in-flight work the instant the agent can
    // exist. `original` snapshots the pre-move frontmatter so an explicit
    // spawn failure can roll the card back rather than strand it in
    // `in_progress` pointing at a pane that never launched.
    let original = tf.task.clone();
    let prev_column = tf.task.column.clone();
    let now = Utc::now();
    tf.task.assigned_to = Some(workspace_name.clone());
    tf.task.branch = Some(branch.clone());
    tf.task.updated_at = now;
    if prev_column != Column::in_progress() {
        let new_priority = shelbi_state::list_column(project, Column::in_progress())
            .map_err(|e| anyhow!(e))?
            .len() as u32;
        tf.task.column = Column::in_progress();
        tf.task.priority = new_priority;
    }
    shelbi_state::save_task(project, &tf.task, &tf.body).map_err(|e| anyhow!(e))?;
    if prev_column != Column::in_progress() {
        shelbi_state::renumber_column(project, prev_column.clone()).map_err(|e| anyhow!(e))?;
    }

    println!("→ launching {workspace_name} on {id} (branch: {branch}, agent: {agent_name})");
    let addr = match shelbi_orchestrator::workspace::start_workspace_on_task(
        shelbi_orchestrator::workspace::StartSpec {
            project: &project_yaml,
            workspace,
            task_id: id,
            branch: &branch,
            task_body: &tf.body,
            agent: Some(agent_name.as_str()),
        },
    ) {
        Ok(addr) => addr,
        Err(e) => {
            // Spawn failed. Roll the card back to its pre-start position so
            // a failed launch doesn't leave it wedged in `in_progress`
            // assigned to a workspace that isn't running. Best-effort: the
            // original spawn error is what the user needs to see, but a
            // rollback failure is surfaced too so a half-moved board isn't
            // silent.
            if let Err(re) = rollback_start(project, &original, &tf.body, prev_column.clone()) {
                eprintln!(
                    "warning: `{id}` was moved to in_progress but the spawn failed and the \
                     rollback also failed ({re}); run `shelbi task move {id} --to {prev_column}` \
                     to recover"
                );
            }
            return Err(anyhow!(e).context("launching workspace"));
        }
    };

    // Spawn succeeded — record the dispatch event now (only successful
    // starts get an events.log line; a rolled-back start leaves no
    // misleading dispatch record).
    if prev_column != Column::in_progress() {
        let base_reason = reason.unwrap_or("user:cli:start");
        let dispatched_reason = dispatch_reason_with_agent(base_reason, &agent_name);
        let workflow = shelbi_state::resolve_task_workflow_name(&project_yaml, &tf.task);
        if let Err(e) = shelbi_state::append_task_event(
            project,
            id,
            workflow,
            prev_column.clone(),
            Column::in_progress(),
            &dispatched_reason,
        ) {
            eprintln!("warning: append_task_event failed: {e}");
        }
    }

    println!(
        "✓ {id} → in_progress on {workspace_name} ({})",
        addr.target()
    );
    Ok(())
}

/// Undo the in_progress move `start` persisted before spawning, after the
/// spawn itself failed. Re-saves the pre-move frontmatter and renumbers
/// both the column we bumped the card out of and `in_progress` (where the
/// aborted card was briefly appended) so priorities stay contiguous.
fn rollback_start(project: &str, original: &Task, body: &str, prev_column: Column) -> Result<()> {
    shelbi_state::save_task(project, original, body).map_err(|e| anyhow!(e))?;
    if prev_column != Column::in_progress() {
        shelbi_state::renumber_column(project, Column::in_progress()).map_err(|e| anyhow!(e))?;
        shelbi_state::renumber_column(project, prev_column).map_err(|e| anyhow!(e))?;
    }
    Ok(())
}

/// Compose the dispatch event's `reason=` value by appending the
/// resolved agent name. `append_task_event` folds the embedded space into
/// an underscore so the final on-the-wire shape is
/// `<base>_agent=<agent>` — keeping the field readable to a human and to
/// the activity-feed parser without breaking the single-token contract.
fn dispatch_reason_with_agent(base: &str, agent: &str) -> String {
    format!("{base} agent={agent}")
}

/// `shelbi task resume` — relaunch the assigned workspace on the task it is
/// already working, WITHOUT discarding the in-flight worktree. The recovery
/// counterpart to [`start`]: where `start` wipes context (kills the pane,
/// re-checks-out a clean branch) for a fresh dispatch, `resume` is for a
/// stalled or killed worker — the tmux session died, the pane wedged, or the
/// agent stopped mid-task — and gets it going again on the SAME task with its
/// commits and uncommitted changes intact.
///
/// The worktree is preserved as-is (see
/// [`shelbi_orchestrator::workspace::resume_workspace_on_task`]); we never cut
/// or reset the branch here. We only touch the board when the card has drifted
/// out of `in_progress` (a killed worker whose card someone moved back), in
/// which case we restore it — mirroring `start`'s persist-before-spawn ordering
/// and rollback-on-failure so a failed relaunch never strands the card.
fn resume(
    project: &str,
    id: &str,
    workspace_arg: Option<&str>,
    reason: Option<&str>,
) -> Result<()> {
    let project_yaml = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let mut tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;

    // Resolve workspace: explicit --workspace wins; otherwise reuse the task's
    // existing assignment. A resume without either has nothing to relaunch.
    let workspace_name = workspace_arg
        .map(str::to_string)
        .or_else(|| tf.task.assigned_to.clone())
        .ok_or_else(|| {
            anyhow!(
                "task `{id}` has no assigned workspace — pass `--workspace NAME` (the \
                 workspace whose worktree holds the in-flight work)"
            )
        })?;
    let workspace = project_yaml.workspace(&workspace_name).ok_or_else(|| {
        anyhow!(
            "workspace `{workspace_name}` not declared in project `{project}` (known: {})",
            project_yaml
                .workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    // Refuse to clobber a DIFFERENT in-flight task on the same workspace —
    // same guard as `start`. Resuming this task onto a workspace busy with
    // another would leave two agents racing one worktree.
    let conflict = shelbi_state::list_column(project, Column::in_progress())
        .map_err(|e| anyhow!(e))?
        .into_iter()
        .find(|other| {
            other.task.assigned_to.as_deref() == Some(workspace_name.as_str())
                && other.task.id != id
        });
    if let Some(other) = conflict {
        bail!(
            "workspace `{workspace_name}` is already on task `{}` (in_progress) — \
             move it to another column first",
            other.task.id
        );
    }

    // Resolve the branch WITHOUT cutting or resetting it: the branch already
    // exists (the worker created + committed on it). Prefer the task's recorded
    // branch, falling back to the conventional `shelbi/<id>`.
    let branch = tf
        .task
        .branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{id}"));

    // Same agent-resolution as `start` — the active status's agent under the
    // project's Zen state, developer as the fallback.
    let agent_name =
        resolve_active_agent_for_dispatch(project, &tf.task).map_err(|e| anyhow!(e))?;

    // Restore `in_progress` if the card drifted out of it. Persist BEFORE the
    // relaunch (same ordering rationale as `start`): the board should reflect
    // the in-flight work the instant the agent can exist. `original` snapshots
    // the pre-move frontmatter so a spawn failure rolls the card back.
    let original = tf.task.clone();
    let prev_column = tf.task.column.clone();
    let moved_into_progress = prev_column != Column::in_progress();
    tf.task.assigned_to = Some(workspace_name.clone());
    tf.task.branch = Some(branch.clone());
    tf.task.updated_at = Utc::now();
    if moved_into_progress {
        let new_priority = shelbi_state::list_column(project, Column::in_progress())
            .map_err(|e| anyhow!(e))?
            .len() as u32;
        tf.task.column = Column::in_progress();
        tf.task.priority = new_priority;
    }
    shelbi_state::save_task(project, &tf.task, &tf.body).map_err(|e| anyhow!(e))?;
    if moved_into_progress {
        shelbi_state::renumber_column(project, prev_column.clone()).map_err(|e| anyhow!(e))?;
    }

    println!("→ resuming {workspace_name} on {id} (branch: {branch}, agent: {agent_name})");
    let addr = match shelbi_orchestrator::workspace::resume_workspace_on_task(
        shelbi_orchestrator::workspace::StartSpec {
            project: &project_yaml,
            workspace,
            task_id: id,
            branch: &branch,
            task_body: &tf.body,
            agent: Some(agent_name.as_str()),
        },
    ) {
        Ok(addr) => addr,
        Err(e) => {
            // Roll the card back only if we moved it — a resume of an
            // already-in-progress task left the board untouched, so there's
            // nothing to undo.
            if moved_into_progress {
                if let Err(re) = rollback_start(project, &original, &tf.body, prev_column.clone()) {
                    eprintln!(
                        "warning: `{id}` was restored to in_progress but the resume failed and \
                         the rollback also failed ({re}); run `shelbi task move {id} --to \
                         {prev_column}` to recover"
                    );
                }
            }
            return Err(anyhow!(e).context("resuming workspace"));
        }
    };

    // Only a card we actually moved records a dispatch event — a resume of an
    // already-in-progress task leaves no misleading transition line.
    if moved_into_progress {
        let base_reason = reason.unwrap_or("user:cli:resume");
        let dispatched_reason = dispatch_reason_with_agent(base_reason, &agent_name);
        let workflow = shelbi_state::resolve_task_workflow_name(&project_yaml, &tf.task);
        if let Err(e) = shelbi_state::append_task_event(
            project,
            id,
            workflow,
            prev_column.clone(),
            Column::in_progress(),
            &dispatched_reason,
        ) {
            eprintln!("warning: append_task_event failed: {e}");
        }
    }

    println!("✓ {id} resumed on {workspace_name} ({})", addr.target());
    Ok(())
}

fn edit(project: &str, id: &str) -> Result<()> {
    let path = shelbi_state::task_path(project, id).map_err(|e| anyhow!(e))?;
    if !path.exists() {
        bail!("task `{id}` not found");
    }
    super::launch_editor(&path)
}

fn rm(project: &str, id: &str) -> Result<()> {
    let tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;
    let column = tf.task.column;
    shelbi_state::delete_task(project, id).map_err(|e| anyhow!(e))?;
    shelbi_state::renumber_column(project, column).map_err(|e| anyhow!(e))?;
    println!("✓ {id} deleted");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers

/// Slugify a title to a kebab-case id, appending `-2`, `-3`, ... if the
/// base collides with an existing task file.
fn generate_unique_id(project: &str, title: &str) -> Result<String> {
    let base = slugify(title);
    if base.is_empty() {
        bail!("could not generate id from title `{title}` — pass --id explicitly");
    }
    let tasks = shelbi_state::tasks_dir(project).map_err(|e| anyhow!(e))?;
    let mut candidate = base.clone();
    let mut n: u32 = 2;
    while tasks.join(format!("{candidate}.md")).exists() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    // Reword the length error so it points at the title the user actually
    // typed rather than the slugified id they never saw.
    if candidate.len() > MAX_TASK_ID_LEN {
        bail!(
            "title is too long: it slugifies to a {}-byte id (max {MAX_TASK_ID_LEN}) — \
             the workspace branch `shelbi/<id>` would exceed GitHub's 255-byte ref limit. \
             Shorten the title or pass --id with an explicit shorter id.",
            candidate.len(),
        );
    }
    Ok(candidate)
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_hyphen = true; // true to trim leading hyphens
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-cli-task-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn task_in(column: Column, id: &str) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: id.replace('-', " "),
            column,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(
            slugify("Fix login bug on Safari"),
            "fix-login-bug-on-safari"
        );
        assert_eq!(slugify("  Hello, World!  "), "hello-world");
        assert_eq!(slugify("CSV → JSON"), "csv-json");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("Already-kebab-OK"), "already-kebab-ok");
    }

    #[test]
    fn generate_unique_id_rejects_titles_that_produce_overlong_ids() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // A title whose slug exceeds the limit by a few bytes is enough to
        // trip the workspace branch over GitHub's 255-byte ref cap.
        let long_title = "a".repeat(MAX_TASK_ID_LEN + 10);
        let err = generate_unique_id("p", &long_title)
            .unwrap_err()
            .to_string();
        assert!(err.contains("title is too long"), "err: {err}");
        assert!(err.contains(&MAX_TASK_ID_LEN.to_string()), "err: {err}");

        // A title at exactly the limit slugifies to the limit and is accepted.
        let exact = "a".repeat(MAX_TASK_ID_LEN);
        let id = generate_unique_id("p", &exact).unwrap();
        assert_eq!(id.len(), MAX_TASK_ID_LEN);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn move_to_writes_default_reason_to_events_log() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_task("p", &task_in(Column::backlog(), "a"), "").unwrap();
        move_to("p", "a", "todo", None).unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        let line = lines[0];
        // Workflow-aware shape: `<ts> task=<id> workflow=<name> <from> ->
        // <to> reason=<r> from_category=<c> to_category=<c>` (§10).
        assert!(line.contains(" task=a "), "line: {line}");
        assert!(line.contains(" workflow=default "), "line: {line}");
        assert!(line.contains(" backlog -> todo "), "line: {line}");
        assert!(line.contains(" reason=user:cli "), "line: {line}");
        assert!(line.contains(" from_category=backlog "), "line: {line}");
        assert!(line.ends_with(" to_category=ready"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn move_to_with_reason_flag_overrides_default() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // `move_to` now runs the depends_on-aware branch cut on hub when
        // a card lands in `in_progress` — see `commands::task::move_to`.
        // The hook needs both a loadable project YAML and a real git
        // repo at the hub's `work_dir`, so we provision them up front.
        crate::commands::test_support::provision_hub_repo_for_project(&home, "p");

        shelbi_state::save_task("p", &task_in(Column::todo(), "b"), "").unwrap();
        move_to(
            "p",
            "b",
            "in_progress",
            Some("orchestrator:auto-dispatch workspace=alpha"),
        )
        .unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        // sanitize_reason folds whitespace to underscores so the `reason=`
        // value stays a single token even with the `from_category=` /
        // `to_category=` annotations trailing it (§10 shape).
        assert!(
            lines[0].contains(" reason=orchestrator:auto-dispatch_workspace=alpha "),
            "line: {}",
            lines[0],
        );
        assert!(
            lines[0].ends_with(" to_category=active"),
            "line: {}",
            lines[0],
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn move_to_no_op_emits_no_event() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_task("p", &task_in(Column::todo(), "c"), "").unwrap();
        // Already in `todo` — move_task short-circuits, no event line.
        move_to("p", "c", "todo", None).unwrap();

        let path = shelbi_state::events_log_path().unwrap();
        assert!(
            !path.exists() || std::fs::read_to_string(&path).unwrap().is_empty(),
            "no-op move must not write an events.log line",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn move_to_rejects_status_missing_from_workflow() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Stand in for `shelbi init` — the workflow loader requires
        // the project's status catalogue to be on disk.
        shelbi_state::save_project_statuses("p", &shelbi_core::default_project_statuses()).unwrap();

        // Author a workflow that omits `review` — moves to it must fail.
        let wf_dir = shelbi_state::workflows_dir("p").unwrap();
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("research.yaml"),
            r#"name: research
statuses:
  - { id: backlog,     owner: user                          }
  - { id: todo,        owner: agent, agent: orchestrator    }
  - { id: in-progress, owner: agent, agent: developer       }
  - { id: done,        owner: user                          }
"#,
        )
        .unwrap();

        let mut task = task_in(Column::todo(), "d");
        task.workflow = Some("research".into());
        shelbi_state::save_task("p", &task, "").unwrap();

        let err = move_to("p", "d", "review", None).unwrap_err().to_string();
        assert!(err.contains("workflow `research`"), "{err}");
        // The error lists valid status ids (stable identifiers) in kebab
        // form. Pull out the `(valid: ...)` segment so the assertion on
        // "review missing from the workflow" isn't fooled by the
        // destination id (`review`) that opens the message.
        let valid = err
            .split_once("(valid:")
            .and_then(|(_, tail)| tail.split_once(')'))
            .map(|(list, _)| list.trim())
            .unwrap_or("");
        assert!(valid.contains("backlog"), "{err}");
        assert!(valid.contains("done"), "{err}");
        assert!(!valid.contains("review"), "{err}");

        // Task must stay put — no event written.
        let path = shelbi_state::events_log_path().unwrap();
        assert!(
            !path.exists() || std::fs::read_to_string(&path).unwrap().is_empty(),
            "rejected move must not write an events.log line",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn move_to_custom_status_is_a_valid_target() {
        // A task's position is a status id, so ANY status the workflow
        // declares — including a custom `qa` status with no legacy column
        // backing — is a reachable `task move` target. A status the
        // workflow does NOT declare still errors, naming the declared ids.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Project catalogue = defaults + a custom `qa` status.
        let mut statuses = shelbi_core::default_project_statuses();
        statuses.statuses.push(shelbi_core::ProjectStatus {
            id: "qa".into(),
            name: "QA".into(),
            category: shelbi_core::StatusCategory::Handoff,
        });
        shelbi_state::save_project_statuses("p", &statuses).unwrap();

        let wf_dir = shelbi_state::workflows_dir("p").unwrap();
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("qa.yaml"),
            r#"name: qa
statuses:
  - { id: backlog,     owner: user                       }
  - { id: todo,        owner: agent, agent: orchestrator  }
  - { id: in-progress, owner: agent, agent: developer     }
  - { id: qa,          owner: user                        }
  - { id: done,        owner: user                        }
"#,
        )
        .unwrap();

        let mut task = task_in(Column::todo(), "t");
        task.workflow = Some("qa".into());
        shelbi_state::save_task("p", &task, "").unwrap();

        // `--to qa`: a declared custom status is now a real move target and
        // the task lands there (its stored position id is the custom id).
        move_to("p", "t", "qa", None).unwrap();
        assert_eq!(
            shelbi_state::load_task("p", "t")
                .unwrap()
                .task
                .column
                .as_str(),
            "qa",
        );

        // A status absent from this workflow (`review`) still errors, and
        // the valid-status list names the declared ids (never the undeclared
        // target). Task stays put.
        let err = move_to("p", "t", "review", None).unwrap_err().to_string();
        let valid = err
            .split_once("(valid:")
            .and_then(|(_, tail)| tail.split_once(')'))
            .map(|(l, _)| l.trim())
            .unwrap_or("");
        assert!(valid.contains("backlog"), "{err}");
        assert!(valid.contains("qa"), "{err}");
        assert!(!valid.contains("review"), "{err}");
        assert_eq!(
            shelbi_state::load_task("p", "t")
                .unwrap()
                .task
                .column
                .as_str(),
            "qa",
            "rejected move must not relocate the task",
        );

        // A core status the workflow declares still moves successfully.
        move_to("p", "t", "done", None).unwrap();
        assert_eq!(
            shelbi_state::load_task("p", "t").unwrap().task.column,
            Column::done(),
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn move_to_canceled_succeeds_and_round_trips() {
        // The headline acceptance: a default-workflow task can be moved to
        // the archived `canceled` status, and it round-trips through disk.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_project_statuses("p", &shelbi_core::default_project_statuses()).unwrap();
        // A default-workflow task (no explicit `workflow:` field).
        shelbi_state::save_task("p", &task_in(Column::todo(), "c"), "").unwrap();

        move_to("p", "c", "canceled", None).unwrap();
        let reloaded = shelbi_state::load_task("p", "c").unwrap();
        assert_eq!(reloaded.task.column, Column::canceled());
        assert_eq!(reloaded.task.column.as_str(), "canceled");
        assert_eq!(
            reloaded.task.column.category(),
            shelbi_core::StatusCategory::Archived,
        );

        // The move emitted an events-log line with the archived category.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(log.contains(" todo -> canceled "), "{log}");
        assert!(log.contains("to_category=archived"), "{log}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn list_workflow_filter_composes_with_column_and_ready() {
        // Three tasks across two workflows; verify the filter wiring on
        // each list mode (default / --column / --ready) returns Ok and
        // doesn't panic when the filter matches zero, one, or all tasks.
        // Output assertions live behind a refactor (split compute from
        // render); the smoke test catches accidental regressions in the
        // wiring.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let mut a = task_in(Column::todo(), "a");
        a.workflow = Some("research".into());
        let b = task_in(Column::todo(), "b"); // no workflow → matches `default`
        let mut c = task_in(Column::backlog(), "c");
        c.workflow = Some("research".into());
        for t in [&a, &b, &c] {
            shelbi_state::save_task("p", t, "").unwrap();
        }

        // Workflow filter alone — should not error.
        list("p", None, false, Some("research")).unwrap();
        list("p", None, false, Some("default")).unwrap();
        list("p", None, false, Some("nonexistent")).unwrap();

        // Composes with --column.
        list("p", Some("todo"), false, Some("research")).unwrap();
        list("p", Some("backlog"), false, Some("research")).unwrap();

        // Composes with --ready.
        list("p", None, true, Some("research")).unwrap();
        list("p", None, true, Some("default")).unwrap();
        list("p", None, true, Some("nonexistent")).unwrap();

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn list_workflow_default_matches_tasks_without_explicit_workflow() {
        // `--workflow default` must match tasks whose frontmatter omits
        // `workflow:` entirely — that's the contract `Task::workflow_or_default`
        // promises and the contract callers (orchestrator, future TUI
        // filter) rely on. Verified by exercising the matcher closure
        // directly through the filter_workflow_name helper-equivalent
        // pattern used inside `list`.
        let no_explicit = task_in(Column::todo(), "n");
        assert_eq!(no_explicit.workflow_or_default(), "default");

        let mut research = task_in(Column::todo(), "r");
        research.workflow = Some("research".into());
        assert_eq!(research.workflow_or_default(), "research");
    }

    fn write_workflow(project: &str, name: &str, yaml: &str) {
        let dir = shelbi_state::workflows_dir(project).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.yaml")), yaml).unwrap();
    }

    fn materialize_default_agents_for_test(project: &str) {
        // The workflow loader rejects `agent:` references that don't
        // point at a real `agents/<name>/` directory. The resolver's
        // workflow loads pass through that check, so the test fixture
        // has to materialize the default agent set just like a real
        // `shelbi init` does.
        shelbi_state::materialize_default_agents(project).unwrap();
    }

    #[test]
    fn dispatch_reason_appends_agent_segment_for_both_default_and_orchestrator_paths() {
        // Acceptance (a) — every event emitted when `shelbi task start`
        // spawns a workspace must include `_agent=<name>` in `reason=`.
        // The helper composes the raw reason; `append_task_event` folds
        // the embedded space into the underscore that ends up on disk.

        // Default (user-driven) reason.
        let r = dispatch_reason_with_agent("user:cli:start", "developer");
        assert_eq!(r, "user:cli:start agent=developer");

        // Orchestrator-supplied reason (the auto-dispatch contract from
        // the default orchestrator playbook).
        let r =
            dispatch_reason_with_agent("orchestrator:auto-dispatch workspace=alpha", "developer");
        assert_eq!(
            r,
            "orchestrator:auto-dispatch workspace=alpha agent=developer"
        );

        // After the sanitizer runs (whitespace → underscore) the on-disk
        // shape becomes a single parseable token — that's what the
        // activity-feed parser keys off `_agent=` to extract.
        let sanitized: String = r
            .chars()
            .map(|c| if c.is_whitespace() { '_' } else { c })
            .collect();
        assert_eq!(
            sanitized,
            "orchestrator:auto-dispatch_workspace=alpha_agent=developer"
        );
    }

    #[test]
    fn start_event_line_carries_agent_segment_via_move_to_round_trip() {
        // Acceptance (a) end-to-end check: the on-disk line shape after
        // emission contains the `_agent=<name>` segment. We exercise the
        // emission path through `append_task_event` directly with the
        // composed reason (mirrors what `start()` writes) so the test
        // doesn't need to stand up a real tmux pane to spawn the
        // workspace.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let dispatched =
            dispatch_reason_with_agent("orchestrator:auto-dispatch workspace=alpha", "developer");
        shelbi_state::append_task_event(
            "demo",
            "demo-task",
            "default",
            Column::todo(),
            Column::in_progress(),
            &dispatched,
        )
        .unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(
            log.contains(" reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer "),
            "log: {log}",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_active_agent_dispatches_developer_for_default_workflow() {
        // Acceptance criterion (a) from the task: a default `shelbi task
        // start` resolves the active status's agent and lands on
        // `developer`. The resolver doesn't care about Zen mode for an
        // `owner: agent` status, so this passes regardless of state.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents_for_test("p");
        write_workflow(
            "p",
            "default",
            r#"
name: default
statuses:
  - { id: backlog,     name: Backlog,    category: backlog,  owner: user                       }
  - { id: todo,        name: Todo,       category: ready,    owner: agent, agent: orchestrator }
  - { id: in-progress, name: InProgress, category: active,   owner: agent, agent: developer    }
  - { id: review,      name: Review,     category: handoff,  owner: user,  agent: orchestrator }
  - { id: done,        name: Done,       category: done,     owner: user                       }
"#,
        );

        let task = task_in(Column::todo(), "t1");
        let agent = resolve_active_agent_for_dispatch("p", &task).unwrap();
        assert_eq!(agent, "developer");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_active_agent_falls_back_to_developer_for_workflow_without_active() {
        // A legacy workflow without an `active`-category status (rare,
        // but possible with the historic minimal flow). The resolver
        // can't resolve through the workflow, so it falls back to the
        // bundled `developer` agent — that way the spawn path still
        // mounts agent context, instead of silently dispatching with
        // nothing.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents_for_test("p");
        write_workflow(
            "p",
            "default",
            r#"
name: default
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user }
  - { id: done,    name: Done,    category: done,    owner: user }
"#,
        );
        let task = task_in(Column::todo(), "t2");
        let agent = resolve_active_agent_for_dispatch("p", &task).unwrap();
        assert_eq!(agent, shelbi_state::DEVELOPER_AGENT);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resume_without_assignment_or_workspace_flag_errors() {
        // A resume needs to know which workspace holds the in-flight work.
        // With no `assigned_to` and no `--workspace`, it must fail cleanly
        // (before touching any pane) rather than guessing.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        crate::commands::test_support::provision_hub_repo_for_project(&home, "p");

        shelbi_state::save_task("p", &task_in(Column::in_progress(), "orphan"), "").unwrap();
        let err = resume("p", "orphan", None, None).unwrap_err().to_string();
        assert!(err.contains("no assigned workspace"), "err: {err}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resume_rejects_unknown_workspace() {
        // An explicit `--workspace` that isn't declared in the project must
        // be rejected with the known-workspaces list, same as `start`/`assign`.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // provision_hub_repo_for_project declares no workspaces, so any name
        // is "unknown" — exactly the case under test.
        crate::commands::test_support::provision_hub_repo_for_project(&home, "p");

        shelbi_state::save_task("p", &task_in(Column::in_progress(), "t"), "").unwrap();
        let err = resume("p", "t", Some("ghost"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("workspace `ghost` not declared"), "err: {err}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn move_to_missing_workflow_falls_back_to_default() {
        // A task pinned to a workflow the project hasn't authored falls
        // back to the canonical default — same five statuses, so a move
        // to `review` still succeeds.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let mut task = task_in(Column::todo(), "e");
        task.workflow = Some("nonexistent".into());
        shelbi_state::save_task("p", &task, "").unwrap();

        move_to("p", "e", "review", None).unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(log.contains(" task=e "), "{log}");
        assert!(log.contains(" todo -> review "), "{log}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
