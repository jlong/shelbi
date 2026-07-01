//! Team-contributor heuristic that prefills the interactive mode picker
//! in `shelbi init`.
//!
//! The recommendation is a *suggestion*, not a default — the user still
//! confirms it in the prompt. But it's the difference between the wizard
//! landing on "in-repo" for a shared team repo (right) versus asking a
//! solo developer to pick a mode they don't have context for (wrong).
//!
//! Kept as a small pure function ([`recommend_mode`]) that takes canned
//! signals so it's straightforward to unit-test. The IO-touching
//! [`probe_team_signals`] wrapper is thin — a couple of `git` invocations
//! and a line-count — with no fallbacks that hide errors, so a
//! non-repo cwd cleanly falls into the "no remote / no history" branch
//! and lands on `global`.

use std::path::Path;
use std::process::Command;

use super::InitMode;

/// Canned inputs to [`recommend_mode`]. Split from the probe so tests
/// can construct arbitrary combinations without shelling out to git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamSignals {
    /// True when `git config --get remote.origin.url` resolves — i.e.
    /// the repo has a remote origin. Missing remote → probably a scratch
    /// dir or an unpublished repo; probably solo.
    pub has_remote_origin: bool,
    /// Distinct committer emails seen by `git log --format=%ae | sort -u`.
    /// A count ≥ 2 is our signal that more than one person has touched
    /// the repo (the team-contributor threshold).
    pub distinct_committer_emails: usize,
}

/// Pure recommender. Both signals must hold for `InRepo`: multi-emailed
/// AND has-remote-origin. Either missing → `Global`.
pub fn recommend_mode(signals: &TeamSignals) -> InitMode {
    if signals.has_remote_origin && signals.distinct_committer_emails >= 2 {
        InitMode::InRepo
    } else {
        InitMode::Global
    }
}

/// Probe the two signals from a real git checkout. Any git failure
/// (missing binary, non-repo, empty history) resolves to the defaults
/// that flip the heuristic to `Global` — the safe fallback for a
/// solo-developer setup.
pub fn probe_team_signals(cwd: &Path) -> TeamSignals {
    TeamSignals {
        has_remote_origin: has_remote_origin(cwd),
        distinct_committer_emails: distinct_committer_emails(cwd),
    }
}

fn has_remote_origin(cwd: &Path) -> bool {
    Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .map(|o| {
            o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty()
        })
        .unwrap_or(false)
}

fn distinct_committer_emails(cwd: &Path) -> usize {
    let out = match Command::new("git")
        .args(["log", "--format=%ae"])
        .current_dir(cwd)
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return 0,
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut seen = std::collections::HashSet::new();
    for line in stdout.lines() {
        let email = line.trim();
        if !email.is_empty() {
            seen.insert(email.to_string());
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommend_in_repo_when_both_signals_present() {
        let s = TeamSignals {
            has_remote_origin: true,
            distinct_committer_emails: 4,
        };
        assert_eq!(recommend_mode(&s), InitMode::InRepo);
    }

    #[test]
    fn recommend_global_when_only_one_email() {
        let s = TeamSignals {
            has_remote_origin: true,
            distinct_committer_emails: 1,
        };
        assert_eq!(recommend_mode(&s), InitMode::Global);
    }

    #[test]
    fn recommend_global_when_no_remote_even_with_many_emails() {
        // Solo dev with pushed to nobody but with many committer emails
        // (unusual but possible — e.g. a repo cloned & imported into a
        // local staging tree) still falls to Global because the remote
        // is the "shared" signal.
        let s = TeamSignals {
            has_remote_origin: false,
            distinct_committer_emails: 5,
        };
        assert_eq!(recommend_mode(&s), InitMode::Global);
    }

    #[test]
    fn recommend_global_when_no_history_yet() {
        // Fresh `git init` — no commits, no emails. Solo path.
        let s = TeamSignals {
            has_remote_origin: false,
            distinct_committer_emails: 0,
        };
        assert_eq!(recommend_mode(&s), InitMode::Global);
    }

    #[test]
    fn recommend_matches_boundary_exactly_at_two() {
        // Exactly the documented threshold — two distinct emails.
        let s = TeamSignals {
            has_remote_origin: true,
            distinct_committer_emails: 2,
        };
        assert_eq!(recommend_mode(&s), InitMode::InRepo);
    }

    #[test]
    fn probe_survives_non_git_directory() {
        // A tmp dir that isn't a git repo cleanly returns the defaults
        // that make the heuristic prefer Global.
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-heuristic-non-git-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let signals = probe_team_signals(&tmp);
        assert!(!signals.has_remote_origin);
        assert_eq!(signals.distinct_committer_emails, 0);
        assert_eq!(recommend_mode(&signals), InitMode::Global);
    }
}
