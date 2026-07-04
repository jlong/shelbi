//! Review flow: load a ready-for-review task onto a **review workspace** and
//! start the `review` agent there to serve the branch for a human.
//!
//! This is the canonical review surface. Review workspaces are `role: review`
//! pool slots (scarce by design — 1–2 per machine) that own a persistent
//! worktree under `.shelbi/wt/<name>`; the branch is *moved* onto that worktree
//! (released from whatever dev worktree produced it), so the machine's
//! top-level clone is never checked out into or dirtied by review. See
//! `Plans/review-workspaces.md` (§11).
//!
//! `shelbi review <task>` is now a thin alias over [`review_task`]: it picks a
//! free review workspace on the resolved machine and loads the task onto it, or
//! — when every review workspace is busy — reports the task's queue position
//! rather than failing. A project that declares no `role: review` workspace
//! gets a loud onboarding error pointing at the config it needs to add.
//!
//! The heavy lifting (branch move, `PORT` injection, the `review` agent, the
//! loaded-marker contract) lives in [`crate::workspace::load_review_workspace`]
//! (Phase 3); this module resolves *where* to load and owns the assignment
//! bookkeeping.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use shelbi_core::{Column, Error, Host, Machine, Project, Result, Task, TmuxAddr};
use shelbi_state::TaskFile;

use crate::workspace::{load_review_workspace, review_workspace_port, workspace_worktree, StartSpec};

/// A review workspace currently occupied by a task, surfaced when every review
/// workspace on a machine is busy so the caller can tell the user what's ahead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyReviewSlot {
    pub workspace: String,
    pub task_id: String,
}

/// Result of [`review_task`]: either the task loaded onto a review workspace,
/// or every review workspace was busy and the task is queued.
#[derive(Debug, Clone)]
pub enum ReviewOutcome {
    /// The task was loaded onto `workspace` (on `machine`, reachable at
    /// `host`); its review agent is coming up in the pane at `addr`, serving on
    /// `port` (when the workspace is a review slot with a resolved port).
    Loaded {
        workspace: String,
        machine: String,
        host: Host,
        addr: TmuxAddr,
        port: Option<u16>,
    },
    /// Every review workspace on `machine` is busy. The task waits in the
    /// review queue at `position` (1-based); `busy` lists what's holding the
    /// slots.
    Queued {
        machine: String,
        position: usize,
        busy: Vec<BusyReviewSlot>,
    },
}

