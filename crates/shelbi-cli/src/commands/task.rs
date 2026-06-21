//! `shelbi task <subcommand>` — Kanban board management.
//!
//! Tasks are stored as `<shelbi_home>/projects/<project>/tasks/<id>.md`
//! files (markdown body + YAML frontmatter). The orchestrator creates
//! tasks (typically into `backlog`); the user curates them through the
//! columns; workers pick up `todo` tasks.
//!
//! Priorities within a column are contiguous integers 0..N. Any operation
//! that changes a column's membership renumbers it before returning, so
//! callers can treat `priority` as a stable position index.

use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::{Args as ClapArgs, Subcommand};
use shelbi_core::{validate_task_id, Column, Task};

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum TaskCmd {
    /// Create a new task. Defaults to the backlog column.
    Add(AddArgs),
    /// List tasks (all columns, or one with `--column`).
    List {
        #[arg(long)]
        column: Option<String>,
        /// Show only unblocked todo items, in priority order. Useful for
        /// orchestrator agents and for users planning next work. Mutually
        /// exclusive with `--column`.
        #[arg(long, conflicts_with = "column")]
        ready: bool,
    },
    /// Print a task's frontmatter + body, plus the resolved status of each
    /// `depends_on` entry.
    Show { id: String },
    /// Edit a task's dependency list.
    Depends(DependsArgs),
    /// Move a task to another column.
    Move {
        id: String,
        #[arg(long, value_name = "COLUMN")]
        to: String,
        /// Reason string recorded in `~/.shelbi/events.log`. The
        /// orchestrator parses this to identify auto-dispatch moves vs.
        /// user-driven ones. Defaults to `user:cli`.
        #[arg(long, value_name = "REASON")]
        reason: Option<String>,
    },
    /// Assign a task to a worker. Worker must be declared in project YAML.
    Assign {
        id: String,
        #[arg(long, value_name = "WORKER")]
        to: String,
    },
    /// Clear a task's worker assignment.
    Unassign { id: String },
    /// Launch the assigned worker on this task: ensure the worktree is on
    /// the task's branch, kill any existing worker pane (clears context),
    /// start the runner with the task's prompt. Moves the task into
    /// `in_progress`. Pass `--worker` to assign at the same time.
    Start {
        id: String,
        #[arg(long, value_name = "WORKER")]
        worker: Option<String>,
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
    /// Initial column. Defaults to `backlog`.
    #[arg(long, default_value = "backlog")]
    pub column: String,
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
    /// worker on this machine. Persisted in the task frontmatter; the
    /// orchestrator decides whether to honor it.
    #[arg(long = "prefers-machine", value_name = "NAME")]
    pub prefers_machine: Option<String>,
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
        TaskCmd::List { column, ready } => list(&project, column.as_deref(), ready),
        TaskCmd::Show { id } => show(&project, &id),
        TaskCmd::Depends(args) => depends(&project, args),
        TaskCmd::Move { id, to, reason } => move_to(&project, &id, &to, reason.as_deref()),
        TaskCmd::Assign { id, to } => assign(&project, &id, &to),
        TaskCmd::Unassign { id } => unassign(&project, &id),
        TaskCmd::Start { id, worker, branch, reason } => {
            start(&project, &id, worker.as_deref(), branch.as_deref(), reason.as_deref())
        }
        TaskCmd::Prio(args) => prio(&project, args),
        TaskCmd::Edit { id } => edit(&project, &id),
        TaskCmd::Rm { id } => rm(&project, &id),
    }
}

