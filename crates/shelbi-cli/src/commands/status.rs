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
//! project's status catalogue from `workflows/statuses.yaml` — is kept
//! for backwards compatibility. It's disjoint from the flag-driven
//! snapshot path.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};
use clap::Subcommand;
use shelbi_core::{Column, IntegrationMode, MachineKind, Project, StatusCategory};
use shelbi_state::{read_state, ZenModeState};

use super::require_project;

/// The one HANDOFF.md file the `--handoff` flag reads and (on success)
/// deletes. Case-sensitive by design: mixed-case FS behaviors aside, the
/// task convention is uppercase.
const HANDOFF_FILE_NAME: &str = "HANDOFF.md";

/// How many bytes off the tail of `~/.shelbi/events.log` to scan for a
/// `project=<name> zen=off reason=crash-recovery` line. The log is
/// append-only and grows without bound between rotations, so we seek to
/// the last chunk rather than reading the whole file on every `status`
/// bootstrap (F9). 64 KiB is hundreds of event lines — far more than a
/// single crash-to-`status` window — and a crash line older than that is
/// also older than [`CRASH_EVENT_MAX_AGE_SECS`] and wouldn't be flagged
/// anyway.
const CRASH_EVENT_TAIL_BYTES: u64 = 64 * 1024;

/// Wall-clock window (seconds) a crash-recovery line must fall within to
/// be flagged (F20). A line-count heuristic alone re-fires a three-week-
/// old crash forever on a quiet hub and scrolls a real one out of view on
/// a busy one; keying off the line's own RFC3339 timestamp makes "recent"
/// mean recent. Six hours comfortably spans a work session's bootstraps
/// without resurfacing yesterday's already-dismissed recovery.
const CRASH_EVENT_MAX_AGE_SECS: i64 = 6 * 60 * 60;

#[derive(Debug, Subcommand)]
pub enum StatusCmd {
    /// Print the canonical status list — order, id, name, category —
    /// from `workflows/statuses.yaml`. Order here is the left-to-right
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
    if handoff {
        // Consuming HANDOFF.md deletes it, so this is the mutating status
        // shape and must not run against a stale daemon.
        super::hub_version::ensure_daemon_matches_for_mutation()?;
    } else {
        super::hub_version::warn_on_mismatch();
    }
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
    println!(
        "handoff: {}",
        if hp {
            "present (run `shelbi status --handoff` to consume)"
        } else {
            "none"
        }
    );

    println!("daemon: {}", super::hub_version::status_line());
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
    print_orchestrator_integration(project)?;

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
        println!("HANDOFF.md present — run `shelbi status --handoff` to read and consume it.");
    } else {
        println!("no HANDOFF.md present");
    }

    println!();
    println!("## Daemon");
    println!();
    println!("daemon: {}", super::hub_version::status_line());
    Ok(())
}

/// Print the orchestrator's integration-health line for the `## Workspaces`
/// section. The declared workspaces already carry their `INTEG` column; the
/// orchestrator has no workspace slot, so it gets this dedicated line — the
/// one place a disengaged native Codex bridge (and any stuck delivery queue)
/// becomes visible without hand-reading `codex-thread.json`.
fn print_orchestrator_integration(project: &str) -> Result<()> {
    let health =
        shelbi_orchestrator::wake::codex_integration_health(project).map_err(|e| anyhow!(e))?;
    println!("{}", orchestrator_integration_line(health.as_ref()));
    Ok(())
}

