use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use shelbi_core::{Host, Machine, MergeStrategy, Status, TmuxAddr};

use super::require_project;

/// How many lines of the workspace's tmux scrollback to embed in the PR body.
/// Chosen to comfortably cover the workspace's final report and the last few
/// commits' chatter while staying well under GitHub's ~65k char PR body cap.
const TRANSCRIPT_LINES: usize = 500;

pub fn run(project_opt: Option<String>, id: String, pr: bool) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let file = shelbi_state::load_agent(&project_name, &id).map_err(|e| anyhow!(e))?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let machine = project
        .machine(&file.agent.machine)
        .ok_or_else(|| anyhow!("machine `{}` no longer in project", file.agent.machine))?
        .clone();
    let host = machine.host();
    let branch = file.agent.branch.clone();
    let target = project.base_branch().to_string();
    let strategy = project.merge_strategy();

    if pr {
        return run_pr(
            &project_name,
            &file,
            &project,
            &machine,
            &host,
            &branch,
            &target,
            strategy,
            &id,
        );
    }

    preflight(&host, &machine, &target)?;
    capture_uncommitted(&host, &file.agent.worktree, &id)?;
    integrate_branch(&host, &machine, &branch, &target, strategy, &id)?;
    cleanup(&host, &machine, &file.agent.worktree, &branch, &file.agent.tmux);

    let mut updated = file.agent.clone();
    updated.status = Status::Done;
    updated.updated = Utc::now();
    shelbi_state::save_agent(&project_name, &updated, &file.body).map_err(|e| anyhow!(e))?;
    shelbi_state::append_log(&project_name, &id, "merge").map_err(|e| anyhow!(e))?;
    println!("✓ merged {id} into {target} ({strategy})");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_pr(
    project_name: &str,
    file: &shelbi_state::AgentFile,
    _project: &shelbi_core::Project,
    machine: &Machine,
    host: &Host,
    branch: &str,
    target: &str,
    _strategy: MergeStrategy,
    id: &str,
) -> Result<()> {
    // 1. Make sure `gh` is reachable on the workspace host.
    let gh_probe = shelbi_ssh::run(host, ["gh", "--version"]).map_err(|e| anyhow!(e))?;
    if !gh_probe.status.success() {
        bail!(
            "`gh` (GitHub CLI) not found on {} — install it (https://cli.github.com) and \
             re-run, or use plain `shelbi merge` for a local merge",
            machine.name
        );
    }

    // 2. Capture any uncommitted edits in the worktree.
    capture_uncommitted(host, &file.agent.worktree, id)?;

    // 3. Push the branch.
    let wt = file.agent.worktree.to_string_lossy().into_owned();
    shelbi_ssh::run_capture(host, ["git", "-C", &wt, "push", "-u", "origin", branch])
        .map_err(|e| anyhow!(e))?;

    // 4. Gather optional context — `git diff --stat` against the target and
    //    the tail of the workspace's tmux pane. Both are best-effort; if either
    //    fails we just omit that section rather than blocking the PR.
    let diff_stat = capture_diff_stat(host, &wt, target, branch);
    let transcript = capture_transcript(host, &file.agent.tmux);

    // 5. Open the PR. Title pulled from the task heading in the markdown body;
    //    body is the rest of the agent file (sans the H1) plus the diff stat
    //    and transcript sections.
    let (title, body) = derive_pr_text(
        &file.body,
        id,
        diff_stat.as_deref(),
        transcript.as_deref(),
    );
    let pr_url = shelbi_ssh::run_capture(
        host,
        [
            "gh", "-C", &wt, "pr", "create", "--base", target, "--head", branch, "--title",
            &title, "--body", &body,
        ],
    )
    .map_err(|e| anyhow!(e))?;
    let pr_url = pr_url.trim();

    // 6. Update state (still Running until merged in PR, but flag Waiting so
    //    Review view picks it up).
    let mut updated = file.agent.clone();
    updated.status = Status::Waiting;
    updated.updated = Utc::now();
    shelbi_state::save_agent(project_name, &updated, &file.body).map_err(|e| anyhow!(e))?;
    shelbi_state::append_log(project_name, id, &format!("pr opened: {pr_url}"))
        .map_err(|e| anyhow!(e))?;

    println!("✓ branch pushed and PR opened");
    println!("  {pr_url}");
    Ok(())
}

