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

pub mod review;
pub mod worker;

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

/// Swap the named view's pane into the dashboard's right slot. `view` is
/// one of `orch`, `tasks`, `review`, `machines`. Reads the stored pane id
/// from the session's tmux environment.
pub fn show_view(project_name: &str, view: &str) -> Result<()> {
    let session = format!("shelbi-{project_name}");
    let key = format!("SHELBI_PANE_{view}");

    // `show-environment -t session KEY` prints `KEY=value` (or `-KEY` if
    // unset). Parse it.
    let out = std::process::Command::new("tmux")
        .args(["show-environment", "-t", &session, &key])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "view `{view}` has no stored pane id ({}); is shelbi set up for this session?",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    let Some((_k, pane_id)) = line.split_once('=') else {
        return Err(Error::Other(format!("unexpected tmux env output: {line}")));
    };
    if pane_id.is_empty() {
        return Err(Error::Other(format!("empty pane id for `{view}`")));
    }

    // Swap the target pane into the dashboard's right slot.
    let dashboard = format!("{session}:dashboard.{{right}}");
    let _ = std::process::Command::new("tmux")
        .args(["swap-pane", "-s", pane_id, "-t", &dashboard])
        .status()
        .map_err(Error::Io)?;
    // Make sure focus lands on the now-visible view.
    let _ = std::process::Command::new("tmux")
        .args(["select-window", "-t", &format!("{session}:dashboard")])
        .status();
    let _ = std::process::Command::new("tmux")
        .args(["select-pane", "-t", &dashboard])
        .status();
    Ok(())
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

    // Install the session-closed cleanup hook before doing anything else.
    // Idempotent and project-agnostic — set every ensure_dashboard call so
    // it survives shelbi upgrades and tmux-server restarts.
    install_stash_cleanup_hook(&host)?;

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

    // Enable mouse on the project session so sidebar clicks and scroll
    // wheel reach the ratatui pane. Scoped to this session — won't disturb
    // mouse behavior in the user's other tmux sessions. Idempotent; safe
    // to call every bootstrap.
    let _ = shelbi_ssh::run_capture(
        &host,
        ["tmux", "set-option", "-t", session, "mouse", "on"],
    );

    // 2. If the dashboard already has 2+ panes, layout is set up.
    let panes = shelbi_ssh::run_capture(
        &host,
        ["tmux", "list-panes", "-t", &dashboard, "-F", "#P"],
    )?;
    let pane_count = panes.lines().filter(|l| !l.trim().is_empty()).count();
    if pane_count >= 2 {
        return Ok(BootstrapStatus::AlreadyRunning);
    }

    // Install the Ctrl+P tmux binding for the palette popup. The binding
    // itself is server-scoped (tmux has no session-local key bindings),
    // but the action is gated on the session name: outside a `shelbi-*`
    // session the keystroke is passed straight through with `send-keys`
    // so the user's other tmux sessions see Ctrl-P with no behavior
    // change. Gone if the tmux server restarts.
    let popup_cmd = format!("{} popup", shelbi_agent::shell_escape(&shelbi_bin));
    let _ = shelbi_ssh::run(
        &host,
        [
            "tmux",
            "bind-key",
            "-n",
            "C-p",
            "if-shell",
            "-F",
            "#{m:shelbi-*,#{session_name}}",
            &format!("run-shell \"{popup_cmd}\""),
            "send-keys C-p",
        ],
    );

    // 3. Split the dashboard window: orchestrator on the right.
    //    `-P -F #{pane_id}` echoes the new pane's stable ID (e.g. `%42`)
    //    which we'll stash in a session env var so the sidebar / palette
    //    can swap it back in by ID later.
    let orch_pane_id = shelbi_ssh::run_capture(
        &host,
        [
            "tmux",
            "split-window",
            "-h",
            "-l",
            "70%",
            "-t",
            &dashboard,
            "-P",
            "-F",
            "#{pane_id}",
            "sh",
            "-c",
            &orch_cmd,
        ],
    )?;
    let orch_pane_id = orch_pane_id.trim().to_string();
    set_session_env(&host, session, "SHELBI_PANE_orch", &orch_pane_id)?;

    // Focus the orchestrator pane so the user can type immediately.
    shelbi_ssh::run_capture(
        &host,
        ["tmux", "select-pane", "-t", &format!("{dashboard}.{{right}}")],
    )?;

    // 4. Materialize the hidden `__views` window with tasks/review/machines
    //    panes. Each runs a tiny watch loop or one-shot script. Sidebar
    //    swaps them into the dashboard's right pane via `tmux swap-pane`.
    create_hidden_views(&host, session, project_name, &shelbi_bin)?;

    Ok(BootstrapStatus::Started)
}

