//! `shelbi workspace <subcommand>` — manage the project's declared workspace
//! pool. Workspaces are durable slots (one worktree each); tasks come and go.
//! See [`shelbi_orchestrator::workspace`] for the lifecycle primitives.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use shelbi_core::{Column, IntegrationMode, Issue, WorkspaceSpec};
use shelbi_orchestrator::pool_admin::{self, PoolError};
use shelbi_orchestrator::workspace as orch_workspace;
use shelbi_state::WorkspaceStatus;

use super::require_project;

/// Default agent name surfaced for any in-progress task whose frontmatter
/// doesn't pin an explicit `agent:` (the long-term plan threads this
/// through the task status; until then "developer" matches the only
/// task-running agent the scaffold materializes).
const DEFAULT_TASK_AGENT: &str = "developer";

/// Idle-row placeholder for the AGENT column. Plain text (no glyph) so the
/// column reads cleanly in a non-fancy terminal.
const IDLE_AGENT_CELL: &str = "-";

/// Exit code for `shelbi workspace add` when it refuses to touch a
/// user-authored `.claude/settings.local.json`. Distinct from the generic
/// failure code (1) so the orchestrator (or a script) can tell "merge these
/// hooks and re-run" apart from any other error.
pub const SETTINGS_MERGE_REQUIRED_EXIT: i32 = 3;

#[derive(Debug, Subcommand)]
pub enum WorkspaceCmd {
    /// Add a workspace to the project's pool on the local machine and
    /// materialize its worktree (wiring Shelbi's Claude hooks into
    /// `.claude/settings.local.json`). Refuses a name that already exists or
    /// slug-collides with an existing workspace.
    Add {
        /// Name for the new workspace (task-id character set: lowercase
        /// letters, digits, `-`, `_`).
        name: String,
        /// Machine to place the workspace on. Defaults to the local hub.
        #[arg(long)]
        machine: Option<String>,
        /// Retired: a workspace no longer selects a runner — the dispatched
        /// agent owns runner/model resolution. Accepted-and-ignored so an
        /// existing script keeps working; will be removed in a later release.
        #[arg(long, hide = true)]
        runner: Option<String>,
    },
    /// Remove a workspace from the pool and tear down its worktree + pane.
    /// Refuses a workspace with an active task, or whose worktree holds
    /// uncommitted or unpushed work, unless `--force`.
    Rm {
        name: String,
        /// Remove even if the workspace holds an in-progress/review task or
        /// its worktree holds uncommitted or unpushed work.
        #[arg(long)]
        force: bool,
    },
    /// List declared workspaces with their host, currently loaded agent (or
    /// `-` when idle), integration tier, and state
    /// (`idle` / `in_progress: <task-id>`).
    List,
    /// Kill a workspace's tmux pane. Releases the workspace's in-flight task back
    /// to `todo` (unassigned) so the board doesn't show an orphaned
    /// in_progress card; pass `--keep-task` to leave the task in place.
    Stop {
        name: String,
        /// Leave the in-flight task in `in_progress` with `assigned_to`
        /// pointing at this workspace. Use when you're about to restart the
        /// workspace on the same task and don't want the card to move.
        #[arg(long)]
        keep_task: bool,
    },
    /// Show observed workspace state from the hub-side poller. Reads
    /// `~/.shelbi/workspaces/<name>/status.yaml` files, no tmux probing.
    /// With NAME, prints a single row + the raw status.yaml.
    Status {
        /// Workspace to inspect. Omit to show every declared workspace.
        name: Option<String>,
    },
}

pub fn run(project_opt: Option<String>, cmd: WorkspaceCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    // Version gate: `add`/`rm`/`stop` mutate board/config state; the views
    // warn and proceed.
    match &cmd {
        WorkspaceCmd::List | WorkspaceCmd::Status { .. } => super::hub_version::warn_on_mismatch(),
        _ => super::hub_version::ensure_daemon_matches_for_mutation()?,
    }
    match cmd {
        WorkspaceCmd::Add {
            name,
            machine,
            runner,
        } => {
            if runner.is_some() {
                eprintln!(
                    "shelbi: --runner is retired and ignored — a workspace no longer selects a \
                     runner; the dispatched agent owns runner/model resolution"
                );
            }
            add(&project, &name, machine.as_deref())
        }
        WorkspaceCmd::Rm { name, force } => rm(&project, &name, force),
        WorkspaceCmd::List => list(&project),
        WorkspaceCmd::Stop { name, keep_task } => stop(&project, &name, keep_task),
        WorkspaceCmd::Status { name } => status(&project, name.as_deref()),
    }
}

/// `shelbi workspace add <name>` — append a workspace to the pool on the local
/// machine, materialize its worktree, and wire Shelbi's Claude hooks into
/// `.claude/settings.local.json`. The lifecycle lives in
/// [`pool_admin::add_workspace`], shared with the desktop.
///
/// The worktree is materialized and wired BEFORE the pool entry is persisted.
/// That's what makes the user-authored-settings merge flow work: on
/// [`PoolError::HooksMergeRequired`] the command exits with
/// [`SETTINGS_MERGE_REQUIRED_EXIT`] without having added the name, so when the
/// orchestrator merges the hooks and re-runs `workspace add <name>`, the
/// name-collision guard doesn't refuse the re-run.
fn add(project: &str, name: &str, machine: Option<&str>) -> Result<()> {
    let req = pool_admin::AddWorkspace {
        name: name.to_string(),
        machine: machine.map(str::to_string),
        tags: Vec::new(),
        slot: None,
    };
    match pool_admin::add_workspace(project, &req) {
        Ok(added) => {
            if added.hooks == pool_admin::HooksOutcome::SelfHealed {
                eprintln!("{}", orch_workspace::settings_selfheal_notice(name));
            }
            println!(
                "✓ added workspace `{name}` (machine {})",
                added.workspace.machine
            );
            Ok(())
        }
        Err(PoolError::HooksMergeRequired { message, .. }) => {
            // A user-authored settings.local.json whose shape Shelbi can't
            // safely self-heal. Print the structured, orchestrator-addressed
            // message and exit with a DISTINCT status; the pool entry wasn't
            // persisted.
            eprintln!("{message}");
            std::process::exit(SETTINGS_MERGE_REQUIRED_EXIT);
        }
        Err(e) => Err(anyhow!(e)),
    }
}

