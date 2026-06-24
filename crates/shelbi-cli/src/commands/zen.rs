//! `shelbi zen <on|off|pause|status>` — toggle Zen Mode and report state.
//!
//! Zen Mode is the trust boundary that lets the orchestrator auto-promote
//! finished tasks through CI and into the default branch without waiting
//! on a human reviewer. The tri-state lives in
//! `~/.shelbi/projects/<project>/state.json::zen_mode` and is one of
//! `off`, `paused`, or `on`. Every toggle writes a `project=<name>` line
//! to `~/.shelbi/events.log` tagged `reason=user:zen-<action>` so the
//! activity feed shows who flipped what and when.

use anyhow::{anyhow, Result};
use clap::Subcommand;

use shelbi_core::{Column, Task};
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
}

pub fn run(project: Option<String>, cmd: ZenCmd) -> Result<()> {
    let name = require_project(project)?;
    match cmd {
        ZenCmd::On => set(&name, ZenModeState::On, "on"),
        ZenCmd::Off => set(&name, ZenModeState::Off, "off"),
        ZenCmd::Pause => set(&name, ZenModeState::Paused, "pause"),
        ZenCmd::Status => status(&name),
    }
}

fn set(project: &str, target: ZenModeState, action: &str) -> Result<()> {
    let mut state = read_state(project).map_err(|e| anyhow!(e))?;
    state.zen_mode = target;
    write_state(project, &state).map_err(|e| anyhow!(e))?;
    // Best-effort: an events-log write failure shouldn't surface as a
    // hard error to the user — the state change already succeeded and is
    // what they asked for. Mirror the pattern used by `quit_project`.
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
    // Best-effort project load: a malformed YAML shouldn't mask a
    // toggle that already landed on disk. Surface the parse error in
    // the checks line instead of aborting.
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

/// Tasks currently in [`Column::InProgress`] that the orchestrator should
/// treat as on the Zen track. The intent of pause is "in-flight merges
/// complete", so we count any task that isn't explicitly opted-out unless
/// Zen is fully off — in that case, only tasks with an explicit per-task
/// `zen.enabled = true` opt-in still belong to the Zen track.
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
        // Pause means "no new auto-promotions, in-flight merges complete";
        // for status accounting that means a task in progress that would
        // be Zen-eligible while On should still show as in-flight while
        // Paused.
        let unset = make_task("a", Column::InProgress, None);
        let opt_out = make_task("b", Column::InProgress, Some(false));
        assert!(zen_applies(&unset, ZenModeState::Paused));
        assert!(!zen_applies(&opt_out, ZenModeState::Paused));
    }
}
