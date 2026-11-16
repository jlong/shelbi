//! Where a job reports: back to the worker that started it, or to the
//! orchestrator's feed.
//!
//! `shelbi job run` / `job watch` run inside a workspace's agent session record
//! that workspace (with the agent and task it serves) on the job
//! ([`JobStarter`], from the session's env: [`starter_from_env`]). When such a
//! job finishes, or runs past its estimate, the awareness tick asks whether that
//! session is still up and still serving the task ([`starter_live`]). If it is,
//! the report goes to the session as a one-line prompt ([`JobNotice::prompt`])
//! and the orchestrator's feed gets no `job` line for it; the Jobs screen and
//! the history still show the job. If it isn't (the session ended, or the
//! workspace now serves another task), or the job has no starter, the report is
//! the `job` line in `events.log` it always was.
//!
//! The prompt is delivered off the tick's thread ([`deliver_or_fall_back`]):
//! it takes the workspace's lock, as every send and dispatch does, re-checks
//! the session under it, and falls back to the feed when the session has moved
//! on or the prompt couldn't be shown as delivered, so a report is never lost.

use std::path::PathBuf;
use std::time::Duration;

use shelbi_core::{Host, Project, Result};
use shelbi_state::awareness::{Job, JobKind, JobStarter, JobStatus};

use crate::session_process_backend::{session_name, SessionProcessBackend};

/// How much of a finished command's last output line the prompt carries.
const PROMPT_DETAIL_BUDGET: usize = 200;

/// One job report, owned so it can be delivered on another thread and still
/// fall back to the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobNotice {
    /// The job finished.
    Finished {
        id: String,
        kind: JobKind,
        label: String,
        status: JobStatus,
        exit: Option<i32>,
        detail: Option<String>,
        log: Option<PathBuf>,
    },
    /// The job is still running past its estimate.
    Overdue {
        id: String,
        label: String,
        estimate: Duration,
        elapsed: Duration,
    },
}

impl JobNotice {
    /// The overdue report for running `job` at `elapsed`, or `None` when it has
    /// no estimate.
    pub fn overdue(job: &Job, elapsed: Duration) -> Option<Self> {
        Some(JobNotice::Overdue {
            id: job.id.clone(),
            label: job.display_label(),
            estimate: job.estimate?,
            elapsed,
        })
    }

    pub fn id(&self) -> &str {
        match self {
            JobNotice::Finished { id, .. } | JobNotice::Overdue { id, .. } => id,
        }
    }

    /// Write the report to the orchestrator's feed: the `job` line, or the job
    /// `overdue` line.
    pub fn append_to_feed(&self, project: &str) -> Result<()> {
        match self {
            JobNotice::Finished { id, kind, label, status, exit, detail, log } => {
                shelbi_state::append_job_event(
                    project,
                    &shelbi_state::JobEvent {
                        id,
                        kind: kind.as_str(),
                        label,
                        status: status.as_str(),
                        exit: *exit,
                        detail: detail.as_deref(),
                        log: log.as_deref(),
                    },
                )
            }
            JobNotice::Overdue { id, label, estimate, elapsed } => {
                shelbi_state::append_job_overdue_event(project, id, *estimate, *elapsed, label)
            }
        }
    }

