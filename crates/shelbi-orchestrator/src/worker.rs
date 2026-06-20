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

use std::path::PathBuf;

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

    // 0. If the project asks for auto-mode, claude must be v2.1.83+. Older
    //    versions silently fall back to `default` and the user gets a Bash
    //    prompt on every command — exactly the bug we're trying to avoid.
    //    Surface it up front so the failure mode is "shelbi rejected this
    //    machine" instead of "my worker keeps pausing for no reason."
    require_auto_mode_supported(&host, &runner, &spec.project.worker_permissions_mode)?;

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
    let launch = shelbi_agent::launch_command(&runner);
    let cd_launch = if host.is_local() {
        format!(
            "cd {wd} && {launch}",
            wd = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
        )
    } else {
        format!(
            "cd {wd} && exec \"${{SHELL:-/bin/bash}}\" -lc {launch}",
            wd = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
            launch = shelbi_agent::shell_escape(&launch),
        )
    };
    shelbi_tmux::send_line(&host, &addr, &cd_launch)?;

    // 6. Wait for claude to render its input prompt before typing the task
    //    body. A fixed sleep is unreliable: 1.5s is plenty locally but
    //    remote workers boot in 3–5s (network + cold caches + worktree
    //    materialization), so the prompt arrives before claude is reading
    //    stdin and lands in the void. Poll the pane until the `❯` glyph —
    //    claude's input prompt — is visible.
    wait_for_claude_ready(&host, &addr, std::time::Duration::from_secs(15))?;
    let prompt = compose_prompt(spec.task_id, spec.branch, spec.task_body);
    shelbi_tmux::send_line(&host, &addr, &prompt)?;

    Ok(addr)
}

/// The minimum claude version that understands `defaultMode: "auto"`. Older
/// versions silently fall back to `default` and the worker pauses on every
/// Bash prompt.
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
    // Only the `claude` CLI honors the `defaultMode` setting; other runners
    // (codex etc.) ignore it, so the version probe is meaningless for them.
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

/// Poll the worker's pane until claude's input prompt glyph (`❯`) appears
/// — i.e., claude has finished booting and is reading stdin. Errors out
/// after `max` rather than silently sending into the void.
fn wait_for_claude_ready(
    host: &Host,
    addr: &TmuxAddr,
    max: std::time::Duration,
) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        let buf = shelbi_tmux::capture(host, addr)?;
        if buf.contains('❯') {
            return Ok(());
        }
        if start.elapsed() >= max {
            return Err(Error::Other(format!(
                "timed out after {:?} waiting for claude input prompt on {}",
                max,
                addr.target()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
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

/// Build the initial prompt: the loop-closing instruction (front-loaded so
/// claude weights it strongly) followed by the task body.
///
/// We can't tell the worker to run `shelbi task move <id> --to review` —
/// shelbi is only installed on the hub, not on remote worker machines. So
/// the handoff happens via a tmux pane-title marker (`printf` is a shell
/// builtin, available everywhere) that the hub poller watches for.
fn compose_prompt(task_id: &str, branch: &str, body: &str) -> String {
    let trimmed = body.trim();
    let body_section = if trimmed.is_empty() {
        format!("# Task {task_id}")
    } else {
        trimmed.to_string()
    };
    format!(
        "You are working on task `{task_id}` on branch `{branch}`. The task is described below.\n\
         \n\
         When the work is complete and committed, signal you're ready for review by emitting this terminal escape sequence (sets the tmux pane title):\n\
         \n\
         \x20\x20\x20\x20printf '\\033]2;shelbi:review\\007'\n\
         \n\
         The hub detects the title change and moves your task into the review column.\n\
         \n\
         ---\n\
         \n\
         {body_section}"
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
    fn prompt_includes_task_id_branch_and_done_instruction() {
        let prompt = compose_prompt("fix-login", "shelbi/fix-login", "Fix the Safari SSO bug.");
        assert!(prompt.contains("Fix the Safari SSO bug."));
        assert!(prompt.contains("fix-login"));
        assert!(prompt.contains("shelbi/fix-login"));
        // Worker can't run shelbi (it's hub-only). Hand-off is via a
        // pane-title marker that the hub poller watches for.
        assert!(prompt.contains("printf"));
        assert!(prompt.contains("shelbi:review"));
        // Instructions land before the body so claude weights them
        // strongly, separated by a `---` rule.
        let instruction_pos = prompt.find("shelbi:review").expect("instruction present");
        let body_pos = prompt.find("Fix the Safari SSO bug.").expect("body present");
        assert!(
            instruction_pos < body_pos,
            "instructions must appear before the task body"
        );
        assert!(prompt.contains("\n---\n"));
    }

    #[test]
    fn prompt_falls_back_to_task_id_heading_when_body_empty() {
        let prompt = compose_prompt("fix-login", "shelbi/fix-login", "   ");
        assert!(prompt.contains("# Task fix-login"));
        assert!(prompt.contains("shelbi:review"));
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
