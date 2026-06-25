//! Tear down every Shelbi-owned tmux session on the host — every
//! `shelbi-<project>` and every `_shelbi-<project>` stash, plus the
//! per-worker panes (local windows + remote sessions). Detaches the
//! attached client at the end so the user's terminal lands back on a
//! shell rather than getting orphaned.
//!
//! Invoked from the palette's "Quit Shelbi" entry after the
//! confirmation popover. Sibling to `quit_project` — that closes one
//! project, this closes all of them.
//!
//! Like `quit_project`, this writes no task or worktree state; the
//! agents on disk are already authoritative, and any in-flight worker
//! changes stay in their worktrees for the user to pick up later.

use anyhow::Result;

use shelbi_orchestrator::worker as orch_worker;

/// One project the host is currently managing — has a live
/// `shelbi-<name>` tmux session. Used by the confirmation popover so
/// the user can see exactly what's about to be torn down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedProject {
    pub name: String,
    /// Workers whose tmux pane is currently live. Zero is rendered as
    /// "no active workers" in the popover; the project still appears
    /// because the main session is open.
    pub active_workers: usize,
}

/// Enumerate every project with a live `shelbi-<name>` session,
/// decorated with a count of its currently-live worker panes.
/// Sorted alphabetically so the popover order is stable across runs.
///
/// Best-effort: a project whose YAML fails to load shows up with
/// `active_workers = 0` rather than being dropped — the session is
/// still real and still needs to be killed.
pub fn list_managed_projects() -> Vec<ManagedProject> {
    let listing = list_sessions_listing();
    let mut out: Vec<ManagedProject> = shelbi_project_session_names(&listing)
        .map(|name| {
            let active_workers = shelbi_state::load_project(&name)
                .map(|p| count_active_workers(&p))
                .unwrap_or(0);
            ManagedProject {
                name,
                active_workers,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn count_active_workers(project: &shelbi_core::Project) -> usize {
    let mut count = 0;
    for worker in &project.workers {
        let Some(machine) = project.machine(&worker.machine) else {
            continue;
        };
        let host = machine.host();
        let Ok(addr) = orch_worker::worker_tmux_addr(project, worker) else {
            continue;
        };
        if orch_worker::worker_pane_alive(&host, &addr).unwrap_or(false) {
            count += 1;
        }
    }
    count
}

/// Tear down everything. For each managed project, kill its worker
/// panes (local windows + remote sessions), clear its zen crash
/// heartbeat so the next start doesn't misread the explicit shutdown
/// as a crash, and append a `closed reason=user:quit-shelbi` event so
/// the activity feed shows the close. Then kill every
/// `_shelbi-<name>` stash session, detach the attached tmux client
/// (otherwise the client can flash to the popup's host pane between
/// the session dying and the client noticing), and finally kill every
/// `shelbi-<name>` session.
///
/// Idempotent throughout — `kill-session` on an absent target is a
/// no-op, and best-effort tmux/SSH errors don't abort the rest of
/// the teardown.
pub fn run() -> Result<()> {
    let listing = list_sessions_listing();
    let names: Vec<String> = shelbi_project_session_names(&listing).collect();

    for name in &names {
        let _ = shelbi_state::zen_clear_crash(name);
        if let Ok(p) = shelbi_state::load_project(name) {
            for worker in &p.workers {
                let Some(machine) = p.machine(&worker.machine) else {
                    continue;
                };
                let host = machine.host();
                let Ok(addr) = orch_worker::worker_tmux_addr(&p, worker) else {
                    continue;
                };
                let _ = orch_worker::kill_worker_pane(&host, &addr);
            }
        }
        let _ = shelbi_state::append_project_event(name, "closed", "user:quit-shelbi");
    }

    for name in &names {
        let _ = run_tmux(["kill-session", "-t", &format!("_shelbi-{name}")]);
    }

    let _ = run_tmux(["detach-client"]);

    for name in &names {
        let _ = run_tmux(["kill-session", "-t", &format!("shelbi-{name}")]);
    }

    Ok(())
}

/// `tmux list-sessions` output, one session per line. Empty string
/// if tmux isn't reachable — callers then treat that as "nothing to
/// quit" and the run is a no-op.
fn list_sessions_listing() -> String {
    std::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Extract project names from session names of the form
/// `shelbi-<name>`. The hidden `_shelbi-<name>` stash sessions are
/// skipped automatically — their prefix is `_shelbi-`, not `shelbi-`.
fn shelbi_project_session_names(listing: &str) -> impl Iterator<Item = String> + '_ {
    listing.lines().filter_map(|line| {
        let name = line.trim();
        let rest = name.strip_prefix("shelbi-")?;
        if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        }
    })
}

fn run_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new("tmux")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_strip_shelbi_prefix() {
        let listing = "\
shelbi-alpha
shelbi-bravo
";
        let names: Vec<_> = shelbi_project_session_names(listing).collect();
        assert_eq!(names, vec!["alpha", "bravo"]);
    }

    #[test]
    fn project_names_skip_stash_and_non_shelbi_sessions() {
        let listing = "\
shelbi-alpha
_shelbi-alpha
_shelbi-bravo
plain-session
shelbi-bravo
";
        let names: Vec<_> = shelbi_project_session_names(listing).collect();
        assert_eq!(names, vec!["alpha", "bravo"]);
    }

    #[test]
    fn project_names_tolerate_blank_lines_and_whitespace() {
        let listing = "\
shelbi-alpha

   shelbi-bravo
shelbi-
";
        // The "shelbi-" line with no project name is dropped — strip
        // returns an empty string and the filter excludes it.
        let names: Vec<_> = shelbi_project_session_names(listing).collect();
        assert_eq!(names, vec!["alpha", "bravo"]);
    }

    #[test]
    fn project_names_returns_nothing_for_empty_listing() {
        let names: Vec<_> = shelbi_project_session_names("").collect();
        assert!(names.is_empty());
    }
}