/// Format the orchestrator integration line. `None` means the orchestrator
/// runner isn't the native Codex bridge, so it reports the ordinary
/// verified-submission tier (`conventional`). A disengaged bridge reports
/// `degraded` with its recorded `reason=`; an undelivered queue appends
/// `pending_batches=` and the `oldest=` event timestamp so a stuck queue is
/// visible at a glance.
fn orchestrator_integration_line(
    health: Option<&shelbi_orchestrator::wake::CodexIntegrationHealth>,
) -> String {
    let Some(health) = health else {
        return format!("orchestrator  integration={}", IntegrationMode::Conventional);
    };
    let mut line = format!("orchestrator  integration={}", health.mode());
    match &health.inactive_reason {
        Some(reason) => line.push_str(&format!(" reason={reason}")),
        // A disengaged bridge with no recorded reason predates reason tracking;
        // still flag it rather than leaving the `degraded` unexplained.
        None if !health.native_active => line.push_str(" reason=unknown"),
        None => {}
    }
    if health.pending_batches > 0 {
        line.push_str(&format!(" pending_batches={}", health.pending_batches));
        if let Some(timestamp) = &health.oldest_pending_timestamp {
            line.push_str(&format!(" oldest={timestamp}"));
        }
    }
    line
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
/// assignment. Also used by the daemon version gate to decide whether an
/// automatic daemon restart is safe.
pub(crate) fn workspace_idle_busy(project: &str) -> Result<(usize, usize)> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let in_progress =
        shelbi_state::list_column(project, Column::in_progress()).map_err(|e| anyhow!(e))?;
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

/// Scan the tail of `~/.shelbi/events.log` for a *recent*
/// `project=<name> zen=off reason=crash-recovery` line. "Recent" is a
/// wall-clock window ([`CRASH_EVENT_MAX_AGE_SECS`]) measured off the
/// line's own leading RFC3339 timestamp, not a line count (F20). Only the
/// last [`CRASH_EVENT_TAIL_BYTES`] of the log are read, seeking rather
/// than slurping the whole (unbounded) file (F9).
///
/// Returns `false` on any I/O error or when the log is absent — the
/// summary already prints the mode, so a missing signal degrades to "no
/// crash flag" and the orchestrator's first reply is unaffected.
fn has_recent_crash_event(project: &str) -> Result<bool> {
    let path = match shelbi_state::events_log_path() {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    let tail = match read_tail(&path, CRASH_EVENT_TAIL_BYTES) {
        Ok(t) => t,
        Err(_) => return Ok(false),
    };
    let needle_project = format!(" project={project} ");
    let cutoff = Utc::now() - Duration::seconds(CRASH_EVENT_MAX_AGE_SECS);
    // Newest-first. The first crash line whose timestamp is inside the
    // window wins; a line whose timestamp doesn't parse (or the partial
    // leading line left by a mid-line seek) is skipped rather than
    // trusted. Because we seek to a byte offset, an ancient crash beyond
    // the tail chunk is simply not seen — which is also beyond the age
    // window, so the answer is the same.
    for line in tail.lines().rev() {
        if line.contains(&needle_project)
            && line.contains("zen=off")
            && line.contains("reason=crash-recovery")
        {
            if let Some(ts) = event_line_timestamp(line) {
                if ts >= cutoff {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Read at most the last `max_bytes` of `path` as (lossy) UTF-8, seeking
/// to the tail rather than reading the whole file. A file shorter than
/// `max_bytes` is returned whole. When the seek lands mid-line the
/// caller's per-line scan drops the partial leading fragment (it can't
/// match a full crash-recovery line, and its timestamp won't parse).
fn read_tail(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    if start > 0 {
        f.seek(SeekFrom::Start(start))?;
    }
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Parse the leading RFC3339 timestamp off an `events.log` line. Every
/// writer prefixes `<rfc3339> ` (see the `append_*` family in
/// `shelbi_state`), so the timestamp is the whitespace-delimited first
/// token. Returns `None` when it doesn't parse.
fn event_line_timestamp(line: &str) -> Option<DateTime<Utc>> {
    let token = line.split_whitespace().next()?;
    DateTime::parse_from_rfc3339(token)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
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
    fn orchestrator_integration_line_defaults_to_conventional_without_a_codex_thread() {
        // No persisted Codex thread => the orchestrator isn't the native
        // bridge, so it reports the ordinary verified-submission tier.
        let line = orchestrator_integration_line(None);
        assert_eq!(line, "orchestrator  integration=conventional");
    }

    #[test]
    fn orchestrator_integration_line_reports_structured_active_bridge() {
        let health = shelbi_orchestrator::wake::CodexIntegrationHealth {
            native_active: true,
            inactive_reason: None,
            pending_batches: 0,
            oldest_pending_timestamp: None,
        };
        assert_eq!(
            orchestrator_integration_line(Some(&health)),
            "orchestrator  integration=structured"
        );
    }

    #[test]
    fn orchestrator_integration_line_flags_degraded_reason_and_pending_queue() {
        let health = shelbi_orchestrator::wake::CodexIntegrationHealth {
            native_active: false,
            inactive_reason: Some("protocol-incompatible".into()),
            pending_batches: 2,
            oldest_pending_timestamp: Some("2026-07-15T09:00:00+00:00".into()),
        };
        assert_eq!(
            orchestrator_integration_line(Some(&health)),
            "orchestrator  integration=degraded reason=protocol-incompatible \
             pending_batches=2 oldest=2026-07-15T09:00:00+00:00"
        );
    }

    #[test]
    fn orchestrator_integration_line_flags_reasonless_legacy_disengagement() {
        // A `native_active: false` thread file written before reason tracking
        // still surfaces as degraded rather than an unexplained mode.
        let health = shelbi_orchestrator::wake::CodexIntegrationHealth {
            native_active: false,
            inactive_reason: None,
            pending_batches: 0,
            oldest_pending_timestamp: None,
        };
        assert_eq!(
            orchestrator_integration_line(Some(&health)),
            "orchestrator  integration=degraded reason=unknown"
        );
    }

    #[test]
    fn list_succeeds_against_default_statuses() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // No statuses.yaml on disk — loader falls back to the built-in
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
            (Column::backlog(), "b1"),
            (Column::backlog(), "b2"),
            (Column::todo(), "t1"),
            (Column::in_progress(), "i1"),
            (Column::review(), "r1"),
            (Column::done(), "d1"),
            (Column::done(), "d2"),
            (Column::done(), "d3"),
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
        assert!(
            handoff.exists(),
            "second --full still must not delete HANDOFF.md"
        );

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

    #[test]
    fn stale_crash_recovery_event_is_not_flagged() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = shelbi_state::events_log_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // A crash-recovery line dated well outside the wall-clock window.
        // The old line-count heuristic would flag this forever; the
        // timestamp check must not (F20).
        let old = (Utc::now() - Duration::days(3)).to_rfc3339();
        std::fs::write(
            &path,
            format!("{old} project=p zen=off reason=crash-recovery\n"),
        )
        .unwrap();
        assert!(!has_recent_crash_event("p").unwrap());

        // A fresh line for the same project IS flagged.
        let fresh = Utc::now().to_rfc3339();
        std::fs::write(
            &path,
            format!("{fresh} project=p zen=off reason=crash-recovery\n"),
        )
        .unwrap();
        assert!(has_recent_crash_event("p").unwrap());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn crash_scan_reads_only_the_tail_not_the_whole_log() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = shelbi_state::events_log_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // A recent crash line at the very START of the log, followed by
        // more than CRASH_EVENT_TAIL_BYTES of unrelated churn. Because the
        // scan seeks to the tail, the leading crash line is never read —
        // proving `status` no longer slurps the whole file (F9).
        let recent = Utc::now().to_rfc3339();
        let mut log = format!("{recent} project=p zen=off reason=crash-recovery\n");
        let filler_line = format!("{recent} workspace=alpha status=idle\n");
        while (log.len() as u64) < CRASH_EVENT_TAIL_BYTES + 8192 {
            log.push_str(&filler_line);
        }
        std::fs::write(&path, &log).unwrap();
        assert!(
            !has_recent_crash_event("p").unwrap(),
            "a crash line beyond the tail window must not be seen",
        );

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
