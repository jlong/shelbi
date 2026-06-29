//! Branch-cut lifecycle for the `Todo -> InProgress` transition.
//!
//! When a task moves into `InProgress` we cut its feature branch on the
//! hub workdir, idempotently, and persist the branch name back onto the
//! task. The base of the cut is **dependency-aware**: if the task has
//! `depends_on` entries, the first dep that already carries a `branch:`
//! is used as the base — that's what makes a chain `A -> B -> C` build
//! one branch on top of the other instead of all of them re-rooting at
//! `main`. With no usable dep branch, the cut falls back to the
//! project-level base from [`Project::base_branch`].
//!
//! This module wraps `shelbi_state::move_task` for both the CLI
//! (`shelbi task move`) and the TUI (kanban left/right). `shelbi task
//! start` also routes through here so the same branch ends up persisted
//! whether the user did `move` then `start` or `start` straight from
//! `todo`. The cut runs against the project's *hub* workdir
//! (`crate::git::locate_hub_workdir`); for a hub-local workspace that's
//! the same repo `sync_worktree` later reads, so the next `git worktree
//! add` sees the branch already in place. SSH workspaces still inherit the
//! pre-existing `sync_worktree` fallback (cut off `default_branch` on
//! the remote machine) when the resolved base isn't visible there — a
//! depends_on chain across machines is out of scope for this pass.

use shelbi_core::{Error, Host, Project, Result, Task};
use shelbi_state::TaskFile;

use crate::git::{locate_hub_workdir, run_in_dir};

/// The branch a task's worktree should be on. Returns `task.branch` if
/// already populated; otherwise the conventional `shelbi/<task-id>`.
///
/// Pure — no I/O. Use [`ensure_branch_for_in_progress`] when you also
/// want the branch cut on disk + persisted into the task file.
pub fn branch_name_for_task(task: &Task) -> String {
    task.branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{}", task.id))
}

/// Resolve the base branch a task's feature branch should be cut from.
///
/// The contract:
///
/// 1. If `task.depends_on` has any entries, walk them in declaration
///    order. The first dep that exists in `all_tasks` *and* carries a
///    `branch:` value wins — that branch becomes the base.
/// 2. Otherwise (no deps, or none of them have a branch yet) fall back
///    to [`Project::base_branch`].
///
/// Notes on the dep selection:
///
/// - We accept any column for the chosen dep — a dep can be
///   `InProgress`, `Review`, or even `Done` and still hand us a valid
///   base. Validating that the dep's branch still exists on the host is
///   the cut step's job ([`cut_branch_on_hub`]).
/// - We deliberately **don't** skip `Done` deps. After a merge their
///   branch may or may not still exist locally; if it does, treating it
///   as the base lets the user opt into stacked chains by keeping
///   merged branches around. The cut step degrades gracefully when the
///   branch isn't there.
pub fn resolve_base_branch(project: &Project, task: &Task, all_tasks: &[TaskFile]) -> String {
    for dep_id in &task.depends_on {
        if let Some(dep) = all_tasks.iter().find(|tf| tf.task.id == *dep_id) {
            if let Some(b) = dep.task.branch.as_deref() {
                if !b.trim().is_empty() {
                    return b.to_string();
                }
            }
        }
    }
    project.base_branch().to_string()
}

/// Idempotently cut `branch` off `base` in the project's hub workdir.
///
/// Behavior:
/// - Branch already exists on hub → success, no-op.
/// - Branch missing, base exists on hub → `git branch <branch> <base>`.
/// - Branch missing, base missing → [`Error::Other`] naming both refs so
///   the caller can surface a clear "this dep hasn't been pushed yet"
///   message. The transition that triggered the cut is the right place
///   to abort: silently dropping back to `main` would lose the
///   depends_on intent without the user noticing.
pub fn cut_branch_on_hub(project: &Project, branch: &str, base: &str) -> Result<()> {
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();
    if local_branch_exists(&host, &wt, branch)? {
        return Ok(());
    }
    if !local_branch_exists(&host, &wt, base)? {
        return Err(Error::Other(format!(
            "branch-cut: cannot cut `{branch}` because base `{base}` does not exist \
             on the hub repo at `{wt}` (push the dep's branch first, or set \
             `branch:` on the task to point at an existing ref)"
        )));
    }
    let out = run_in_dir(&host, &wt, &["git", "branch", branch, base])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt} branch {branch} {base}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Prepare a task for the `Todo -> InProgress` transition: pick a
