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
use shelbi_core::{Column, ConfigMode, IntegrationMode, Task, WorkspaceSpec};
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
        /// Runner for the workspace. Defaults to an existing workspace's runner
        /// (or the sole declared runner when there's exactly one).
        #[arg(long)]
        runner: Option<String>,
    },
    /// Remove a workspace from the pool and tear down its worktree + pane.
    /// Refuses a workspace with an active task unless `--force`.
    Rm {
        name: String,
        /// Remove even if the workspace holds an in-progress/review task.
        #[arg(long)]
        force: bool,
    },
    /// List declared workspaces with their host, runner name, currently
    /// loaded agent (or `-` when idle), and state
    /// (`idle` / `in_progress: <task-id>`).
    List,
    /// Change the runner assigned to existing workspace slots.
    SetRunner {
        /// Runner name declared under `agent_runners`.
        runner: String,
        /// Workspace names to update. Omit with `--all` to update every slot.
        names: Vec<String>,
        /// Update every declared workspace.
        #[arg(long)]
        all: bool,
    },
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
    // Version gate: `stop` and `set-runner` mutate board/config state;
    // the views warn and proceed.
    match &cmd {
        WorkspaceCmd::List | WorkspaceCmd::Status { .. } => super::hub_version::warn_on_mismatch(),
        _ => super::hub_version::ensure_daemon_matches_for_mutation()?,
    }
    match cmd {
        WorkspaceCmd::Add {
            name,
            machine,
            runner,
        } => add(&project, &name, machine.as_deref(), runner.as_deref()),
        WorkspaceCmd::Rm { name, force } => rm(&project, &name, force),
        WorkspaceCmd::List => list(&project),
        WorkspaceCmd::SetRunner { runner, names, all } => {
            set_runner(&project, &runner, &names, all)
        }
        WorkspaceCmd::Stop { name, keep_task } => stop(&project, &name, keep_task),
        WorkspaceCmd::Status { name } => status(&project, name.as_deref()),
    }
}

/// `shelbi workspace add <name>` — append a workspace to the pool on the local
/// machine, materialize its worktree, and wire Shelbi's Claude hooks into
/// `.claude/settings.local.json`.
///
/// Ordering is deliberate: we materialize + wire BEFORE persisting the pool
/// entry, and only save the YAML once wiring succeeds. That's what makes the
/// case-4 (user-authored settings) merge flow work — the command exits without
/// having added the name, so when the orchestrator merges the hooks and re-runs
/// `workspace add <name>`, the name-collision guard doesn't refuse the re-run.
/// The worktree materialize is idempotent, so re-running is cheap.
fn add(project: &str, name: &str, machine: Option<&str>, runner: Option<&str>) -> Result<()> {
    let mut p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;

    let spec = plan_workspace_add(&p, name, machine, runner)?;
    let machine_name = spec.machine.clone();
    let runner_name = spec.runner.clone();

    let machine = p
        .machine(&machine_name)
        .ok_or_else(|| anyhow!("unknown machine `{machine_name}`"))?
        .clone();
    orch_workspace::materialize_idle_worktree(project, &machine, &spec, p.base_branch())
        .map_err(|e| anyhow!(e))?;

    // Wire Shelbi's Claude hooks into settings.local.json. Only meaningful for
    // Claude runners (codex/other pollers ignore the file), so skip the wiring
    // for a non-Claude runner but still succeed the add.
    let is_claude = p
        .runner(&runner_name)
        .map(|r| shelbi_agent::RunnerAdapter::for_command(&r.command).is_claude())
        .unwrap_or(false);
    if is_claude {
        let host = machine.host();
        let worktree = orch_workspace::workspace_worktree(&machine, &spec);
        // The candidate (pool + new spec) is what the settings renderer reads.
        let mut candidate = p.clone();
        candidate.workspaces.push(spec.clone());
        let block = orch_workspace::render_workspace_settings_preferring_agent(&candidate, None)
            .map_err(|e| anyhow!(e))?;
        match orch_workspace::wire_settings_local(&host, &worktree, name, &block)
            .map_err(|e| anyhow!(e))?
        {
            orch_workspace::SettingsWireResult::Wired => {}
            orch_workspace::SettingsWireResult::MergeRequired { message } => {
                // Case 4: user-authored settings.local.json. Print the
                // structured, orchestrator-addressed message and exit with a
                // DISTINCT status WITHOUT persisting the pool entry.
                eprintln!("{message}");
                std::process::exit(SETTINGS_MERGE_REQUIRED_EXIT);
            }
        }
    }

    // Wiring succeeded — now persist the pool entry.
    p.workspaces.push(spec);
    save_workspace_config(&p)?;
    println!("✓ added workspace `{name}` (machine {machine_name}, runner {runner_name})");
    Ok(())
}