/// Load `task_id` onto a review workspace, or report its queue position when
/// all review workspaces on the resolved machine are busy.
///
/// Steps: onboarding check (project declares ≥1 `role: review` workspace) →
/// resolve the machine → pick a free review slot (reusing the one this task is
/// already on, if any) → persist the assignment → dispatch via
/// [`load_review_workspace`]. A dispatch failure rolls the assignment back so a
/// failed load doesn't leave the card pinned to a workspace that isn't running.
pub fn review_task(
    project_name: &str,
    task_id: &str,
    machine_override: Option<&str>,
) -> Result<ReviewOutcome> {
    let project = shelbi_state::load_project(project_name)?;
    let mut tf = shelbi_state::load_task(project_name, task_id)?;

    // Onboarding cutover (§11): review workspaces are the review surface now.
    // A project that never declared one gets a loud, actionable error rather
    // than a silent fallback to the retired top-level-clone checkout.
    if !project.workspaces.iter().any(|w| w.is_review()) {
        return Err(Error::Other(format!(
            "project `{project_name}` declares no `role: review` workspace — review \
             workspaces are the review surface now (the old top-level-clone checkout \
             was retired). Declare at least one in your project config, e.g.\n\n  \
             workspaces:\n    - {{ name: review-1, machine: <machine>, runner: <runner>, role: review }}\n\n\
             then re-run `shelbi review {task_id}`. See Plans/review-workspaces.md (§5.1, §11)."
        )));
    }

    let machine = resolve_review_machine(&project, &tf.task, machine_override)?.clone();
    let reviews = project.review_workspaces(&machine.name);
    if reviews.is_empty() {
        return Err(Error::Other(format!(
            "machine `{}` declares no `role: review` workspace — declare one there, or \
             pass `--machine <other>` to review on a machine that has one.",
            machine.name
        )));
    }

    // Active tasks (in-progress + review) tell us which review workspaces are
    // occupied and where this task sits in the review queue.
    let mut active = shelbi_state::list_column(project_name, Column::InProgress)?;
    active.extend(shelbi_state::list_column(project_name, Column::Review)?);

    match pick_review_slot(&reviews, &active, task_id) {
        SlotPick::Free(ws) => {
            let ws = (*ws).clone();
            let branch = tf
                .task
                .branch
                .clone()
                .unwrap_or_else(|| format!("shelbi/{task_id}"));

            // Persist the assignment BEFORE dispatch (F7): the workspace is now
            // "busy" the instant the load can start, so a second `shelbi review`
            // can't pick the same slot, and `shelbi message`/`action` can
            // resolve the worktree. We don't move the card's column — review
            // workspaces carry both freshly-promoted Review tasks and (for a
            // manual early review) still-InProgress ones, and busy-detection
            // keys off `assigned_to` across both columns, not the column alone.
            let original = tf.task.clone();
            tf.task.assigned_to = Some(ws.name.clone());
            tf.task.branch = Some(branch.clone());
            tf.task.updated_at = chrono::Utc::now();
            shelbi_state::save_task(project_name, &tf.task, &tf.body)?;

            let addr = match load_review_workspace(StartSpec {
                project: &project,
                workspace: &ws,
                task_id,
                branch: &branch,
                task_body: &tf.body,
                // `None` → load_review_workspace forces the `review` agent.
                agent: None,
            }) {
                Ok(addr) => addr,
                Err(e) => {
                    // Roll the assignment back so a failed load doesn't strand
                    // the card pinned to a workspace that isn't running.
                    // Best-effort: the load error is what the user needs to see.
                    if let Err(re) = shelbi_state::save_task(project_name, &original, &tf.body) {
                        eprintln!(
                            "shelbi: review load for `{task_id}` failed and the assignment \
                             rollback also failed ({re}); run `shelbi task assign {task_id} \
                             --to <workspace>` to fix the board"
                        );
                    }
                    return Err(e);
                }
            };

            let port = review_workspace_port(&project, &ws);
            Ok(ReviewOutcome::Loaded {
                workspace: ws.name,
                host: machine.host(),
                machine: machine.name,
                addr,
                port,
            })
        }
        SlotPick::AllBusy(busy) => {
            let review_names: HashSet<&str> =
                reviews.iter().map(|w| w.name.as_str()).collect();
            let position = queue_position(&active, task_id, &review_names);
            Ok(ReviewOutcome::Queued {
                machine: machine.name,
                position,
                busy,
            })
        }
    }
}

/// Look up the project + task on disk and load it onto a review workspace,
/// returning the tmux target (`session:window`) of the review pane the caller
/// should focus. Used by the TUI sidebar and the Ctrl+P palette so they share
/// one code path with the CLI. A queued task has no pane to focus, so it
/// surfaces as an error carrying the queue position.
pub fn start_review_by_id(project_name: &str, task_id: &str) -> Result<String> {
    match review_task(project_name, task_id, None)? {
        ReviewOutcome::Loaded { addr, .. } => Ok(addr.target()),
        ReviewOutcome::Queued { machine, position, .. } => Err(Error::Other(format!(
            "all review workspaces on `{machine}` are busy — `{task_id}` is queued at \
             position {position} and will load when one frees"
        ))),
    }
}