fn create_hidden_views(
    host: &shelbi_core::Host,
    session: &str,
    project_name: &str,
    shelbi_bin: &str,
) -> Result<()> {
    // Stash lives in a separate session — `_shelbi-<project>` — so the
    // user never sees a `__views` window in their visible session's
    // window list. Pane IDs are global in tmux, so swap-pane across
    // sessions works just like within one.
    let stash = format!("_{session}");

    // Already exists? Skip (idempotent).
    if shelbi_tmux::has_session(host, &stash)? {
        return Ok(());
    }

    let bin = shelbi_agent::shell_escape(shelbi_bin);
    let proj = shelbi_agent::shell_escape(project_name);
    // Tasks + review are real ratatui apps (`shelbi __tasks <p>`,
    // `shelbi __review <p>`). Wrap each in a `while true` loop so an
    // accidental crash or Ctrl-C respawns the TUI instead of leaving the
    // stash pane empty — palette swap-pane assumes the pane id stays alive.
    let tasks_cmd = format!(
        "while true; do {bin} __tasks {proj}; sleep 1; done",
        bin = bin,
        proj = proj,
    );
    let review_cmd = format!(
        "while true; do {bin} __review {proj}; sleep 1; done",
        bin = bin,
        proj = proj,
    );
    // Live worker/machine table — `shelbi worker list` probes each
    // worker's tmux pane and prints the assigned task (if any), so remote
    // workers show up alongside local ones with the same shape. Refresh
    // every 5s; the SSH probe per remote worker keeps this cheap-but-not-
    // free, hence the slower cadence than the kanban view.
    let machines_cmd = format!(
        "while true; do printf '\\033c'; echo 'workers · {proj_label}'; echo; {bin} --project {proj} worker list 2>&1; sleep 5; done",
        bin = bin,
        proj = proj,
        proj_label = project_name,
    );

    // Create the stash session detached, with tasks pane.
    let tasks_id = shelbi_ssh::run_capture(
        host,
        [
            "tmux", "new-session", "-d", "-s", &stash, "-n", "views",
            "-P", "-F", "#{pane_id}",
            "sh", "-c", &tasks_cmd,
        ],
    )?;
    let tasks_id = tasks_id.trim().to_string();

    let stash_win = format!("{stash}:views");

    let review_id = shelbi_ssh::run_capture(
        host,
        [
            "tmux", "split-window", "-v", "-t", &stash_win,
            "-P", "-F", "#{pane_id}",
            "sh", "-c", &review_cmd,
        ],
    )?;
    let review_id = review_id.trim().to_string();

    let machines_id = shelbi_ssh::run_capture(
        host,
        [
            "tmux", "split-window", "-v", "-t", &stash_win,
            "-P", "-F", "#{pane_id}",
            "sh", "-c", &machines_cmd,
        ],
    )?;
    let machines_id = machines_id.trim().to_string();

    // Env vars live on the *visible* session — that's where show_view reads
    // them from. swap-pane finds the target pane by global pane id anyway.
    set_session_env(host, session, "SHELBI_PANE_tasks", &tasks_id)?;
    set_session_env(host, session, "SHELBI_PANE_review", &review_id)?;
    set_session_env(host, session, "SHELBI_PANE_machines", &machines_id)?;
    Ok(())
}

fn set_session_env(
    host: &shelbi_core::Host,
    session: &str,
    key: &str,
    value: &str,
) -> Result<()> {
    shelbi_ssh::run_capture(host, ["tmux", "set-environment", "-t", session, key, value])?;
    Ok(())
}

/// Install a global `session-closed` hook on the tmux server so that when
/// the user kills a `shelbi-<project>` session its `_shelbi-<project>`
/// stash gets cleaned up too. The pattern `shelbi-*` ignores the stash
/// itself (`_shelbi-*`), so the hook can't recurse. Uses hook array index
/// 42 to avoid clobbering any unrelated `session-closed` hooks the user
/// may have set.
fn install_stash_cleanup_hook(host: &shelbi_core::Host) -> Result<()> {
    let hook_cmd = r##"run-shell -b "case \"#{hook_session_name}\" in shelbi-*) tmux kill-session -t \"_#{hook_session_name}\" 2>/dev/null;; esac""##;
    let _ = shelbi_ssh::run(
        host,
        ["tmux", "set-hook", "-g", "session-closed[42]", hook_cmd],
    );
    Ok(())
}