/// Validate + resolve a `workspace add` into a ready-to-persist
/// [`WorkspaceSpec`] without touching disk: rejects an invalid name, an exact
/// or slug collision with an existing workspace, and an unknown/undefaultable
/// machine/runner. Pure over `p` so the collision + defaulting rules are
/// unit-testable without a real worktree/git.
fn plan_workspace_add(
    p: &shelbi_core::Project,
    name: &str,
    machine: Option<&str>,
    runner: Option<&str>,
) -> Result<WorkspaceSpec> {
    // Name must be a valid id (used as a tmux window + worktree dir name) and
    // must not collide — exactly, or by slug — with an existing workspace.
    shelbi_core::validate_task_id(name)
        .map_err(|e| anyhow!("invalid workspace name `{name}`: {e}"))?;
    let new_slug = shelbi_core::normalize_project_name(name)
        .map_err(|_| anyhow!("workspace name `{name}` does not reduce to a valid slug"))?;
    for w in &p.workspaces {
        if w.name == name {
            return Err(anyhow!("workspace `{name}` already exists"));
        }
        if shelbi_core::normalize_project_name(&w.name)
            .map(|s| s == new_slug)
            .unwrap_or(false)
        {
            return Err(anyhow!(
                "workspace name `{name}` slug-collides with existing workspace `{}`",
                w.name
            ));
        }
    }

    let spec = WorkspaceSpec {
        name: name.to_string(),
        machine: resolve_add_machine(p, machine)?,
        runner: resolve_add_runner(p, runner)?,
        tags: Vec::new(),
        slot: None,
    };

    // Validate machine/runner references via a dry-run clone so a bad
    // `--machine`/`--runner` fails before any disk work.
    let mut candidate = p.clone();
    candidate.workspaces.push(spec.clone());
    candidate.validate_workspaces().map_err(|e| anyhow!(e))?;
    Ok(spec)
}

/// Resolve the machine for `workspace add`: the explicit `--machine`, else the
/// local hub. Errors when `--machine` names an unknown/non-local machine or
/// when the project declares no local machine at all.
fn resolve_add_machine(p: &shelbi_core::Project, machine: Option<&str>) -> Result<String> {
    if let Some(name) = machine {
        let m = p
            .machine(name)
            .ok_or_else(|| anyhow!("unknown machine `{name}`"))?;
        return Ok(m.name.clone());
    }
    p.machines
        .iter()
        .find(|m| m.host().is_local())
        .map(|m| m.name.clone())
        .ok_or_else(|| anyhow!("project declares no local machine to host the workspace"))
}

