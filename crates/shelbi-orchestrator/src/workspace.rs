//! Workspace lifecycle: the pre-declared agent slots that pick up Kanban
//! tasks. See [`crate::ensure_dashboard`] for the project's overall tmux
//! layout; this module is concerned only with the per-workspace slot.
//!
//! Each workspace owns a stable worktree at
//! `<machine.work_dir>/.shelbi/wt/<workspace-name>`. The worktree persists
//! across tasks; the workspace switches branches between assignments. The
//! workspace's tmux pane (window for local hub workspaces, session for remote
//! workspaces) is killed and re-created on every assignment to clear the
//! agent's context — that's the user-specified semantics.
//!
//! Reviewer hint: this module does no state writes to task files; the
//! caller (CLI) is responsible for updating `assigned_to` / `branch` /
//! `column`. We just stand up the worktree + tmux pane + claude.

use std::path::{Path, PathBuf};

use shelbi_core::{Error, Host, Machine, Project, Result, TmuxAddr, WorkspaceSpec};

/// Absolute path to the currently-running `shelbi` binary, so the
/// wrapper invocation we hand to tmux is anchored to *this* build
/// rather than whatever happens to be on PATH inside the pane's
/// shell. Mirrors the helper in `crate::current_exe_string`; kept
/// module-local so workspace.rs has no upward dependency on lib.rs.
fn current_exe_string() -> Result<String> {
    Ok(std::env::current_exe()
        .map_err(Error::Io)?
        .to_string_lossy()
        .into_owned())
}

/// Where a workspace's pane lives in tmux. Local workspaces get a window in the
/// project session; remote workspaces get their own session (so they survive
/// SSH drops).
pub fn workspace_tmux_addr(project: &Project, workspace: &WorkspaceSpec) -> Result<TmuxAddr> {
    let machine = project
        .machine(&workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(workspace.machine.clone()))?;
    Ok(match machine.host() {
        Host::Local => TmuxAddr {
            session: format!("shelbi-{}", project.name),
            window: workspace.name.clone(),
        },
        Host::Ssh { .. } => TmuxAddr {
            session: format!("shelbi-w-{}", workspace.name),
            window: "agent".into(),
        },
    })
}

/// `<machine.work_dir>/.shelbi/wt/<workspace-name>` — the workspace's persistent
/// worktree path on its machine.
pub fn workspace_worktree(machine: &Machine, workspace: &WorkspaceSpec) -> PathBuf {
    machine.work_dir.join(".shelbi").join("wt").join(&workspace.name)
}

/// The review-ready file marker for a workspace:
/// `<worktree>/.claude/shelbi-review-ready`.
///
/// The workspace writes its current task id here to hand off for review; the
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
pub fn workspace_review_marker(machine: &Machine, workspace: &WorkspaceSpec) -> PathBuf {
    workspace_worktree(machine, workspace)
        .join(".claude")
        .join("shelbi-review-ready")
}

/// Read the review-ready marker, returning the task id the workspace wrote into
/// it (trimmed) or `None` if the marker is absent or empty. Works for both
/// local and remote workspaces — `cat` is routed through `shelbi-ssh`, which is
/// a no-op wrapper for [`Host::Local`].
pub fn read_review_marker(host: &Host, marker: &Path) -> Result<Option<String>> {
    let path = marker.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["cat", path.as_str()]).map_err(Error::Io)?;
    if !out.status.success() {
        // Missing file → cat exits non-zero. Not an error for us: the
        // workspace simply hasn't signalled review yet.
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

/// Outcome of [`rebase_workspace_branch_onto_default`]. Pure data — the caller
/// decides what to log and whether to surface a warning to the user. We
/// distinguish "rebase wasn't needed" from "rebase succeeded" so the
/// events.log line can call out the actually-rebased commits when there
/// are any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseOutcome {
    /// Default branch is already an ancestor of the workspace's HEAD — the
    /// branch is up to date and no rewrite was needed.
    AlreadyUpToDate { default_sha: String },
    /// Rebase finished cleanly; the workspace's branch is now on top of
    /// `default_sha`. `before_sha`/`after_sha` are HEAD before/after the
    /// rewrite — equal when the rebase ran but produced an empty result
    /// (rare; harmless).
    Rebased {
        before_sha: String,
        after_sha: String,
        default_sha: String,
    },
    /// Rebase ran into conflicts. We aborted it so the worktree returned to
    /// a clean state and the workspace's branch HEAD is unchanged — the human
    /// reviewer will resolve the conflict during the review checkout.
    Conflict {
        default_sha: String,
        stderr_excerpt: String,
        /// Paths git left in a conflicted (unmerged) state, captured before
        /// the abort. Empty when git couldn't enumerate them. Callers that
        /// report conflicts machine-readably (e.g. `zen probe`'s
        /// `rebase_conflict`) surface this list.
        files: Vec<String>,
    },
    /// The rebase couldn't even be attempted (default branch missing,
    /// uncommitted changes that aren't ours to absorb, git itself errored).
    /// The reason field explains why so the events.log row is actionable.
    Skipped { reason: String },
}

impl RebaseOutcome {
    /// Short status token for the `events.log` `status=` field. Stable
    /// over time — UI consumers parse this.
    pub fn status_token(&self) -> &'static str {
        match self {
            RebaseOutcome::AlreadyUpToDate { .. } => "up-to-date",
            RebaseOutcome::Rebased { .. } => "ok",
            RebaseOutcome::Conflict { .. } => "conflict",
            RebaseOutcome::Skipped { .. } => "skipped",
        }
    }

    /// Compact one-line detail string for the `events.log` `detail=`
    /// field. Short SHAs on the happy paths, a snippet of git's stderr
    /// (or the reason) on the failure paths.
    pub fn detail(&self) -> String {
        fn short(sha: &str) -> &str {
            sha.get(..7).unwrap_or(sha)
        }
        match self {
            RebaseOutcome::AlreadyUpToDate { default_sha } => {
                format!("default={}", short(default_sha))
            }
            RebaseOutcome::Rebased {
                before_sha,
                after_sha,
                default_sha,
            } => format!(
                "{}..{}_onto_{}",
                short(before_sha),
                short(after_sha),
                short(default_sha),
            ),
            RebaseOutcome::Conflict {
                default_sha,
                stderr_excerpt,
                ..
            } => {
                let excerpt = stderr_excerpt.trim();
                if excerpt.is_empty() {
                    format!("default={}", short(default_sha))
                } else {
                    format!("default={} {}", short(default_sha), excerpt)
                }
            }
            RebaseOutcome::Skipped { reason } => reason.clone(),
        }
    }
}

/// Rebase the workspace's current branch in `worktree` onto `default_branch`,
/// leaving the worktree on the same branch (now rewritten) on success.
///
/// Why this exists: when a prereq task lands on `main` after a workspace has
/// already started its own task, the workspace's branch sits one or more
/// commits behind main by the time the review marker fires. Without this
/// hook the user had to drop into the workspace's worktree and run
/// `git rebase main` themselves before the review checkout would surface a
/// clean diff — exactly the manual rebase + force-push the task title
/// names. Running it here eliminates that step on the happy path, and
/// fails safely (worktree returned to its pre-rebase state) when a
/// conflict means a human has to step in anyway.
///
/// The worktree shares its `.git` with the project's main clone via
/// `git worktree add`, so `default_branch` already reflects whatever the
/// orchestrator merged onto it on the hub — no fetch is needed.
///
/// Contract:
///
/// - Worktree must be clean (anything outside `.claude/` would be lost
///   to a rebase conflict). The workspace is expected to have committed
///   before writing the review marker; a dirty worktree returns
///   [`RebaseOutcome::Skipped`].
/// - On conflict we run `git rebase --abort` and return
///   [`RebaseOutcome::Conflict`] — the workspace's branch HEAD is unchanged.
/// - Never panics; every git failure surfaces as `Skipped` or `Conflict`
///   so the caller can log it and continue the review promotion.
pub fn rebase_workspace_branch_onto_default(
    host: &Host,
    worktree: &Path,
    default_branch: &str,
) -> RebaseOutcome {
    let wt_str = worktree.to_string_lossy().into_owned();

    // 1. Bail on a dirty worktree. `.claude/` is shelbi's deploy footprint
    //    (settings.json, the review marker itself); it's gitignored so it
    //    never trips `status --porcelain`, but if a user-authored
    //    `.gitignore` doesn't yet exclude it we still don't want a rebase
    //    aborted by our own files. Mirror the carve-out in `preflight_workdir`.
    let dirty = match shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "status", "--porcelain"],
    ) {
        Ok(s) => s,
        Err(e) => {
            return RebaseOutcome::Skipped {
                reason: format!("git_status_failed:{e}"),
            };
        }
    };
    let user_dirty: Vec<&str> = dirty
        .lines()
        .filter(|l| {
            let path = l.get(3..).unwrap_or("");
            !(path.starts_with(".claude/") || path == ".claude")
        })
        .collect();
    if !user_dirty.is_empty() {
        return RebaseOutcome::Skipped {
            reason: format!("dirty_worktree({}_entries)", user_dirty.len()),
        };
    }

    // 2. Resolve the default branch ref. If the workspace's repo doesn't even
    //    know about `default_branch` (fresh clone never fetched, name
    //    typo'd in project YAML), there's nothing to rebase onto.
    let default_sha = match shelbi_ssh::run(
        host,
        [
            "git",
            "-C",
            &wt_str,
            "rev-parse",
            "--verify",
            &format!("{default_branch}^{{commit}}"),
        ],
    ) {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        Ok(_) | Err(_) => {
            return RebaseOutcome::Skipped {
                reason: format!("default_branch_{default_branch}_not_found"),
            };
        }
    };

    let before_sha = match shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "rev-parse", "HEAD"],
    ) {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            return RebaseOutcome::Skipped {
                reason: format!("rev_parse_HEAD_failed:{e}"),
            };
        }
    };

    // 3. Already a descendant? `merge-base --is-ancestor` exits 0 when
    //    `default_branch` is reachable from HEAD — i.e. the workspace's
    //    branch already contains every commit on main. No rewrite needed.
    let ancestor = shelbi_ssh::run(
        host,
        [
            "git",
            "-C",
            &wt_str,
            "merge-base",
            "--is-ancestor",
            default_branch,
            "HEAD",
        ],
    );
    if matches!(ancestor, Ok(ref o) if o.status.success()) {
        return RebaseOutcome::AlreadyUpToDate { default_sha };
    }

    // 4. Run the rebase. Plain `git rebase` (no autostash — we already
    //    proved the worktree is clean; no `--rebase-merges` — workspaces
    //    produce linear branches).
    let out = match shelbi_ssh::run(host, ["git", "-C", &wt_str, "rebase", default_branch]) {
        Ok(o) => o,
        Err(e) => {
            return RebaseOutcome::Skipped {
                reason: format!("rebase_spawn_failed:{e}"),
            };
        }
    };

    if !out.status.success() {
        // Conflict (or some other rebase-time error). Capture the unmerged
        // paths *before* aborting — `git rebase --abort` rolls the worktree
        // back and forgets them. `--diff-filter=U` lists exactly the files
        // left in a conflicted state.
        let files: Vec<String> = shelbi_ssh::run_capture(
            host,
            ["git", "-C", &wt_str, "diff", "--name-only", "--diff-filter=U"],
        )
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();

        // Abort so the worktree returns to its pre-rebase state — the
        // workspace's branch HEAD is unchanged and the next `git status` is
        // clean. Abort is best-effort: a hung rebase that won't abort would
        // leave the worktree in an interactive state, but we still want to
        // log the conflict and let the review proceed.
        let _ = shelbi_ssh::run(host, ["git", "-C", &wt_str, "rebase", "--abort"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let combined = format!("{stderr}{stdout}");
        let excerpt = combined
            .lines()
            .find(|l| {
                let lc = l.to_ascii_lowercase();
                lc.contains("conflict") || lc.contains("error")
            })
            .map(|l| l.trim().to_string())
            .unwrap_or_else(|| {
                combined
                    .lines()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty())
                    .unwrap_or("rebase failed")
                    .to_string()
            });
        return RebaseOutcome::Conflict {
            default_sha,
            stderr_excerpt: excerpt,
            files,
        };
    }

    let after_sha = shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "rev-parse", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    RebaseOutcome::Rebased {
        before_sha,
        after_sha,
        default_sha,
    }
}

