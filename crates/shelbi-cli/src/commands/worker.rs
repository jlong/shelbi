//! `shelbi worker <subcommand>` — manage the project's declared worker
//! pool. Workers are durable slots (one worktree each); tasks come and go.
//! See [`shelbi_orchestrator::worker`] for the lifecycle primitives.

use anyhow::{anyhow, Result};
use chrono::Utc;
use clap::Subcommand;
use shelbi_core::Column;
use shelbi_orchestrator::worker as orch_worker;

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum WorkerCmd {
    /// List declared workers, their machine/runner, current task (if any),
    /// and whether their tmux pane is live.
    List,
    /// Kill a worker's tmux pane. Releases the worker's in-flight task back
    /// to `todo` (unassigned) so the board doesn't show an orphaned
    /// in_progress card; pass `--keep-task` to leave the task in place.
    Stop {
        name: String,
        /// Leave the in-flight task in `in_progress` with `assigned_to`
        /// pointing at this worker. Use when you're about to restart the
        /// worker on the same task and don't want the card to move.
        #[arg(long)]
        keep_task: bool,
    },
}

pub fn run(project_opt: Option<String>, cmd: WorkerCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        WorkerCmd::List => list(&project),
        WorkerCmd::Stop { name, keep_task } => stop(&project, &name, keep_task),
    }
}

fn list(project: &str) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    if p.workers.is_empty() {
        println!("(no workers declared in {project} — add a `workers:` block to the project YAML)");
        return Ok(());
    }

    // Build a {worker -> task_id} index from in-progress tasks. There
    // should be at most one in-progress task per worker, but if shelbi's
    // state diverged we surface all of them so the user can see the mess.
    let in_progress = shelbi_state::list_column(project, Column::InProgress)
        .map_err(|e| anyhow!(e))?;

    for worker in &p.workers {
        let machine = p
            .machine(&worker.machine)
            .ok_or_else(|| anyhow!("worker `{}` references unknown machine `{}`", worker.name, worker.machine))?;
        let host = machine.host();
        let addr = orch_worker::worker_tmux_addr(&p, worker).map_err(|e| anyhow!(e))?;
        let alive = orch_worker::worker_pane_alive(&host, &addr).unwrap_or(false);

        let mine: Vec<&str> = in_progress
            .iter()
            .filter(|tf| tf.task.assigned_to.as_deref() == Some(worker.name.as_str()))
            .map(|tf| tf.task.id.as_str())
            .collect();

        let pane_state = if alive { "●" } else { "·" };
        let task_summary = if mine.is_empty() {
            "(idle)".to_string()
        } else {
            mine.join(", ")
        };
        println!(
            "{pane_state} {:<12} {:<8} {:<8} {}",
            worker.name, worker.machine, worker.runner, task_summary
        );
    }
    Ok(())
}

fn stop(project: &str, name: &str, keep_task: bool) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let worker = p.worker(name).ok_or_else(|| {
        anyhow!(
            "worker `{name}` not declared in project `{project}` (known: {})",
            p.workers
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let machine = p
        .machine(&worker.machine)
        .ok_or_else(|| anyhow!("worker references unknown machine `{}`", worker.machine))?;
    let host = machine.host();
    let addr = orch_worker::worker_tmux_addr(&p, worker).map_err(|e| anyhow!(e))?;
    orch_worker::kill_worker_pane(&host, &addr).map_err(|e| anyhow!(e))?;
    println!("✓ {name} pane stopped");

    if keep_task {
        return Ok(());
    }

    for id in release_worker_tasks(project, name)? {
        println!("✓ {id} released → todo (was assigned to {name})");
    }
    Ok(())
}

/// Unassign and move-to-todo every in-flight task currently owned by
/// `worker_name`. Returns the released task ids in the order they were
/// processed. There should be at most one, but if state diverged we
/// release them all so the board doesn't keep dangling cards pointing at
/// a dead pane.
fn release_worker_tasks(project: &str, worker_name: &str) -> Result<Vec<String>> {
    let in_progress = shelbi_state::list_column(project, Column::InProgress)
        .map_err(|e| anyhow!(e))?;
    let mut released = Vec::new();
    for tf in in_progress {
        if tf.task.assigned_to.as_deref() != Some(worker_name) {
            continue;
        }
        let id = tf.task.id.clone();
        let mut task = tf.task;
        task.assigned_to = None;
        task.updated_at = Utc::now();
        // Persist the unassign first, then move — `move_task` re-reads the
        // file, so writing in this order keeps both changes in the final
        // on-disk state.
        shelbi_state::save_task(project, &task, &tf.body).map_err(|e| anyhow!(e))?;
        shelbi_state::move_task(project, &id, Column::Todo).map_err(|e| anyhow!(e))?;
        released.push(id);
    }
    Ok(released)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::Task;
    use std::path::PathBuf;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-cli-worker-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_task(id: &str, column: Column, priority: u32, assigned_to: Option<&str>) -> Task {
        let now = Utc::now();
        Task {
            id: id.to_string(),
            title: id.replace('-', " "),
            column,
            priority,
            assigned_to: assigned_to.map(str::to_string),
            branch: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn release_moves_in_flight_back_to_todo_and_unassigns() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Bob's task should stay put; alice's should come back to todo.
        shelbi_state::save_task(
            "p",
            &make_task("fix-login", Column::InProgress, 0, Some("alice")),
            "# body\n",
        )
        .unwrap();
        shelbi_state::save_task(
            "p",
            &make_task("other", Column::InProgress, 1, Some("bob")),
            "",
        )
        .unwrap();
        shelbi_state::save_task("p", &make_task("a", Column::Todo, 0, None), "").unwrap();

        let released = release_worker_tasks("p", "alice").unwrap();
        assert_eq!(released, vec!["fix-login"]);

        let fix = shelbi_state::load_task("p", "fix-login").unwrap();
        assert_eq!(fix.task.column, Column::Todo);
        assert_eq!(fix.task.assigned_to, None);
        // Lands at the bottom of `todo` (after the existing `a`).
        assert_eq!(fix.task.priority, 1);
        assert!(fix.body.contains("# body"));

        // Bob's task is untouched.
        let bob_task = shelbi_state::load_task("p", "other").unwrap();
        assert_eq!(bob_task.task.column, Column::InProgress);
        assert_eq!(bob_task.task.assigned_to.as_deref(), Some("bob"));
        // After alice's task moves out, in_progress is renumbered 0..N.
        assert_eq!(bob_task.task.priority, 0);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn release_is_noop_when_worker_has_no_in_flight_task() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_task("p", &make_task("a", Column::Todo, 0, None), "").unwrap();

        let released = release_worker_tasks("p", "alice").unwrap();
        assert!(released.is_empty());

        std::env::remove_var("SHELBI_HOME");
    }
}
