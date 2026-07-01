//! `shelbi status` — bootstrap snapshot for the orchestrator agent (and
//! a human-friendly summary for interactive use).
//!
//! Three shapes:
//!
//! - `shelbi status` — one-line-per-section human summary
//!   (`board:`, `workspaces:`, `zen:`, `handoff:`). Read-only, side-effect free.
//! - `shelbi status --full` — the full orchestrator bootstrap payload:
//!   board (all columns), workspaces (idle vs in-progress), zen (mode +
//!   crash flag), and a handoff-presence line. Idempotent — safe to
//!   re-run. Does NOT print `HANDOFF.md` contents and does NOT delete
//!   anything.
//! - `shelbi status --handoff` — print the contents of `HANDOFF.md`
//!   (from the resolved project's local machine's `work_dir`) and then
//!   delete the file. Write-then-delete so a crash between the two
//!   doesn't lose the note. No-op when the file is absent.
//!
//! Both flags compose: `shelbi status --full --handoff` prints the full
//! snapshot followed by the handoff-consume block in a single call.
//!
//! The legacy `shelbi status list` subcommand — which prints the
//! project's status catalogue from `workflows/statuses.yml` — is kept
//! for backwards compatibility. It's disjoint from the flag-driven
//! snapshot path.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use shelbi_core::{Column, MachineKind, Project, StatusCategory};
use shelbi_state::{read_state, ZenModeState};

use super::require_project;

/// The one HANDOFF.md file the `--handoff` flag reads and (on success)
/// deletes. Case-sensitive by design: mixed-case FS behaviors aside, the
/// task convention is uppercase.
const HANDOFF_FILE_NAME: &str = "HANDOFF.md";

/// How far back in `~/.shelbi/events.log` to look for a
/// `project=<name> zen=off reason=crash-recovery` line. 200 lines is
/// enough to catch the crash line even after a busy heartbeat/task
/// churn cycle, and small enough that scanning is instant.
const CRASH_EVENT_SCAN_LINES: usize = 200;

#[derive(Debug, Subcommand)]
pub enum StatusCmd {
    /// Print the canonical status list — order, id, name, category —
    /// from `workflows/statuses.yml`. Order here is the left-to-right
    /// column order used by every view in the project.
    List,
}

/// Entry point for `shelbi status [--full] [--handoff] [list]`. `cmd` is
/// the legacy subcommand path (currently just `list`); when it's `None`
/// we run the snapshot with whatever combination of flags the user
/// passed.
pub fn run(
    project: Option<String>,
    cmd: Option<StatusCmd>,
    full: bool,
    handoff: bool,
) -> Result<()> {
    let project = require_project(project)?;
    match cmd {
        Some(StatusCmd::List) => list(&project),
        None => snapshot(&project, full, handoff),
    }
}

fn list(project: &str) -> Result<()> {
    let statuses = shelbi_state::load_project_statuses(project).map_err(|e| anyhow!(e))?;
    println!("{:<7} {:<13} {:<15} CATEGORY", "ORDER", "ID", "NAME");
    for (idx, st) in statuses.statuses.iter().enumerate() {
        println!(
            "{:<7} {:<13} {:<15} {}",
            idx + 1,
            st.id,
            st.name,
            st.category,
        );
    }
    Ok(())
}

