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
    },
    /// Print a task's frontmatter + body.
    Show { id: String },
    /// Move a task to another column.
    Move {
        id: String,
        #[arg(long, value_name = "COLUMN")]
        to: String,
    },
    /// Assign a task to a worker. Worker must be declared in project YAML.
    Assign {
        id: String,
        #[arg(long, value_name = "WORKER")]
        to: String,
    },
    /// Clear a task's worker assignment.
    Unassign { id: String },
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
        TaskCmd::List { column } => list(&project, column.as_deref()),
        TaskCmd::Show { id } => show(&project, &id),
        TaskCmd::Move { id, to } => move_to(&project, &id, &to),
        TaskCmd::Assign { id, to } => assign(&project, &id, &to),
        TaskCmd::Unassign { id } => unassign(&project, &id),
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
        created_at: now,
        updated_at: now,
    };
    let body = args
        .description
        .map(|d| format!("# Task\n\n{d}\n"))
        .unwrap_or_else(|| format!("# Task\n\n{}\n", args.title));
    shelbi_state::save_task(project, &task, &body).map_err(|e| anyhow!(e))?;
    println!("✓ {} created in {column} (priority {priority})", task.id);
    Ok(())
}

fn list(project: &str, column_filter: Option<&str>) -> Result<()> {
    let filter = column_filter
        .map(Column::from_str)
        .transpose()
        .map_err(|e| anyhow!(e))?;

    let all = shelbi_state::list_tasks(project).map_err(|e| anyhow!(e))?;
    if all.is_empty() {
        println!("(no tasks yet)");
        return Ok(());
    }
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
            println!("  {:<28} {}{owner}", tf.task.id, tf.task.title);
        }
    }
    Ok(())
}

fn show(project: &str, id: &str) -> Result<()> {
    let path = shelbi_state::task_path(project, id).map_err(|e| anyhow!(e))?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    print!("{text}");
    Ok(())
}

fn move_to(project: &str, id: &str, to: &str) -> Result<()> {
    let column = Column::from_str(to).map_err(|e| anyhow!(e))?;
    shelbi_state::move_task(project, id, column).map_err(|e| anyhow!(e))?;
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

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Fix login bug on Safari"), "fix-login-bug-on-safari");
        assert_eq!(slugify("  Hello, World!  "), "hello-world");
        assert_eq!(slugify("CSV → JSON"), "csv-json");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("Already-kebab-OK"), "already-kebab-ok");
    }
}