/// branch name, resolve a base, cut the branch on hub, and persist the
/// branch onto the task file.
///
/// Returns the updated [`TaskFile`] so callers can read the post-cut
/// state without a second `load_task`. Idempotent: re-running on a task
/// whose branch already exists is a clean success — the cut step is a
/// no-op and the save is skipped when nothing changed.
///
/// This is the single entry point CLI/TUI move handlers and `task
/// start` should call before the column actually flips. Failing here
/// must abort the move (so the task file's `branch:` and the on-disk
/// git refs stay in sync); callers translate the error into their own
/// surface.
pub fn ensure_branch_for_in_progress(project: &Project, task_id: &str) -> Result<TaskFile> {
    let mut tf = shelbi_state::load_task(&project.name, task_id)?;
    let all_tasks = shelbi_state::list_tasks(&project.name)?;
    let branch = branch_name_for_task(&tf.task);
    let base = resolve_base_branch(project, &tf.task, &all_tasks);
    cut_branch_on_hub(project, &branch, &base)?;
    if tf.task.branch.as_deref() != Some(branch.as_str()) {
        tf.task.branch = Some(branch);
        tf.task.updated_at = chrono::Utc::now();
        shelbi_state::save_task(&project.name, &tf.task, &tf.body)?;
    }
    Ok(tf)
}

// ---------------------------------------------------------------------------
// Internal git helpers

