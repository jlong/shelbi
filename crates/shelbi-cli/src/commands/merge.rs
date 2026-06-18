use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use shelbi_core::{Host, Machine, Status};

use super::require_project;

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
    let target = project.default_branch.clone();

    if pr {
        return run_pr(
            &project_name,
            &file,
            &project,
            &machine,
            &host,
            &branch,
            &target,
            &id,
        );
    }

    preflight(&host, &machine, &target)?;
    capture_uncommitted(&host, &file.agent.worktree, &id)?;
    squash_merge(&host, &machine, &branch, &target, &id)?;
    cleanup(&host, &machine, &file.agent.worktree, &branch, &file.agent.tmux);

    let mut updated = file.agent.clone();
    updated.status = Status::Done;
    updated.updated = Utc::now();
    shelbi_state::save_agent(&project_name, &updated, &file.body).map_err(|e| anyhow!(e))?;
    shelbi_state::append_log(&project_name, &id, "merge").map_err(|e| anyhow!(e))?;
    println!("✓ merged {id} into {target}");
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
    id: &str,
) -> Result<()> {
    // 1. Make sure `gh` is reachable on the worker host.
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

    // 4. Open the PR. Title pulled from the task heading in the markdown body;
    //    body is the rest of the agent file (sans the H1).
    let (title, body) = derive_pr_text(&file.body, id);
    let pr_url = shelbi_ssh::run_capture(
        host,
        [
            "gh", "-C", &wt, "pr", "create", "--base", target, "--head", branch, "--title",
            &title, "--body", &body,
        ],
    )
    .map_err(|e| anyhow!(e))?;
    let pr_url = pr_url.trim();

    // 5. Update state (still Running until merged in PR, but flag Waiting so
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

fn derive_pr_text(body_md: &str, id: &str) -> (String, String) {
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
    let mut body = String::from(body_md);
    body.push_str("\n\n— opened by [shelbi](https://github.com/jlong/shelbi)\n");
    (title, body)
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

fn squash_merge(
    host: &Host,
    machine: &Machine,
    branch: &str,
    target: &str,
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

    shelbi_ssh::run_capture(host, ["git", "-C", &repo, "merge", "--squash", branch])
        .map_err(|e| anyhow!(e))?;
    let summary = format!("shelbi: merge {id} from {branch}");
    shelbi_ssh::run_capture(host, ["git", "-C", &repo, "commit", "-m", &summary])
        .map_err(|e| anyhow!(e))?;
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
