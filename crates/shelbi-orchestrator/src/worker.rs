//! Worker lifecycle: the pre-declared agent slots that pick up Kanban
//! tasks. See [`crate::ensure_dashboard`] for the project's overall tmux
//! layout; this module is concerned only with the per-worker slot.
//!
//! Each worker owns a stable worktree at
//! `<machine.work_dir>/.shelbi/wt/<worker-name>`. The worktree persists
//! across tasks; the worker switches branches between assignments. The
//! worker's tmux pane (window for local hub workers, session for remote
//! workers) is killed and re-created on every assignment to clear the
//! agent's context — that's the user-specified semantics.
//!
//! Reviewer hint: this module does no state writes to task files; the
//! caller (CLI) is responsible for updating `assigned_to` / `branch` /
//! `column`. We just stand up the worktree + tmux pane + claude.

use std::path::{Path, PathBuf};

use shelbi_core::{Error, Host, Machine, Project, Result, TmuxAddr, WorkerSpec};

/// Where a worker's pane lives in tmux. Local workers get a window in the
/// project session; remote workers get their own session (so they survive
/// SSH drops).
pub fn worker_tmux_addr(project: &Project, worker: &WorkerSpec) -> Result<TmuxAddr> {
    let machine = project
        .machine(&worker.machine)
        .ok_or_else(|| Error::UnknownMachine(worker.machine.clone()))?;
    Ok(match machine.host() {
        Host::Local => TmuxAddr {
            session: format!("shelbi-{}", project.name),
            window: worker.name.clone(),
        },
        Host::Ssh { .. } => TmuxAddr {
            session: format!("shelbi-w-{}", worker.name),
            window: "agent".into(),
        },
    })
}

/// `<machine.work_dir>/.shelbi/wt/<worker-name>` — the worker's persistent
/// worktree path on its machine.
pub fn worker_worktree(machine: &Machine, worker: &WorkerSpec) -> PathBuf {
    machine.work_dir.join(".shelbi").join("wt").join(&worker.name)
}

/// The review-ready file marker for a worker:
/// `<worktree>/.claude/shelbi-review-ready`.
///
/// The worker writes its current task id here to hand off for review; the
/// hub poller reads it (`stat`/`cat`, local or over SSH), moves the task to
/// the review column, and clears the file. This replaces the old
/// pane-title / `shelbi task move` handoff, both of which raced Claude's own
/// OSC title writes and the Stop hook. A file survives both: nothing the
/// agent's UI does can clobber it.
///
/// It lives under `.claude/` (not the worktree root) on purpose — `.claude/`
/// is where shelbi already deploys `settings.json`, and shelbi relies on it
/// being gitignored so deployed files don't dirty the worktree between
/// tasks. Keeping the marker there means it never shows up in
/// `git status --porcelain` and so never trips [`sync_worktree`]'s
/// clean-worktree check.
pub fn worker_review_marker(machine: &Machine, worker: &WorkerSpec) -> PathBuf {
    worker_worktree(machine, worker)
        .join(".claude")
        .join("shelbi-review-ready")
}

/// Read the review-ready marker, returning the task id the worker wrote into
/// it (trimmed) or `None` if the marker is absent or empty. Works for both
/// local and remote workers — `cat` is routed through `shelbi-ssh`, which is
/// a no-op wrapper for [`Host::Local`].
pub fn read_review_marker(host: &Host, marker: &Path) -> Result<Option<String>> {
    let path = marker.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["cat", path.as_str()]).map_err(Error::Io)?;
    if !out.status.success() {
        // Missing file → cat exits non-zero. Not an error for us: the
        // worker simply hasn't signalled review yet.
        return Ok(None);
    }
    let content = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!content.is_empty()).then_some(content))
}

/// Remove the review-ready marker (idempotent — `rm -f` succeeds if absent).
/// Called once the poller has consumed the signal, and again at task start to
/// clear any stale marker before the worktree is reused.
pub fn clear_review_marker(host: &Host, marker: &Path) -> Result<()> {
    let path = marker.to_string_lossy().into_owned();
    shelbi_ssh::run(host, ["rm", "-f", path.as_str()]).map_err(Error::Io)?;
    Ok(())
}

/// Does the worker have a live tmux pane right now?
pub fn worker_pane_alive(host: &Host, addr: &TmuxAddr) -> Result<bool> {
    // Local: check `session:window` exists. Remote: it's a whole session.
    // `tmux list-windows -t session -F #W | grep -w window` does both.
    let out = shelbi_ssh::run(
        host,
        ["tmux", "list-windows", "-t", &addr.session, "-F", "#W"],
    )
    .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|w| w.trim() == addr.window))
}

