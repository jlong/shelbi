//! Branch-cut lifecycle for the `Todo -> InProgress` transition.
//!
//! When a task moves into `InProgress` we cut its feature branch on the
//! hub workdir, idempotently, and persist the branch name back onto the
//! task. The base of the cut is **dependency-aware** and resolves by the
//! dep's status:
//!
//! - `Done` deps: their work is already on `main`, so we skip them and
//!   fall back to the project base. Preserving the historical branch
//!   relationship would fail as soon as the merged branch is deleted on
//!   the hub.
//! - `InProgress` / `Review` deps with a live branch: stack on top of
//!   that branch, so a chain `A -> B -> C` builds one branch on top of
//!   the other.
//! - `Backlog` / `Todo` deps: refuse the cut and name the blocking
//!   deps. The dep hasn't been started, so there's no branch to stack
//!   on and silently falling back to `main` would strip the depends_on
//!   intent.
//!
//! With no active dep branch (all deps done, or no deps at all) the cut
//! falls back to the project-level base from [`Project::base_branch`].
//!
//! This module wraps `shelbi_state::move_task` for both the CLI
//! (`shelbi task move`) and the TUI (kanban left/right). `shelbi task
//! start` also routes through here so the same branch ends up persisted
//! whether the user did `move` then `start` or `start` straight from
//! `todo`. The cut runs against the project's *hub* workdir
//! (`crate::git::locate_hub_workdir`); for a hub-local workspace that's
//! the same repo `sync_worktree` later reads, so the next `git worktree
//! add` sees the branch already in place. SSH workspaces still inherit the
//! `sync_worktree` fallback when the resolved base isn't visible there —
//! that path fetches `origin/<default>` on the remote machine and cuts
//! from the freshly-fetched ref (never a possibly-stale local one). A
//! depends_on chain across machines is out of scope for this pass.

use shelbi_core::{Error, Host, Project, Result, StatusCategory, Task};
use shelbi_state::TaskFile;

use crate::branch;
use crate::git::{locate_hub_workdir, run_in_dir};

/// Resolve the base branch a task's feature branch should be cut from.
///
/// The contract walks `task.depends_on` in declaration order and
/// dispatches by each dep's column:
///
/// - `Done`: skip. The dep's work is already on the project base
///   branch, and its feature branch has likely been merged and deleted
///   on the hub — treating it as a base would produce the "base does
///   not exist" error described in the bug report.
/// - `InProgress` / `Review` with a non-empty `branch:` field: the
///   first such dep wins and its branch becomes the base, so a chain
///   stacks correctly.
/// - `Backlog` / `Todo`: collected as blockers. If any dep is in this
///   state we return [`Error::Other`] naming every blocking dep, so the
///   caller can tell the user which dep to start first. Silently
///   falling back to the project base would strip the depends_on
///   intent.
///
/// If nothing blocks and no active dep hands us a branch, fall back to
/// [`Project::base_branch`]. Unknown dep ids (a task file deleted
/// mid-flight) are treated defensively as skips — `validate_depends_on`
/// rejects unknown ids at save time, so this only kicks in for corrupt
/// state.
pub fn resolve_base_branch(
    project: &Project,
    task: &Task,
    all_tasks: &[TaskFile],
) -> Result<String> {
    let mut active_base: Option<String> = None;
    let mut blocking: Vec<String> = Vec::new();
    for dep_id in &task.depends_on {
        let Some(dep) = all_tasks.iter().find(|tf| tf.task.id == *dep_id) else {
            continue;
        };
        // Key off the dep's semantic category, not a fixed column variant,
        // so a workflow that renames its active/handoff status still hands
        // us a branch. A terminal `done` dep is satisfied; an `archived`
        // (e.g. canceled) dep can never complete, so — like a not-yet-done
        // backlog/ready dep — it blocks (consistent with [`Task::is_blocked`]).
        match dep.task.column.category() {
            StatusCategory::Done => {}
            StatusCategory::Active | StatusCategory::Handoff => {
                if active_base.is_none() {
                    if let Some(b) = dep.task.branch.as_deref() {
                        let b = b.trim();
                        if !b.is_empty() {
                            active_base = Some(b.to_string());
                        }
                    }
                }
            }
            StatusCategory::Backlog | StatusCategory::Ready | StatusCategory::Archived => {
                blocking.push(dep_id.clone());
            }
        }
    }
    if !blocking.is_empty() {
        return Err(Error::Other(format!(
            "branch-cut: cannot cut branch for `{task_id}` because dep(s) not yet started: \
             {list} (start the dep(s) first, or remove them from `depends_on`)",
            task_id = task.id,
            list = blocking.join(", "),
        )));
    }
    Ok(active_base.unwrap_or_else(|| project.base_branch().to_string()))
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
    let workflow = shelbi_state::load_task_workflow(&project.name, project, &tf.task)
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    let branch = branch::branch_name_for_task(project, Some(&workflow), &tf.task)?;
    let base = resolve_base_branch(project, &tf.task, &all_tasks)?;
    cut_branch_on_hub(project, &branch, &base)?;
    if tf.task.branch.as_deref() != Some(branch.as_str()) {
        // Targeted, locked set-branch instead of writing the whole task back
        // from a stale read: a concurrent writer that touched another field
        // between our `load_task` and here would otherwise be clobbered
        // (lost update on `updated_at`/column/priority). Reload afterward so
        // the returned `TaskFile` reflects what actually landed on disk.
        shelbi_state::set_task_branch(&project.name, task_id, &branch)?;
        tf = shelbi_state::load_task(&project.name, task_id)?;
    }
    Ok(tf)
}

