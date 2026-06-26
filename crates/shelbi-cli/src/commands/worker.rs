//! `shelbi worker <subcommand>` — manage the project's declared worker
//! pool. Workers are durable slots (one worktree each); tasks come and go.
//! See [`shelbi_orchestrator::worker`] for the lifecycle primitives.

use std::fs;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use shelbi_core::Column;
use shelbi_orchestrator::worker as orch_worker;
use shelbi_state::WorkerStatus;

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
    /// Show observed worker state from the hub-side poller. Reads
    /// `~/.shelbi/workers/<name>/status.yaml` files, no tmux probing.
    /// With NAME, prints a single row + the raw status.yaml.
    Status {
        /// Worker to inspect. Omit to show every declared worker.
        name: Option<String>,
    },
}

pub fn run(project_opt: Option<String>, cmd: WorkerCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        WorkerCmd::List => list(&project),
        WorkerCmd::Stop { name, keep_task } => stop(&project, &name, keep_task),
        WorkerCmd::Status { name } => status(&project, name.as_deref()),
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

fn status(project: &str, name: Option<&str>) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;

    if let Some(only) = name {
        if p.worker(only).is_none() {
            return Err(anyhow!(
                "worker `{only}` not declared in project `{project}` (known: {})",
                p.workers
                    .iter()
                    .map(|w| w.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        print_status_table(&[only.to_string()])?;
        println!();
        let path = shelbi_state::worker_status_path(only).map_err(|e| anyhow!(e))?;
        if path.exists() {
            let yaml = fs::read_to_string(&path)
                .map_err(|e| anyhow!("reading {}: {e}", path.display()))?;
            println!("--- {}", path.display());
            print!("{yaml}");
            if !yaml.ends_with('\n') {
                println!();
            }
        } else {
            println!("(no status.yaml yet — worker hasn't been polled)");
        }
        return Ok(());
    }

    if p.workers.is_empty() {
        println!("(no workers declared in {project})");
        return Ok(());
    }
    let names: Vec<String> = p.workers.iter().map(|w| w.name.clone()).collect();
    print_status_table(&names)
}

fn print_status_table(names: &[String]) -> Result<()> {
    let now = Utc::now();
    println!(
        "{:<12} {:<24} {:<14} {:<12} {}",
        "WORKER", "TASK", "STATE", "LAST SEEN", "IN STATE"
    );
    for name in names {
        let row = shelbi_state::load_worker_status(name).map_err(|e| anyhow!(e))?;
        match row {
            Some(s) => println!(
                "{:<12} {:<24} {:<14} {:<12} {}",
                s.worker,
                task_cell(&s),
                s.state.as_str(),
                format_ago(now, s.last_seen),
                format_ago(now, s.last_transition),
            ),
            None => println!(
                "{:<12} {:<24} {:<14} {:<12} {}",
                name, "—", "?", "never", "—"
            ),
        }
    }
    Ok(())
}

fn task_cell(s: &WorkerStatus) -> String {
    s.current_task.clone().unwrap_or_else(|| "(idle)".to_string())
}

/// Compact "12s" / "5m" / "2h" / "3d" style age. Floors at the unit
/// boundary so the output stays narrow for the table.
fn format_ago(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds();
    if secs < 0 {
        // Clock skew on a remote-written status file — show 0 rather
        // than a negative number that'd confuse the reader.
        return "0s".to_string();
    }
    let s = secs as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
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
        let moved = shelbi_state::move_task(project, &id, Column::Todo)
            .map_err(|e| anyhow!(e))?;
        if let Some((from, to, workflow)) = moved {
            if let Err(e) =
                shelbi_state::append_task_event(&id, &workflow, from, to, "worker:stop")
            {
                eprintln!("warning: append_task_event failed: {e}");
            }
        }
        released.push(id);
    }
    Ok(released)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK as TEST_LOCK;
    use shelbi_core::Task;
    use std::path::PathBuf;

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
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
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

        // The release path emits a `worker:stop` task event so the
        // orchestrator's events.log tail sees the column return — with the
        // workflow-aware shape from `Plans/workflows.md` §10.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(" task=fix-login "), "line: {}", lines[0]);
        assert!(
            lines[0].contains(" workflow=default "),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[0].contains(" in_progress -> todo "),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[0].contains(" reason=worker:stop "),
            "line: {}",
            lines[0]
        );
        assert!(lines[0].ends_with(" to_category=ready"), "line: {}", lines[0]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn format_ago_picks_unit_by_magnitude() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 6, 19, 12, 0, 0).unwrap();
        assert_eq!(format_ago(now, now - chrono::Duration::seconds(0)), "0s");
        assert_eq!(format_ago(now, now - chrono::Duration::seconds(45)), "45s");
        assert_eq!(format_ago(now, now - chrono::Duration::seconds(90)), "1m");
        assert_eq!(format_ago(now, now - chrono::Duration::minutes(75)), "1h");
        assert_eq!(format_ago(now, now - chrono::Duration::hours(50)), "2d");
        // Future timestamp from clock skew clamps to "0s" rather than
        // surfacing a negative value.
        assert_eq!(format_ago(now, now + chrono::Duration::seconds(5)), "0s");
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
