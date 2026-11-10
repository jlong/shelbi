//! `shelbi job` — watched jobs in the awareness store
//! (`shelbi_state::awareness`). `job run` starts a command detached and returns
//! at once; `job watch` follows a pid, a commit's CI, or a PR. The hub poller
//! writes one `job` line to `events.log` when each finishes, so the
//! orchestrator is told instead of having to poll.
//!
//! `__job-exec` is the hidden runner `job run` starts in its own session.

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use clap::Subcommand;
use shelbi_orchestrator::awareness as runner;
use shelbi_state::awareness::{self, Job, JobKind, NewJob};

use super::require_project;

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
    /// List the jobs still being watched.
    List {
        /// Print them as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop watching a job. A `job run` command that is still running is sent
    /// SIGTERM (its whole process group).
    Cancel { id: String },
}

pub fn run(project_opt: Option<String>, cmd: JobCmd) -> Result<()> {
    let project = require_project(project_opt)?;
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
        JobCmd::List { json } => {
            let jobs = awareness::read_awareness(&project).map_err(|e| anyhow!(e))?.jobs;
            if json {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
            } else {
                print!("{}", render_list(&jobs));
            }
            Ok(())
        }
        JobCmd::Cancel { id } => match runner::cancel_job(&project, &id).map_err(|e| anyhow!(e))? {
            Some(job) => {
                println!("✓ canceled job {}: {}", job.id, job.display_label());
                Ok(())
            }
            None => bail!("no job `{id}` (see `shelbi job list`)"),
        },
    }
}

/// `shelbi __job-exec <id> -- <cmd...>`.
pub fn exec(project_opt: Option<String>, id: String, cmd: Vec<String>) -> Result<()> {
    let project = require_project(project_opt)?;
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