/// Kill the worker's pane (idempotent — silently OK if already gone).
pub fn kill_worker_pane(host: &Host, addr: &TmuxAddr) -> Result<()> {
    // Local: `kill-window -t session:window` (the dashboard session
    // must stay alive). Remote: `kill-session -t session` (the session
    // IS the worker).
    //
    // The liveness check has to differ too. For local we look for the
    // worker's window inside the shared dashboard session. For remote
    // we look for the session itself — NOT for a window named `agent`
    // — because tmux's `automatic-rename` (on by default) renames the
    // window after whatever command is running (`claude`, `bash`, …),
    // and a window-name match would miss live sessions and leave them
    // around to collide with the next `task start`.
    match host {
        Host::Local => {
            if !worker_pane_alive(host, addr)? {
                return Ok(());
            }
            let _ = shelbi_ssh::run(host, ["tmux", "kill-window", "-t", &addr.target()])
                .map_err(Error::Io)?;
        }
        Host::Ssh { .. } => {
            if !shelbi_tmux::has_session(host, &addr.session)? {
                return Ok(());
            }
            let _ = shelbi_ssh::run(host, ["tmux", "kill-session", "-t", &addr.session])
                .map_err(Error::Io)?;
        }
    }
    Ok(())
}

/// Spec for `start_worker_on_task`. We don't take a `&Task` because the
/// caller may have a fresh task id without a frontmatter file yet.
pub struct StartSpec<'a> {
    pub project: &'a Project,
    pub worker: &'a WorkerSpec,
    pub task_id: &'a str,
    pub branch: &'a str,
    /// Body of the task markdown — appended to the prompt as context.
    pub task_body: &'a str,
}

