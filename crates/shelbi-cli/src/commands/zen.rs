//! `shelbi zen <subcommand>` — Zen Mode controls (`on/off/pause/status`)
//! plus the single-purpose PR primitives (`pr-create/ci-watch/pr-merge`)
//! that the orchestrator sequences to drive a worker's branch through to
//! merge.
//!
//! Mode toggles persist in `~/.shelbi/projects/<project>/state.json::zen_mode`
//! and write a `zen=<state>` line to `~/.shelbi/events.log` tagged
//! `reason=user:zen-<action>` so the activity feed shows who flipped what.
//!
//! PR primitives each print a single line on stdout and use exit-code +
//! stderr for the failure path. The orchestrator parses the line directly;
//! verbatim gh stderr is the failure detail.

use std::time::Duration;

use anyhow::{anyhow, Result};
use clap::Subcommand;

use shelbi_core::{Column, Task};
use shelbi_orchestrator::zen::{self, CiVerdict};
use shelbi_state::{
    append_project_event, list_column, load_project, read_state, write_state, State, ZenModeState,
};

use crate::commands::require_project;

#[derive(Debug, Subcommand)]
pub enum ZenCmd {
    /// Turn Zen Mode on — orchestrator may auto-merge and auto-promote
    /// finished tasks without waiting on a human reviewer.
    On,
    /// Turn Zen Mode off — every promotion goes through manual review.
    /// In-flight workers keep going; nothing already running is cancelled.
    Off,
    /// Pause Zen Mode — no *new* auto-promotions, but tasks already on
    /// the Zen track may still complete their merge. Useful when you
    /// want to triage without aborting work that's mid-flight.
    Pause,
    /// Show the current mode, the configured local check commands, the
    /// most recent Zen-Mode crash timestamp (if any), and how many
    /// in-flight tasks are still on the Zen track.
    Status,
    /// Push the worker's branch and open a PR for the task. Idempotent —
    /// returns the existing PR number if one is already open for the
    /// branch. Prints the PR number on stdout.
    PrCreate { task_id: String },
    /// Watch the PR's required checks until they settle or the timeout
    /// fires. Prints `green` / `red:<check>:<summary>` / `timeout` on
    /// stdout. Exit code is 0 only for `green`.
    CiWatch {
        pr_number: u64,
        /// Override the project-level CI timeout (default 15m, set via
        /// `zen.ci_timeout` in the project YAML). Accepts `30s`, `5m`,
        /// `2h`, `1d`, or a bare integer of seconds.
        #[arg(long, value_name = "DURATION")]
        timeout: Option<String>,
    },
    /// Squash-merge the PR and delete the source branch. Prints the
    /// merge SHA on stdout.
    PrMerge { pr_number: u64 },
}

pub fn run(project_opt: Option<String>, cmd: ZenCmd) -> Result<()> {
    let project_name = require_project(project_opt)?;

    match cmd {
        ZenCmd::On => set(&project_name, ZenModeState::On, "on"),
        ZenCmd::Off => set(&project_name, ZenModeState::Off, "off"),
        ZenCmd::Pause => set(&project_name, ZenModeState::Paused, "pause"),
        ZenCmd::Status => status(&project_name),
        ZenCmd::PrCreate { task_id } => {
            let project = load_project(&project_name).map_err(|e| anyhow!(e))?;
            let tf = shelbi_state::load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            let pr = zen::pr_create(&project, &project_name, &tf.task, &tf.body)
                .map_err(|e| anyhow!(e))?;
            println!("{pr}");
            Ok(())
        }
        ZenCmd::CiWatch { pr_number, timeout } => {
            let project = load_project(&project_name).map_err(|e| anyhow!(e))?;
            let timeout = match timeout {
                Some(s) => super::events::parse_duration(&s)?,
                None => project.zen.ci_timeout,
            };
            let verdict =
                zen::ci_watch(&project, pr_number, timeout).map_err(|e| anyhow!(e))?;
            println!("{}", verdict.as_line());
            match verdict {
                CiVerdict::Green => Ok(()),
                CiVerdict::Red { .. } => {
                    eprintln!("ci-watch: required check failed (see stdout for details)");
                    std::process::exit(1);
                }
                CiVerdict::Timeout => {
                    eprintln!(
                        "ci-watch: timed out after {} — checks still pending",
                        format_duration(timeout)
                    );
                    std::process::exit(2);
                }
            }
        }
        ZenCmd::PrMerge { pr_number } => {
            let project = load_project(&project_name).map_err(|e| anyhow!(e))?;
            let sha = zen::pr_merge(&project, pr_number).map_err(|e| anyhow!(e))?;
            println!("{sha}");
            Ok(())
        }
    }
}