/// `shelbi workspace rm <name>` — remove a workspace from the pool and tear
/// down its worktree + pane ([`pool_admin::remove_workspace`]). Refuses a
/// workspace holding an active (in-progress/review) task, or whose worktree
/// holds uncommitted or unpushed work, unless `--force`.
fn rm(project: &str, name: &str, force: bool) -> Result<()> {
    let req = pool_admin::RemoveWorkspace {
        name: name.to_string(),
        expected: None,
        force,
    };
    match pool_admin::remove_workspace(project, &req) {
        Ok(removed) => {
            for warning in &removed.warnings {
                eprintln!("warning: {warning}");
            }
            println!("✓ removed workspace `{name}`");
            Ok(())
        }
        Err(PoolError::ActiveTask { task, .. }) => Err(anyhow!(
            "workspace `{name}` has an active task `{task}` — release it \
             (`shelbi workspace stop {name}`) or pass --force"
        )),
        Err(PoolError::Unsafe(reason)) if !force => Err(anyhow!("{reason} (or pass --force)")),
        Err(e) => Err(anyhow!(e)),
    }
}

fn list(project: &str) -> Result<()> {
    print_workspaces(project)
}

/// The workspace-pool rendering used by both `shelbi workspace list` and
/// the `## Workspaces` section of `shelbi status --full`. Extracted so
/// the bootstrap snapshot reuses the exact table shape callers already
/// depend on.
pub(crate) fn print_workspaces(project: &str) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    if p.workspaces.is_empty() {
        println!(
            "(no workspaces declared in {project} — add a `workspaces:` block to the project YAML)"
        );
        return Ok(());
    }

    // Both the in-progress and review columns come from the daemon-owned
    // `board-index.json` (§5) in one read, filtered in memory — never a pair of
    // backend `list_in_status` sweeps from this CLI. Both are open (non-terminal)
    // columns, so the open index carries them.
    let board = shelbi_state::read_board(project)
        .map_err(|e| anyhow!(e))?
        .into_issues();

    // Surfaces every in-progress task assigned to the workspace. There
    // should normally be at most one, but if shelbi's state diverged we
    // print all of them in the STATE cell so the user sees the mess.
    let in_progress: Vec<&shelbi_state::IssueFile> = board
        .iter()
        .filter(|tf| tf.task.column == Column::in_progress())
        .collect();
    let assigned: Vec<&Issue> = in_progress.iter().map(|tf| &tf.task).collect();

    // A `review`-tagged slot holds a review-column (handoff) task while it
    // serves the branch for inspection — that task never sits in `in_progress`,
    // so without this it looks like a live pane no active task points at and
    // gets mislabeled `orphaned session`. Map each review slot to the task it's
    // serving so `list` renders it as `review: <id>` (and the probe skips it).
    let mut review: Vec<shelbi_state::IssueFile> = board
        .iter()
        .filter(|tf| tf.task.column == Column::review())
        .cloned()
        .collect();
    // Review-slot ownership comes from the local assignment overlay, not the
    // index's publish-time `assigned_to` fold, which can lag a fresh review load
    // and mislabel the serving slot `orphaned session`.
    shelbi_state::fold_assignment_overlay(project, &p.issue_tracker, &mut review);
    let review_by_ws = review_assignments(&p, &review);

    let occupied = occupied_idle_workspaces(&p, &assigned, &review_by_ws)?;
    let modes = workspace_integration_modes(&p);
    for line in render_list_with_occupied(&p.workspaces, &assigned, &review_by_ws, &occupied, &modes)?
    {
        println!("{line}");
    }

    // Cutover: call out workspaces still pending the tmux→session migration, so
    // the user can see why dispatch to them is paused. Only surfaced when
    // something is actually pending, so the table is unchanged for a
    // fully-migrated install.
    {
        if let Ok(states) = shelbi_state::all_workspace_migration_states(project) {
            let pending: Vec<&str> = p
                .workspaces
                .iter()
                .filter(|w| {
                    matches!(
                        states.get(&w.name),
                        Some(shelbi_state::MigrationState::Pending)
                    )
                })
                .map(|w| w.name.as_str())
                .collect();
            if !pending.is_empty() {
                println!(
                    "\nmigration pending: {} — dispatch paused until each workspace's \
                     previous-runtime tmux session is confirmed gone (reopen the project \
                     to re-run migration)",
                    pending.join(", ")
                );
            }
        }
    }
    Ok(())
}

/// Map each `review`-tagged workspace to the review-column task currently
/// assigned to it (`workspace name -> task id`). A review slot serving a
/// handoff task is legitimately occupied — not an orphaned session — so both
/// the probe ([`occupied_idle_workspaces`]) and the renderer
/// ([`render_list_with_occupied`]) consult this to treat it as busy.
fn review_assignments(
    project: &shelbi_core::Project,
    review: &[shelbi_state::IssueFile],
) -> BTreeMap<String, String> {
    review
        .iter()
        .filter_map(|tf| {
            let ws = tf.task.assigned_to.as_deref()?;
            let spec = project.workspace(ws)?;
            project
                .effective_tags(spec)
                .contains("review")
                .then(|| (ws.to_string(), tf.task.id.clone()))
        })
        .collect()
}