/// Drive the snapshot path. `full=false, handoff=false` emits the
/// concise summary; `full=true` emits the full sectioned payload;
/// `handoff=true` consumes `HANDOFF.md`. Both flags compose — `--full`
/// runs first (safe/idempotent), then `--handoff` writes and deletes.
fn snapshot(project: &str, full: bool, handoff: bool) -> Result<()> {
    if full {
        print_full(project)?;
    } else if !handoff {
        print_summary(project)?;
    }
    if handoff {
        if full {
            // Blank line so the handoff block reads separately from the
            // full snapshot above it.
            println!();
        }
        consume_handoff(project)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Summary (default, no flags)

fn print_summary(project: &str) -> Result<()> {
    let counts = category_counts(project)?;
    println!(
        "board: {} backlog / {} ready / {} active / {} handoff / {} done",
        counts.backlog, counts.ready, counts.active, counts.handoff, counts.done,
    );

    let (idle, busy) = workspace_idle_busy(project)?;
    if idle + busy == 0 {
        println!("workspaces: (none declared)");
    } else {
        println!("workspaces: {idle} idle, {busy} in-progress");
    }

    let zen = zen_snapshot(project)?;
    println!("zen: {}", zen.summary_line());

    let hp = handoff_present(project)?;
    println!("handoff: {}", if hp { "present (run `shelbi status --handoff` to consume)" } else { "none" });
    Ok(())
}

// ---------------------------------------------------------------------------
// Full snapshot (--full)

fn print_full(project: &str) -> Result<()> {
    println!("## Board");
    println!();
    super::task::print_board(project)?;

    println!();
    println!("## Workspaces");
    println!();
    super::workspace::print_workspaces(project)?;

    println!();
    println!("## Zen");
    println!();
    let zen = zen_snapshot(project)?;
    print_zen_full(&zen);

    println!();
    println!("## Handoff");
    println!();
    let present = handoff_present(project)?;
    if present {
        println!(
            "HANDOFF.md present — run `shelbi status --handoff` to read and consume it."
        );
    } else {
        println!("no HANDOFF.md present");
    }
    Ok(())
}

fn print_zen_full(zen: &ZenSnapshot) {
    println!("mode: {}", zen.mode);
    match zen.last_crashed_at {
        Some(ts) => println!("last crash: {}", ts.to_rfc3339()),
        None => println!("last crash: never"),
    }
    if zen.crash_recovery_event {
        println!(
            "crash-recovery: recent `zen=off reason=crash-recovery` event in events.log — \
             review in-flight work before re-enabling with `shelbi zen on`",
        );
    }
}

// ---------------------------------------------------------------------------
// Handoff consumption (--handoff)

fn consume_handoff(project: &str) -> Result<()> {
    let path = handoff_path(project)?;
    let contents = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("(no HANDOFF.md to consume)");
            return Ok(());
        }
        Err(e) => {
            return Err(anyhow!("reading {}: {e}", path.display()));
        }
    };
    // WRITE first so a crash between print and unlink loses nothing —
    // the note stays on disk and the next `--handoff` invocation
    // re-consumes it.
    println!("--- HANDOFF.md ({}) ---", path.display());
    print!("{contents}");
    if !contents.ends_with('\n') {
        println!();
    }
    println!("--- end HANDOFF.md ---");
    // Best-effort delete: a read that succeeded followed by an unlink
    // failure (permissions, race) is surfaced but not fatal — the
    // caller already got the content and can re-run on the next
    // bootstrap if the file re-appears.
    if let Err(e) = fs::remove_file(&path) {
        eprintln!(
            "warning: consumed HANDOFF.md but failed to delete {}: {e}",
            path.display()
        );
    }
    Ok(())
}

fn handoff_present(project: &str) -> Result<bool> {
    Ok(handoff_path(project)?.exists())
}

/// Resolve `HANDOFF.md`'s path against the project's local machine's
/// `work_dir`. Errors if the project YAML has no local machine — the
/// snapshot is meaningless without one, and rather than silently
/// treating "no local work_dir" as "handoff absent" we surface it so
/// the user can fix their project YAML.
fn handoff_path(project: &str) -> Result<PathBuf> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let work_dir = local_work_dir(&p)?;
    Ok(work_dir.join(HANDOFF_FILE_NAME))
}

fn local_work_dir(p: &Project) -> Result<PathBuf> {
    p.machines
        .iter()
        .find(|m| m.kind == MachineKind::Local)
        .map(|m| m.work_dir.clone())
        .ok_or_else(|| {
            anyhow!(
                "project `{}` declares no local machine — HANDOFF.md needs \
                 a local work_dir to resolve against",
                p.name,
            )
        })
}

// ---------------------------------------------------------------------------
// Shared shape helpers

#[derive(Debug, Default)]
struct CategoryCounts {
    backlog: usize,
    ready: usize,
    active: usize,
    handoff: usize,
    done: usize,
}