/// Tear down the worker's pane, switch its worktree to `branch` (creating
/// the worktree off `default_branch` and the branch off `default_branch` if
/// needed), and start the runner with an initial prompt. Bails on a dirty
/// worktree so the user doesn't silently lose work.
pub fn start_worker_on_task(spec: StartSpec<'_>) -> Result<TmuxAddr> {
    let machine = spec
        .project
        .machine(&spec.worker.machine)
        .ok_or_else(|| Error::UnknownMachine(spec.worker.machine.clone()))?
        .clone();
    let runner = spec
        .project
        .runner(&spec.worker.runner)
        .ok_or_else(|| Error::UnknownRunner(spec.worker.runner.clone()))?
        .clone();

    let host = machine.host();
    let worktree = worker_worktree(&machine, spec.worker);
    let addr = worker_tmux_addr(spec.project, spec.worker)?;

    // 0a. If the project asks for auto-mode, claude must be v2.1.83+. Older
    //     versions silently fall back to `default` and the user gets a Bash
    //     prompt on every command — exactly the bug we're trying to avoid.
    //     Surface it up front so the failure mode is "shelbi rejected this
    //     machine" instead of "my worker keeps pausing for no reason."
    require_auto_mode_supported(&host, &runner, &spec.project.worker_permissions_mode)?;

    // 0b. Clear any stale review marker left in the worktree from a previous
    //     task before we reuse the worktree — otherwise the poller could read
    //     an old task id and misfire. Best-effort: a failure here shouldn't
    //     block standing up the worker.
    let marker = worker_review_marker(&machine, spec.worker);
    let _ = clear_review_marker(&host, &marker);

    // 1. Make sure the worktree exists + is on the right branch, clean.
    sync_worktree(
        &host,
        &machine,
        &worktree,
        spec.branch,
        &spec.project.default_branch,
    )?;

    // 2. Drop a rendered .claude/settings.json into the worktree so the
    //    runner picks up shelbi's window-title hooks (idle/working/blocked).
    //    Overwrite is fine — this is the entire on-worker footprint and we
    //    re-render it on every task start.
    let rendered = shelbi_state::render_worker_settings(spec.project)?;
    deploy_worker_settings(&host, &worktree, &rendered)?;

    // 3. Reset the tmux pane — that's how we clear context. If it doesn't
    //    exist yet, this is a no-op; otherwise the next step recreates it.
    kill_worker_pane(&host, &addr)?;

    // 4. Create the pane. Start with an interactive shell (no `-c <cmd>`)
    //    so the user's rc files run and the pane outlives the agent
    //    process. Local = window in the project session; remote = its own
    //    session so the worker survives an SSH drop.
    match &host {
        Host::Local => {
            if !shelbi_tmux::has_session(&host, &addr.session)? {
                shelbi_tmux::new_session(&host, &addr.session, &addr.window, None)?;
            } else {
                shelbi_tmux::new_window(&host, &addr.session, &addr.window, None)?;
            }
        }
        Host::Ssh { .. } => {
            shelbi_tmux::new_session(&host, &addr.session, &addr.window, None)?;
        }
    }

    // 5. cd into the worktree and launch the agent.
    //
    //    Local: tmux server inherits the user's already-set-up login env
    //    (since the user ran shelbi from their own terminal), so a plain
    //    invocation finds everything on PATH. No `exec` — when the agent
    //    exits, the shell stays so the worker pane is reusable.
    //
    //    Remote: tmux was started by `ssh host -- tmux new-session …`,
    //    which runs through a NON-login non-interactive shell — so tmux
    //    (and every pane it spawns) inherits a stripped-down PATH that's
    //    missing Homebrew, asdf, nvm, etc. Re-exec through `$SHELL -lc`
    //    so the login rc files (~/.zprofile, ~/.bash_profile) run and we
    //    pick up the same PATH the user has in their own terminal —
    //    otherwise claude launches without its expected env and dies with
    //    "Input must be provided either through stdin or as a prompt
    //    argument when using --print".
    //
    //    `LANG=C.UTF-8` is cheap, low-risk insurance: a non-interactive
    //    SSH launch can leave the tmux server in the C locale, and forcing
    //    UTF-8 keeps every box-drawing/glyph path well-defined regardless
    //    of host config.
    // Inject `--permission-mode <mode>` directly on the claude command line
    // rather than trusting the rendered `.claude/settings.json` to take effect.
    // Settings-based mode is fragile (silent fallback to interactive on any
    // I/O race or version regression) — the CLI flag is authoritative and
    // belongs to the spawn path, where we already know the project's mode.
    let runner_with_mode =
        shelbi_agent::with_permission_mode(&runner, &spec.project.worker_permissions_mode);
    let launch = shelbi_agent::launch_command(&runner_with_mode);
    let cd_launch = if host.is_local() {
        format!(
            "cd {wd} && LANG=C.UTF-8 {launch}",
            wd = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
        )
    } else {
        format!(
            "cd {wd} && LANG=C.UTF-8 exec \"${{SHELL:-/bin/bash}}\" -lc {launch}",
            wd = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
            launch = shelbi_agent::shell_escape(&launch),
        )
    };
    shelbi_tmux::send_line(&host, &addr, &cd_launch)?;

    // 6. Wait until claude has drawn its input box before typing the prompt.
    //    A fixed sleep is fragile: claude's boot time varies with load, and
    //    on a freshly-created worktree it may interpose a "trust this folder"
    //    dialog first (which `wait_for_claude_ready` auto-confirms). If the
    //    probe times out we still send — best effort beats aborting the task.
    let ready = wait_for_claude_ready(&host, &addr, READY_TIMEOUT)?;
    if !ready {
        eprintln!(
            "shelbi: claude readiness probe timed out after {}s on {}; \
             sending the prompt anyway",
            READY_TIMEOUT.as_secs(),
            addr.target(),
        );
    }
    let prompt = compose_prompt(spec.task_id, spec.branch, spec.task_body, &marker);
    shelbi_tmux::send_line(&host, &addr, &prompt)?;

    // 7. Verify the prompt actually got submitted, not just typed into the
    //    input box. claude's `UserPromptSubmit` hook (see worker settings
    //    template) writes `\033]2;shelbi:working\007` to the pane title on
    //    every submit — so once any `shelbi:` marker appears in the title,
    //    we know Enter landed. If it doesn't within a short window, the
    //    most common cause is that the trailing Enter raced claude's input
    //    focus and was dropped; resend it once and try again. If still no
    //    marker, surface a dispatch=stalled line in events.log so the
    //    orchestrator (and `shelbi events tail`) sees it instead of the
    //    worker silently sitting on the prompt.
    confirm_prompt_submitted(&host, &addr, spec.task_id, &spec.worker.name);

    Ok(addr)
}

