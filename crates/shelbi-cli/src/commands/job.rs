//! `shelbi job` — watched jobs in the awareness store
//! (`shelbi_state::awareness`). `job run` starts a command detached and returns
//! at once; `job watch` follows a pid, a commit's CI, or a PR. The hub poller
//! writes one `job` line to `events.log` when each finishes, so the
//! orchestrator is told instead of having to poll.
//!
//! A finished job leaves the watch list for the job history
//! (`jobs/history.json`, kept for the newest 200 jobs within 30 days): `job
//! list --all` lists it, and `job log <id>` prints a command job's output,
//! running or finished, while its log is kept.
//!
//! `__job-exec` is the hidden runner `job run` starts as the child of a
//! Shelbi session of its own (`<project>/job/<id>`), so the Jobs screen can
//! show the job's live terminal; it runs the command under the caller's login
//! shell in a PTY and keeps the output in `output.log` too.

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use shelbi_orchestrator::awareness as runner;
use shelbi_state::awareness::{self, Job, JobKind, JobRecord, NewJob};

/// How often `job log --follow` looks for new output and for the job ending.
const FOLLOW_POLL: Duration = Duration::from_millis(250);

use super::require_registered_project;

#[derive(Debug, Subcommand)]
pub enum JobCmd {
    /// Run a command detached and get notified when it exits:
    /// `shelbi job run --label "desktop tests" -- cargo test`. Prints the job
    /// id at once; the command's output goes to the job's `output.log`.
    Run {
        /// A short name for the event line and `job list`.
        #[arg(long)]
        label: Option<String>,
        /// The command and its arguments, after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        cmd: Vec<String>,
    },
    /// Watch something that is already running and get notified when it
    /// finishes: a process (`--pid`), a commit's CI (`--ci <sha>`), or a pull
    /// request merging, closing, or its checks settling (`--pr <n>`).
    #[command(group(clap::ArgGroup::new("target").required(true).args(["pid", "ci", "pr"])))]
    Watch {
        #[arg(long, value_name = "PID")]
        pid: Option<u32>,
        #[arg(long, value_name = "SHA")]
        ci: Option<String>,
        #[arg(long, value_name = "NUMBER")]
        pr: Option<u64>,
        /// A short name for the event line and `job list`.
        #[arg(long)]
        label: Option<String>,
    },
    /// List the jobs still being watched. `--all` adds the finished ones
    /// (the newest 200 from the last 30 days), newest first, with their
    /// status, exit code, duration and log path.
    List {
        /// Include finished jobs after the running ones.
        #[arg(long, visible_alias = "history")]
        all: bool,
        /// Print them as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print a job's output: the whole log, or its last lines with `-n`.
    /// `-f` streams a running job's output until it exits. Works on finished
    /// jobs while the history keeps their log. A `pid`, `ci` or `pr` watch has
    /// no output: its status is printed instead.
    Log {
        id: String,
        /// Print only the last N lines (like `tail -n`).
        #[arg(short = 'n', long = "lines", value_name = "N")]
        lines: Option<usize>,
        /// Keep printing new output until the job exits.
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Stop watching a job. A `job run` command that is still running is sent
    /// SIGTERM (its whole process group).
    Cancel { id: String },
}

pub fn run(project_opt: Option<String>, cmd: JobCmd) -> Result<()> {
    let project = require_registered_project(project_opt)?;
    match cmd {
        JobCmd::Run { label, cmd } => {
            let job = runner::spawn_command_job(&project, &cmd, label).map_err(|e| anyhow!(e))?;
            let log = awareness::job_output_path(&project, &job.id).map_err(|e| anyhow!(e))?;
            println!("✓ job {} started: {}", job.id, job.display_label());
            println!("  output: {}", log.display());
            Ok(())
        }
        JobCmd::Watch { pid, ci, pr, label } => {
            let (kind, target) = match (pid, ci, pr) {
                (Some(pid), _, _) => {
                    if !runner::pid_alive(pid) {
                        bail!("no running process {pid}");
                    }
                    (JobKind::Pid, pid.to_string())
                }
                (_, Some(sha), _) => (JobKind::Ci, resolve_commit(&sha)?),
                (_, _, Some(pr)) => (JobKind::Pr, pr.to_string()),
                _ => bail!("pass one of --pid, --ci or --pr"),
            };
            let job = awareness::add_job(&project, NewJob { kind, target, label })
                .map_err(|e| anyhow!(e))?;
            println!("✓ job {} watching {}", job.id, job.display_label());
            Ok(())
        }
        JobCmd::List { all, json } => {
            let mut jobs = awareness::read_awareness(&project).map_err(|e| anyhow!(e))?.jobs;
            newest_started_first(&mut jobs);
            let history = if all {
                awareness::read_job_history(&project).map_err(|e| anyhow!(e))?
            } else {
                Vec::new()
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&list_json(&jobs, &history)?)?);
            } else if all {
                print!("{}", render_all(&jobs, &history, Utc::now()));
            } else {
                print!("{}", render_list(&jobs));
            }
            Ok(())
        }
        JobCmd::Log { id, lines, follow } => log(&project, &id, lines, follow),
        JobCmd::Cancel { id } => match runner::cancel_job(&project, &id).map_err(|e| anyhow!(e))? {
            Some(job) => {
                println!("✓ canceled job {}: {}", job.id, job.display_label());
                Ok(())
            }
            None => bail!("no job `{id}` (see `shelbi job list`)"),
        },
    }
}