/// Does the workspace have a live tmux pane right now?
pub fn workspace_pane_alive(host: &Host, addr: &TmuxAddr) -> Result<bool> {
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

/// Kill the workspace's pane (idempotent — silently OK if already gone).
///
/// Marks an "expected teardown" for `workspace_name` before touching tmux
/// so the local pane's lifecycle wrapper (`shelbi open <name> --as-pane`)
/// can distinguish shelbi-initiated shutdowns from real pane deaths.
/// Without the mark, `tmux kill-window` delivers SIGHUP to the wrapper,
/// which would emit `workspace=<name> pane_alive=false reason=signal:SIGHUP`
/// to events.log even when the caller is a normal dispatch — spuriously
/// tripping the orchestrator's "pane died, surface to user" reaction rule
/// right before the replacement pane comes up. See
/// bug-workspace-pane-alive-false-sighup-fires-spuriously-right-after-dispatch.
pub fn kill_workspace_pane(host: &Host, addr: &TmuxAddr, workspace_name: &str) -> Result<()> {
    // Local: `kill-window -t session:window` (the dashboard session
    // must stay alive). Remote: `kill-session -t session` (the session
    // IS the workspace).
    //
    // The liveness check has to differ too. For local we look for the
    // workspace's window inside the shared dashboard session. For remote
    // we look for the session itself — NOT for a window named `agent`
    // — because tmux's `automatic-rename` (on by default) renames the
    // window after whatever command is running (`claude`, `bash`, …),
    // and a window-name match would miss live sessions and leave them
    // around to collide with the next `task start`.
    match host {
        Host::Local => {
            if !workspace_pane_alive(host, addr)? {
                return Ok(());
            }
            // Best-effort — the wrapper's fallback (fire the event with
            // its historical reason) is the pre-fix behavior, so a mark
            // failure just degrades to that.
            let _ = shelbi_state::mark_expected_teardown(workspace_name);
            let _ = shelbi_ssh::run(host, ["tmux", "kill-window", "-t", &addr.target()])
                .map_err(Error::Io)?;
        }
        Host::Ssh { .. } => {
            if !shelbi_tmux::has_session(host, &addr.session)? {
                return Ok(());
            }
            // Remote workspaces don't run the lifecycle wrapper (no
            // shelbi binary on the workspace host), so there's nothing
            // to suppress on that side — but writing the marker is
            // still safe and keeps the API symmetric.
            let _ = shelbi_state::mark_expected_teardown(workspace_name);
            let _ = shelbi_ssh::run(host, ["tmux", "kill-session", "-t", &addr.session])
                .map_err(Error::Io)?;
        }
    }
    Ok(())
}

/// Spec for `start_workspace_on_task`. We don't take a `&Task` because the
/// caller may have a fresh task id without a frontmatter file yet.
pub struct StartSpec<'a> {
    pub project: &'a Project,
    pub workspace: &'a WorkspaceSpec,
    pub task_id: &'a str,
    pub branch: &'a str,
    /// Body of the task markdown — appended to the prompt as context.
    pub task_body: &'a str,
    /// Name of the agent (under `agents/<name>/`) whose `instructions.md`
    /// is wired up as the runner's system prompt and whose `skills/` dir
    /// is mounted into the worktree's `.claude/skills/`. `None` skips
    /// the agent-context deploy entirely — kept optional so non-CLI
    /// callers that don't yet resolve through
    /// [`crate::dispatch::resolve_dispatch_agent`] (or tests that
    /// exercise the spawn path in isolation) can opt out without
    /// fabricating an agent name.
    pub agent: Option<&'a str>,
}

/// Tear down the workspace's pane, switch its worktree to `branch` (creating
/// the worktree off `default_branch` and the branch off `default_branch` if
/// needed), and start the runner with an initial prompt. Bails on a dirty
/// worktree so the user doesn't silently lose work.
pub fn start_workspace_on_task(spec: StartSpec<'_>) -> Result<TmuxAddr> {
    let machine = spec
        .project
        .machine(&spec.workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(spec.workspace.machine.clone()))?
        .clone();
    let runner = spec
        .project
        .runner(&spec.workspace.runner)
        .ok_or_else(|| Error::UnknownRunner(spec.workspace.runner.clone()))?
        .clone();

    let host = machine.host();
    let worktree = workspace_worktree(&machine, spec.workspace);
    let addr = workspace_tmux_addr(spec.project, spec.workspace)?;

    // 0a. If the project asks for auto-mode, claude must be v2.1.83+. Older
    //     versions silently fall back to `default` and the user gets a Bash
    //     prompt on every command — exactly the bug we're trying to avoid.
    //     Surface it up front so the failure mode is "shelbi rejected this
    //     machine" instead of "my workspace keeps pausing for no reason."
    require_auto_mode_supported(&host, &runner, &spec.project.workspace_permissions_mode)?;

    // 0b. Clear any stale review marker left in the worktree from a previous
    //     task before we reuse the worktree — otherwise the poller could read
    //     an old task id and misfire. Best-effort: a failure here shouldn't
    //     block standing up the workspace.
    let marker = workspace_review_marker(&machine, spec.workspace);
    let _ = clear_review_marker(&host, &marker);

    // 1. Make sure the worktree exists + is on the right branch, clean.
    sync_worktree(
        &host,
        &machine,
        &worktree,
        spec.branch,
        spec.project.base_branch(),
    )?;

    // 2. Drop a rendered .claude/settings.json into the worktree so the
    //    runner picks up shelbi's window-title hooks (idle/working/blocked)
    //    and the per-task message-tail hooks (Phase 7 push delivery).
    //    Prefer the dispatched agent's `agents/<role>/settings.json` when
    //    present so role-specific hook customization actually takes effect
    //    on the worktree's Claude Code session; fall back to the
    //    project-wide template otherwise. Overwrite is fine — this is the
    //    entire on-workspace footprint and we re-render it on every task
    //    start.
    let rendered = render_workspace_settings_preferring_agent(spec.project, spec.agent)?;
    deploy_workspace_settings(&host, &worktree, &rendered)?;

    // 2b. Deploy the dispatched agent's `instructions.md` + skills into the
    //     worktree's `.claude/` footprint. The instructions file becomes the
    //     runner's `--append-system-prompt` source (see step 5 below); the
    //     skills directory is wiped and re-mounted from
    //     `agents/<agent>/skills/` so consecutive dispatches with different
    //     agents on the same workspace don't accumulate skills from earlier
    //     runs. Skipped entirely when `agent` is `None` (e.g. an embed test
    //     that exercises the spawn path without resolving an agent).
    if let Some(agent) = spec.agent {
        deploy_agent_context(&host, &worktree, &spec.project.name, agent)?;
    }

    // 3. Reset the tmux pane — that's how we clear context. If it doesn't
    //    exist yet, this is a no-op; otherwise the next step recreates it.
    kill_workspace_pane(&host, &addr, &spec.workspace.name)?;

    // Inject `--permission-mode <mode>` directly on the claude command line
    // rather than trusting the rendered `.claude/settings.json` to take effect.
    // Settings-based mode is fragile (silent fallback to interactive on any
    // I/O race or version regression) — the CLI flag is authoritative and
    // belongs to the spawn path, where we already know the project's mode.
    let runner_with_mode =
        shelbi_agent::with_permission_mode(&runner, &spec.project.workspace_permissions_mode);
    let launch = with_agent_system_prompt(
        &shelbi_agent::launch_command(&runner_with_mode),
        spec.agent.map(|_| WORKTREE_AGENT_INSTRUCTIONS_REL),
        &runner,
    );

    // 4 + 5. Create the pane and launch the agent.
    //
    // Local: the pane's top-level process is the `shelbi open
    //   <name> --as-pane` lifecycle wrapper. The wrapper cd's into the
    //   worktree, execs the agent, waits for it, and writes a
    //   `pane_alive=false reason=<…>` line to events.log on any exit
    //   path (clean exit, SIGHUP from tmux teardown, SIGTERM from
    //   kill-window, SIGINT, child crash). Same wrapper invocation as
    //   the sidebar-click path so a manual `tmux kill-window` and a
    //   workspace dispatch can't drift apart.
    //
    // Remote: the lifecycle wrapper isn't deployed to remote machines
    //   (no shelbi binary on the workspace host), so the historical
    //   `send_line(cd && claude)` flow stays. We still create an empty
    //   session first so the user's login rc files run when the shell
    //   spawns.
    //
    //   `LANG=C.UTF-8` is cheap, low-risk insurance: a non-interactive
    //   SSH launch can leave the tmux server in the C locale, and forcing
    //   UTF-8 keeps every box-drawing/glyph path well-defined regardless
    //   of host config.
    //
    //   The `$SHELL -lc` re-exec on the remote path is needed because
    //   tmux was started by `ssh host -- tmux new-session …`, which runs
    //   through a NON-login non-interactive shell — tmux (and every pane
    //   it spawns) inherits a stripped-down PATH missing Homebrew, asdf,
    //   nvm, etc. The login shell sources ~/.zprofile / ~/.bash_profile
    //   and picks up the same PATH the user has in their own terminal.
    match &host {
        Host::Local => {
            let shelbi_bin = current_exe_string()?;
            let pane_cmd = format!(
                "{bin} --project {proj} open {ws} --as-pane",
                bin = shelbi_agent::shell_escape(&shelbi_bin),
                proj = shelbi_agent::shell_escape(&spec.project.name),
                ws = shelbi_agent::shell_escape(&spec.workspace.name),
            );
            // Mirror the orchestrator pane's bootstrap shape (lib.rs):
            // pass `sh`, `-c`, `<cmd>` as three positionals so tmux runs
            // the wrapper through a shell, picking up the user's PATH
            // from the local tmux server's existing env.
            let target = format!("{}:", addr.session);
            if !shelbi_tmux::has_session(&host, &addr.session)? {
                shelbi_ssh::run_capture(
                    &host,
                    [
                        "tmux",
                        "new-session",
                        "-d",
                        "-s",
                        &addr.session,
                        "-n",
                        &addr.window,
                        "sh",
                        "-c",
                        &pane_cmd,
                    ],
                )?;
            } else {
                shelbi_ssh::run_capture(
                    &host,
                    [
                        "tmux",
                        "new-window",
                        "-d",
                        "-t",
                        &target,
                        "-n",
                        &addr.window,
                        "sh",
                        "-c",
                        &pane_cmd,
                    ],
                )?;
            }
        }
        Host::Ssh { .. } => {
            shelbi_tmux::new_session(&host, &addr.session, &addr.window, None)?;
            let cd_launch = remote_cd_launch(&worktree, &launch);
            shelbi_tmux::send_line(&host, &addr, &cd_launch)?;
        }
    }

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
    let prompt = compose_prompt(
        spec.task_id,
        spec.branch,
        spec.task_body,
        &marker,
        &spec.project.default_branch,
        &spec.project.name,
        shelbi_agent::polls_for_messages(&runner),
    );
    shelbi_tmux::send_line(&host, &addr, &prompt)?;

    // 7. Verify the prompt actually got submitted, not just typed into the
    //    input box. claude's `UserPromptSubmit` hook (see workspace settings
    //    template) writes `\033]2;shelbi:working\007` to the pane title on
    //    every submit — so once any `shelbi:` marker appears in the title,
    //    we know Enter landed. If it doesn't within a short window, the
    //    most common cause is that the trailing Enter raced claude's input
    //    focus and was dropped; resend it once and try again. If still no
    //    marker, surface a dispatch=stalled line in events.log so the
    //    orchestrator (and `shelbi events tail`) sees it instead of the
    //    workspace silently sitting on the prompt.
    confirm_prompt_submitted(&host, &addr, spec.task_id, &spec.workspace.name);

    Ok(addr)
}