fn add(project: &str, args: AddArgs) -> Result<()> {
    let column = Column::from_str(&args.column).map_err(|e| anyhow!(e))?;
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

    let priority = shelbi_state::list_column(project, column)
        .map_err(|e| anyhow!(e))?
        .len() as u32;
    let now = Utc::now();
    let task = Task {
        id: id.clone(),
        title: args.title.clone(),
        column,
        priority,
        assigned_to: None,
        branch: None,
        depends_on: dedup_preserving_order(args.depends_on.clone()),
        prefers_machine: args.prefers_machine.clone(),
        created_at: now,
        updated_at: now,
    };
    if !task.depends_on.is_empty() {
        let existing = shelbi_state::list_tasks(project).map_err(|e| anyhow!(e))?;
        shelbi_state::validate_depends_on(&task, &existing).map_err(|e| anyhow!(e))?;
    }
    let body = args
        .description
        .map(|d| format!("# Task\n\n{d}\n"))
        .unwrap_or_else(|| format!("# Task\n\n{}\n", args.title));
    shelbi_state::save_task(project, &task, &body).map_err(|e| anyhow!(e))?;
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

fn list(project: &str, column_filter: Option<&str>, ready: bool) -> Result<()> {
    if ready {
        let ready_tasks = shelbi_state::list_ready(project).map_err(|e| anyhow!(e))?;
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

    let filter = column_filter
        .map(Column::from_str)
        .transpose()
        .map_err(|e| anyhow!(e))?;

    let all = shelbi_state::list_tasks(project).map_err(|e| anyhow!(e))?;
    if all.is_empty() {
        println!("(no tasks yet)");
        return Ok(());
    }
    let columns: std::collections::HashMap<String, Column> = all
        .iter()
        .map(|tf| (tf.task.id.clone(), tf.task.column))
        .collect();
    for col in Column::ALL {
        if let Some(want) = filter {
            if want != col {
                continue;
            }
        }
        let in_col: Vec<_> = all.iter().filter(|tf| tf.task.column == col).collect();
        println!("{col} ({})", in_col.len());
        for tf in in_col {
            let owner = tf
                .task
                .assigned_to
                .as_deref()
                .map(|w| format!("  [{w}]"))
                .unwrap_or_default();
            let badge = if tf.task.is_blocked(&columns) { " 🔒" } else { "" };
            println!("  {:<28} {}{owner}{badge}", tf.task.id, tf.task.title);
        }
    }
    Ok(())
}

fn show(project: &str, id: &str) -> Result<()> {
    let path = shelbi_state::task_path(project, id).map_err(|e| anyhow!(e))?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
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
    let column = Column::from_str(to).map_err(|e| anyhow!(e))?;
    let moved = shelbi_state::move_task(project, id, column).map_err(|e| anyhow!(e))?;
    if let Some((from, to_col)) = moved {
        let reason = reason.unwrap_or("user:cli");
        if let Err(e) = shelbi_state::append_task_event(id, from, to_col, reason) {
            eprintln!("warning: append_task_event failed: {e}");
        }
    }
    println!("✓ {id} → {column}");
    Ok(())
}

fn assign(project: &str, id: &str, worker: &str) -> Result<()> {
    let project_yaml = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    if project_yaml.worker(worker).is_none() {
        bail!(
            "worker `{worker}` not declared in project `{project}` (known: {})",
            project_yaml
                .workers
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;
    tf.task.assigned_to = Some(worker.to_string());
    tf.task.updated_at = Utc::now();
    shelbi_state::save_task(project, &tf.task, &tf.body).map_err(|e| anyhow!(e))?;
    println!("✓ {id} assigned to {worker}");
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
    let col = shelbi_state::list_column(project, tf.task.column).map_err(|e| anyhow!(e))?;
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

    shelbi_state::set_task_priority(project, &args.id, new_pos as u32)
        .map_err(|e| anyhow!(e))?;
    println!("✓ {} now at slot {new_pos} in {}", args.id, tf.task.column);
    Ok(())
}

fn start(
    project: &str,
    id: &str,
    worker_arg: Option<&str>,
    branch_arg: Option<&str>,
    reason: Option<&str>,
) -> Result<()> {
    let project_yaml = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let mut tf = shelbi_state::load_task(project, id).map_err(|e| anyhow!(e))?;

    // Resolve worker: explicit --worker wins; otherwise reuse task.assigned_to.
    let worker_name = worker_arg
        .map(str::to_string)
        .or_else(|| tf.task.assigned_to.clone())
        .ok_or_else(|| {
            anyhow!(
                "task `{id}` has no assigned worker — pass `--worker NAME` or run \
                 `shelbi task assign {id} --to <worker>` first"
            )
        })?;
    let worker = project_yaml.worker(&worker_name).ok_or_else(|| {
        anyhow!(
            "worker `{worker_name}` not declared in project `{project}` (known: {})",
            project_yaml
                .workers
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    // Refuse to clobber another in-flight task on the same worker. Pulling
    // a worker off mid-task is intentional — make the user do it explicitly
    // via `task move <other> --to todo` first.
    let conflict = shelbi_state::list_column(project, Column::InProgress)
        .map_err(|e| anyhow!(e))?
        .into_iter()
        .find(|tf| {
            tf.task.assigned_to.as_deref() == Some(worker_name.as_str()) && tf.task.id != id
        });
    if let Some(other) = conflict {
        bail!(
            "worker `{worker_name}` is already on task `{}` (in_progress) — \
             move it to another column first",
            other.task.id
        );
    }

    let branch = branch_arg
        .map(str::to_string)
        .or_else(|| tf.task.branch.clone())
        .unwrap_or_else(|| format!("shelbi/{id}"));

    println!("→ launching {worker_name} on {id} (branch: {branch})");
    let addr = shelbi_orchestrator::worker::start_worker_on_task(
        shelbi_orchestrator::worker::StartSpec {
            project: &project_yaml,
            worker,
            task_id: id,
            branch: &branch,
            task_body: &tf.body,
        },
    )
    .map_err(|e| anyhow!(e))?;

    // Persist task state. Move to in_progress before saving so the
    // assigned_to/branch land alongside the column change in a single write.
    let now = Utc::now();
    tf.task.assigned_to = Some(worker_name.clone());
    tf.task.branch = Some(branch.clone());
    tf.task.updated_at = now;
    let prev_column = tf.task.column;
    if prev_column != Column::InProgress {
        let new_priority = shelbi_state::list_column(project, Column::InProgress)
            .map_err(|e| anyhow!(e))?
            .len() as u32;
        tf.task.column = Column::InProgress;
        tf.task.priority = new_priority;
    }
    shelbi_state::save_task(project, &tf.task, &tf.body).map_err(|e| anyhow!(e))?;
    if prev_column != Column::InProgress {
        shelbi_state::renumber_column(project, prev_column).map_err(|e| anyhow!(e))?;
        let reason = reason.unwrap_or("user:cli:start");
        if let Err(e) =
            shelbi_state::append_task_event(id, prev_column, Column::InProgress, reason)
        {
            eprintln!("warning: append_task_event failed: {e}");
        }
    }

    println!("✓ {id} → in_progress on {worker_name} ({})", addr.target());
    Ok(())
}

fn edit(project: &str, id: &str) -> Result<()> {
    let path = shelbi_state::task_path(project, id).map_err(|e| anyhow!(e))?;
    if !path.exists() {
        bail!("task `{id}` not found");
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor).arg(&path).status()?;
    if !status.success() {
        bail!("{editor} exited with {status}");
    }
    Ok(())
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
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Fix login bug on Safari"), "fix-login-bug-on-safari");
        assert_eq!(slugify("  Hello, World!  "), "hello-world");
        assert_eq!(slugify("CSV → JSON"), "csv-json");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("Already-kebab-OK"), "already-kebab-ok");
    }

    #[test]
    fn move_to_writes_default_reason_to_events_log() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_task("p", &task_in(Column::Backlog, "a"), "").unwrap();
        move_to("p", "a", "todo", None).unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        let line = lines[0];
        assert!(line.contains(" task=a "), "line: {line}");
        assert!(line.contains(" backlog -> todo "), "line: {line}");
        assert!(line.ends_with("reason=user:cli"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn move_to_with_reason_flag_overrides_default() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_task("p", &task_in(Column::Todo, "b"), "").unwrap();
        move_to("p", "b", "in_progress", Some("orchestrator:auto-dispatch worker=alpha"))
            .unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        // sanitize_reason folds whitespace to underscores so the line stays
        // parseable; the orchestrator parses by `reason=<prefix>:...`.
        assert!(
            lines[0].ends_with("reason=orchestrator:auto-dispatch_worker=alpha"),
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

        shelbi_state::save_task("p", &task_in(Column::Todo, "c"), "").unwrap();
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
}