// ---------------------------------------------------------------------------
// Internal git helpers

fn local_branch_exists(host: &Host, wt: &str, branch: &str) -> Result<bool> {
    let ref_name = format!("refs/heads/{branch}");
    let out = run_in_dir(
        host,
        wt,
        &["git", "rev-parse", "--verify", "--quiet", &ref_name],
    )?;
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
                prompt_injection: None,
                dialog_signatures: vec![],
            },
        );
        Project {
            name: "lifecycle-test".into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.to_path_buf(),
                host: None,
                tags: Vec::new(),
                forward: None,
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
                tags: Vec::new(),
                slot: None,
            }],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: GitConfig {
                base_branch: base_branch.map(String::from),
                branch_prefix: Some("shelbi".into()),
                ..Default::default()
            },
        }
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .status()
            .unwrap();
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
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    // ----- branch_name_for_task ----------------------------------------

    // ----- resolve_base_branch -----------------------------------------

    #[test]
    fn resolve_base_falls_back_to_project_base_when_no_deps() {
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let t = task_with("a", Column::todo(), None, &[]);
        assert_eq!(resolve_base_branch(&p, &t, &[]).unwrap(), "main");
    }

    #[test]
    fn resolve_base_uses_first_active_dep_branch() {
        // Chain shape: A is in progress with branch `shelbi/a`; B depends
        // on A. B's base should be `shelbi/a`, not main.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::in_progress(), Some("shelbi/a"), &[]);
        let candidate = task_with("b", Column::todo(), None, &["a"]);
        let all = vec![tf_with(dep_a)];
        assert_eq!(
            resolve_base_branch(&p, &candidate, &all).unwrap(),
            "shelbi/a"
        );
    }

    #[test]
    fn resolve_base_uses_review_dep_branch() {
        // A review-column dep still owns a live branch that hasn't
        // landed on main yet — stack B on top of it, same as InProgress.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::review(), Some("shelbi/a"), &[]);
        let candidate = task_with("b", Column::todo(), None, &["a"]);
        let all = vec![tf_with(dep_a)];
        assert_eq!(
            resolve_base_branch(&p, &candidate, &all).unwrap(),
            "shelbi/a"
        );
    }

    #[test]
    fn resolve_base_skips_done_deps_even_when_branch_still_set() {
        // Bug repro: A merged and its branch was deleted on the hub, but
        // the task file's `branch:` field is still populated. Old
        // behavior used it as the base and blew up in `cut_branch_on_hub`
        // when the ref was missing. New behavior treats Done as "work is
        // on main" and falls back to project base regardless of `branch:`.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::done(), Some("shelbi/a"), &[]);
        let candidate = task_with("b", Column::todo(), None, &["a"]);
        let all = vec![tf_with(dep_a)];
        assert_eq!(resolve_base_branch(&p, &candidate, &all).unwrap(), "main");
    }

    #[test]
    fn resolve_base_prefers_active_dep_when_mixed_with_done() {
        // `depends_on: [done-a, in-progress-b]` — done-a is on main,
        // in-progress-b has a live branch. Pick b's branch so the child
        // stacks correctly.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::done(), Some("shelbi/a"), &[]);
        let dep_b = task_with("b", Column::in_progress(), Some("shelbi/b"), &[]);
        let candidate = task_with("c", Column::todo(), None, &["a", "b"]);
        let all = vec![tf_with(dep_a), tf_with(dep_b)];
        assert_eq!(
            resolve_base_branch(&p, &candidate, &all).unwrap(),
            "shelbi/b"
        );
    }

    #[test]
    fn resolve_base_refuses_when_any_dep_is_in_backlog() {
        // Dep in Backlog has no branch yet. Silently falling back to
        // main would strip the depends_on intent, so refuse.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::backlog(), None, &[]);
        let candidate = task_with("b", Column::todo(), None, &["a"]);
        let all = vec![tf_with(dep_a)];
        let err = resolve_base_branch(&p, &candidate, &all).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`b`"), "msg: {msg}");
        assert!(msg.contains("not yet started: a"), "msg: {msg}");
    }

    #[test]
    fn resolve_base_refuses_when_any_dep_is_in_todo() {
        // Same guard as backlog — a Todo dep hasn't been started, so no
        // branch to stack on. Even if another dep is InProgress with a
        // usable branch, the Todo dep is a hard blocker.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("todo-dep", Column::todo(), None, &[]);
        let dep_b = task_with("b", Column::in_progress(), Some("shelbi/b"), &[]);
        let candidate = task_with("c", Column::todo(), None, &["todo-dep", "b"]);
        let all = vec![tf_with(dep_a), tf_with(dep_b)];
        let err = resolve_base_branch(&p, &candidate, &all).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not yet started: todo-dep"), "msg: {msg}");
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
        let candidate = task_with("orphan", Column::todo(), None, &["ghost"]);
        assert_eq!(resolve_base_branch(&p, &candidate, &[]).unwrap(), "main");
    }

    #[test]
    fn resolve_base_honors_git_block_override_in_fallback() {
        // Project sets `git.base_branch: develop`; the no-dep fallback
        // must hit `Project::base_branch()`, not the top-level
        // `default_branch`.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, Some("develop"));
        let t = task_with("a", Column::todo(), None, &[]);
        assert_eq!(resolve_base_branch(&p, &t, &[]).unwrap(), "develop");
    }

    #[test]
    fn resolve_base_treats_blank_dep_branch_as_unset() {
        // A whitespace-only `branch:` on an active dep is meaningless.
        // Nothing else in depends_on means no blocker either — fall back
        // to project base rather than using whitespace as a ref name.
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);
        let dep_a = task_with("a", Column::in_progress(), Some("   "), &[]);
        let candidate = task_with("b", Column::todo(), None, &["a"]);
        let all = vec![tf_with(dep_a)];
        assert_eq!(resolve_base_branch(&p, &candidate, &all).unwrap(), "main");
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

        write_task(
            &home,
            &p.name,
            &task_with("solo", Column::todo(), None, &[]),
            "",
        );

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
            &task_with("a", Column::in_progress(), Some("shelbi/a"), &[]),
            "",
        )
        .unwrap();
        shelbi_state::save_task(&p.name, &task_with("b", Column::todo(), None, &["a"]), "")
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
        assert_eq!(
            b_sha.trim(),
            a_sha,
            "shelbi/b must be cut at shelbi/a's HEAD"
        );

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
            &task_with("a", Column::todo(), Some("shelbi/a"), &[]),
            "",
        )
        .unwrap();
        run_git(&repo, &["branch", "shelbi/a", "main"]);

        let before_mtime = std::fs::metadata(shelbi_state::task_path(&p.name, "a").unwrap())
            .unwrap()
            .modified()
            .unwrap();

        // Sleep so a write would produce a fresh mtime — we're proving
        // the save is skipped, not just that it happened too fast to
        // notice.
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure_branch_for_in_progress(&p, "a").unwrap();

        let after_mtime = std::fs::metadata(shelbi_state::task_path(&p.name, "a").unwrap())
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
            &task_with("a", Column::in_progress(), Some("shelbi/a"), &[]),
            "",
        )
        .unwrap();
        shelbi_state::save_task(&p.name, &task_with("b", Column::todo(), None, &["a"]), "")
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

    #[test]
    fn ensure_cuts_from_main_when_done_dep_branch_was_deleted() {
        // Bug repro: dep A is `done` (its PR merged and the hub deleted
        // the branch). Dep A's task file still has `branch: shelbi/a`.
        // Starting B (which depends on A) must succeed by cutting off
        // the project base, not blow up because `shelbi/a` is gone.
        let _g = test_lock::acquire();
        let home = fresh_home("ensure-done-dep");
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);

        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_task(
            &p.name,
            &task_with("a", Column::done(), Some("shelbi/a"), &[]),
            "",
        )
        .unwrap();
        shelbi_state::save_task(&p.name, &task_with("b", Column::todo(), None, &["a"]), "")
            .unwrap();
        // Deliberately: no `shelbi/a` branch in the repo. Simulates the
        // post-merge state where the dep's branch was deleted from the
        // hub.
        assert!(!branch_exists(&repo, "shelbi/a"));

        let tf = ensure_branch_for_in_progress(&p, "b").unwrap();
        assert_eq!(tf.task.branch.as_deref(), Some("shelbi/b"));
        assert!(branch_exists(&repo, "shelbi/b"));

        // And confirm shelbi/b's HEAD is at main's HEAD — proving the
        // cut base was main, not some ghost of the deleted branch.
        let main_sha = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "main"])
            .output()
            .unwrap()
            .stdout;
        let b_sha = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "shelbi/b"])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(main_sha, b_sha, "shelbi/b must be cut at main's HEAD");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_refuses_when_dep_is_still_in_todo() {
        // Dep A hasn't been started; B depends on A. The cut must refuse
        // and name A rather than silently falling back to main.
        let _g = test_lock::acquire();
        let home = fresh_home("ensure-todo-dep");
        let (_tmp, repo) = fixture_repo();
        let p = project_at(&repo, None);

        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_task(&p.name, &task_with("a", Column::todo(), None, &[]), "").unwrap();
        shelbi_state::save_task(&p.name, &task_with("b", Column::todo(), None, &["a"]), "")
            .unwrap();

        let err = match ensure_branch_for_in_progress(&p, "b") {
            Ok(_) => panic!("expected error when dep is still in todo"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(msg.contains("`b`"), "msg: {msg}");
        assert!(msg.contains("not yet started: a"), "msg: {msg}");
        assert!(!branch_exists(&repo, "shelbi/b"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