fn local_branch_exists(host: &Host, wt: &str, branch: &str) -> Result<bool> {
    let ref_name = format!("refs/heads/{branch}");
    let out = run_in_dir(host, wt, &["git", "rev-parse", "--verify", "--quiet", &ref_name])?;
    Ok(out.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use shelbi_core::{
        AgentRunnerSpec, Column, GitConfig, HeartbeatConfig, Machine, MachineKind,
        OrchestratorSpec, WorkspaceSpec, ZenConfig,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::Command;

    use crate::test_lock;

    fn fresh_home(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-lifecycle-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn task_with(id: &str, column: Column, branch: Option<&str>, deps: &[&str]) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: branch.map(|s| s.to_string()),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        }
    }

    fn tf_with(task: Task) -> TaskFile {
        TaskFile {
            task,
            body: String::new(),
        }
    }

    fn project_at(repo: &std::path::Path, base_branch: Option<&str>) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
            },
        );
        Project {
            name: "lifecycle-test".into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.to_path_buf(),
                host: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![WorkspaceSpec {
                name: "alice".into(),
                machine: "hub".into(),
                runner: "claude".into(),
            }],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
            git: GitConfig {
                base_branch: base_branch.map(String::from),
                ..Default::default()
            },
        }
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git").current_dir(cwd).args(args).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    fn fixture_repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        run_git(&repo, &["init", "-q", "-b", "main", "."]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        (tmp, repo)
    }

    fn branch_exists(repo: &std::path::Path, branch: &str) -> bool {
        Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
            .status()
            .unwrap()
            .success()
    }

    // ----- branch_name_for_task ----------------------------------------

    #[test]
    fn branch_name_defaults_to_shelbi_slash_id_when_unset() {
        let t = task_with("fix-login", Column::Todo, None, &[]);
        assert_eq!(branch_name_for_task(&t), "shelbi/fix-login");
    }

    #[test]
    fn branch_name_honors_existing_value() {
        // Pre-set branch is the "release task" pattern from Plans/workflows.md
        // §12: the user pins a specific ref instead of accepting the default.
        let t = task_with("release", Column::Todo, Some("release/v1.2"), &[]);
        assert_eq!(branch_name_for_task(&t), "release/v1.2");
    }

    // ----- resolve_base_branch -----------------------------------------

    #[test]
    fn resolve_base_falls_back_to_project_base_when_no_deps() {
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let t = task_with("a", Column::Todo, None, &[]);
        assert_eq!(resolve_base_branch(&p, &t, &[]), "main");
    }

    #[test]
    fn resolve_base_uses_first_dep_with_branch() {
        // Chain shape: A is in progress with branch `shelbi/a`; B depends
        // on A. B's base should be `shelbi/a`, not main.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::InProgress, Some("shelbi/a"), &[]);
        let candidate = task_with("b", Column::Todo, None, &["a"]);
        let all = vec![tf_with(dep_a)];
        assert_eq!(resolve_base_branch(&p, &candidate, &all), "shelbi/a");
    }

    #[test]
    fn resolve_base_skips_deps_without_branch() {
        // dep `a` exists but has no branch yet (still in Backlog) — keep
        // walking and pick the next dep that does. A real shape: the user
        // declared two deps to mean "any of these"; the first to be cut
        // wins.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::Backlog, None, &[]);
        let dep_b = task_with("b", Column::InProgress, Some("shelbi/b"), &[]);
        let candidate = task_with("c", Column::Todo, None, &["a", "b"]);
        let all = vec![tf_with(dep_a), tf_with(dep_b)];
        assert_eq!(resolve_base_branch(&p, &candidate, &all), "shelbi/b");
    }

    #[test]
    fn resolve_base_falls_back_when_dep_lookup_fails_in_all_tasks() {
        // Defensive: a dep id that's not in `all_tasks` (e.g. someone
        // deleted the task file mid-flight) must not crash. Fall back to
        // project base — the validate_depends_on path would reject this
        // at save time, so the fallback only kicks in for corrupted
        // states.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let candidate = task_with("orphan", Column::Todo, None, &["ghost"]);
        assert_eq!(resolve_base_branch(&p, &candidate, &[]), "main");
    }

    #[test]
    fn resolve_base_honors_git_block_override_in_fallback() {
        // Project sets `git.base_branch: develop`; the no-dep fallback
        // must hit `Project::base_branch()`, not the top-level
        // `default_branch`.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, Some("develop"));
        let t = task_with("a", Column::Todo, None, &[]);
        assert_eq!(resolve_base_branch(&p, &t, &[]), "develop");
    }

    #[test]
    fn resolve_base_treats_blank_dep_branch_as_unset() {
        // A whitespace-only `branch:` is meaningless — skip it.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::InProgress, Some("   "), &[]);
        let candidate = task_with("b", Column::Todo, None, &["a"]);
        let all = vec![tf_with(dep_a)];
        assert_eq!(resolve_base_branch(&p, &candidate, &all), "main");
    }

    // ----- cut_branch_on_hub -------------------------------------------

    #[test]
    fn cut_branch_creates_off_base_when_missing() {
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        cut_branch_on_hub(&p, "shelbi/feat", "main").unwrap();
        assert!(branch_exists(&repo, "shelbi/feat"));
    }

    #[test]
    fn cut_branch_is_idempotent() {
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        cut_branch_on_hub(&p, "shelbi/feat", "main").unwrap();
        // Second call is a no-op success — and explicitly does NOT
        // re-cut, so the branch's HEAD doesn't move underneath the
        // workspace. We verify that by advancing `main` first, then
        // re-cutting, then confirming the feature branch is still at
        // the original commit.
        let head_before = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "shelbi/feat"])
            .output()
            .unwrap();
        std::fs::write(repo.join("README.md"), "second\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "-q", "-m", "advance main"]);
        cut_branch_on_hub(&p, "shelbi/feat", "main").unwrap();
        let head_after = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "shelbi/feat"])
            .output()
            .unwrap();
        assert_eq!(head_before.stdout, head_after.stdout);
    }

    #[test]
    fn cut_branch_errors_when_base_missing() {
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        // No `shelbi/dep` branch in the repo — the cut must surface
        // a recognizable error rather than silently falling back to
        // main.
        let err = cut_branch_on_hub(&p, "shelbi/dependent", "shelbi/dep").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("shelbi/dep"), "msg: {msg}");
        assert!(msg.contains("base"), "msg: {msg}");
        assert!(!branch_exists(&repo, "shelbi/dependent"));
    }

    // ----- ensure_branch_for_in_progress -------------------------------

    fn write_task(home: &std::path::Path, project: &str, task: &Task, body: &str) {
        std::env::set_var("SHELBI_HOME", home);
        shelbi_state::save_task(project, task, body).unwrap();
    }

    #[test]
    fn ensure_cuts_and_persists_branch_for_unbranched_task() {
        let _g = test_lock::acquire();
        let home = fresh_home("ensure-cut");
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);

        write_task(&home, &p.name, &task_with("solo", Column::Todo, None, &[]), "");

        let tf = ensure_branch_for_in_progress(&p, "solo").unwrap();
        assert_eq!(tf.task.branch.as_deref(), Some("shelbi/solo"));
        assert!(branch_exists(&repo, "shelbi/solo"));

        // Reload from disk to confirm persistence — not just an
        // in-memory change.
        let reloaded = shelbi_state::load_task(&p.name, "solo").unwrap();
        assert_eq!(reloaded.task.branch.as_deref(), Some("shelbi/solo"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_uses_dep_branch_as_base_for_chained_task() {
        let _g = test_lock::acquire();
        let home = fresh_home("ensure-chain");
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);

        // Dep `a` exists on disk with its branch already cut (typical
        // shape for an in-progress dep).
        run_git(&repo, &["branch", "shelbi/a", "main"]);
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_task(
            &p.name,
            &task_with("a", Column::InProgress, Some("shelbi/a"), &[]),
            "",
        )
        .unwrap();
        shelbi_state::save_task(
            &p.name,
            &task_with("b", Column::Todo, None, &["a"]),
            "",
        )
        .unwrap();

        // Advance `shelbi/a` so we can verify B's branch is cut off
        // *that* commit, not off main.
        run_git(&repo, &["checkout", "-q", "shelbi/a"]);
        std::fs::write(repo.join("a.txt"), "from a\n").unwrap();
        run_git(&repo, &["add", "a.txt"]);
        run_git(&repo, &["commit", "-q", "-m", "a's work"]);
        let a_sha = String::from_utf8(
            Command::new("git")
                .current_dir(&repo)
                .args(["rev-parse", "shelbi/a"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let a_sha = a_sha.trim();
        run_git(&repo, &["checkout", "-q", "main"]);

        let tf = ensure_branch_for_in_progress(&p, "b").unwrap();
        assert_eq!(tf.task.branch.as_deref(), Some("shelbi/b"));
        assert!(branch_exists(&repo, "shelbi/b"));

        let b_sha = String::from_utf8(
            Command::new("git")
                .current_dir(&repo)
                .args(["rev-parse", "shelbi/b"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(b_sha.trim(), a_sha, "shelbi/b must be cut at shelbi/a's HEAD");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_is_idempotent_when_branch_already_persisted_and_cut() {
        let _g = test_lock::acquire();
        let home = fresh_home("ensure-idem");
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);

        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_task(
            &p.name,
            &task_with("a", Column::Todo, Some("shelbi/a"), &[]),
            "",
        )
        .unwrap();
        run_git(&repo, &["branch", "shelbi/a", "main"]);

        let before_mtime = std::fs::metadata(
            shelbi_state::task_path(&p.name, "a").unwrap(),
        )
        .unwrap()
        .modified()
        .unwrap();

        // Sleep so a write would produce a fresh mtime — we're proving
        // the save is skipped, not just that it happened too fast to
        // notice.
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure_branch_for_in_progress(&p, "a").unwrap();

        let after_mtime = std::fs::metadata(
            shelbi_state::task_path(&p.name, "a").unwrap(),
        )
        .unwrap()
        .modified()
        .unwrap();
        assert_eq!(
            before_mtime, after_mtime,
            "no-op ensure must not rewrite the task file"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_surfaces_missing_base_error_with_dep_branch_name() {
        // Dep declared with a branch that doesn't actually exist on the
        // hub yet (e.g. an SSH workspace that hasn't pushed). The cut must
        // refuse rather than silently rebase onto main and pretend
        // depends_on was satisfied.
        let _g = test_lock::acquire();
        let home = fresh_home("ensure-missing-base");
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);

        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_task(
            &p.name,
            &task_with("a", Column::InProgress, Some("shelbi/a"), &[]),
            "",
        )
        .unwrap();
        shelbi_state::save_task(
            &p.name,
            &task_with("b", Column::Todo, None, &["a"]),
            "",
        )
        .unwrap();
        // No `shelbi/a` branch in the repo — the cut for b must fail.
        let err = match ensure_branch_for_in_progress(&p, "b") {
            Ok(_) => panic!("expected error when dep's branch doesn't exist on hub"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("shelbi/a"), "msg: {msg}");
        assert!(!branch_exists(&repo, "shelbi/b"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