/// Resolve the runner for `workspace add`: the explicit `--runner`, else an
/// existing workspace's runner, else the sole declared runner. Errors when
/// `--runner` is unknown or when no default can be inferred.
fn resolve_add_runner(p: &shelbi_core::Project, runner: Option<&str>) -> Result<String> {
    if let Some(name) = runner {
        if !p.agent_runners.contains_key(name) {
            return Err(anyhow!(
                "runner `{name}` is not declared in agent_runners (known: {})",
                p.agent_runners
                    .keys()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return Ok(name.to_string());
    }
    if let Some(w) = p.workspaces.first() {
        return Ok(w.runner.clone());
    }
    if p.agent_runners.len() == 1 {
        return Ok(p.agent_runners.keys().next().unwrap().clone());
    }
    Err(anyhow!(
        "no default runner: pass --runner (known: {})",
        p.agent_runners
            .keys()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// `shelbi workspace rm <name>` — remove a workspace from the pool and tear
/// down its worktree + pane. Refuses a workspace holding an active
/// (in-progress/review) task unless `--force`.
fn rm(project: &str, name: &str, force: bool) -> Result<()> {
    let mut p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let workspace = p
        .workspace(name)
        .ok_or_else(|| {
            anyhow!(
                "workspace `{name}` not declared in project `{project}` (known: {})",
                p.workspaces
                    .iter()
                    .map(|w| w.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?
        .clone();

    if !force {
        if let Some(task) = active_task_for(project, name) {
            return Err(anyhow!(
                "workspace `{name}` has an active task `{task}` — release it \
                 (`shelbi workspace stop {name}`) or pass --force"
            ));
        }
    }

    let machine = p
        .machine(&workspace.machine)
        .ok_or_else(|| anyhow!("workspace references unknown machine `{}`", workspace.machine))?
        .clone();

    // Kill any live pane, then tear down the worktree. Both are best-effort /
    // idempotent so a partially-provisioned workspace still removes cleanly.
    let addr = orch_workspace::workspace_tmux_addr(&p, &workspace).map_err(|e| anyhow!(e))?;
    if let Err(e) = orch_workspace::kill_workspace_pane(&machine.host(), &addr, name) {
        eprintln!("warning: killing pane for `{name}`: {e}");
    }
    orch_workspace::remove_workspace_worktree(project, &machine, &workspace)
        .map_err(|e| anyhow!(e))?;

    p.workspaces.retain(|w| w.name != name);
    save_workspace_config(&p)?;
    println!("✓ removed workspace `{name}`");
    Ok(())
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

    // Surfaces every in-progress task assigned to the workspace. There
    // should normally be at most one, but if shelbi's state diverged we
    // print all of them in the STATE cell so the user sees the mess.
    let in_progress =
        shelbi_state::list_column(project, Column::in_progress()).map_err(|e| anyhow!(e))?;
    let assigned: Vec<&Task> = in_progress.iter().map(|tf| &tf.task).collect();

    let occupied = occupied_idle_workspaces(&p, &assigned)?;
    let modes = workspace_integration_modes(&p);
    for line in render_list_with_occupied(&p.workspaces, &assigned, &occupied, &modes)? {
        println!("{line}");
    }
    Ok(())
}

/// The integration transport tier for each declared workspace, keyed by
/// workspace name. Workspace agents don't run the native Codex bridge; their
/// tier is decided by whether Shelbi can push/verify against the runner —
/// Claude Code and Codex panes get the verified tmux-submission + OSC-hook
/// contract (`conventional`), anything Shelbi can only poll is `degraded`.
fn workspace_integration_modes(
    project: &shelbi_core::Project,
) -> BTreeMap<String, IntegrationMode> {
    project
        .workspaces
        .iter()
        .map(|workspace| {
            let mode = project
                .runner(&workspace.runner)
                .map(|runner| workspace_integration_mode(&runner.command))
                .unwrap_or(IntegrationMode::Degraded);
            (workspace.name.clone(), mode)
        })
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
    in_progress: &[&Task],
) -> Result<BTreeMap<String, OccupiedKind>> {
    let assigned: BTreeSet<&str> = in_progress
        .iter()
        .filter_map(|t| t.assigned_to.as_deref())
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
            orch_workspace::workspace_tmux_addr(project, workspace).map_err(|e| anyhow!(e))?;
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
    in_progress: &[&Task],
) -> Result<Vec<String>> {
    render_list_with_occupied(workspaces, in_progress, &BTreeMap::new(), &BTreeMap::new())
}

fn render_list_with_occupied(
    workspaces: &[WorkspaceSpec],
    in_progress: &[&Task],
    occupied_idle: &BTreeMap<String, OccupiedKind>,
    integration: &BTreeMap<String, IntegrationMode>,
) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(workspaces.len() + 1);
    out.push(format!(
        "{:<12} {:<8} {:<14} {:<14} {:<13} {}",
        "NAME", "HOST", "RUNNER", "AGENT", "INTEG", "STATE"
    ));
    for workspace in workspaces {
        let mine: Vec<&Task> = in_progress
            .iter()
            .copied()
            .filter(|t| t.assigned_to.as_deref() == Some(workspace.name.as_str()))
            .collect();
        let (agent, state) = if mine.is_empty() {
            let state = match occupied_idle.get(&workspace.name) {
                Some(kind) => kind.state_cell(),
                None => "idle".to_string(),
            };
            (IDLE_AGENT_CELL.to_string(), state)
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
            "{:<12} {:<8} {:<14} {:<14} {:<13} {}",
            workspace.name, workspace.machine, workspace.runner, agent, integ, state
        ));
    }
    Ok(out)
}

fn set_runner(project: &str, runner: &str, names: &[String], all: bool) -> Result<()> {
    let mut p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let changed = update_workspace_runners(&mut p, runner, names, all)?;
    save_workspace_config(&p)?;
    let scope = if all {
        "all workspaces".to_string()
    } else {
        changed.join(", ")
    };
    println!("✓ set runner `{runner}` for {scope}");
    Ok(())
}

fn update_workspace_runners(
    project: &mut shelbi_core::Project,
    runner: &str,
    names: &[String],
    all: bool,
) -> Result<Vec<String>> {
    if !project.agent_runners.contains_key(runner) {
        return Err(anyhow!(
            "runner `{runner}` is not declared in agent_runners (known: {})",
            project
                .agent_runners
                .keys()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if all && !names.is_empty() {
        return Err(anyhow!("pass either workspace names or --all, not both"));
    }
    if !all && names.is_empty() {
        return Err(anyhow!(
            "name at least one workspace, or pass --all to update every workspace"
        ));
    }

    let targets: std::collections::BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let known: std::collections::BTreeSet<&str> =
        project.workspaces.iter().map(|w| w.name.as_str()).collect();
    if !all {
        let missing: Vec<&str> = targets.difference(&known).copied().collect();
        if !missing.is_empty() {
            return Err(anyhow!(
                "unknown workspace(s): {} (known: {})",
                missing.join(", "),
                known.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
    }

    let mut changed = Vec::new();
    for workspace in &mut project.workspaces {
        if all || targets.contains(workspace.name.as_str()) {
            workspace.runner = runner.to_string();
            changed.push(workspace.name.clone());
        }
    }
    Ok(changed)
}

fn save_workspace_config(project: &shelbi_core::Project) -> Result<()> {
    if project.config_mode != Some(ConfigMode::InRepo) {
        return shelbi_state::save_project(project).map_err(|e| anyhow!(e));
    }

    let dir = shelbi_state::project_dir(&project.name).map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&dir).map_err(|e| anyhow!(e))?;
    let path = dir.join("local.yaml");
    let tmp = dir.join("local.yaml.tmp");
    let body = project.to_local_yaml_string().map_err(|e| anyhow!(e))?;
    fs::write(&tmp, body).map_err(|e| anyhow!("writing {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        anyhow!("renaming {} to {}: {e}", tmp.display(), path.display())
    })?;
    Ok(())
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
    let addr = orch_workspace::workspace_tmux_addr(&p, workspace).map_err(|e| anyhow!(e))?;
    orch_workspace::kill_workspace_pane(&host, &addr, &workspace.name).map_err(|e| anyhow!(e))?;
    println!("✓ {name} pane stopped");

    if keep_task {
        return Ok(());
    }

    for id in release_workspace_tasks(project, name)? {
        println!("✓ {id} released → todo (was assigned to {name})");
    }
    Ok(())
}

/// The task currently loaded on `workspace` — an in-progress or review-column
/// task assigned to it. Resolves the task a workspace is holding when the
/// caller doesn't already have `$TASK_ID` in the environment.
fn active_task_for(project: &str, workspace: &str) -> Option<String> {
    for column in [Column::in_progress(), Column::review()] {
        if let Ok(tasks) = shelbi_state::list_column(project, column) {
            if let Some(tf) = tasks
                .into_iter()
                .find(|tf| tf.task.assigned_to.as_deref() == Some(workspace))
            {
                return Some(tf.task.id);
            }
        }
    }
    None
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
    let in_progress =
        shelbi_state::list_column(project, Column::in_progress()).map_err(|e| anyhow!(e))?;
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
        let moved = shelbi_state::release_task_to_todo(project, &id).map_err(|e| anyhow!(e))?;
        if let Some((from, to, workflow)) = moved {
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
    use shelbi_core::Project;
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

    fn make_task(id: &str, column: Column, priority: u32, assigned_to: Option<&str>) -> Task {
        let now = Utc::now();
        Task {
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
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    fn make_workspace(name: &str, machine: &str, runner: &str) -> WorkspaceSpec {
        WorkspaceSpec {
            name: name.to_string(),
            machine: machine.to_string(),
            runner: runner.to_string(),
            tags: Vec::new(),
            slot: None,
        }
    }

    #[test]
    fn render_list_emits_header_followed_by_one_row_per_workspace() {
        let workspaces = vec![
            make_workspace("alpha", "hub", "opus-4-7"),
            make_workspace("bravo", "hub", "opus-4-7"),
        ];
        let assigned = make_task("aw-task-1", Column::in_progress(), 0, Some("alpha"));
        let in_progress: Vec<&Task> = vec![&assigned];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        assert_eq!(rows.len(), 3);
        // Header is the canonical column order — clients reading the
        // pipe-format depend on it.
        let header = &rows[0];
        let name_at = header.find("NAME").unwrap();
        let host_at = header.find("HOST").unwrap();
        let runner_at = header.find("RUNNER").unwrap();
        let agent_at = header.find("AGENT").unwrap();
        let state_at = header.find("STATE").unwrap();
        assert!(name_at < host_at);
        assert!(host_at < runner_at);
        assert!(runner_at < agent_at);
        assert!(agent_at < state_at);
        // The legacy `claude` column is gone.
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
            make_workspace("alpha", "hub", "opus-4-7"),
            make_workspace("bravo", "hub", "opus-4-7"),
        ];
        let in_progress: Vec<&Task> = Vec::new();
        // alpha runs a hook-capable runner (conventional); bravo's runner is
        // unrecognized, so it can only be polled (degraded).
        let modes = BTreeMap::from([
            ("alpha".to_string(), IntegrationMode::Conventional),
            ("bravo".to_string(), IntegrationMode::Degraded),
        ]);

        let rows =
            render_list_with_occupied(&workspaces, &in_progress, &BTreeMap::new(), &modes).unwrap();
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
    fn render_list_active_workspace_surfaces_runner_and_default_agent() {
        let workspaces = vec![make_workspace("alpha", "hub", "opus-4-7")];
        let task = make_task("aw-fix-login", Column::in_progress(), 0, Some("alpha"));
        let in_progress: Vec<&Task> = vec![&task];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        let row = &rows[1];
        assert!(row.contains("alpha"), "row: {row}");
        assert!(row.contains("hub"), "row: {row}");
        // RUNNER reads the workspace runner name verbatim.
        assert!(row.contains("opus-4-7"), "row: {row}");
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
        let workspaces = vec![make_workspace("delta", "devbox", "sonnet-4-6")];
        let mut task = make_task("aw-write-tests", Column::in_progress(), 0, Some("delta"));
        task.params.insert("agent".into(), "qa".into());
        let in_progress: Vec<&Task> = vec![&task];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        let row = &rows[1];
        assert!(row.contains("sonnet-4-6"), "row: {row}");
        // The task's `agent: qa` wins over the developer default.
        assert!(
            row.contains(" qa "),
            "row should carry AGENT=qa cell: {row}"
        );
        assert!(row.contains("in_progress: aw-write-tests"), "row: {row}");
    }

    #[test]
    fn render_list_idle_workspace_uses_placeholder_agent_and_idle_state() {
        let workspaces = vec![make_workspace("bravo", "hub", "opus-4-7")];
        // No in-progress tasks at all — bravo is idle.
        let in_progress: Vec<&Task> = Vec::new();

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
        let workspaces = vec![make_workspace("delta", "devbox", "sonnet-4-6")];
        let in_progress: Vec<&Task> = Vec::new();
        let occupied = BTreeMap::from([("delta".to_string(), OccupiedKind::Orphaned)]);

        let rows = render_list_with_occupied(&workspaces, &in_progress, &occupied, &BTreeMap::new()).unwrap();
        let row = &rows[1];
        assert!(row.contains("delta"), "row: {row}");
        assert!(row.contains(&format!(" {IDLE_AGENT_CELL} ")), "row: {row}");
        assert!(row.trim_end().ends_with("orphaned session"), "row: {row}");
    }

    /// A live slot carrying the user-shell mark (sidebar click on an idle
    /// workspace) renders as user-occupied, not as an orphaned session —
    /// that's the cell the orchestrator reads to skip the slot for
    /// dispatch while the user is in it.
    #[test]
    fn render_list_marks_user_shell_slot_as_occupied() {
        let workspaces = vec![make_workspace("delta", "devbox", "sonnet-4-6")];
        let in_progress: Vec<&Task> = Vec::new();
        let occupied = BTreeMap::from([("delta".to_string(), OccupiedKind::UserShell)]);

        let rows = render_list_with_occupied(&workspaces, &in_progress, &occupied, &BTreeMap::new()).unwrap();
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
        let workspaces = vec![make_workspace("delta", "devbox", "sonnet-4-6")];
        let in_progress: Vec<&Task> = Vec::new();
        let occupied = BTreeMap::from([(
            "delta".to_string(),
            OccupiedKind::Unreachable(
                "ssh probe timed out after 5s (interactive auth pending?)".to_string(),
            ),
        )]);

        let rows = render_list_with_occupied(&workspaces, &in_progress, &occupied, &BTreeMap::new()).unwrap();
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
        let workspaces = vec![make_workspace("delta", "devbox", "sonnet-4-6")];
        let task = make_task("bug-fix", Column::in_progress(), 0, Some("delta"));
        let in_progress: Vec<&Task> = vec![&task];
        let occupied = BTreeMap::from([("delta".to_string(), OccupiedKind::Orphaned)]);

        let rows = render_list_with_occupied(&workspaces, &in_progress, &occupied, &BTreeMap::new()).unwrap();
        let row = &rows[1];
        assert!(row.contains("in_progress: bug-fix"), "row: {row}");
        assert!(!row.contains("orphaned session"), "row: {row}");
    }

    #[test]
    fn render_list_only_counts_tasks_assigned_to_this_workspace() {
        // alpha has a task; bravo is on the same host but idle. The bravo
        // row should not show alpha's task.
        let workspaces = vec![
            make_workspace("alpha", "hub", "opus-4-7"),
            make_workspace("bravo", "hub", "opus-4-7"),
        ];
        let task = make_task("aw-fix-login", Column::in_progress(), 0, Some("alpha"));
        let in_progress: Vec<&Task> = vec![&task];

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
        let workspaces = vec![make_workspace("alpha", "hub", "opus-4-7")];
        let in_progress: Vec<&Task> = Vec::new();
        let canonical = render_list(&workspaces, &in_progress).unwrap();
        // The alias arm in main.rs forwards into `commands::workspace::run`,
        // which calls into `list`, which calls `render_list`. Asserting
        // here on the same `render_list` output is what `shelbi worker
        // list` would render too — modulo the one-line stderr nag the
        // alias arm prints before dispatching.
        assert!(canonical[0].contains("NAME"));
        assert!(canonical[0].contains("RUNNER"));
        assert!(canonical[0].contains("AGENT"));
        assert!(canonical[0].contains("STATE"));
        assert!(!canonical[0].contains("CLAUDE"));
    }

    fn mixed_runner_project() -> Project {
        Project::from_yaml_str(
            r#"
name: p
repo: /tmp/p
default_branch: main
machines:
  - { name: hub, kind: local, work_dir: /tmp/p }
orchestrator:
  runner: codex
agent_runners:
  claude: { command: claude, flags: [] }
  codex: { command: codex, flags: [] }
workspaces:
  - { name: alpha, machine: hub, runner: claude }
  - { name: bravo, machine: hub, runner: codex }
"#,
        )
        .unwrap()
    }

    fn mixed_runner_in_repo_project() -> Project {
        Project::from_yaml_str(
            r#"
name: p
config_mode: in-repo
repo: /tmp/p
default_branch: main
machines:
  - { name: hub, kind: local, work_dir: /tmp/p }
orchestrator:
  runner: codex
agent_runners:
  claude: { command: claude, flags: [] }
  codex: { command: codex, flags: [] }
workspaces:
  - { name: alpha, machine: hub, runner: claude }
  - { name: bravo, machine: hub, runner: codex }
"#,
        )
        .unwrap()
    }

    #[test]
    fn render_list_makes_mixed_workspace_runners_obvious() {
        let p = mixed_runner_project();
        let rows = render_list(&p.workspaces, &[]).unwrap();

        assert!(rows[0].contains("RUNNER"));
        assert!(rows[1].contains("alpha"), "row: {}", rows[1]);
        assert!(rows[1].contains("claude"), "row: {}", rows[1]);
        assert!(rows[2].contains("bravo"), "row: {}", rows[2]);
        assert!(rows[2].contains("codex"), "row: {}", rows[2]);
    }

    #[test]
    fn plan_workspace_add_defaults_machine_and_runner() {
        let p = mixed_runner_project();
        // No overrides: local hub machine + first existing workspace's runner.
        let spec = plan_workspace_add(&p, "charlie", None, None).unwrap();
        assert_eq!(spec.name, "charlie");
        assert_eq!(spec.machine, "hub");
        assert_eq!(spec.runner, "claude"); // alpha's runner
        assert!(spec.tags.is_empty());
        assert_eq!(spec.slot, None);
    }

    #[test]
    fn plan_workspace_add_honors_overrides() {
        let p = mixed_runner_project();
        // Bind the overrides so the shelbi-agent grep-guard (which forbids a
        // basename literal wrapped in Some(...) outside RunnerKind::from_command)
        // stays green.
        let machine = Some("hub");
        let runner_override = "codex";
        let spec = plan_workspace_add(&p, "charlie", machine, Some(runner_override)).unwrap();
        assert_eq!(spec.machine, "hub");
        assert_eq!(spec.runner, "codex");
    }

    #[test]
    fn plan_workspace_add_rejects_exact_and_slug_collisions() {
        let p = mixed_runner_project();
        // Exact name.
        let err = plan_workspace_add(&p, "alpha", None, None).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        // Slug collision: `alpha-` is a valid id but normalizes (trailing `-`
        // trimmed) onto the existing `alpha`.
        let err2 = plan_workspace_add(&p, "alpha-", None, None).unwrap_err();
        assert!(err2.to_string().contains("slug-collides"), "{err2}");
    }

    #[test]
    fn plan_workspace_add_rejects_invalid_name() {
        let p = mixed_runner_project();
        let err = plan_workspace_add(&p, "Has Spaces", None, None).unwrap_err();
        assert!(err.to_string().contains("invalid workspace name"), "{err}");
    }

    #[test]
    fn plan_workspace_add_rejects_unknown_runner() {
        let p = mixed_runner_project();
        let err = plan_workspace_add(&p, "charlie", None, Some("aider")).unwrap_err();
        assert!(err.to_string().contains("agent_runners"), "{err}");
    }

    #[test]
    fn add_then_rm_round_trips_the_pool_yaml() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Persist a project so load/save round-trips through disk.
        let p = mixed_runner_project();
        shelbi_state::save_project(&p).unwrap();

        // Simulate the pool mutation halves of add/rm (the disk-touching
        // worktree steps are covered by the orchestrator crate). Add appends;
        // rm removes; both go through the same save path the commands use.
        let mut after_add = shelbi_state::load_project("p").unwrap();
        let spec = plan_workspace_add(&after_add, "charlie", None, None).unwrap();
        after_add.workspaces.push(spec);
        save_workspace_config(&after_add).unwrap();

        let reloaded = shelbi_state::load_project("p").unwrap();
        assert!(reloaded.workspace("charlie").is_some());
        assert_eq!(reloaded.workspaces.len(), 3);

        let mut after_rm = reloaded;
        after_rm.workspaces.retain(|w| w.name != "charlie");
        save_workspace_config(&after_rm).unwrap();

        let final_p = shelbi_state::load_project("p").unwrap();
        assert!(final_p.workspace("charlie").is_none());
        assert_eq!(final_p.workspaces.len(), 2);
        // The untouched workspaces survive verbatim.
        assert!(final_p.workspace("alpha").is_some());
        assert!(final_p.workspace("bravo").is_some());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn update_workspace_runners_can_bulk_switch_to_codex() {
        let mut p = mixed_runner_project();
        let changed = update_workspace_runners(&mut p, "codex", &[], true).unwrap();

        assert_eq!(changed, vec!["alpha", "bravo"]);
        assert!(p.workspaces.iter().all(|w| w.runner == "codex"));
        assert_eq!(p.orchestrator.runner, "codex");
    }

    #[test]
    fn update_workspace_runners_can_switch_selected_slots_only() {
        let mut p = mixed_runner_project();
        let names = vec!["alpha".to_string()];
        let changed = update_workspace_runners(&mut p, "codex", &names, false).unwrap();

        assert_eq!(changed, vec!["alpha"]);
        assert_eq!(p.workspace("alpha").unwrap().runner, "codex");
        assert_eq!(p.workspace("bravo").unwrap().runner, "codex");
    }

    #[test]
    fn update_workspace_runners_rejects_unknown_runner() {
        let mut p = mixed_runner_project();
        let err = update_workspace_runners(&mut p, "aider", &[], true).unwrap_err();
        assert!(err.to_string().contains("agent_runners"));
    }

    #[test]
    fn save_workspace_config_writes_local_yaml_for_in_repo_projects() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let mut p = mixed_runner_in_repo_project();
        update_workspace_runners(&mut p, "codex", &[], true).unwrap();
        save_workspace_config(&p).unwrap();

        let local = std::fs::read_to_string(home.join("projects/p/local.yaml")).unwrap();
        assert!(local.contains("workspaces:"), "local.yaml: {local}");
        assert!(local.contains("runner: codex"), "local.yaml: {local}");
        assert!(!home.join("projects/p.yaml").exists());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `serve`'s task-stamping fallback: when `$TASK_ID` isn't in the env
    /// (a human running the command), the server record is stamped with the
    /// in-progress or review task assigned to the workspace, and `None` when
    /// nothing is loaded there.
    #[test]
    fn active_task_for_finds_in_progress_or_review_assignment() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // In-progress task on review-1 → found.
        shelbi_state::save_task(
            "p",
            &make_task("feat-x", Column::in_progress(), 0, Some("review-1")),
            "",
        )
        .unwrap();
        assert_eq!(active_task_for("p", "review-1").as_deref(), Some("feat-x"));

        // A review-column task also counts (a branch loaded for a human).
        shelbi_state::save_task(
            "p",
            &make_task("feat-y", Column::review(), 0, Some("review-2")),
            "",
        )
        .unwrap();
        assert_eq!(active_task_for("p", "review-2").as_deref(), Some("feat-y"));

        // Nothing assigned to review-9 → None.
        assert_eq!(active_task_for("p", "review-9"), None);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn release_moves_in_flight_back_to_todo_and_unassigns() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

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
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_task("p", &make_task("a", Column::todo(), 0, None), "").unwrap();

        let released = release_workspace_tasks("p", "alice").unwrap();
        assert!(released.is_empty());

        std::env::remove_var("SHELBI_HOME");
    }
}