/// Resolve which machine to review on. Preference order: explicit override, the
/// machine of the task's assigned workspace **if it declares review
/// workspaces**, else the first machine (declaration order) that declares any.
///
/// Callers should run the project-wide onboarding check first; the final
/// `Err` here is a defensive backstop for the "no machine has a review
/// workspace" case.
pub fn resolve_review_machine<'a>(
    project: &'a Project,
    task: &Task,
    explicit: Option<&str>,
) -> Result<&'a Machine> {
    if let Some(name) = explicit {
        return project
            .machine(name)
            .ok_or_else(|| Error::UnknownMachine(name.to_string()));
    }
    if let Some(workspace_name) = &task.assigned_to {
        if let Some(workspace) = project.workspace(workspace_name) {
            if !project.review_workspaces(&workspace.machine).is_empty() {
                if let Some(m) = project.machine(&workspace.machine) {
                    return Ok(m);
                }
            }
        }
    }
    project
        .machines
        .iter()
        .find(|m| !project.review_workspaces(&m.name).is_empty())
        .ok_or_else(|| {
            Error::Other("no machine declares a `role: review` workspace".into())
        })
}

/// Which review workspace (if any) this task should load onto.
enum SlotPick<'a> {
    Free(&'a shelbi_core::WorkspaceSpec),
    AllBusy(Vec<BusyReviewSlot>),
}

/// Pick a review workspace for `this_task` among `reviews` (a machine's review
/// workspaces, in declaration order), given the currently `active` tasks
/// (in-progress + review). Reuses the slot this task is already on (a re-review
/// / refresh), otherwise takes the first free slot; returns the busy list when
/// all are occupied. Pure so it can be unit-tested without disk.
fn pick_review_slot<'a>(
    reviews: &[&'a shelbi_core::WorkspaceSpec],
    active: &[TaskFile],
    this_task: &str,
) -> SlotPick<'a> {
    let review_names: HashSet<&str> = reviews.iter().map(|w| w.name.as_str()).collect();

    // Map each occupied review workspace → the task holding it. First writer
    // wins so a stray duplicate assignment can't mask the original holder.
    let mut busy_map: HashMap<&str, &str> = HashMap::new();
    for tf in active {
        if let Some(ws) = tf.task.assigned_to.as_deref() {
            if review_names.contains(ws) {
                busy_map.entry(ws).or_insert(tf.task.id.as_str());
            }
        }
    }

    // Refresh: this task is already loaded on a review workspace → reuse it.
    for w in reviews {
        if busy_map.get(w.name.as_str()).copied() == Some(this_task) {
            return SlotPick::Free(w);
        }
    }
    // First free review workspace in declaration order.
    for w in reviews {
        if !busy_map.contains_key(w.name.as_str()) {
            return SlotPick::Free(w);
        }
    }
    // All busy — report what's holding each slot, in declaration order.
    let busy = reviews
        .iter()
        .filter_map(|w| {
            busy_map.get(w.name.as_str()).map(|t| BusyReviewSlot {
                workspace: w.name.clone(),
                task_id: (*t).to_string(),
            })
        })
        .collect();
    SlotPick::AllBusy(busy)
}

/// 1-based position of `this_task` in the review queue: the Review-column tasks
/// not currently loaded on a review workspace, ordered by priority then age. A
/// task not (yet) in Review sorts to the back. Pure for unit-testing.
fn queue_position(active: &[TaskFile], this_task: &str, review_names: &HashSet<&str>) -> usize {
    let mut waiting: Vec<&Task> = active
        .iter()
        .map(|tf| &tf.task)
        .filter(|t| t.column == Column::Review)
        .filter(|t| {
            t.assigned_to
                .as_deref()
                .map_or(true, |w| !review_names.contains(w))
        })
        .collect();
    waiting.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    match waiting.iter().position(|t| t.id == this_task) {
        Some(i) => i + 1,
        None => waiting.len() + 1,
    }
}