fn derive_pr_text(
    body_md: &str,
    id: &str,
    diff_stat: Option<&str>,
    transcript: Option<&str>,
) -> (String, String) {
    let mut lines = body_md.lines();
    // First "# Task" h1 is shelbi-emitted; the next non-blank line is the prompt.
    let mut title = format!("shelbi: {id}");
    for line in &mut lines {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        // First line of task prompt becomes the PR title (truncated).
        title = if t.len() > 70 { format!("{}…", &t[..69]) } else { t.to_string() };
        break;
    }

    let mut body = body_md.trim_end().to_string();
    body.push('\n');

    if let Some(stat) = diff_stat.map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str("\n## Files changed\n\n```\n");
        body.push_str(stat);
        if !stat.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("```\n");
    }

    if let Some(t) = transcript.map(str::trim_end).filter(|s| !s.is_empty()) {
        body.push_str("\n<details>\n<summary>Workspace transcript</summary>\n\n```\n");
        body.push_str(t);
        if !t.ends_with('\n') {
            body.push('\n');
        }
        body.push_str("```\n\n</details>\n");
    }

    body.push_str("\n— opened by [shelbi](https://github.com/jlong/shelbi)\n");
    (title, body)
}

/// `git diff --stat <target>...<branch>` against the just-pushed branch.
/// `...` (three dots) compares against the merge-base, so the summary
/// reflects what the PR actually adds, not unrelated drift on `target`.
fn capture_diff_stat(host: &Host, worktree: &str, target: &str, branch: &str) -> Option<String> {
    let range = format!("{target}...{branch}");
    let out = shelbi_ssh::run_capture(host, ["git", "-C", worktree, "diff", "--stat", &range])
        .ok()?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Tail of the workspace's tmux pane scrollback — the last things the agent
/// said before it handed off. Best-effort: if the pane is gone or capture
/// fails, the PR body just won't include this section.
fn capture_transcript(host: &Host, addr: &TmuxAddr) -> Option<String> {
    let raw = shelbi_tmux::capture_history(host, addr, TRANSCRIPT_LINES).ok()?;
    let trimmed = raw.trim_end();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn preflight(host: &Host, machine: &Machine, target: &str) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let current = shelbi_ssh::run_capture(
        host,
        ["git", "-C", &repo, "rev-parse", "--abbrev-ref", "HEAD"],
    )
    .map_err(|e| anyhow!(e))?;
    let current = current.trim();
    if current != target {
        bail!(
            "parent repo at {} is on branch `{current}`, not `{target}` — \
             checkout `{target}` and rerun, or use --pr when it lands in Phase 7",
            repo
        );
    }
    let dirty = shelbi_ssh::run_capture(host, ["git", "-C", &repo, "status", "--porcelain"])
        .map_err(|e| anyhow!(e))?;
    // `.shelbi/` is shelbi's own working space — ignore it from the dirty
    // check, even if the user hasn't added it to .gitignore yet.
    let user_dirty: Vec<&str> = dirty
        .lines()
        .filter(|l| {
            let path = l.get(3..).unwrap_or("");
            !(path.starts_with(".shelbi/") || path == ".shelbi" || path == ".gitignore")
        })
        .collect();
    if !user_dirty.is_empty() {
        bail!(
            "parent repo working tree is dirty — commit or stash first:\n{}",
            user_dirty.join("\n")
        );
    }
    Ok(())
}

fn capture_uncommitted(host: &Host, worktree: &std::path::Path, id: &str) -> Result<()> {
    let wt = worktree.to_string_lossy().into_owned();
    let dirty = shelbi_ssh::run_capture(host, ["git", "-C", &wt, "status", "--porcelain"])
        .map_err(|e| anyhow!(e))?;
    if dirty.trim().is_empty() {
        return Ok(());
    }
    shelbi_ssh::run_capture(host, ["git", "-C", &wt, "add", "-A"]).map_err(|e| anyhow!(e))?;
    shelbi_ssh::run_capture(
        host,
        [
            "git",
            "-C",
            &wt,
            "commit",
            "-m",
            &format!("shelbi: capture pending work from {id}"),
        ],
    )
    .map_err(|e| anyhow!(e))?;
    Ok(())
}

/// Integrate `branch` into `target` using `strategy`. Runs in `machine.work_dir`
/// — the parent repo checkout — which `preflight` has already verified is
/// clean and sitting on `target`.
fn integrate_branch(
    host: &Host,
    machine: &Machine,
    branch: &str,
    target: &str,
    strategy: MergeStrategy,
    id: &str,
) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    // Refuse if the branch has no commits beyond the target (nothing to merge).
    let ahead = shelbi_ssh::run_capture(
        host,
        [
            "git",
            "-C",
            &repo,
            "rev-list",
            "--count",
            &format!("{target}..{branch}"),
        ],
    )
    .map_err(|e| anyhow!(e))?;
    if ahead.trim() == "0" {
        bail!("branch `{branch}` has no commits beyond `{target}` — nothing to merge");
    }

    let summary = format!("shelbi: merge {id} from {branch}");
    match strategy {
        MergeStrategy::Squash => {
            shelbi_ssh::run_capture(host, ["git", "-C", &repo, "merge", "--squash", branch])
                .map_err(|e| anyhow!(e))?;
            shelbi_ssh::run_capture(host, ["git", "-C", &repo, "commit", "-m", &summary])
                .map_err(|e| anyhow!(e))?;
        }
        MergeStrategy::Merge => {
            // `--no-ff` so the merge commit is preserved even when the
            // branch is a fast-forward — matches gh's behavior.
            shelbi_ssh::run_capture(
                host,
                ["git", "-C", &repo, "merge", "--no-ff", "-m", &summary, branch],
            )
            .map_err(|e| anyhow!(e))?;
        }
        MergeStrategy::Rebase => {
            // `git rebase <target> <branch>` checks out the branch and
            // replays it onto target. Switch back to target and fast-
            // forward so the parent repo lands on the rebased tip with
            // `target` as the current branch.
            shelbi_ssh::run_capture(
                host,
                ["git", "-C", &repo, "rebase", target, branch],
            )
            .map_err(|e| anyhow!(e))?;
            shelbi_ssh::run_capture(host, ["git", "-C", &repo, "checkout", target])
                .map_err(|e| anyhow!(e))?;
            shelbi_ssh::run_capture(
                host,
                ["git", "-C", &repo, "merge", "--ff-only", branch],
            )
            .map_err(|e| anyhow!(e))?;
        }
    }
    Ok(())
}

