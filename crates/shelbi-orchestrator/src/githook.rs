//! Shelbi-managed git hooks for the hub checkout.
//!
//! Motivation (bug-worker-commit-landed-on-hub-main-checkout): the only
//! git checkout on the hub that can attach the project's default branch
//! is the hub work_dir itself — linked worktrees can't double-claim it.
//! Every observed "commit landed on local main" incident was an agent
//! session (orchestrator or interactive firefighting) editing files in
//! the hub checkout and committing before cutting a branch. Prose in the
//! orchestrator instructions ("Don't edit code directly") was the only
//! guard; this module adds the mechanical one: a `pre-commit` hook.
//!
//! ## Context-scoped, not repo-wide
//!
//! The hook lives in the repo's shared hooks dir, which every linked
//! worktree — and the human's own main checkout — inherits. An earlier
//! version of this guard blocked *every* commit to the default branch
//! from any working tree, so a user committing to `main` in a plain,
//! non-Shelbi shell hit a hook they never agreed to and didn't know
//! existed. That silent, repo-wide governance cost real trust
//! ([[feedback-no-silent-git-hook-install]]).
//!
//! The guard is now inverted to be **context-scoped**: it is a no-op
//! unless the committing process carries [`MANAGED_CONTEXT_ENV`], which
//! Shelbi exports into the environment of every orchestrator/worker pane
//! it spawns. A human's plain shell has no such marker, so their commits
//! to `main` are never touched; a Shelbi agent pane inherits it and is
//! still blocked from landing work on a protected branch before cutting a
//! task branch — exactly the incident the guard exists to prevent. The
//! automated squash-merge path commits on a *detached* HEAD in a throwaway
//! worktree (`git worktree add --detach`), which the hook allows either
//! way (`git symbolic-ref` fails on a detached HEAD). The escape hatch for
//! a genuinely intentional protected-branch commit *inside* a managed
//! context is the [`HOOK_BYPASS_ENV`] env var (or git's own `--no-verify`).
//!
//! ## Disclosed install, refresh-only on open, removable
//!
//! Installation is disclosed and consented at `shelbi init` /
//! `shelbi guard install` — never silently created on every project open.
//! On subsequent opens ([`crate::ensure_dashboard`]) the guard is
//! *refreshed only if it already exists* ([`InstallMode::RefreshOnly`]),
//! so Shelbi never newly writes a hook the user didn't agree to. A
//! foreign user hook is always left untouched
//! ([`HookInstall::SkippedForeignHook`]). The hook is removable at any
//! time with `shelbi guard uninstall` ([`uninstall_hub_branch_guard`]),
//! and Shelbi removes it on project teardown so nothing lingers.

use std::path::{Path, PathBuf};

use shelbi_core::{Error, Result};

/// Marker line identifying a hook file as Shelbi-managed. Never change
/// this string — it's how a later install distinguishes "ours, safe to
/// refresh" from a user's own hook it must not clobber, and how
/// [`uninstall_hub_branch_guard`] confirms a hook is ours before removing.
pub const HOOK_MARKER: &str = "# shelbi-managed: hub-default-branch-guard";

/// Env var Shelbi exports into every orchestrator/worker pane it spawns.
/// The context-scoped guard is a no-op unless a committing process carries
/// it, so the hook governs Shelbi's own agents but never the human's plain
/// shell. See the module docs for the trust rationale.
pub const MANAGED_CONTEXT_ENV: &str = "SHELBI_MANAGED_CONTEXT";

/// Env var that bypasses the guard for a genuinely intentional commit on a
/// protected branch *from within a managed context*:
/// `SHELBI_ALLOW_DEFAULT_BRANCH_COMMIT=1 git commit …`.
pub const HOOK_BYPASS_ENV: &str = "SHELBI_ALLOW_DEFAULT_BRANCH_COMMIT";

/// Whether [`install_hub_branch_guard`] may create a brand-new hook or
/// only refresh one Shelbi already installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// Create the hook if no `pre-commit` exists (the disclosed/consented
    /// `shelbi init` and `shelbi guard install` paths).
    CreateIfMissing,
    /// Only refresh an already-Shelbi-installed hook; never write a new
    /// one. Used on every project open so Shelbi cannot silently install a
    /// hook into a repo the user never opted into.
    RefreshOnly,
}

