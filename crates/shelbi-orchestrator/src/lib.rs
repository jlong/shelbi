//! Project tmux session bootstrap.
//!
//! Each shelbi project owns one tmux session named `shelbi-<project>`. Its
//! first window is `dashboard`, a two-pane layout:
//!
//! - left pane (small): the `shelbi __sidebar <project>` ratatui process —
//!   nav, agent list, Ctrl+Space palette.
//! - right pane: the configured orchestrator agent CLI (e.g. `claude`),
//!   running natively in the pane. The user types into it directly.
//!
//! Worker agents are additional windows in the same session (local) or
//! their own `shelbi-w-<id>` sessions on a remote machine (so they survive
//! SSH disconnect). The `shelbi orchestrate` CLI and the TUI launcher both
//! call into `ensure_dashboard()` so the bootstrap is idempotent and
//! consistent.

use shelbi_core::{Error, MachineKind, Result, TmuxAddr};

pub const DEFAULT_SYSTEM_PROMPT: &str = include_str!("default_orchestrator.md");

/// Outcome of `ensure_dashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapStatus {
    AlreadyRunning,
    Started,
}

/// Resolve the active orchestrator system prompt for a project: per-project
/// override (`ORCHESTRATOR.md`) if present, else the bundled default.
pub fn system_prompt(project: &str) -> Result<String> {
    let path = shelbi_state::project_dir(project)?.join("ORCHESTRATOR.md");
    if path.exists() {
        Ok(std::fs::read_to_string(&path).map_err(Error::Io)?)
    } else {
        Ok(DEFAULT_SYSTEM_PROMPT.to_string())
    }
}

/// The dashboard window's tmux address (orchestrator's session).
pub fn dashboard_addr(project_name: &str) -> TmuxAddr {
    TmuxAddr {
        session: format!("shelbi-{project_name}"),
        window: "dashboard".into(),
    }
}

/// Idempotently set up the project's tmux session with a `dashboard`
/// window split into sidebar (left) + orchestrator (right). Safe to call
/// repeatedly.
pub fn ensure_dashboard(project_name: &str) -> Result<BootstrapStatus> {
    let project = shelbi_state::load_project(project_name)?;

    let hub = project
        .machines
        .iter()
        .find(|m| matches!(m.kind, MachineKind::Local))
        .ok_or_else(|| {
            Error::Other(format!("project `{project_name}` has no local hub machine"))
        })?;
    let host = hub.host();

    let runner_spec = project
        .runner(&project.orchestrator.runner)
        .ok_or_else(|| {
            Error::Other(format!(
                "orchestrator runner `{}` not declared in project `{project_name}`",
                project.orchestrator.runner
            ))
        })?
        .clone();

    let addr = dashboard_addr(project_name);
    let session = &addr.session;
    let dashboard = format!("{session}:dashboard");

    // Materialize the orchestrator's workdir + CLAUDE.md upfront — needed
    // whether we create the session from scratch or just the right pane.
    let workdir = shelbi_state::project_dir(project_name)?;
    shelbi_state::ensure_dir(&workdir)?;
    let prompt = system_prompt(project_name)?;
    std::fs::write(workdir.join("CLAUDE.md"), &prompt).map_err(Error::Io)?;

    let shelbi_bin = std::env::current_exe()
        .map_err(Error::Io)?
        .to_string_lossy()
        .into_owned();
    let sidebar_cmd = format!(
        "{bin} __sidebar {proj}",
        bin = shelbi_agent::shell_escape(&shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
    );
    let launch = shelbi_agent::launch_command(&runner_spec);
    let orch_cmd = format!(
        "cd {wd} && SHELBI_PROJECT={proj} SHELBI_TMUX_SESSION={sess} exec {launch}",
        wd = shelbi_agent::shell_escape(&workdir.to_string_lossy()),
        proj = shelbi_agent::shell_escape(project_name),
        sess = shelbi_agent::shell_escape(session),
    );

    // 1. Ensure the project session exists with a `dashboard` window whose
    //    initial pane runs the sidebar directly (no send-keys race).
    if !shelbi_tmux::has_session(&host, session)? {
        shelbi_ssh::run_capture(
            &host,
            [
                "tmux",
                "new-session",
                "-d",
                "-s",
                session,
                "-n",
                "dashboard",
                "sh",
                "-c",
                &sidebar_cmd,
            ],
        )?;
    } else {
        let windows = shelbi_ssh::run_capture(
            &host,
            ["tmux", "list-windows", "-t", session, "-F", "#W"],
        )?;
        if !windows.lines().any(|w| w.trim() == "dashboard") {
            shelbi_ssh::run_capture(
                &host,
                [
                    "tmux",
                    "new-window",
                    "-d",
                    "-t",
                    &format!("{session}:"),
                    "-n",
                    "dashboard",
                    "sh",
                    "-c",
                    &sidebar_cmd,
                ],
            )?;
        }
    }

    // 2. If the dashboard already has 2+ panes, layout is set up.
    let panes = shelbi_ssh::run_capture(
        &host,
        ["tmux", "list-panes", "-t", &dashboard, "-F", "#P"],
    )?;
    let pane_count = panes.lines().filter(|l| !l.trim().is_empty()).count();
    if pane_count >= 2 {
        return Ok(BootstrapStatus::AlreadyRunning);
    }

    // 3. Split the dashboard window: orchestrator on the right.
    shelbi_ssh::run_capture(
        &host,
        [
            "tmux",
            "split-window",
            "-h",
            "-l",
            "70%",
            "-t",
            &dashboard,
            "sh",
            "-c",
            &orch_cmd,
        ],
    )?;
    // Focus the orchestrator pane so the user can type immediately.
    // Use the `{right}` selector (portable across pane-base-index 0/1).
    shelbi_ssh::run_capture(
        &host,
        ["tmux", "select-pane", "-t", &format!("{dashboard}.{{right}}")],
    )?;

    Ok(BootstrapStatus::Started)
}
