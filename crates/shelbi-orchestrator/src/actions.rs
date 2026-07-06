//! Workflow action primitives — `push_branch`, `open_pr`, `merge`,
//! `close_pr`, `delete_branch`, `restack`.
//!
//! Each function does one git/gh thing the workflow `transitions:` block
//! can name (see Plans/workflows.md, "Action set"). They are deliberately
//! single-purpose so the workflow engine — and a human at the CLI — can
//! sequence them per the active workflow without the primitive deciding
//! what should run next. The one exception is `merge`, which auto-fires
//! `restack` on every not-`Done` child task that lists the merging task
//! in its `depends_on:` — stacked workflows are only coherent if the
//! cascade happens atomically with the parent's merge, so we bake it in
//! rather than asking every workflow YAML to declare it.
//!
//! All actions are idempotent and silently no-op when not applicable:
//!
//! - `push_branch` pushes the task's branch from the workspace's worktree.
//!   Pushing an up-to-date branch reports `Everything up-to-date` and
//!   still succeeds.
//! - `open_pr` opens a PR for the task's branch. If one is already open,
//!   returns its number unchanged. The base branch is picked by a fallback
//!   chain — see [`open_pr`].
//! - `merge` integrates the task's branch into the target branch using the
//!   project's configured merge strategy. Two paths: if a PR is open, runs
//!   `gh pr merge --<strategy>`; otherwise the hub fetches from origin and
//!   performs `git merge --<strategy>` in a throwaway temp worktree, then
//!   pushes the result to `origin/<target>` — the hub's own checkout never
//!   moves. After the integration commit lands, fires `restack` on every
//!   not-`Done` child that depends on this task. See [`merge`].
//! - `close_pr` closes any *open* PR for the task's branch; with no open
//!   PR it returns `None` instead of erroring.
//! - `delete_branch` removes the branch from origin and from the hub's
//!   local refs. Skipped when a workspace still has it checked out so we
//!   don't yank a branch out from under an active task.
//! - `restack` rewrites a child task's branch onto a new base — typically
//!   fired after the parent task's branch was merged — and retargets its
//!   open PR (if any). See [`restack`].
//!
//! `push_branch` and `open_pr` run against the workspace's worktree (that's
//! where the branch lives, and `gh pr create` needs a remote-tracking
//! branch to associate with). `merge`, `close_pr`, and `delete_branch`
//! run on the hub — by the time the orchestrator is integrating or
//! cleaning up a branch the branch is on origin, so gh / git from any
//! hub checkout work fine.

use shelbi_core::{validate_branch, Column, Error, Host, MergeStrategy, Project, Result, Task};

use crate::git::{
    compose_pr_body, head_commit_subject, locate_hub_workdir, locate_workspace_worktree,
    lookup_open_pr, lookup_pr_base, parse_pr_number_from_url, run_in_dir,
    wait_for_merge_commit_sha,
};
use crate::workspace::workspace_worktree;

/// Outcome of [`delete_branch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// Branch was removed from at least one of (origin, hub local).
    Deleted,
    /// A workspace still has the branch checked out; nothing was touched.
    /// Per the workflow spec, the branch will be replaced naturally on
    /// that workspace's next dispatch.
    Skipped { reason: String },
    /// Branch wasn't present in either location — there was nothing to do.
    NotPresent,
}

impl DeleteOutcome {
    /// Single-line wire format printed on stdout by `shelbi action
    /// delete-branch`. Prefix-keyed so a caller can match on
    /// `deleted` / `skipped:` / `not-present` without parsing JSON.
    pub fn as_line(&self) -> String {
        match self {
            DeleteOutcome::Deleted => "deleted".to_string(),
            DeleteOutcome::Skipped { reason } => {
                let safe = reason.replace('\n', " ");
                format!("skipped:{safe}")
            }
            DeleteOutcome::NotPresent => "not-present".to_string(),
        }
    }
}

/// Outcome of [`merge`]. Tells the caller which path actually ran so a
/// follow-on action (or the user staring at stdout) can tell whether the
/// integration went through GitHub or stayed entirely on the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// An open PR for the task's branch was found and merged via
    /// `gh pr merge --<strategy>`. GitHub picked the merge commit SHA.
    /// `sha` is `None` when GitHub reported the PR merged but hadn't
    /// recorded the merge commit yet after our polling window (merge
    /// queues, busy repos) — the merge itself succeeded.
    ViaPr { pr: u64, sha: Option<String> },
    /// No PR was open. The hub fetched the branch from origin, ran
    /// `git merge --<strategy>` against `origin/<target>` in a throwaway
    /// temp worktree, and pushed the result to `origin/<target>`. The
    /// SHA is the resulting remote tip of `target`; the hub work_dir's
    /// own checkout is never touched.
    HubSide { sha: String, target: String },
}

impl MergeOutcome {
    /// Single-line wire format printed on stdout by `shelbi action merge`.
    /// Prefix-keyed (`pr:` / `hub:`) so a caller can tell the two paths
    /// apart without parsing JSON. A ViaPr merge whose SHA GitHub hasn't
    /// recorded yet prints the literal token `sha-pending` in the SHA
    /// slot.
    pub fn as_line(&self) -> String {
        match self {
            MergeOutcome::ViaPr { pr, sha } => {
                format!("pr:{pr}:{}", sha.as_deref().unwrap_or("sha-pending"))
            }
            MergeOutcome::HubSide { sha, target } => format!("hub:{target}:{sha}"),
        }
    }

    /// SHA of the integration commit, regardless of which path took it
    /// there. The caller usually wants this to log the merge or to feed
    /// a follow-on `delete_branch` / restack pass. `None` only for a
    /// ViaPr merge whose SHA GitHub hadn't recorded yet.
    pub fn sha(&self) -> Option<&str> {
        match self {
            MergeOutcome::ViaPr { sha, .. } => sha.as_deref(),
            MergeOutcome::HubSide { sha, .. } => Some(sha),
        }
    }
}

/// Bundled return shape for [`merge`]: the integration commit on top, plus
/// one [`RestackOutcome`] per child task that depends on the merged task.
/// Children appear in `shelbi_state::list_tasks` order so the wire format
/// is deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeResult {
    pub merge: MergeOutcome,
    pub restacks: Vec<RestackOutcome>,
}

/// Outcome of a single [`restack`] pass against one child task. The two
/// variants split "we rewrote the branch" from "we left it alone" so the
/// caller (and a human at the CLI) can tell at a glance whether anything
/// moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestackOutcome {
    /// Child branch was rebased onto `new_base` and force-pushed. `sha`
    /// is the resulting tip of the branch on origin. `retargeted_pr` is
    /// `Some(n)` when an open PR existed and was retargeted to `new_base`
    /// in the same pass, `None` otherwise.
    Restacked {
        task_id: String,
        branch: String,
        new_base: String,
        sha: String,
        retargeted_pr: Option<u64>,
    },
    /// Nothing was rewritten. `reason` is a short token-style label
    /// (`held-by-<workspace>`, `no-branch`, `already-restacked`,
    /// `restack-deferred:waiting-on=<ids>`,
    /// `no-commits-beyond-from-base`, `rebase-conflict`, …) so a caller
    /// can match on a prefix without parsing free-form text.
    Skipped { task_id: String, reason: String },
}

impl RestackOutcome {
    /// Single-line wire format printed on stdout by `shelbi action restack`
    /// and by `shelbi action merge` (one line per auto-fired child).
    /// Prefix-keyed (`restacked:` / `skipped:`) so a caller can dispatch on
    /// the first colon without parsing the rest.
    pub fn as_line(&self) -> String {
        match self {
            RestackOutcome::Restacked {
                task_id,
                branch,
                new_base,
                sha,
                retargeted_pr,
            } => {
                let pr = retargeted_pr
                    .map(|p| format!(" pr={p}"))
                    .unwrap_or_default();
                format!("restacked:{task_id}:{branch}:{new_base}:{sha}{pr}")
            }
            RestackOutcome::Skipped { task_id, reason } => {
                let safe = reason.replace('\n', " ");
                format!("skipped:{task_id}:{safe}")
            }
        }
    }
}