fn set(project: &str, target: ZenModeState, action: &str) -> Result<()> {
    let mut state = read_state(project).map_err(|e| anyhow!(e))?;
    state.zen_mode = target;
    write_state(project, &state).map_err(|e| anyhow!(e))?;
    let _ = append_project_event(
        project,
        &format!("zen={}", target.as_str()),
        &format!("user:zen-{action}"),
    );
    print_status(project, &state)
}

fn status(project: &str) -> Result<()> {
    let state = read_state(project).map_err(|e| anyhow!(e))?;
    print_status(project, &state)
}

fn print_status(project: &str, state: &State) -> Result<()> {
    println!("zen mode: {}", state.zen_mode);
    match load_project(project) {
        Ok(p) if p.zen.checks.local.is_empty() => {
            println!("checks: (none configured — set zen.checks.local in {project}.yaml)");
        }
        Ok(p) => {
            println!("checks:");
            for c in &p.zen.checks.local {
                println!("  - {c}");
            }
        }
        Err(e) => println!("checks: (could not read {project}.yaml: {e})"),
    }
    match state.zen_last_crashed_at {
        Some(ts) => println!("last crash: {}", ts.to_rfc3339()),
        None => println!("last crash: never"),
    }
    let in_flight = count_in_flight_zen(project, state.zen_mode).unwrap_or(0);
    println!("in-flight zen tasks: {in_flight}");
    Ok(())
}

fn count_in_flight_zen(project: &str, mode: ZenModeState) -> Result<usize> {
    let tasks = list_column(project, Column::InProgress).map_err(|e| anyhow!(e))?;
    Ok(tasks.iter().filter(|tf| zen_applies(&tf.task, mode)).count())
}

fn zen_applies(task: &Task, mode: ZenModeState) -> bool {
    let explicit = task.zen.as_ref().and_then(|z| z.enabled);
    match (explicit, mode) {
        (Some(b), _) => b,
        (None, ZenModeState::On) | (None, ZenModeState::Paused) => true,
        (None, ZenModeState::Off) => false,
    }
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs % 86_400 == 0 {
        format!("{}d", secs / 86_400)
    } else if secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else if secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::TaskZenConfig;

    fn make_task(id: &str, col: Column, zen_enabled: Option<bool>) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column: col,
            priority: 0,
            assigned_to: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: zen_enabled.map(|b| TaskZenConfig {
                enabled: Some(b),
                checks_additional: Vec::new(),
                checks_only: Vec::new(),
            }),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn zen_off_counts_only_explicit_opt_ins() {
        let opt_in = make_task("a", Column::InProgress, Some(true));
        let opt_out = make_task("b", Column::InProgress, Some(false));
        let unset = make_task("c", Column::InProgress, None);
        assert!(zen_applies(&opt_in, ZenModeState::Off));
        assert!(!zen_applies(&opt_out, ZenModeState::Off));
        assert!(!zen_applies(&unset, ZenModeState::Off));
    }

    #[test]
    fn zen_on_counts_unset_and_opt_ins() {
        let unset = make_task("a", Column::InProgress, None);
        let opt_in = make_task("b", Column::InProgress, Some(true));
        let opt_out = make_task("c", Column::InProgress, Some(false));
        assert!(zen_applies(&unset, ZenModeState::On));
        assert!(zen_applies(&opt_in, ZenModeState::On));
        assert!(!zen_applies(&opt_out, ZenModeState::On));
    }

    #[test]
    fn zen_paused_matches_on_for_in_flight_counting() {
        let unset = make_task("a", Column::InProgress, None);
        let opt_out = make_task("b", Column::InProgress, Some(false));
        assert!(zen_applies(&unset, ZenModeState::Paused));
        assert!(!zen_applies(&opt_out, ZenModeState::Paused));
    }

    #[test]
    fn duration_formats_to_compact_units() {
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration::from_secs(300)), "5m");
        assert_eq!(format_duration(Duration::from_secs(7200)), "2h");
        assert_eq!(format_duration(Duration::from_secs(86_400)), "1d");
    }
}