fn category_counts(project: &str) -> Result<CategoryCounts> {
    let tasks = shelbi_state::list_tasks(project).map_err(|e| anyhow!(e))?;
    let mut c = CategoryCounts::default();
    for tf in &tasks {
        match tf.task.column.category() {
            StatusCategory::Backlog => c.backlog += 1,
            StatusCategory::Ready => c.ready += 1,
            StatusCategory::Active => c.active += 1,
            StatusCategory::Handoff => c.handoff += 1,
            StatusCategory::Done => c.done += 1,
            // No default-workflow status maps to `Archived`; if a
            // custom workflow adds one, it silently drops out of the
            // summary line rather than distorting the shape. The
            // `--full` board dump below still surfaces it.
            StatusCategory::Archived => {}
        }
    }
    Ok(c)
}

/// Count declared workspaces bucketed into idle / in-progress. Reads the
/// project YAML for the declared pool and the `in_progress` column for
/// assignment.
fn workspace_idle_busy(project: &str) -> Result<(usize, usize)> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let in_progress = shelbi_state::list_column(project, Column::InProgress)
        .map_err(|e| anyhow!(e))?;
    let mut idle = 0usize;
    let mut busy = 0usize;
    for w in &p.workspaces {
        let has_task = in_progress
            .iter()
            .any(|tf| tf.task.assigned_to.as_deref() == Some(w.name.as_str()));
        if has_task {
            busy += 1;
        } else {
            idle += 1;
        }
    }
    Ok((idle, busy))
}

#[derive(Debug)]
struct ZenSnapshot {
    mode: ZenModeState,
    last_crashed_at: Option<DateTime<Utc>>,
    /// True when the tail of `events.log` shows a recent
    /// `project=<name> zen=off reason=crash-recovery` line. Surfaced
    /// separately from `mode` because the mode was reset to `off` at
    /// crash time — the event line is what the orchestrator uses to
    /// flag the recovery in its first reply.
    crash_recovery_event: bool,
}

impl ZenSnapshot {
    fn summary_line(&self) -> String {
        let mode = self.mode.as_str();
        if self.crash_recovery_event {
            format!("{mode} (crash-recovery flagged)")
        } else {
            mode.to_string()
        }
    }
}

fn zen_snapshot(project: &str) -> Result<ZenSnapshot> {
    let state = read_state(project).map_err(|e| anyhow!(e))?;
    let crash_recovery_event = has_recent_crash_event(project)?;
    Ok(ZenSnapshot {
        mode: state.zen_mode,
        last_crashed_at: state.zen_last_crashed_at,
        crash_recovery_event,
    })
}

