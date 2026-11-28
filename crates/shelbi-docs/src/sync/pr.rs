//! The rolling pull request a `folder`-mode docs' edits ride in.
//!
//! Edits to a repo folder (`docs/`) are committed to the rolling
//! `shelbi/wiki-docs` branch and pushed; this keeps exactly one open PR from
//! that branch into the base branch. While one is open a push updates it;
//! once it is merged (or closed) the next push with changes opens a new one.

use std::path::Path;
use std::time::Duration;

/// An open pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
}

/// Opens or finds the rolling PR. A trait so tests drive the sync without a
/// GitHub.
pub trait PullRequests: Send + Sync {
    /// The open PR from `head` into `base` in the repo checked out at
    /// `worktree`, opening one when there is none.
    fn ensure_open(&self, worktree: &Path, head: &str, base: &str) -> Result<PullRequest, String>;
}

/// The title the rolling PR opens with.
pub const PR_TITLE: &str = "Documentation edits";

/// The body the rolling PR opens with.
pub const PR_BODY: &str = "Edits to the project's docs made in Shelbi's docs. Shelbi keeps this pull request up to date \
     as the docs change; merge it to land them. The next edit after it merges opens a new one.";

/// [`PullRequests`] through the `gh` CLI.
#[derive(Debug, Default, Clone)]
pub struct GhPullRequests;

const GH_DEADLINE: Duration = Duration::from_secs(45);

impl PullRequests for GhPullRequests {
    fn ensure_open(&self, worktree: &Path, head: &str, base: &str) -> Result<PullRequest, String> {
        let listed = gh(
            worktree,
            &["pr", "list", "--head", head, "--state", "open", "--json", "number,url", "--limit", "1"],
        )?;
        if let Some(pr) = parse_list(&listed) {
            return Ok(pr);
        }
        let created = gh(
            worktree,
            &["pr", "create", "--head", head, "--base", base, "--title", PR_TITLE, "--body", PR_BODY],
        )?;
        // `gh pr create` prints the new PR's URL; its number ends it.
        let url = created
            .lines()
            .map(str::trim)
            .rfind(|l| l.starts_with("http"))
            .unwrap_or("")
            .to_string();
        let number = url.rsplit('/').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        Ok(PullRequest { number, url })
    }
}

fn parse_list(json: &str) -> Option<PullRequest> {
    #[derive(serde::Deserialize)]
    struct Row {
        number: u64,
        url: String,
    }
    let rows: Vec<Row> = serde_json::from_str(json.trim()).ok()?;
    rows.into_iter().next().map(|r| PullRequest {
        number: r.number,
        url: r.url,
    })
}

fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("gh")
        .args(args)
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run gh: {e}"))?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() > GH_DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("gh {} took longer than {}s", args[..2].join(" "), GH_DEADLINE.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut p) = child.stdout.take() {
        let _ = p.read_to_string(&mut out);
    }
    if let Some(mut p) = child.stderr.take() {
        let _ = p.read_to_string(&mut err);
    }
    if status.success() {
        Ok(out)
    } else {
        Err(format!("gh {} failed: {}", args[..2].join(" "), err.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_pr_is_read_from_the_list() {
        let pr = parse_list(r#"[{"number":12,"url":"https://github.com/o/r/pull/12"}]"#).unwrap();
        assert_eq!(pr.number, 12);
        assert!(parse_list("[]").is_none());
    }
}