/// How long to wait for proof the prompt got submitted (pane title flips
/// to a `shelbi:*` marker OR the pane content shows claude is busy
/// processing). Submit lands almost immediately when the hook fires; this
/// just covers the slow path (busy SSH, sluggish tmux server).
const PROMPT_SUBMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// How often to re-check the pane while waiting for the submit signal.
const PROMPT_SUBMIT_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// Wait for the prompt-submitted signal; if it doesn't arrive, resend Enter
/// once and wait again; if it still doesn't arrive, log a dispatch=stalled
/// event and warn on stderr. Best-effort — failures here don't abort the
/// task start (the workspace may still recover), they just surface the stall
/// so the orchestrator stops assuming the dispatch succeeded.
fn confirm_prompt_submitted(host: &Host, addr: &TmuxAddr, task_id: &str, workspace: &str) {
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
        "shelbi: dispatched prompt to {} but no submission signal appeared \
         after a retry Enter — workspace may be sitting on an unsubmitted prompt; \
         check the pane",
        addr.target(),
    );
    if let Err(e) = shelbi_state::append_dispatch_event(
        task_id,
        workspace,
        "enter-stalled",
        "no submit signal after retry",
    ) {
        eprintln!("shelbi: failed to record dispatch stall in events.log: {e}");
    }
}

/// Poll the workspace's pane until we have proof the prompt got submitted, or
/// `timeout` elapses. Capture failures during the poll are transient (the
/// SSH socket can hiccup); we just ignore them and keep polling.
///
/// Two independent signals — either one is sufficient:
///
/// 1. **Pane title carries a `shelbi:*` marker.** The workspace's
///    `UserPromptSubmit` hook writes `shelbi:working` via OSC, so when the
///    title shows that, Enter definitely landed. The catch is that
///    Claude's own OSC 2 writes (it updates the title with a live
///    activity summary as it works) typically clobber `shelbi:working`
///    within tens of milliseconds — the marker is gone by the time our
///    poll cycle reads it. So we can't rely on this as the only signal.
///
/// 2. **Pane content shows Claude is actively processing.** When the
///    prompt has been submitted and claude is working, the pane renders a
///    spinner line like `⏺ Brewed for 5s · …` or `· Booping… (10s · ↑ 2k
///    tokens)` and an `esc to interrupt` footer — none of which appear in
///    the empty-input / waiting-for-user state. This signal survives
///    Claude's title overwrites because we read the pane *body*, not the
///    title.
fn wait_for_prompt_submitted(host: &Host, addr: &TmuxAddr, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let title = shelbi_tmux::pane_title(host, addr).unwrap_or_default();
        if shelbi_state::parse_pane_title_marker(&title).is_some() {
            return true;
        }
        // Title-marker missed (probably clobbered by claude's own OSC).
        // Fall back to the pane body — claude's busy spinner / "esc to
        // interrupt" line is a much more durable signal that Enter landed.
        let screen = shelbi_tmux::capture(host, addr).unwrap_or_default();
        if claude_is_processing(&screen) {
            return true;
        }
        std::thread::sleep(PROMPT_SUBMIT_POLL);
    }
    false
}

/// True when the captured pane shows claude is actively processing a
/// prompt — the prompt-submitted state, as distinct from an empty input
/// box waiting for the user to type something.
///
/// Why these markers are the right ones: each appears ONLY after a
/// prompt has been submitted and claude has started work, and NONE of
/// them appear on the empty-input / ready-for-typing screen. So a match
/// here is sufficient to conclude Enter landed. We avoid keying on the
/// prompt body text (claude's history scrollback contains it in both
/// "submitted" and "still in input" states, depending on how the pane
/// wrapped) and avoid keying on the static input footer (`shift+tab to
/// cycle`, `for shortcuts`) — those persist across both states.
fn claude_is_processing(screen: &str) -> bool {
    // Lowercase compare so "ESC to interrupt" / "esc to interrupt" both
    // match — Claude's footer phrasing has drifted across versions.
    // NB: do NOT add "esc to cancel" here — the trust-this-folder dialog
    // uses that exact string, and we'd otherwise read the dialog as
    // "prompt submitted" before the user has cleared it.
    let lower = screen.to_ascii_lowercase();
    const BUSY_MARKERS: &[&str] = &[
        "esc to interrupt", // claude's "currently working" footer
        "ctrl+c to stop",   // some older versions
        // Claude's spinner line ends with `(<duration> · ↑ <n> tokens)` or
        // `(<duration> · ↓ <n> tokens)` once tokens have streamed. Either
        // direction is proof a prompt got submitted and claude is mid-turn.
        "tokens)",
    ];
    BUSY_MARKERS.iter().any(|m| lower.contains(m))
}

/// The minimum claude version that understands `--permission-mode auto`.
/// Older versions either silently fall back to `default` or reject the flag,
/// and the workspace pauses on every Bash prompt.
const CLAUDE_AUTO_MODE_MIN: (u32, u32, u32) = (2, 1, 83);

/// If the project wants auto-mode and the runner is claude, ensure the
/// workspace host's claude is new enough to understand it. Quiet pass-through
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
            "claude {} on this workspace is too old for workspace_permissions_mode: auto \
             (need {}+, classifier-based auto-approval). Either upgrade claude on the \
             workspace host, or set `workspace_permissions_mode` in this project's config to \
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

/// Poll the workspace pane until claude's input box is on screen and ready to
/// accept the initial prompt. Returns `Ok(true)` once ready, `Ok(false)` on
/// timeout (the caller sends anyway).
///
/// ## Why this exists / what the bug actually was
///
/// The original code slept a fixed 1500ms then typed. That fails on a
/// fresh devbox workspace for a reason that is *not* terminal encoding:
/// investigation on a Linux workspace showed claude emits the `❯` prompt glyph
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

/// Render the workspace `settings.json` for `project`, preferring the
/// dispatched agent's per-role `agents/<role>/settings.json` over the
/// project-wide `workspace-settings.json.template` when one is present.
///
/// Per-role precedence matters because the Phase 7 message-tail hooks
/// live in the role's settings file — a user editing
/// `agents/developer/settings.json` to add a project-specific hook
/// should see that change on the next dispatch, not silently lose it to
/// the project-wide fallback. Substitutes the legacy
/// `{{workspace_permissions_mode}}` placeholder for backwards-compat
/// with user-authored templates that still reference it.
///
/// Falls back to the project-wide template when `agent` is `None` (e.g.
/// embed tests) or when the role doesn't ship a settings.json (e.g.
/// a future codex-only role).
pub fn render_workspace_settings_preferring_agent(
    project: &shelbi_core::Project,
    agent: Option<&str>,
) -> Result<String> {
    let body = match agent {
        Some(name) => shelbi_state::load_agent_settings(&project.name, name)
            .map_err(|e| Error::Other(format!("{e}")))?,
        None => None,
    };
    let template = match body {
        Some(b) => b,
        None => shelbi_state::render_workspace_settings(project)
            .map_err(|e| Error::Other(format!("{e}")))?,
    };
    Ok(template.replace("{{workspace_permissions_mode}}", &project.workspace_permissions_mode))
}