/// `shelbi __job-exec <id> -- <cmd...>`. Only `job run` starts it, after
/// checking the project is registered, so it skips that check: a failure here
/// would leave the job without a result. The store's writers still refuse a
/// project with no state dir.
pub fn exec(project_opt: Option<String>, id: String, cmd: Vec<String>) -> Result<()> {
    let project = super::require_project(project_opt)?;
    runner::run_job_exec(&project, &id, &cmd).map_err(|e| anyhow!(e))
}

/// The full sha of `rev` in the current repository; a 40-hex sha is taken as
/// is when there is no repository to ask (GitHub needs the full object id).
fn resolve_commit(rev: &str) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])
        .output();
    if let Ok(out) = out {
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                return Ok(sha);
            }
        }
    }
    if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(rev.to_ascii_lowercase());
    }
    bail!("can't resolve `{rev}` to a commit here; pass the full 40-character sha")
}

/// Running jobs newest first, as `--all` lists them (ids break a tie).
fn newest_started_first(jobs: &mut [Job]) {
    jobs.sort_by(|a, b| {
        b.started_at.cmp(&a.started_at).then_with(|| {
            awareness::job_id_number(&b.id).cmp(&awareness::job_id_number(&a.id))
        })
    });
}

/// The `--json` list: each running job with `"status": "running"`, then
/// (with `--all`) each finished record with its `duration_secs`.
fn list_json(jobs: &[Job], history: &[JobRecord]) -> Result<serde_json::Value> {
    let mut out = Vec::new();
    for job in jobs {
        let mut v = serde_json::to_value(job)?;
        v["status"] = "running".into();
        out.push(v);
    }
    for record in history {
        let mut v = serde_json::to_value(record)?;
        v["duration_secs"] = record.duration().as_secs().into();
        out.push(v);
    }
    Ok(serde_json::Value::Array(out))
}

/// A duration as the listings show it, whole seconds (`2m13s`).
fn duration_text(d: Duration) -> String {
    shelbi_core::format_duration(Duration::from_secs(d.as_secs()))
}

/// `job list --all`: running jobs, then finished ones, newest first.
fn render_all(jobs: &[Job], history: &[JobRecord], now: DateTime<Utc>) -> String {
    if jobs.is_empty() && history.is_empty() {
        return "no jobs\n".into();
    }
    let row = |id: &str, kind: JobKind, status: &str, exit: &str, took: &str, when: &str, label: &str| {
        format!("{id:<5} {kind:<8} {status:<9} {exit:<8} {took:<9} {when:<22} {label}\n", kind = kind.as_str())
    };
    let mut out = String::new();
    for job in jobs {
        let took = duration_text((now - job.started_at).to_std().unwrap_or_default());
        let when = format!("started {}", super::remind::relative(job.started_at, now));
        out.push_str(&row(&job.id, job.kind, "running", "", &took, &when, &job.display_label()));
    }
    for r in history {
        let exit = r.exit_code.map(|c| format!("exit {c}")).unwrap_or_default();
        let when = format!("finished {}", super::remind::relative(r.finished_at, now));
        let took = duration_text(r.duration());
        out.push_str(&row(&r.id, r.kind, r.status.as_str(), &exit, &took, &when, &r.display_label()));
        if let Some(log) = &r.log {
            out.push_str(&format!("      log: {}\n", log.display()));
        }
    }
    out
}