/// How long to wait for the `UserPromptSubmit` hook to flip the pane title
/// after sending the initial prompt. Hook firing is a single printf, so a
/// well-behaved submit lands almost immediately; this just covers the slow
/// path (busy SSH, sluggish tmux server).
const PROMPT_SUBMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How often to re-check the pane title while waiting for the submit signal.
const PROMPT_SUBMIT_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// Wait for the prompt-submitted signal; if it doesn't arrive, resend Enter
/// once and wait again; if it still doesn't arrive, log a dispatch=stalled
/// event and warn on stderr. Best-effort — failures here don't abort the
/// task start (the worker may still recover), they just surface the stall
/// so the orchestrator stops assuming the dispatch succeeded.
fn confirm_prompt_submitted(host: &Host, addr: &TmuxAddr, task_id: &str, worker: &str) {
    if wait_for_prompt_submitted(host, addr, PROMPT_SUBMIT_WAIT) {
        return;
    }
    // First Enter likely raced claude's focus — resend a bare Enter and give
    // the hook one more window to fire. Avoid spamming Enters: a second one
    // after the prompt is already processed would submit an empty message,
    // which claude ignores, but more than that starts to look like noise.
    if let Err(e) = shelbi_tmux::send_enter(host, addr) {
        eprintln!(
            "shelbi: retry Enter to {} after stalled dispatch failed: {e}",
            addr.target(),
        );
    }
    if wait_for_prompt_submitted(host, addr, PROMPT_SUBMIT_WAIT) {
        return;
    }
    eprintln!(
        "shelbi: dispatched prompt to {} but no shelbi:* pane title appeared \
         after a retry Enter — worker may be sitting on an unsubmitted prompt; \
         check the pane",
        addr.target(),
    );
    if let Err(e) = shelbi_state::append_dispatch_event(
        task_id,
        worker,
        "enter-stalled",
        "no shelbi marker after retry",
    ) {
        eprintln!("shelbi: failed to record dispatch stall in events.log: {e}");
    }
}

/// Poll the pane title until the `UserPromptSubmit` hook has fired (any
/// `shelbi:*` marker is present), or `timeout` elapses. Capture failures
/// during the poll are transient (the SSH socket can hiccup); we just
/// ignore them and keep polling.
fn wait_for_prompt_submitted(host: &Host, addr: &TmuxAddr, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let title = shelbi_tmux::pane_title(host, addr).unwrap_or_default();
        if shelbi_state::parse_pane_title_marker(&title).is_some() {
            return true;
        }
        std::thread::sleep(PROMPT_SUBMIT_POLL);
    }
    false
}

/// The minimum claude version that understands `--permission-mode auto`.
/// Older versions either silently fall back to `default` or reject the flag,
/// and the worker pauses on every Bash prompt.
const CLAUDE_AUTO_MODE_MIN: (u32, u32, u32) = (2, 1, 83);

/// If the project wants auto-mode and the runner is claude, ensure the
/// worker host's claude is new enough to understand it. Quiet pass-through
/// when the probe fails for unrelated reasons (claude missing from PATH,
/// weird output) — `wait_for_claude_ready` will surface a launch failure
/// downstream with a clearer signal than "version probe failed."
fn require_auto_mode_supported(
    host: &Host,
    runner: &shelbi_core::AgentRunnerSpec,
    mode: &str,
) -> Result<()> {
    if mode != "auto" {
        return Ok(());
    }
    // Only the `claude` CLI accepts `--permission-mode`; other runners
    // (codex etc.) reject the flag, so the version probe is meaningless
    // for them.
    if std::path::Path::new(&runner.command).file_name().and_then(|s| s.to_str()) != Some("claude") {
        return Ok(());
    }
    let Some(version) = probe_claude_version(host) else {
        eprintln!(
            "shelbi: couldn't read `claude --version` on {host:?}; \
             skipping auto-mode compatibility check (claude {}+ required)",
            format_version(CLAUDE_AUTO_MODE_MIN),
        );
        return Ok(());
    };
    if version < CLAUDE_AUTO_MODE_MIN {
        return Err(Error::Other(format!(
            "claude {} on this worker is too old for worker_permissions_mode: auto \
             (need {}+, classifier-based auto-approval). Either upgrade claude on the \
             worker host, or set `worker_permissions_mode` in this project's config to \
             `acceptEdits` (auto-accept edits but still gate Bash) or `bypassPermissions` \
             (no seatbelt — auto-accept everything).",
            format_version(version),
            format_version(CLAUDE_AUTO_MODE_MIN),
        )));
    }
    Ok(())
}