/// Write the rendered workspace `settings.json` to `<worktree>/.claude/` on
/// `host`. Local hosts get a direct filesystem write; remote hosts get an
/// `ssh mkdir -p` followed by `scp` of the rendered file. The workspace
/// machine never executes any shelbi code — this file is the whole
/// on-workspace footprint.
pub fn deploy_workspace_settings(
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

/// Relative path (from the worktree root) where the dispatched agent's
/// `instructions.md` is deployed. Kept in `.claude/` alongside the
/// settings + review marker so the whole shelbi deploy footprint is
/// gitignored together and there's exactly one place to look when
/// debugging an agent that loaded the wrong prompt.
pub const WORKTREE_AGENT_INSTRUCTIONS_REL: &str = ".claude/agent-instructions.md";

/// Relative path (from the worktree root) where the dispatched agent's
/// `skills/` directory is mounted. Claude Code auto-loads any
/// `.claude/skills/` entries on launch.
pub const WORKTREE_AGENT_SKILLS_REL: &str = ".claude/skills";

/// Deploy the dispatched agent's `instructions.md` to the worktree and
/// refresh `.claude/skills/` from the agent's `skills/` directory. One
/// orchestration helper so the spawn path doesn't have to manage the
/// individual primitives; the caller just hands us a (host, worktree,
/// project, agent) tuple and we do the rest.
///
/// Idempotent and overwrite-safe: every call rewrites the instructions
/// file and clears `.claude/skills/` before mounting, so the worktree's
/// agent context reflects the *current* agent — not a leftover from a
/// prior task on the same workspace.
pub fn deploy_agent_context(
    host: &Host,
    worktree: &Path,
    project_name: &str,
    agent: &str,
) -> Result<()> {
    // Compose preamble (`agents/_shared/preamble.md`, if present) + the
    // agent's `instructions.md` into one body. `compose_agent_prompt`
    // handles the missing-preamble case (just the agent's prompt) and
    // the `{{assistant_name}}` substitution. Any file-read failure
    // surfaces from there with the same shape the old direct-read used.
    let mut composed = shelbi_state::compose_agent_prompt(project_name, agent)
        .map_err(|e| Error::Other(format!("{e}")))?;
    // Orchestrator-only: if a `handoff.md` was left by the previous
    // instance (on `shelbi reload` or `shelbi quit`), splice it onto
    // the end of the system prompt as a `<system-reminder>` block so
    // the new instance picks up where the old one left off. Read-and-
    // delete — handoff is one-shot; persistent state lives in
    // `state.json`. Skipped silently for every non-orchestrator agent
    // (developer dispatches, custom agents) so a worktree's deploy
    // never ingests handoff data meant for the dashboard.
    if agent == shelbi_state::ORCHESTRATOR_AGENT {
        match shelbi_state::take_orchestrator_handoff(project_name) {
            Ok(Some(handoff)) => {
                composed = splice_orchestrator_handoff(&composed, &handoff);
            }
            Ok(None) => {}
            Err(e) => {
                // Surface but don't fail the deploy — a missing handoff
                // is a degraded start (new instance is cold), not a
                // broken one.
                tracing::warn!(
                    project = project_name,
                    error = %e,
                    "take_orchestrator_handoff failed; starting orchestrator cold",
                );
            }
        }
    }
    deploy_agent_instructions(host, worktree, &composed)?;

    let skills_src = shelbi_state::agent_skills_dir(project_name, agent)?;
    refresh_agent_skills(host, worktree, &skills_src)?;

    // Best-effort: nudge the user toward the new agents/ layout the
    // first time a dispatch lands while a legacy CLAUDE.md is still on
    // disk. Idempotent across multiple dispatches per orchestrator
    // session (state-flag gated). Don't fail the dispatch on a hint
    // write error — the user can still hand-migrate.
    let _ = shelbi_state::maybe_emit_claude_md_migration_hint(project_name);
    Ok(())
}

/// Wrap `handoff` in a `<system-reminder>` block and append it to
/// `composed`. The block is rendered as plain text — the orchestrator's
/// claude runtime renders it as a system-reminder in the conversation
/// transcript, which is exactly how we want the next instance to read
/// it (load-bearing context from the previous instance, distinct from
/// the evergreen instructions above it).
///
/// Pure on its inputs so it's unit-testable without a real handoff
/// file on disk.
fn splice_orchestrator_handoff(composed: &str, handoff: &str) -> String {
    let mut out =
        String::with_capacity(composed.len() + handoff.len() + 64);
    out.push_str(composed);
    if !composed.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str("<system-reminder>\n");
    out.push_str(handoff);
    if !handoff.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("</system-reminder>\n");
    out
}

/// Write `instructions` to `<worktree>/.claude/agent-instructions.md` so
/// the runner can `--append-system-prompt "$(cat …)"` from it on launch.
/// Mirrors [`deploy_workspace_settings`]'s local-vs-remote split.
pub fn deploy_agent_instructions(
    host: &Host,
    worktree: &Path,
    instructions: &str,
) -> Result<()> {
    let claude_dir = worktree.join(".claude");
    let dest_path = claude_dir.join("agent-instructions.md");
    match host {
        Host::Local => {
            std::fs::create_dir_all(&claude_dir).map_err(Error::Io)?;
            std::fs::write(&dest_path, instructions).map_err(Error::Io)?;
            Ok(())
        }
        Host::Ssh { host: ssh_host } => scp_text_to_remote(
            ssh_host,
            &claude_dir.to_string_lossy(),
            &dest_path.to_string_lossy(),
            instructions,
            "agent-instructions",
        ),
    }
}

/// Clear `<worktree>/.claude/skills/` and recursively mirror
/// `skills_src` into it. A non-existent or empty `skills_src` just
/// produces an empty destination — the v1 default agents ship with no
/// skills, so this is the normal happy path.
pub fn refresh_agent_skills(host: &Host, worktree: &Path, skills_src: &Path) -> Result<()> {
    let dest = worktree.join(".claude").join("skills");
    let dest_str = dest.to_string_lossy().into_owned();

    // Clear first. `rm -rf` is idempotent — succeeds whether the path
    // exists or not, which keeps the first-dispatch (nothing there yet)
    // and Nth-dispatch (carry-over from a different agent) cases on the
    // same code path.
    let rm = shelbi_ssh::run(host, ["rm", "-rf", &dest_str]).map_err(Error::Io)?;
    if !rm.status.success() {
        return Err(Error::Command {
            cmd: format!("rm -rf {dest_str}"),
            status: rm.status.to_string(),
            stderr: String::from_utf8_lossy(&rm.stderr).into_owned(),
        });
    }

    // Always (re)create the destination — even when skills_src is empty
    // we want `.claude/skills/` to exist so the runner's skill loader
    // doesn't trip on a missing path.
    let mkdir = shelbi_ssh::run(host, ["mkdir", "-p", &dest_str]).map_err(Error::Io)?;
    if !mkdir.status.success() {
        return Err(Error::Command {
            cmd: format!("mkdir -p {dest_str}"),
            status: mkdir.status.to_string(),
            stderr: String::from_utf8_lossy(&mkdir.stderr).into_owned(),
        });
    }

    if !skills_src.is_dir() {
        // Agent's skills/ dir not on disk (legacy materialize, user nuked
        // it, etc.). The hub side's self-heal will recreate it on next
        // `shelbi reload`; for now an empty deploy is correct.
        return Ok(());
    }

    let entries: Vec<_> = std::fs::read_dir(skills_src)
        .map_err(Error::Io)?
        .collect::<std::result::Result<_, _>>()
        .map_err(Error::Io)?;
    if entries.is_empty() {
        return Ok(());
    }

    match host {
        Host::Local => copy_dir_contents_local(skills_src, &dest)?,
        Host::Ssh { host: ssh_host } => copy_dir_contents_to_remote(ssh_host, skills_src, &dest)?,
    }
    Ok(())
}

/// Recursively copy the contents of `src` (a directory) into `dest`. The
/// destination must already exist. Used to mirror the agent's `skills/`
/// directory into the worktree on local hosts; remote hosts go through
/// [`copy_dir_contents_to_remote`].
fn copy_dir_contents_local(src: &Path, dest: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let file_type = entry.file_type().map_err(Error::Io)?;
        let entry_path = entry.path();
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&target).map_err(Error::Io)?;
            copy_dir_contents_local(&entry_path, &target)?;
        } else {
            std::fs::copy(&entry_path, &target).map_err(Error::Io)?;
        }
    }
    Ok(())
}