/// The integration transport tier for each declared workspace, keyed by
/// workspace name. Workspace agents don't run the native Codex bridge; their
/// tier is decided by whether Shelbi can push/verify against the runner —
/// Claude Code and Codex panes get the verified tmux-submission + OSC-hook
/// contract (`conventional`), anything Shelbi can only poll is `degraded`.
fn workspace_integration_modes(
    project: &shelbi_core::Project,
) -> BTreeMap<String, IntegrationMode> {
    // A workspace no longer selects a runner; its integration tier follows the
    // project's baseline runner (uniform-fleet assumption).
    let mode = project
        .default_runner_spec()
        .map(|runner| workspace_integration_mode(&runner.command))
        .unwrap_or(IntegrationMode::Degraded);
    project
        .workspaces
        .iter()
        .map(|workspace| (workspace.name.clone(), mode))
        .collect()
}

/// Classify a workspace runner command into its integration tier. A runner
/// Shelbi knows how to wake with verified submission and read OSC pane-title
/// markers from (Claude Code, Codex) is `conventional`; an unrecognized runner
/// is `degraded` (polling contract only).
fn workspace_integration_mode(runner_command: &str) -> IntegrationMode {
    match shelbi_agent::RunnerAdapter::for_command(runner_command).kind() {
        shelbi_core::RunnerKind::Claude | shelbi_core::RunnerKind::Codex => {
            IntegrationMode::Conventional
        }
        shelbi_core::RunnerKind::Generic => IntegrationMode::Degraded,
    }
}

/// Why an idle workspace's STATE cell isn't the plain `idle` token. A
/// **user shell** is the sidebar's click-an-idle-workspace pane
/// (deliberate, user-occupied, not dispatchable — `shelbi task start`
/// refuses it); an **orphaned session** is any other leftover allocation
/// (e.g. an agent pane whose task was moved away), which dispatch will
/// reset as before; **unreachable** means the workspace's machine couldn't
/// be probed (timeout or transport failure) and carries the one-line
/// reason to render.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OccupiedKind {
    UserShell,
    Orphaned,
    Unreachable(String),
}

impl OccupiedKind {
    fn state_cell(&self) -> String {
        match self {
            OccupiedKind::UserShell => "occupied (user shell)".to_string(),
            OccupiedKind::Orphaned => "orphaned session".to_string(),
            OccupiedKind::Unreachable(reason) => format!("unreachable ({reason})"),
        }
    }
}