/// What `job log` found for an id.
enum Found {
    Running(Job),
    Finished(JobRecord),
    /// Only a job directory, from before the history existed.
    DirOnly,
}

/// `shelbi job log <id> [-n N] [-f]`. A closed stdout (`| head`) ends it
/// quietly.
fn log(project: &str, id: &str, lines: Option<usize>, follow: bool) -> Result<()> {
    match print_log(project, id, lines, follow) {
        Err(e)
            if e
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            Ok(())
        }
        r => r,
    }
}

fn print_log(project: &str, id: &str, lines: Option<usize>, follow: bool) -> Result<()> {
    let store = awareness::read_awareness(project).map_err(|e| anyhow!(e))?;
    let path = awareness::job_output_path(project, id).map_err(|e| anyhow!(e))?;
    let found = if let Some(job) = store.jobs.into_iter().find(|j| j.id == id) {
        Found::Running(job)
    } else if let Some(record) = awareness::find_job_record(project, id).map_err(|e| anyhow!(e))? {
        Found::Finished(record)
    } else if path.exists() {
        Found::DirOnly
    } else {
        bail!("no job `{id}` (see `shelbi job list --all`; finished jobs are kept for the newest 200 within 30 days)");
    };
    let now = Utc::now();
    let mut stdout = std::io::stdout().lock();
    let kind = match &found {
        Found::Running(j) => j.kind,
        Found::Finished(r) => r.kind,
        Found::DirOnly => JobKind::Command,
    };
    if kind != JobKind::Command {
        let text = match &found {
            Found::Running(job) => watch_status_running(job, now),
            Found::Finished(record) => watch_status_finished(record, now),
            Found::DirOnly => unreachable!("a directory-only job is a command"),
        };
        stdout.write_all(text.as_bytes())?;
        return Ok(());
    }
    if !path.exists() && !matches!(found, Found::Running(_)) {
        bail!("job {id}'s output is no longer kept ({} is gone)", path.display());
    }
    let start = match (lines, path.exists()) {
        (Some(n), true) => awareness::last_lines_offset(&path, n)?,
        _ => 0,
    };
    let pos = copy_from(&path, start, &mut stdout)?;
    let Found::Running(job) = found else {
        return Ok(());
    };
    if !follow {
        return Ok(());
    }
    follow_log(&path, pos, &mut stdout, || job_ended(project, &job), FOLLOW_POLL)?;
    if let Ok(Some(result)) = awareness::read_job_result(project, id) {
        let status = if result.succeeded() { "ok" } else { "failed" };
        let exit = result
            .exit_code
            .or(result.signal.map(|s| 128 + s))
            .map(|c| format!(", exit {c}"))
            .unwrap_or_default();
        eprintln!("job {id} finished: {status}{exit}");
    } else {
        eprintln!("job {id} is no longer running");
    }
    Ok(())
}

/// Whether a running command job has ended: its result is written, it left
/// the store (canceled or reported), or its runner is gone.
fn job_ended(project: &str, job: &Job) -> bool {
    if matches!(awareness::read_job_result(project, &job.id), Ok(Some(_))) {
        return true;
    }
    let listed = awareness::read_awareness(project)
        .map(|s| s.jobs.iter().any(|j| j.id == job.id))
        .unwrap_or(true);
    if !listed {
        return true;
    }
    job.runner_pid.is_some_and(|pid| !runner::pid_alive(pid))
}

/// Copy `path` from byte `start` to `out`; returns the offset copied up to.
/// A missing file copies nothing.
fn copy_from(path: &Path, start: u64, out: &mut impl Write) -> std::io::Result<u64> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(start),
        Err(e) => return Err(e),
    };
    file.seek(SeekFrom::Start(start))?;
    let copied = std::io::copy(&mut file, out)?;
    out.flush()?;
    Ok(start + copied)
}