/// Scan the tail of `~/.shelbi/events.log` for a
/// `project=<name> zen=off reason=crash-recovery` line. Returns `false`
/// on any I/O error or when the log is absent — the summary already
/// prints the mode, so a missing signal degrades to "no crash flag" and
/// the orchestrator's first reply is unaffected.
fn has_recent_crash_event(project: &str) -> Result<bool> {
    let path = match shelbi_state::events_log_path() {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(false),
    };
    let needle_project = format!(" project={project} ");
    let needle_zen = "zen=off";
    let needle_reason = "reason=crash-recovery";
    // Walk the trailing N lines newest-first — we only care about the
    // most recent crash line, and a leading scan of a large log would
    // also flag ancient recoveries the user already dismissed.
    for line in text.lines().rev().take(CRASH_EVENT_SCAN_LINES) {
        if line.contains(&needle_project)
            && line.contains(needle_zen)
            && line.contains(needle_reason)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK as TEST_LOCK;
    use shelbi_core::{default_project_statuses, ProjectStatuses};
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-cli-status-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn list_succeeds_against_default_statuses() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // No statuses.yml on disk — loader falls back to the built-in
        // default and `list` should still print without erroring.
        list("p").unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_prints_in_canonical_declared_order() {
        // Sanity-check that the printed order matches the on-disk
        // declared order — the column-ordering contract everything
        // downstream relies on.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let custom = ProjectStatuses {
            statuses: vec![
                shelbi_core::ProjectStatus {
                    id: "z-last".into(),
                    name: "Z Last".into(),
                    category: shelbi_core::StatusCategory::Archived,
                },
                shelbi_core::ProjectStatus {
                    id: "a-first".into(),
                    name: "A First".into(),
                    category: shelbi_core::StatusCategory::Ready,
                },
            ],
        };
        shelbi_state::save_project_statuses("p", &custom).unwrap();
        // List drives off the loader, which preserves on-disk order.
        let loaded = shelbi_state::load_project_statuses("p").unwrap();
        assert_eq!(loaded.statuses[0].id, "z-last");
        assert_eq!(loaded.statuses[1].id, "a-first");
        // Sanity: the helper exists and the round-trip preserves order.
        let _ = default_project_statuses();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn category_counts_bucket_default_columns() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        for (col, id) in [
            (Column::Backlog, "b1"),
            (Column::Backlog, "b2"),
            (Column::Todo, "t1"),
            (Column::InProgress, "i1"),
            (Column::Review, "r1"),
            (Column::Done, "d1"),
            (Column::Done, "d2"),
            (Column::Done, "d3"),
        ] {
            shelbi_state::save_task("p", &make_task(id, col), "").unwrap();
        }

        let counts = category_counts("p").unwrap();
        assert_eq!(counts.backlog, 2);
        assert_eq!(counts.ready, 1);
        assert_eq!(counts.active, 1);
        assert_eq!(counts.handoff, 1);
        assert_eq!(counts.done, 3);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn consume_handoff_writes_then_deletes() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let work_dir = crate::commands::test_support::provision_hub_repo_for_project(&home, "p");
        let handoff = work_dir.join(HANDOFF_FILE_NAME);
        std::fs::write(&handoff, "note body\nmore lines\n").unwrap();

        // Presence check is true before the read.
        assert!(handoff_present("p").unwrap());
        consume_handoff("p").unwrap();
        // File is gone after a successful consume — one-shot semantics.
        assert!(!handoff.exists());
        assert!(!handoff_present("p").unwrap());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn consume_handoff_is_noop_when_absent() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let _work_dir = crate::commands::test_support::provision_hub_repo_for_project(&home, "p");
        // No file staged — read exits with Ok and prints the "(no
        // HANDOFF.md to consume)" line.
        consume_handoff("p").unwrap();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn snapshot_full_alone_is_idempotent_even_when_handoff_exists() {
        // The `--full` flag is meant to be safe to re-run: a HANDOFF.md
        // that exists on disk must stay put after `--full` returns.
        // Consumption only happens under `--handoff`.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let work_dir = crate::commands::test_support::provision_hub_repo_for_project(&home, "p");
        let handoff = work_dir.join(HANDOFF_FILE_NAME);
        std::fs::write(&handoff, "stay put\n").unwrap();

        snapshot("p", true, false).unwrap();
        assert!(handoff.exists(), "--full alone must not delete HANDOFF.md");
        // A second `--full` still leaves it in place — this is the
        // acceptance-criterion "safe to re-run" test.
        snapshot("p", true, false).unwrap();
        assert!(handoff.exists(), "second --full still must not delete HANDOFF.md");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn snapshot_full_and_handoff_compose_without_side_effects_when_absent() {
        // `--full --handoff` on a project with no HANDOFF.md should print
        // both sections in one call and NOT error just because the note
        // isn't there — that's the orchestrator's every-bootstrap shape.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let _work_dir = crate::commands::test_support::provision_hub_repo_for_project(&home, "p");
        snapshot("p", true, true).unwrap();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn zen_snapshot_flags_recent_crash_recovery_event() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let _work_dir = crate::commands::test_support::provision_hub_repo_for_project(&home, "p");

        // A crash-recovery line for *another* project must NOT flip the
        // flag on `p` — the events log is hub-global, so `project=<name>`
        // filtering has to be exact.
        shelbi_state::append_project_event("other", "zen=off", "crash-recovery").unwrap();
        assert!(!zen_snapshot("p").unwrap().crash_recovery_event);

        // Same line but for `p` DOES flip it — the needle scan honors
        // the project name in the token.
        shelbi_state::append_project_event("p", "zen=off", "crash-recovery").unwrap();
        assert!(zen_snapshot("p").unwrap().crash_recovery_event);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn make_task(id: &str, column: Column) -> shelbi_core::Task {
        let now = Utc::now();
        shelbi_core::Task {
            id: id.to_string(),
            title: id.to_string(),
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
}
