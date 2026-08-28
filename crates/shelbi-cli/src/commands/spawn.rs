use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::Args as ClapArgs;
use shelbi_core::{validate_agent_id, Agent, Host, Machine, Project, Status, TmuxAddr};

use super::require_project;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Kebab-case workspace id, used as branch + worktree dir + tmux window name.
    pub id: String,
    /// Machine to run on (must be declared in the project).
    #[arg(long)]
    pub on: String,
    /// Agent runner name (must be declared in the project).
    #[arg(long)]
    pub runner: String,
    /// Initial prompt to send to the agent.
    pub prompt: String,
    /// Override the generated branch name.
    #[arg(long)]
    pub branch: Option<String>,
    /// tmux session to attach the workspace window to. Defaults to
    /// `shelbi-<project>`.
    #[arg(long, env = "SHELBI_TMUX_SESSION")]
    pub session: Option<String>,
}

pub fn run(project_opt: Option<String>, args: Args) -> Result<()> {
    let project_name = require_project(project_opt)?;
    validate_agent_id(&args.id).map_err(|e| anyhow!(e))?;

    let project = shelbi_state::load_project(&project_name)
        .with_context(|| format!("loading project `{project_name}`"))?;

    // Share the workspace injection lock with `shelbi send` and task
    // dispatch. Besides serializing duplicate legacy spawns, this keeps the
    // initial text -> settle -> Enter sequence from interleaving with a send
    // that begins as soon as the agent record becomes visible.
    let _pane_injection_lock =
        shelbi_state::lock_workspace(&project_name, &args.id).map_err(|e| anyhow!(e))?;

    let machine = project
        .machine(&args.on)
        .ok_or_else(|| anyhow!("machine `{}` not in project `{project_name}`", args.on))?
        .clone();

    let runner_spec = project
        .runner(&args.runner)
        .ok_or_else(|| anyhow!("runner `{}` not declared in project", args.runner))?
        .clone();

    let host = machine.host();
    // For LOCAL workspaces we put them as a window inside `shelbi-<project>` so
    // they sit alongside the dashboard and orchestrator. For REMOTE workspaces
    // we give each workspace its own tmux session named `shelbi-w-<id>` on the
    // remote — so the workspace survives a hub disconnect, and re-attaching is
    // just `ssh host -t tmux attach -t shelbi-w-<id>`.
    let (session, window_name) = if host.is_local() {
        (
            args.session
                .clone()
                .unwrap_or_else(|| format!("shelbi-{}", project.name)),
            args.id.clone(),
        )
    } else {
        (format!("shelbi-w-{}", args.id), "agent".to_string())
    };
    let branch = args
        .branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{}", args.id));

    let worktree = worktree_path(&machine, &args.id);
    let work_dir_display = expand_tilde(&worktree);

    println!(
        "spawning agent {} on {} ({host:?})\n  branch: {}\n  worktree: {}\n  session/window: {}:{}",
        args.id,
        machine.name,
        branch,
        work_dir_display.display(),
        session,
        window_name,
    );

    // 1. Make sure the repo's .gitignore covers .shelbi/ so the parent
    //    worktree doesn't get marked dirty by our metadata.
    ensure_gitignored(&host, &machine)?;

    // 2. Create the worktree (git worktree add -b <branch> <path>).
    // Lock order is workspace -> Git worktrees/refs. Keep the inner lock
    // scoped to named checkout; pane startup must not hold it.
    let git_worktree_lock = shelbi_state::lock_git_worktrees(&project_name)
        .map_err(|e| anyhow!(e))?;
    create_worktree(&host, &machine, &branch, &worktree, &project)?;
    drop(git_worktree_lock);

    // 3. Spawn the workspace tmux pane. We open it with an interactive shell
    //    (no inline command) so the user's rc files run and pick up tools
    //    installed in shell-specific PATHs (npm-global, asdf, pyenv, nvm).
    //    Then we send-keys the cd+launch and the initial prompt.
    let addr = if host.is_local() {
        if !shelbi_tmux::has_session(&host, &session).map_err(|e| anyhow!(e))? {
            shelbi_tmux::new_session(&host, &session, "shelbi", None)
                .map_err(|e| anyhow!(e))
                .context("creating tmux session")?;
        }
        shelbi_tmux::new_window(&host, &session, &window_name, None)
            .map_err(|e| anyhow!(e))
            .context("creating workspace window")?
    } else {
        if shelbi_tmux::has_session(&host, &session).map_err(|e| anyhow!(e))? {
            bail!(
                "remote tmux session `{session}` already exists on {} — pick a new task id, \
                 or kill it with `ssh {} tmux kill-session -t {session}`",
                machine.name,
                machine.name
            );
        }
        shelbi_tmux::new_session(&host, &session, &window_name, None)
            .map_err(|e| anyhow!(e))
            .context("creating remote workspace session")?;
        TmuxAddr {
            session: session.clone(),
            window: window_name.clone(),
        }
    };

    // 4. Launch the agent in the now-interactive shell. `exec` replaces the
    //    shell so the window closes naturally when the agent exits.
    //
    //    Local: tmux server inherits the user's already-set-up login env
    //    (since they ran shelbi from a terminal), so a plain `exec` finds
    //    everything the user has on PATH.
    //
    //    Remote: tmux server was started by `ssh host -- tmux new-session …`,
    //    which runs through a NON-login non-interactive shell — so tmux
    //    (and every pane it spawns) inherits a stripped-down PATH that's
    //    missing Homebrew, asdf, nvm, etc. Re-exec through `$SHELL -lc`
    //    so the login rc files (~/.zprofile, ~/.bash_profile) run and
    //    we pick up the same PATH the user has in their own terminal.
    let launch_cmd = shelbi_agent::launch_command(&runner_spec);
    let cd_launch = if host.is_local() {
        format!(
            "cd {} && exec {}",
            shelbi_agent::shell_escape(&worktree.to_string_lossy()),
            launch_cmd
        )
    } else {
        // Prefix with the hub-endpoint env (`SHELBI_HUB_ADDR`, plus the legacy
        // `SHELBI_HUB_SOCK` on Unix-forward hosts) so the agent's socket-write
        // paragraph (see agents/developer/instructions.md) can reach the hub
        // over whichever transport the reverse forward settled on — Unix socket
        // or the TCP loopback fallback used for Tailscale-SSH hosts. Without it
        // the agent's instructions short-circuit and worker→hub events are
        // dropped (per Phase 5's accepted residual risk).
        let hub_env = shelbi_orchestrator::workspace::remote_hub_env_prefix(&host);
        format!(
            "cd {} && {}exec \"${{SHELL:-/bin/bash}}\" -lc {}",
            shelbi_agent::shell_escape(&worktree.to_string_lossy()),
            hub_env,
            shelbi_agent::shell_escape(&launch_cmd),
        )
    };
    shelbi_tmux::send_line(&host, &addr, &cd_launch)
        .map_err(|e| anyhow!(e))
        .context("launching agent")?;

    // 5. Claude must draw its structural input box before we type. A fixed
    //    delay can land the prompt in a slow startup screen or trust dialog;
    //    once the later empty box appears, that lost text can look falsely
    //    submitted. Non-Claude runners have no supported pane parser, so they
    //    retain the conservative startup settle and explicit unverified
    //    delivery verdict.
    let submit_profile = shelbi_orchestrator::submit::SubmitProfile::for_runner(&runner_spec);
    if submit_profile.uses_claude_ui() {
        let ready = match shelbi_orchestrator::ready::wait_for_claude_ready(
            &host,
            &addr,
            shelbi_orchestrator::ready::READY_TIMEOUT,
        ) {
            Ok(ready) => ready,
            Err(probe_error) => {
                shelbi_state::append_send_event(
                    &project.name,
                    &args.id,
                    "stuck",
                    "readiness_probe_error",
                )
                .map_err(|log_error| {
                    anyhow!(
                        "waiting for Claude input readiness failed ({probe_error}); recording the stuck delivery also failed: {log_error}"
                    )
                })?;
                return Err(anyhow!(
                    "waiting for Claude input readiness failed: {probe_error}; prompt was not sent and the failure was recorded in events.log"
                ));
            }
        };
        if !ready {
            shelbi_state::append_send_event(
                &project.name,
                &args.id,
                "stuck",
                "readiness_timeout",
            )
            .map_err(|e| {
                anyhow!(
                    "Claude input readiness timed out; recording the stuck delivery also failed: {e}"
                )
            })?;
            bail!(
                "Claude input readiness timed out after {}s on {}; prompt was not sent and the failure was recorded in events.log",
                shelbi_orchestrator::ready::READY_TIMEOUT.as_secs(),
                addr.target(),
            );
        }
    } else {
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    super::send::send_verified(
        &project.name,
        &args.id,
        &runner_spec,
        &host,
        &addr,
        &args.prompt,
    )
    .context("sending initial prompt")?;

    // 5. Write the agent state file.
    let now = Utc::now();
    let agent = Agent {
        id: args.id.clone(),
        project: project.name.clone(),
        machine: machine.name.clone(),
        runner: args.runner.clone(),
        branch: branch.clone(),
        worktree: worktree.clone(),
        status: Status::Running,
        created: now,
        updated: now,
        tmux: TmuxAddr {
            session: session.clone(),
            window: window_name.clone(),
        },
    };
    let body = format!(
        "# Task\n\n{}\n\n## Progress\n\n- spawned at {}\n",
        args.prompt,
        now.to_rfc3339()
    );
    shelbi_state::save_agent(&project.name, &agent, &body).map_err(|e| anyhow!(e))?;
    shelbi_state::append_log(&project.name, &args.id, &format!("spawn: {}", args.prompt))
        .map_err(|e| anyhow!(e))?;

    println!("✓ agent {} spawned at {}", args.id, addr.target());
    Ok(())
}

fn worktree_path(machine: &Machine, id: &str) -> PathBuf {
    machine.work_dir.join(".shelbi").join("wt").join(id)
}

fn expand_tilde(p: &std::path::Path) -> PathBuf {
    if let Some(stripped) = p.to_str().and_then(|s| s.strip_prefix("~/")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    p.to_path_buf()
}

/// Add shelbi's footprint to the repo's `.gitignore` if it isn't already
/// covered: `.shelbi/` (metadata) plus the fixed `.claude/` files shelbi
/// deploys into the worktree on every dispatch (settings.json,
/// agent-instructions.md, the ready marker). Shelbi's *skill* mounts are
/// intentionally NOT claimed here — they're excluded per-name in the
/// worktree's git exclude at mount time, so a user's own `.claude/skills/`
/// stays under their git control (see `refresh_agent_skills`). Writes to the
/// file on the workspace's filesystem via `sh -c`; never commits.
fn ensure_gitignored(host: &Host, machine: &Machine) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    // Probe each footprint independently so a repo that already ignores
    // `.shelbi/` (but not the shelbi-written `.claude/` files) still gets the
    // `.claude/` block appended. `git check-ignore` exits 0 if the path is
    // ignored, 1 if not, 128 on error.
    let mut snippet = String::new();
    if !check_ignored(host, &repo, ".shelbi/")? {
        snippet.push_str(
            "\n# shelbi worktrees + metadata (https://github.com/jlong/shelbi)\n.shelbi/\n",
        );
    }
    // Representative probe: if the ready marker isn't ignored, none of the
    // shelbi-written `.claude/` files are, so append the whole block.
    //
    // NB: we deliberately do NOT blanket-claim `.claude/skills/` here — that
    // would hide (and, historically, let Shelbi destroy) user-authored skills.
    // Shelbi's own skill mounts are gitignored per-name in the worktree's git
    // exclude at mount time (see `refresh_agent_skills` /
    // `deploy_orchestrator_system_skill`), so a user's `.claude/skills/` stays
    // fully under their own git control.
    if !check_ignored(host, &repo, ".claude/shelbi-ready")? {
        snippet.push_str(
            "\n# shelbi deploy footprint written into the worktree on dispatch\n\
             .claude/settings.json\n\
             .claude/agent-instructions.md\n\
             .claude/shelbi-ready\n",
        );
    }
    if snippet.is_empty() {
        return Ok(());
    }
    let gitignore = format!("{repo}/.gitignore");
    // Append via `sh -c` so the redirect works locally and over SSH.
    let cmd = format!(
        "printf '%s' {} >> {}",
        shelbi_agent::shell_escape(&snippet),
        shelbi_agent::shell_escape(&gitignore),
    );
    shelbi_ssh::run_capture(host, ["sh", "-c", &cmd]).map_err(|e| anyhow!(e))?;
    Ok(())
}

/// `git check-ignore -q <path>` → true when the path is ignored. Exit 0 =
/// ignored, 1 = not ignored, 128 = error (treated as "not ignored" so a
/// probe failure just appends the snippet rather than blocking the spawn).
fn check_ignored(host: &Host, repo: &str, path: &str) -> Result<bool> {
    let probe = shelbi_ssh::run(host, ["git", "-C", repo, "check-ignore", "-q", path])
        .map_err(|e| anyhow!(e))?;
    Ok(probe.status.success())
}

fn create_worktree(
    host: &Host,
    machine: &Machine,
    branch: &str,
    worktree: &std::path::Path,
    project: &Project,
) -> Result<()> {
    let repo_dir = machine.work_dir.to_string_lossy().into_owned();
    let wt_str = worktree.to_string_lossy().into_owned();
    let parent_branch = project.base_branch().to_string();

    // Check if branch already exists locally. If yes, attach the worktree to it;
    // if not, create it from the default branch.
    let branch_exists = shelbi_ssh::run(
        host,
        ["git", "-C", &repo_dir, "rev-parse", "--verify", branch],
    )
    .map_err(|e| anyhow!(e))?
    .status
    .success();

    let mut args: Vec<String> = vec![
        "git".into(),
        "-C".into(),
        repo_dir.clone(),
        "worktree".into(),
        "add".into(),
    ];
    if branch_exists {
        args.push(wt_str.clone());
        args.push(branch.into());
    } else {
        // Cut the fresh branch from an up-to-date base: freshen against
        // `origin/<base>` when the base tracks a remote (see `resolve_cut_base`),
        // otherwise fall back to the local ref unchanged.
        let base = resolve_cut_base(host, &repo_dir, &parent_branch);
        // When we resolved to `origin/<base>`, suppress the automatic upstream
        // git would otherwise set (it'd point the task branch at `origin/<base>`
        // — wrong; the branch pushes to its own `origin/<branch>` later). The
        // local-ref fallback keeps the pre-existing tracking behavior.
        if base != parent_branch {
            args.push("--no-track".into());
        }
        args.push("-b".into());
        args.push(branch.into());
        args.push(wt_str.clone());
        args.push(base);
    }

    // Route through the shared transport-loss recovery: a `worktree add` that
    // loses the managed ControlMaster mid-checkout (large repos take seconds)
    // cleans the partial worktree and retries once on a fresh non-multiplexed
    // connection rather than failing the spawn on a transient mux drop.
    let output =
        shelbi_orchestrator::workspace::worktree_add_with_recovery(host, &repo_dir, &wt_str, &args)
            .map_err(|e| anyhow!(e))?;
    if !output.status.success() {
        bail!(
            "git worktree add failed: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Resolve the ref a brand-new task branch should be cut from, freshening the
/// base against its remote first when it has one.
///
/// The hub's local base ref (e.g. `main`) drifts behind `origin/main` as other
/// work merges — nothing advances the local ref between dispatches. Cutting a
/// new branch from that stale local ref silently bases the task on old code,
/// forcing needless rebases/conflicts at merge time. So when `base` has a
/// remote-tracking counterpart (`refs/remotes/origin/<base>`), we best-effort
/// `git fetch origin <base>` and cut from `origin/<base>` — the freshest remote
/// state — rather than the local ref.
///
/// Basing on `origin/<base>` also sidesteps the "base is checked out in the
/// hub's primary worktree" problem: git refuses to attach two worktrees to the
/// same local branch, but any number of worktrees can branch off the
/// remote-tracking ref.
///
/// Two deliberate fallbacks to the local `base` ref:
/// - **No remote counterpart** — the subtask/umbrella workflows
///   (`app-feature-subtask`, `site-update-subtask`) base on a local-only
///   integration branch (`task/<id>`) that was never pushed, and local-only
///   projects / test fixtures have no `origin` at all. Either way there is no
///   `origin/<base>`, so we branch from the local ref and never touch origin.
/// - **Best-effort fetch failure** — offline, or the remote branch vanished. We
///   warn and cut from whatever `origin/<base>` last resolved to (no worse than
///   the local ref) rather than failing the dispatch.
fn resolve_cut_base(host: &Host, repo: &str, base: &str) -> String {
    // A base with no remote-tracking counterpart is a local-only integration
    // branch (subtask/umbrella) or a repo with no `origin`. No fresher remote
    // truth exists — cut from the local ref, and don't fetch.
    let remote_ref = format!("refs/remotes/origin/{base}");
    let has_remote_counterpart = shelbi_ssh::run(
        host,
        ["git", "-C", repo, "rev-parse", "--verify", "--quiet", &remote_ref],
    )
    .map(|out| out.status.success())
    .unwrap_or(false);
    if !has_remote_counterpart {
        return base.to_string();
    }

    // Freshen the remote-tracking ref, best-effort. A failure (offline, deleted
    // remote branch) warns and proceeds from the last-known `origin/<base>` —
    // never a hard dispatch failure. `run_resilient` rides out a transient
    // managed-ControlMaster drop before giving up.
    match shelbi_ssh::run_resilient(host, ["git", "-C", repo, "fetch", "origin", base]) {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "shelbi: `git fetch origin {base}` failed in {repo}; cutting the new branch \
             from the last-known origin/{base}: {}",
            shelbi_ssh::describe_failure(host, &out)
        ),
        Err(e) => eprintln!(
            "shelbi: `git fetch origin {base}` errored in {repo}; cutting the new branch \
             from the last-known origin/{base}: {e}"
        ),
    }
    format!("origin/{base}")
}

#[cfg(test)]
mod cut_base_tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    /// Run `git <args>` in `dir` with a deterministic identity so commits work
    /// in a bare CI environment. Panics on spawn failure; returns the output.
    fn git(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git")
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = git(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `git rev-parse <rev>` in `dir`, trimmed. Empty string if it doesn't resolve.
    fn rev(dir: &Path, r: &str) -> String {
        let out = git(dir, &["rev-parse", r]);
        if out.status.success() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            String::new()
        }
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Build an `origin` bare repo with a `main` commit and a `hub` clone of it.
    /// Returns (origin_path, hub_path). The `_root` guard keeps the tempdir alive.
    fn seed_repo(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let seed = root.join("seed");
        let origin = root.join("origin.git");
        let hub = root.join("hub");
        std::fs::create_dir_all(&seed).unwrap();

        git_ok(root, &["init", "--bare", "-b", "main", origin.to_str().unwrap()]);
        git_ok(&seed, &["init", "-b", "main"]);
        std::fs::write(seed.join("README.md"), "seed\n").unwrap();
        git_ok(&seed, &["add", "."]);
        git_ok(&seed, &["commit", "-m", "seed"]);
        git_ok(&seed, &["remote", "add", "origin", origin.to_str().unwrap()]);
        git_ok(&seed, &["push", "origin", "main"]);
        git_ok(root, &["clone", origin.to_str().unwrap(), hub.to_str().unwrap()]);
        (origin, hub)
    }

    #[test]
    fn cuts_from_freshened_origin_when_base_tracks_a_remote() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let (origin, hub) = seed_repo(tmp.path());

        // Advance origin from a second working clone so the hub's local `main`
        // and `origin/main` both go stale relative to the true remote tip.
        let other = tmp.path().join("other");
        git_ok(
            tmp.path(),
            &["clone", origin.to_str().unwrap(), other.to_str().unwrap()],
        );
        std::fs::write(other.join("f2.txt"), "more\n").unwrap();
        git_ok(&other, &["add", "."]);
        git_ok(&other, &["commit", "-m", "advance"]);
        git_ok(&other, &["push", "origin", "main"]);
        let remote_tip = rev(&other, "HEAD");

        // Before: the hub's origin/main is behind the true remote tip.
        assert_ne!(rev(&hub, "refs/remotes/origin/main"), remote_tip);

        let base = resolve_cut_base(&Host::Local, hub.to_str().unwrap(), "main");
        assert_eq!(base, "origin/main");
        // The best-effort fetch ran: hub's origin/main now matches the remote tip.
        assert_eq!(rev(&hub, "refs/remotes/origin/main"), remote_tip);
    }

    #[test]
    fn falls_back_to_local_ref_when_base_has_no_remote_counterpart() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let (_origin, hub) = seed_repo(tmp.path());

        // A local-only integration branch (the subtask/umbrella case): it exists
        // locally but has no `origin/<base>`.
        git_ok(&hub, &["branch", "task/add-auth", "main"]);

        let base = resolve_cut_base(&Host::Local, hub.to_str().unwrap(), "task/add-auth");
        // Unchanged local ref — and no spurious origin ref was created.
        assert_eq!(base, "task/add-auth");
        assert_eq!(rev(&hub, "refs/remotes/origin/task/add-auth"), "");
    }

    #[test]
    fn falls_back_to_local_ref_when_repo_has_no_origin() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("solo");
        std::fs::create_dir_all(&repo).unwrap();
        git_ok(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "solo\n").unwrap();
        git_ok(&repo, &["add", "."]);
        git_ok(&repo, &["commit", "-m", "init"]);

        let base = resolve_cut_base(&Host::Local, repo.to_str().unwrap(), "main");
        assert_eq!(base, "main");
    }

    #[test]
    fn best_effort_fetch_failure_still_cuts_from_last_known_origin() {
        if !git_available() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let (_origin, hub) = seed_repo(tmp.path());
        // origin/main is present from the clone; break the remote so the fetch
        // fails (offline / vanished remote).
        git_ok(
            &hub,
            &["remote", "set-url", "origin", "/no/such/path.git"],
        );
        let before = rev(&hub, "refs/remotes/origin/main");

        let base = resolve_cut_base(&Host::Local, hub.to_str().unwrap(), "main");
        // Warned + proceeded from the last-known origin/main rather than erroring.
        assert_eq!(base, "origin/main");
        assert_eq!(rev(&hub, "refs/remotes/origin/main"), before);
    }
}
