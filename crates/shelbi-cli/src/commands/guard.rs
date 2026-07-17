//! `shelbi guard` — manage the hub checkout's context-scoped default-branch
//! commit guard (the Shelbi-managed `pre-commit` hook).
//!
//! The hook only blocks commits made from inside a Shelbi-managed agent pane
//! (which exports `SHELBI_MANAGED_CONTEXT`); a human's plain shell is never
//! governed. Install is disclosed and consented here (and at `shelbi init`);
//! project open only *refreshes* an already-installed hook. See
//! [`shelbi_orchestrator::githook`] for the mechanics and the trust rationale.

use std::path::Path;

use anyhow::{anyhow, Result};
use clap::Subcommand;
use shelbi_core::{MachineKind, Project};
use shelbi_orchestrator::githook::{self, HookInstall, HookUninstall, InstallMode};

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum GuardCmd {
    /// Install (or refresh) the default-branch commit guard in this project's
    /// hub checkout. Discloses what the hook does before writing it.
    Install,
    /// Remove the Shelbi-managed default-branch commit guard from this
    /// project's hub checkout. A user-authored hook is never touched.
    Uninstall,
    /// Report whether the guard is installed in this project's hub checkout.
    Status,
}

pub fn run(project_opt: Option<String>, cmd: GuardCmd) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let work_dir = hub_work_dir(&project)?;
    match cmd {
        GuardCmd::Install => {
            let outcome = install(&project, work_dir)?;
            report_install(&outcome, work_dir);
        }
        GuardCmd::Uninstall => {
            let outcome = githook::uninstall_hub_branch_guard(work_dir)?;
            report_uninstall(&outcome, work_dir);
        }
        GuardCmd::Status => report_status(work_dir),
    }
    Ok(())
}

/// The hub checkout that owns the shared `pre-commit` hook — the project's
/// local machine `work_dir`.
fn hub_work_dir(project: &Project) -> Result<&Path> {
    project
        .machines
        .iter()
        .find(|m| matches!(m.kind, MachineKind::Local))
        .map(|m| m.work_dir.as_path())
        .ok_or_else(|| anyhow!("project `{}` has no local hub machine", project.name))
}

/// Install the guard, creating it if absent. Shared by `shelbi guard install`
/// and the `shelbi init` disclosure path so both write the same hook.
pub fn install(project: &Project, work_dir: &Path) -> Result<HookInstall> {
    let protected = shelbi_orchestrator::protected_default_branches(project);
    let refs: Vec<&str> = protected.iter().map(String::as_str).collect();
    githook::install_hub_branch_guard(work_dir, &refs, InstallMode::CreateIfMissing)
        .map_err(|e| anyhow!(e))
}

/// Install the guard from an `init`/scaffold path: disclose on a fresh
/// install, warn (stderr) when a foreign hook blocks it, and stay silent on
/// the "already there" refresh and on best-effort failures (e.g. the project
/// root isn't a git repo yet — `shelbi guard install` can add it later). Never
/// fails init: a hook is a convenience, not a precondition.
pub fn install_at_init(project: &Project, work_dir: &Path) {
    match install(project, work_dir) {
        Ok(HookInstall::Installed) => disclose_on_first_install(work_dir),
        Ok(HookInstall::SkippedForeignHook) => {
            eprintln!(
                "shelbi: {}/.git/hooks/pre-commit is user-authored — left untouched. \
                 The default-branch commit guard was NOT installed.",
                work_dir.display()
            );
        }
        // Refreshed (already installed) and any best-effort error stay quiet.
        _ => {}
    }
}

/// One-time disclosure printed when `shelbi init` actually writes the hook,
/// so the user learns it exists, what it does, and how to remove it — the
/// exact transparency the silent-install trust incident demanded.
pub fn disclose_on_first_install(work_dir: &Path) {
    eprintln!(
        "shelbi: installed a git pre-commit hook at {}/.git/hooks/pre-commit.\n\
         shelbi:   It blocks commits to a protected branch ONLY inside Shelbi-managed\n\
         shelbi:   agent panes (marked with SHELBI_MANAGED_CONTEXT); your own commits from\n\
         shelbi:   a normal shell are never affected. Remove it anytime with\n\
         shelbi:   `shelbi guard uninstall`.",
        work_dir.display(),
    );
}

