//! Shelbi-managed git hooks for the hub checkout.
//!
//! Motivation (bug-worker-commit-landed-on-hub-main-checkout): the only
//! git checkout on the hub that can attach the project's default branch
//! is the hub work_dir itself — linked worktrees can't double-claim it.
//! Every observed "commit landed on local main" incident was an agent
//! session (orchestrator or interactive firefighting) editing files in
//! the hub checkout and committing before cutting a branch. Prose in the
//! orchestrator instructions ("Don't edit code directly") was the only
//! guard; this module adds the mechanical one: a `pre-commit` hook that
//! rejects commits while HEAD is attached to a protected branch.
//!
//! The hook lives in the repo's hooks dir, which linked worktrees share —
//! that's fine and even desirable: task branches are never the default
//! branch, and the automated squash-merge path commits on a *detached*
//! HEAD in a throwaway worktree (`git worktree add --detach`), which the
//! hook explicitly allows. The escape hatch for genuinely intentional
//! commits is the `SHELBI_ALLOW_DEFAULT_BRANCH_COMMIT` env var (or git's
//! own `--no-verify`).
//!
//! Installation is idempotent and runs at project open
//! ([`crate::ensure_dashboard`]): a missing hook is written, a
//! Shelbi-managed hook (identified by [`HOOK_MARKER`]) is refreshed in
//! place, and a foreign user hook is left untouched (surfaced to the
//! caller as [`HookInstall::SkippedForeignHook`]).

use std::path::{Path, PathBuf};

use shelbi_core::{Error, Result};

/// Marker line identifying a hook file as Shelbi-managed. Never change
/// this string — it's how a later install distinguishes "ours, safe to
/// refresh" from a user's own hook it must not clobber.
pub const HOOK_MARKER: &str = "# shelbi-managed: hub-default-branch-guard";

/// Env var that bypasses the guard for genuinely intentional commits on
/// a protected branch: `SHELBI_ALLOW_DEFAULT_BRANCH_COMMIT=1 git commit …`.
pub const HOOK_BYPASS_ENV: &str = "SHELBI_ALLOW_DEFAULT_BRANCH_COMMIT";

/// What [`install_hub_branch_guard`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookInstall {
    /// No `pre-commit` hook existed; the guard was written.
    Installed,
    /// A Shelbi-managed hook existed and was rewritten with the current
    /// script (branch list or script body may have changed).
    Refreshed,
    /// A user-authored `pre-commit` hook (no [`HOOK_MARKER`]) is already
    /// in place — left untouched so we don't destroy user config. The
    /// guard is NOT active in this case; the caller should warn.
    SkippedForeignHook,
}

/// Render the `pre-commit` script that rejects commits while HEAD is
/// attached to any of `protected` (deduplicated by the caller). Detached
/// HEAD is always allowed — `git symbolic-ref` fails there, which is the
/// state the automated squash-merge's temp worktree commits in.
fn hook_script(protected: &[&str]) -> String {
    let branches = protected.join(" ");
    format!(
        r#"#!/bin/sh
{HOOK_MARKER}
# Installed and refreshed by Shelbi at project open. Do not edit — changes
# will be overwritten on the next open. Blocks `git commit` while HEAD is
# attached to a protected branch so a hub-checkout session can't land work
# directly on it (task bug-worker-commit-landed-on-hub-main-checkout).
# Intentional override: {HOOK_BYPASS_ENV}=1 git commit …
[ -n "${{{HOOK_BYPASS_ENV}:-}}" ] && exit 0
branch=$(git symbolic-ref --quiet --short HEAD) || exit 0
for protected in {branches}; do
  if [ "$branch" = "$protected" ]; then
    echo "shelbi: refusing to commit on \`$branch\` — this checkout's protected branch." >&2
    echo "shelbi: put the work on a task branch instead:" >&2
    echo "shelbi:   git checkout -b <branch>   (your staged changes come with you)" >&2
    echo "shelbi: or dispatch it properly via \`shelbi task start <task-id>\`." >&2
    echo "shelbi: intentional override: {HOOK_BYPASS_ENV}=1 git commit …" >&2
    exit 1
  fi
done
exit 0
"#
    )
}