fn occupied_idle_workspaces(
    project: &shelbi_core::Project,
    in_progress: &[&Issue],
    review_by_ws: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, OccupiedKind>> {
    // A slot assigned an in-progress task OR serving a review-column task is
    // already accounted for by the renderer (`in_progress: <id>` / `review:
    // <id>`) — skip it here so a live review pane is never probed into an
    // `orphaned session` verdict, and so we don't burn a probe deadline on it.
    let assigned: BTreeSet<&str> = in_progress
        .iter()
        .filter_map(|t| t.assigned_to.as_deref())
        .chain(review_by_ws.keys().map(|s| s.as_str()))
        .collect();
    let deadline = orch_workspace::probe_deadline();
    // One unreachable verdict per *machine*, not per workspace: once a
    // machine's probe times out or fails, its remaining workspaces inherit
    // the verdict without burning another deadline each — that's what keeps
    // `workspace list` bounded (~one deadline per dead machine) instead of
    // deadline × workspaces.
    let mut machine_down: BTreeMap<String, String> = BTreeMap::new();
    let mut occupied = BTreeMap::new();
    for workspace in &project.workspaces {
        if assigned.contains(workspace.name.as_str()) {
            continue;
        }
        if let Some(reason) = machine_down.get(&workspace.machine) {
            occupied.insert(
                workspace.name.clone(),
                OccupiedKind::Unreachable(reason.clone()),
            );
            continue;
        }
        let machine = project.machine(&workspace.machine).ok_or_else(|| {
            anyhow!(
                "workspace `{}` references unknown machine `{}`",
                workspace.name,
                workspace.machine
            )
        })?;
        let host = machine.host();
        let addr =
            orch_workspace::workspace_target(project, workspace).map_err(|e| anyhow!(e))?;
        match orch_workspace::probe_workspace_slot(&host, &addr, deadline) {
            orch_workspace::SlotProbe::Dead => {}
            orch_workspace::SlotProbe::Alive { user_shell } => {
                let kind = if user_shell {
                    OccupiedKind::UserShell
                } else {
                    OccupiedKind::Orphaned
                };
                occupied.insert(workspace.name.clone(), kind);
            }
            orch_workspace::SlotProbe::Unreachable { reason } => {
                machine_down.insert(workspace.machine.clone(), reason.clone());
                occupied.insert(workspace.name.clone(), OccupiedKind::Unreachable(reason));
            }
        }
    }
    Ok(occupied)
}

/// Render the `shelbi workspace list` table: a header row followed by one
/// row per workspace. Pure so the column rendering can be tested without
/// touching stdout. The caller has already filtered `in_progress` to
/// `Column::in_progress()` tasks — we still re-filter by `assigned_to` per
/// workspace.
///
/// Errors when a workspace references an undeclared machine: that's a
/// project YAML bug the user should fix, and surfacing it from `list` is
/// the same behavior as the old per-row `machine().ok_or_else(...)` path.
#[cfg(test)]
fn render_list(
    workspaces: &[WorkspaceSpec],
    in_progress: &[&Issue],
) -> Result<Vec<String>> {
    render_list_with_occupied(
        workspaces,
        in_progress,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
}

fn render_list_with_occupied(
    workspaces: &[WorkspaceSpec],
    in_progress: &[&Issue],
    review_by_ws: &BTreeMap<String, String>,
    occupied_idle: &BTreeMap<String, OccupiedKind>,
    integration: &BTreeMap<String, IntegrationMode>,
) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(workspaces.len() + 1);
    out.push(format!(
        "{:<12} {:<8} {:<14} {:<13} {}",
        "NAME", "HOST", "AGENT", "INTEG", "STATE"
    ));
    for workspace in workspaces {
        let mine: Vec<&Issue> = in_progress
            .iter()
            .copied()
            .filter(|t| t.assigned_to.as_deref() == Some(workspace.name.as_str()))
            .collect();
        let (agent, state) = if mine.is_empty() {
            // A review slot serving a handoff task reads as `review: <id>`
            // (agent `review`), never `orphaned session` — its loaded task
            // just lives in the review column, not `in_progress`. Checked
            // before the occupied-idle probe so a live review pane is never
            // misclassified.
            if let Some(task_id) = review_by_ws.get(&workspace.name) {
                (
                    shelbi_state::REVIEW_AGENT.to_string(),
                    format!("review: {task_id}"),
                )
            } else {
                let state = match occupied_idle.get(&workspace.name) {
                    Some(kind) => kind.state_cell(),
                    None => "idle".to_string(),
                };
                (IDLE_AGENT_CELL.to_string(), state)
            }
        } else {
            // `agent:` from the task frontmatter wins when present —
            // matches the same lookup the task-start path uses to load
            // agent instructions/skills. Multiple assignments shouldn't
            // happen but the first one's agent is the best we can render
            // in a single cell.
            let agent = mine[0]
                .param_str("agent")
                .unwrap_or(DEFAULT_TASK_AGENT)
                .to_string();
            let ids = mine
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            (agent, format!("in_progress: {ids}"))
        };
        // Missing entry (the bare `render_list` helper passes an empty map)
        // defaults to `conventional` — the tier an ordinary Claude Code
        // workspace runs at.
        let integ = integration
            .get(&workspace.name)
            .copied()
            .unwrap_or(IntegrationMode::Conventional);
        out.push(format!(
            "{:<12} {:<8} {:<14} {:<13} {}",
            workspace.name, workspace.machine, agent, integ, state
        ));
    }
    Ok(out)
}

fn stop(project: &str, name: &str, keep_task: bool) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let workspace = p.workspace(name).ok_or_else(|| {
        anyhow!(
            "workspace `{name}` not declared in project `{project}` (known: {})",
            p.workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let machine = p.machine(&workspace.machine).ok_or_else(|| {
        anyhow!(
            "workspace references unknown machine `{}`",
            workspace.machine
        )
    })?;
    let host = machine.host();
    let addr = orch_workspace::workspace_target(&p, workspace).map_err(|e| anyhow!(e))?;

    // Release/park the task BEFORE killing the pane. The stranded-slot resume
    // only acts on a *dead* review pane whose task is still assigned on disk;
    // parking (unassign + marker) while the pane is still alive means no tick
    // can observe the (dead + assigned) state that would trigger a resume, so
    // there's no window for the churn loop. `--keep-task` skips this entirely
    // (a bare pane bounce that resume is meant to bring back).
    if !keep_task {
        // A review slot serves a handoff-category task that lives in the review
        // column, not `in_progress`. Releasing it to `todo` (the dev-slot path
        // below) would be wrong — it belongs in review — and leaving it
        // assigned makes the stranded-slot resume relaunch the pane on the next
        // tick (the churn loop). So *park* it: unassign it from the slot and
        // mark it so neither the auto-loader nor the resume re-grabs it. It
        // stays in the review column, and the slot returns to idle.
        if p.effective_tags(workspace).contains("review") {
            for id in park_review_workspace_tasks(project, name)? {
                println!("✓ {id} unloaded from review (parked — won't auto-reload)");
            }
        }
        // Also release any in-progress task assigned here. Normally a review
        // slot holds none, but running this unconditionally reconciles a slot
        // whose state diverged (an in-progress card still pinned to it) rather
        // than leaving it dangling.
        for id in release_workspace_tasks(project, name)? {
            println!("✓ {id} released → todo (was assigned to {name})");
        }
    }

    orch_workspace::kill_workspace_pane(&host, &addr, &workspace.name).map_err(|e| anyhow!(e))?;
    println!("✓ {name} pane stopped");
    Ok(())
}

/// Park every review-column task assigned to `workspace_name`: unassign it and
/// set its parked marker so the auto-loader and the stranded-slot resume leave
/// it alone. Returns the parked task ids. There is normally at most one, but a
/// diverged board is reconciled by parking them all. Used by [`stop`] on a
/// `review`-tagged slot.
fn park_review_workspace_tasks(project: &str, workspace_name: &str) -> Result<Vec<String>> {
    let store = shelbi_state::issue_store_for(project).map_err(|e| anyhow!(e))?;
    let review = store
        .list_in_status(&Column::review())
        .map_err(|e| anyhow!(e))?;
    let mut parked = Vec::new();
    for tf in review {
        if tf.task.assigned_to.as_deref() != Some(workspace_name) {
            continue;
        }
        let id = tf.task.id.clone();
        store.park_review(&id).map_err(|e| anyhow!(e))?;
        parked.push(id);
    }
    Ok(parked)
}

fn status(project: &str, name: Option<&str>) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;

    if let Some(only) = name {
        if p.workspace(only).is_none() {
            return Err(anyhow!(
                "workspace `{only}` not declared in project `{project}` (known: {})",
                p.workspaces
                    .iter()
                    .map(|w| w.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        print_status_table(&[only.to_string()])?;
        println!();
        let path = shelbi_state::workspace_status_path(only).map_err(|e| anyhow!(e))?;
        if path.exists() {
            let yaml = fs::read_to_string(&path)
                .map_err(|e| anyhow!("reading {}: {e}", path.display()))?;
            println!("--- {}", path.display());
            print!("{yaml}");
            if !yaml.ends_with('\n') {
                println!();
            }
        } else {
            println!("(no status.yaml yet — workspace hasn't been polled)");
        }
        return Ok(());
    }

    if p.workspaces.is_empty() {
        println!("(no workspaces declared in {project})");
        return Ok(());
    }
    let names: Vec<String> = p.workspaces.iter().map(|w| w.name.clone()).collect();
    print_status_table(&names)
}

fn print_status_table(names: &[String]) -> Result<()> {
    let now = Utc::now();
    println!(
        "{:<12} {:<24} {:<14} {:<12} IN STATE",
        "WORKSPACE", "TASK", "STATE", "LAST SEEN"
    );
    for name in names {
        match shelbi_state::load_workspace_status(name) {
            Ok(Some(s)) => println!(
                "{:<12} {:<24} {:<14} {:<12} {}",
                s.workspace,
                task_cell(&s),
                s.state.as_str(),
                format_ago(now, s.last_seen),
                format_ago(now, s.last_transition),
            ),
            Ok(None) => println!("{:<12} {:<24} {:<14} {:<12} —", name, "—", "?", "never"),
            // A corrupt or unreadable status.yaml for one workspace must
            // not blank out the whole fleet table. Surface the failure on
            // its own row (and to stderr) and keep rendering the rest.
            Err(e) => {
                eprintln!("workspace status: reading status for `{name}`: {e}");
                println!(
                    "{:<12} {:<24} {:<14} {:<12} —",
                    name, "(unreadable)", "err", "?"
                );
            }
        }
    }
    Ok(())
}

fn task_cell(s: &WorkspaceStatus) -> String {
    s.current_task
        .clone()
        .unwrap_or_else(|| "(idle)".to_string())
}

/// Compact "12s" / "5m" / "2h" / "3d" style age. Floors at the unit
/// boundary so the output stays narrow for the table.
fn format_ago(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds();
    if secs < 0 {
        // Clock skew on a remote-written status file — show 0 rather
        // than a negative number that'd confuse the reader.
        return "0s".to_string();
    }
    let s = secs as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// Unassign and move-to-todo every in-flight task currently owned by
/// `workspace_name`. Returns the released task ids in the order they were
/// processed. There should be at most one, but if state diverged we
/// release them all so the board doesn't keep dangling cards pointing at
/// a dead pane.
fn release_workspace_tasks(project: &str, workspace_name: &str) -> Result<Vec<String>> {
    let store = shelbi_state::issue_store_for(project).map_err(|e| anyhow!(e))?;
    let in_progress = store
        .list_in_status(&Column::in_progress())
        .map_err(|e| anyhow!(e))?;
    let mut released = Vec::new();
    for tf in in_progress {
        if tf.task.assigned_to.as_deref() != Some(workspace_name) {
            continue;
        }
        let id = tf.task.id.clone();
        // Unassign + move-to-todo in one atomic locked write. A prior
        // unassign-then-move split could crash between the two writes and
        // strand the card unowned-but-still-in-progress, where the
        // owner-keyed recovery scan would never see it again (F18).
        let moved = store
            .move_status_and_unassign(&id, &Column::todo(), "workspace:stop")
            .map_err(|e| anyhow!(e))?;
        if let Some(shelbi_state::StatusMove { from, to, workflow }) = moved {
            if let Err(e) =
                shelbi_state::append_task_event(project, &id, &workflow, from, to, "workspace:stop")
            {
                eprintln!("warning: append_task_event failed: {e}");
            }
        }
        released.push(id);
    }
    Ok(released)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-cli-workspace-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Register a minimal `p` project so board reads/writes routed through the
    /// `IssueStore` can resolve a (file-system) store. Kept off `fresh_home` so
    /// the `add`/`rm` layout tests still exercise project creation from scratch.
    fn register_project(home: &std::path::Path, name: &str) {
        let projects = home.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join(format!("{name}.yaml")),
            format!(
                "name: {name}\n\
repo: /tmp/{name}\n\
default_branch: main\n\
orchestrator:\n\
\x20 runner: claude\n\
agent_runners:\n\
\x20 claude:\n\
\x20\x20\x20 command: claude\n\
\x20\x20\x20 flags: []\n\
machines:\n\
\x20 - name: local\n\
\x20\x20\x20 kind: local\n\
\x20\x20\x20 work_dir: /tmp/{name}\n\
workspaces: []\n"
            ),
        )
        .unwrap();
    }

    /// Register a `github`-backed project `name` with a dev slot `alpha` and a
    /// `review`-tagged slot `review-1`, so `print_workspaces` reads a remote
    /// board the daemon owns (`board-index.json`, never `gh`).
    fn register_github_project_with_slots(home: &std::path::Path, name: &str) {
        let projects = home.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join(format!("{name}.yaml")),
            format!(
                "name: {name}\nrepo: /tmp/{name}\ndefault_branch: main\n\
orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n\
machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/{name}\n\
workspaces:\n  - name: alpha\n    machine: local\n  - name: review-1\n    machine: local\n    tags: [review]\n\
issue_tracker:\n  backend: github\n  github:\n    repo: owner/repo\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn workspace_list_reads_the_index_and_makes_no_gh_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        // `shelbi workspace list` on a `github` project reads the in-progress and
        // review columns from the daemon-owned index (§5) in one file read, not a
        // pair of backend `list_in_status` sweeps. A recording `gh` runner proves
        // no `gh` is spawned; the seeded index carries the assignments the render
        // reflects.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project_with_slots(&home, "gw");
        let mut idx = shelbi_state::BoardIndex::fresh(vec![
            shelbi_state::IssueFile {
                task: make_task("work-1", Column::in_progress(), 0, Some("alpha")),
                body: String::new(),
                tracker_assignees: Vec::new(),
            },
            shelbi_state::IssueFile {
                task: make_task("rev-1", Column::review(), 0, Some("review-1")),
                body: String::new(),
                tracker_assignees: Vec::new(),
            },
        ]);
        idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
        shelbi_state::write_board_index("gw", &idx).unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let rec = Arc::clone(&calls);
        shelbi_state::set_test_gh_runner(move |_| {
            rec.fetch_add(1, Ordering::SeqCst);
            Ok(String::new())
        });

        print_workspaces("gw").unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "workspace list must read the index, never issue a `gh` list"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn make_task(id: &str, column: Column, priority: u32, assigned_to: Option<&str>) -> Issue {
        let now = Utc::now();
        Issue {
            id: id.to_string(),
            title: id.replace('-', " "),
            column,
            priority,
            assigned_to: assigned_to.map(str::to_string),
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    fn make_workspace(name: &str, machine: &str) -> WorkspaceSpec {
        WorkspaceSpec {
            name: name.to_string(),
            machine: machine.to_string(),
            tags: Vec::new(),
            slot: None,
        }
    }

    #[test]
    fn render_list_emits_header_followed_by_one_row_per_workspace() {
        let workspaces = vec![
            make_workspace("alpha", "hub"),
            make_workspace("bravo", "hub"),
        ];
        let assigned = make_task("aw-task-1", Column::in_progress(), 0, Some("alpha"));
        let in_progress: Vec<&Issue> = vec![&assigned];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        assert_eq!(rows.len(), 3);
        // Header is the canonical column order — clients reading the
        // pipe-format depend on it.
        let header = &rows[0];
        let name_at = header.find("NAME").unwrap();
        let host_at = header.find("HOST").unwrap();
        let agent_at = header.find("AGENT").unwrap();
        let state_at = header.find("STATE").unwrap();
        assert!(name_at < host_at);
        assert!(host_at < agent_at);
        assert!(agent_at < state_at);
        // A workspace no longer selects a runner, so there is no RUNNER column.
        assert!(!header.contains("RUNNER"));
        assert!(!header.contains("CLAUDE"));
        assert!(!header.contains("MODEL"));
        // The integration tier column sits between AGENT and STATE so STATE
        // stays the trailing free-form cell.
        let integ_at = header.find("INTEG").unwrap();
        assert!(agent_at < integ_at);
        assert!(integ_at < state_at);
    }

    #[test]
    fn render_list_surfaces_per_workspace_integration_tier() {
        let workspaces = vec![
            make_workspace("alpha", "hub"),
            make_workspace("bravo", "hub"),
        ];
        let in_progress: Vec<&Issue> = Vec::new();
        // alpha runs a hook-capable runner (conventional); bravo's runner is
        // unrecognized, so it can only be polled (degraded).
        let modes = BTreeMap::from([
            ("alpha".to_string(), IntegrationMode::Conventional),
            ("bravo".to_string(), IntegrationMode::Degraded),
        ]);

        let rows = render_list_with_occupied(
            &workspaces,
            &in_progress,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &modes,
        )
        .unwrap();
        assert!(rows[1].contains("conventional"), "row: {}", rows[1]);
        assert!(rows[2].contains("degraded"), "row: {}", rows[2]);
        // STATE stays last: an idle workspace still ends with `idle`.
        assert!(rows[1].trim_end().ends_with("idle"), "row: {}", rows[1]);
    }

    #[test]
    fn workspace_integration_mode_classifies_runner_commands() {
        assert_eq!(
            workspace_integration_mode("claude"),
            IntegrationMode::Conventional
        );
        assert_eq!(
            workspace_integration_mode("/opt/homebrew/bin/codex"),
            IntegrationMode::Conventional
        );
        assert_eq!(
            workspace_integration_mode("some-unknown-runner"),
            IntegrationMode::Degraded
        );
    }

    #[test]
    fn render_list_active_workspace_surfaces_default_agent() {
        let workspaces = vec![make_workspace("alpha", "hub")];
        let task = make_task("aw-fix-login", Column::in_progress(), 0, Some("alpha"));
        let in_progress: Vec<&Issue> = vec![&task];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        let row = &rows[1];
        assert!(row.contains("alpha"), "row: {row}");
        assert!(row.contains("hub"), "row: {row}");
        // Tasks without an explicit `agent:` frontmatter fall back to the
        // default task agent.
        assert!(row.contains(DEFAULT_TASK_AGENT), "row: {row}");
        assert!(
            row.contains("in_progress: aw-fix-login"),
            "row should carry STATE cell: {row}"
        );
    }

    #[test]
    fn render_list_honors_explicit_agent_frontmatter() {
        let workspaces = vec![make_workspace("delta", "devbox")];
        let mut task = make_task("aw-write-tests", Column::in_progress(), 0, Some("delta"));
        task.params.insert("agent".into(), "qa".into());
        let in_progress: Vec<&Issue> = vec![&task];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        let row = &rows[1];
        // The task's `agent: qa` wins over the developer default.
        assert!(
            row.contains(" qa "),
            "row should carry AGENT=qa cell: {row}"
        );
        assert!(row.contains("in_progress: aw-write-tests"), "row: {row}");
    }

    #[test]
    fn render_list_idle_workspace_uses_placeholder_agent_and_idle_state() {
        let workspaces = vec![make_workspace("bravo", "hub")];
        // No in-progress tasks at all — bravo is idle.
        let in_progress: Vec<&Issue> = Vec::new();

        let rows = render_list(&workspaces, &in_progress).unwrap();
        let row = &rows[1];
        assert!(row.contains("bravo"), "row: {row}");
        // AGENT cell collapses to the idle placeholder, not the literal
        // `developer` (so an idle workspace doesn't masquerade as one
        // that's loaded the default agent).
        assert!(
            row.contains(&format!(" {IDLE_AGENT_CELL} ")),
            "row should carry AGENT={IDLE_AGENT_CELL} cell: {row}"
        );
        // STATE is the plain `idle` token. Not `in_progress: ...` because
        // there's no assigned task.
        assert!(row.trim_end().ends_with("idle"), "row: {row}");
        assert!(!row.contains("in_progress:"), "row: {row}");
    }

    #[test]
    fn render_list_marks_unassigned_live_slot_as_orphaned_session() {
        let workspaces = vec![make_workspace("delta", "devbox")];
        let in_progress: Vec<&Issue> = Vec::new();
        let occupied = BTreeMap::from([("delta".to_string(), OccupiedKind::Orphaned)]);

        let rows = render_list_with_occupied(
            &workspaces,
            &in_progress,
            &BTreeMap::new(),
            &occupied,
            &BTreeMap::new(),
        )
        .unwrap();
        let row = &rows[1];
        assert!(row.contains("delta"), "row: {row}");
        assert!(row.contains(&format!(" {IDLE_AGENT_CELL} ")), "row: {row}");
        assert!(row.trim_end().ends_with("orphaned session"), "row: {row}");
    }

    /// A `review`-tagged slot serving a handoff (review-column) task renders
    /// as `review: <id>`, never `orphaned session` — even though its loaded
    /// task is NOT in `in_progress`. This is the regression from the
    /// stranded-review-slot bug: a live review agent pane was mislabeled
    /// orphaned because only in-progress assignments counted as "occupied".
    #[test]
    fn render_list_review_slot_serving_handoff_is_not_orphaned() {
        let mut ws = make_workspace("review-1", "hub");
        ws.tags = vec!["review".to_string()];
        let workspaces = vec![ws];
        let in_progress: Vec<&Issue> = Vec::new();
        let review_by_ws =
            BTreeMap::from([("review-1".to_string(), "palette-second-column".to_string())]);
        // Even if a probe had (wrongly) flagged it orphaned, the review
        // assignment wins and the probe should have skipped it anyway.
        let occupied = BTreeMap::from([("review-1".to_string(), OccupiedKind::Orphaned)]);

        let rows = render_list_with_occupied(
            &workspaces,
            &in_progress,
            &review_by_ws,
            &occupied,
            &BTreeMap::new(),
        )
        .unwrap();
        let row = &rows[1];
        assert!(!row.contains("orphaned session"), "row: {row}");
        assert!(
            row.contains("review: palette-second-column"),
            "row should show the served task: {row}"
        );
        // AGENT cell reads `review`, not the idle placeholder.
        assert!(row.contains(" review "), "row: {row}");
    }

    /// A live slot carrying the user-shell mark (sidebar click on an idle
    /// workspace) renders as user-occupied, not as an orphaned session —
    /// that's the cell the orchestrator reads to skip the slot for
    /// dispatch while the user is in it.
    #[test]
    fn render_list_marks_user_shell_slot_as_occupied() {
        let workspaces = vec![make_workspace("delta", "devbox")];
        let in_progress: Vec<&Issue> = Vec::new();
        let occupied = BTreeMap::from([("delta".to_string(), OccupiedKind::UserShell)]);

        let rows = render_list_with_occupied(
            &workspaces,
            &in_progress,
            &BTreeMap::new(),
            &occupied,
            &BTreeMap::new(),
        )
        .unwrap();
        let row = &rows[1];
        assert!(row.contains("delta"), "row: {row}");
        assert!(row.contains(&format!(" {IDLE_AGENT_CELL} ")), "row: {row}");
        assert!(
            row.trim_end().ends_with("occupied (user shell)"),
            "row: {row}"
        );
        assert!(!row.contains("orphaned session"), "row: {row}");
    }

    /// A machine that can't be probed (SSH parked on interactive auth, or
    /// any transport failure) renders its idle workspaces as `unreachable`
    /// with the one-line reason — instead of hanging or aborting the table.
    #[test]
    fn render_list_marks_unprobeable_machine_workspaces_unreachable() {
        let workspaces = vec![make_workspace("delta", "devbox")];
        let in_progress: Vec<&Issue> = Vec::new();
        let occupied = BTreeMap::from([(
            "delta".to_string(),
            OccupiedKind::Unreachable(
                "ssh probe timed out after 5s (interactive auth pending?)".to_string(),
            ),
        )]);

        let rows = render_list_with_occupied(
            &workspaces,
            &in_progress,
            &BTreeMap::new(),
            &occupied,
            &BTreeMap::new(),
        )
        .unwrap();
        let row = &rows[1];
        assert!(row.contains("delta"), "row: {row}");
        assert!(row.contains(&format!(" {IDLE_AGENT_CELL} ")), "row: {row}");
        assert!(
            row.trim_end()
                .ends_with("unreachable (ssh probe timed out after 5s (interactive auth pending?))"),
            "row: {row}"
        );
        assert!(!row.contains("idle"), "row: {row}");
    }

    #[test]
    fn render_list_prefers_assigned_task_over_orphan_marker() {
        let workspaces = vec![make_workspace("delta", "devbox")];
        let task = make_task("bug-fix", Column::in_progress(), 0, Some("delta"));
        let in_progress: Vec<&Issue> = vec![&task];
        let occupied = BTreeMap::from([("delta".to_string(), OccupiedKind::Orphaned)]);

        let rows = render_list_with_occupied(
            &workspaces,
            &in_progress,
            &BTreeMap::new(),
            &occupied,
            &BTreeMap::new(),
        )
        .unwrap();
        let row = &rows[1];
        assert!(row.contains("in_progress: bug-fix"), "row: {row}");
        assert!(!row.contains("orphaned session"), "row: {row}");
    }

    #[test]
    fn render_list_only_counts_tasks_assigned_to_this_workspace() {
        // alpha has a task; bravo is on the same host but idle. The bravo
        // row should not show alpha's task.
        let workspaces = vec![
            make_workspace("alpha", "hub"),
            make_workspace("bravo", "hub"),
        ];
        let task = make_task("aw-fix-login", Column::in_progress(), 0, Some("alpha"));
        let in_progress: Vec<&Issue> = vec![&task];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        assert!(rows[1].contains("in_progress: aw-fix-login"));
        assert!(!rows[2].contains("in_progress:"), "bravo row: {}", rows[2]);
        assert!(
            rows[2].trim_end().ends_with("idle"),
            "bravo row: {}",
            rows[2]
        );
    }

    /// The deprecated `shelbi worker list` alias dispatches into the same
    /// `commands::workspace::run` handler the canonical `shelbi workspace
    /// list` uses, so by construction the column set is identical. We
    /// exercise the dispatch path here so a future refactor that
    /// accidentally diverges the alias gets caught.
    #[test]
    fn deprecation_alias_prints_the_same_columns() {
        let workspaces = vec![make_workspace("alpha", "hub")];
        let in_progress: Vec<&Issue> = Vec::new();
        let canonical = render_list(&workspaces, &in_progress).unwrap();
        // The alias arm in main.rs forwards into `commands::workspace::run`,
        // which calls into `list`, which calls `render_list`. Asserting
        // here on the same `render_list` output is what `shelbi worker
        // list` would render too — modulo the one-line stderr nag the
        // alias arm prints before dispatching.
        assert!(canonical[0].contains("NAME"));
        assert!(canonical[0].contains("AGENT"));
        assert!(canonical[0].contains("STATE"));
        assert!(!canonical[0].contains("RUNNER"));
        assert!(!canonical[0].contains("CLAUDE"));
    }

    #[test]
    fn release_moves_in_flight_back_to_todo_and_unassigns() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "p");

        // Bob's task should stay put; alice's should come back to todo.
        shelbi_state::save_task(
            "p",
            &make_task("fix-login", Column::in_progress(), 0, Some("alice")),
            "# body\n",
        )
        .unwrap();
        shelbi_state::save_task(
            "p",
            &make_task("other", Column::in_progress(), 1, Some("bob")),
            "",
        )
        .unwrap();
        shelbi_state::save_task("p", &make_task("a", Column::todo(), 0, None), "").unwrap();

        let released = release_workspace_tasks("p", "alice").unwrap();
        assert_eq!(released, vec!["fix-login"]);

        let fix = shelbi_state::load_task("p", "fix-login").unwrap();
        assert_eq!(fix.task.column, Column::todo());
        assert_eq!(fix.task.assigned_to, None);
        // Lands at the bottom of `todo` (after the existing `a`).
        assert_eq!(fix.task.priority, 1);
        assert!(fix.body.contains("# body"));

        // Bob's task is untouched.
        let bob_task = shelbi_state::load_task("p", "other").unwrap();
        assert_eq!(bob_task.task.column, Column::in_progress());
        assert_eq!(bob_task.task.assigned_to.as_deref(), Some("bob"));
        // After alice's task moves out, in_progress is renumbered 0..N.
        assert_eq!(bob_task.task.priority, 0);

        // The release path emits a `workspace:stop` task event so the
        // orchestrator's events.log tail sees the column return — with the
        // workflow-aware shape from `Plans/workflows.md` §10.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(" task=fix-login "), "line: {}", lines[0]);
        assert!(
            lines[0].contains(" workflow=default "),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[0].contains(" in_progress -> todo "),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[0].contains(" reason=workspace:stop "),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[0].ends_with(" to_category=ready"),
            "line: {}",
            lines[0]
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn format_ago_picks_unit_by_magnitude() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 6, 19, 12, 0, 0).unwrap();
        assert_eq!(format_ago(now, now - chrono::Duration::seconds(0)), "0s");
        assert_eq!(format_ago(now, now - chrono::Duration::seconds(45)), "45s");
        assert_eq!(format_ago(now, now - chrono::Duration::seconds(90)), "1m");
        assert_eq!(format_ago(now, now - chrono::Duration::minutes(75)), "1h");
        assert_eq!(format_ago(now, now - chrono::Duration::hours(50)), "2d");
        // Future timestamp from clock skew clamps to "0s" rather than
        // surfacing a negative value.
        assert_eq!(format_ago(now, now + chrono::Duration::seconds(5)), "0s");
    }

    #[test]
    fn release_is_noop_when_workspace_has_no_in_flight_task() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        register_project(&home, "p");
        shelbi_state::save_task("p", &make_task("a", Column::todo(), 0, None), "").unwrap();

        let released = release_workspace_tasks("p", "alice").unwrap();
        assert!(released.is_empty());

        std::env::remove_var("SHELBI_HOME");
    }
}
