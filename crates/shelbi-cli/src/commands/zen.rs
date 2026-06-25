//! `shelbi zen <subcommand>` — Zen Mode controls + introspection primitives.
//!
//! - `on/off/pause/status` toggle and report Zen Mode state.
//! - `probe` reports facts about a finished branch (local checks, conflict,
//!   diff size, danger paths) as JSON.
//! - `pr-create/ci-watch/pr-merge` are the single-purpose PR primitives the
//!   orchestrator sequences to drive a merge.
//!
//! Mode toggles persist in `~/.shelbi/projects/<project>/state.json::zen_mode`
//! and write a `mode=zen <prev> -> <new> reason=user:cli` line to
//! `~/.shelbi/events.log`. The Alt+Z hotkey and the (future) crash-recovery
//! path emit the same shape with `reason=user:hotkey` and
//! `reason=system:crash-recovery` respectively — the orchestrator reacts to
//! all three the same way.
//!
//! All non-toggle commands print a single line on stdout (probe prints JSON)
//! and use exit-code + stderr for failures. The orchestrator parses the
//! lines directly.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::Utc;
use clap::Subcommand;

use shelbi_core::{danger_paths_for_project, Column, Project, Task, ZenDangerPaths};
use shelbi_orchestrator::zen::{self, CiVerdict, DryRunDecision};
use shelbi_state::{
    append_zen_dryrun_event, list_column, load_project, read_state, set_zen_mode, State,
    ZenModeState,
};

use crate::commands::require_project;

/// Default cadence for the dry-run preview loop. Slow enough that a
/// busy project doesn't see one probe stomping the next; fast enough
/// that a state change (worker handing off, user promoting a task)
/// shows up in the preview within one tick.
const DRYRUN_DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Subcommand)]
pub enum ZenCmd {
    /// Turn Zen Mode on — orchestrator may auto-merge and auto-promote
    /// finished tasks without waiting on a human reviewer.
    On,
    /// Turn Zen Mode off — every promotion goes through manual review.
    /// In-flight workers keep going; nothing already running is cancelled.
    Off,
    /// Pause Zen Mode — no *new* auto-promotions, but tasks already on
    /// the Zen track may still complete their merge.
    Pause,
    /// Show the current mode, configured local check commands, last
    /// crash timestamp (if any), and how many in-flight tasks are on
    /// the Zen track.
    Status,
    /// Run every probe primitive for `task` and print the JSON report.
    /// The task must be assigned to a worker so we know which worktree
    /// to probe.
    Probe { task_id: String },
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
    /// Print backlog task ids that are mechanically eligible for Zen
    /// auto-promotion, one per line, in priority order. Mechanical only —
    /// the orchestrator's prompt applies the judgment categories on top.
    Scan,
    /// Preview what Zen Mode would do without touching any state. Runs
    /// the backlog scan and the merge-conditions bar on every tick and
    /// logs each "would have …" decision to stdout, a dedicated dry-run
    /// log (`~/.shelbi/logs/zen-dryrun.log`), and the activity feed.
    /// Use this before flipping Zen on for real to confirm the policy
    /// matches your intent. No PRs, merges, or board moves happen.
    DryRun {
        /// Stop after this long. Accepts `30s`, `5m`, `2h`, `1d`, or a
        /// bare integer of seconds. Omit to run until Ctrl-C.
        #[arg(long, value_name = "DURATION")]
        r#for: Option<String>,
        /// Override the per-tick interval (default 5s). Same duration
        /// grammar as `--for`.
        #[arg(long, value_name = "DURATION")]
        interval: Option<String>,
    },
}

