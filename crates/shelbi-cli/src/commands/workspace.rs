//! `shelbi workspace <subcommand>` — manage the project's declared workspace
//! pool. Workspaces are durable slots (one worktree each); tasks come and go.
//! See [`shelbi_orchestrator::workspace`] for the lifecycle primitives.

use std::fs;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use shelbi_core::{Column, Task, WorkspaceSpec};
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

#[derive(Debug, Subcommand)]
pub enum WorkspaceCmd {
    /// List declared workspaces with their host, model identifier (runner
    /// name), currently loaded agent (or `-` when idle), and state
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
    /// Start (or restart) a review workspace's long-lived dev server in a
    /// dedicated, shelbi-managed server pane. This is the entry point the
    /// Review agent invokes once it has resolved the serve command; shelbi
    /// owns the pane's lifecycle from here (a `server_alive=false` event on
    /// death, teardown on re-dispatch/stop, and the leaked-port reaper).
    ///
    /// The port defaults to the workspace's deterministic review slot; the
    /// serve command defaults to the project's `review.serve`. Any server
    /// pane already running for the workspace is torn down first.
    Serve {
        /// The review workspace to serve on. Defaults to `$SHELBI_WORKSPACE`
        /// (set in the review pane's env) when omitted.
        name: Option<String>,
        /// Override the injected port. Defaults to the workspace's review
        /// slot (`review.base_port + index * review.port_stride`).
        #[arg(long)]
        port: Option<u16>,
        /// The dev-server command to run (e.g. `npm run dev -- --port $PORT`).
        /// Defaults to the project's declared `review.serve`; required when
        /// that's absent (the agent passes its auto-detected command here).
        #[arg(long)]
        serve: Option<String>,
        /// Skip the HTTP ready-probe and return as soon as the pane is up.
        #[arg(long)]
        no_wait: bool,
    },
}

pub fn run(project_opt: Option<String>, cmd: WorkspaceCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        WorkspaceCmd::List => list(&project),
        WorkspaceCmd::Stop { name, keep_task } => stop(&project, &name, keep_task),
        WorkspaceCmd::Status { name } => status(&project, name.as_deref()),
        WorkspaceCmd::Serve {
            name,
            port,
            serve,
            no_wait,
        } => self::serve(&project, name, port, serve, no_wait),
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
        println!("(no workspaces declared in {project} — add a `workspaces:` block to the project YAML)");
        return Ok(());
    }

    // Surfaces every in-progress task assigned to the workspace. There
    // should normally be at most one, but if shelbi's state diverged we
    // print all of them in the STATE cell so the user sees the mess.
    let in_progress = shelbi_state::list_column(project, Column::InProgress)
        .map_err(|e| anyhow!(e))?;
    let assigned: Vec<&Task> = in_progress.iter().map(|tf| &tf.task).collect();

    for line in render_list(&p.workspaces, &assigned)? {
        println!("{line}");
    }
    Ok(())
}

