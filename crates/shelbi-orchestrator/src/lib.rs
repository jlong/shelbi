//! Orchestrator agent bootstrap.
//!
//! The orchestrator is a configured agent CLI (e.g. `claude`) running in
//! the project's tmux session in a window named `orchestrator`, with:
//!
//! 1. The `shelbi` binary on PATH, used as its tool surface.
//! 2. A generated system-prompt fragment (default + optional per-project
//!    `ORCHESTRATOR.md` override) materialized as `CLAUDE.md` in the
//!    orchestrator's workdir.
//! 3. `SHELBI_PROJECT` + `SHELBI_TMUX_SESSION` env vars set so every
//!    `shelbi` call the orchestrator shells out to resolves to the right
//!    context automatically.

use shelbi_core::{Error, MachineKind, Result, TmuxAddr};

pub const DEFAULT_SYSTEM_PROMPT: &str = include_str!("default_orchestrator.md");

/// Outcome of `ensure_running`.
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

/// The tmux address where the orchestrator window lives (or will live) for
/// the given project.
pub fn orchestrator_addr(project_name: &str) -> TmuxAddr {
    TmuxAddr {
        session: format!("shelbi-{project_name}"),
        window: "orchestrator".into(),
    }
}

/// Idempotently start the orchestrator pane for `project_name`. Safe to
/// call repeatedly — returns `AlreadyRunning` when the window exists.
///
/// This is called from both `shelbi orchestrate` and from the TUI's
/// first project-load so users don't have to manually spawn it.
pub fn ensure_running(project_name: &str) -> Result<BootstrapStatus> {
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

    let addr = orchestrator_addr(project_name);

    if !shelbi_tmux::has_session(&host, &addr.session)? {
        shelbi_tmux::new_session(&host, &addr.session, "shelbi", None)?;
    }

    // Detect whether the orchestrator window already exists.
    let listed = shelbi_ssh::run_capture(
        &host,
        ["tmux", "list-windows", "-t", &addr.session, "-F", "#W"],
    )?;
    if listed.lines().any(|w| w.trim() == addr.window) {
        return Ok(BootstrapStatus::AlreadyRunning);
    }

    // Materialize orchestrator workdir + CLAUDE.md prompt.
    let workdir = shelbi_state::project_dir(project_name)?;
    shelbi_state::ensure_dir(&workdir)?;
    let prompt = system_prompt(project_name)?;
    std::fs::write(workdir.join("CLAUDE.md"), &prompt).map_err(Error::Io)?;

    // Open the orchestrator window with an interactive shell (so rc files
    // run and PATH picks up shell-managed tools), then send-keys cd + env
    // + agent launch.
    shelbi_tmux::new_window(&host, &addr.session, &addr.window, None)?;
    let launch = shelbi_agent::launch_command(&runner_spec);
    let cmd_line = format!(
        "cd {wd} && SHELBI_PROJECT={proj} SHELBI_TMUX_SESSION={sess} exec {launch}",
        wd = shelbi_agent::shell_escape(&workdir.to_string_lossy()),
        proj = shelbi_agent::shell_escape(project_name),
        sess = shelbi_agent::shell_escape(&addr.session),
    );
    shelbi_tmux::send_line(&host, &addr, &cmd_line)?;

    Ok(BootstrapStatus::Started)
}
