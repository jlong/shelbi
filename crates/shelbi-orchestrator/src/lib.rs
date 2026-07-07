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
//! Workspace agents are additional windows in the same session (local) or
//! their own `shelbi-w-<id>` sessions on a remote machine (so they survive
//! SSH disconnect). The `shelbi orchestrate` CLI and the TUI launcher both
//! call into `ensure_dashboard()` so the bootstrap is idempotent and
//! consistent.

use shelbi_core::{Error, Host, MachineKind, Result, TmuxAddr};
use shelbi_state::keymap::{load_keymaps, GlobalAction};

pub mod actions;
pub mod branch;
pub mod dispatch;
mod git;
pub mod githook;
pub mod handoff;
pub mod lifecycle;
pub mod load;
pub mod ready;
pub mod supervision;
pub mod transition;
pub mod workspace;
pub mod zen;

#[cfg(test)]
pub(crate) mod test_lock {
    //! Shared mutex for any orchestrator-crate test that mutates the
    //! process-wide `SHELBI_HOME` env var. `actions.rs` and `lifecycle.rs`
    //! both spin up fixture homes; without a *single* lock they race the
    //! env var and produce flaky "No such file or directory" failures.
    use std::sync::{Mutex, MutexGuard};

    pub static LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the lock, recovering from a prior test that panicked with
    /// the guard held. A `PoisonError` here doesn't mean the test that
    /// poisoned it touched any state we care about — only that some
    /// other lock-holder panicked — so we take the inner guard and
    /// proceed.
    pub fn acquire() -> MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Bundled orchestrator system prompt. The template file lives in
/// `shelbi-state` so the per-project `agents/orchestrator/instructions.md`
/// materialize / self-heal path and this constant agree byte-for-byte.
/// Re-exported here so existing callers (the dashboard bootstrap) don't
/// have to learn a new import path.
pub const DEFAULT_SYSTEM_PROMPT: &str = shelbi_state::DEFAULT_ORCHESTRATOR_INSTRUCTIONS;

// Sidebar pane width is clamped to this char range. Below the min the
// footer hint (`  ^P palette  q quit`, ~20 chars) starts to truncate
// and the green `ZEN MODE ON` band has no room to breathe; above the
// max the orchestrator pane loses room without the sidebar gaining
// anything useful. Within the range the sidebar tracks
// `SIDEBAR_TARGET_PCT` of the window width — chosen so the
// orchestrator gets noticeably more room on both narrow and wide
// terminals than the previous fixed 30% split.
const SIDEBAR_MIN_COLS: u32 = 24;
const SIDEBAR_MAX_COLS: u32 = 40;
const SIDEBAR_TARGET_PCT: u32 = 25;

/// Outcome of `ensure_dashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapStatus {
    AlreadyRunning,
    Started,
}

/// Per-pane outcome for `reload`. Each pane is independent: the report
/// records what was found and whether the respawn succeeded.
#[derive(Debug, Default, Clone)]
pub struct ReloadReport {
    pub sidebar: PaneReloadStatus,
    pub tasks: PaneReloadStatus,
    pub machines: PaneReloadStatus,
    pub activity: PaneReloadStatus,
    /// Orchestrator (dashboard right pane). Respawned after the four
    /// shelbi-owned panes above so a freshly installed binary's
    /// updated `instructions.md` / preamble takes effect without the
    /// user having to manually tear down the orchestrator pane. The
    /// previous instance's in-flight state is carried forward via
    /// [`handoff::request_orchestrator_handoff`], whose outcome lives
    /// on [`ReloadReport::handoff`].
    pub orchestrator: PaneReloadStatus,
    /// What happened when we asked the previous orchestrator to write
    /// `agents/orchestrator/handoff.md` before the respawn. `None` is
    /// the legacy/no-attempt state; otherwise carries the outcome of
    /// the request (file written, pane already dead, timeout, etc.).
    pub handoff: Option<handoff::HandoffOutcome>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum PaneReloadStatus {
    #[default]
    NotAttempted,
    Respawned {
        target: String,
    },
    /// The pane didn't exist on the session yet (e.g. session predates a
    /// view that was added in a newer shelbi). Reload created it fresh
    /// and pinned the new pane id into the session env.
    Created {
        target: String,
    },
    Missing,
    Failed {
        target: String,
        reason: String,
    },
}

/// Relative path (from the orchestrator's workdir) where the composed
/// orchestrator system prompt is staged for claude's
/// `--append-system-prompt` flag. Mirrors the workspace-side
/// [`crate::workspace::WORKTREE_AGENT_INSTRUCTIONS_REL`] so both panes
/// load their agent context from the same conventional location.
pub const ORCH_AGENT_INSTRUCTIONS_REL: &str = ".claude/agent-instructions.md";

/// The dashboard window's tmux address (orchestrator's session).
pub fn dashboard_addr(project_name: &str) -> TmuxAddr {
    TmuxAddr {
        session: format!("shelbi-{project_name}"),
        window: "dashboard".into(),
    }
}

/// Is the project's orchestrator pane currently alive?
///
/// Reads the stashed `SHELBI_PANE_orch` pane id from the dashboard session
/// and checks it against tmux's live pane set. Deliberately conservative:
/// it returns `Ok(true)` — "assume alive, don't relaunch" — for every case
/// where we *can't* prove a real death (no local hub, the session is gone
/// because the user quit, or the pane id was never stashed on a pre-pin
/// session). It returns `Ok(false)` only when the session exists, a pane id
/// is stashed, and that pane is not among the live panes — an actual
/// orchestrator crash. This is what [`crate::supervision`] keys off to
/// relaunch the orchestrator (via [`ensure_dashboard`], whose
/// `__zen-orch-start` step keeps the Zen crash-recovery downgrade intact).
pub fn orchestrator_pane_alive(project_name: &str) -> Result<bool> {
    let project = shelbi_state::load_project(project_name)?;
    let Some(hub) = project
        .machines
        .iter()
        .find(|m| matches!(m.kind, MachineKind::Local))
    else {
        // No local hub → the orchestrator pane doesn't live on a box we
        // watch; nothing to supervise.
        return Ok(true);
    };
    let host = hub.host();
    let session = dashboard_addr(project_name).session;
    if !shelbi_tmux::has_session(&host, &session)? {
        // Session gone (the user quit the project / shelbi) — the whole
        // dashboard is down by design, not a crash to paper over.
        return Ok(true);
    }
    let Some(pane_id) = read_session_env_var(&host, &session, "SHELBI_PANE_orch")? else {
        // Never stashed (a session that pre-dates the pin, or one still
        // bootstrapping) — don't second-guess it.
        return Ok(true);
    };
    pane_id_alive(&host, &pane_id)
}

/// Is `pane_id` (a stable tmux `%N`) among the server's live panes? A failed
/// tmux call reports `Ok(false)` — treated as dead, matching the rest of the
/// liveness probes.
fn pane_id_alive(host: &Host, pane_id: &str) -> Result<bool> {
    let out = shelbi_ssh::run(host, ["tmux", "list-panes", "-a", "-F", "#{pane_id}"])
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|p| p.trim() == pane_id))
}

/// Swap the named view's pane into the dashboard's right slot. `view` is
/// one of `orch`, `tasks`, `machines`, `activity`. Reads the
/// stored pane id from the session's tmux environment.
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

    // Swap the target pane into the dashboard's right slot. A non-zero exit
    // here means the click silently no-ops (e.g. the stored pane id is stale
    // or the dashboard layout lost its `{right}` slot) — surface it instead
    // of discarding the status (Shelbi ContextStore
    // docs/planning:reviews/adversarial-2026-07/orchestrator-lifecycle.md F13).
    let dashboard = format!("{session}:dashboard.{{right}}");
    let swap = std::process::Command::new("tmux")
        .args(["swap-pane", "-s", pane_id, "-t", &dashboard])
        .output()
        .map_err(Error::Io)?;
    if !swap.status.success() {
        return Err(Error::Other(format!(
            "swap-pane failed for view `{view}` (pane {pane_id} → {dashboard}): {}",
            String::from_utf8_lossy(&swap.stderr).trim()
        )));
    }
    // Make sure focus lands on the now-visible view.
    let _ = std::process::Command::new("tmux")
        .args(["select-window", "-t", &format!("{session}:dashboard")])
        .status();
    let _ = std::process::Command::new("tmux")
        .args(["select-pane", "-t", &dashboard])
        .status();
    Ok(())
}