/// Render the `shelbi workspace list` table: a header row followed by one
/// row per workspace. Pure so the column rendering can be tested without
/// touching stdout. The caller has already filtered `in_progress` to
/// `Column::InProgress` tasks — we still re-filter by `assigned_to` per
/// workspace.
///
/// Errors when a workspace references an undeclared machine: that's a
/// project YAML bug the user should fix, and surfacing it from `list` is
/// the same behavior as the old per-row `machine().ok_or_else(...)` path.
pub(crate) fn render_list(
    workspaces: &[WorkspaceSpec],
    in_progress: &[&Task],
) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(workspaces.len() + 1);
    out.push(format!(
        "{:<12} {:<8} {:<14} {:<14} {}",
        "NAME", "HOST", "MODEL", "AGENT", "STATE"
    ));
    for workspace in workspaces {
        let mine: Vec<&Task> = in_progress
            .iter()
            .copied()
            .filter(|t| t.assigned_to.as_deref() == Some(workspace.name.as_str()))
            .collect();
        let (agent, state) = if mine.is_empty() {
            (IDLE_AGENT_CELL.to_string(), "idle".to_string())
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
            let ids = mine.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ");
            (agent, format!("in_progress: {ids}"))
        };
        // MODEL column reads from the same source the legacy `claude`
        // column did — the workspace's runner name. The column header is
        // more descriptive ("MODEL") so projects whose runner is named
        // after the underlying model (e.g. `opus-4-7`) read correctly;
        // legacy projects whose runner is named `claude` keep working
        // verbatim.
        out.push(format!(
            "{:<12} {:<8} {:<14} {:<14} {}",
            workspace.name, workspace.machine, workspace.runner, agent, state
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
    let machine = p
        .machine(&workspace.machine)
        .ok_or_else(|| anyhow!("workspace references unknown machine `{}`", workspace.machine))?;
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

/// Start (or restart) a review workspace's dev server in a managed server
/// pane. Resolves the port and serve command (CLI override → project config →
/// review slot), spawns the pane via
/// [`shelbi_orchestrator::server_pane::spawn_server_pane`], and — unless
/// `--no-wait` — polls the HTTP ready-probe before printing the URL.
fn serve(
    project: &str,
    name: Option<String>,
    port_override: Option<u16>,
    serve_override: Option<String>,
    no_wait: bool,
) -> Result<()> {
    use shelbi_orchestrator::server_pane;

    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;

    // Workspace: explicit arg wins, else the review pane's injected env, else
    // an actionable error (no silent guess).
    let name = name
        .or_else(|| std::env::var("SHELBI_WORKSPACE").ok().filter(|s| !s.is_empty()))
        .ok_or_else(|| {
            anyhow!(
                "no workspace given: pass a NAME or run inside a review pane \
                 (which sets $SHELBI_WORKSPACE)"
            )
        })?;
    let workspace = p
        .workspace(&name)
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

    // Port: override wins; else the workspace's deterministic review slot.
    // A dev workspace has no review slot, so serving one requires `--port`.
    let port = match port_override.or_else(|| {
        orch_workspace::review_workspace_port(&p, &workspace)
    }) {
        Some(port) => port,
        None => {
            return Err(anyhow!(
                "workspace `{name}` isn't a review workspace, so it has no \
                 assigned port — pass `--port <n>` to serve it anyway"
            ));
        }
    };

    // Serve command: override wins; else the project's declared review.serve.
    // Auto-detection (framework heuristics) is the Review agent's job — it
    // passes its resolved command via `--serve` — so an absent command here
    // is a hard, guidance-carrying error rather than a guess.
    let serve_cmd = serve_override.or_else(|| p.review.serve.clone()).ok_or_else(|| {
        anyhow!(
            "no serve command: pass `--serve '<cmd>'` or declare `review.serve` \
             in the project config"
        )
    })?;

    // The task the server is loading, for the record. Prefer the review
    // pane's injected TASK_ID; fall back to the active task on the workspace.
    let task_id = std::env::var("TASK_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| active_task_for(project, &name))
        .unwrap_or_default();

    let record = server_pane::spawn_server_pane(server_pane::ServerPaneParams {
        project: &p,
        workspace: &workspace,
        task_id: &task_id,
        port,
        serve: &serve_cmd,
    })
    .map_err(|e| anyhow!(e))?;
    println!("✓ server pane started for {name} (pane {}) on port {port}", record.pane_id);

    if no_wait {
        println!("  {} (ready-probe skipped)", record.url);
        return Ok(());
    }

    // Ready-probe: gate the "ready" line on the server actually answering.
    // Timeout comes from `review.ready_probe`; default 90s.
    let timeout = p
        .review
        .ready_probe
        .as_ref()
        .map(|rp| rp.timeout)
        .unwrap_or(std::time::Duration::from_secs(90));
    match server_pane::wait_for_http_ready(port, "/", timeout) {
        Some(code) => println!("✓ serving at {} (HTTP {code})", record.url),
        None => println!(
            "⚠ {} did not answer within {}s — the pane is up; check its output",
            record.url,
            timeout.as_secs()
        ),
    }
    Ok(())
}

/// The task currently loaded on `workspace` — an in-progress or review-column
/// task assigned to it. Used to stamp the server record when `$TASK_ID` isn't
/// in the environment (a human running `serve` by hand).
fn active_task_for(project: &str, workspace: &str) -> Option<String> {
    for column in [Column::InProgress, Column::Review] {
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
            Ok(None) => println!(
                "{:<12} {:<24} {:<14} {:<12} —",
                name, "—", "?", "never"
            ),
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
    s.current_task.clone().unwrap_or_else(|| "(idle)".to_string())
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
    let in_progress = shelbi_state::list_column(project, Column::InProgress)
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
        let moved =
            shelbi_state::release_task_to_todo(project, &id).map_err(|e| anyhow!(e))?;
        if let Some((from, to, workflow)) = moved {
            if let Err(e) =
                shelbi_state::append_task_event(&id, &workflow, from, to, "workspace:stop")
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
            role: Default::default(),
        }
    }

    #[test]
    fn render_list_emits_header_followed_by_one_row_per_workspace() {
        let workspaces = vec![
            make_workspace("alpha", "hub", "opus-4-7"),
            make_workspace("bravo", "hub", "opus-4-7"),
        ];
        let assigned = make_task("aw-task-1", Column::InProgress, 0, Some("alpha"));
        let in_progress: Vec<&Task> = vec![&assigned];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        assert_eq!(rows.len(), 3);
        // Header is the canonical column order — clients reading the
        // pipe-format depend on it.
        let header = &rows[0];
        let name_at = header.find("NAME").unwrap();
        let host_at = header.find("HOST").unwrap();
        let model_at = header.find("MODEL").unwrap();
        let agent_at = header.find("AGENT").unwrap();
        let state_at = header.find("STATE").unwrap();
        assert!(name_at < host_at);
        assert!(host_at < model_at);
        assert!(model_at < agent_at);
        assert!(agent_at < state_at);
        // The legacy `claude` column is gone.
        assert!(!header.contains("CLAUDE"));
    }

    #[test]
    fn render_list_active_workspace_surfaces_model_and_default_agent() {
        let workspaces = vec![make_workspace("alpha", "hub", "opus-4-7")];
        let task = make_task("aw-fix-login", Column::InProgress, 0, Some("alpha"));
        let in_progress: Vec<&Task> = vec![&task];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        let row = &rows[1];
        assert!(row.contains("alpha"), "row: {row}");
        assert!(row.contains("hub"), "row: {row}");
        // MODEL reads the runner name verbatim — the column header is
        // descriptive, but the source is unchanged.
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
        let mut task = make_task("aw-write-tests", Column::InProgress, 0, Some("delta"));
        task.params.insert("agent".into(), "qa".into());
        let in_progress: Vec<&Task> = vec![&task];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        let row = &rows[1];
        assert!(row.contains("sonnet-4-6"), "row: {row}");
        // The task's `agent: qa` wins over the developer default.
        assert!(row.contains(" qa "), "row should carry AGENT=qa cell: {row}");
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
    fn render_list_only_counts_tasks_assigned_to_this_workspace() {
        // alpha has a task; bravo is on the same host but idle. The bravo
        // row should not show alpha's task.
        let workspaces = vec![
            make_workspace("alpha", "hub", "opus-4-7"),
            make_workspace("bravo", "hub", "opus-4-7"),
        ];
        let task = make_task("aw-fix-login", Column::InProgress, 0, Some("alpha"));
        let in_progress: Vec<&Task> = vec![&task];

        let rows = render_list(&workspaces, &in_progress).unwrap();
        assert!(rows[1].contains("in_progress: aw-fix-login"));
        assert!(!rows[2].contains("in_progress:"), "bravo row: {}", rows[2]);
        assert!(rows[2].trim_end().ends_with("idle"), "bravo row: {}", rows[2]);
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
        assert!(canonical[0].contains("MODEL"));
        assert!(canonical[0].contains("AGENT"));
        assert!(canonical[0].contains("STATE"));
        assert!(!canonical[0].contains("CLAUDE"));
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
            &make_task("feat-x", Column::InProgress, 0, Some("review-1")),
            "",
        )
        .unwrap();
        assert_eq!(active_task_for("p", "review-1").as_deref(), Some("feat-x"));

        // A review-column task also counts (a branch loaded for a human).
        shelbi_state::save_task(
            "p",
            &make_task("feat-y", Column::Review, 0, Some("review-2")),
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
            &make_task("fix-login", Column::InProgress, 0, Some("alice")),
            "# body\n",
        )
        .unwrap();
        shelbi_state::save_task(
            "p",
            &make_task("other", Column::InProgress, 1, Some("bob")),
            "",
        )
        .unwrap();
        shelbi_state::save_task("p", &make_task("a", Column::Todo, 0, None), "").unwrap();

        let released = release_workspace_tasks("p", "alice").unwrap();
        assert_eq!(released, vec!["fix-login"]);

        let fix = shelbi_state::load_task("p", "fix-login").unwrap();
        assert_eq!(fix.task.column, Column::Todo);
        assert_eq!(fix.task.assigned_to, None);
        // Lands at the bottom of `todo` (after the existing `a`).
        assert_eq!(fix.task.priority, 1);
        assert!(fix.body.contains("# body"));

        // Bob's task is untouched.
        let bob_task = shelbi_state::load_task("p", "other").unwrap();
        assert_eq!(bob_task.task.column, Column::InProgress);
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
        assert!(lines[0].ends_with(" to_category=ready"), "line: {}", lines[0]);

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
        shelbi_state::save_task("p", &make_task("a", Column::Todo, 0, None), "").unwrap();

        let released = release_workspace_tasks("p", "alice").unwrap();
        assert!(released.is_empty());

        std::env::remove_var("SHELBI_HOME");
    }
}