/// If a workspace worktree on this machine is currently on `branch`, switch
/// it to `default_branch` so the branch ref is free for another worktree to
/// claim. Bails on a dirty workspace worktree (we'd silently lose work).
///
/// `sync_worktree` reuses this on the dispatch path (F14): re-dispatching a
/// task whose branch is live in another workspace's worktree would otherwise
/// die on `fatal: '<branch>' is already checked out`. It's safe to call from
/// there because the dispatch only reaches its checkout when the *target*
/// worktree's HEAD is already off `branch`, so this never detaches the
/// worktree it's about to check the branch back out into.
pub(crate) fn release_branch_from_workspace_worktrees(
    host: &Host,
    project: &Project,
    machine: &Machine,
    branch: &str,
) -> Result<()> {
    for workspace in &project.workspaces {
        if workspace.machine != machine.name {
            continue;
        }
        let wt: PathBuf = workspace_worktree(machine, workspace);
        let wt_str = wt.to_string_lossy().into_owned();
        // Skip workspaces without an actual worktree yet.
        let exists = shelbi_ssh::run(host, ["test", "-e", &format!("{wt_str}/.git")])
            .map_err(Error::Io)?
            .status
            .success();
        if !exists {
            continue;
        }
        let head = shelbi_ssh::run_capture(
            host,
            ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
        )?;
        if head.trim() != branch {
            continue;
        }
        let dirty = shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(Error::Other(format!(
                "workspace `{}`'s worktree is on `{branch}` with uncommitted \
                 changes — commit, stash, or discard before reviewing",
                workspace.name
            )));
        }
        // Detach HEAD on the workspace's worktree — frees the branch ref so
        // another worktree can claim it. We avoid switching to a named branch
        // here because the natural choice (`default_branch`) is typically
        // checked out elsewhere, and git refuses to double-claim a branch
        // across worktrees. sync_worktree will re-attach to the right branch
        // the next time the workspace gets a task.
        let out = shelbi_ssh::run(host, ["git", "-C", &wt_str, "checkout", "--detach"])
            .map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: format!("git -C {wt_str} checkout --detach"),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }
    Ok(())
}