/// Stream what is appended to `path` after `pos` until `ended` says the job
/// is over, then copy what it wrote last. Returns the final offset.
fn follow_log(
    path: &Path,
    mut pos: u64,
    out: &mut impl Write,
    mut ended: impl FnMut() -> bool,
    poll: Duration,
) -> std::io::Result<u64> {
    loop {
        // Check before copying, so the last copy sees every byte written
        // before the end was noticed.
        let done = ended();
        pos = copy_from(path, pos, out)?;
        if done {
            return Ok(pos);
        }
        std::thread::sleep(poll);
    }
}

/// What a running watch prints in place of output.
fn watch_status_running(job: &Job, now: DateTime<Utc>) -> String {
    format!(
        "job {} is a {} watch on {}, started {} and still waiting. A watch has no command output.\n",
        job.id,
        job.kind,
        job.display_label(),
        super::remind::relative(job.started_at, now),
    )
}

/// What a finished watch prints in place of output.
fn watch_status_finished(record: &JobRecord, now: DateTime<Utc>) -> String {
    let mut out = format!(
        "job {} was a {} watch on {}: {} {} (after {}). A watch has no command output.\n",
        record.id,
        record.kind,
        record.display_label(),
        record.status,
        super::remind::relative(record.finished_at, now),
        duration_text(record.duration()),
    );
    if let Some(detail) = &record.detail {
        out.push_str(&format!("{detail}\n"));
    }
    out
}

fn render_list(jobs: &[Job]) -> String {
    if jobs.is_empty() {
        return "no jobs\n".into();
    }
    let now = Utc::now();
    let mut out = String::new();
    for job in jobs {
        out.push_str(&format!(
            "{:<5} {:<8} started {:<9} {}\n",
            job.id,
            job.kind.as_str(),
            super::remind::relative(job.started_at, now),
            job.display_label(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_state::awareness::JobStatus;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn job(id: &str, kind: JobKind, target: &str, started_at: DateTime<Utc>) -> Job {
        Job { id: id.into(), kind, target: target.into(), label: None, started_at, runner_pid: None }
    }

    #[test]
    fn all_lists_running_then_finished_with_status_exit_duration_and_log() {
        let now = Utc::now();
        let ago = |s: i64| now - chrono::Duration::seconds(s);
        let running = vec![job("j9", JobKind::Command, "cargo test", ago(125))];
        let failed = JobRecord {
            exit_code: Some(3),
            log: Some("/h/jobs/j7/output.log".into()),
            ..JobRecord::finished(&job("j7", JobKind::Command, "cargo build", ago(3_733)), JobStatus::Failed, ago(3_600))
        };
        let merged = JobRecord::finished(&job("j6", JobKind::Pr, "2265", ago(9_000)), JobStatus::Merged, ago(7_200));
        let text = render_all(&running, &[failed, merged], now);
        println!("{text}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].starts_with("j9    command  running") && lines[0].contains("2m5s") && lines[0].contains("started 2m ago"), "{text}");
        assert!(lines[1].starts_with("j7    command  failed    exit 3   2m13s") && lines[1].contains("finished 1h ago"), "{text}");
        assert_eq!(lines[2], "      log: /h/jobs/j7/output.log");
        assert!(lines[3].starts_with("j6    pr       merged") && lines[3].ends_with("PR #2265"), "{text}");
        assert_eq!(render_all(&[], &[], now), "no jobs\n");
    }

    #[test]
    fn follow_copies_everything_written_before_the_end() {
        let dir = std::env::temp_dir().join(format!("shelbi-job-follow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("output.log");
        std::fs::write(&path, "a\n").unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let (path, done) = (path.clone(), done.clone());
            std::thread::spawn(move || {
                for line in ["b\n", "c\n"] {
                    std::thread::sleep(Duration::from_millis(30));
                    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
                    f.write_all(line.as_bytes()).unwrap();
                }
                done.store(true, Ordering::SeqCst);
            })
        };
        let mut out = Vec::new();
        let start = copy_from(&path, 0, &mut out).unwrap();
        let end = follow_log(&path, start, &mut out, || done.load(Ordering::SeqCst), Duration::from_millis(5)).unwrap();
        writer.join().unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a\nb\nc\n");
        assert_eq!(end, 6);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