/// Run `claude --version` on `host` and parse `(major, minor, patch)` from
/// its stdout. Returns `None` on any failure — caller decides how to react.
///
/// Local: shelbi's own PATH (inherited from the user's terminal) already
/// has claude. Remote: ssh's default non-login shell strips Homebrew /
/// nvm / asdf off PATH, so we re-exec through `$SHELL -lc` to source the
/// user's login rc — same trick we use to launch the agent itself.
fn probe_claude_version(host: &Host) -> Option<(u32, u32, u32)> {
    let out = match host {
        Host::Local => shelbi_ssh::run(host, ["claude", "--version"]).ok()?,
        Host::Ssh { .. } => {
            shelbi_ssh::run(host, ["$SHELL", "-lc", "'claude --version'"]).ok()?
        }
    };
    if !out.status.success() {
        return None;
    }
    parse_claude_version(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `2.1.83 (Claude Code)` (or similar) into `(2, 1, 83)`.
fn parse_claude_version(s: &str) -> Option<(u32, u32, u32)> {
    let token = s.split_whitespace().next()?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn format_version((maj, min, pat): (u32, u32, u32)) -> String {
    format!("{maj}.{min}.{pat}")
}

/// How long to wait for claude's input box to appear before giving up and
/// sending the prompt anyway.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How often to re-capture the pane while waiting for readiness.
const READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Poll the worker pane until claude's input box is on screen and ready to
/// accept the initial prompt. Returns `Ok(true)` once ready, `Ok(false)` on
/// timeout (the caller sends anyway).
///
/// ## Why this exists / what the bug actually was
///
/// The original code slept a fixed 1500ms then typed. That fails on a
/// fresh devbox worker for a reason that is *not* terminal encoding:
/// investigation on a Linux worker showed claude emits the `❯` prompt glyph
/// (`e2 9d af`) under both `en_US.UTF-8` and the bare `C` locale, and
/// `tmux capture-pane` preserves those bytes intact. So a single-glyph probe
/// matches fine on Linux — encoding is a red herring.
///
/// The real fragility is twofold:
///  1. `❯` is *ambiguous*: it is also the menu cursor in claude's modal
///     dialogs (`❯ 1. Yes, I trust this folder`), so a probe keyed on the
///     glyph alone can fire on a dialog instead of the input box.
///  2. On first entry to an untrusted directory tree claude shows a "trust
///     this folder" dialog and waits. The hub rarely sees it (its work_dir
///     tree is already trusted), but a fresh devbox does — and a fixed sleep
///     types the task body straight into that dialog, where the first Enter
///     just confirms trust and the prompt is lost.
///
/// So we (a) auto-confirm the trust dialog (shelbi owns these worktrees, so
/// trusting them is implied by the assignment) and (b) key readiness on
/// signals unique to the *input box*, never present in a modal menu.
fn wait_for_claude_ready(
    host: &Host,
    addr: &TmuxAddr,
    timeout: std::time::Duration,
) -> Result<bool> {
    let start = std::time::Instant::now();
    let mut trust_dismissed = false;
    while start.elapsed() < timeout {
        // A capture failure here is transient (pane still spinning up); keep
        // polling rather than aborting the whole task start.
        let screen = shelbi_tmux::capture(host, addr).unwrap_or_default();
        if is_input_ready(&screen) {
            return Ok(true);
        }
        if !trust_dismissed && is_trust_dialog(&screen) {
            shelbi_tmux::send_enter(host, addr)?;
            trust_dismissed = true;
        }
        std::thread::sleep(READY_POLL_INTERVAL);
    }
    Ok(false)
}

/// True when the captured pane shows claude's input box ready for typing.
///
/// We match the footer/status line that claude renders *only* once the input
/// box is live — `shift+tab to cycle` is present in every permission mode,
/// and the others cover mode/version wording drift. None of these strings
/// appear in claude's modal dialogs, so this won't fire on the trust prompt.
fn is_input_ready(screen: &str) -> bool {
    const READY_MARKERS: &[&str] = &[
        "shift+tab to cycle", // permission-mode footer (all modes)
        "for shortcuts",      // "? for shortcuts" footer (plain mode)
        "auto mode on",
        "accept edits on",
        "plan mode on",
    ];
    READY_MARKERS.iter().any(|m| screen.contains(m))
}

/// True when the captured pane shows claude's "trust this folder" dialog.
fn is_trust_dialog(screen: &str) -> bool {
    let s = screen.to_ascii_lowercase();
    s.contains("trust this folder") || s.contains("do you trust")
}

/// Write the rendered worker `settings.json` to `<worktree>/.claude/` on
/// `host`. Local hosts get a direct filesystem write; remote hosts get an
/// `ssh mkdir -p` followed by `scp` of the rendered file. The worker
/// machine never executes any shelbi code — this file is the whole
/// on-worker footprint.
pub fn deploy_worker_settings(
    host: &Host,
    worktree: &std::path::Path,
    rendered: &str,
) -> Result<()> {
    let claude_dir = worktree.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    match host {
        Host::Local => {
            std::fs::create_dir_all(&claude_dir).map_err(Error::Io)?;
            std::fs::write(&settings_path, rendered).map_err(Error::Io)?;
            Ok(())
        }
        Host::Ssh { host: ssh_host } => scp_settings_to_remote(
            ssh_host,
            &claude_dir.to_string_lossy(),
            &settings_path.to_string_lossy(),
            rendered,
        ),
    }
}

fn scp_settings_to_remote(
    ssh_host: &str,
    remote_dir: &str,
    remote_path: &str,
    rendered: &str,
) -> Result<()> {
    // 1. Ensure the .claude/ dir exists on the remote.
    let mkdir = shelbi_ssh::run(
        &Host::Ssh { host: ssh_host.to_string() },
        ["mkdir", "-p", remote_dir],
    )
    .map_err(Error::Io)?;
    if !mkdir.status.success() {
        return Err(Error::Command {
            cmd: format!("ssh {ssh_host} mkdir -p {remote_dir}"),
            status: mkdir.status.to_string(),
            stderr: String::from_utf8_lossy(&mkdir.stderr).into_owned(),
        });
    }

    // 2. Stage the rendered template in a local tempfile, then scp it. The
    //    tempfile is in $TMPDIR so the local FS handles cleanup if we crash
    //    before unlinking it.
    let tmp_path = std::env::temp_dir().join(format!(
        "shelbi-worker-settings-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&tmp_path, rendered).map_err(Error::Io)?;

    let dest = format!("{ssh_host}:{remote_path}");
    let mut cmd = std::process::Command::new("scp");
    // -q quiets scp's progress chatter; -B disables interactive prompts
    // (we expect keys via ssh-agent).
    cmd.arg("-q").arg("-B").arg(&tmp_path).arg(&dest);
    let out = cmd.output().map_err(Error::Io)?;
    let _ = std::fs::remove_file(&tmp_path);
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("scp {} {dest}", tmp_path.display()),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Build the initial prompt: the task body + the loop-closing instruction
/// that tells the worker how to mark itself done.
///
/// The handoff is a file marker, not a pane title or a `shelbi` CLI call.
/// The worker writes its task id into `<worktree>/.claude/shelbi-review-ready`
/// (see [`worker_review_marker`]); the hub poller picks it up and moves the
/// task to the review column. This survives Claude's own OSC pane-title
/// writes and the Stop hook, both of which used to clobber a `shelbi:review`
/// title before the poller could read it, and it needs no `shelbi` binary on
/// the worker host.
fn compose_prompt(task_id: &str, branch: &str, body: &str, marker: &Path) -> String {
    let trimmed = body.trim();
    let body_section = if trimmed.is_empty() {
        format!("# Task {task_id}\n")
    } else {
        trimmed.to_string()
    };
    let id_esc = shelbi_agent::shell_escape(task_id);
    let marker_esc = shelbi_agent::shell_escape(&marker.to_string_lossy());
    format!(
        "{body_section}\n\n\
         ---\n\
         You are working on task `{task_id}` on branch `{branch}`. When \
         the work is complete and committed, signal that it's ready for \
         review by writing the task id to the review marker file:\n\
         \n\
         printf '%s\\n' {id_esc} > {marker_esc}\n\
         \n\
         The hub watches for this file and moves your task to the review \
         column on its next poll. Write the marker once; you can keep \
         working in this pane and talk to the user afterward without \
         affecting the handoff."
    )
}

/// Ensure the worktree exists and is checked out on `branch`. Creates the
/// worktree off the project's default branch if absent, creates the branch
/// off the default if it doesn't exist yet, and bails if the worktree has
/// uncommitted changes (otherwise switching branches would lose work).
fn sync_worktree(
    host: &Host,
    machine: &Machine,
    worktree: &std::path::Path,
    branch: &str,
    default_branch: &str,
) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let wt_str = worktree.to_string_lossy().into_owned();

    let worktree_exists = shelbi_ssh::run(
        host,
        ["test", "-d", &format!("{wt_str}/.git")],
    )
    .map_err(Error::Io)?
    .status
    .success()
        || shelbi_ssh::run(host, ["test", "-f", &format!("{wt_str}/.git")])
            .map_err(Error::Io)?
            .status
            .success();

    let branch_exists = shelbi_ssh::run(
        host,
        ["git", "-C", &repo, "rev-parse", "--verify", branch],
    )
    .map_err(Error::Io)?
    .status
    .success();

    if !worktree_exists {
        // Fresh worktree off the requested branch (or off the default if
        // the branch is also new).
        let mut argv: Vec<String> = vec![
            "git".into(),
            "-C".into(),
            repo.clone(),
            "worktree".into(),
            "add".into(),
        ];
        if branch_exists {
            argv.push(wt_str.clone());
            argv.push(branch.into());
        } else {
            argv.push("-b".into());
            argv.push(branch.into());
            argv.push(wt_str.clone());
            argv.push(default_branch.into());
        }
        let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: argv.join(" "),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        return Ok(());
    }

    // Already exists — make sure it's clean and on the right branch.
    let dirty = shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        return Err(Error::Other(format!(
            "worker worktree at {wt_str} has uncommitted changes — \
             commit, stash, or discard before assigning a new task:\n{dirty}"
        )));
    }

    let current = shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
    )?;
    if current.trim() == branch {
        return Ok(());
    }

    // Switch (and create the branch off default if it doesn't exist).
    let mut argv: Vec<String> = vec!["git".into(), "-C".into(), wt_str.clone(), "checkout".into()];
    if !branch_exists {
        argv.push("-b".into());
        argv.push(branch.into());
        argv.push(default_branch.into());
    } else {
        argv.push(branch.into());
    }
    let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: argv.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{AgentRunnerSpec, MachineKind, OrchestratorSpec};
    use std::collections::BTreeMap;

    fn fixture_project() -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
            },
        );
        Project {
            name: "myapp".into(),
            repo: "git@example:repo.git".into(),
            default_branch: "main".into(),
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/myapp".into(),
                    host: None,
                },
                Machine {
                    name: "m2".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/myapp".into(),
                    host: Some("m2.local".into()),
                },
            ],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: vec![
                WorkerSpec {
                    name: "alice".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                },
                WorkerSpec {
                    name: "bob".into(),
                    machine: "m2".into(),
                    runner: "claude".into(),
                },
            ],
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
        }
    }

    #[test]
    fn local_worker_lives_in_project_session_window() {
        let p = fixture_project();
        let addr = worker_tmux_addr(&p, &p.workers[0]).unwrap();
        assert_eq!(addr.session, "shelbi-myapp");
        assert_eq!(addr.window, "alice");
    }

    #[test]
    fn remote_worker_gets_its_own_session() {
        let p = fixture_project();
        let addr = worker_tmux_addr(&p, &p.workers[1]).unwrap();
        assert_eq!(addr.session, "shelbi-w-bob");
        assert_eq!(addr.window, "agent");
    }

    #[test]
    fn worktree_path_under_machine_workdir() {
        let p = fixture_project();
        let wt = worker_worktree(&p.machines[0], &p.workers[0]);
        assert_eq!(wt, PathBuf::from("/tmp/myapp/.shelbi/wt/alice"));
    }

    #[test]
    fn prompt_includes_task_id_branch_and_review_marker_instruction() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
        );
        assert!(prompt.contains("Fix the Safari SSO bug."));
        assert!(prompt.contains("fix-login"));
        assert!(prompt.contains("shelbi/fix-login"));
        // Hands off via the file marker, not the old pane-title / CLI path.
        assert!(prompt.contains(".claude/shelbi-review-ready"));
        assert!(prompt.contains("printf"));
        assert!(!prompt.contains("shelbi task move"));
        assert!(prompt.contains("\n---\n"));
    }

    #[test]
    fn prompt_falls_back_to_task_id_heading_when_body_empty() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
        let prompt = compose_prompt("fix-login", "shelbi/fix-login", "   ", &marker);
        assert!(prompt.contains("# Task fix-login"));
        assert!(prompt.contains(".claude/shelbi-review-ready"));
    }

    // Real captures observed on a Linux (delta) worker, used to pin the
    // readiness/trust detection against claude's actual rendered output.
    const TRUST_DIALOG_SCREEN: &str = "\
 Do you trust the files in this folder?

 /work/myapp/.shelbi/wt/bob

 Claude Code may read, edit, and execute files here.

 ❯ 1. Yes, I trust this folder
   2. No, exit

 Enter to confirm · Esc to cancel";

    const INPUT_BOX_SCREEN: &str = "\
╭─── Claude Code v2.1.183 ──────────────────────────╮
│            Welcome back John!                      │
╰───────────────────────────────────────────────────╯