/// Move `branch` off whatever workspace worktree currently holds it and check
/// it out in `review_worktree`. Generalizes
/// [`release_branch_from_workspace_worktrees`] for the review-*load* path:
/// releasing (detaching) the dev worktree that produced the branch frees the
/// ref so git will let the review worktree claim it, then we check it out
/// there.
///
/// The machine's top-level clone is never touched — only workspace worktrees
/// swap the branch. A review *workspace* is a separate persistent worktree
/// under `.shelbi/wt/<name>`.
///
/// Assumes `review_worktree` already exists and is not itself on `branch`;
/// [`crate::workspace`]'s `sync_review_worktree` guarantees both before
/// calling.
pub(crate) fn move_branch_to_review_worktree(
    host: &Host,
    project: &Project,
    machine: &Machine,
    review_worktree: &std::path::Path,
    branch: &str,
) -> Result<()> {
    release_branch_from_workspace_worktrees(host, project, machine, branch)?;
    let wt_str = review_worktree.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["git", "-C", &wt_str, "checkout", branch])
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt_str} checkout {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::model::WorkspaceRole;
    use shelbi_core::{AgentRunnerSpec, MachineKind, OrchestratorSpec, WorkspaceSpec};
    use std::collections::BTreeMap;

    fn ws(name: &str, machine: &str, role: WorkspaceRole) -> WorkspaceSpec {
        WorkspaceSpec {
            name: name.into(),
            machine: machine.into(),
            runner: "claude".into(),
            role,
        }
    }

    fn task(id: &str, column: Column, assigned: Option<&str>, priority: u32) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column,
            priority,
            assigned_to: assigned.map(str::to_string),
            workflow: None,
            branch: Some(format!("shelbi/{id}")),
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        }
    }

    fn tf(t: Task) -> TaskFile {
        TaskFile { task: t, body: String::new() }
    }

    fn project(workspaces: Vec<WorkspaceSpec>) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![], dialog_signatures: vec![] },
        );
        Project {
            name: "p".into(),
            repo: "r".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/p".into(),
                    host: None,
                },
                Machine {
                    name: "m2".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/p".into(),
                    host: Some("m2.local".into()),
                },
            ],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces,
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            review: Default::default(),
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn machine_resolution_prefers_assigned_workspace_machine_with_review_slots() {
        let p = project(vec![
            ws("dev-hub", "hub", WorkspaceRole::Dev),
            ws("review-hub", "hub", WorkspaceRole::Review),
            ws("dev-m2", "m2", WorkspaceRole::Dev),
            ws("review-m2", "m2", WorkspaceRole::Review),
        ]);
        let t = task("x", Column::Review, Some("dev-m2"), 0);
        let m = resolve_review_machine(&p, &t, None).unwrap();
        assert_eq!(m.name, "m2");
    }

    #[test]
    fn machine_resolution_skips_assigned_machine_without_review_slots() {
        // Task ran on hub (no review slot there); fall back to the first
        // machine that has one — m2.
        let p = project(vec![
            ws("dev-hub", "hub", WorkspaceRole::Dev),
            ws("review-m2", "m2", WorkspaceRole::Review),
        ]);
        let t = task("x", Column::Review, Some("dev-hub"), 0);
        let m = resolve_review_machine(&p, &t, None).unwrap();
        assert_eq!(m.name, "m2");
    }

    #[test]
    fn machine_resolution_honors_explicit_override() {
        let p = project(vec![ws("review-m2", "m2", WorkspaceRole::Review)]);
        let t = task("x", Column::Review, None, 0);
        let m = resolve_review_machine(&p, &t, Some("m2")).unwrap();
        assert_eq!(m.name, "m2");
    }

    #[test]
    fn pick_takes_first_free_review_workspace() {
        let r1 = ws("review-1", "hub", WorkspaceRole::Review);
        let r2 = ws("review-2", "hub", WorkspaceRole::Review);
        let reviews = vec![&r1, &r2];
        let active = vec![tf(task("a", Column::Review, Some("review-1"), 0))];
        match pick_review_slot(&reviews, &active, "b") {
            SlotPick::Free(w) => assert_eq!(w.name, "review-2"),
            SlotPick::AllBusy(_) => panic!("expected a free slot"),
        }
    }

    #[test]
    fn pick_reuses_the_slot_this_task_is_already_on() {
        let r1 = ws("review-1", "hub", WorkspaceRole::Review);
        let r2 = ws("review-2", "hub", WorkspaceRole::Review);
        let reviews = vec![&r1, &r2];
        // review-1 free, review-2 holds our task → refresh reuses review-2.
        let active = vec![tf(task("b", Column::Review, Some("review-2"), 0))];
        match pick_review_slot(&reviews, &active, "b") {
            SlotPick::Free(w) => assert_eq!(w.name, "review-2"),
            SlotPick::AllBusy(_) => panic!("expected reuse of review-2"),
        }
    }

    #[test]
    fn pick_reports_all_busy_with_holders() {
        let r1 = ws("review-1", "hub", WorkspaceRole::Review);
        let r2 = ws("review-2", "hub", WorkspaceRole::Review);
        let reviews = vec![&r1, &r2];
        let active = vec![
            tf(task("a", Column::Review, Some("review-1"), 0)),
            tf(task("b", Column::Review, Some("review-2"), 0)),
        ];
        match pick_review_slot(&reviews, &active, "c") {
            SlotPick::AllBusy(busy) => {
                assert_eq!(
                    busy,
                    vec![
                        BusyReviewSlot { workspace: "review-1".into(), task_id: "a".into() },
                        BusyReviewSlot { workspace: "review-2".into(), task_id: "b".into() },
                    ]
                );
            }
            SlotPick::Free(_) => panic!("expected all busy"),
        }
    }

    #[test]
    fn queue_position_orders_unloaded_review_tasks_by_priority() {
        let names: HashSet<&str> = ["review-1"].into_iter().collect();
        let active = vec![
            tf(task("loaded", Column::Review, Some("review-1"), 0)),
            tf(task("first", Column::Review, None, 0)),
            tf(task("second", Column::Review, None, 1)),
        ];
        // "loaded" is on a review workspace → excluded from the queue.
        assert_eq!(queue_position(&active, "first", &names), 1);
        assert_eq!(queue_position(&active, "second", &names), 2);
        // A task not among the waiting set sorts to the back.
        assert_eq!(queue_position(&active, "unknown", &names), 3);
    }
}
