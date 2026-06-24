//! Tear down every tmux session belonging to a project, then switch the
//! attached client to whatever shelbi project session is next-most-recent.
//!
//! Invoked from the palette's "Quit Project" entry. Replaces the older
//! plain-`kill-session` flow, which left remote worker sessions orphaned
//! (they live on each worker's machine — the local `session-closed` hook
//! only catches the local stash) and dropped the user wherever tmux
//! happened to switch by default.

use anyhow::{anyhow, Result};

use shelbi_orchestrator::worker as orch_worker;

/// Quit `project`:
///
/// 1. Kill every worker pane (local windows + remote sessions). The user
///    is closing the whole project, so we don't try to preserve in-flight
///    task assignments here — the cards stay on the board and get picked
///    up the next time the project's dispatched.
/// 2. Pick the most-recently-attached *other* `shelbi-*` session.
/// 3. `switch-client` to it BEFORE killing the current session — without
///    this the popup's tmux client briefly disconnects, which can flash
///    a bare terminal at the user.
/// 4. Kill the hidden stash session (`_shelbi-<project>`) and then the
///    visible session (`shelbi-<project>`). Both are idempotent and
///    cleared by the local `session-closed` hook anyway, but doing the
///    work explicitly keeps the teardown order deterministic.
/// 5. Append a `project=<name> closed reason=user:quit-project` line to
///    the events log so the activity feed shows the close.
pub fn run(project: &str) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;

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

    let current = format!("shelbi-{project}");
    if let Some(target) = pick_next_session(&list_sessions(), &current) {
        let _ = run_tmux(["switch-client", "-t", &target]);
    }

    let _ = run_tmux(["kill-session", "-t", &format!("_shelbi-{project}")]);
    let _ = run_tmux(["kill-session", "-t", &current]);

    let _ = shelbi_state::append_project_event(project, "closed", "user:quit-project");

    Ok(())
}

/// `tmux list-sessions` output, one line per session, formatted
/// `<name> <last_attached>`. `last_attached` is unix seconds; 0 if the
/// session has never been attached. Returns an empty string if tmux
/// isn't reachable — callers treat that as "no other sessions".
fn list_sessions() -> String {
    std::process::Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name} #{session_last_attached}",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// From `tmux list-sessions` output, pick the most-recently-attached
/// session whose name starts with `shelbi-` and isn't `current`. The
/// `_shelbi-*` stash sessions are excluded automatically — their prefix
/// is `_shelbi-`, not `shelbi-`.
fn pick_next_session(listing: &str, current: &str) -> Option<String> {
    let mut best: Option<(String, u64)> = None;
    for line in listing.lines() {
        let mut parts = line.splitn(2, ' ');
        let name = parts.next().unwrap_or("").trim();
        let ts = parts.next().unwrap_or("").trim().parse::<u64>().unwrap_or(0);
        if name.is_empty() || name == current || !name.starts_with("shelbi-") {
            continue;
        }
        match &best {
            Some((_, best_ts)) if *best_ts >= ts => {}
            _ => best = Some((name.to_string(), ts)),
        }
    }
    best.map(|(name, _)| name)
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
    fn picks_most_recently_attached_other_shelbi_session() {
        let listing = "\
shelbi-alpha 1000
shelbi-bravo 3000
shelbi-charlie 2000
";
        assert_eq!(
            pick_next_session(listing, "shelbi-alpha").as_deref(),
            Some("shelbi-bravo")
        );
    }

    #[test]
    fn skips_current_and_stash_and_non_shelbi() {
        let listing = "\
shelbi-alpha 5000
_shelbi-alpha 9999
_shelbi-bravo 9999
plain-session 9999
shelbi-bravo 4000
";
        // Current is alpha. Bravo wins; both _shelbi stashes and plain
        // are excluded (stash by prefix, plain by missing shelbi- prefix).
        assert_eq!(
            pick_next_session(listing, "shelbi-alpha").as_deref(),
            Some("shelbi-bravo")
        );
    }

    #[test]
    fn returns_none_when_only_current_exists() {
        let listing = "shelbi-alpha 1000\n";
        assert!(pick_next_session(listing, "shelbi-alpha").is_none());
    }

    #[test]
    fn returns_none_when_listing_is_empty() {
        assert!(pick_next_session("", "shelbi-alpha").is_none());
    }

    #[test]
    fn never_attached_session_with_zero_timestamp_is_eligible() {
        // A freshly-bootstrapped session has last_attached=0 and should
        // still be a valid landing target — better than detaching the
        // client outright.
        let listing = "\
shelbi-alpha 1000
shelbi-bravo 0
";
        assert_eq!(
            pick_next_session(listing, "shelbi-alpha").as_deref(),
            Some("shelbi-bravo")
        );
    }

    #[test]
    fn tolerates_malformed_lines() {
        let listing = "\
shelbi-alpha not-a-number
shelbi-bravo 500
\n\
shelbi-charlie
";
        // alpha parses to ts=0; bravo wins with ts=500; charlie's
        // missing field parses to 0.
        assert_eq!(
            pick_next_session(listing, "shelbi-alpha").as_deref(),
            Some("shelbi-bravo")
        );
    }
}