/// Recursively copy `src` (a directory's contents) into a remote `dest`
/// over SCP. v1 default agents ship with empty skills, so this happy
/// path is normally a no-op; when users start populating `skills/` the
/// tree is expected to be small (a handful of `.md`s + maybe an
/// `assets/` dir). Per-file SCP is plenty.
fn copy_dir_contents_to_remote(ssh_host: &str, src: &Path, dest: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let file_type = entry.file_type().map_err(Error::Io)?;
        let entry_path = entry.path();
        let target = dest.join(entry.file_name());
        let target_str = target.to_string_lossy().into_owned();
        if file_type.is_dir() {
            let mkdir = shelbi_ssh::run(
                &Host::Ssh { host: ssh_host.to_string() },
                ["mkdir", "-p", &target_str],
            )
            .map_err(Error::Io)?;
            if !mkdir.status.success() {
                return Err(Error::Command {
                    cmd: format!("ssh {ssh_host} mkdir -p {target_str}"),
                    status: mkdir.status.to_string(),
                    stderr: String::from_utf8_lossy(&mkdir.stderr).into_owned(),
                });
            }
            copy_dir_contents_to_remote(ssh_host, &entry_path, &target)?;
        } else {
            let dest_uri = format!("{ssh_host}:{target_str}");
            let mut cmd = std::process::Command::new("scp");
            cmd.arg("-q").arg("-B").arg(&entry_path).arg(&dest_uri);
            let out = cmd.output().map_err(Error::Io)?;
            if !out.status.success() {
                return Err(Error::Command {
                    cmd: format!("scp {} {dest_uri}", entry_path.display()),
                    status: out.status.to_string(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Build the `cd <wt> && … exec $SHELL -lc <launch>` line we send into
/// the remote tmux pane, prefixing the exec with `SHELBI_HUB_SOCK=<path>`
/// so the agent picks up the SSH-reverse-forwarded hub socket
/// ([`shelbi_state::remote_hub_socket_path`], default
/// `/tmp/shelbi-hub.sock`). Worker→hub events (`pane_alive=false`,
/// future verbs) flow through that socket; with no `SHELBI_HUB_SOCK` set
/// the agent's instructions paragraph falls through to a no-op and loss
/// is accepted (the spec calls this "best-effort + hub-side detection").
///
/// We park the assignment immediately before `exec` so it scopes to the
/// agent process (the surrounding `$SHELL -lc` strips its own
/// environment otherwise — env-prefix-before-exec is the POSIX idiom
/// for "set var for THIS command only and inherit it").
fn remote_cd_launch(worktree: &Path, launch: &str) -> String {
    let sock = shelbi_state::remote_hub_socket_path();
    format!(
        "cd {wd} && LANG=C.UTF-8 SHELBI_HUB_SOCK={sock} exec \"${{SHELL:-/bin/bash}}\" -lc {launch}",
        wd = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
        sock = shelbi_agent::shell_escape(&sock.to_string_lossy()),
        launch = shelbi_agent::shell_escape(launch),
    )
}

/// Append the `--append-system-prompt "$(cat .claude/agent-instructions.md)"`
/// flag to the runner's launch command when an agent is being dispatched
/// AND the runner is `claude` (the only CLI that understands the flag).
/// Returns `launch` unchanged for non-claude runners or when no agent
/// instructions are being deployed.
///
/// The `$(cat …)` substitution is intentional: keeping the prompt body
/// out of the command line means the launched line stays human-readable
/// in the pane (no 10 KB of `# Task …\n\n` scrollback noise) and avoids
/// the per-platform ARG_MAX risk of inlining a large prompt.
fn with_agent_system_prompt(
    launch: &str,
    instructions_rel: Option<&str>,
    runner: &shelbi_core::AgentRunnerSpec,
) -> String {
    let Some(rel) = instructions_rel else {
        return launch.to_string();
    };
    let is_claude = std::path::Path::new(&runner.command)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("claude");
    if !is_claude {
        // Other runners (codex etc.) don't understand the flag; leave
        // them alone. The agent's instructions.md is still deployed to
        // the worktree for callers that want to read it manually.
        return launch.to_string();
    }
    format!(
        "{launch} --append-system-prompt \"$(cat {rel})\"",
        rel = shelbi_agent::shell_escape(rel),
    )
}

fn scp_settings_to_remote(
    ssh_host: &str,
    remote_dir: &str,
    remote_path: &str,
    rendered: &str,
) -> Result<()> {
    scp_text_to_remote(ssh_host, remote_dir, remote_path, rendered, "workspace-settings")
}

/// Push `body` to `remote_path` on `ssh_host`, ensuring `remote_dir`
/// exists first. Shared by the workspace-settings and agent-instructions
/// deploy paths so both go through the same scp + mkdir routine. `tag`
/// is folded into the local tempfile name so a crash mid-deploy leaves
/// debuggable breadcrumbs.
fn scp_text_to_remote(
    ssh_host: &str,
    remote_dir: &str,
    remote_path: &str,
    body: &str,
    tag: &str,
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

    // 2. Stage the body in a local tempfile, then scp it. The tempfile
    //    is in $TMPDIR so the local FS handles cleanup if we crash
    //    before unlinking it.
    let tmp_path = std::env::temp_dir().join(format!(
        "shelbi-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&tmp_path, body).map_err(Error::Io)?;

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

/// Build the initial prompt: the task body + the loop-closing instructions
/// that tell the workspace how to rebase onto current `default_branch` and then
/// mark itself done.
///
/// The handoff is a file marker, not a pane title or a `shelbi` CLI call.
/// The workspace writes its task id into `<worktree>/.claude/shelbi-review-ready`
/// (see [`workspace_review_marker`]); the hub poller picks it up and moves the
/// task to the review column. This survives Claude's own OSC pane-title
/// writes and the Stop hook, both of which used to clobber a `shelbi:review`
/// title before the poller could read it, and it needs no `shelbi` binary on
/// the workspace host.
///
/// The rebase step lives in the prompt (not in poll-side promotion code) so
/// the workspace re-runs its checks against the rebased base before signalling
/// review — a hub-side rebase happens after handoff, when there's no agent
/// around to fix conflicts or re-run tests.
fn compose_prompt(
    task_id: &str,
    branch: &str,
    body: &str,
    marker: &Path,
    default_branch: &str,
    project: &str,
    polls_messages: bool,
) -> String {
    let trimmed = body.trim();
    let body_section = if trimmed.is_empty() {
        format!("# Task {task_id}\n")
    } else {
        trimmed.to_string()
    };
    let id_esc = shelbi_agent::shell_escape(task_id);
    let marker_esc = shelbi_agent::shell_escape(&marker.to_string_lossy());
    let polling_section = if polls_messages {
        message_polling_section(task_id, project, &id_esc)
    } else {
        String::new()
    };
    format!(
        "{body_section}\n\n\
         ---\n\
         You are working on task `{task_id}` on branch `{branch}`. When \
         the work is complete and committed, do these two things in order:\n\
         \n\
         1. Rebase your branch onto current `{default_branch}` so the review \
         sees a clean diff against an up-to-date base — a stale base produces \
         test failures that have nothing to do with your change and inflates \
         the diff with commits already on `{default_branch}`:\n\
         \n\
         git fetch origin {default_branch} && git rebase origin/{default_branch}\n\
         \n\
         If the rebase produces conflicts, resolve them, run `git rebase \
         --continue`, and re-run any affected tests before moving on. Do NOT \
         write the marker until the rebase is complete and your work still \
         passes against the rebased base.\n\
         \n\
         2. Signal that it's ready for review by writing the task id to the \
         review marker file:\n\
         \n\
         printf '%s\\n' {id_esc} > {marker_esc}\n\
         \n\
         The hub watches for this file and moves your task to the review \
         column on its next poll. Write the marker once; you can keep \
         working in this pane and talk to the user afterward without \
         affecting the handoff.{polling_section}"
    )
}

/// The pull-style message-delivery paragraph appended to the prompt for
/// runners without a hook surface (codex, aider, …). Claude Code receives
/// hub messages through its hooks and never sees this section.
///
/// Codex drives every step — file reads, edits, and commands — through its
/// `shell` tool, so "after every shell command" is the concrete, guaranteed
/// cadence the task plan asks for: any non-trivial work runs at least one
/// shell command in a short window, where "between significant steps" would
/// be vague enough to skip.
///
/// The cursor file holds the 1-indexed number of the *next* unread line. It
/// starts at 1 (read from the top), and after each poll advances to
/// `<line count> + 1` so the following `tail -n +$CURSOR` begins past the
/// last line already read — re-reading is what an off-by-one here would
/// cause, so the `+ 1` is load-bearing. The `2>/dev/null` guards keep a
/// cold start (no log yet) from erroring before the hub's first write.
fn message_polling_section(task_id: &str, project: &str, id_esc: &str) -> String {
    format!(
        "\n\n\
         ---\n\
         Your message log is at `.shelbi/messages/{task_id}.log` (relative to \
         this worktree). The orchestrator appends directives there while you \
         work, and you must pull them yourself. **After every shell command \
         you run**, check the log for new lines:\n\
         \n\
         CURSOR=$(cat .shelbi/messages/{id_esc}.cursor 2>/dev/null || echo 1)\n\
         tail -n +\"$CURSOR\" .shelbi/messages/{id_esc}.log 2>/dev/null\n\
         echo $(($(wc -l < .shelbi/messages/{id_esc}.log 2>/dev/null || echo 0) + 1)) > .shelbi/messages/{id_esc}.cursor\n\
         \n\
         Act on any new messages before continuing your current work. Each \
         line is one JSON message with a `msg_id`; for every new message, ack \
         it so the orchestrator knows it landed:\n\
         \n\
         echo '{{\"verb\":\"message-ack\",\"project\":\"{project}\",\"task_id\":\"{task_id}\",\"msg_id\":\"<msg-id>\"}}' | nc -U \"$SHELBI_HUB_SOCK\""
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
            "workspace worktree at {wt_str} has uncommitted changes — \
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
            config_mode: None,
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
            workspaces: vec![
                WorkspaceSpec {
                    name: "alice".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                },
                WorkspaceSpec {
                    name: "bob".into(),
                    machine: "m2".into(),
                    runner: "claude".into(),
                },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn local_workspace_lives_in_project_session_window() {
        let p = fixture_project();
        let addr = workspace_tmux_addr(&p, &p.workspaces[0]).unwrap();
        assert_eq!(addr.session, "shelbi-myapp");
        assert_eq!(addr.window, "alice");
    }

    #[test]
    fn remote_workspace_gets_its_own_session() {
        let p = fixture_project();
        let addr = workspace_tmux_addr(&p, &p.workspaces[1]).unwrap();
        assert_eq!(addr.session, "shelbi-w-bob");
        assert_eq!(addr.window, "agent");
    }

    #[test]
    fn worktree_path_under_machine_workdir() {
        let p = fixture_project();
        let wt = workspace_worktree(&p.machines[0], &p.workspaces[0]);
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
            "main",
            "myapp",
            false,
        );
        assert!(prompt.contains("Fix the Safari SSO bug."));
        assert!(prompt.contains("fix-login"));
        assert!(prompt.contains("shelbi/fix-login"));
        // Hands off via the file marker, not the old pane-title / CLI path.
        assert!(prompt.contains(".claude/shelbi-review-ready"));
        assert!(prompt.contains("printf"));
        assert!(!prompt.contains("shelbi task move"));
        assert!(prompt.contains("\n---\n"));
        // A hook-capable runner (claude) gets no polling instructions.
        assert!(!prompt.contains(".shelbi/messages/"));
        assert!(!prompt.contains("message-ack"));
    }

    #[test]
    fn prompt_falls_back_to_task_id_heading_when_body_empty() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
        let prompt =
            compose_prompt("fix-login", "shelbi/fix-login", "   ", &marker, "main", "myapp", false);
        assert!(prompt.contains("# Task fix-login"));
        assert!(prompt.contains(".claude/shelbi-review-ready"));
    }

    #[test]
    fn polling_runner_prompt_includes_message_log_instructions() {
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            "main",
            "myapp",
            true,
        );
        // Still hands off the same way, and still rebases first.
        assert!(prompt.contains(".claude/shelbi-review-ready"));
        assert!(prompt.contains("git rebase origin/main"));
        // Concrete poll cadence + the per-task log/cursor paths.
        assert!(prompt.contains("After every shell command"));
        assert!(prompt.contains(".shelbi/messages/fix-login.log"));
        assert!(prompt.contains(".shelbi/messages/fix-login.cursor"));
        // Cursor advances past the last-read line (the +1 that avoids a
        // re-delivery off-by-one).
        assert!(prompt.contains("wc -l < .shelbi/messages/fix-login.log"));
        assert!(prompt.contains(") + 1)) > .shelbi/messages/fix-login.cursor"));
        assert!(prompt.contains("tail -n +\"$CURSOR\" .shelbi/messages/fix-login.log"));
        // Ack carries the project + task id and goes to the hub socket.
        assert!(prompt.contains("\"verb\":\"message-ack\""));
        assert!(prompt.contains("\"project\":\"myapp\""));
        assert!(prompt.contains("\"task_id\":\"fix-login\""));
        assert!(prompt.contains("nc -U \"$SHELBI_HUB_SOCK\""));
    }

    #[test]
    fn prompt_instructs_workspace_to_rebase_onto_default_branch_before_marker() {
        // The whole point of this rebase step is to keep the review's base
        // fresh — otherwise tests fail against a stale `default_branch` and
        // the diff_size includes commits that are already on main. Pin the
        // ordering (rebase → marker) and the exact command so a future
        // prompt rewording can't quietly drop the rebase or invert the
        // sequence.
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            "main",
            "myapp",
            false,
        );
        assert!(
            prompt.contains("git fetch origin main && git rebase origin/main"),
            "missing rebase command in prompt: {prompt}"
        );
        let rebase_at = prompt
            .find("git rebase origin/main")
            .expect("rebase command must appear in prompt");
        let marker_at = prompt
            .find("printf '%s\\n'")
            .expect("marker printf must appear in prompt");
        assert!(
            rebase_at < marker_at,
            "rebase must be instructed BEFORE the marker write; prompt: {prompt}"
        );
    }

    #[test]
    fn prompt_uses_projects_default_branch_for_rebase_target() {
        // Not every project's main branch is named `main` — verify the
        // command picks up `default_branch` rather than hard-coding it.
        let marker = PathBuf::from("/work/myapp/.shelbi/wt/alice/.claude/shelbi-review-ready");
        let prompt = compose_prompt(
            "fix-login",
            "shelbi/fix-login",
            "Fix the Safari SSO bug.",
            &marker,
            "trunk",
            "myapp",
            false,
        );
        assert!(
            prompt.contains("git fetch origin trunk && git rebase origin/trunk"),
            "rebase must target the project's default_branch: {prompt}"
        );
        assert!(
            !prompt.contains("origin/main"),
            "stale `main` reference leaked into prompt: {prompt}"
        );
    }

    // Real captures observed on a Linux (delta) workspace, used to pin the
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

    // Captured from a workspace pane that had just submitted its prompt and
    // was mid-turn — used to pin the busy-state heuristic against
    // claude's actual rendered output. The point of this whole helper is
    // that nothing here mentions `shelbi:` anywhere: claude's own OSC 2
    // writes have already clobbered the workspace's `shelbi:working` title
    // marker, so the pane-title probe would have missed this state.
    const BUSY_SCREEN_SPINNER: &str = "\
✻ Brewed for 1m 1s · 2 shells, 1 monitor still running

· Booping… (7m 16s · ↑ 19.8k tokens)
─────────────────────────────────────────────────────
❯
─────────────────────────────────────────────────────
  Model: Opus 4.7 | Ctx Used: 17.0% | Cost: $4.69
  ⏵⏵ auto mode on (shift+tab to cycle)";

    const BUSY_SCREEN_ESC_FOOTER: &str = "\
⏺ Update(crates/shelbi-orchestrator/src/review.rs)
  ⎿  Added 1 line

✳ Working on the fix...
─────────────────────────────────────────────────────
❯
─────────────────────────────────────────────────────
  esc to interrupt · ctrl+c twice to exit";

    #[test]
    fn claude_is_processing_detects_busy_pane_when_title_marker_lost() {
        // Both fixtures are post-submit screens where claude is mid-turn.
        // Neither has a `shelbi:` title marker (claude's own OSC 2 writes
        // have already overwritten it), so the title-based probe alone
        // would mis-fire `enter-stalled`. The content fallback catches
        // both.
        assert!(claude_is_processing(BUSY_SCREEN_SPINNER));
        assert!(claude_is_processing(BUSY_SCREEN_ESC_FOOTER));
    }

    #[test]
    fn claude_is_processing_does_not_fire_on_empty_input_or_trust_dialog() {
        // The empty-input ready screen — what the pane looks like
        // BEFORE the prompt is typed. Must not match, otherwise the
        // probe declares success before we've even sent Enter.
        assert!(!claude_is_processing(INPUT_BOX_SCREEN));
        // Trust dialog before claude has accepted the first prompt —
        // the prompt would've been typed INTO this dialog instead of an
        // input box, and we want the probe to keep waiting (and the
        // trust-dismiss path to dismiss it) rather than spuriously
        // signal "submitted."
        assert!(!claude_is_processing(TRUST_DIALOG_SCREEN));
        assert!(!claude_is_processing(""));
        assert!(!claude_is_processing("➜  bob git:(main) claude"));
    }

    #[test]
    fn claude_is_processing_matches_case_insensitively() {
        // Claude's footer text has rendered both "ESC to interrupt" and
        // "esc to interrupt" across versions; we lower-case the screen
        // before matching so neither slips through.
        assert!(claude_is_processing("ESC to interrupt"));
        // The token-counter parenthetical matches in either streaming
        // direction (↑ user-prompt, ↓ tool-output).
        assert!(claude_is_processing("(12s · ↑ 1.2k tokens)"));
        assert!(claude_is_processing("(45s · ↓ 8k tokens)"));
    }

    #[test]
    fn claude_is_processing_does_not_false_positive_on_trust_dialog_footer() {
        // The trust-this-folder dialog footer reads "Enter to confirm ·
        // Esc to cancel" — that "esc to" prefix is the same one claude
        // uses in its busy footer ("esc to interrupt"). We deliberately
        // do NOT include "esc to cancel" in the busy markers because
        // the trust dialog must never read as "claude submitted my
        // prompt and is working" — the prompt was typed INTO the
        // dialog, not into claude's input. Pin that behavior so a
        // future "be more inclusive" tweak can't quietly regress it.
        assert!(!claude_is_processing("Enter to confirm · Esc to cancel"));
    }

    #[test]
    fn review_marker_lives_under_gitignored_claude_dir() {
        let p = fixture_project();
        let marker = workspace_review_marker(&p.machines[0], &p.workspaces[0]);
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
        // Mirror the relevant lines from start_workspace_on_task — the spawn
        // path must compose claude's launch line with --permission-mode so
        // the workspace doesn't depend on settings.json's defaultMode taking
        // effect (which has silently regressed in the past).
        let p = fixture_project(); // workspace_permissions_mode = "auto"
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode auto");
    }

    #[test]
    fn spawn_path_passes_through_non_auto_modes() {
        let mut p = fixture_project();
        p.workspace_permissions_mode = "acceptEdits".into();
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode acceptEdits");
    }

    #[test]
    fn spawn_path_omits_flag_for_default_mode() {
        // `default` is claude's own baseline; passing the flag is a no-op
        // that just clutters the command line.
        let mut p = fixture_project();
        p.workspace_permissions_mode = "default".into();
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude");
    }

    #[test]
    fn spawn_path_doesnt_double_flag_when_yaml_already_has_permission_mode() {
        // Repro the real shelbi YAML: `agent_runners.claude.flags:
        // [--permission-mode, auto]` was kept as a pre-bd7a23f quick fix and
        // the spawn-path injection then produced `claude --permission-mode
        // auto --permission-mode auto`. The idempotency check in
        // with_permission_mode must collapse this back to one flag.
        let mut p = fixture_project();
        p.agent_runners.insert(
            "claude".into(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec!["--permission-mode".into(), "auto".into()],
            },
        );
        let runner = p.runner("claude").unwrap().clone();
        let runner_with_mode =
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
        let launch = shelbi_agent::launch_command(&runner_with_mode);
        assert_eq!(launch, "claude --permission-mode auto");
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
            shelbi_agent::with_permission_mode(&runner, &p.workspace_permissions_mode);
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
    fn deploy_workspace_settings_writes_local_file_and_creates_dir() {
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

        deploy_workspace_settings(&Host::Local, &worktree, rendered).unwrap();

        let settings = worktree.join(".claude/settings.json");
        let actual = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(actual, rendered);

        // Idempotent: a second call overwrites without error.
        let updated = r#"{"permissions":{"defaultMode":"plan"}}"#;
        deploy_workspace_settings(&Host::Local, &worktree, updated).unwrap();
        let actual2 = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(actual2, updated);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn agent_test_tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-agent-deploy-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn deploy_agent_instructions_writes_to_claude_dir() {
        // The runner's --append-system-prompt sources from this file; the
        // spawn path's job is to put the agent's instructions.md there
        // verbatim. Pin the destination path so a refactor that moves the
        // deploy footprint can't silently break the runner's loader.
        let tmp = agent_test_tmpdir("instructions");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let body = "# Developer Agent\n\nYou are the developer.\n";

        deploy_agent_instructions(&Host::Local, &worktree, body).unwrap();
        let dest = worktree.join(".claude/agent-instructions.md");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), body);

        // Idempotent — a dispatch on the same workspace overwrites with
        // the new agent's body (e.g. developer → orchestrator).
        let new_body = "# Orchestrator Agent\n";
        deploy_agent_instructions(&Host::Local, &worktree, new_body).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), new_body);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn refresh_agent_skills_mirrors_source_and_creates_empty_dir_when_no_src() {
        let tmp = agent_test_tmpdir("skills-empty");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        // First case: source skills dir doesn't exist on disk at all.
        // Refresh must still leave `.claude/skills/` in place (empty) so
        // the runner's loader doesn't trip on a missing path.
        let missing_src = tmp.join("agent-with-no-skills/skills");
        refresh_agent_skills(&Host::Local, &worktree, &missing_src).unwrap();
        let dest = worktree.join(".claude/skills");
        assert!(dest.is_dir(), "skills dest must exist even with no source");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);

        // Second case: source has files — they should appear at the dest.
        let src = tmp.join("agent-with-skills/skills");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("greet.md"), "say hi\n").unwrap();
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("nested/inner.md"), "inner skill\n").unwrap();
        refresh_agent_skills(&Host::Local, &worktree, &src).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("greet.md")).unwrap(),
            "say hi\n",
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("nested/inner.md")).unwrap(),
            "inner skill\n",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn refresh_agent_skills_clears_carryover_from_previous_dispatch() {
        // The "different agent on the same workspace" scenario: dispatch
        // A leaves `.claude/skills/a-tool.md`; dispatch B (different
        // agent) must not see A's leftovers. The refresh contract is
        // "destination reflects current source, period."
        let tmp = agent_test_tmpdir("skills-clear");
        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        // Dispatch A.
        let agent_a_skills = tmp.join("agent-a/skills");
        std::fs::create_dir_all(&agent_a_skills).unwrap();
        std::fs::write(agent_a_skills.join("a-tool.md"), "agent A skill\n").unwrap();
        refresh_agent_skills(&Host::Local, &worktree, &agent_a_skills).unwrap();
        let dest = worktree.join(".claude/skills");
        assert!(dest.join("a-tool.md").exists());

        // Dispatch B with different skills — A's file must be gone.
        let agent_b_skills = tmp.join("agent-b/skills");
        std::fs::create_dir_all(&agent_b_skills).unwrap();
        std::fs::write(agent_b_skills.join("b-tool.md"), "agent B skill\n").unwrap();
        refresh_agent_skills(&Host::Local, &worktree, &agent_b_skills).unwrap();
        assert!(
            !dest.join("a-tool.md").exists(),
            "agent A's skill must be cleared on dispatch B"
        );
        assert!(dest.join("b-tool.md").exists(), "agent B's skill missing");

        // Dispatch C with no skills at all (or back to A's empty agent)
        // — the dest must be empty afterwards.
        let agent_c_skills = tmp.join("agent-c/skills");
        std::fs::create_dir_all(&agent_c_skills).unwrap();
        refresh_agent_skills(&Host::Local, &worktree, &agent_c_skills).unwrap();
        assert!(dest.is_dir(), "skills dir must persist (just empty)");
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            0,
            "skills dir must be empty after dispatch C",
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Phase 5 acceptance criterion: the line we send into a remote
    /// pane sets `SHELBI_HUB_SOCK` so the agent can write
    /// worker→hub events through the SSH-reverse-forwarded socket.
    /// The default landing path is `/tmp/shelbi-hub.sock`.
    #[test]
    fn remote_cd_launch_prefixes_exec_with_hub_socket_env_var() {
        let _g = crate::test_lock::acquire();
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        let wt = PathBuf::from("/work/myapp/.shelbi/wt/bob");
        let line = remote_cd_launch(&wt, "claude --permission-mode auto");
        assert!(
            line.contains("SHELBI_HUB_SOCK=/tmp/shelbi-hub.sock"),
            "expected default socket path in: {line}"
        );
        // Must scope to the exec'd shell, not the surrounding tmux
        // pane — otherwise the `$SHELL -lc` env-strip drops it before
        // claude inherits it.
        let env_at = line.find("SHELBI_HUB_SOCK=").unwrap();
        let exec_at = line.find("exec ").unwrap();
        assert!(env_at < exec_at, "SHELBI_HUB_SOCK must come BEFORE exec: {line}");
        assert!(line.starts_with("cd "), "still cd's into the worktree: {line}");
        assert!(line.contains("LANG=C.UTF-8"), "LANG fix preserved: {line}");
    }

    /// `$SHELBI_REMOTE_HUB_SOCK` is the hub-side override knob (per
    /// [`shelbi_state::remote_hub_socket_path`]) — if a user retargets
    /// the reverse forward, the env var injected into the remote pane
    /// must follow.
    #[test]
    fn remote_cd_launch_honors_remote_hub_socket_path_override() {
        let _g = crate::test_lock::acquire();
        std::env::set_var("SHELBI_REMOTE_HUB_SOCK", "/run/user/1000/shelbi.sock");
        let wt = PathBuf::from("/work/myapp/.shelbi/wt/bob");
        let line = remote_cd_launch(&wt, "claude");
        assert!(
            line.contains("SHELBI_HUB_SOCK=/run/user/1000/shelbi.sock"),
            "override not honored: {line}"
        );
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
    }

    #[test]
    fn with_agent_system_prompt_appends_claude_flag_when_agent_set() {
        let runner = AgentRunnerSpec { command: "claude".into(), flags: vec![] };
        let launch = "claude --permission-mode auto";
        let out = with_agent_system_prompt(
            launch,
            Some(WORKTREE_AGENT_INSTRUCTIONS_REL),
            &runner,
        );
        // The flag reads from the worktree-relative file so it works
        // identically on local and remote hosts (cwd is the worktree).
        assert!(
            out.contains("--append-system-prompt"),
            "missing flag: {out}"
        );
        assert!(
            out.contains("$(cat .claude/agent-instructions.md)"),
            "expected cat substitution in launch line: {out}"
        );
        // The pre-existing flags must survive the append.
        assert!(out.starts_with("claude --permission-mode auto"), "got: {out}");
    }

    #[test]
    fn with_agent_system_prompt_noop_when_no_agent_or_non_claude() {
        let claude = AgentRunnerSpec { command: "claude".into(), flags: vec![] };
        let codex = AgentRunnerSpec { command: "codex".into(), flags: vec![] };
        let base = "claude --permission-mode auto";

        // No agent → no flag injection (e.g. a test or non-CLI caller
        // that omits the agent context).
        assert_eq!(
            with_agent_system_prompt(base, None, &claude),
            base,
        );

        // Codex doesn't understand the flag; injecting would crash the
        // runner. The agent's instructions.md is still on disk for any
        // future runner-specific loader to pick up.
        assert_eq!(
            with_agent_system_prompt(
                "codex",
                Some(WORKTREE_AGENT_INSTRUCTIONS_REL),
                &codex,
            ),
            "codex",
        );
    }

    /// Hub-side fixture: lays out `<SHELBI_HOME>/projects/<p>/agents/
    /// <developer,orchestrator>/{instructions.md,skills/}` so the
    /// deploy_agent_context happy path has something real to read from.
    fn install_default_agents_under_home(home: &Path, project: &str) {
        let dev = home.join(format!("projects/{project}/agents/developer"));
        let orch = home.join(format!("projects/{project}/agents/orchestrator"));
        std::fs::create_dir_all(dev.join("skills")).unwrap();
        std::fs::create_dir_all(orch.join("skills")).unwrap();
        std::fs::write(dev.join("instructions.md"), "# developer\nfix the bug\n").unwrap();
        std::fs::write(orch.join("instructions.md"), "# orchestrator\ncoordinate\n").unwrap();
        // One skill on the developer to prove mounting carries content.
        std::fs::write(dev.join("skills/debug.md"), "# skill: debug\n").unwrap();
    }

    /// Phase 7: when the dispatch resolves to an agent that ships a
    /// per-role `agents/<role>/settings.json`, that file's content is
    /// preferred over the project-wide workspace-settings template.
    /// This is what carries the SessionStart + Stop message-tail hooks
    /// onto the worktree's Claude Code session.
    #[test]
    fn render_workspace_settings_prefers_per_role_when_present() {
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("render-prefer-role");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "myapp");

        // Per-role settings.json with a unique marker.
        let role_settings = home.join("projects/myapp/agents/developer/settings.json");
        std::fs::write(&role_settings, r#"{"_marker":"from-developer-role"}"#).unwrap();

        let project = fixture_project();
        let rendered =
            render_workspace_settings_preferring_agent(&project, Some("developer")).unwrap();
        assert!(
            rendered.contains("from-developer-role"),
            "per-role settings should win: {rendered}",
        );

        // Without an agent name → project-wide template fallback. The
        // shipped default contains the Phase 7 SessionStart hook string.
        let rendered_fallback =
            render_workspace_settings_preferring_agent(&project, None).unwrap();
        assert!(
            rendered_fallback.contains("SessionStart"),
            "project-wide default must include SessionStart hook: {rendered_fallback}",
        );
        assert!(
            !rendered_fallback.contains("from-developer-role"),
            "fallback must not see the per-role marker: {rendered_fallback}",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `{{workspace_permissions_mode}}` substitution still works for
    /// the per-role file (so a user-authored template with the legacy
    /// placeholder doesn't ship an unsubstituted literal into the
    /// deployed `.claude/settings.json`).
    #[test]
    fn render_workspace_settings_substitutes_placeholder_in_per_role_file() {
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("render-prefer-role-subst");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "myapp");

        let role_settings = home.join("projects/myapp/agents/developer/settings.json");
        std::fs::write(
            &role_settings,
            r#"{"permissions":{"defaultMode":"{{workspace_permissions_mode}}"}}"#,
        )
        .unwrap();

        let mut project = fixture_project();
        project.workspace_permissions_mode = "acceptEdits".into();
        let rendered =
            render_workspace_settings_preferring_agent(&project, Some("developer")).unwrap();
        assert!(
            rendered.contains(r#""defaultMode":"acceptEdits""#),
            "placeholder must be substituted: {rendered}",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_agent_context_loads_named_agent_into_worktree() {
        // Acceptance criterion (a): the developer agent's instructions.md
        // lands at `.claude/agent-instructions.md` and its skills/ dir
        // mirrors into `.claude/skills/`. Exercises the full hub-side
        // path that the spawn function calls in step 2b.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-developer");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "developer").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        assert_eq!(
            std::fs::read_to_string(&instructions).unwrap(),
            "# developer\nfix the bug\n",
            "developer's instructions.md must land verbatim in the worktree",
        );
        let skill = worktree.join(".claude/skills/debug.md");
        assert_eq!(
            std::fs::read_to_string(&skill).unwrap(),
            "# skill: debug\n",
            "developer's skills/ contents must mirror into .claude/skills/",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Acceptance criterion (a): when `agents/_shared/preamble.md`
    /// exists, the workspace spawn path deploys the agent's
    /// `.claude/agent-instructions.md` with the preamble prepended (and
    /// a blank line separator) — the runner's --append-system-prompt
    /// then loads the composed prompt as one body.
    #[test]
    fn deploy_agent_context_prepends_shared_preamble_when_present() {
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-preamble");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");
        // Seed the optional shared preamble. The compose pipeline must
        // pick it up without any opt-in flag.
        let preamble_path = shelbi_state::agent_shared_preamble_path("p").unwrap();
        std::fs::create_dir_all(preamble_path.parent().unwrap()).unwrap();
        std::fs::write(&preamble_path, "monorepo overview\n").unwrap();

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "developer").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert_eq!(
            body, "monorepo overview\n\n# developer\nfix the bug\n",
            "preamble must lead, blank line separator, then the agent's instructions"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn splice_orchestrator_handoff_wraps_in_system_reminder_block() {
        // The next orchestrator instance reads the handoff as system-
        // reminder context — locked-in shape so a renamer doesn't
        // silently move the marker tags.
        let out = splice_orchestrator_handoff(
            "# orchestrator\nbody\n",
            "in-flight: nothing\n",
        );
        assert!(out.starts_with("# orchestrator\nbody\n"));
        assert!(out.contains("<system-reminder>\nin-flight: nothing\n</system-reminder>\n"));
    }

    #[test]
    fn splice_orchestrator_handoff_normalises_missing_trailing_newlines() {
        // Either input may lack a trailing newline (the orchestrator
        // wrote without one, or a hand-edited instructions.md was
        // saved without one). The splice has to leave the block tags
        // on their own lines regardless.
        let out = splice_orchestrator_handoff("body", "handoff");
        assert!(out.ends_with("</system-reminder>\n"));
        assert!(out.contains("\n<system-reminder>\nhandoff\n</system-reminder>\n"));
    }

    #[test]
    fn deploy_agent_context_splices_handoff_into_orchestrator_prompt_then_deletes_file() {
        // Acceptance criterion: on orchestrator (re)launch, if
        // `agents/orchestrator/handoff.md` exists, the deploy step
        // splices it in as a <system-reminder> block AND deletes the
        // file so the next launch doesn't double-ingest the same
        // handoff.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-handoff-splice");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        // Seed the handoff file the previous instance would have left
        // behind on `shelbi reload` / `shelbi quit`.
        let handoff_path = shelbi_state::orchestrator_handoff_path("p").unwrap();
        std::fs::write(&handoff_path, "watching: task xyz\n").unwrap();

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "orchestrator").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(
            body.contains("# orchestrator\ncoordinate"),
            "orchestrator instructions should still be present: {body}"
        );
        assert!(
            body.contains("<system-reminder>\nwatching: task xyz\n</system-reminder>"),
            "handoff must be spliced as a system-reminder block: {body}"
        );
        assert!(
            !handoff_path.exists(),
            "handoff.md must be deleted after ingestion (one-shot transfer file)"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_agent_context_skips_handoff_splice_for_non_orchestrator_agents() {
        // A workspace dispatch under the developer agent must NEVER
        // ingest the orchestrator's handoff — that file is private to
        // the dashboard pane, and a leaked splice would dump
        // unrelated state into a worker's prompt.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-handoff-dev-skip");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        let handoff_path = shelbi_state::orchestrator_handoff_path("p").unwrap();
        std::fs::write(&handoff_path, "private orch state\n").unwrap();

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "developer").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(
            !body.contains("private orch state"),
            "developer dispatch must NOT ingest orchestrator handoff: {body}"
        );
        assert!(
            handoff_path.exists(),
            "developer dispatch must NOT consume the orchestrator's handoff file"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_agent_context_orchestrator_works_without_handoff_file() {
        // No handoff file is the normal cold-start case (first launch,
        // first reload, etc.). Deploy must not error or splice an
        // empty block.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-handoff-none");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        deploy_agent_context(&Host::Local, &worktree, "p", "orchestrator").unwrap();

        let instructions = worktree.join(".claude/agent-instructions.md");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(
            !body.contains("<system-reminder>"),
            "cold start must NOT inject an empty system-reminder block: {body}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn deploy_agent_context_swaps_agents_on_successive_dispatches() {
        // Acceptance criteria (b) + (c) combined: same workspace, two
        // back-to-back dispatches under DIFFERENT agents (the
        // user-owned-status-under-Zen path is one common producer of
        // this). The second dispatch must clear the first's skills and
        // overwrite the instructions.md — the worktree's agent context
        // reflects the *current* agent, not whatever shipped first.
        let _g = crate::test_lock::acquire();
        let tmp = agent_test_tmpdir("ctx-swap");
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        install_default_agents_under_home(&home, "p");

        let worktree = tmp.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        // Dispatch 1 — developer.
        deploy_agent_context(&Host::Local, &worktree, "p", "developer").unwrap();
        let instructions = worktree.join(".claude/agent-instructions.md");
        let skills_dir = worktree.join(".claude/skills");
        assert!(skills_dir.join("debug.md").exists());
        assert!(std::fs::read_to_string(&instructions)
            .unwrap()
            .contains("developer"));

        // Dispatch 2 — orchestrator. Different agent, no skills of its
        // own. The developer's `debug.md` must NOT survive the swap; the
        // instructions file is overwritten.
        deploy_agent_context(&Host::Local, &worktree, "p", "orchestrator").unwrap();
        assert!(
            !skills_dir.join("debug.md").exists(),
            "developer's skill leaked into orchestrator dispatch",
        );
        assert!(
            skills_dir.is_dir(),
            "skills dir must persist after agent swap (just empty)",
        );
        assert!(std::fs::read_to_string(&instructions)
            .unwrap()
            .contains("orchestrator"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod rebase_git_tests {
    //! Real-git tests for [`rebase_workspace_branch_onto_default`]. Each test
    //! provisions a tiny on-disk repo with a `main` branch + a feature
    //! branch off it, then exercises one outcome of the rebase function.
    //! Skipped silently if `git` isn't on PATH so the suite still runs on
    //! a git-less sandbox.
    //!
    //! The shape under test is the workspace-side worktree path: in the real
    //! system the worktree shares a `.git` with the project's main clone
    //! (via `git worktree add`), but for the rebase function only the
    //! worktree's own ref/object access matters, so a plain `git init`
    //! repo with a working tree is enough fidelity.
    use super::*;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run_git_in(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(repo);
        for a in args {
            cmd.arg(a);
        }
        // Pin author identity so commit creation works on hosts without
        // a configured user (CI sandboxes, fresh containers). These are
        // process-scoped via env vars, so they don't touch the user's
        // global git config.
        cmd.env("GIT_AUTHOR_NAME", "Shelbi Test");
        cmd.env("GIT_AUTHOR_EMAIL", "test@shelbi.local");
        cmd.env("GIT_COMMITTER_NAME", "Shelbi Test");
        cmd.env("GIT_COMMITTER_EMAIL", "test@shelbi.local");
        cmd.output().expect("git command failed to spawn")
    }

    /// Create a fresh repo with `main` as the default branch and one
    /// committed README so that branching is meaningful. Returns the
    /// repo path.
    fn init_repo(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-rebase-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let init = run_git_in(&dir, &["init", "-q", "-b", "main"]);
        assert!(
            init.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        std::fs::write(dir.join("README.md"), "# repo\n").unwrap();
        let add = run_git_in(&dir, &["add", "README.md"]);
        assert!(add.status.success());
        let commit = run_git_in(&dir, &["commit", "-q", "-m", "initial"]);
        assert!(
            commit.status.success(),
            "initial commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
        dir
    }

    fn commit_file(repo: &std::path::Path, name: &str, contents: &str, message: &str) {
        std::fs::write(repo.join(name), contents).unwrap();
        let add = run_git_in(repo, &["add", name]);
        assert!(add.status.success());
        let commit = run_git_in(repo, &["commit", "-q", "-m", message]);
        assert!(
            commit.status.success(),
            "commit failed: {}",
            String::from_utf8_lossy(&commit.stderr),
        );
    }

    fn head_sha(repo: &std::path::Path) -> String {
        let out = run_git_in(repo, &["rev-parse", "HEAD"]);
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn branch_sha(repo: &std::path::Path, branch: &str) -> String {
        let out = run_git_in(repo, &["rev-parse", branch]);
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn already_up_to_date_when_branch_contains_default() {
        // Feature branch that's strictly ahead of main → nothing to rebase.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("uptodate");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"])
            .status
            .success()
            .then_some(())
            .expect("branch checkout");
        commit_file(&repo, "feature.txt", "hi\n", "feature work");

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        match outcome {
            RebaseOutcome::AlreadyUpToDate { default_sha } => {
                assert_eq!(default_sha, branch_sha(&repo, "main"));
            }
            other => panic!("expected AlreadyUpToDate, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn rebases_cleanly_when_main_advanced_independently() {
        // Branch off main, then advance main with a non-conflicting commit,
        // then run the auto-rebase. The feature branch should be rewritten
        // on top of the new main, with the feature commit preserved.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("clean");

        // Branch off main's current tip.
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "feature\n", "feature work");
        let feature_before = head_sha(&repo);

        // Advance main with a commit on a separate file.
        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "prereq.txt", "prereq\n", "prereq landed");
        let new_main = head_sha(&repo);

        // Back to the feature branch (this is how the workspace's worktree
        // would be left at the moment the marker fires).
        run_git_in(&repo, &["checkout", "-q", "feature"]);
        assert_eq!(head_sha(&repo), feature_before);

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        match outcome {
            RebaseOutcome::Rebased {
                before_sha,
                after_sha,
                default_sha,
            } => {
                assert_eq!(before_sha, feature_before);
                assert_eq!(default_sha, new_main);
                assert_ne!(after_sha, feature_before, "HEAD must move");
                // Post-rebase: the feature commit sits on top of new main.
                let parent_of_head = String::from_utf8_lossy(
                    &run_git_in(&repo, &["rev-parse", "HEAD~1"]).stdout,
                )
                .trim()
                .to_string();
                assert_eq!(parent_of_head, new_main);
                // Both files survive the rewrite.
                assert!(repo.join("feature.txt").exists());
                assert!(repo.join("prereq.txt").exists());
            }
            other => panic!("expected Rebased, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn conflict_is_aborted_and_branch_head_unchanged() {
        // Both branches touch the same file; the rebase must conflict,
        // we must abort it, and the workspace's branch HEAD must return to
        // its pre-rebase state with a clean worktree. The human reviewer
        // resolves the conflict during the review checkout.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("conflict");

        // Seed a file on main, then branch off.
        commit_file(&repo, "shared.txt", "v0\n", "seed shared");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "shared.txt", "feature change\n", "feature edit");
        let feature_before = head_sha(&repo);

        // Conflicting main commit on the same file.
        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "shared.txt", "main change\n", "main edit");

        run_git_in(&repo, &["checkout", "-q", "feature"]);
        assert_eq!(head_sha(&repo), feature_before);

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        match outcome {
            RebaseOutcome::Conflict { stderr_excerpt, files, .. } => {
                assert!(
                    !stderr_excerpt.is_empty(),
                    "expected a non-empty conflict excerpt"
                );
                assert!(
                    files.iter().any(|f| f == "shared.txt"),
                    "conflict must name the unmerged file, got {files:?}"
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // HEAD must be unchanged after the abort, and the worktree must be
        // clean (no rebase-in-progress state, no merge conflict markers).
        assert_eq!(
            head_sha(&repo),
            feature_before,
            "branch HEAD must roll back after abort"
        );
        let status = run_git_in(&repo, &["status", "--porcelain"]);
        assert!(status.status.success());
        let stdout = String::from_utf8_lossy(&status.stdout);
        assert!(
            stdout.trim().is_empty(),
            "worktree must be clean after abort, got: {stdout}"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn missing_default_branch_is_skipped() {
        // Default-branch name in project YAML doesn't exist in the
        // worktree (renamed branch, typo, fresh shallow clone). Function
        // must skip rather than fail the whole review handoff.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("missing-default");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "hi\n", "feature work");

        let outcome =
            rebase_workspace_branch_onto_default(&Host::Local, &repo, "ghost-branch-does-not-exist");
        match outcome {
            RebaseOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("ghost-branch-does-not-exist"),
                    "reason should name the missing branch: {reason}"
                );
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn dirty_worktree_is_skipped() {
        // Workspace forgot to commit before writing the marker. The function
        // must NOT run the rebase (it would lose the uncommitted work) and
        // must report a skip reason explaining why.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("dirty");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "v0\n", "feature work");

        // Advance main so a rebase would otherwise be needed.
        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "prereq.txt", "prereq\n", "prereq");
        run_git_in(&repo, &["checkout", "-q", "feature"]);

        // Uncommitted change in the workspace's worktree.
        std::fs::write(repo.join("feature.txt"), "wip change\n").unwrap();

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        match outcome {
            RebaseOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("dirty"),
                    "reason should mention dirty: {reason}"
                );
            }
            other => panic!("expected Skipped on dirty worktree, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn dirty_only_in_dot_claude_is_ignored() {
        // shelbi's own deploy footprint lives under `.claude/` (settings,
        // review marker). It's gitignored in normal use, but if a user
        // hasn't ignored it yet we still don't want it blocking a rebase
        // — same carve-out the review preflight applies.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let repo = init_repo("dotclaude");
        run_git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        commit_file(&repo, "feature.txt", "v0\n", "feature work");

        run_git_in(&repo, &["checkout", "-q", "main"]);
        commit_file(&repo, "prereq.txt", "prereq\n", "prereq");
        run_git_in(&repo, &["checkout", "-q", "feature"]);

        // Drop a marker-style file under .claude/. It's untracked, but
        // the carve-out skips it from the dirty check.
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(repo.join(".claude/shelbi-review-ready"), "task-id\n").unwrap();

        let outcome = rebase_workspace_branch_onto_default(&Host::Local, &repo, "main");
        assert!(
            matches!(outcome, RebaseOutcome::Rebased { .. }),
            "expected Rebased despite .claude/ presence, got {outcome:?}"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn detail_format_uses_short_shas() {
        // The detail helper feeds straight into events.log; downstream
        // parsers expect a stable, compact shape — short 7-char SHAs and
        // a recognizable `default=` prefix.
        let outcome = RebaseOutcome::Rebased {
            before_sha: "abcdef0123456789".into(),
            after_sha: "1234567890abcdef".into(),
            default_sha: "fedcba9876543210".into(),
        };
        assert_eq!(outcome.detail(), "abcdef0..1234567_onto_fedcba9");

        let outcome = RebaseOutcome::AlreadyUpToDate {
            default_sha: "abcdef0123456789".into(),
        };
        assert_eq!(outcome.detail(), "default=abcdef0");
    }
}