────────────────────────────────────────────────────
❯ Try \"edit <filepath> to...\"
────────────────────────────────────────────────────
  ⏵⏵ accept edits on (shift+tab to cycle) · ← for agents";

    #[test]
    fn input_ready_detects_live_input_box_not_trust_dialog() {
        assert!(is_input_ready(INPUT_BOX_SCREEN));
        // The trust dialog also contains `❯`, but must NOT read as ready.
        assert!(!is_input_ready(TRUST_DIALOG_SCREEN));
        // A bare shell prompt before claude has drawn anything.
        assert!(!is_input_ready("➜  bob git:(main) claude"));
        assert!(!is_input_ready(""));
    }

    #[test]
    fn input_ready_matches_each_permission_mode_footer() {
        assert!(is_input_ready("⏵⏵ auto mode on (shift+tab to cycle)"));
        assert!(is_input_ready("⏸ plan mode on (shift+tab to cycle)"));
        assert!(is_input_ready("? for shortcuts"));
    }

    #[test]
    fn trust_dialog_detected_case_insensitively() {
        assert!(is_trust_dialog(TRUST_DIALOG_SCREEN));
        assert!(is_trust_dialog("DO YOU TRUST the files in this folder?"));
        // The live input box is not a trust dialog.
        assert!(!is_trust_dialog(INPUT_BOX_SCREEN));
    }

    #[test]
    fn review_marker_lives_under_gitignored_claude_dir() {
        let p = fixture_project();
        let marker = worker_review_marker(&p.machines[0], &p.workers[0]);
        assert_eq!(
            marker,
            PathBuf::from("/tmp/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready")
        );
    }

    #[test]
    fn parses_typical_claude_version_output() {
        assert_eq!(parse_claude_version("2.1.83 (Claude Code)\n"), Some((2, 1, 83)));
        assert_eq!(parse_claude_version("2.1.153 (Claude Code)"), Some((2, 1, 153)));
        assert_eq!(parse_claude_version("10.0.0\n"), Some((10, 0, 0)));
    }

    #[test]
    fn rejects_unparseable_version_output() {
        // Empty, garbage, missing patch — never block startup on a parse
        // failure; the caller falls back to a warning + proceed.
        assert_eq!(parse_claude_version(""), None);
        assert_eq!(parse_claude_version("not a version\n"), None);
        assert_eq!(parse_claude_version("2.1\n"), None);
        assert_eq!(parse_claude_version("2.x.83\n"), None);
    }

    #[test]
    fn auto_mode_min_orders_correctly() {
        // Tuple comparison is the whole point of the check — verify it
        // behaves the way the require_… code assumes.
        assert!((2, 1, 83) >= CLAUDE_AUTO_MODE_MIN);
        assert!((2, 1, 153) >= CLAUDE_AUTO_MODE_MIN);
        assert!((2, 2, 0) >= CLAUDE_AUTO_MODE_MIN);
        assert!((3, 0, 0) >= CLAUDE_AUTO_MODE_MIN);
        assert!((2, 1, 82) < CLAUDE_AUTO_MODE_MIN);
        assert!((2, 0, 100) < CLAUDE_AUTO_MODE_MIN);
        assert!((1, 9, 9) < CLAUDE_AUTO_MODE_MIN);
    }

    #[test]
    fn require_auto_mode_no_op_for_non_auto_modes() {
        // Skip the probe entirely if the user picked anything other than
        // `auto` — other modes don't depend on the classifier.
        let runner = AgentRunnerSpec { command: "claude".into(), flags: vec![] };
        for mode in ["acceptEdits", "bypassPermissions", "plan", "default"] {
            require_auto_mode_supported(&Host::Local, &runner, mode).unwrap();
        }
    }

    #[test]
    fn spawn_path_injects_permission_mode_for_claude() {
        // Mirror the relevant lines from start_worker_on_task — the spawn
        // path must compose claude's launch line with --permission-mode so
        // the worker doesn't depend on settings.json's defaultMode taking
        // effect (which has silently regressed in the past).
        let p = fixture_project(); // worker_permissions_mode = "auto"
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.worker_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode auto");
    }

    #[test]
    fn spawn_path_passes_through_non_auto_modes() {
        let mut p = fixture_project();
        p.worker_permissions_mode = "acceptEdits".into();
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.worker_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode acceptEdits");
    }

    #[test]
    fn spawn_path_omits_flag_for_default_mode() {
        // `default` is claude's own baseline; passing the flag is a no-op
        // that just clutters the command line.
        let mut p = fixture_project();
        p.worker_permissions_mode = "default".into();
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.worker_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude");
    }

    #[test]
    fn spawn_path_leaves_non_claude_runners_alone() {
        // Codex doesn't understand --permission-mode; injecting it would
        // crash the runner on launch.
        let mut p = fixture_project();
        p.agent_runners.insert(
            "codex".into(),
            AgentRunnerSpec { command: "codex".into(), flags: vec!["--print".into()] },
        );
        let runner = p.runner("codex").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.worker_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "codex --print");
    }

    #[test]
    fn require_auto_mode_skips_non_claude_runners() {
        // Auto mode is a claude setting; codex / other runners ignore the
        // `defaultMode` key, so probing their `--version` would be both
        // pointless and misleading.
        let runner = AgentRunnerSpec { command: "codex".into(), flags: vec!["--print".into()] };
        require_auto_mode_supported(&Host::Local, &runner, "auto").unwrap();
    }

    #[test]
    fn deploy_worker_settings_writes_local_file_and_creates_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-deploy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let rendered = r#"{"permissions":{"defaultMode":"acceptEdits"}}"#;

        deploy_worker_settings(&Host::Local, &worktree, rendered).unwrap();

        let settings = worktree.join(".claude/settings.json");
        let actual = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(actual, rendered);

        // Idempotent: a second call overwrites without error.
        let updated = r#"{"permissions":{"defaultMode":"plan"}}"#;
        deploy_worker_settings(&Host::Local, &worktree, updated).unwrap();
        let actual2 = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(actual2, updated);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