/// Focus the dashboard window on the declared workspace's pane,
/// lazily creating it if it doesn't exist yet.
///
/// Delegates to `shelbi open <name>` so the focus-or-create
/// decision lives in exactly one place. That CLI subcommand owns the
/// lifecycle wrapper that wraps local workspace panes (so a worker
/// dying writes a `pane_alive=false` event to `~/.shelbi/events.log`)
/// and preserves the remote proxy-window mechanism that makes devbox
/// workspaces clickable from the local sidebar.
///
/// Used by the sidebar's Enter-on-workspace handler and the Ctrl+P
/// palette's workspace entries — both call here so they can't drift.
pub fn focus_workspace(project_name: &str, workspace_name: &str) -> Result<()> {
    let shelbi_bin = current_exe_string()?;
    let out = std::process::Command::new(&shelbi_bin)
        .args(["--project", project_name, "open", workspace_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("status={}", out.status)
        } else {
            stderr
        };
        return Err(Error::Other(format!(
            "shelbi open `{workspace_name}` failed: {detail}"
        )));
    }
    Ok(())
}

/// Idempotently set up the project's tmux session with a `dashboard`
/// window split into sidebar (left) + orchestrator (right). Safe to call
/// repeatedly.
pub fn ensure_dashboard(project_name: &str) -> Result<BootstrapStatus> {
    let project = shelbi_state::load_project(project_name)?;

    // Serialize the whole bootstrap. `ensure_dashboard` is check-then-act
    // (count panes, split if <2); two callers racing it (CLI + TUI launcher)
    // would each split and double-split the dashboard or orphan the
    // orchestrator pane (F11). The loser blocks here, then finds the layout
    // already present and heals it below. Held until the guard drops at end
    // of scope.
    let _bootstrap_lock = shelbi_state::lock_dashboard(project_name)?;

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

    // Install/refresh the hub checkout's default-branch commit guard
    // (bug-worker-commit-landed-on-hub-main-checkout): a pre-commit hook
    // that rejects commits while HEAD is attached to the default branch,
    // so an agent session working in the hub checkout can't land code on
    // local `main` before cutting a branch. Best-effort — a work_dir
    // that isn't a git repo (fresh setup, test fixture) or a foreign user
    // hook degrades to a loud warning, not a failed project open.
    let mut protected: Vec<&str> = vec![&project.default_branch];
    if project.base_branch() != project.default_branch {
        protected.push(project.base_branch());
    }
    match githook::install_hub_branch_guard(&hub.work_dir, &protected) {
        Ok(githook::HookInstall::SkippedForeignHook) => {
            eprintln!(
                "shelbi: warning: {}/.git/hooks/pre-commit is user-authored — \
                 the default-branch commit guard was NOT installed; commits on \
                 `{}` in the hub checkout stay unguarded",
                hub.work_dir.display(),
                project.default_branch,
            );
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "shelbi: warning: couldn't install the default-branch commit \
                 guard in {}: {e}",
                hub.work_dir.display(),
            );
        }
    }

    // Materialize the orchestrator's workdir upfront — needed whether we
    // create the session from scratch or just the right pane. The
    // orchestrator's agent context (composed preamble +
    // `agents/orchestrator/instructions.md` + skills) is deployed into
    // the workdir's `.claude/` footprint and wired through claude's
    // `--append-system-prompt` flag below; the legacy
    // `<workdir>/CLAUDE.md` write is gone (see `aw-deprecate-claude-md-…`
    // task). A missing `agents/orchestrator/` is best-effort — the user
    // may have nuked it; the launch still succeeds, just without the
    // bundled orchestrator prompt.
    let workdir = shelbi_state::project_dir(project_name)?;
    shelbi_state::ensure_dir(&workdir)?;
    let _ = workspace::deploy_agent_context(
        &host,
        &workdir,
        project_name,
        shelbi_state::ORCHESTRATOR_AGENT,
    );

    // Drop the sidebar-clamp script. The bootstrapped hooks invoke it
    // via `sh <path>` — keeping the body in a file dodges all of the
    // tmux double-quote / $VAR / #{...} escape gymnastics that fighting
    // the same logic inline would require.
    let clamp_script_path = workdir.join("sidebar-clamp.sh");
    std::fs::write(&clamp_script_path, sidebar_clamp_script(session)).map_err(Error::Io)?;

    let shelbi_bin = current_exe_string()?;
    let sidebar_cmd_str = sidebar_cmd(&shelbi_bin, project_name);
    let launch = launch_with_bootstrap(&runner_spec, project_name, &workdir);
    let orch_cmd = orchestrator_pane_cmd(
        &shelbi_bin,
        project_name,
        session,
        &workdir.to_string_lossy(),
        &launch,
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
                &sidebar_cmd_str,
            ],
        )?;
    } else {
        let windows =
            shelbi_ssh::run_capture(&host, ["tmux", "list-windows", "-t", session, "-F", "#W"])?;
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
                    &sidebar_cmd_str,
                ],
            )?;
        }
    }

    // Enable mouse on the project session so sidebar clicks and scroll
    // wheel reach the ratatui pane. Scoped to this session — won't disturb
    // mouse behavior in the user's other tmux sessions. Idempotent; safe
    // to call every bootstrap.
    let _ = shelbi_ssh::run_capture(&host, ["tmux", "set-option", "-t", session, "mouse", "on"]);

    // Install / re-install the palette popup tmux binding using the chord
    // resolved from `keys.yaml` (with this project's overrides applied).
    // Runs on every bootstrap so switching to a project whose override
    // changes the chord rebinds tmux without manual fiddling.
    let _ = apply_palette_binding(&host, project_name, &shelbi_bin);

    // 2. If the dashboard already has 2+ panes, the split layout is set up.
    //    Don't return yet: the hidden-view stash may be missing or only
    //    partially built — a crash between the split and step 4, or a shelbi
    //    upgrade that added a view. The old early-return here (before step 4)
    //    meant that half-created stash never healed (F9). Run the idempotent
    //    view heal, then report AlreadyRunning.
    let panes =
        shelbi_ssh::run_capture(&host, ["tmux", "list-panes", "-t", &dashboard, "-F", "#P"])?;
    let pane_count = panes.lines().filter(|l| !l.trim().is_empty()).count();
    if pane_count >= 2 {
        ensure_hidden_views(&host, session, project_name, &shelbi_bin)?;
        return Ok(BootstrapStatus::AlreadyRunning);
    }

    // 3. Split the dashboard window: orchestrator on the right.
    //    Initial split is 50/50 — the sidebar-clamp hooks installed
    //    below set the final sizing as soon as a client attaches (or
    //    immediately, if we're being run from inside one).
    //    `-P -F #{pane_id}` echoes the new pane's stable ID (e.g. `%42`)
    //    which we'll stash in a session env var so the sidebar / palette
    //    can swap it back in by ID later.
    let orch_pane_id = shelbi_ssh::run_capture(
        &host,
        [
            "tmux",
            "split-window",
            "-h",
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
        [
            "tmux",
            "select-pane",
            "-t",
            &format!("{dashboard}.{{right}}"),
        ],
    )?;

    // Bound the sidebar to a sane char-width range so it neither
    // bloats on wide terminals nor cramps the orchestrator on narrow
    // ones. The hooks re-clamp on every client resize (including the
    // first attach); the one-shot below covers the in-tmux
    // `switch-client` path, where no attach event fires.
    install_sidebar_clamp_hooks(&host, session, &clamp_script_path)?;
    let _ = clamp_sidebar(&host, &clamp_script_path);

    // 4. Materialize the hidden `__views` window with tasks/review/machines
    //    panes. Each runs a tiny watch loop or one-shot script. Sidebar
    //    swaps them into the dashboard's right pane via `tmux swap-pane`.
    ensure_hidden_views(&host, session, project_name, &shelbi_bin)?;

    Ok(BootstrapStatus::Started)
}