/// What [`install_hub_branch_guard`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookInstall {
    /// No `pre-commit` hook existed; the guard was written
    /// ([`InstallMode::CreateIfMissing`] only).
    Installed,
    /// A Shelbi-managed hook existed and was rewritten with the current
    /// script (branch list or script body may have changed).
    Refreshed,
    /// [`InstallMode::RefreshOnly`] and no `pre-commit` hook exists — the
    /// normal "user hasn't opted in / has been removed" state on project
    /// open. Not an error and not worth a warning: nothing was written.
    SkippedNotInstalled,
    /// A user-authored `pre-commit` hook (no [`HOOK_MARKER`]) is already
    /// in place — left untouched so we don't destroy user config. The
    /// guard is NOT active in this case; the caller should warn.
    SkippedForeignHook,
}

/// What [`uninstall_hub_branch_guard`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookUninstall {
    /// A Shelbi-managed hook was found and removed.
    Removed,
    /// No `pre-commit` hook exists — nothing to remove.
    NotPresent,
    /// A user-authored `pre-commit` hook (no [`HOOK_MARKER`]) is in place —
    /// left untouched. We never delete a hook we didn't write.
    SkippedForeignHook,
}

/// Render the `pre-commit` script. The guard is context-scoped: a no-op
/// unless the committing process carries [`MANAGED_CONTEXT_ENV`], after
/// which it rejects commits while HEAD is attached to any of `protected`
/// (deduplicated by the caller). Detached HEAD is always allowed — `git
/// symbolic-ref` fails there, which is the state the automated
/// squash-merge's temp worktree commits in.
fn hook_script(protected: &[&str]) -> String {
    let branches = protected.join(" ");
    format!(
        r#"#!/bin/sh
{HOOK_MARKER}
# Installed by Shelbi (see `shelbi guard --help`) and refreshed on project
# open only while it already exists — never silently re-created. Do not edit;
# refresh overwrites the body. Remove it anytime: `shelbi guard uninstall`
# (or just delete this file).
#
# Context-scoped: this hook is a NO-OP in your normal shell. It blocks a
# `git commit` on a protected branch ONLY when run from inside a Shelbi-managed
# agent pane (which exports {MANAGED_CONTEXT_ENV}), so an orchestrator/worker
# can't land work directly on the branch before cutting a task branch. Your own
# commits from a plain shell are never affected.
# Intentional override inside a managed context: {HOOK_BYPASS_ENV}=1 git commit …
[ -n "${{{MANAGED_CONTEXT_ENV}:-}}" ] || exit 0
[ -n "${{{HOOK_BYPASS_ENV}:-}}" ] && exit 0
branch=$(git symbolic-ref --quiet --short HEAD) || exit 0
for protected in {branches}; do
  if [ "$branch" = "$protected" ]; then
    echo "shelbi: refusing to commit on \`$branch\` — this checkout's protected branch." >&2
    echo "shelbi: this pane is a Shelbi-managed context; put the work on a task branch instead:" >&2
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

/// Install or refresh the hub checkout's context-scoped default-branch
/// commit guard.
///
/// `mode` decides whether an absent hook is created: pass
/// [`InstallMode::CreateIfMissing`] on the disclosed/consented `shelbi
/// init` and `shelbi guard install` paths, and [`InstallMode::RefreshOnly`]
/// on every project open so Shelbi never silently writes a hook the user
/// didn't opt into. `protected` is the branch list to guard (the project's
/// `default_branch`, plus `git.base_branch` when it differs). A
/// user-authored `pre-commit` hook is never overwritten; the caller gets
/// [`HookInstall::SkippedForeignHook`] and should surface a warning.
///
/// Idempotent — safe to call repeatedly. Hub-local only: takes a plain
/// path because the hub machine is the only place the default branch can be
/// attached (linked worktrees can't double-claim it), so there's nothing to
/// install on remote clones' behalf here.
pub fn install_hub_branch_guard(
    work_dir: &Path,
    protected: &[&str],
    mode: InstallMode,
) -> Result<HookInstall> {
    let dir = hooks_dir(work_dir)?;
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;
    let hook_path = dir.join("pre-commit");

    let outcome = match std::fs::read_to_string(&hook_path) {
        Ok(existing) if existing.contains(HOOK_MARKER) => HookInstall::Refreshed,
        Ok(_) => return Ok(HookInstall::SkippedForeignHook),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match mode {
            InstallMode::CreateIfMissing => HookInstall::Installed,
            // Refresh-only: nothing to refresh, and we won't create one
            // silently. This is the normal state until the user opts in.
            InstallMode::RefreshOnly => return Ok(HookInstall::SkippedNotInstalled),
        },
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

/// Remove the Shelbi-managed default-branch commit guard from the hub
/// checkout. Only a hook carrying [`HOOK_MARKER`] is deleted — a
/// user-authored `pre-commit` hook is never touched
/// ([`HookUninstall::SkippedForeignHook`]), and an absent hook is a clean
/// no-op ([`HookUninstall::NotPresent`]).
///
/// Backs `shelbi guard uninstall` and the project-teardown cleanup so
/// nothing Shelbi installed lingers after the user stops using it.
pub fn uninstall_hub_branch_guard(work_dir: &Path) -> Result<HookUninstall> {
    let hook_path = hooks_dir(work_dir)?.join("pre-commit");
    match std::fs::read_to_string(&hook_path) {
        Ok(existing) if existing.contains(HOOK_MARKER) => {
            std::fs::remove_file(&hook_path).map_err(Error::Io)?;
            Ok(HookUninstall::Removed)
        }
        // A foreign hook — or one we can't read/identify — is left in place.
        Ok(_) | Err(_) if hook_path.exists() => Ok(HookUninstall::SkippedForeignHook),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HookUninstall::NotPresent),
        // Unreadable but present: don't delete what we can't identify.
        _ => Ok(HookUninstall::SkippedForeignHook),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Run git as a *plain human shell* would: no managed-context marker,
    /// no bypass var. The env scrubbing matters because these tests may run
    /// inside a Shelbi worker pane that exports `SHELBI_MANAGED_CONTEXT` —
    /// without scrubbing, "plain shell" cases would inherit it and the
    /// context-scoped guard would wrongly fire.
    fn run_git(repo: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env_remove(HOOK_BYPASS_ENV)
            .env_remove(MANAGED_CONTEXT_ENV)
            .output()
            .expect("run git")
    }

    /// Run git as a Shelbi-managed agent pane would: the managed-context
    /// marker is present, so the guard is armed.
    fn run_git_managed(repo: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env_remove(HOOK_BYPASS_ENV)
            .env(MANAGED_CONTEXT_ENV, "1")
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

    /// Acceptance: a `git commit` on the protected branch *from a
    /// Shelbi-managed context* (marker present) is rejected, with a message
    /// pointing at the task-branch flow; the same commit on a task branch
    /// goes through.
    #[test]
    fn hook_blocks_commit_on_protected_branch_and_allows_task_branch() {
        let repo = fixture_repo("block-main");
        let outcome = install_hub_branch_guard(&repo, &["main"], InstallMode::CreateIfMissing).unwrap();
        assert_eq!(outcome, HookInstall::Installed);

        stage_change(&repo, "b.txt");
        let out = run_git_managed(&repo, &["commit", "-m", "should be blocked"]);
        assert!(!out.status.success(), "commit on main must be rejected in a managed context");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("git checkout -b") && stderr.contains("shelbi task start"),
            "rejection must point at the task-branch flow, got: {stderr}"
        );

        // Same staged change commits fine once a branch is cut — the
        // recovery the hook's message recommends — even in a managed context.
        assert_git_ok(&repo, &["checkout", "-b", "fix/some-task"]);
        let out = run_git_managed(&repo, &["commit", "-m", "lands on the task branch"]);
        assert!(
            out.status.success(),
            "commit on a non-protected branch must be allowed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Acceptance (the trust fix): a `git commit` on the protected branch
    /// from a *plain human shell* (no managed-context marker) succeeds — the
    /// hook is a no-op there and never governs the user's own repo.
    #[test]
    fn hook_is_a_noop_without_managed_context_marker() {
        let repo = fixture_repo("noop-plain-shell");
        install_hub_branch_guard(&repo, &["main"], InstallMode::CreateIfMissing).unwrap();

        stage_change(&repo, "b.txt");
        // `run_git` scrubs the marker, simulating a plain non-Shelbi shell.
        let out = run_git(&repo, &["commit", "-m", "human commit on main"]);
        assert!(
            out.status.success(),
            "a commit on main from a non-managed shell must be allowed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Acceptance: the documented override exists — inside a managed context
    /// the bypass env var lets an intentional commit through on the
    /// protected branch.
    #[test]
    fn bypass_env_var_allows_intentional_commit_on_protected_branch() {
        let repo = fixture_repo("bypass");
        install_hub_branch_guard(&repo, &["main"], InstallMode::CreateIfMissing).unwrap();

        stage_change(&repo, "b.txt");
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-m", "intentional"])
            .env(MANAGED_CONTEXT_ENV, "1")
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
        install_hub_branch_guard(&repo, &["main"], InstallMode::CreateIfMissing).unwrap();

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
        install_hub_branch_guard(&repo, &["main", "develop"], InstallMode::CreateIfMissing).unwrap();

        assert_git_ok(&repo, &["checkout", "-b", "develop"]);
        stage_change(&repo, "b.txt");
        let out = run_git_managed(&repo, &["commit", "-m", "blocked on develop too"]);
        assert!(
            !out.status.success(),
            "commit on a secondary protected branch must be rejected in a managed context"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// `RefreshOnly` never writes a brand-new hook — the project-open path
    /// must not silently install into a repo the user never opted into. Once
    /// a Shelbi hook exists (a consented install), `RefreshOnly` rewrites it.
    #[test]
    fn refresh_only_never_creates_but_refreshes_existing() {
        let repo = fixture_repo("refresh-only");

        // No hook yet: refresh-only is a no-op, nothing written.
        assert_eq!(
            install_hub_branch_guard(&repo, &["main"], InstallMode::RefreshOnly).unwrap(),
            HookInstall::SkippedNotInstalled
        );
        assert!(
            !repo.join(".git/hooks/pre-commit").exists(),
            "refresh-only must not create a hook the user never consented to"
        );

        // Consented install, then a later open refreshes the existing hook.
        assert_eq!(
            install_hub_branch_guard(&repo, &["main"], InstallMode::CreateIfMissing).unwrap(),
            HookInstall::Installed
        );
        assert_eq!(
            install_hub_branch_guard(&repo, &["main", "develop"], InstallMode::RefreshOnly).unwrap(),
            HookInstall::Refreshed
        );
        let hook = std::fs::read_to_string(repo.join(".git/hooks/pre-commit")).unwrap();
        assert!(hook.contains("develop"), "refresh must rewrite the branch list");

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Acceptance: the guard is trivially removable, and removal never
    /// touches a user-authored hook.
    #[test]
    fn uninstall_removes_managed_hook_and_spares_foreign_hook() {
        let repo = fixture_repo("uninstall");
        let hook_path = repo.join(".git/hooks/pre-commit");

        // Nothing installed yet.
        assert_eq!(
            uninstall_hub_branch_guard(&repo).unwrap(),
            HookUninstall::NotPresent
        );

        // Install then remove ours.
        install_hub_branch_guard(&repo, &["main"], InstallMode::CreateIfMissing).unwrap();
        assert!(hook_path.exists());
        assert_eq!(
            uninstall_hub_branch_guard(&repo).unwrap(),
            HookUninstall::Removed
        );
        assert!(!hook_path.exists(), "the managed hook must be gone");

        // A foreign hook is never removed.
        let user_hook = "#!/bin/sh\necho user hook\n";
        std::fs::write(&hook_path, user_hook).unwrap();
        assert_eq!(
            uninstall_hub_branch_guard(&repo).unwrap(),
            HookUninstall::SkippedForeignHook
        );
        assert_eq!(
            std::fs::read_to_string(&hook_path).unwrap(),
            user_hook,
            "a user-authored hook must be left byte-identical"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Re-install refreshes a Shelbi-managed hook in place (e.g. the
    /// protected-branch list changed) but never clobbers a user hook.
    #[test]
    fn reinstall_refreshes_managed_hook_and_skips_foreign_hook() {
        let repo = fixture_repo("refresh");
        assert_eq!(
            install_hub_branch_guard(&repo, &["main"], InstallMode::CreateIfMissing).unwrap(),
            HookInstall::Installed
        );
        assert_eq!(
            install_hub_branch_guard(&repo, &["main", "develop"], InstallMode::CreateIfMissing).unwrap(),
            HookInstall::Refreshed
        );
        let hook = std::fs::read_to_string(repo.join(".git/hooks/pre-commit")).unwrap();
        assert!(hook.contains("develop"), "refresh must rewrite the branch list");

        // Foreign hook: replace ours with a user script, then re-install.
        let user_hook = "#!/bin/sh\necho user hook\n";
        std::fs::write(repo.join(".git/hooks/pre-commit"), user_hook).unwrap();
        assert_eq!(
            install_hub_branch_guard(&repo, &["main"], InstallMode::CreateIfMissing).unwrap(),
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
        assert!(install_hub_branch_guard(&dir, &["main"], InstallMode::CreateIfMissing).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