pub fn run(project_opt: Option<String>, cmd: ZenCmd) -> Result<()> {
    let project_name = require_project(project_opt)?;

    match cmd {
        ZenCmd::On => set(&project_name, ZenModeState::On),
        ZenCmd::Off => set(&project_name, ZenModeState::Off),
        ZenCmd::Pause => set(&project_name, ZenModeState::Paused),
        ZenCmd::Status => status(&project_name),
        ZenCmd::Probe { task_id } => {
            let project = load_project(&project_name).map_err(|e| anyhow!(e))?;
            let tf = shelbi_state::load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            let branch = tf
                .task
                .branch
                .clone()
                .unwrap_or_else(|| format!("shelbi/{}", tf.task.id));
            let report = zen::probe(&project, &tf.task, &branch).map_err(|e| anyhow!(e))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
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
        ZenCmd::Scan => {
            let project = load_project(&project_name).map_err(|e| anyhow!(e))?;
            let ids = zen::mechanically_eligible(&project).map_err(|e| anyhow!(e))?;
            for id in ids {
                println!("{id}");
            }
            Ok(())
        }
        ZenCmd::DryRun { r#for, interval } => {
            let duration = r#for
                .as_deref()
                .map(super::events::parse_duration)
                .transpose()?;
            let tick = match interval {
                Some(s) => super::events::parse_duration(&s)?,
                None => DRYRUN_DEFAULT_INTERVAL,
            };
            dry_run(&project_name, duration, tick)
        }
    }
}

fn set(project: &str, target: ZenModeState) -> Result<()> {
    set_zen_mode(project, target, "user:cli").map_err(|e| anyhow!(e))?;
    let state = read_state(project).map_err(|e| anyhow!(e))?;
    print_status(project, &state)
}

fn status(project: &str) -> Result<()> {
    let state = read_state(project).map_err(|e| anyhow!(e))?;
    print_status(project, &state)
}

fn print_status(project: &str, state: &State) -> Result<()> {
    println!("zen mode: {}", state.zen_mode);
    match load_project(project) {
        Ok(p) => {
            if p.zen.checks.local.is_empty() {
                println!(
                    "checks: (none configured — set zen.checks.local in {project}.yaml)"
                );
            } else {
                println!("checks:");
                for c in &p.zen.checks.local {
                    println!("  - {c}");
                }
            }
            print_danger_paths(&p);
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

fn print_danger_paths(p: &Project) {
    let resolved = danger_paths_for_project(p);
    let header = match &p.zen.danger_paths {
        ZenDangerPaths::Override(_) => "danger paths (project override):".to_string(),
        ZenDangerPaths::Extend(_) if p.detected_shapes.is_empty() => {
            "danger paths:".to_string()
        }
        ZenDangerPaths::Extend(_) => {
            let labels: Vec<&'static str> =
                p.detected_shapes.iter().map(|s| s.label()).collect();
            format!("danger paths (detected: {}):", labels.join(", "))
        }
    };
    println!("{header}");
    if resolved.is_empty() {
        println!("  (none)");
    } else {
        for path in &resolved {
            println!("  - {path}");
        }
    }
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

/// Drive the read-only Zen preview loop. Ticks every `interval` until
/// `duration` elapses (or forever, on Ctrl-C, when `duration` is None).
/// Each tick calls `zen::dry_run_tick` and logs newly-surfaced decisions
/// to three sinks:
///
/// - stdout — single-line, grep-able, machine-readable.
/// - `~/.shelbi/logs/zen-dryrun.log` — dedicated, timestamped, append-only.
/// - `~/.shelbi/events.log` — `zen-dryrun task=… action=… detail=…` lines
///   that the activity feed renders with a distinct visual tag.
///
/// Findings are deduplicated by `(action, task_id, detail)` for the
/// lifetime of the run so a stable board state doesn't produce repeated
/// log lines on every tick. The dedupe is intentionally lossy across
/// invocations — re-running `zen dry-run` is meant to give a fresh
/// preview, not respect history.
fn dry_run(project: &str, duration: Option<Duration>, interval: Duration) -> Result<()> {
    let project_obj = load_project(project).map_err(|e| anyhow!(e))?;

    let log_path = dryrun_log_path()?;
    let header = format!(
        "# shelbi zen dry-run — project={project} started={start} interval={int} duration={dur}\n",
        start = Utc::now().to_rfc3339(),
        int = format_duration(interval),
        dur = duration
            .map(format_duration)
            .unwrap_or_else(|| "until-ctrl-c".to_string()),
    );
    init_dryrun_log(&log_path, &header)?;

    eprintln!(
        "zen dry-run: previewing {project} every {} ({}). Ctrl-C to stop.",
        format_duration(interval),
        duration
            .map(|d| format!("for {}", format_duration(d)))
            .unwrap_or_else(|| "until interrupted".to_string()),
    );
    eprintln!("zen dry-run: log file → {}", log_path.display());

    let deadline = duration.map(|d| Instant::now() + d);
    let mut seen: HashSet<String> = HashSet::new();
    let mut first_tick = true;

    loop {
        let decisions = zen::dry_run_tick(&project_obj).map_err(|e| anyhow!(e))?;
        let mut new_this_tick = 0_usize;
        for d in decisions {
            let key = d.dedup_key();
            if !seen.insert(key) {
                continue;
            }
            emit_decision(&log_path, &d);
            new_this_tick += 1;
        }
        if first_tick && new_this_tick == 0 {
            eprintln!("zen dry-run: nothing to preview right now (no backlog candidates, no tasks in review).");
        }
        first_tick = false;

        let now = Instant::now();
        let sleep_for = match deadline {
            Some(end) if now >= end => break,
            Some(end) => interval.min(end.saturating_duration_since(now)),
            None => interval,
        };
        std::thread::sleep(sleep_for);
    }

    eprintln!("zen dry-run: window ended; exiting cleanly.");
    Ok(())
}

/// `~/.shelbi/logs/zen-dryrun.log` — the dedicated dry-run log path.
fn dryrun_log_path() -> Result<PathBuf> {
    let dir = shelbi_state::shelbi_home()
        .map_err(|e| anyhow!(e))?
        .join("logs");
    Ok(dir.join("zen-dryrun.log"))
}

/// Truncate + write the header for a fresh dry-run. Each run owns its
/// own log content — overlapping runs are explicitly out of scope and
/// would clobber each other here.
fn init_dryrun_log(path: &PathBuf, header: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    f.write_all(header.as_bytes())?;
    Ok(())
}

/// Surface one decision to all three sinks. Best-effort on the disk-bound
/// sinks: a transient I/O failure shouldn't kill the preview loop.
fn emit_decision(log_path: &PathBuf, d: &DryRunDecision) {
    let line = d.as_line();
    println!("{line}");

    let ts = Utc::now().to_rfc3339();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(f, "{ts} {line}");
    }
    // Activity feed integration — `zen-dryrun` prefix lets the TUI render
    // these rows with a distinct visual tag without changing the existing
    // task=/worker= line shapes.
    let _ = append_zen_dryrun_event(&d.task_id, d.action.as_str(), &d.detail);
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