/// Push the task's branch from the workspace's worktree to `origin`.
///
/// Errors when the task has no assigned workspace or no `branch` field — both
/// are caller bugs (the workflow contract guarantees both fields by the
/// time this fires). Re-pushing an up-to-date branch is a clean success.
pub fn push_branch(project: &Project, task: &Task) -> Result<()> {
    let branch = require_branch(task)?;
    let (host, worktree) = locate_workspace_worktree(project, task)?;
    let wt = worktree.to_string_lossy().into_owned();

    let out = run_in_dir(&host, &wt, &["git", "push", "-u", "origin", "--", &branch])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt} push -u origin -- {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Open a PR for the task's branch. Idempotent — if an open PR for the
/// branch already exists, returns its number instead of opening a second
/// one.
///
/// The PR's base branch is picked by the fallback chain documented in
/// `Plans/workflows.md` §12 ("Action set", `open_pr` row, and the
/// "Dependent tasks" subsection):
///
/// 1. **`target_override`** — the per-transition `target:` value the
///    workflow engine supplies for *this* edge. Highest precedence so a
///    workflow can declare multi-hop merges (e.g. feature → develop →
///    main) without forking actions per hop.
/// 2. **Parent task's branch via `depends_on:`** — when the task lists
///    one or more parents that are not yet `Done` and carry a `branch:`
///    in their frontmatter, the PR targets the first such parent's
///    branch. This is the stacked-PR semantic the spec walks through
///    verbatim ("the PR's base is the parent task's branch — not the
///    workflow's `base_branch`"). A parent already in `Done` is skipped
///    so we don't aim at a branch the `delete_branch` action may have
///    already removed.
/// 3. **`project.base_branch()`** — the effective project base (workflow
///    `git:` override or top-level `default_branch`). Always set; the
///    unconditional fallback.
///
/// Push happens elsewhere — sequence `[push_branch, open_pr]` in the
/// workflow when the branch isn't yet on `origin`. We don't push from
/// `open_pr` so a workflow author can compose the two primitives
/// independently.
pub fn open_pr(
    project: &Project,
    project_name: &str,
    task: &Task,
    task_body: &str,
    target_override: Option<&str>,
) -> Result<u64> {
    let branch = require_branch(task)?;
    let (host, worktree) = locate_workspace_worktree(project, task)?;
    let wt = worktree.to_string_lossy().into_owned();

    // Idempotency: an open PR for this branch is the spec's "no-op if a
    // PR is already open" case. Picking `state=open` intentionally — a
    // closed/merged PR is stale; the next push warrants a fresh PR.
    if let Some(num) = lookup_open_pr(&host, &wt, &branch)? {
        return Ok(num);
    }

    let target = resolve_pr_target(project, project_name, task, target_override)?;
    let title = head_commit_subject(&host, &wt)?;
    let task_path = shelbi_state::task_path(project_name, &task.id)
        .map_err(|e| Error::Other(format!("resolve task path for `{}`: {e}", task.id)))?
        .to_string_lossy()
        .into_owned();
    let body = compose_pr_body(task_body, &task_path);

    let out = run_in_dir(
        &host,
        &wt,
        &[
            "gh", "pr", "create", "--head", &branch, "--base", &target, "--title", &title,
            "--body", &body,
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr create --head {branch} --base {target}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    parse_pr_number_from_url(stdout.trim()).ok_or_else(|| {
        Error::Other(format!(
            "gh pr create returned `{}` — couldn't parse a PR number out of it",
            stdout.trim()
        ))
    })
}

/// Run the [`open_pr`] target resolution chain. Disk lookup for parent
/// tasks is the only side-effect; nothing here pushes or talks to gh.
fn resolve_pr_target(
    project: &Project,
    project_name: &str,
    task: &Task,
    target_override: Option<&str>,
) -> Result<String> {
    Ok(resolve_pr_target_from(
        project.base_branch(),
        task,
        target_override,
        |parent_id| {
            shelbi_state::load_task(project_name, parent_id)
                .ok()
                .map(|tf| tf.task)
        },
    ))
}

/// Pure-logic core of [`resolve_pr_target`]. Splits the parent-task
/// lookup out behind a closure so the chain priorities are unit-testable
/// without a `SHELBI_HOME`.
fn resolve_pr_target_from<F>(
    project_base_branch: &str,
    task: &Task,
    target_override: Option<&str>,
    parent_lookup: F,
) -> String
where
    F: Fn(&str) -> Option<Task>,
{
    if let Some(t) = target_override {
        return t.to_string();
    }
    for parent_id in &task.depends_on {
        let Some(parent) = parent_lookup(parent_id) else {
            // Unknown parent — covered by `validate_depends_on` at save
            // time, so reaching this means an out-of-band edit. Don't
            // blow up here; fall through to the next candidate.
            continue;
        };
        if parent.column == Column::done() {
            // Parent's branch may already be gone (its Done-side
            // `delete_branch` action ran). Restack handles rewriting
            // the child's base when the parent merges, so by the time
            // open_pr fires the chain target should be the same as the
            // project base anyway. Skip the dead branch and keep
            // walking.
            continue;
        }
        if let Some(branch) = parent.branch {
            return branch;
        }
    }
    project_base_branch.to_string()
}

/// Integrate the task's branch into the target branch using the project's
/// configured [`MergeStrategy`].
///
/// Two paths share this primitive, picked by whether a PR is currently
/// open for the branch:
///
/// - **`gh pr merge` path** — when an open PR exists, the hub runs
///   `gh pr merge <pr> --<strategy>`. GitHub picks the merge commit and
///   we read the SHA back via `gh pr view --json mergeCommit` (polling —
///   GitHub records it asynchronously). The PR's own base wins; we don't
///   re-target it from `target_override` because `open_pr` was already
///   responsible for picking the right base, and the child restack
///   cascade uses that stored base (`baseRefName`) as its target.
/// - **Hub-side fetch path** — when no PR is open, the hub fetches the
///   branch (and `target`) from origin, fast-forwards `target` to
///   `origin/target`, runs `git merge --<strategy>` against `target`,
///   then pushes `target` back to origin. The hub's work_dir must be
///   clean of user changes (`.shelbi/` is ignored, the same way
///   `shelbi merge` preflight does it).
///
/// The effective target is `target_override` (the per-transition `target:`
/// from the workflow YAML) if set, else [`Project::base_branch`]. The
/// effective strategy is [`Project::merge_strategy`]. In v1 only `squash`
/// and `merge` are accepted — `rebase` is reserved for a follow-up and
/// returns a clear error rather than silently choosing one of the others.
///
/// `merge` does **not** delete the branch as a side-effect — that's
/// `delete_branch`'s job. Workflows sequence them as
/// `[merge, delete_branch]` so each action stays single-purpose and the
/// user can tweak the policy independently.
///
/// **Auto-fire: restack children.** After the integration commit lands,
/// `merge` walks every not-`Done` task that lists `task.id` in its
/// `depends_on:` and calls [`restack`] on each, passing the merged task's
/// `branch` as `from_base` and the merge `target` as `onto`. Children
/// without a branch, children whose branch is held by a live workspace, and
/// children whose branch is already based on the new target are skipped —
/// see [`RestackOutcome`]. Errors inside a single child's restack land in
/// the bundled outcome as `Skipped { reason: "restack-error:..." }` rather
/// than rolling back the parent's merge.
///
/// This piece is *not* single-purpose by design — stacked workflows are
/// only coherent if the child cascade fires atomically with the parent's
/// merge. The workflow YAML doesn't need to sequence it; `merge` owns it.
pub fn merge(
    project: &Project,
    project_name: &str,
    task: &Task,
    target_override: Option<&str>,
) -> Result<MergeResult> {
    let branch = require_branch(task)?;
    let strategy = require_supported_strategy(project.merge_strategy())?;
    let target = target_override
        .map(str::to_string)
        .unwrap_or_else(|| project.base_branch().to_string());
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();

    let (outcome, merged_target) = if let Some(pr) = lookup_open_pr(&host, &wt, &branch)? {
        // gh pr merge respects the PR's *stored* base, which can differ
        // from `target_override`/`project.base_branch()` — in a stacked
        // workflow the PR merges into a parent branch, not the project
        // base. Read `baseRefName` before merging so the children we
        // restack land on the ref GitHub actually merged into.
        let pr_base = lookup_pr_base(&host, &wt, pr)?;
        let sha = merge_via_pr(&host, &wt, pr, strategy)?;
        (MergeOutcome::ViaPr { pr, sha }, pr_base)
    } else {
        let sha = merge_hub_side(&host, &wt, &branch, &target, strategy, &task.id)?;
        (
            MergeOutcome::HubSide {
                sha,
                target: target.clone(),
            },
            target,
        )
    };

    let restacks = restack_children(project, project_name, task, &branch, &merged_target);

    Ok(MergeResult {
        merge: outcome,
        restacks,
    })
}

/// Walk every not-`Done` task in the project, restacking the ones that
/// list `parent_task.id` in their `depends_on:`. Used by [`merge`] to keep
/// stacked PR chains coherent after the parent merges.
///
/// Multi-parent children defer until every parent dependency is Done. The
/// merge that just completed counts as Done for this decision even if the
/// caller supplied a pre-move task snapshot; TUI transitions persist the
/// Done column before firing actions, while manual `shelbi action merge`
/// calls do not. Once all parents are Done, the child is restacked once
/// using the dependency branch it was most likely cut from as the rebase
/// cutoff. Errors inside a single child's restack are captured as a
/// `Skipped` outcome rather than short-circuiting the cascade — the
/// parent has already been integrated, so a child rebase conflict
/// shouldn't roll back the merge.
fn restack_children(
    project: &Project,
    project_name: &str,
    parent_task: &Task,
    parent_branch: &str,
    onto: &str,
) -> Vec<RestackOutcome> {
    let mut outcomes = Vec::new();
    let tasks = match shelbi_state::list_tasks(project_name) {
        Ok(t) => t,
        Err(e) => {
            // We can't enumerate tasks — surface a single synthetic skip
            // so the operator sees that the cascade didn't run, without
            // blowing up the merge that already succeeded.
            outcomes.push(RestackOutcome::Skipped {
                task_id: parent_task.id.clone(),
                reason: format!("list-tasks-error:{e}").replace(' ', "_"),
            });
            return outcomes;
        }
    };
    for tf in &tasks {
        let child = &tf.task;
        if child.column == Column::done() {
            continue;
        }
        if !child.depends_on.iter().any(|id| id == &parent_task.id) {
            continue;
        }
        let (from_base, deferred) =
            restack_base_for_child(child, &tasks, parent_task, parent_branch);
        if let Some(waiting_on) = deferred {
            outcomes.push(RestackOutcome::Skipped {
                task_id: child.id.clone(),
                reason: format!("restack-deferred:waiting-on={}", waiting_on.join(",")),
            });
            continue;
        }
        let id = child.id.clone();
        match restack(project, child, &from_base, Some(onto)) {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => outcomes.push(RestackOutcome::Skipped {
                task_id: id,
                reason: format!("restack-error:{e}").replace(' ', "_"),
            }),
        }
    }
    outcomes
}

fn restack_base_for_child(
    child: &Task,
    tasks: &[shelbi_state::TaskFile],
    just_merged_parent: &Task,
    just_merged_parent_branch: &str,
) -> (String, Option<Vec<String>>) {
    if child.depends_on.len() <= 1 {
        return (just_merged_parent_branch.to_string(), None);
    }

    let waiting_on = unfinished_multi_parent_deps(child, tasks, &just_merged_parent.id);
    if !waiting_on.is_empty() {
        return (just_merged_parent_branch.to_string(), Some(waiting_on));
    }

    (
        multi_parent_restack_cutoff(child, tasks)
            .unwrap_or_else(|| just_merged_parent_branch.to_string()),
        None,
    )
}

fn unfinished_multi_parent_deps(
    child: &Task,
    tasks: &[shelbi_state::TaskFile],
    just_merged_parent_id: &str,
) -> Vec<String> {
    if child.depends_on.len() <= 1 {
        return Vec::new();
    }

    child
        .depends_on
        .iter()
        .filter(|dep_id| {
            if dep_id.as_str() == just_merged_parent_id {
                return false;
            }
            tasks
                .iter()
                .find(|tf| tf.task.id == **dep_id)
                .map(|tf| tf.task.column != Column::done())
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

fn multi_parent_restack_cutoff(child: &Task, tasks: &[shelbi_state::TaskFile]) -> Option<String> {
    child.depends_on.iter().find_map(|dep_id| {
        tasks
            .iter()
            .find(|tf| tf.task.id == *dep_id)
            .and_then(|tf| tf.task.branch.as_deref())
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
            .map(str::to_string)
    })
}

/// Rewrite `child_task`'s branch onto a new base.
///
/// The intended call shape is the cascade fired by [`merge`] after a
/// parent task lands: with `from_base` set to the parent task's branch and
/// `onto_override` set to the parent's merge target, this primitive does
/// `git rebase --onto <onto> <from_base> <child_branch>` and force-pushes
/// the result back to origin. The same primitive is reachable from the
/// CLI (`shelbi action restack`) so a human can re-anchor a child branch
/// manually after an out-of-band base change.
///
/// **Resolution of `onto`.** When `onto_override` is `Some(...)`, that
/// wins. Otherwise the project's effective `base_branch()` is used. Unlike
/// [`open_pr`], we don't walk `depends_on:` for a fallback parent — by the
/// time `restack` fires, the parent on top of which we *were* stacked has
/// just merged; there's no other "stacking parent" to chain through.
///
/// **Where it runs.** The hub. Restack provisions a detached worktree off
/// `origin/<child_branch>` under the system temp dir, runs the rebase
/// there, force-pushes with `--force-with-lease`, then removes the
/// worktree. The hub's main `work_dir` is never moved off whatever branch
/// the operator left it on. The hub must therefore be local
/// ([`locate_hub_workdir`] already enforces this).
///
/// **Skips.** Restack returns a [`RestackOutcome::Skipped`] (not an error)
/// for the cases the workflow contract says are normal:
///
/// - child task has no `branch:` field;
/// - a workspace has the child branch checked out (we'd otherwise diverge
///   it from under live work — same rule as `delete_branch`);
/// - the child branch isn't on `origin` yet (`push_branch` hasn't fired);
/// - the child is already based on `onto` (`origin/<onto>` is an ancestor
///   of `origin/<child_branch>`);
/// - the child has no commits beyond `from_base` (nothing to replay);
/// - the rebase produces conflicts (we abort and leave the branch alone).
///
/// Hard errors — missing `onto` ref on origin, push failure under
/// `--force-with-lease`, `gh pr edit` failure — propagate as
/// [`Error::Command`] so a misconfiguration surfaces loudly.
pub fn restack(
    project: &Project,
    child_task: &Task,
    from_base: &str,
    onto_override: Option<&str>,
) -> Result<RestackOutcome> {
    let task_id = child_task.id.clone();
    let Some(child_branch) = child_task.branch.clone() else {
        return Ok(RestackOutcome::Skipped {
            task_id,
            reason: "no-branch".into(),
        });
    };
    // The child branch doesn't come through `require_branch`, so guard it
    // here — it's spliced into `--force-with-lease={branch}` and
    // `HEAD:{branch}`, where a crafted value becomes a git option.
    validate_branch(&child_branch).map_err(|e| Error::Other(format!("task `{task_id}`: {e}")))?;

    if let Some(workspace_name) = workspace_holding_branch(project, &child_branch)? {
        return Ok(RestackOutcome::Skipped {
            task_id,
            reason: format!("held-by-{workspace_name}"),
        });
    }

    let onto = onto_override
        .map(str::to_string)
        .unwrap_or_else(|| project.base_branch().to_string());

    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();

    // Fetch every ref we'll touch in one pass so the rebase sees their
    // current tips on origin. A missing ref aborts the fetch with a
    // non-zero exit — handle the "not on origin" cases below explicitly
    // so the error message names the action the operator should run.
    if !remote_branch_exists(&host, &wt, &child_branch)? {
        return Ok(RestackOutcome::Skipped {
            task_id,
            reason: "child-branch-not-on-origin".into(),
        });
    }
    if !remote_branch_exists(&host, &wt, from_base)? {
        return Ok(RestackOutcome::Skipped {
            task_id,
            reason: format!("from-base-not-on-origin:{from_base}"),
        });
    }
    if !remote_branch_exists(&host, &wt, &onto)? {
        return Err(Error::Other(format!(
            "restack: target branch `{onto}` is not on origin — push it first \
             or fix the workflow `target:`/project `base_branch` config"
        )));
    }
    run_or_command_err(
        &host,
        &wt,
        &[
            "git",
            "fetch",
            "origin",
            "--",
            &child_branch,
            from_base,
            &onto,
        ],
        || format!("git -C {wt} fetch origin -- {child_branch} {from_base} {onto}"),
    )?;

    let onto_ref = format!("origin/{onto}");
    let child_ref = format!("origin/{child_branch}");
    let from_ref = format!("origin/{from_base}");

    // Already-restacked guard: if the child branch already contains every
    // commit on `onto`, there's nothing for us to do. Re-running the
    // rebase would still rewrite SHAs (rebasing onto an ancestor produces
    // identical-content but different-author-date commits if dates
    // changed), which is a needless force-push from a primitive that's
    // supposed to be idempotent.
    let behind_onto = run_capture_stdout(
        &host,
        &wt,
        &[
            "git",
            "rev-list",
            "--count",
            &format!("{child_ref}..{onto_ref}"),
        ],
    )?;
    if behind_onto.trim() == "0" {
        return Ok(RestackOutcome::Skipped {
            task_id,
            reason: "already-restacked".into(),
        });
    }

    // Nothing-to-replay guard: if the child branch has zero commits past
    // `from_base`, the rebase would advance the branch tip to `onto` —
    // turning an empty stack-tip into "the target," which is almost
    // certainly not what the user wants. Surface it as a skip rather than
    // a silent fast-forward.
    let ahead_of_from = run_capture_stdout(
        &host,
        &wt,
        &[
            "git",
            "rev-list",
            "--count",
            &format!("{from_ref}..{child_ref}"),
        ],
    )?;
    if ahead_of_from.trim() == "0" {
        return Ok(RestackOutcome::Skipped {
            task_id,
            reason: "no-commits-beyond-from-base".into(),
        });
    }

    let tmp_path = unique_temp_worktree_path("restack", &task_id);
    // git worktree add refuses to overwrite an existing path. Clean up
    // any stale dir from a previous crashed restack before we re-add.
    let _ = std::fs::remove_dir_all(&tmp_path);
    let tmp = tmp_path.to_string_lossy().into_owned();

    run_or_command_err(
        &host,
        &wt,
        &["git", "worktree", "add", "--detach", &tmp, &child_ref],
        || format!("git -C {wt} worktree add --detach {tmp} {child_ref}"),
    )?;

    let rebase = run_in_dir(
        &host,
        &tmp,
        &["git", "rebase", "--onto", &onto_ref, &from_ref],
    )?;
    if !rebase.status.success() {
        // Abort the rebase so the worktree is in a clean state for the
        // remove that follows, then remove the worktree. Both are
        // best-effort — even on failure we still want to surface the
        // skip rather than tangle the cleanup with the conflict report.
        let _ = run_in_dir(&host, &tmp, &["git", "rebase", "--abort"]);
        let _ = run_in_dir(&host, &wt, &["git", "worktree", "remove", "--force", &tmp]);
        let _ = std::fs::remove_dir_all(&tmp_path);
        return Ok(RestackOutcome::Skipped {
            task_id,
            reason: "rebase-conflict".into(),
        });
    }

    let new_sha = run_capture_stdout(&host, &tmp, &["git", "rev-parse", "HEAD"])?
        .trim()
        .to_string();

    // --force-with-lease without an expected SHA uses the local copy of
    // refs/remotes/origin/<branch> as the lease. We just fetched that ref
    // above, so a race between fetch and push is the only way the lease
    // would (correctly) fail — exactly the case we want to refuse.
    let push = run_in_dir(
        &host,
        &tmp,
        &[
            "git",
            "push",
            &format!("--force-with-lease={child_branch}"),
            "origin",
            "--",
            &format!("HEAD:{child_branch}"),
        ],
    )?;
    let push_status = push.status;
    let push_stderr = String::from_utf8_lossy(&push.stderr).into_owned();

    // Tear the worktree down before bailing on a push error so we don't
    // leak it; the error is what the caller gets, regardless.
    let _ = run_in_dir(&host, &wt, &["git", "worktree", "remove", "--force", &tmp]);
    let _ = std::fs::remove_dir_all(&tmp_path);

    if !push_status.success() {
        return Err(Error::Command {
            cmd: format!(
                "git -C {tmp} push --force-with-lease={child_branch} origin HEAD:{child_branch}"
            ),
            status: push_status.to_string(),
            stderr: push_stderr,
        });
    }

    // Retargeting the PR is a best-effort follow-on: a project whose
    // origin isn't a GitHub remote (and therefore has no `gh` configured)
    // can still benefit from the rebase + push above; we don't want the
    // restack to fail end-to-end just because there's nothing to retarget.
    // Tolerate the specific gh signal for "no GitHub host" and treat it
    // as "no PR to touch."
    let retargeted_pr = match lookup_open_pr_tolerant(&host, &wt, &child_branch)? {
        Some(pr) => {
            let pr_str = pr.to_string();
            let out = run_in_dir(&host, &wt, &["gh", "pr", "edit", &pr_str, "--base", &onto])?;
            if !out.status.success() {
                return Err(Error::Command {
                    cmd: format!("gh pr edit {pr_str} --base {onto}"),
                    status: out.status.to_string(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                });
            }
            Some(pr)
        }
        None => None,
    };

    Ok(RestackOutcome::Restacked {
        task_id,
        branch: child_branch,
        new_base: onto,
        sha: new_sha,
        retargeted_pr,
    })
}

/// [`lookup_open_pr`] degraded to "no PR" when gh reports that origin
/// isn't a GitHub remote. Used by [`restack`] for the optional PR
/// retarget step — a project pointing at a non-GitHub remote can still
/// benefit from the rebase + push, so we don't want the action to fail
/// end-to-end just because there's nothing for `gh pr edit` to touch.
///
/// We match on the specific stderr fragment gh prints in this case
/// rather than treating *every* gh failure as "no PR" — a real
/// authentication or network failure should still propagate so the
/// operator can see it.
fn lookup_open_pr_tolerant(host: &Host, wt: &str, branch: &str) -> Result<Option<u64>> {
    match lookup_open_pr(host, wt, branch) {
        Ok(v) => Ok(v),
        Err(Error::Command { ref stderr, .. })
            if stderr.contains("none of the git remotes configured for this repository point to a known GitHub host") =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Unique-per-call `$TMPDIR` path for a throwaway git worktree. The
/// process id + a process-wide counter keep concurrent calls (and
/// parallel test runs) from colliding on the same path — `git worktree
/// add` refuses to overwrite an existing one.
fn unique_temp_worktree_path(kind: &str, id: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "shelbi-{kind}-{}-{}-{}",
        std::process::id(),
        sanitize_path_segment(id),
        seq,
    ))
}

/// Map a task id (already validated to be kebab/snake alphanumeric) to a
/// safe filesystem segment for the temp worktree path. Belt-and-suspenders
/// against an out-of-band frontmatter edit that snuck a `/` past
/// validation — the worktree path lives in $TMPDIR, never inside the
/// repo.
fn sanitize_path_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `gh pr merge <pr> --<strategy>` on the hub, then read the resulting
/// merge commit SHA back with `gh pr view`. Mirrors the shape of
/// [`crate::zen::pr_merge`] but stops short of `--delete-branch` — the
/// workflow's `delete_branch` action is responsible for that.
///
/// Returns `None` when the PR reports `MERGED` but GitHub hadn't recorded
/// the merge commit yet after the polling window — see
/// [`wait_for_merge_commit_sha`].
fn merge_via_pr(host: &Host, wt: &str, pr: u64, strategy: MergeStrategy) -> Result<Option<String>> {
    let pr_str = pr.to_string();
    let strategy_flag = strategy.gh_flag();
    let out = run_in_dir(host, wt, &["gh", "pr", "merge", &pr_str, strategy_flag])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr merge {pr_str} {strategy_flag}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    // gh pr merge doesn't print the merge SHA. Ask gh for it separately,
    // polling — GitHub records the merge commit asynchronously.
    wait_for_merge_commit_sha(host, wt, pr)
}

/// Hub-side fetch + local merge — used when no PR exists for the branch.
/// Steps:
/// 1. Refuse if the branch never made it to origin — that's a workflow
///    contract violation: `merge` runs after `push_branch` (or after the
///    user pushed the branch some other way). The error names the missing
///    ref so the operator can fix it.
/// 2. `git fetch origin <target> <branch>` so we have the latest tips.
/// 3. Refuse if the branch has no commits beyond `origin/<target>` — a
///    no-op merge would record yesterday's HEAD as a "merge SHA."
/// 4. Run `git merge --<strategy>` against `origin/<branch>` in a
///    throwaway temp worktree detached at `origin/<target>` — the same
///    isolation [`restack`] uses. Each concurrent merge gets its own
///    index and working tree, so two tasks merging at once can't
///    interleave git state in the shared work_dir, and the hub's
///    checked-out branch never moves (matching the ViaPr path, which
///    doesn't touch the local checkout either). For `--squash`, follow
///    with a commit since `--squash` only stages.
/// 5. Push the resulting tip to `origin/<target>` and return its SHA.
///
/// The hub's local `<target>` branch ref is deliberately left alone:
/// integration is a remote-side fact, and every consumer (probe,
/// restack, the next merge) re-fetches `origin/<target>` before acting.
/// If a concurrent merge lands on the target between our fetch and our
/// push, the push is rejected as a non-fast-forward — re-running the
/// action re-fetches and re-merges on top of the freshly landed tip.
fn merge_hub_side(
    host: &Host,
    wt: &str,
    branch: &str,
    target: &str,
    strategy: MergeStrategy,
    task_id: &str,
) -> Result<String> {
    // Probe `origin` for the branch *before* fetching so we can surface
    // the workflow-contract violation directly. A bare `git fetch
    // origin <branch>` against a missing ref dies with `couldn't find
    // remote ref <branch>`, which is accurate but doesn't tell the
    // operator the action they need to run.
    if !remote_branch_exists(host, wt, branch)? {
        return Err(Error::Other(format!(
            "branch `{branch}` is not on origin — run the `push_branch` action \
             first, or push the branch manually and retry"
        )));
    }

    run_or_command_err(
        host,
        wt,
        &["git", "fetch", "origin", "--", target, branch],
        || format!("git -C {wt} fetch origin -- {target} {branch}"),
    )?;

    // Guard against "no commits beyond target." A no-op merge is not what
    // any caller wants; bailing here surfaces the misconfiguration loudly
    // instead of returning yesterday's HEAD as the "merge SHA."
    let ahead = run_capture_stdout(
        host,
        wt,
        &[
            "git",
            "rev-list",
            "--count",
            &format!("origin/{target}..origin/{branch}"),
        ],
    )?;
    if ahead.trim() == "0" {
        return Err(Error::Other(format!(
            "branch `{branch}` has no commits beyond `{target}` — nothing to merge"
        )));
    }

    let tmp_path = unique_temp_worktree_path("merge", task_id);
    // git worktree add refuses to overwrite an existing path. Clean up
    // any stale dir from a previous crashed merge before we re-add.
    let _ = std::fs::remove_dir_all(&tmp_path);
    let tmp = tmp_path.to_string_lossy().into_owned();
    let origin_target = format!("origin/{target}");
    run_or_command_err(
        host,
        wt,
        &["git", "worktree", "add", "--detach", &tmp, &origin_target],
        || format!("git -C {wt} worktree add --detach {tmp} {origin_target}"),
    )?;

    let merged = merge_and_push_in_worktree(host, &tmp, branch, target, strategy, task_id);

    // Tear the worktree down regardless of outcome — a conflicted merge
    // must not leak a half-merged tree into $TMPDIR. Best-effort, same
    // as restack: the merge result (or its error) is what the caller
    // gets either way.
    let _ = run_in_dir(host, wt, &["git", "worktree", "remove", "--force", &tmp]);
    let _ = std::fs::remove_dir_all(&tmp_path);

    merged
}

/// The mutating half of [`merge_hub_side`], run entirely inside the
/// throwaway worktree at `tmp` (detached at `origin/<target>`). Split
/// out so the caller can tear the worktree down on every exit path
/// without repeating the cleanup before each `?`.
fn merge_and_push_in_worktree(
    host: &Host,
    tmp: &str,
    branch: &str,
    target: &str,
    strategy: MergeStrategy,
    task_id: &str,
) -> Result<String> {
    let origin_branch = format!("origin/{branch}");
    match strategy {
        MergeStrategy::Squash => {
            run_or_command_err(
                host,
                tmp,
                &["git", "merge", "--squash", &origin_branch],
                || format!("git -C {tmp} merge --squash origin/{branch}"),
            )?;
            // `--squash` only stages; we still owe a commit. The message
            // matches the legacy `shelbi merge` shape so log readers see
            // the same prefix regardless of which path produced the
            // commit.
            let msg = format!("shelbi: merge {task_id} from {branch}");
            run_or_command_err(host, tmp, &["git", "commit", "-m", &msg], || {
                format!("git -C {tmp} commit -m \"{msg}\"")
            })?;
        }
        MergeStrategy::Merge => {
            // `--no-ff` forces a merge commit even when the branch could
            // fast-forward — the spec's "Merge" strategy explicitly
            // preserves the branch's commits *plus* a merge commit on
            // top, which is what `gh pr merge --merge` produces on
            // GitHub. `--no-edit` keeps git from launching $EDITOR; the
            // default `Merge branch '…'` message stays.
            run_or_command_err(
                host,
                tmp,
                &["git", "merge", "--no-ff", "--no-edit", &origin_branch],
                || format!("git -C {tmp} merge --no-ff origin/{branch}"),
            )?;
        }
        MergeStrategy::Rebase => unreachable!("rejected upstream by require_supported_strategy"),
    }

    run_or_command_err(
        host,
        tmp,
        &["git", "push", "origin", &format!("HEAD:{target}")],
        || format!("git -C {tmp} push origin HEAD:{target}"),
    )?;

    let sha = run_capture_stdout(host, tmp, &["git", "rev-parse", "HEAD"])?
        .trim()
        .to_string();
    if sha.is_empty() {
        return Err(Error::Other(format!(
            "post-merge `git rev-parse HEAD` returned empty output in {tmp}"
        )));
    }
    Ok(sha)
}

/// v1 of the `merge` action supports `squash` and `merge`. `rebase` is
/// reserved (see Plans/workflows.md §12 "Action set"); reject it loudly
/// rather than silently picking one of the other strategies.
fn require_supported_strategy(strategy: MergeStrategy) -> Result<MergeStrategy> {
    match strategy {
        MergeStrategy::Squash | MergeStrategy::Merge => Ok(strategy),
        MergeStrategy::Rebase => Err(Error::Other(
            "merge action does not support `merge_strategy: rebase` yet — \
             set `git.merge_strategy` to `squash` or `merge` in the project \
             or workflow YAML"
                .into(),
        )),
    }
}

/// `run_in_dir` + convert a non-zero status into [`Error::Command`] using
/// a caller-supplied `cmd` description. Sugar to keep the merge orchestration
/// readable.
fn run_or_command_err<F>(host: &Host, wt: &str, argv: &[&str], cmd_for_err: F) -> Result<()>
where
    F: FnOnce() -> String,
{
    let out = run_in_dir(host, wt, argv)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: cmd_for_err(),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

/// `run_in_dir` and return stdout as an owned String, surfacing failures
/// as [`Error::Command`]. Sugar for the merge orchestration's read
/// commands.
fn run_capture_stdout(host: &Host, wt: &str, argv: &[&str]) -> Result<String> {
    let out = run_in_dir(host, wt, argv)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: argv.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Close any open PR for the task's branch on the hub.
///
/// Returns `Some(pr_number)` when a PR was closed, `None` when no open PR
/// existed for the branch (the spec's "no-op if none open" case).
pub fn close_pr(project: &Project, task: &Task) -> Result<Option<u64>> {
    let branch = require_branch(task)?;
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();

    let Some(num) = lookup_open_pr(&host, &wt, &branch)? else {
        return Ok(None);
    };
    let num_str = num.to_string();
    let out = run_in_dir(&host, &wt, &["gh", "pr", "close", &num_str])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr close {num_str}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(Some(num))
}

/// Delete the task's branch from origin and from the hub's local refs.
///
/// Skipped when any of the project's workspaces currently has the branch
/// checked out in its worktree — yanking the branch out from under an
/// active task would force the workspace into a detached HEAD on its next
/// fetch. Returns [`DeleteOutcome::NotPresent`] when the branch is already
/// gone in both places (idempotent).
pub fn delete_branch(project: &Project, task: &Task) -> Result<DeleteOutcome> {
    let branch = require_branch(task)?;

    if let Some(workspace_name) = workspace_holding_branch(project, &branch)? {
        return Ok(DeleteOutcome::Skipped {
            reason: format!("branch is checked out in workspace `{workspace_name}`"),
        });
    }

    if let Some(child_id) = deferred_multi_parent_child_needing_branch(project, task, &branch) {
        return Ok(DeleteOutcome::Skipped {
            reason: format!("restack-deferred:needed-by={child_id}"),
        });
    }

    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();

    let local_present = local_branch_exists(&host, &wt, &branch)?;
    let remote_present = remote_branch_exists(&host, &wt, &branch)?;
    if !local_present && !remote_present {
        return Ok(DeleteOutcome::NotPresent);
    }

    if remote_present {
        let out = run_in_dir(
            &host,
            &wt,
            &["git", "push", "origin", "--delete", "--", &branch],
        )?;
        if !out.status.success() {
            // Race: the remote branch was removed between our probe and
            // the push (e.g. by a concurrent `gh pr merge --delete-branch`).
            // git reports `remote ref does not exist` and exits non-zero;
            // for an idempotent primitive that's a benign success.
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            if !stderr.contains("remote ref does not exist") {
                return Err(Error::Command {
                    cmd: format!("git -C {wt} push origin --delete -- {branch}"),
                    status: out.status.to_string(),
                    stderr,
                });
            }
        }
    }

    if local_present {
        let out = run_in_dir(&host, &wt, &["git", "branch", "-D", "--", &branch])?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: format!("git -C {wt} branch -D -- {branch}"),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }

    Ok(DeleteOutcome::Deleted)
}

fn deferred_multi_parent_child_needing_branch(
    project: &Project,
    parent_task: &Task,
    parent_branch: &str,
) -> Option<String> {
    let tasks = shelbi_state::list_tasks(&project.name).ok()?;
    tasks
        .iter()
        .map(|tf| &tf.task)
        .find(|child| {
            child.column != Column::done()
                && child.depends_on.len() > 1
                && child.depends_on.iter().any(|id| id == &parent_task.id)
                && !unfinished_multi_parent_deps(child, &tasks, &parent_task.id).is_empty()
                && multi_parent_restack_cutoff(child, &tasks).as_deref() == Some(parent_branch)
        })
        .map(|child| child.id.clone())
}

// ---------------------------------------------------------------------------
// helpers

fn require_branch(task: &Task) -> Result<String> {
    let branch = task.branch.clone().ok_or_else(|| {
        Error::Other(format!(
            "task `{}` has no branch — populate `branch:` in its frontmatter \
             before running this action",
            task.id
        ))
    })?;
    // `save_task` validates `branch:` at write time, but frontmatter can
    // be edited out of band — re-check before the value reaches git argv,
    // where a dashed name would be parsed as an option.
    validate_branch(&branch).map_err(|e| Error::Other(format!("task `{}`: {e}", task.id)))?;
    Ok(branch)
}

/// Return the name of the first workspace whose worktree has `branch` checked
/// out, or `None` if no workspace is holding it. Workspaces whose worktree
/// doesn't exist yet are silently skipped — they can't be holding any
/// branch.
fn workspace_holding_branch(project: &Project, branch: &str) -> Result<Option<String>> {
    for workspace in &project.workspaces {
        let Some(machine) = project.machine(&workspace.machine) else {
            // Misconfiguration — `validate_workspaces` already rejects this
            // shape on project load, so we can defensively skip it here
            // without short-circuiting the whole delete on an unrelated
            // YAML typo.
            continue;
        };
        let host = machine.host();
        let wt = workspace_worktree(machine, workspace);
        match worktree_branch(&host, &wt.to_string_lossy())? {
            Some(b) if b == branch => return Ok(Some(workspace.name.clone())),
            _ => {}
        }
    }
    Ok(None)
}

/// Return the current branch name in `wt`, or `None` if the worktree
/// doesn't exist yet (a fresh workspace that's never been dispatched) or is
/// in a detached HEAD state.
fn worktree_branch(host: &Host, wt: &str) -> Result<Option<String>> {
    let out = run_in_dir(host, wt, &["git", "rev-parse", "--abbrev-ref", "HEAD"])?;
    if !out.status.success() {
        // No worktree, no git repo, or read failure — none of which means
        // "this workspace is holding our branch." Treat as "nothing to skip
        // here" rather than failing the delete.
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "HEAD" {
        // Detached HEAD: the branch ref doesn't gate the delete, so
        // we don't need to know what commit they're on.
        return Ok(None);
    }
    Ok(Some(s))
}

fn local_branch_exists(host: &Host, wt: &str, branch: &str) -> Result<bool> {
    let ref_name = format!("refs/heads/{branch}");
    let out = run_in_dir(
        host,
        wt,
        &["git", "rev-parse", "--verify", "--quiet", &ref_name],
    )?;
    match out.status.code() {
        // Ref resolves → the branch exists.
        Some(0) => Ok(true),
        // `--verify --quiet` exits 1 with no output when the ref simply
        // doesn't resolve — that's the genuine "branch absent" answer.
        Some(1) => Ok(false),
        // Anything else — notably 128 (not a git repo / bad worktree /
        // corrupt refs) — is a real failure, not "no such branch".
        // Reporting it as absent would let `delete_branch` mis-route on a
        // broken repo; surface it instead.
        _ => Err(Error::Command {
            cmd: format!("git -C {wt} rev-parse --verify --quiet {ref_name}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
    }
}

fn remote_branch_exists(host: &Host, wt: &str, branch: &str) -> Result<bool> {
    let out = run_in_dir(host, wt, &["git", "ls-remote", "--heads", "origin", branch])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt} ls-remote --heads origin {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    // ls-remote prints one `<sha>\t<ref>` line per match, or empty when
    // the ref doesn't exist on the remote.
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{
        AgentRunnerSpec, Column, HeartbeatConfig, Machine, MachineKind, OrchestratorSpec,
        WorkspaceSpec, ZenConfig,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::Command;

    // --- DeleteOutcome wire format -----------------------------------------

    #[test]
    fn delete_outcome_as_line_is_prefix_keyed() {
        assert_eq!(DeleteOutcome::Deleted.as_line(), "deleted");
        assert_eq!(DeleteOutcome::NotPresent.as_line(), "not-present");
        let line = DeleteOutcome::Skipped {
            reason: "branch is checked out in workspace `alice`".into(),
        }
        .as_line();
        assert!(line.starts_with("skipped:"));
        assert!(line.contains("alice"));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn delete_outcome_skipped_flattens_newlines() {
        let line = DeleteOutcome::Skipped {
            reason: "first line\nsecond".into(),
        }
        .as_line();
        assert!(!line.contains('\n'));
    }

    // --- require_branch contract ------------------------------------------

    fn bare_task(id: &str) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column: Column::in_progress(),
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            params: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn require_branch_errors_when_task_has_none() {
        let t = bare_task("orphan");
        let err = require_branch(&t).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("orphan"), "{msg}");
        assert!(msg.contains("branch"), "{msg}");
    }

    #[test]
    fn require_branch_returns_branch_when_present() {
        let mut t = bare_task("ok");
        t.branch = Some("shelbi/ok".into());
        assert_eq!(require_branch(&t).unwrap(), "shelbi/ok");
    }

    #[test]
    fn require_branch_rejects_git_option_shaped_values() {
        // `save_task` already rejects these at write time, but frontmatter
        // can be edited out of band — the action layer is the last stop
        // before git argv.
        for bad in ["--delete", "-f", "HEAD:other", "a b"] {
            let mut t = bare_task("evil");
            t.branch = Some(bad.into());
            let err = require_branch(&t).unwrap_err();
            assert!(err.to_string().contains("invalid branch"), "{bad}: {err}");
        }
    }

    // --- git-backed primitives against fixture repos ----------------------

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    /// Build a tiny fixture repo with a `main` branch and an extra
    /// `feature` branch. Returns the repo path.
    fn fixture_repo_with_feature() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        run_git(&repo, &["init", "-q", "-b", "main", "."]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        run_git(&repo, &["branch", "feature"]);
        (tmp, repo)
    }

    #[test]
    fn local_branch_exists_finds_present_and_missing() {
        let (_tmp, repo) = fixture_repo_with_feature();
        let wt = repo.to_string_lossy().into_owned();
        assert!(local_branch_exists(&Host::Local, &wt, "main").unwrap());
        assert!(local_branch_exists(&Host::Local, &wt, "feature").unwrap());
        assert!(!local_branch_exists(&Host::Local, &wt, "nope").unwrap());
    }

    #[test]
    fn worktree_branch_returns_current_branch() {
        let (_tmp, repo) = fixture_repo_with_feature();
        let wt = repo.to_string_lossy().into_owned();
        assert_eq!(
            worktree_branch(&Host::Local, &wt).unwrap().as_deref(),
            Some("main"),
        );
    }

    #[test]
    fn worktree_branch_returns_none_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let wt = missing.to_string_lossy().into_owned();
        // No git repo here — the helper must report "no branch" rather
        // than error, so the delete probe can keep looking at the
        // remaining workspaces.
        assert_eq!(worktree_branch(&Host::Local, &wt).unwrap(), None);
    }

    /// Build a project with one local workspace pointed at `repo` so the
    /// worktree-branch probe finds the right HEAD. We piggy-back on
    /// `workspace_worktree`'s `<work_dir>/.shelbi/wt/<workspace>` derivation
    /// by creating a worktree at that path off `repo`.
    fn project_with_local_workspace_holding(
        repo: &std::path::Path,
        workspace: &str,
        branch: &str,
    ) -> Project {
        let wt_path = repo.join(".shelbi").join("wt").join(workspace);
        // git worktree add requires the parent dir to exist.
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        run_git(
            repo,
            &["worktree", "add", wt_path.to_str().unwrap(), branch],
        );

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
            name: "fixture".into(),
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
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![WorkspaceSpec {
                name: workspace.into(),
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
            git: shelbi_core::GitConfig::default(),
        }
    }

    #[test]
    fn workspace_holding_branch_finds_the_holder() {
        let (_tmp, repo) = fixture_repo_with_feature();
        let project = project_with_local_workspace_holding(&repo, "alice", "feature");

        let holder = workspace_holding_branch(&project, "feature").unwrap();
        assert_eq!(holder.as_deref(), Some("alice"));

        // A branch nobody holds returns None.
        assert!(workspace_holding_branch(&project, "other")
            .unwrap()
            .is_none());
    }

    #[test]
    fn workspace_holding_branch_ignores_missing_worktrees() {
        // A workspace whose worktree hasn't been provisioned yet must not
        // count as holding any branch — otherwise a delete_branch on a
        // fresh project would always say "skipped".
        let (_tmp, repo) = fixture_repo_with_feature();
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
        let project = Project {
            name: "fixture".into(),
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
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![WorkspaceSpec {
                name: "never-spawned".into(),
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
            git: shelbi_core::GitConfig::default(),
        };
        assert!(workspace_holding_branch(&project, "feature")
            .unwrap()
            .is_none());
    }

    fn task_on_branch(id: &str, branch: &str) -> Task {
        let mut t = bare_task(id);
        t.branch = Some(branch.into());
        t
    }

    #[test]
    fn delete_branch_skipped_when_a_workspace_holds_it() {
        let (_tmp, repo) = fixture_repo_with_feature();
        let project = project_with_local_workspace_holding(&repo, "alice", "feature");

        let out = delete_branch(&project, &task_on_branch("t", "feature")).unwrap();
        match out {
            DeleteOutcome::Skipped { reason } => {
                assert!(reason.contains("alice"), "{reason}");
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        // Branch must still exist.
        let wt = repo.to_string_lossy().into_owned();
        assert!(local_branch_exists(&Host::Local, &wt, "feature").unwrap());
    }

    #[test]
    fn delete_branch_not_present_when_branch_already_gone() {
        // Use the origin-bearing fixture so `remote_branch_exists` has
        // a remote to probe; a hub repo without `origin` configured is
        // not a realistic shape for this primitive.
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        run_git(&local, &["push", "origin", "--delete", "feature"]);
        run_git(&local, &["branch", "-D", "feature"]);

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
        let project = Project {
            name: "fixture".into(),
            repo: local.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: local.clone(),
                host: None,
                tags: Vec::new(),
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: shelbi_core::GitConfig::default(),
        };
        let out = delete_branch(&project, &task_on_branch("t", "feature")).unwrap();
        assert!(matches!(out, DeleteOutcome::NotPresent), "{out:?}");
    }

    /// Build two linked local repos: a "remote" bare repo and a working
    /// clone whose `origin` points at it. Lets us drive `delete_branch`'s
    /// remote-side codepath without GitHub.
    fn fixture_repo_with_origin() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("origin.git");
        let local = tmp.path().join("local");
        run_git(tmp.path(), &["init", "-q", "--bare", "origin.git"]);

        std::fs::create_dir_all(&local).unwrap();
        run_git(&local, &["init", "-q", "-b", "main", "."]);
        run_git(&local, &["config", "user.email", "test@example.com"]);
        run_git(&local, &["config", "user.name", "Test"]);
        run_git(
            &local,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        std::fs::write(local.join("README.md"), "hi\n").unwrap();
        run_git(&local, &["add", "README.md"]);
        run_git(&local, &["commit", "-q", "-m", "init"]);
        run_git(&local, &["push", "-u", "origin", "main"]);
        run_git(&local, &["branch", "feature"]);
        run_git(&local, &["push", "-u", "origin", "feature"]);

        (tmp, remote, local)
    }

    #[test]
    fn delete_branch_removes_local_and_remote() {
        let (_tmp, _remote, local) = fixture_repo_with_origin();

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
        let project = Project {
            name: "fixture".into(),
            repo: local.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: local.clone(),
                host: None,
                tags: Vec::new(),
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: shelbi_core::GitConfig::default(),
        };

        let wt = local.to_string_lossy().into_owned();
        assert!(local_branch_exists(&Host::Local, &wt, "feature").unwrap());
        assert!(remote_branch_exists(&Host::Local, &wt, "feature").unwrap());

        let out = delete_branch(&project, &task_on_branch("t", "feature")).unwrap();
        assert!(matches!(out, DeleteOutcome::Deleted), "{out:?}");

        assert!(!local_branch_exists(&Host::Local, &wt, "feature").unwrap());
        assert!(!remote_branch_exists(&Host::Local, &wt, "feature").unwrap());

        // Idempotent — second call sees nothing to do.
        let again = delete_branch(&project, &task_on_branch("t", "feature")).unwrap();
        assert!(matches!(again, DeleteOutcome::NotPresent), "{again:?}");
    }

    #[test]
    fn delete_branch_preserves_cutoff_for_deferred_multi_parent_child() {
        let _g = auto_fire_lock();
        let home = fresh_shelbi_home("delete-deferred-cutoff");
        std::env::set_var("SHELBI_HOME", &home);

        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        add_second_parent_branch(&local);
        let project = project_with_no_workspaces(&local);

        let mut parent = bare_task("par");
        parent.branch = Some("parent".into());
        write_task_file("fixture", &parent);

        let mut other_parent = bare_task("par2");
        other_parent.branch = Some("parent2".into());
        write_task_file("fixture", &other_parent);

        let mut child = bare_task("ch");
        child.branch = Some("child".into());
        child.depends_on = vec!["par".into(), "par2".into()];
        write_task_file("fixture", &child);

        let out = delete_branch(&project, &parent).unwrap();
        assert_eq!(
            out,
            DeleteOutcome::Skipped {
                reason: "restack-deferred:needed-by=ch".into()
            }
        );

        let wt = local.to_string_lossy().into_owned();
        assert!(run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "rev-parse", "--verify", "parent"]
        )
        .is_ok());
        assert!(run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "rev-parse", "--verify", "origin/parent"],
        )
        .is_ok());

        std::env::remove_var("SHELBI_HOME");
    }

    // --- open_pr target resolution chain ----------------------------------
    //
    // Pure-logic coverage of `resolve_pr_target_from` — the disk-free core
    // of the chain. The integration with `shelbi_state::load_task` is one
    // closure away; tests below build that closure in memory so the rules
    // are pinned without touching SHELBI_HOME or shelling out to gh.

    fn parent(id: &str, column: Column, branch: Option<&str>) -> Task {
        let mut t = bare_task(id);
        t.column = column;
        t.branch = branch.map(str::to_string);
        t
    }

    fn child(id: &str, deps: &[&str]) -> Task {
        let mut t = bare_task(id);
        t.depends_on = deps.iter().map(|s| s.to_string()).collect();
        t
    }

    fn lookup(parents: Vec<Task>) -> impl Fn(&str) -> Option<Task> {
        move |id: &str| parents.iter().find(|t| t.id == id).cloned()
    }

    #[test]
    fn target_override_wins_over_everything_else() {
        // Even when `depends_on:` would otherwise point at a parent branch,
        // the workflow engine's per-transition `target:` is the explicit
        // user signal. It must beat both the parent chain and the project
        // default — that's how a workflow declares an intermediate hop.
        let child = child("ch", &["par"]);
        let parents = lookup(vec![parent(
            "par",
            Column::in_progress(),
            Some("shelbi/par"),
        )]);
        let target = resolve_pr_target_from("main", &child, Some("develop"), parents);
        assert_eq!(target, "develop");
    }

    #[test]
    fn depends_on_parent_branch_targets_stacked_base() {
        // The spec's canonical stacked-PR shape: child's PR base is the
        // parent task's `branch:`, *not* the workflow's base_branch. No
        // override given.
        let ch = child("ch", &["par"]);
        let parents = lookup(vec![parent(
            "par",
            Column::in_progress(),
            Some("shelbi/par"),
        )]);
        let target = resolve_pr_target_from("main", &ch, None, parents);
        assert_eq!(target, "shelbi/par");
    }

    #[test]
    fn done_parent_is_skipped_so_we_dont_target_a_deleted_branch() {
        // Once the parent merges, the Done-side `delete_branch` action
        // removes the parent branch. Restack rewrites the child's base, but
        // a fresh `open_pr` after that point must aim at the project base
        // — not a dangling ref.
        let ch = child("ch", &["par"]);
        let parents = lookup(vec![parent("par", Column::done(), Some("shelbi/par"))]);
        let target = resolve_pr_target_from("main", &ch, None, parents);
        assert_eq!(target, "main");
    }

    #[test]
    fn parent_without_branch_falls_through_to_project_default() {
        // A parent that hasn't been dispatched yet (still in Backlog or
        // Todo) won't have a `branch:` field. The chain shouldn't synth a
        // branch out of the id; it should keep walking.
        let ch = child("ch", &["par"]);
        let parents = lookup(vec![parent("par", Column::backlog(), None)]);
        let target = resolve_pr_target_from("main", &ch, None, parents);
        assert_eq!(target, "main");
    }

    #[test]
    fn first_usable_parent_wins_for_multi_parent_tasks() {
        // The spec uses singular "parent task," but `depends_on:` is a
        // list. The first parent with a usable branch is the natural pick
        // — earlier deps shouldn't be skipped just because a later one
        // also has a branch.
        let ch = child("ch", &["par1", "par2"]);
        let parents = lookup(vec![
            parent("par1", Column::in_progress(), Some("shelbi/par1")),
            parent("par2", Column::in_progress(), Some("shelbi/par2")),
        ]);
        let target = resolve_pr_target_from("main", &ch, None, parents);
        assert_eq!(target, "shelbi/par1");
    }

    #[test]
    fn earlier_done_parent_yields_to_later_usable_parent() {
        // par1 already merged (Done, branch may be deleted), par2 is
        // still active. Resolution should keep walking past par1 and find
        // par2 rather than fall straight through to project base.
        let ch = child("ch", &["par1", "par2"]);
        let parents = lookup(vec![
            parent("par1", Column::done(), Some("shelbi/par1")),
            parent("par2", Column::in_progress(), Some("shelbi/par2")),
        ]);
        let target = resolve_pr_target_from("main", &ch, None, parents);
        assert_eq!(target, "shelbi/par2");
    }

    #[test]
    fn missing_parent_lookup_falls_through_silently() {
        // `depends_on:` validation catches dangling ids at save time, so a
        // None from the lookup here means an out-of-band edit. Don't blow
        // up the action; fall through to the next candidate (or project
        // default).
        let ch = child("ch", &["ghost"]);
        let parents = lookup(Vec::new());
        let target = resolve_pr_target_from("main", &ch, None, parents);
        assert_eq!(target, "main");
    }

    #[test]
    fn no_depends_on_uses_project_base_branch() {
        let ch = child("ch", &[]);
        let parents = lookup(Vec::new());
        let target = resolve_pr_target_from("trunk", &ch, None, parents);
        assert_eq!(target, "trunk");
    }

    // --- merge wire format + strategy gate --------------------------------

    #[test]
    fn merge_outcome_as_line_is_prefix_keyed() {
        let pr = MergeOutcome::ViaPr {
            pr: 42,
            sha: Some("deadbeefcafef00d".into()),
        };
        assert_eq!(pr.as_line(), "pr:42:deadbeefcafef00d");
        assert_eq!(pr.sha(), Some("deadbeefcafef00d"));

        // Merged but GitHub hadn't recorded the SHA yet — success line
        // with a stable placeholder token in the SHA slot.
        let pending = MergeOutcome::ViaPr { pr: 42, sha: None };
        assert_eq!(pending.as_line(), "pr:42:sha-pending");
        assert_eq!(pending.sha(), None);

        let hub = MergeOutcome::HubSide {
            sha: "beadc0de".into(),
            target: "main".into(),
        };
        assert_eq!(hub.as_line(), "hub:main:beadc0de");
        assert_eq!(hub.sha(), Some("beadc0de"));
    }

    #[test]
    fn supported_strategies_pass_through() {
        assert_eq!(
            require_supported_strategy(MergeStrategy::Squash).unwrap(),
            MergeStrategy::Squash
        );
        assert_eq!(
            require_supported_strategy(MergeStrategy::Merge).unwrap(),
            MergeStrategy::Merge
        );
    }

    #[test]
    fn rebase_strategy_errors_with_actionable_message() {
        let err = require_supported_strategy(MergeStrategy::Rebase).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rebase"), "{msg}");
        // The message must name the YAML knob the user has to flip; a
        // bare "unsupported" wouldn't tell them where to look.
        assert!(msg.contains("merge_strategy"), "{msg}");
        assert!(msg.contains("squash"), "{msg}");
    }

    // --- merge hub-side integration ---------------------------------------
    //
    // We exercise the hub-side fetch path against the same bare-remote
    // fixture used by `delete_branch_removes_local_and_remote`. Tests
    // drive `merge_hub_side` directly rather than the public `merge()`
    // because `merge()`'s very first step is `gh pr list` — and gh
    // refuses to query a non-GitHub remote, so the fixture's plain
    // bare repo can't be probed. The PR-branching decision in `merge()`
    // is a one-line `if let`; integration testing against real GitHub
    // covers it (see `zen::pr_merge`'s existing harness, same shape).
    //
    // The gh-pr path is integration-only (requires GitHub), so we don't
    // try to simulate it here — the strategy flag selection and SHA
    // round-trip are covered by the existing `zen::pr_merge` tests on
    // real PRs.

    /// Build a project pointing at `local` with the given strategy.
    /// Kept around because the rebase-gate test still goes through the
    /// public `merge()` entry point.
    fn project_at(local: &std::path::Path, strategy: MergeStrategy) -> Project {
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
            name: "fixture".into(),
            repo: local.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: local.to_path_buf(),
                host: None,
                tags: Vec::new(),
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: shelbi_core::GitConfig {
                base_branch: None,
                merge_strategy: strategy,
            },
        }
    }

    /// Add a commit to `feature` so it has something beyond `main`, then
    /// push it.
    fn advance_feature_with_origin(local: &std::path::Path) {
        run_git(local, &["checkout", "feature"]);
        std::fs::write(local.join("feature.txt"), "from feature\n").unwrap();
        run_git(local, &["add", "feature.txt"]);
        run_git(local, &["commit", "-q", "-m", "feature work"]);
        run_git(local, &["push", "origin", "feature"]);
        run_git(local, &["checkout", "main"]);
    }

    #[test]
    fn hub_side_squash_merges_branch_into_target() {
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        advance_feature_with_origin(&local);
        let wt = local.to_string_lossy().into_owned();

        let head_before = run_capture_stdout(&Host::Local, &wt, &["git", "rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        let sha = merge_hub_side(
            &Host::Local,
            &wt,
            "feature",
            "main",
            MergeStrategy::Squash,
            "t",
        )
        .unwrap();
        assert!(!sha.is_empty());

        // The squashed change landed on origin/main — integration is a
        // remote-side fact now that the merge runs in a temp worktree.
        let remote_sha =
            run_capture_stdout(&Host::Local, &wt, &["git", "rev-parse", "origin/main"])
                .unwrap()
                .trim()
                .to_string();
        assert_eq!(remote_sha, sha);

        // The squash commit is a single new commit on origin/main (the
        // message is shelbi's, not the workspace's), so log shape: init,
        // then merge.
        let log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/main", "--format=%s"],
        )
        .unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert_eq!(lines[0], "shelbi: merge t from feature");
        assert_eq!(lines[1], "init");

        // The hub work_dir's checkout never moved — like the ViaPr path.
        let head_after = run_capture_stdout(&Host::Local, &wt, &["git", "rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(head_after, head_before, "hub checkout must not move");
    }

    #[test]
    fn hub_side_merge_strategy_preserves_branch_history() {
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        advance_feature_with_origin(&local);
        let wt = local.to_string_lossy().into_owned();

        let _sha = merge_hub_side(
            &Host::Local,
            &wt,
            "feature",
            "main",
            MergeStrategy::Merge,
            "t",
        )
        .unwrap();

        // `--merge` strategy preserves the feature commit AND adds a
        // merge commit on top, so three subjects show up in the remote
        // target's log: the merge commit, the feature commit, and the
        // initial commit. We don't pin their interleaving — git log's
        // default ordering for merges is topology-driven and can differ
        // between git versions — but all three must be present and
        // exactly one of them must be the merge commit.
        let log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/main", "--format=%s"],
        )
        .unwrap();
        let subjects: std::collections::HashSet<&str> = log.lines().collect();
        assert_eq!(subjects.len(), 3, "{log}");
        assert!(subjects.contains("feature work"), "{log}");
        assert!(subjects.contains("init"), "{log}");
        assert!(
            subjects.iter().any(|s| s.starts_with("Merge")),
            "expected a merge commit subject in {log}",
        );
    }

    #[test]
    fn hub_side_routes_merge_to_an_arbitrary_target() {
        // The `target_override` -> `merge_hub_side(target=...)` plumbing
        // shouldn't care which branch is named. Cut a `develop` branch
        // off `main` on both local and origin and aim the merge at it,
        // mirroring a workflow with `target: develop` on its review edge.
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        advance_feature_with_origin(&local);
        run_git(&local, &["branch", "develop"]);
        run_git(&local, &["push", "origin", "develop"]);
        let wt = local.to_string_lossy().into_owned();

        let _sha = merge_hub_side(
            &Host::Local,
            &wt,
            "feature",
            "develop",
            MergeStrategy::Squash,
            "t",
        )
        .unwrap();

        // origin/main untouched; origin/develop got the squash commit.
        let main_log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/main", "--format=%s"],
        )
        .unwrap();
        assert_eq!(main_log.trim(), "init");
        let dev_log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/develop", "--format=%s"],
        )
        .unwrap();
        let dev_lines: Vec<&str> = dev_log.lines().collect();
        assert_eq!(dev_lines[0], "shelbi: merge t from feature");
    }

    #[test]
    fn rebase_strategy_blocks_action_before_any_git_runs() {
        // The gate is at the top of `merge` so a misconfigured project
        // fails fast, before we touch the working tree. Use a fixture
        // that *would* otherwise be a clean merge candidate — the test
        // proves the gate fires regardless of whether the merge could
        // have succeeded. This is the one merge test that drives the
        // public `merge()` rather than `merge_hub_side`: the gate sits
        // before the `gh pr list` probe, so the bare-remote fixture
        // never hits gh.
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        advance_feature_with_origin(&local);
        let project = project_at(&local, MergeStrategy::Rebase);

        // The rebase gate fires before merge() reaches the child-task
        // enumeration, so it never touches shelbi_state. No SHELBI_HOME
        // dance needed here.
        let err = merge(&project, "fixture", &task_on_branch("t", "feature"), None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rebase"), "{msg}");

        // Tree untouched — main still at the initial commit.
        let wt = local.to_string_lossy().into_owned();
        let log =
            run_capture_stdout(&Host::Local, &wt, &["git", "log", "main", "--format=%s"]).unwrap();
        assert_eq!(log.trim(), "init");
    }

    #[test]
    fn missing_origin_branch_errors_pointing_at_push_branch() {
        // No `push_branch` ran first → no `origin/feature` ref. The
        // error must name the action the operator should run, not just
        // bubble up git's "couldn't find remote ref" message.
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        // Remove the feature branch from origin so the precondition fails.
        run_git(&local, &["push", "origin", "--delete", "feature"]);
        let wt = local.to_string_lossy().into_owned();

        let err = merge_hub_side(
            &Host::Local,
            &wt,
            "feature",
            "main",
            MergeStrategy::Squash,
            "t",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not on origin"), "{msg}");
        assert!(msg.contains("push_branch"), "{msg}");
    }

    #[test]
    fn no_commits_beyond_target_errors_instead_of_recording_a_no_op() {
        // If feature is identical to main, there's nothing to merge.
        // Returning the current HEAD as the "merge SHA" would silently
        // log a successful integration that did nothing — refuse loudly.
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        // `feature` was branched off init but never advanced, so it's
        // sitting at the same SHA as main. Push it so origin/feature
        // exists (the precondition).
        run_git(&local, &["push", "origin", "feature"]);
        let wt = local.to_string_lossy().into_owned();

        let err = merge_hub_side(
            &Host::Local,
            &wt,
            "feature",
            "main",
            MergeStrategy::Squash,
            "t",
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no commits beyond"), "{msg}");
    }

    #[test]
    fn dirty_hub_work_dir_neither_blocks_nor_leaks_into_the_merge() {
        // The merge runs in a throwaway temp worktree, so the state of
        // the hub work_dir — untracked scratch files, staged user edits,
        // `.shelbi/` scribbles — is irrelevant: it can't block the merge
        // and, more importantly, can't be swept into the integration
        // commit. (The old in-place implementation had to refuse on a
        // dirty tree precisely because a staged user change *would* have
        // ridden along with a `--squash` commit.)
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        advance_feature_with_origin(&local);

        // Untracked scratch file + a staged (but uncommitted) user edit.
        std::fs::write(local.join("user-wip.txt"), "scratch\n").unwrap();
        std::fs::write(local.join("README.md"), "staged user edit\n").unwrap();
        run_git(&local, &["add", "README.md"]);
        let wt = local.to_string_lossy().into_owned();

        let sha = merge_hub_side(
            &Host::Local,
            &wt,
            "feature",
            "main",
            MergeStrategy::Squash,
            "t",
        )
        .unwrap();

        // The squash commit contains exactly the feature's file — none of
        // the hub work_dir's dirt.
        let touched = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "show", "--name-only", "--format=", &sha],
        )
        .unwrap();
        let files: Vec<&str> = touched.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(files, vec!["feature.txt"], "{touched}");

        // And the user's in-flight state survived untouched.
        assert!(local.join("user-wip.txt").exists());
        let staged = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "diff", "--cached", "--name-only"],
        )
        .unwrap();
        assert_eq!(staged.trim(), "README.md", "staged user edit must survive");
    }

    #[test]
    fn hub_side_merge_leaves_hub_checkout_untouched() {
        // F11: the old implementation checked `target` out in the shared
        // work_dir and left it there. The temp-worktree implementation
        // must leave the hub's checked-out branch and HEAD exactly where
        // they were, and clean up its worktree.
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        advance_feature_with_origin(&local);
        let wt = local.to_string_lossy().into_owned();

        // Park the hub on a branch that is *not* the merge target so a
        // regression to "checkout target" is visible.
        run_git(&local, &["checkout", "-q", "-b", "parked"]);
        let head_before =
            run_capture_stdout(&Host::Local, &wt, &["git", "rev-parse", "HEAD"]).unwrap();

        let _sha = merge_hub_side(
            &Host::Local,
            &wt,
            "feature",
            "main",
            MergeStrategy::Squash,
            "t",
        )
        .unwrap();

        let branch_after = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "rev-parse", "--abbrev-ref", "HEAD"],
        )
        .unwrap();
        assert_eq!(
            branch_after.trim(),
            "parked",
            "checked-out branch must not change"
        );
        let head_after =
            run_capture_stdout(&Host::Local, &wt, &["git", "rev-parse", "HEAD"]).unwrap();
        assert_eq!(head_after, head_before, "HEAD must not move");

        // No leaked temp worktree — only the main working tree remains.
        let worktrees = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "worktree", "list", "--porcelain"],
        )
        .unwrap();
        let count = worktrees
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count();
        assert_eq!(count, 1, "temp worktree must be removed:\n{worktrees}");
    }

    #[test]
    fn concurrent_hub_side_merges_do_not_interleave_shared_state() {
        // F2: two tasks merging at once used to run checkout/merge/commit
        // in the same shared work_dir. With per-merge temp worktrees each
        // merge owns its index, so concurrent merges can't cross-
        // contaminate — the only allowed interference is a loud, clean
        // failure (a non-fast-forward push or a transient ref lock),
        // which a retry resolves.
        let (_tmp, _remote, local) = fixture_repo_with_origin();

        // Two independent branches, each adding its own file.
        for (branch, file) in [("task-a", "a.txt"), ("task-b", "b.txt")] {
            run_git(&local, &["checkout", "-q", "-b", branch, "main"]);
            std::fs::write(local.join(file), format!("{branch}\n")).unwrap();
            run_git(&local, &["add", file]);
            run_git(&local, &["commit", "-q", "-m", &format!("{branch} work")]);
            run_git(&local, &["push", "-q", "origin", branch]);
        }
        run_git(&local, &["checkout", "-q", "main"]);
        let wt = local.to_string_lossy().into_owned();
        let head_before =
            run_capture_stdout(&Host::Local, &wt, &["git", "rev-parse", "HEAD"]).unwrap();

        std::thread::scope(|s| {
            let handles: Vec<_> = ["task-a", "task-b"]
                .into_iter()
                .map(|branch| {
                    let wt = wt.clone();
                    s.spawn(move || {
                        // Retry: losing the push race (or a fetch ref-lock
                        // collision) is the sanctioned loud failure; the
                        // orchestrator would re-run the action the same way.
                        for attempt in 0.. {
                            match merge_hub_side(
                                &Host::Local,
                                &wt,
                                branch,
                                "main",
                                MergeStrategy::Squash,
                                branch,
                            ) {
                                Ok(sha) => return sha,
                                Err(e) if attempt < 5 => {
                                    eprintln!("{branch} attempt {attempt} retrying: {e}");
                                    std::thread::sleep(std::time::Duration::from_millis(50));
                                }
                                Err(e) => panic!("{branch} failed after retries: {e}"),
                            }
                        }
                        unreachable!()
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
        });

        // Both squash commits landed on origin/main, and each touches
        // exactly its own file — no cross-contamination between the two
        // concurrent merges.
        run_git(&local, &["fetch", "-q", "origin", "main"]);
        let log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/main", "--format=%H %s"],
        )
        .unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "init + two squash commits expected:\n{log}");
        for line in &lines[..2] {
            let (sha, subject) = line.split_once(' ').unwrap();
            let expected_file = if subject.contains("task-a") {
                "a.txt"
            } else if subject.contains("task-b") {
                "b.txt"
            } else {
                panic!("unexpected commit subject: {subject}");
            };
            let touched = run_capture_stdout(
                &Host::Local,
                &wt,
                &["git", "show", "--name-only", "--format=", sha],
            )
            .unwrap();
            let files: Vec<&str> = touched.lines().filter(|l| !l.trim().is_empty()).collect();
            assert_eq!(
                files,
                vec![expected_file],
                "commit {subject} must touch only its own file"
            );
        }

        // The shared work_dir came through clean and unmoved.
        let head_after =
            run_capture_stdout(&Host::Local, &wt, &["git", "rev-parse", "HEAD"]).unwrap();
        assert_eq!(head_after, head_before, "hub HEAD must not move");
        let status =
            run_capture_stdout(&Host::Local, &wt, &["git", "status", "--porcelain"]).unwrap();
        assert_eq!(status.trim(), "", "hub work_dir must stay clean");
    }

    // --- restack: wire format ---------------------------------------------

    #[test]
    fn restack_outcome_as_line_is_prefix_keyed() {
        let restacked = RestackOutcome::Restacked {
            task_id: "child".into(),
            branch: "shelbi/child".into(),
            new_base: "main".into(),
            sha: "deadbeef".into(),
            retargeted_pr: None,
        };
        assert_eq!(
            restacked.as_line(),
            "restacked:child:shelbi/child:main:deadbeef"
        );

        let with_pr = RestackOutcome::Restacked {
            task_id: "child".into(),
            branch: "shelbi/child".into(),
            new_base: "main".into(),
            sha: "deadbeef".into(),
            retargeted_pr: Some(42),
        };
        assert_eq!(
            with_pr.as_line(),
            "restacked:child:shelbi/child:main:deadbeef pr=42"
        );

        let skipped = RestackOutcome::Skipped {
            task_id: "child".into(),
            reason: "already-restacked".into(),
        };
        assert_eq!(skipped.as_line(), "skipped:child:already-restacked");

        // Reason newlines collapse so the line stays parseable.
        let multiline = RestackOutcome::Skipped {
            task_id: "child".into(),
            reason: "first\nsecond".into(),
        };
        assert!(!multiline.as_line().contains('\n'));
    }

    // --- restack: hub-side integration ------------------------------------
    //
    // Build a stacked fixture: main → parent → child. After "parent" merges
    // into main, restack should rewrite child's history so its commits land
    // on top of main (no longer on top of parent).

    /// Build a fixture with `main`, `parent` (one commit past main), and
    /// `child` (one commit past parent). Origin tracks all three. Returns
    /// the temp dir guard, the bare remote path, and the working clone.
    fn fixture_repo_with_stacked_branches() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let (tmp, remote, local) = fixture_repo_with_origin();
        // The `feature` branch from the origin fixture isn't needed for
        // these tests — leave it alone.

        // Create `parent` off main with a commit.
        run_git(&local, &["checkout", "-b", "parent", "main"]);
        std::fs::write(local.join("parent.txt"), "from parent\n").unwrap();
        run_git(&local, &["add", "parent.txt"]);
        run_git(&local, &["commit", "-q", "-m", "parent work"]);
        run_git(&local, &["push", "-u", "origin", "parent"]);

        // Create `child` off parent with a commit.
        run_git(&local, &["checkout", "-b", "child", "parent"]);
        std::fs::write(local.join("child.txt"), "from child\n").unwrap();
        run_git(&local, &["add", "child.txt"]);
        run_git(&local, &["commit", "-q", "-m", "child work"]);
        run_git(&local, &["push", "-u", "origin", "child"]);

        // Park HEAD on main so restack's hub work_dir starts in the same
        // state the orchestrator usually leaves it in.
        run_git(&local, &["checkout", "main"]);

        (tmp, remote, local)
    }

    /// Simulate that `parent` already merged into `main` on origin (squash
    /// strategy: one new commit on main containing parent's content). The
    /// `parent` branch itself is left in place so restack's `from_base`
    /// ref still resolves — `delete_branch` runs *after* restack in the
    /// workflow.
    ///
    /// We use `git merge --squash` with an explicit shelbi-flavored
    /// commit message instead of `cherry-pick` so the resulting commit
    /// SHA is guaranteed distinct from parent's tip — a cherry-pick on
    /// the same wall-clock second as the original commit collides on
    /// timestamps and (since everything else matches) produces an
    /// identical SHA, which silently breaks the "behind_onto" guard's
    /// preconditions in tests.
    fn advance_main_with_parent_squashed(local: &std::path::Path) {
        run_git(local, &["checkout", "main"]);
        run_git(local, &["merge", "--squash", "parent"]);
        run_git(
            local,
            &["commit", "-q", "-m", "shelbi: squash parent into main"],
        );
        run_git(local, &["push", "origin", "main"]);
    }

    fn add_second_parent_branch(local: &std::path::Path) {
        run_git(local, &["checkout", "-b", "parent2", "main"]);
        std::fs::write(local.join("parent2.txt"), "from parent2\n").unwrap();
        run_git(local, &["add", "parent2.txt"]);
        run_git(local, &["commit", "-q", "-m", "parent2 work"]);
        run_git(local, &["push", "-u", "origin", "parent2"]);
        run_git(local, &["checkout", "main"]);
    }

    fn advance_main_with_second_parent_squashed(local: &std::path::Path) {
        run_git(local, &["checkout", "main"]);
        run_git(local, &["merge", "--squash", "parent2"]);
        run_git(
            local,
            &["commit", "-q", "-m", "shelbi: squash parent2 into main"],
        );
        run_git(local, &["push", "origin", "main"]);
    }

    fn project_with_no_workspaces(local: &std::path::Path) -> Project {
        project_at(local, MergeStrategy::Squash)
    }

    fn child_task_on_branch(id: &str, branch: &str, depends_on: &[&str]) -> Task {
        let mut t = bare_task(id);
        t.branch = Some(branch.into());
        t.depends_on = depends_on.iter().map(|s| s.to_string()).collect();
        t
    }

    #[test]
    fn restack_rebases_child_onto_new_base_and_force_pushes() {
        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        advance_main_with_parent_squashed(&local);

        let project = project_with_no_workspaces(&local);
        let child = child_task_on_branch("ch", "child", &["par"]);

        let out = restack(&project, &child, "parent", Some("main")).unwrap();
        match out {
            RestackOutcome::Restacked {
                task_id,
                branch,
                new_base,
                sha,
                retargeted_pr,
            } => {
                assert_eq!(task_id, "ch");
                assert_eq!(branch, "child");
                assert_eq!(new_base, "main");
                assert!(!sha.is_empty());
                // No PR exists in this bare-remote fixture, so there's
                // nothing for gh pr edit to retarget.
                assert!(retargeted_pr.is_none());
            }
            other => panic!("expected Restacked, got {other:?}"),
        }

        // origin/child now sits on top of origin/main. Concretely: it
        // contains main + a single "child work" commit, NOT the original
        // "parent work" commit (that's been squashed into main).
        let wt = local.to_string_lossy().into_owned();
        // Refetch so the local repo sees the force-pushed history.
        run_or_command_err(&Host::Local, &wt, &["git", "fetch", "origin"], || {
            "git fetch origin".into()
        })
        .unwrap();
        let child_log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/child", "--format=%s"],
        )
        .unwrap();
        let subjects: Vec<&str> = child_log.lines().collect();
        assert_eq!(subjects[0], "child work", "{child_log}");
        // Subsequent commits are main's history (init + the squashed
        // parent commit our fixture wrote — the message matches
        // `advance_main_with_parent_squashed`).
        assert!(
            subjects.contains(&"shelbi: squash parent into main"),
            "{child_log}",
        );
        assert!(subjects.contains(&"init"), "{child_log}");

        // And the child branch is now a direct descendant of main —
        // i.e., origin/main is an ancestor of origin/child.
        let merge_base = run_capture_stdout(
            &Host::Local,
            &wt,
            &[
                "git",
                "merge-base",
                "--is-ancestor",
                "origin/main",
                "origin/child",
            ],
        );
        // `--is-ancestor` exits 0 iff true; run_capture_stdout already
        // errors on non-zero exit, so any value here means yes.
        assert!(merge_base.is_ok());
    }

    #[test]
    fn restack_is_idempotent_when_already_on_new_base() {
        // After a successful restack, re-running with the same args must
        // be a clean no-op. The rebase that *would* run replays the same
        // commits onto the same base, but we'd still force-push — which
        // a primitive that claims idempotency shouldn't do.
        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        advance_main_with_parent_squashed(&local);

        let project = project_with_no_workspaces(&local);
        let child = child_task_on_branch("ch", "child", &["par"]);

        // First pass: real work.
        let first = restack(&project, &child, "parent", Some("main")).unwrap();
        assert!(matches!(first, RestackOutcome::Restacked { .. }));

        // Second pass: already-restacked guard fires.
        let second = restack(&project, &child, "parent", Some("main")).unwrap();
        match second {
            RestackOutcome::Skipped { reason, .. } => {
                assert_eq!(reason, "already-restacked");
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn restack_skips_when_workspace_holds_the_child_branch() {
        // A workspace actively working in the child branch's worktree would
        // diverge from a force-push. Mirror `delete_branch`'s skip-on-hold
        // policy: surface the workspace name so the operator can choose to
        // wait or rotate the workspace, but don't tamper with origin.
        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        advance_main_with_parent_squashed(&local);

        let project = project_with_local_workspace_holding(&local, "alice", "child");
        let child = child_task_on_branch("ch", "child", &["par"]);

        let out = restack(&project, &child, "parent", Some("main")).unwrap();
        match out {
            RestackOutcome::Skipped { reason, .. } => {
                assert!(reason.starts_with("held-by-"), "{reason}");
                assert!(reason.contains("alice"), "{reason}");
            }
            other => panic!("expected Skipped(held), got {other:?}"),
        }

        // Origin/child is untouched (still has parent's commit in its
        // history, not main's squashed version).
        let wt = local.to_string_lossy().into_owned();
        let child_log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/child", "--format=%s"],
        )
        .unwrap();
        let subjects: Vec<&str> = child_log.lines().collect();
        assert_eq!(subjects[0], "child work");
        assert_eq!(subjects[1], "parent work");
    }

    #[test]
    fn restack_skips_when_child_has_no_branch() {
        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        let project = project_with_no_workspaces(&local);

        let mut child = bare_task("ch");
        child.depends_on = vec!["par".into()];
        // No branch set.

        let out = restack(&project, &child, "parent", Some("main")).unwrap();
        match out {
            RestackOutcome::Skipped { task_id, reason } => {
                assert_eq!(task_id, "ch");
                assert_eq!(reason, "no-branch");
            }
            other => panic!("expected Skipped(no-branch), got {other:?}"),
        }
    }

    #[test]
    fn restack_skips_when_child_branch_is_not_on_origin() {
        // push_branch hasn't fired yet — restack has nothing to rewrite.
        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        advance_main_with_parent_squashed(&local);
        // Remove origin/child but keep the local ref.
        run_git(&local, &["push", "origin", "--delete", "child"]);

        let project = project_with_no_workspaces(&local);
        let child = child_task_on_branch("ch", "child", &["par"]);

        let out = restack(&project, &child, "parent", Some("main")).unwrap();
        match out {
            RestackOutcome::Skipped { reason, .. } => {
                assert_eq!(reason, "child-branch-not-on-origin");
            }
            other => panic!("expected Skipped(child-branch-not-on-origin), got {other:?}"),
        }
    }

    #[test]
    fn restack_skips_when_child_has_no_commits_past_from_base() {
        // If child's tip is at parent's tip (workspace's branch was opened
        // but never advanced), the rebase would slide the branch tip up
        // to `onto` — turning an empty stack into "the merged target."
        // Skip rather than do that silently.
        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        advance_main_with_parent_squashed(&local);

        // Reset origin/child back to origin/parent so it has no commits
        // beyond parent.
        run_git(&local, &["checkout", "child"]);
        run_git(&local, &["reset", "--hard", "parent"]);
        run_git(&local, &["push", "origin", "+child"]);
        run_git(&local, &["checkout", "main"]);

        let project = project_with_no_workspaces(&local);
        let child = child_task_on_branch("ch", "child", &["par"]);

        let out = restack(&project, &child, "parent", Some("main")).unwrap();
        match out {
            RestackOutcome::Skipped { reason, .. } => {
                assert_eq!(reason, "no-commits-beyond-from-base");
            }
            other => panic!("expected Skipped(no-commits), got {other:?}"),
        }
    }

    #[test]
    fn restack_skips_when_rebase_produces_conflicts() {
        // Cook a conflict: have child write to a file that main also
        // changes (after the parent squash). When restack tries to replay
        // child's commit onto main, git refuses with a conflict.
        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();

        // Edit main on origin so it touches `child.txt` differently.
        run_git(&local, &["checkout", "main"]);
        std::fs::write(local.join("child.txt"), "main version\n").unwrap();
        run_git(&local, &["add", "child.txt"]);
        run_git(&local, &["commit", "-q", "-m", "conflicting main change"]);
        run_git(&local, &["push", "origin", "main"]);

        let project = project_with_no_workspaces(&local);
        let child = child_task_on_branch("ch", "child", &["par"]);

        let out = restack(&project, &child, "parent", Some("main")).unwrap();
        match out {
            RestackOutcome::Skipped { reason, .. } => {
                assert_eq!(reason, "rebase-conflict");
            }
            other => panic!("expected Skipped(rebase-conflict), got {other:?}"),
        }

        // origin/child is untouched.
        let wt = local.to_string_lossy().into_owned();
        let child_log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/child", "--format=%s"],
        )
        .unwrap();
        let subjects: Vec<&str> = child_log.lines().collect();
        assert_eq!(subjects[0], "child work");
        assert_eq!(subjects[1], "parent work");
    }

    // --- merge: auto-fire restack on not-Done children --------------------
    //
    // `merge()`'s very first step is a `gh pr list` probe, and `gh`
    // refuses to query a non-GitHub remote — so these tests can't drive
    // the public `merge()` against the bare-remote fixture (same caveat
    // the existing merge tests call out above `project_at`). Instead we
    // cover the cascade by driving the private `restack_children` helper
    // directly: it's the function that owns the "find not-Done children
    // and restack each" logic, and it's the only behaviour that's *new*
    // on top of an already-tested `merge()`.
    //
    // These tests need a `SHELBI_HOME` because `restack_children` calls
    // `shelbi_state::list_tasks`. We serialize them on the orchestrator-
    // crate-wide `test_lock` so `lifecycle::tests` (which also mutates
    // `SHELBI_HOME`) can't race us — a per-module lock would silently
    // interleave on the global env var.

    fn auto_fire_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::test_lock::acquire()
    }

    fn write_task_file(project: &str, task: &Task) {
        shelbi_state::save_task(project, task, "").unwrap();
    }

    fn fresh_shelbi_home(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-restack-test-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn restack_children_cascades_only_to_not_done_dependents() {
        let _g = auto_fire_lock();
        let home = fresh_shelbi_home("auto-fire");
        std::env::set_var("SHELBI_HOME", &home);

        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        advance_main_with_parent_squashed(&local);
        let project = project_with_no_workspaces(&local);

        // Parent task at column=InProgress on branch `parent`.
        let mut parent = bare_task("par");
        parent.branch = Some("parent".into());
        write_task_file("fixture", &parent);

        // Child task at column=InProgress depending on `par`, branch `child`.
        let mut child = bare_task("ch");
        child.branch = Some("child".into());
        child.depends_on = vec!["par".into()];
        write_task_file("fixture", &child);

        // Done child that depends on `par` — must NOT be restacked even
        // though its dep list matches.
        let mut done_child = bare_task("done-ch");
        done_child.branch = Some("done-ch-branch".into());
        done_child.depends_on = vec!["par".into()];
        done_child.column = Column::done();
        write_task_file("fixture", &done_child);

        // Unrelated InProgress task with no dep on `par` — must NOT be
        // restacked. Catches a "scan returned everyone" regression.
        let mut unrelated = bare_task("solo");
        unrelated.branch = Some("solo-branch".into());
        write_task_file("fixture", &unrelated);

        let outcomes = restack_children(&project, "fixture", &parent, "parent", "main");

        // Exactly one outcome — for `ch`.
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        match &outcomes[0] {
            RestackOutcome::Restacked {
                task_id, new_base, ..
            } => {
                assert_eq!(task_id, "ch");
                assert_eq!(new_base, "main");
            }
            other => panic!("expected Restacked for `ch`, got {other:?}"),
        }

        // origin/child is now on top of origin/main: its tip commit is
        // child work, and origin/main is an ancestor.
        let wt = local.to_string_lossy().into_owned();
        run_or_command_err(&Host::Local, &wt, &["git", "fetch", "origin"], || {
            "git fetch origin".into()
        })
        .unwrap();
        let child_log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/child", "--format=%s"],
        )
        .unwrap();
        let subjects: Vec<&str> = child_log.lines().collect();
        assert_eq!(subjects[0], "child work", "{child_log}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn restack_children_defers_multi_parent_child_until_all_parents_done() {
        let _g = auto_fire_lock();
        let home = fresh_shelbi_home("multi-parent-defer");
        std::env::set_var("SHELBI_HOME", &home);

        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        add_second_parent_branch(&local);
        advance_main_with_parent_squashed(&local);
        let project = project_with_no_workspaces(&local);

        let mut parent = bare_task("par");
        parent.branch = Some("parent".into());
        write_task_file("fixture", &parent);

        let mut other_parent = bare_task("par2");
        other_parent.branch = Some("parent2".into());
        write_task_file("fixture", &other_parent);

        let mut child = bare_task("ch");
        child.branch = Some("child".into());
        child.depends_on = vec!["par".into(), "par2".into()];
        write_task_file("fixture", &child);

        let outcomes = restack_children(&project, "fixture", &parent, "parent", "main");
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        match &outcomes[0] {
            RestackOutcome::Skipped { task_id, reason } => {
                assert_eq!(task_id, "ch");
                assert_eq!(reason, "restack-deferred:waiting-on=par2");
            }
            other => panic!("expected deferred skip, got {other:?}"),
        }

        let wt = local.to_string_lossy().into_owned();
        run_or_command_err(&Host::Local, &wt, &["git", "fetch", "origin"], || {
            "git fetch origin".into()
        })
        .unwrap();
        let child_log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/child", "--format=%s"],
        )
        .unwrap();
        let subjects: Vec<&str> = child_log.lines().collect();
        assert_eq!(subjects[0], "child work", "{child_log}");
        assert_eq!(subjects[1], "parent work", "{child_log}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn restack_children_rebases_multi_parent_child_once_when_all_parents_done() {
        let _g = auto_fire_lock();
        let home = fresh_shelbi_home("multi-parent-ready");
        std::env::set_var("SHELBI_HOME", &home);

        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        add_second_parent_branch(&local);
        advance_main_with_parent_squashed(&local);
        advance_main_with_second_parent_squashed(&local);
        let project = project_with_no_workspaces(&local);

        let mut parent = bare_task("par");
        parent.branch = Some("parent".into());
        parent.column = Column::done();
        write_task_file("fixture", &parent);

        let mut final_parent = bare_task("par2");
        final_parent.branch = Some("parent2".into());
        final_parent.column = Column::done();
        write_task_file("fixture", &final_parent);

        let mut child = bare_task("ch");
        child.branch = Some("child".into());
        child.depends_on = vec!["par".into(), "par2".into()];
        write_task_file("fixture", &child);

        let outcomes = restack_children(&project, "fixture", &final_parent, "parent2", "main");
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        match &outcomes[0] {
            RestackOutcome::Restacked {
                task_id, new_base, ..
            } => {
                assert_eq!(task_id, "ch");
                assert_eq!(new_base, "main");
            }
            other => panic!("expected Restacked, got {other:?}"),
        }

        let wt = local.to_string_lossy().into_owned();
        run_or_command_err(&Host::Local, &wt, &["git", "fetch", "origin"], || {
            "git fetch origin".into()
        })
        .unwrap();
        let child_log = run_capture_stdout(
            &Host::Local,
            &wt,
            &["git", "log", "origin/child", "--format=%s"],
        )
        .unwrap();
        let subjects: Vec<&str> = child_log.lines().collect();
        assert_eq!(subjects[0], "child work", "{child_log}");
        assert!(subjects.contains(&"shelbi: squash parent into main"));
        assert!(subjects.contains(&"shelbi: squash parent2 into main"));
        assert!(
            !subjects.contains(&"parent work"),
            "child history should not replay the original parent branch:\n{child_log}"
        );
        assert!(
            !subjects.contains(&"parent2 work"),
            "child history should not replay the second parent branch:\n{child_log}"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn restack_children_with_no_dependents_returns_empty() {
        let _g = auto_fire_lock();
        let home = fresh_shelbi_home("no-children");
        std::env::set_var("SHELBI_HOME", &home);

        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        advance_main_with_parent_squashed(&local);
        let project = project_with_no_workspaces(&local);

        // Parent on disk, but nobody depends on it.
        let mut parent = bare_task("par");
        parent.branch = Some("parent".into());
        write_task_file("fixture", &parent);

        let outcomes = restack_children(&project, "fixture", &parent, "parent", "main");
        assert!(outcomes.is_empty(), "{outcomes:?}");

        std::env::remove_var("SHELBI_HOME");
    }

    // --- merge via PR: restack target is the PR's stored base -------------
    //
    // The gh path is normally integration-only, but the F9 regression
    // (children restacked onto the recomputed project base instead of the
    // branch the PR actually merged into) needs pinning. We drive the
    // public `merge()` end-to-end with a stub `gh` on PATH: `run_in_dir`
    // launches every command through a login shell that sources
    // `~/.profile` (see `git.rs::run_in_dir_runs_in_login_shell_that_sources_rc`),
    // so pointing HOME at a tempdir whose `.profile` prepends our stub-bin
    // dir makes `gh` resolve to the stub while `git` stays real.

    #[test]
    fn merge_via_pr_restacks_children_onto_the_prs_stored_base() {
        let _g = auto_fire_lock();

        // Stacked fixture, plus a `develop` branch simulating where PR #7
        // for `parent` really merged: develop = main + squashed parent.
        // The project base stays `main` — if merge() recomputes the
        // restack target instead of reading the PR's baseRefName, the
        // child lands on `main` and the assertion below catches it.
        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        run_git(&local, &["checkout", "-b", "develop", "main"]);
        run_git(&local, &["merge", "--squash", "parent"]);
        run_git(
            &local,
            &["commit", "-q", "-m", "squash parent into develop"],
        );
        run_git(&local, &["push", "-u", "origin", "develop"]);
        run_git(&local, &["checkout", "main"]);

        let stub = tempfile::tempdir().unwrap();
        let bin = stub.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("gh"),
            r#"#!/bin/sh
case "$*" in
  *"pr list"*"--head parent"*) echo 7 ;;
  *"pr list"*) : ;;
  *"baseRefName"*) echo develop ;;
  *"pr merge"*) : ;;
  *"state,mergeCommit"*) echo "MERGED feedfacecafebeef" ;;
esac
exit 0
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        std::fs::write(
            stub.path().join(".profile"),
            format!("export PATH=\"{}:$PATH\"\n", bin.display()),
        )
        .unwrap();

        let prev_shell = std::env::var_os("SHELL");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("SHELL", "/bin/sh");
        std::env::set_var("HOME", stub.path());
        let home = fresh_shelbi_home("via-pr-base");
        std::env::set_var("SHELBI_HOME", &home);

        let mut parent_task = bare_task("par");
        parent_task.branch = Some("parent".into());
        write_task_file("fixture", &parent_task);
        let mut child_task = bare_task("ch");
        child_task.branch = Some("child".into());
        child_task.depends_on = vec!["par".into()];
        write_task_file("fixture", &child_task);

        let project = project_with_no_workspaces(&local);
        let result = merge(&project, "fixture", &parent_task, None);

        // Restore the env before asserting so a failure doesn't leave the
        // process-global vars pointing at dead tempdirs for later tests.
        match prev_shell {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::env::remove_var("SHELBI_HOME");

        let result = result.unwrap();
        match &result.merge {
            MergeOutcome::ViaPr { pr, sha } => {
                assert_eq!(*pr, 7);
                assert_eq!(sha.as_deref(), Some("feedfacecafebeef"));
            }
            other => panic!("expected ViaPr, got {other:?}"),
        }
        assert_eq!(result.restacks.len(), 1, "{:?}", result.restacks);
        match &result.restacks[0] {
            RestackOutcome::Restacked {
                task_id, new_base, ..
            } => {
                assert_eq!(task_id, "ch");
                // The F9 point: the child restacks onto the PR's stored
                // base (`develop`), not the project base (`main`).
                assert_eq!(new_base, "develop");
            }
            other => panic!("expected Restacked, got {other:?}"),
        }

        // And the git state agrees: origin/develop is now an ancestor of
        // origin/child — the child's new history contains the squashed
        // parent content that only exists on develop.
        let wt = local.to_string_lossy().into_owned();
        run_or_command_err(&Host::Local, &wt, &["git", "fetch", "origin"], || {
            "git fetch origin".into()
        })
        .unwrap();
        let is_ancestor = run_capture_stdout(
            &Host::Local,
            &wt,
            &[
                "git",
                "merge-base",
                "--is-ancestor",
                "origin/develop",
                "origin/child",
            ],
        );
        assert!(is_ancestor.is_ok(), "{is_ancestor:?}");
    }

    #[test]
    fn restack_children_skips_dependent_held_by_a_workspace() {
        // A workspace holding the child's branch makes `restack` skip — we
        // surface that as the cascade's outcome rather than dropping it,
        // so the operator can see *why* the child wasn't moved.
        let _g = auto_fire_lock();
        let home = fresh_shelbi_home("held");
        std::env::set_var("SHELBI_HOME", &home);

        let (_tmp, _remote, local) = fixture_repo_with_stacked_branches();
        advance_main_with_parent_squashed(&local);
        let project = project_with_local_workspace_holding(&local, "alice", "child");

        let mut parent = bare_task("par");
        parent.branch = Some("parent".into());
        write_task_file("fixture", &parent);

        let mut child = bare_task("ch");
        child.branch = Some("child".into());
        child.depends_on = vec!["par".into()];
        write_task_file("fixture", &child);

        let outcomes = restack_children(&project, "fixture", &parent, "parent", "main");
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        match &outcomes[0] {
            RestackOutcome::Skipped { task_id, reason } => {
                assert_eq!(task_id, "ch");
                assert!(reason.starts_with("held-by-"), "{reason}");
            }
            other => panic!("expected Skipped(held), got {other:?}"),
        }

        std::env::remove_var("SHELBI_HOME");
    }
}