    /// The one-line prompt a worker gets: the same facts as the feed's line,
    /// and the command to read more.
    ///
    /// ```text
    /// Shelbi job j3 "desktop tests" finished: failed, exit 3. Last line: "error: 2 tests failed". Read the output: shelbi job log j3 -n 50
    /// Shelbi job j3 "desktop tests" is past its estimate: running 45m, estimate 30m. Check it: shelbi job log j3 -n 20. Revise with shelbi job estimate j3 <duration>, or stop it with shelbi job cancel j3
    /// ```
    pub fn prompt(&self) -> String {
        match self {
            JobNotice::Finished { id, kind, label, status, exit, detail, .. } => {
                let mut out = format!("Shelbi job {id} {} finished: {status}", one_line(label, 120));
                if let Some(exit) = exit {
                    out.push_str(&format!(", exit {exit}"));
                }
                out.push('.');
                if let Some(detail) = detail.as_deref().filter(|d| !d.trim().is_empty()) {
                    let what = if *kind == JobKind::Command { "Last line" } else { "Detail" };
                    out.push_str(&format!(" {what}: {}.", one_line(detail, PROMPT_DETAIL_BUDGET)));
                }
                if *kind == JobKind::Command {
                    out.push_str(&format!(" Read the output: shelbi job log {id} -n 50"));
                } else {
                    out.push_str(&format!(" Details: shelbi job log {id}"));
                }
                out
            }
            JobNotice::Overdue { id, label, estimate, elapsed } => format!(
                "Shelbi job {id} {} is past its estimate: running {}, estimate {}. Check it: shelbi job \
                 log {id} -n 20. Revise with shelbi job estimate {id} <duration>, or stop it with shelbi \
                 job cancel {id}",
                one_line(label, 120),
                shelbi_core::format_duration(whole_minutes(*elapsed)),
                shelbi_core::format_duration(*estimate),
            ),
        }
    }
}

/// `text` quoted on one line (no control characters, `"` as `'`), cut to
/// `max` chars. The prompt is typed into the agent's input box, where a
/// newline would submit early.
fn one_line(text: &str, max: usize) -> String {
    shelbi_state::quote_event_text(text, max)
}

/// Whole minutes once there is one, as the overdue line shows elapsed time.
fn whole_minutes(d: Duration) -> Duration {
    if d.as_secs() >= 60 {
        Duration::from_secs(d.as_secs() / 60 * 60)
    } else {
        Duration::from_secs(d.as_secs())
    }
}

// ---------------------------------------------------------------------------
// Who started a job

/// The worker running this process, for a job it is starting in `project`:
/// `None` outside a workspace's agent session (the orchestrator, the user's
/// shell), or when the session belongs to another project.
pub fn starter_from_env(project: &str) -> Option<JobStarter> {
    starter_from(project, |key| std::env::var(key).ok(), |task| {
        let project = shelbi_state::load_project(project).ok()?;
        workspace_serving(&project, task, |name| SessionProcessBackend.live_session_task(name))
    })
}