fn cleanup(
    host: &Host,
    machine: &Machine,
    worktree: &std::path::Path,
    branch: &str,
    tmux: &shelbi_core::TmuxAddr,
) {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let wt = worktree.to_string_lossy().into_owned();

    // Best-effort; don't fail the whole merge if cleanup hiccups.
    let _ = shelbi_tmux::kill_window(host, tmux);
    let _ = shelbi_ssh::run_capture(
        host,
        ["git", "-C", &repo, "worktree", "remove", "--force", &wt],
    );
    let _ = shelbi_ssh::run_capture(host, ["git", "-C", &repo, "branch", "-D", branch]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_body_with_no_extras_matches_legacy_shape() {
        let (title, body) = derive_pr_text("# Task\n\nFix login.\n", "fix-login", None, None);
        assert_eq!(title, "Fix login.");
        assert!(body.starts_with("# Task\n\nFix login.\n"));
        assert!(body.contains("— opened by [shelbi]"));
        assert!(!body.contains("## Files changed"));
        assert!(!body.contains("Workspace transcript"));
    }

    #[test]
    fn pr_body_includes_diff_stat_when_provided() {
        let stat = " src/foo.rs | 12 +++++++-----\n 1 file changed, 7 insertions(+), 5 deletions(-)";
        let (_, body) = derive_pr_text("# Task\n\nFix.\n", "fix", Some(stat), None);
        assert!(body.contains("## Files changed"));
        assert!(body.contains("src/foo.rs | 12 +++++++-----"));
        assert!(body.contains("1 file changed, 7 insertions(+), 5 deletions(-)"));
        // Stat lives inside a fenced block so GitHub renders the columns intact.
        assert!(body.contains("```\nsrc/foo.rs"));
    }

    #[test]
    fn pr_body_includes_transcript_in_collapsed_details() {
        let transcript = "claude> done\nshelbi task move fix --to review\n";
        let (_, body) = derive_pr_text("# Task\n\nFix.\n", "fix", None, Some(transcript));
        assert!(body.contains("<details>"));
        assert!(body.contains("<summary>Workspace transcript</summary>"));
        assert!(body.contains("claude> done"));
        assert!(body.contains("</details>"));
    }

    #[test]
    fn pr_body_omits_sections_for_empty_or_whitespace_inputs() {
        let (_, body) = derive_pr_text("# Task\n\nFix.\n", "fix", Some("   \n\n"), Some(""));
        assert!(!body.contains("## Files changed"));
        assert!(!body.contains("Workspace transcript"));
    }

    #[test]
    fn pr_body_orders_sections_diff_then_transcript_then_footer() {
        let (_, body) = derive_pr_text(
            "# Task\n\nFix.\n",
            "fix",
            Some(" a | 1 +\n"),
            Some("the transcript"),
        );
        let diff_at = body.find("## Files changed").unwrap();
        let transcript_at = body.find("Workspace transcript").unwrap();
        let footer_at = body.find("— opened by [shelbi]").unwrap();
        assert!(diff_at < transcript_at);
        assert!(transcript_at < footer_at);
    }
}