/// Resolve the repo's hooks directory via `git rev-parse --git-path
/// hooks`, which honors `core.hooksPath` on modern git. A relative
/// result (the common case: `.git/hooks`) is joined onto `work_dir`.
fn hooks_dir(work_dir: &Path) -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(work_dir)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "resolving hooks dir for {}: git rev-parse --git-path hooks failed: {}",
            work_dir.display(),
            String::from_utf8_lossy(&out.stderr).trim(),
        )));
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let path = PathBuf::from(rel);
    Ok(if path.is_absolute() {
        path
    } else {
        work_dir.join(path)
    })
}

/// Install or refresh the hub checkout's default-branch commit guard.
///
/// Idempotent — safe to call on every project open. `protected` is the
/// branch list to guard (the project's `default_branch`, plus
/// `git.base_branch` when it differs). A user-authored `pre-commit` hook
/// is never overwritten; the caller gets [`HookInstall::SkippedForeignHook`]
/// and should surface a warning.
///
/// Hub-local only: takes a plain path because the hub machine is the
/// only place the default branch can be attached (linked worktrees can't
/// double-claim it), so there's nothing to install on remote clones'
/// behalf here.
pub fn install_hub_branch_guard(work_dir: &Path, protected: &[&str]) -> Result<HookInstall> {
    let dir = hooks_dir(work_dir)?;
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;
    let hook_path = dir.join("pre-commit");

    let outcome = match std::fs::read_to_string(&hook_path) {
        Ok(existing) if existing.contains(HOOK_MARKER) => HookInstall::Refreshed,
        Ok(_) => return Ok(HookInstall::SkippedForeignHook),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HookInstall::Installed,
        // An unreadable existing hook (perms, non-UTF8 binary) is treated
        // as foreign — refusing to touch what we can't identify.
        Err(_) => return Ok(HookInstall::SkippedForeignHook),
    };

    // Write-then-rename so a crash mid-write can't leave a truncated,
    // half-executable hook that breaks every commit in the repo.
    let tmp = dir.join(".pre-commit.shelbi.tmp");
    std::fs::write(&tmp, hook_script(protected)).map_err(Error::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(Error::Io)?;
    }
    std::fs::rename(&tmp, &hook_path).map_err(Error::Io)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run_git(repo: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            // Make the hook's env deterministic even when the test runs
            // inside a session that exported the bypass var.
            .env_remove(HOOK_BYPASS_ENV)
            .output()
            .expect("run git")
    }

    fn assert_git_ok(repo: &Path, args: &[&str]) {
        let out = run_git(repo, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Fresh repo on `main` with one commit, in a unique temp dir.
    fn fixture_repo(tag: &str) -> PathBuf {
        let repo = std::env::temp_dir().join(format!(
            "shelbi-githook-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&repo).unwrap();
        assert_git_ok(&repo, &["init", "-b", "main"]);
        assert_git_ok(&repo, &["config", "user.email", "test@example.com"]);
        assert_git_ok(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        assert_git_ok(&repo, &["add", "a.txt"]);
        assert_git_ok(&repo, &["commit", "-m", "init"]);
        repo
    }

    fn stage_change(repo: &Path, name: &str) {
        std::fs::write(repo.join(name), "change\n").unwrap();
        assert_git_ok(repo, &["add", name]);
    }

    /// Acceptance: a `git commit` while HEAD is attached to the default
    /// branch is rejected, with a message pointing at the task-branch
    /// flow; the same commit on a task branch goes through.
    #[test]
    fn hook_blocks_commit_on_protected_branch_and_allows_task_branch() {
        let repo = fixture_repo("block-main");
        let outcome = install_hub_branch_guard(&repo, &["main"]).unwrap();
        assert_eq!(outcome, HookInstall::Installed);

        stage_change(&repo, "b.txt");
        let out = run_git(&repo, &["commit", "-m", "should be blocked"]);
        assert!(!out.status.success(), "commit on main must be rejected");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("git checkout -b") && stderr.contains("shelbi task start"),
            "rejection must point at the task-branch flow, got: {stderr}"
        );

        // Same staged change commits fine once a branch is cut — the
        // recovery the hook's message recommends.
        assert_git_ok(&repo, &["checkout", "-b", "fix/some-task"]);
        assert_git_ok(&repo, &["commit", "-m", "lands on the task branch"]);

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Acceptance: the documented override exists — the bypass env var
    /// lets an intentional commit through on the protected branch.
    #[test]
    fn bypass_env_var_allows_intentional_commit_on_protected_branch() {
        let repo = fixture_repo("bypass");
        install_hub_branch_guard(&repo, &["main"]).unwrap();

        stage_change(&repo, "b.txt");
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-m", "intentional"])
            .env(HOOK_BYPASS_ENV, "1")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "bypass env must allow the commit: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Acceptance (squash-merge compatibility): the automated merge path
    /// commits on a *detached* HEAD in a temp worktree — exercise exactly
    /// that shape (`worktree add --detach` + `merge --squash` + `commit`)
    /// and assert the hook lets it through. Hooks are shared across
    /// linked worktrees, so this is the load-bearing compatibility check.
    #[test]
    fn detached_squash_merge_commit_in_temp_worktree_is_allowed() {
        let repo = fixture_repo("squash-merge");
        install_hub_branch_guard(&repo, &["main"]).unwrap();

        // A task branch with a commit beyond main.
        assert_git_ok(&repo, &["checkout", "-b", "shelbi/task-x"]);
        stage_change(&repo, "feature.txt");
        assert_git_ok(&repo, &["commit", "-m", "task work"]);
        assert_git_ok(&repo, &["checkout", "main"]);

        // Mirror merge_and_push_in_worktree: detached temp worktree at
        // the target, squash, explicit commit.
        let tmp = repo.join("tmp-merge-wt");
        let tmp_str = tmp.to_string_lossy().into_owned();
        assert_git_ok(&repo, &["worktree", "add", "--detach", &tmp_str, "main"]);
        assert_git_ok(&tmp, &["merge", "--squash", "shelbi/task-x"]);
        assert_git_ok(&tmp, &["commit", "-m", "shelbi: merge task-x from shelbi/task-x"]);

        assert_git_ok(&repo, &["worktree", "remove", "--force", &tmp_str]);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// The hook guards every branch in `protected` (default + base when
    /// they differ), not just the first.
    #[test]
    fn hook_blocks_every_protected_branch() {
        let repo = fixture_repo("multi-branch");
        install_hub_branch_guard(&repo, &["main", "develop"]).unwrap();

        assert_git_ok(&repo, &["checkout", "-b", "develop"]);
        stage_change(&repo, "b.txt");
        let out = run_git(&repo, &["commit", "-m", "blocked on develop too"]);
        assert!(
            !out.status.success(),
            "commit on a secondary protected branch must be rejected"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Re-install refreshes a Shelbi-managed hook in place (e.g. the
    /// protected-branch list changed) but never clobbers a user hook.
    #[test]
    fn reinstall_refreshes_managed_hook_and_skips_foreign_hook() {
        let repo = fixture_repo("refresh");
        assert_eq!(
            install_hub_branch_guard(&repo, &["main"]).unwrap(),
            HookInstall::Installed
        );
        assert_eq!(
            install_hub_branch_guard(&repo, &["main", "develop"]).unwrap(),
            HookInstall::Refreshed
        );
        let hook = std::fs::read_to_string(repo.join(".git/hooks/pre-commit")).unwrap();
        assert!(hook.contains("develop"), "refresh must rewrite the branch list");

        // Foreign hook: replace ours with a user script, then re-install.
        let user_hook = "#!/bin/sh\necho user hook\n";
        std::fs::write(repo.join(".git/hooks/pre-commit"), user_hook).unwrap();
        assert_eq!(
            install_hub_branch_guard(&repo, &["main"]).unwrap(),
            HookInstall::SkippedForeignHook
        );
        assert_eq!(
            std::fs::read_to_string(repo.join(".git/hooks/pre-commit")).unwrap(),
            user_hook,
            "a user-authored hook must be left byte-identical"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A work_dir that isn't a git repo surfaces an error (the caller
    /// treats install as best-effort and warns).
    #[test]
    fn install_errors_on_non_repo_dir() {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-githook-nonrepo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(install_hub_branch_guard(&dir, &["main"]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