/// [`starter_from_env`] over an env lookup and, for a session launched before
/// the workspace was in its env, a lookup of the workspace whose live session
/// serves a task.
fn starter_from(
    project: &str,
    env: impl Fn(&str) -> Option<String>,
    workspace_for_task: impl FnOnce(&str) -> Option<String>,
) -> Option<JobStarter> {
    let env = |key: &str| env(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    // A worker's session names its project (`PROJECT`); a job it starts for
    // another project has no worker of that project to report to.
    if let Some(own) = env("PROJECT").or_else(|| env("SHELBI_PROJECT")) {
        if own != project {
            return None;
        }
    }
    let task = env("TASK_ID");
    let workspace = match env(shelbi_state::WORKSPACE_ENV) {
        Some(ws) => ws,
        None => {
            // Sessions dispatched before `SHELBI_WORKSPACE` existed: only a
            // managed agent session serving a task can be a worker's.
            env("SHELBI_MANAGED_CONTEXT")?;
            workspace_for_task(task.as_deref()?)?
        }
    };
    Some(JobStarter { workspace, agent: env("SHELBI_AGENT"), task })
}

/// The local workspace of `project` whose live agent session serves `task`.
fn workspace_serving(
    project: &Project,
    task: &str,
    session_task: impl Fn(&str) -> Option<Option<String>>,
) -> Option<String> {
    project
        .workspaces
        .iter()
        .find(|ws| {
            local_session_name(project, &ws.name)
                .is_some_and(|name| session_task(&name).flatten().as_deref() == Some(task))
        })
        .map(|ws| ws.name.clone())
}

/// The session name of `workspace`'s agent, when it runs on this machine. A
/// remote workspace's job could not have been started here: its `shelbi`
/// writes the remote machine's own store.
fn local_session_name(project: &Project, workspace: &str) -> Option<String> {
    let spec = project.workspace(workspace)?;
    let machine = project.machine(&spec.machine)?;
    if machine.host() != Host::Local {
        return None;
    }
    let target = crate::workspace::workspace_target(project, spec).ok()?;
    Some(session_name(&target))
}

// ---------------------------------------------------------------------------
// Delivering a report

/// Whether `starter`'s session is up and still serves the task it started the
/// job for, so a report can go to it.
pub fn starter_live(project: &Project, starter: &JobStarter) -> bool {
    starter_live_with(project, starter, |name| SessionProcessBackend.live_session_task(name))
}

/// [`starter_live`] over a lookup of a live session's task (`None`: no live
/// session; `Some(None)`: one serving no task).
fn starter_live_with(
    project: &Project,
    starter: &JobStarter,
    session_task: impl Fn(&str) -> Option<Option<String>>,
) -> bool {
    let Some(name) = local_session_name(project, &starter.workspace) else {
        return false;
    };
    match (session_task(&name), starter.task.as_deref()) {
        (None, _) => false,
        // A session redispatched to another task (or now serving none) isn't the
        // one that asked.
        (Some(now), Some(task)) => now.as_deref() == Some(task),
        (Some(_), None) => true,
    }
}

/// Send `notice` to `starter`'s session as a prompt, or to the orchestrator's
/// feed when it can't land there: the session moved on since the tick looked,
/// or the send failed or couldn't be confirmed (the send itself records a
/// `stuck` line then). Blocking, for as long as a verified send takes; the
/// tick calls it on a thread of its own.
pub fn deliver_or_fall_back(project: &str, starter: &JobStarter, notice: &JobNotice) {
    let sent = crate::agent_send::send_to_workspace_if(project, &starter.workspace, &notice.prompt(), |p| {
        starter_live(p, starter)
    });
    let why = match sent {
        Ok(Some(_)) => return,
        Ok(None) => "the worker's session ended or moved on to another task".to_string(),
        Err(e) => e.to_string(),
    };
    tracing::info!(project, job = notice.id(), workspace = %starter.workspace, reason = %why, "job report not delivered to its worker; reporting to the orchestrator");
    if let Err(e) = notice.append_to_feed(project) {
        tracing::warn!(project, job = notice.id(), error = %e, "job report lost: neither the worker nor the feed took it");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        let mut p = Project::from_yaml_str(
            "repo: /tmp/p\nmachines:\n  - { name: local, kind: local, work_dir: /tmp/p }\n  - { name: far, kind: ssh, host: far.example, work_dir: /w }\n\
             workspaces:\n  - { name: alpha, machine: local }\n  - { name: bravo, machine: local }\n  - { name: remote, machine: far }\n\
             orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n",
        )
        .unwrap();
        p.name = "demo".into();
        p
    }

    fn starter(ws: &str, task: Option<&str>) -> JobStarter {
        JobStarter { workspace: ws.into(), agent: Some("developer".into()), task: task.map(str::to_string) }
    }

    /// Sessions by name: `alpha` serves task `t1`, `bravo` serves `t9`.
    fn sessions(name: &str) -> Option<Option<String>> {
        match name {
            "demo/ws/alpha" => Some(Some("t1".into())),
            "demo/ws/bravo" => Some(Some("t9".into())),
            _ => None,
        }
    }

    #[test]
    fn a_starter_is_live_only_while_its_session_serves_the_same_task() {
        let p = project();
        assert!(starter_live_with(&p, &starter("alpha", Some("t1")), sessions));
        // Redispatched to another task: not the session that asked.
        assert!(!starter_live_with(&p, &starter("bravo", Some("t1")), sessions));
        // Session gone.
        assert!(!starter_live_with(&p, &starter("alpha", Some("t1")), |_| None));
        // No task recorded: any live session of the workspace will do.
        assert!(starter_live_with(&p, &starter("bravo", None), sessions));
        // Unknown and remote workspaces never are.
        assert!(!starter_live_with(&p, &starter("ghost", None), sessions));
        assert!(!starter_live_with(&p, &starter("remote", None), |_| Some(None)));
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
    }

    #[test]
    fn the_starter_comes_from_the_worker_session_env() {
        let env = env_of(&[
            ("SHELBI_WORKSPACE", "alpha"),
            ("SHELBI_AGENT", "developer"),
            ("TASK_ID", "t1"),
            ("PROJECT", "demo"),
            ("SHELBI_MANAGED_CONTEXT", "1"),
        ]);
        assert_eq!(starter_from("demo", &env, |_| unreachable!()), Some(starter("alpha", Some("t1"))));
        // A job for another project has no worker there.
        assert_eq!(starter_from("other", &env, |_| unreachable!()), None);
        // The orchestrator's session, or the user's shell: no starter.
        let orch = env_of(&[("SHELBI_PROJECT", "demo"), ("SHELBI_MANAGED_CONTEXT", "1")]);
        assert_eq!(starter_from("demo", orch, |_| unreachable!()), None);
        assert_eq!(starter_from("demo", env_of(&[]), |_| unreachable!()), None);
    }

    #[test]
    fn a_session_from_before_the_workspace_env_is_found_by_its_task() {
        let env = env_of(&[("SHELBI_AGENT", "developer"), ("TASK_ID", "t9"), ("PROJECT", "demo"), ("SHELBI_MANAGED_CONTEXT", "1")]);
        let p = project();
        let found = starter_from("demo", &env, |task| workspace_serving(&p, task, sessions));
        assert_eq!(found, Some(starter("bravo", Some("t9"))));
        // Not a managed session: never guessed.
        let plain = env_of(&[("TASK_ID", "t9"), ("PROJECT", "demo")]);
        assert_eq!(starter_from("demo", plain, |task| workspace_serving(&p, task, sessions)), None);
    }

    #[test]
    fn the_prompt_is_one_line_with_the_result_and_how_to_read_more() {
        let finished = JobNotice::Finished {
            id: "j3".into(),
            kind: JobKind::Command,
            label: "desktop tests".into(),
            status: JobStatus::Failed,
            exit: Some(3),
            detail: Some("error: 2 tests \"failed\"\nmore".into()),
            log: Some("/h/jobs/j3/output.log".into()),
        };
        assert_eq!(
            finished.prompt(),
            "Shelbi job j3 \"desktop tests\" finished: failed, exit 3. Last line: \"error: 2 tests 'failed' more\". \
             Read the output: shelbi job log j3 -n 50"
        );
        let pr = JobNotice::Finished {
            id: "j4".into(),
            kind: JobKind::Pr,
            label: "PR #42".into(),
            status: JobStatus::Merged,
            exit: None,
            detail: None,
            log: None,
        };
        assert_eq!(pr.prompt(), "Shelbi job j4 \"PR #42\" finished: merged. Details: shelbi job log j4");
        let overdue = JobNotice::Overdue {
            id: "j3".into(),
            label: "desktop tests".into(),
            estimate: Duration::from_secs(30 * 60),
            elapsed: Duration::from_secs(45 * 60 + 12),
        };
        let text = overdue.prompt();
        assert!(text.starts_with("Shelbi job j3 \"desktop tests\" is past its estimate: running 45m, estimate 30m."), "{text}");
        assert!(text.contains("shelbi job estimate j3 <duration>") && text.contains("shelbi job cancel j3"), "{text}");
        for t in [finished.prompt(), pr.prompt(), text] {
            assert!(!t.contains('\n'), "{t}");
        }
    }
}