fn report_install(outcome: &HookInstall, work_dir: &Path) {
    match outcome {
        HookInstall::Installed => disclose_on_first_install(work_dir),
        HookInstall::Refreshed => {
            println!(
                "✓ refreshed the Shelbi commit guard at {}/.git/hooks/pre-commit",
                work_dir.display()
            );
        }
        HookInstall::SkippedForeignHook => {
            eprintln!(
                "shelbi: {}/.git/hooks/pre-commit is user-authored — left untouched. \
                 The Shelbi commit guard was NOT installed.",
                work_dir.display()
            );
        }
        // CreateIfMissing never returns SkippedNotInstalled, but handle it
        // exhaustively rather than swallow a future behavior change.
        HookInstall::SkippedNotInstalled => {}
    }
}

fn report_uninstall(outcome: &HookUninstall, work_dir: &Path) {
    match outcome {
        HookUninstall::Removed => {
            println!(
                "✓ removed the Shelbi commit guard from {}/.git/hooks/pre-commit",
                work_dir.display()
            );
        }
        HookUninstall::NotPresent => {
            println!(
                "(no pre-commit hook at {}/.git/hooks — nothing to remove)",
                work_dir.display()
            );
        }
        HookUninstall::SkippedForeignHook => {
            eprintln!(
                "shelbi: {}/.git/hooks/pre-commit is user-authored — left untouched.",
                work_dir.display()
            );
        }
    }
}

fn report_status(work_dir: &Path) {
    let hook = work_dir.join(".git/hooks/pre-commit");
    match std::fs::read_to_string(&hook) {
        Ok(body) if body.contains(githook::HOOK_MARKER) => {
            println!("installed: Shelbi commit guard at {}", hook.display());
        }
        Ok(_) => {
            println!(
                "not installed: {} exists but is user-authored (left untouched)",
                hook.display()
            );
        }
        Err(_) => {
            println!("not installed: no Shelbi commit guard in {}", work_dir.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn git_repo(tag: &str) -> PathBuf {
        let repo = std::env::temp_dir().join(format!(
            "shelbi-guard-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&repo).unwrap();
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-b", "main"])
            .output()
            .expect("git init")
            .status
            .success();
        assert!(ok, "git init failed");
        repo
    }

    fn project_with_hub(work_dir: &Path) -> Project {
        let yaml = format!(
            "name: t\nrepo: {wd}\ndefault_branch: main\n\
             machines:\n  - name: hub\n    kind: local\n    work_dir: {wd}\n\
             orchestrator:\n  runner: claude\n\
             agent_runners:\n  claude: {{ command: claude, flags: [] }}\n",
            wd = work_dir.display()
        );
        serde_yaml::from_str(&yaml).expect("minimal project YAML parses")
    }

    #[test]
    fn hub_work_dir_resolves_the_local_machine() {
        let repo = git_repo("hub-resolve");
        let project = project_with_hub(&repo);
        assert_eq!(hub_work_dir(&project).unwrap(), repo.as_path());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn install_then_uninstall_roundtrips_the_managed_hook() {
        let repo = git_repo("roundtrip");
        let project = project_with_hub(&repo);
        let work_dir = hub_work_dir(&project).unwrap();
        let hook = repo.join(".git/hooks/pre-commit");

        assert_eq!(install(&project, work_dir).unwrap(), HookInstall::Installed);
        let body = std::fs::read_to_string(&hook).unwrap();
        assert!(body.contains(githook::HOOK_MARKER), "wrote a Shelbi-managed hook");

        assert_eq!(
            githook::uninstall_hub_branch_guard(work_dir).unwrap(),
            HookUninstall::Removed
        );
        assert!(!hook.exists(), "uninstall removed the managed hook");

        let _ = std::fs::remove_dir_all(&repo);
    }
}