/// Resolve the palette-open chord from the user's keys.yaml (with this
/// project's overrides applied) and install it as a tmux `bind-key`. The
/// previous binding — read from `~/.shelbi/state.json::tmux_palette_key` —
/// is unbound first so we don't leave a stale entry behind when the chord
/// changes between bootstraps or project switches.
///
/// The bind itself uses tmux `if-shell` to scope the palette popup to
/// `shelbi-*` sessions: outside one, the keystroke is passed through with
/// `send-keys` so the user's other tmux sessions see the chord unchanged.
/// That preserves the historical behavior of the hardcoded `C-p` bind
/// the user could rely on in their own sessions.
///
/// Chords that can't be expressed in tmux syntax (currently anything with
/// the `super` modifier — see [`shelbi_state::keymap::KeyChord::to_tmux_key`])
/// fall back to `C-p` with a stderr warning. Refusing to install anything
/// would brick palette access, which is worse than ignoring the override.
fn apply_palette_binding(
    host: &shelbi_core::Host,
    project_name: &str,
    shelbi_bin: &str,
) -> Result<String> {
    let (keymaps, _diags) = load_keymaps(Some(project_name));
    let chord = keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .copied()
        .expect("OpenPalette must have a default chord");

    let tmux_key = chord.to_tmux_key().unwrap_or_else(|| {
        eprintln!(
            "warning: palette chord `{}` is not tmux-expressible; falling back to C-p",
            chord.canonical(),
        );
        "C-p".to_string()
    });

    // Unbind the prior key (if any) so a chord change doesn't leave the
    // old binding hanging around on the tmux server.
    let prev = shelbi_state::read_global_state()
        .ok()
        .and_then(|s| s.tmux_palette_key);
    if let Some(prev_key) = prev.as_deref() {
        if prev_key != tmux_key {
            let _ = shelbi_ssh::run(host, ["tmux", "unbind-key", "-n", prev_key]);
        }
    }

    // Install the binding. Gated to shelbi-* sessions; non-shelbi
    // sessions see the chord pass straight through via send-keys so the
    // user's other tmux sessions are unaffected. The binding is global
    // to the tmux server and is gone if the server restarts — bootstrap
    // re-installs it on the next `ensure_dashboard` call.
    let popup_cmd = format!("{} popup", shelbi_agent::shell_escape(shelbi_bin));
    let _ = shelbi_ssh::run(
        host,
        [
            "tmux",
            "bind-key",
            "-n",
            &tmux_key,
            "if-shell",
            "-F",
            "#{m:shelbi-*,#{session_name}}",
            &format!("run-shell \"{popup_cmd}\""),
            &format!("send-keys {tmux_key}"),
        ],
    );

    // Persist the new key so the next bootstrap / project switch knows
    // what to unbind. Best-effort: a missing $SHELBI_HOME (already
    // surfaced elsewhere) shouldn't block the rest of bootstrap.
    let _ = shelbi_state::update_global_state(|state| {
        state.tmux_palette_key = Some(tmux_key.clone());
        Ok(())
    });

    Ok(tmux_key)
}

/// The three hidden views, in creation order, paired with the pane command
/// each runs. Order matters only for the fresh-build path (`tasks` seeds the
/// stash session; the rest split off it), but a single source of truth keeps
/// the fresh build and the heal pass from drifting apart.
fn hidden_view_cmds(shelbi_bin: &str, project_name: &str) -> [(&'static str, String); 3] {
    [
        ("tasks", tasks_cmd(shelbi_bin, project_name)),
        ("machines", machines_cmd(shelbi_bin, project_name)),
        ("activity", activity_cmd(shelbi_bin, project_name)),
    ]
}

/// Idempotently ensure the hidden `__views` stash exists with a live pane for
/// every view. On a fresh session this builds the whole stash; on an existing
/// one it heals only the panes that are missing or dead — the state a crash
/// mid-bootstrap or a shelbi upgrade (new view added) leaves behind, which the
/// coarse `has_session` skip in `create_hidden_views` never repaired (F9).
///
/// Safe to call whether or not the visible dashboard already looks complete;
/// `ensure_dashboard` runs it on both the fresh-split and already-running
/// paths so a partially-built stash always converges to four live panes.
fn ensure_hidden_views(
    host: &shelbi_core::Host,
    session: &str,
    project_name: &str,
    shelbi_bin: &str,
) -> Result<()> {
    let stash = format!("_{session}");

    // No stash yet — build it whole (the `tasks` pane seeds the session, the
    // rest split off it). This is the original first-time bootstrap path.
    if !shelbi_tmux::has_session(host, &stash)? {
        return create_hidden_views(host, session, project_name, shelbi_bin);
    }

    // Stash exists: heal per view. A view is healthy iff `SHELBI_PANE_<view>`
    // is set on the visible session AND names a pane still alive in the stash.
    // Anything else gets a fresh pane spliced into the `views` window and its
    // id pinned back into the session env so `show_view` can swap it in.
    let stash_win = format!("{stash}:views");
    // A partial stash (seed pane + up to four heals) can exceed the four-pane
    // budget of the detached default 80x24 window and trip `no space for new
    // pane`. Grow it first; the panes render at the visible dashboard's size
    // once swapped in, so the stash geometry is immaterial.
    resize_stash_window(host, &stash_win);
    let live = live_stash_pane_ids(host, &stash)?;
    for (view, cmd) in hidden_view_cmds(shelbi_bin, project_name) {
        let env_key = format!("SHELBI_PANE_{view}");
        let healthy = read_session_env_var(host, session, &env_key)?
            .map(|id| live.contains(&id))
            .unwrap_or(false);
        if healthy {
            continue;
        }
        let pane_id = shelbi_ssh::run_capture(
            host,
            [
                "tmux",
                "split-window",
                "-v",
                "-t",
                &stash_win,
                "-P",
                "-F",
                "#{pane_id}",
                "sh",
                "-c",
                &cmd,
            ],
        )?;
        set_session_env(host, session, &env_key, pane_id.trim())?;
    }
    Ok(())
}

/// Grow the detached stash window so vertical splits always have room. The
/// default 80x24 detached window fits only ~4 stacked panes; a partial-heal
/// pass can need more transiently. Best-effort — a failure just risks the
/// original `no space` error on the next split, which surfaces there.
fn resize_stash_window(host: &shelbi_core::Host, stash_win: &str) {
    let _ = shelbi_ssh::run(
        host,
        [
            "tmux",
            "resize-window",
            "-t",
            stash_win,
            "-x",
            "220",
            "-y",
            "200",
        ],
    );
}

/// Pane ids currently alive in the stash's `views` window.
fn live_stash_pane_ids(host: &shelbi_core::Host, stash: &str) -> Result<Vec<String>> {
    let out = shelbi_ssh::run_capture(
        host,
        [
            "tmux",
            "list-panes",
            "-t",
            &format!("{stash}:views"),
            "-F",
            "#{pane_id}",
        ],
    )?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Read `key` from `session`'s tmux environment over `host`. Returns `None`
/// when the variable is unset or explicitly cleared (`-KEY`). Mirrors the
/// local-only `read_pane_id`, but routes through `host` so it also works on
/// the remote-hub path. Uses `run` (not `run_capture`) because tmux exits
/// non-zero for an unknown variable — a legitimate "unset", not an error.
fn read_session_env_var(
    host: &shelbi_core::Host,
    session: &str,
    key: &str,
) -> Result<Option<String>> {
    let out = shelbi_ssh::run(host, ["tmux", "show-environment", "-t", session, key])
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    // `-KEY` form means the variable is explicitly unset on this session.
    if line.starts_with('-') {
        return Ok(None);
    }
    match line.split_once('=') {
        Some((_, value)) if !value.is_empty() => Ok(Some(value.to_string())),
        _ => Ok(None),
    }
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

    // Already exists? Skip (idempotent). Per-pane healing of an existing
    // stash is `ensure_hidden_views`' job — this builds the whole thing.
    if shelbi_tmux::has_session(host, &stash)? {
        return Ok(());
    }

    let stash_win = format!("{stash}:views");

    // Build all four panes from the single source of truth. The first pane
    // seeds the detached stash session; the rest split off it. Each pane's
    // id is pinned into the *visible* session env as soon as it's created —
    // that's where `show_view` reads them from, and setting them
    // incrementally means a crash partway through leaves valid env entries
    // for whatever was created (the rest heal on the next bootstrap).
    for (i, (view, cmd)) in hidden_view_cmds(shelbi_bin, project_name)
        .iter()
        .enumerate()
    {
        let pane_id = if i == 0 {
            let id = shelbi_ssh::run_capture(
                host,
                [
                    "tmux",
                    "new-session",
                    "-d",
                    "-s",
                    &stash,
                    "-n",
                    "views",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "sh",
                    "-c",
                    cmd,
                ],
            )?;
            // Grow the detached window before stacking the rest so the splits
            // don't run out of rows (default detached size is only 80x24).
            resize_stash_window(host, &stash_win);
            id
        } else {
            shelbi_ssh::run_capture(
                host,
                [
                    "tmux",
                    "split-window",
                    "-v",
                    "-t",
                    &stash_win,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "sh",
                    "-c",
                    cmd,
                ],
            )?
        };
        set_session_env(
            host,
            session,
            &format!("SHELBI_PANE_{view}"),
            pane_id.trim(),
        )?;
    }
    Ok(())
}

fn set_session_env(host: &shelbi_core::Host, session: &str, key: &str, value: &str) -> Result<()> {
    shelbi_ssh::run_capture(host, ["tmux", "set-environment", "-t", session, key, value])?;
    Ok(())
}

/// Shell snippet that queries the dashboard window's current width via
/// tmux, computes `SIDEBAR_TARGET_PCT%` clamped to `[MIN, MAX]`, and
/// resizes the left (sidebar) pane to that. Written to disk so the
/// hook can invoke it by path without inlining shell into a tmux
/// command-list string.
fn sidebar_clamp_script(session: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # Auto-generated by shelbi; rewritten on every `ensure_dashboard`.\n\
         w=$(tmux display-message -p -t '{sess}:dashboard' '#{{window_width}}' 2>/dev/null)\n\
         [ -z \"$w\" ] && exit 0\n\
         c=$((w * {pct} / 100))\n\
         [ \"$c\" -lt {min} ] && c={min}\n\
         [ \"$c\" -gt {max} ] && c={max}\n\
         tmux resize-pane -t '{sess}:dashboard.{{left}}' -x \"$c\" 2>/dev/null || true\n",
        sess = session,
        pct = SIDEBAR_TARGET_PCT,
        min = SIDEBAR_MIN_COLS,
        max = SIDEBAR_MAX_COLS,
    )
}

/// Install `client-attached` and `client-resized` hooks on the session
/// so the sidebar pane is re-clamped to `[MIN, MAX]` cols every time
/// the client's terminal size changes. Without this the pane would
/// scale proportionally with the window, which is exactly what we're
/// trying to avoid.
fn install_sidebar_clamp_hooks(
    host: &shelbi_core::Host,
    session: &str,
    script_path: &std::path::Path,
) -> Result<()> {
    let path_esc = shelbi_agent::shell_escape(&script_path.to_string_lossy());
    let hook_cmd = format!("run-shell -b 'sh {path_esc}'");
    for event in ["client-attached", "client-resized"] {
        let _ = shelbi_ssh::run(host, ["tmux", "set-hook", "-t", session, event, &hook_cmd]);
    }
    Ok(())
}

/// Run the clamp once now, for the in-tmux `switch-client` path where
/// no `client-attached` fires. Best-effort; failures are silent because
/// any real client interaction will re-trigger the hook.
fn clamp_sidebar(host: &shelbi_core::Host, script_path: &std::path::Path) -> std::io::Result<()> {
    let path = script_path.to_string_lossy();
    shelbi_ssh::run(host, ["sh", path.as_ref()]).map(|_| ())
}

/// Install a global `session-closed` hook on the tmux server so that when
/// the user kills a `shelbi-<project>` session its `_shelbi-<project>`
/// stash gets cleaned up too. The pattern `shelbi-*` ignores the stash
/// itself (`_shelbi-*`), so the hook can't recurse. Uses hook array index
/// 42 to avoid clobbering any unrelated `session-closed` hooks the user
/// may have set.
fn install_stash_cleanup_hook(host: &shelbi_core::Host) -> Result<()> {
    let hook_cmd = r##"run-shell -b "case \"#{hook_session_name}\" in shelbi-*) tmux kill-session -t \"_#{hook_session_name}\" 2>/dev/null;; esac; true""##;
    let _ = shelbi_ssh::run(
        host,
        ["tmux", "set-hook", "-g", "session-closed[42]", hook_cmd],
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Shelbi-owned pane command builders.
//
// Single source of truth for what each pane runs. Both `ensure_dashboard`
// (first-time bootstrap) and `reload` (in-place respawn after a fresh
// binary install) format their `sh -c` strings through these — otherwise
// they would drift.

fn current_exe_string() -> Result<String> {
    Ok(std::env::current_exe()
        .map_err(Error::Io)?
        .to_string_lossy()
        .into_owned())
}

fn sidebar_cmd(shelbi_bin: &str, project_name: &str) -> String {
    format!(
        "{bin} __sidebar {proj}",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
    )
}

/// Initial positional prompt fed to the orchestrator agent on launch so
/// it runs the "Bootstrap on session start" sequence from its
/// `instructions.md` without waiting for the user to type "start
/// monitoring". The prompt names every step verbatim so the agent can't
/// elide arming the `shelbi events tail --follow` watch — that's the
/// step that turns auto-dispatch back on after a cold start.
const ORCH_BOOTSTRAP_PROMPT: &str = "Run the \"Bootstrap on session start\" sequence \
    from your instructions now: snapshot `shelbi task list`, `shelbi workspace list`, and \
    `shelbi zen status`; scan recent `~/.shelbi/events.log` for a \
    `zen=off reason=crash-recovery` line; then start `shelbi events tail --follow` \
    in the background and watch it with the Monitor tool so auto-dispatch reacts \
    to new transitions. If this runner cannot receive asynchronous Monitor callbacks, \
    also follow the \"Polling-only event drain\" section: before every user-facing \
    reply, run `shelbi orchestrator events drain` (the durable cursor is persisted \
    for you in the project config dir and resumes automatically), apply any returned \
    task/workspace/heartbeat/pane-death facts through the normal reaction rules, and \
    only then answer.";

/// Wrap `launch_command(runner_spec)` with the orchestrator's
/// auto-bootstrap context.
///
/// Claude receives the composed `agents/orchestrator/instructions.md`
/// through `--append-system-prompt` and the bootstrap request as its
/// first positional prompt, preserving the historical Claude startup
/// shape. Codex has no `--append-system-prompt` equivalent in Shelbi's
/// runner abstraction, but its interactive CLI accepts an initial
/// positional prompt; for Codex we build that prompt from the project
/// identity, worktree path, rendered instructions file, bootstrap
/// request, and any reload handoff context spliced into the rendered
/// file so the first turn already knows it is Shelbi's scheduler.
fn launch_with_bootstrap(
    spec: &shelbi_core::AgentRunnerSpec,
    project_name: &str,
    workdir: &std::path::Path,
) -> String {
    let launch = shelbi_agent::launch_command(spec);
    if is_claude_runner(spec) {
        format!(
            "{launch} --append-system-prompt \"$(cat {rel})\" {prompt}",
            rel = shelbi_agent::shell_escape(ORCH_AGENT_INSTRUCTIONS_REL),
            prompt = shelbi_agent::shell_escape(ORCH_BOOTSTRAP_PROMPT),
        )
    } else if is_codex_runner(spec) {
        format!(
            "{launch} {prompt}",
            prompt = codex_orchestrator_prompt_arg(project_name, workdir),
        )
    } else {
        launch
    }
}

fn is_claude_runner(spec: &shelbi_core::AgentRunnerSpec) -> bool {
    std::path::Path::new(&spec.command)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("claude")
}

fn is_codex_runner(spec: &shelbi_core::AgentRunnerSpec) -> bool {
    std::path::Path::new(&spec.command)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("codex")
}

fn codex_orchestrator_prompt_arg(project_name: &str, workdir: &std::path::Path) -> String {
    let workdir = workdir.to_string_lossy();
    let before = format!(
        "You are Shelbi's orchestrator/scheduler for project `{project_name}`.\n\
         Project worktree: `{workdir}`.\n\
         Do not edit project code directly; coordinate workspaces and board state.\n\n\
         Authoritative Shelbi orchestrator instructions follow. Treat them as your developer-agent contract. \
         They include the project-local orchestrator role, bootstrap rules, event-tail responsibility, \
         Zen Mode rules, and any reload handoff context captured before this pane was restarted. \
         If a handoff `<system-reminder>` block is present there, use it as continuity context.\n\n\
         This is a polling-only runner contract: before every user-facing reply, drain \
         pending project events with `shelbi orchestrator events drain` (the cursor is \
         persisted for you in the project config dir and resumes automatically regardless \
         of your shell's working directory; pass `--cursor <N>` only to replay from an \
         explicit offset), apply any returned task transitions, workspace transitions, \
         heartbeats, and pane-death facts through the normal reaction rules, and only then \
         answer the user. The drain gives facts; you remain responsible for scheduling \
         decisions.\n\n",
    );
    let after = format!("\n\n{ORCH_BOOTSTRAP_PROMPT}");
    concat_shell_prompt_parts(&before, ORCH_AGENT_INSTRUCTIONS_REL, &after)
}

fn concat_shell_prompt_parts(before: &str, cat_rel: &str, after: &str) -> String {
    format!(
        "\"$(printf %s {before})$(cat {cat_rel})$(printf %s {after})\"",
        before = shelbi_agent::shell_escape(before),
        cat_rel = shelbi_agent::shell_escape(cat_rel),
        after = shelbi_agent::shell_escape(after),
    )
}

/// Heartbeat cadence for the orchestrator pane's background liveness
/// loop. Sized so a stalled write or one missed tick still falls
/// comfortably inside `ZEN_CRASH_RECOVERY_WINDOW_SECS` — the next
/// startup must see a recent timestamp to infer the crash.
const ORCH_HEARTBEAT_INTERVAL_SECS: u32 = 60;

/// Build the `sh -c` script the orchestrator pane runs. The script
/// wraps the agent launch with the Zen Mode lifecycle so a pane crash
/// auto-disables Zen on the next start:
///
/// 1. `__zen-orch-start` — check `state.json` for a recent unmatched
///    heartbeat; if found, force `zen_mode = off` and warn.
/// 2. background heartbeat — refresh the timestamp every 60s; bytes
///    only land in `state.json`, never `events.log`.
/// 3. `<launch>` — the configured agent (e.g. `claude`).
/// 4. `__zen-orch-exit` — clears the timestamp on graceful exit.
///
/// If the pane is killed mid-run, the whole process group dies before
/// step 4, leaving the heartbeat timestamp in place — that's the
/// signal step 1 reads on the next start.
///
/// Note we deliberately don't `exec` the launch: the wrapper shell
/// must survive the agent's exit to run step 4 and reap the
/// background heartbeat.
fn orchestrator_pane_cmd(
    shelbi_bin: &str,
    project_name: &str,
    session: &str,
    workdir: &str,
    launch: &str,
) -> String {
    let bin = shelbi_agent::shell_escape(shelbi_bin);
    let proj = shelbi_agent::shell_escape(project_name);
    let sess = shelbi_agent::shell_escape(session);
    let wd = shelbi_agent::shell_escape(workdir);
    let interval = ORCH_HEARTBEAT_INTERVAL_SECS;
    format!(
        "cd {wd} && \
         export SHELBI_PROJECT={proj} SHELBI_TMUX_SESSION={sess} && \
         {bin} __zen-orch-start {proj}; \
         ({bin} __zen-heartbeat {proj}; \
            while sleep {interval}; do {bin} __zen-heartbeat {proj}; done) & \
         HB=$!; \
         {launch}; \
         RC=$?; \
         kill $HB 2>/dev/null; \
         wait $HB 2>/dev/null; \
         {bin} __zen-orch-exit {proj}; \
         exit $RC",
    )
}

// Tasks is a real ratatui app (`shelbi __tasks <p>`). Wrap it in a `while
// true` loop so an accidental crash or Ctrl-C respawns the TUI instead of
// leaving the stash pane empty — palette swap-pane assumes the pane id stays
// alive.
fn tasks_cmd(shelbi_bin: &str, project_name: &str) -> String {
    format!(
        "while true; do {bin} __tasks {proj}; sleep 1; done",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
    )
}

fn activity_cmd(shelbi_bin: &str, project_name: &str) -> String {
    format!(
        "while true; do {bin} __activity {proj}; sleep 1; done",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
    )
}

// Live workspace/machine table — `shelbi workspace list` probes each workspace's
// tmux pane and prints the assigned task (if any), so remote workspaces
// show up alongside local ones with the same shape. Refresh every 5s;
// the SSH probe per remote workspace keeps this cheap-but-not-free, hence
// the slower cadence than the kanban view.
fn machines_cmd(shelbi_bin: &str, project_name: &str) -> String {
    // The label must be shell-escaped like every other value: a raw
    // project name interpolated into the single-quoted `echo` broke the
    // render loop on a name containing `'` and let `x'; rm -rf ~; echo '`
    // execute. Pass the escaped name as a separate `echo` argument so it's
    // printed literally, never re-parsed by the shell.
    format!(
        "while true; do printf '\\033c'; echo 'workspaces ·' {proj}; echo; {bin} --project {proj} workspace list 2>&1; sleep 5; done",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
    )
}

// ---------------------------------------------------------------------------
// reload — respawn shelbi-owned panes in-place so a freshly installed
// binary takes effect without disturbing the orchestrator or workspaces.

/// Respawn the long-lived shelbi-owned panes in-place so an updated
/// `shelbi` binary takes effect. Targets:
///
/// - `shelbi-<project>:dashboard.{left}` → `shelbi __sidebar <project>`
/// - stash `tasks` pane → tasks-view loop
/// - stash `machines` pane → `shelbi workspace list` loop
/// - stash `activity` pane → activity-view loop
/// - orchestrator pane (`dashboard.{right}`) → its launch wrapper
///
/// Before respawning the orchestrator pane, the previous instance is
/// asked to write `agents/orchestrator/handoff.md` — a one-shot
/// state-transfer file the new instance ingests via the
/// `deploy_agent_context` splice path. See
/// [`handoff::request_orchestrator_handoff`] for the request/poll
/// dance, [`handoff::HandoffOutcome`] for the variants. A missing or
/// timed-out handoff degrades to a cold start, not a stuck reload.
///
/// For each stash view, if `SHELBI_PANE_<view>` isn't set on the
/// session (the session was bootstrapped before that view existed),
/// reload creates the pane fresh in the stash window and pins its id
/// into the session env — so a freshly-installed binary that adds a
/// new view becomes clickable without re-creating the whole session.
///
/// Out of scope: workspace panes (claude re-shells out on each CLI
/// call). Those pick up the new binary automatically the next time
/// they invoke `shelbi`.
///
/// Idempotent: re-running incurs a visible flicker per pane but no
/// state loss — the panes' job is to render derived state from disk,
/// so a fresh process picks up where the old one was. A missing pane
/// is created on the first call and respawned on subsequent ones.
pub fn reload(project_name: &str) -> Result<ReloadReport> {
    let session = format!("shelbi-{project_name}");

    // Session must exist — there's nothing to reload if the user hasn't
    // booted the dashboard yet.
    if !local_session_exists(&session)? {
        return Err(Error::Other(format!(
            "session `{session}` not running; run `shelbi orchestrate` first"
        )));
    }

    let shelbi_bin = current_exe_string()?;

    // 0. Ask the previous orchestrator to write its handoff before we
    //    touch any pane. Done up front so the orchestrator has the most
    //    time possible to respond before we get to the respawn step,
    //    and so its write doesn't race the sidebar flicker. Errors are
    //    swallowed — the report's `handoff` field records what
    //    happened for the caller's display.
    let handoff_outcome = match handoff::request_orchestrator_handoff(project_name) {
        Ok(outcome) => Some(outcome),
        Err(e) => {
            tracing::warn!(
                project = project_name,
                error = %e,
                "request_orchestrator_handoff failed; orchestrator will restart cold",
            );
            None
        }
    };
    let mut report = ReloadReport {
        handoff: handoff_outcome,
        ..Default::default()
    };

    // Re-apply the palette tmux binding so reload picks up `keys.yaml`
    // edits without forcing the user to kill + restart the dashboard.
    let _ = apply_palette_binding(&Host::Local, project_name, &shelbi_bin);

    // 1. Sidebar — pane id isn't stored at bootstrap; target positionally.
    //    `dashboard.{left}` resolves to the leftmost pane in the dashboard
    //    window, which is always the sidebar (the orchestrator's split
    //    landed on the right and view-swaps only touch dashboard.{right}).
    let sidebar_target = format!("{session}:dashboard.{{left}}");
    report.sidebar = respawn_pane(&sidebar_target, &sidebar_cmd(&shelbi_bin, project_name));

    // 2-5. Stash panes — pane ids are stored in session env at bootstrap.
    report.tasks = reload_stash_pane(&session, "tasks", &tasks_cmd(&shelbi_bin, project_name));
    report.machines = reload_stash_pane(
        &session,
        "machines",
        &machines_cmd(&shelbi_bin, project_name),
    );
    report.activity = reload_stash_pane(
        &session,
        "activity",
        &activity_cmd(&shelbi_bin, project_name),
    );

    // 6. Orchestrator pane. Re-deploy the agent context first so the new
    //    launch sees the latest `instructions.md` / `preamble.md` AND
    //    splices in the handoff (then deletes it). Then respawn the
    //    pane in place — the pane id is preserved by `respawn-pane`,
    //    so `show_view("orch")` keeps working.
    report.orchestrator = reload_orchestrator_pane(&session, project_name, &shelbi_bin);

    Ok(report)
}

/// Respawn the orchestrator pane in place. Re-deploys the agent
/// context (which composes `agents/_shared/preamble.md` +
/// `agents/orchestrator/instructions.md` + spliced handoff) and
/// rebuilds the launch wrapper. Failures are surfaced as
/// `PaneReloadStatus::Failed` rather than aborting the broader reload
/// — the four shelbi-owned panes have already been respawned
/// successfully and the user can still drive the dashboard while the
/// orchestrator is down.
fn reload_orchestrator_pane(
    session: &str,
    project_name: &str,
    shelbi_bin: &str,
) -> PaneReloadStatus {
    let project = match shelbi_state::load_project(project_name) {
        Ok(p) => p,
        Err(e) => {
            return PaneReloadStatus::Failed {
                target: format!("{session}:dashboard.{{right}}"),
                reason: format!("load_project: {e}"),
            };
        }
    };
    let runner_spec = match project.runner(&project.orchestrator.runner) {
        Some(r) => r.clone(),
        None => {
            return PaneReloadStatus::Failed {
                target: format!("{session}:dashboard.{{right}}"),
                reason: format!(
                    "orchestrator runner `{}` not declared in project",
                    project.orchestrator.runner,
                ),
            };
        }
    };
    let workdir = match shelbi_state::project_dir(project_name) {
        Ok(d) => d,
        Err(e) => {
            return PaneReloadStatus::Failed {
                target: format!("{session}:dashboard.{{right}}"),
                reason: format!("project_dir: {e}"),
            };
        }
    };
    // Re-stage `.claude/agent-instructions.md` from current on-disk
    // instructions + preamble, splicing in handoff.md if present and
    // deleting it after read. Best-effort — failures are logged and
    // the launch proceeds with whatever's already in the file.
    if let Err(e) = workspace::deploy_agent_context(
        &Host::Local,
        &workdir,
        project_name,
        shelbi_state::ORCHESTRATOR_AGENT,
    ) {
        tracing::warn!(
            project = project_name,
            error = %e,
            "deploy_agent_context failed during reload; using stale agent-instructions.md",
        );
    }

    let launch = launch_with_bootstrap(&runner_spec, project_name, &workdir);
    let cmd = orchestrator_pane_cmd(
        shelbi_bin,
        project_name,
        session,
        &workdir.to_string_lossy(),
        &launch,
    );

    if let Err(e) = mark_orchestrator_reload_expected(project_name) {
        return PaneReloadStatus::Failed {
            target: format!("{session}:dashboard.{{right}}"),
            reason: format!("mark expected orchestrator shutdown: {e}"),
        };
    }

    // Prefer the stored pane id so a view-swap-mid-reload (orchestrator
    // not currently visible in the right slot) still hits the right
    // pane. Fall back to the positional `{right}` target for older
    // sessions that pre-date the `SHELBI_PANE_orch` env pin.
    let target = match read_pane_id(session, "orch") {
        Ok(Some(id)) => id,
        Ok(None) => format!("{session}:dashboard.{{right}}"),
        Err(e) => {
            return PaneReloadStatus::Failed {
                target: "(env SHELBI_PANE_orch)".into(),
                reason: e.to_string(),
            };
        }
    };
    respawn_pane(&target, &cmd)
}

fn mark_orchestrator_reload_expected(project_name: &str) -> Result<()> {
    shelbi_state::zen_clear_crash(project_name)
}

fn reload_stash_pane(session: &str, view: &str, cmd: &str) -> PaneReloadStatus {
    match read_pane_id(session, view) {
        Ok(Some(id)) => respawn_pane(&id, cmd),
        // Session predates this view — allocate a fresh pane in the stash
        // window and pin its id into the session env so show_view can find
        // it without requiring the user to recreate the session.
        Ok(None) => create_stash_pane(session, view, cmd),
        Err(e) => PaneReloadStatus::Failed {
            target: format!("(env SHELBI_PANE_{view})"),
            reason: e.to_string(),
        },
    }
}

/// Allocate a new pane in the stash session's `views` window, run `cmd`
/// in it, and set `SHELBI_PANE_<view>` on the visible session to the
/// new pane id. Mirrors what `create_hidden_views` does at bootstrap,
/// but for a single view at a time and over local tmux (reload always
/// runs on the hub).
fn create_stash_pane(session: &str, view: &str, cmd: &str) -> PaneReloadStatus {
    let stash_win = format!("_{session}:views");

    let split = std::process::Command::new("tmux")
        .args([
            "split-window",
            "-v",
            "-t",
            &stash_win,
            "-P",
            "-F",
            "#{pane_id}",
            "sh",
            "-c",
            cmd,
        ])
        .output();
    let pane_id = match split {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => {
            return PaneReloadStatus::Failed {
                target: stash_win,
                reason: format!(
                    "split-window failed: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
            };
        }
        Err(e) => {
            return PaneReloadStatus::Failed {
                target: stash_win,
                reason: e.to_string(),
            };
        }
    };
    if pane_id.is_empty() {
        return PaneReloadStatus::Failed {
            target: stash_win,
            reason: "tmux returned empty pane id from split-window".to_string(),
        };
    }

    let key = format!("SHELBI_PANE_{view}");
    let set = std::process::Command::new("tmux")
        .args(["set-environment", "-t", session, &key, &pane_id])
        .output();
    match set {
        Ok(o) if o.status.success() => PaneReloadStatus::Created { target: pane_id },
        Ok(o) => PaneReloadStatus::Failed {
            target: pane_id,
            reason: format!(
                "set-environment failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ),
        },
        Err(e) => PaneReloadStatus::Failed {
            target: pane_id,
            reason: format!("set-environment failed: {e}"),
        },
    }
}

/// `tmux has-session -t <name>` — true if the session is alive on the
/// local tmux server. Reload always runs on the hub (matching the
/// `show_view` convention), so we don't route through `shelbi-ssh`.
fn local_session_exists(session: &str) -> Result<bool> {
    let out = std::process::Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map_err(Error::Io)?;
    Ok(out.status.success())
}

/// Read `SHELBI_PANE_<view>` from the session's tmux environment.
/// Returns `None` if the variable was never set (older sessions
/// pre-dating the stash layout, or a partially-bootstrapped session).
fn read_pane_id(session: &str, view: &str) -> Result<Option<String>> {
    let key = format!("SHELBI_PANE_{view}");
    let out = std::process::Command::new("tmux")
        .args(["show-environment", "-t", session, &key])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    // `-KEY` form means the variable is explicitly unset on this session.
    if line.starts_with('-') {
        return Ok(None);
    }
    let Some((_, value)) = line.split_once('=') else {
        return Ok(None);
    };
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value.to_string()))
    }
}

/// `tmux respawn-pane -k -t <target> sh -c <cmd>` — kill the running
/// process in the pane (`-k`) and start a fresh one. The pane's id is
/// preserved, so any swap-pane references stay valid.
fn respawn_pane(target: &str, cmd: &str) -> PaneReloadStatus {
    let out = std::process::Command::new("tmux")
        .args(["respawn-pane", "-k", "-t", target, "sh", "-c", cmd])
        .output();
    match out {
        Ok(o) if o.status.success() => PaneReloadStatus::Respawned {
            target: target.to_string(),
        },
        Ok(o) => PaneReloadStatus::Failed {
            target: target.to_string(),
            reason: String::from_utf8_lossy(&o.stderr).trim().to_string(),
        },
        Err(e) => PaneReloadStatus::Failed {
            target: target.to_string(),
            reason: e.to_string(),
        },
    }
}

#[cfg(test)]
mod pane_cmd_tests {
    use super::*;

    // These tests lock in the exact `sh -c` strings used for each shelbi-
    // owned pane. Both `ensure_dashboard` and `reload` route through the
    // same builders, so a regression here means the two paths could
    // disagree on what the pane runs.

    #[test]
    fn sidebar_cmd_is_invocation_of_internal_subcommand() {
        let out = sidebar_cmd("/usr/local/bin/shelbi", "myapp");
        assert_eq!(out, "/usr/local/bin/shelbi __sidebar myapp");
    }

    #[test]
    fn tasks_cmd_wraps_in_respawn_loop() {
        let out = tasks_cmd("/usr/local/bin/shelbi", "myapp");
        assert_eq!(
            out,
            "while true; do /usr/local/bin/shelbi __tasks myapp; sleep 1; done"
        );
    }

    #[test]
    fn activity_cmd_wraps_in_respawn_loop() {
        let out = activity_cmd("/usr/local/bin/shelbi", "myapp");
        assert_eq!(
            out,
            "while true; do /usr/local/bin/shelbi __activity myapp; sleep 1; done"
        );
    }

    #[test]
    fn machines_cmd_calls_workspace_list_on_a_loop() {
        let out = machines_cmd("/usr/local/bin/shelbi", "myapp");
        // sanity check: clears the screen each tick, runs `workspace list`,
        // and threads --project through so the inner subcommand picks the
        // right project even though it's invoked through `sh -c`.
        assert!(out.contains("printf '\\033c'"));
        assert!(out.contains("/usr/local/bin/shelbi --project myapp workspace list"));
        assert!(out.contains("sleep 5"));
    }

    #[test]
    fn machines_cmd_neutralizes_a_quote_injection_in_the_project_name() {
        // A hostile/typo'd project name must not break out of the label or
        // inject a command. shell_escape wraps the single quote as
        // `'\''`, so the payload is printed literally, never executed.
        let out = machines_cmd("/usr/local/bin/shelbi", "x'; rm -rf ~; echo '");
        // The payload survives only inside the escaped single-quoted form —
        // there is no unquoted `; rm -rf` that a shell would execute.
        assert!(out.contains(r"'x'\''; rm -rf ~; echo '\'''"));
    }

    #[test]
    fn orchestrator_pane_cmd_wraps_launch_with_lifecycle_hooks() {
        let out = orchestrator_pane_cmd(
            "/usr/local/bin/shelbi",
            "myapp",
            "shelbi-myapp",
            "/Users/me/.shelbi/projects/myapp",
            "claude --print",
        );
        // cd into workdir + export env first. Shell-safe alphanumeric
        // paths skip the quoting branch — see shelbi_agent::shell_escape.
        assert!(out.starts_with("cd /Users/me/.shelbi/projects/myapp && "));
        assert!(out.contains("export SHELBI_PROJECT=myapp SHELBI_TMUX_SESSION=shelbi-myapp"));
        // Crash recovery check runs before the heartbeat loop spawns.
        let start_idx = out
            .find("__zen-orch-start myapp")
            .expect("missing __zen-orch-start");
        let heartbeat_idx = out
            .find("__zen-heartbeat myapp")
            .expect("missing __zen-heartbeat");
        let launch_idx = out.find("claude --print").expect("missing launch");
        let exit_idx = out
            .find("__zen-orch-exit myapp")
            .expect("missing __zen-orch-exit");
        assert!(start_idx < heartbeat_idx, "start must precede heartbeat");
        assert!(
            heartbeat_idx < launch_idx,
            "heartbeat must spawn before launch"
        );
        assert!(launch_idx < exit_idx, "exit must run after launch returns");
        // Heartbeat loop is spawned in the background and killed afterwards.
        assert!(out.contains("HB=$!"), "must capture heartbeat pid");
        assert!(
            out.contains("kill $HB"),
            "must kill heartbeat after launch exits"
        );
        // We deliberately don't exec the launch so the wrapper survives.
        assert!(!out.contains(" exec "), "exec would skip the cleanup hooks");
        // Exit code of the agent is preserved.
        assert!(out.contains("RC=$?") && out.contains("exit $RC"));
    }

    #[test]
    fn orchestrator_pane_cmd_shell_escapes_workdir_with_spaces() {
        // Project workdirs can contain spaces (`~/Documents/Project Name/`)
        // — the wrapper must single-quote the whole `cd` arg, otherwise
        // sh -c would split it into two tokens.
        let out = orchestrator_pane_cmd(
            "/usr/local/bin/shelbi",
            "myapp",
            "shelbi-myapp",
            "/Users/me/My Projects/myapp",
            "claude",
        );
        assert!(out.contains("cd '/Users/me/My Projects/myapp' && "));
    }

    #[test]
    fn reload_expected_shutdown_clears_crash_marker_without_disabling_zen() {
        let _g = crate::test_lock::acquire();
        let home = std::env::temp_dir().join(format!(
            "shelbi-reload-expected-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("anon")
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::write_state(
            "myapp",
            &shelbi_state::State {
                zen_mode: shelbi_state::ZenModeState::On,
                zen_last_crashed_at: Some(chrono::Utc::now()),
                ..shelbi_state::State::default()
            },
        )
        .unwrap();

        mark_orchestrator_reload_expected("myapp").unwrap();

        let state = shelbi_state::read_state("myapp").unwrap();
        assert_eq!(state.zen_mode, shelbi_state::ZenModeState::On);
        assert!(state.zen_last_crashed_at.is_none());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn launch_with_bootstrap_appends_initial_prompt_for_claude() {
        // Cold-start guarantee: the bootstrap prompt is the agent's first
        // user message, so the events.log Monitor watch arms without the
        // user typing "start monitoring".
        let spec = shelbi_core::AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = launch_with_bootstrap(&spec, "myapp", std::path::Path::new("/tmp/myapp"));
        assert!(
            out.starts_with("claude "),
            "launch should start with `claude`, got: {out}"
        );
        // Single-quoted so the whole prompt lands as one positional arg
        // inside the `sh -c "...; {launch}; ..."` wrapper.
        assert!(
            out.contains("'Run the \"Bootstrap on session start\""),
            "missing escaped prompt: {out}"
        );
        assert!(
            out.contains("shelbi events tail --follow"),
            "prompt must name the tail command"
        );
        assert!(
            out.contains("Monitor tool"),
            "prompt must mention the Monitor tool"
        );
        // Orchestrator now sources its system prompt from
        // `agents/orchestrator/instructions.md` (composed with the
        // shared preamble) via `--append-system-prompt`, replacing the
        // pre-task `<workdir>/CLAUDE.md` auto-load that this task
        // (`aw-deprecate-claude-md-…`) sunsets.
        assert!(
            out.contains("--append-system-prompt"),
            "missing --append-system-prompt flag: {out}"
        );
        assert!(
            out.contains("$(cat .claude/agent-instructions.md)"),
            "expected cat-from-relative-path substitution: {out}"
        );
    }

    #[test]
    fn launch_with_bootstrap_preserves_existing_flags() {
        let spec = shelbi_core::AgentRunnerSpec {
            command: "claude".into(),
            flags: vec!["--permission-mode".into(), "auto".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = launch_with_bootstrap(&spec, "myapp", std::path::Path::new("/tmp/myapp"));
        // The runner's own flags must land before `--append-system-prompt`
        // (so `--permission-mode` isn't consumed by the wrong parser) and
        // both must land before the positional bootstrap prompt.
        assert!(
            out.starts_with("claude --permission-mode auto --append-system-prompt"),
            "runner flags must precede --append-system-prompt: {out}"
        );
        let append_idx = out.find("--append-system-prompt").unwrap();
        let prompt_idx = out.find("'Run the").expect("missing bootstrap prompt");
        assert!(
            append_idx < prompt_idx,
            "--append-system-prompt must precede the positional bootstrap prompt: {out}"
        );
    }

    #[test]
    fn launch_with_bootstrap_embeds_orchestrator_context_for_codex() {
        // Codex accepts an initial positional prompt but has no
        // --append-system-prompt surface in Shelbi's runner abstraction.
        // The prompt must therefore carry the rendered orchestrator
        // instructions, project identity, worktree, bootstrap ask, and
        // reload handoff continuity guidance.
        let spec = shelbi_core::AgentRunnerSpec {
            command: "codex".into(),
            flags: vec!["--print".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = launch_with_bootstrap(
            &spec,
            "shelbi",
            std::path::Path::new("/Users/jlong/Workspaces/shelbi"),
        );
        assert!(
            out.starts_with("codex --print "),
            "runner flags must be preserved: {out}"
        );
        assert!(
            out.contains("orchestrator/scheduler for project `shelbi`"),
            "missing Codex role/project identity: {out}"
        );
        assert!(
            out.contains("Project worktree: `/Users/jlong/Workspaces/shelbi`"),
            "missing project worktree: {out}"
        );
        assert!(
            out.contains("$(cat .claude/agent-instructions.md)"),
            "Codex prompt must receive rendered orchestrator instructions: {out}"
        );
        assert!(
            out.contains("handoff `<system-reminder>` block"),
            "Codex startup must call out reload handoff continuity: {out}"
        );
        assert!(
            out.contains("Run the \"Bootstrap on session start\" sequence"),
            "missing bootstrap request: {out}"
        );
        assert!(
            !out.contains("--append-system-prompt"),
            "Codex must not receive Claude-only flags: {out}"
        );
    }

    #[test]
    fn launch_with_bootstrap_skips_unknown_runners() {
        let spec = shelbi_core::AgentRunnerSpec {
            command: "aider".into(),
            flags: vec!["--foo".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = launch_with_bootstrap(&spec, "myapp", std::path::Path::new("/tmp/myapp"));
        assert_eq!(out, "aider --foo");
    }

    #[test]
    fn launch_with_bootstrap_recognizes_absolute_claude_paths() {
        // Same basename rule as with_permission_mode — a project that
        // pins `/opt/homebrew/bin/claude` still gets the auto-bootstrap.
        let spec = shelbi_core::AgentRunnerSpec {
            command: "/opt/homebrew/bin/claude".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = launch_with_bootstrap(&spec, "myapp", std::path::Path::new("/tmp/myapp"));
        assert!(
            out.contains("'Run the \"Bootstrap on session start\""),
            "claude detected by basename: {out}"
        );
    }

    #[test]
    fn launch_with_bootstrap_recognizes_absolute_codex_paths() {
        let spec = shelbi_core::AgentRunnerSpec {
            command: "/opt/homebrew/bin/codex".into(),
            flags: vec![],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let out = launch_with_bootstrap(&spec, "myapp", std::path::Path::new("/tmp/myapp"));
        assert!(
            out.contains("orchestrator/scheduler for project `myapp`"),
            "codex detected by basename: {out}"
        );
    }

    #[test]
    fn cmd_builders_shell_escape_paths_with_spaces() {
        // A binary path with spaces (`/Users/jane doe/.cargo/bin/shelbi`)
        // would tear apart in `sh -c` without quoting.
        let out = sidebar_cmd("/Users/jane doe/.cargo/bin/shelbi", "myapp");
        assert_eq!(out, "'/Users/jane doe/.cargo/bin/shelbi' __sidebar myapp");
    }
}

#[cfg(test)]
mod create_stash_pane_tmux_tests {
    //! Tmux-touching integration tests for the create-missing-pane path.
    //!
    //! Each test spins up two unique-named tmux sessions on the local
    //! server (visible + stash, mirroring `create_hidden_views`), exercises
    //! `create_stash_pane`, then tears the sessions down. Skipped silently
    //! if `tmux` isn't on PATH so the unit-test suite still runs on
    //! tmux-less CI.
    use super::*;

    fn tmux_available() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn kill_session(name: &str) {
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", name])
            .output();
    }

    /// Provision the two sessions reload expects to find: a visible
    /// `<vis>` session with a window, and a stash `_<vis>` session whose
    /// first window is `views` (with one seed pane). Returns the session
    /// name so the caller can pass it to `create_stash_pane` and clean
    /// up afterwards.
    fn provision_sessions(label: &str) -> (String, String) {
        let vis = format!("shelbi-test-{label}-{}", std::process::id());
        let stash = format!("_{vis}");
        // Best-effort cleanup of prior leakage from a crashed test run.
        kill_session(&vis);
        kill_session(&stash);

        let ok = std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &vis,
                "-n",
                "dashboard",
                "sh",
                "-c",
                "sleep 30",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "failed to create visible test session `{vis}`");

        let ok = std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &stash,
                "-n",
                "views",
                "sh",
                "-c",
                "sleep 30",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "failed to create stash test session `{stash}`");

        (vis, stash)
    }

    #[test]
    fn create_stash_pane_allocates_pane_and_sets_session_env() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let (vis, _stash) = provision_sessions("alloc");

        let status = create_stash_pane(&vis, "activity", "sleep 30");
        let pane_id = match &status {
            PaneReloadStatus::Created { target } => target.clone(),
            other => panic!("expected Created, got {other:?}"),
        };
        assert!(
            pane_id.starts_with('%'),
            "expected tmux pane id like `%42`, got `{pane_id}`"
        );

        // Env was pinned to the visible session under the canonical key.
        let env_out = std::process::Command::new("tmux")
            .args(["show-environment", "-t", &vis, "SHELBI_PANE_activity"])
            .output()
            .expect("tmux show-environment");
        assert!(env_out.status.success());
        let env_line = String::from_utf8_lossy(&env_out.stdout).trim().to_string();
        assert_eq!(env_line, format!("SHELBI_PANE_activity={pane_id}"));

        // And read_pane_id (used by reload's respawn branch) finds it now.
        let round_trip = read_pane_id(&vis, "activity").expect("read_pane_id");
        assert_eq!(round_trip.as_deref(), Some(pane_id.as_str()));

        kill_session(&vis);
        kill_session(&format!("_{vis}"));
    }

    #[test]
    fn reload_stash_pane_creates_then_respawns_on_second_call() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let (vis, _stash) = provision_sessions("idem");

        let first = reload_stash_pane(&vis, "activity", "sleep 30");
        let pane_id = match &first {
            PaneReloadStatus::Created { target } => target.clone(),
            other => panic!("expected Created on first call, got {other:?}"),
        };

        // Second call should find the env entry and respawn in-place,
        // preserving the pane id.
        let second = reload_stash_pane(&vis, "activity", "sleep 30");
        match &second {
            PaneReloadStatus::Respawned { target } => {
                assert_eq!(target, &pane_id, "pane id should be reused on respawn");
            }
            other => panic!("expected Respawned on second call, got {other:?}"),
        }

        kill_session(&vis);
        kill_session(&format!("_{vis}"));
    }
}

#[cfg(test)]
mod ensure_hidden_views_tmux_tests {
    //! Tmux-touching tests for the F9 hidden-view heal pass. Each spins up a
    //! visible session (so `set_session_env` has a target), exercises
    //! `ensure_hidden_views` against the local tmux server, and asserts the
    //! stash converges to one live pane per view with env pinned. Skipped
    //! silently when `tmux` isn't on PATH.
    use super::*;
    use shelbi_core::Host;

    fn tmux_available() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn kill_session(name: &str) {
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", name])
            .output();
    }

    fn pane_count(session_win: &str) -> usize {
        let out = std::process::Command::new("tmux")
            .args(["list-panes", "-t", session_win, "-F", "#{pane_id}"])
            .output()
            .expect("tmux list-panes");
        if !out.status.success() {
            return 0;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    }

    fn env_var(session: &str, key: &str) -> Option<String> {
        read_session_env_var(&Host::Local, session, key).unwrap()
    }

    /// Fresh path: no stash session yet. `ensure_hidden_views` must build the
    /// whole stash — three panes, one env var each on the visible session.
    #[test]
    fn builds_stash_from_scratch_when_missing() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let vis = format!("shelbi-test-ehv-fresh-{}", std::process::id());
        let stash = format!("_{vis}");
        kill_session(&vis);
        kill_session(&stash);
        std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &vis,
                "-n",
                "dashboard",
                "sh",
                "-c",
                "sleep 30",
            ])
            .status()
            .unwrap();

        ensure_hidden_views(&Host::Local, &vis, "proj", "shelbi").unwrap();

        assert_eq!(
            pane_count(&format!("{stash}:views")),
            3,
            "expected 3 view panes"
        );
        for view in ["tasks", "machines", "activity"] {
            let id = env_var(&vis, &format!("SHELBI_PANE_{view}"));
            assert!(
                id.is_some(),
                "SHELBI_PANE_{view} should be pinned on the visible session"
            );
            assert!(
                id.unwrap().starts_with('%'),
                "env should hold a tmux pane id"
            );
        }

        kill_session(&vis);
        kill_session(&stash);
    }

    /// Heal path — the F9 core: a dashboard whose panes are present but whose
    /// view stash is only partially built (session exists, no env pinned) is
    /// completed on the next call. Simulates a crash between the split and
    /// stash creation. Also asserts idempotency: a second call is a no-op.
    #[test]
    fn heals_partial_stash_and_is_idempotent() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let vis = format!("shelbi-test-ehv-heal-{}", std::process::id());
        let stash = format!("_{vis}");
        kill_session(&vis);
        kill_session(&stash);
        std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &vis,
                "-n",
                "dashboard",
                "sh",
                "-c",
                "sleep 30",
            ])
            .status()
            .unwrap();
        // Stash exists with a lone seed pane and NO env pinned — the
        // "half-created view stash" the old early-return never healed.
        std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &stash,
                "-n",
                "views",
                "sh",
                "-c",
                "sleep 30",
            ])
            .status()
            .unwrap();
        assert_eq!(pane_count(&format!("{stash}:views")), 1);

        ensure_hidden_views(&Host::Local, &vis, "proj", "shelbi").unwrap();

        // Seed pane + one fresh pane per view = 4, all three views pinned to a
        // pane that's actually alive in the stash.
        assert_eq!(
            pane_count(&format!("{stash}:views")),
            4,
            "one pane spliced per missing view"
        );
        let live = live_stash_pane_ids(&Host::Local, &stash).unwrap();
        for view in ["tasks", "machines", "activity"] {
            let id = env_var(&vis, &format!("SHELBI_PANE_{view}"))
                .unwrap_or_else(|| panic!("SHELBI_PANE_{view} unset after heal"));
            assert!(
                live.contains(&id),
                "pinned {view} pane {id} must be alive in the stash"
            );
        }

        // Second call: every view is now healthy, so nothing new is spliced.
        ensure_hidden_views(&Host::Local, &vis, "proj", "shelbi").unwrap();
        assert_eq!(
            pane_count(&format!("{stash}:views")),
            4,
            "heal must be idempotent"
        );

        kill_session(&vis);
        kill_session(&stash);
    }

    /// A dead env reference (view pinned to a pane that no longer exists) is
    /// re-created, not left dangling.
    #[test]
    fn heals_view_pinned_to_a_dead_pane() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let vis = format!("shelbi-test-ehv-dead-{}", std::process::id());
        let stash = format!("_{vis}");
        kill_session(&vis);
        kill_session(&stash);
        std::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &vis,
                "-n",
                "dashboard",
                "sh",
                "-c",
                "sleep 30",
            ])
            .status()
            .unwrap();

        // Build a full stash first.
        ensure_hidden_views(&Host::Local, &vis, "proj", "shelbi").unwrap();
        assert_eq!(pane_count(&format!("{stash}:views")), 3);

        // Point `tasks` at a bogus, non-existent pane id.
        std::process::Command::new("tmux")
            .args(["set-environment", "-t", &vis, "SHELBI_PANE_tasks", "%99999"])
            .status()
            .unwrap();

        ensure_hidden_views(&Host::Local, &vis, "proj", "shelbi").unwrap();

        // `tasks` was re-created (4 panes now), the other two untouched.
        assert_eq!(
            pane_count(&format!("{stash}:views")),
            4,
            "dead tasks pane re-created"
        );
        let live = live_stash_pane_ids(&Host::Local, &stash).unwrap();
        let tasks_id = env_var(&vis, "SHELBI_PANE_tasks").unwrap();
        assert_ne!(tasks_id, "%99999", "stale id must be replaced");
        assert!(live.contains(&tasks_id), "healed tasks pane must be alive");

        kill_session(&vis);
        kill_session(&stash);
    }
}
