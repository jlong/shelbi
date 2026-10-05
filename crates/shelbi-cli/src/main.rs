use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod commands;
mod issue_tracker_setup;
mod project_root;
mod wizard;

#[derive(Debug, Parser)]
#[command(
    name = "shelbi",
    version,
    // We define our own `--version` flag below so we can bind the short `-v`
    // to it; disable the auto-generated one to avoid a duplicate flag.
    disable_version_flag = true,
    about = "Open-source agent orchestrator for the terminal",
    long_about = None,
)]
struct Cli {
    /// Print version information and exit.
    //
    // Heads-up for a future `--verbose` author: `-v` is conventionally
    // "verbose" in many CLIs, and clap's own default version short is `-V`
    // (uppercase). The user explicitly asked for `-v` -> version, so it lives
    // here (with `-V` kept as an alias). If you add `--verbose`, you'll need
    // to move version off `-v` to resolve the collision.
    #[arg(
        short = 'v',
        short_alias = 'V',
        long = "version",
        action = clap::ArgAction::Version,
    )]
    version: (),

    /// Override the shelbi root directory (default: baked at install time;
    /// also overridable via $SHELBI_ROOT). The flag wins over both env vars
    /// and the compile-time default; `~/.shelbi` is the final fallback. With
    /// `init`, place this before the subcommand; `init --root PATH` names the
    /// project root instead.
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<std::path::PathBuf>,

    /// Project to operate on. Defaults to the project named in $SHELBI_PROJECT
    /// or the registered project whose work_dir contains the current
    /// directory (matched against ~/.shelbi/projects/*.yaml).
    #[arg(long, short = 'p', global = true, env = "SHELBI_PROJECT")]
    project: Option<String>,

    /// Accept the detected `shelbi init` plan without prompts. For other
    /// commands, assume "yes" when an optional confirmation supports it.
    #[arg(long, short = 'y', global = true)]
    yes: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Print the orchestrator's bootstrap snapshot. Bare `shelbi status`
    /// emits a concise human summary; `--full` emits the LLM-consumable
    /// payload (board + workspaces + zen + handoff-presence);
    /// `--handoff` prints `HANDOFF.md` from the project's work_dir and
    /// deletes it. Both flags compose. The legacy `list` subcommand
    /// still prints the project-wide status catalogue.
    Status {
        #[command(subcommand)]
        cmd: Option<commands::status::StatusCmd>,
        /// Emit the full sectioned bootstrap payload (board, workspaces,
        /// zen, handoff-presence). Idempotent — safe to re-run.
        #[arg(long)]
        full: bool,
        /// Print the contents of `HANDOFF.md` from the project's local
        /// `work_dir` and delete the file. No-op when absent.
        /// Destructive; separated from `--full` so bootstrap snapshots
        /// stay safe to re-run.
        #[arg(long)]
        handoff: bool,
    },
    /// Send a follow-up message to a running workspace (or legacy spawn
    /// agent). Resolves NAME against the project YAML's `workspaces:`
    /// block first, then falls back to the legacy spawn agent registry.
    Send { id: String, message: String },
    /// Push a message to a task's workspace via the file-based message log
    /// (`<worktree>/.shelbi/messages/<task-id>.log`). Distinct from `send`:
    /// `send` injects keystrokes into the agent session; `message` appends a
    /// durable JSON record the workspace tails and (best-effort) acks.
    ///
    /// The push being durable is NOT the same as the worker having read it:
    /// use `--wait` (blocks, exits non-zero if the ack window elapses) or
    /// `shelbi message status <msg-id>` to learn the real delivery outcome.
    #[command(args_conflicts_with_subcommands = true)]
    Message {
        /// Query a previously pushed message's delivery status instead of
        /// sending. `shelbi message status <msg-id>`.
        #[command(subcommand)]
        status: Option<commands::message::MessageStatusCmd>,
        /// Issue id whose assigned workspace receives the message.
        id: Option<String>,
        /// Message kind.
        #[arg(value_enum)]
        kind: Option<commands::message::MessageKind>,
        /// Message body.
        body: Option<String>,
        /// Question id this message replies to (sets `in_response_to`).
        /// Typically paired with `kind = reply`.
        #[arg(long = "in-response-to", value_name = "QUESTION-ID")]
        in_response_to: Option<String>,
        /// Block until the worker confirms delivery (an `ack=worker` event),
        /// polling the events stream. Optional value overrides the wait
        /// window in seconds (default 120). Exits non-zero if the window
        /// elapses without a confirmation — so a non-interactive caller can
        /// tell "delivered" from "queued but never read".
        #[arg(long, value_name = "SECS", num_args = 0..=1, default_missing_value = "120")]
        wait: Option<u64>,
    },
    /// Ensure a workspace's session is up so the TUI can show it: a no-op for a
    /// workspace mid-task (dispatch owns its agent session), or a plain
    /// interactive login shell in the worktree for an idle workspace.
    Open {
        /// Name of the workspace to open.
        name: String,
    },
    /// Manage the project's Kanban issue board.
    Issue {
        #[command(subcommand)]
        cmd: commands::issue::IssueCmd,
    },
    /// Deprecated alias for `issue`. Prints a deprecation notice and forwards
    /// to `shelbi issue`. Kept so existing scripts and muscle memory keep
    /// working through one release; prefer `shelbi issue`.
    #[command(hide = true)]
    Task {
        #[command(subcommand)]
        cmd: commands::issue::IssueCmd,
    },
    /// Operate on the issue-tracker backend itself (e.g. `migrate` between
    /// `file_system` and `github`).
    IssueStore {
        #[command(subcommand)]
        cmd: commands::issue_store::IssueStoreCmd,
    },
    /// Inspect and control the project's declared workspace pool.
    Workspace {
        #[command(subcommand)]
        cmd: commands::workspace::WorkspaceCmd,
    },
    /// Deprecated alias for `shelbi workspace`. Will be removed in a future
    /// release — see the stderr nag emitted on invocation.
    #[command(hide = true)]
    Worker {
        #[command(subcommand)]
        cmd: commands::workspace::WorkspaceCmd,
    },
    /// Inspect and manage the project's `agents/<name>/` workspaces:
    /// `list`, `show`, `new`, `edit`.
    Agent {
        #[command(subcommand)]
        cmd: commands::agent::AgentCmd,
    },
    /// Find or install a compatible `shelbi` binary on the project's remote
    /// machines: `setup <name>`, `status [<name>]`.
    Machine {
        #[command(subcommand)]
        cmd: commands::machine::MachineCmd,
    },
    /// Manage the project's workflow definitions (status sets).
    Workflow {
        #[command(subcommand)]
        cmd: commands::workflow::WorkflowCmd,
    },
    /// Manage projects (add, ...).
    Project {
        #[command(subcommand)]
        cmd: commands::project::ProjectCmd,
    },
    /// Discover and validate Shelbi-owned configuration. The existing
    /// keybinding commands remain available alongside `inventory` and `lint`.
    Config {
        #[command(subcommand)]
        cmd: commands::config::ConfigCmd,
    },
    /// Runtime health checks for the project's GitHub API budget: the observed
    /// request rate, remaining budget, projected time-to-exhaustion, and a
    /// warning (naming the top callers) when a budget would run out within 30
    /// minutes at the current rate.
    Doctor,
    /// Inspect the hub-global workspace-state transition log.
    Events {
        #[command(subcommand)]
        cmd: commands::events::EventsCmd,
    },
    /// Run the hub-side daemon that listens on `~/.shelbi/hub.sock`
    /// (overridable via `$SHELBI_HUB_SOCK`) for worker messages and
    /// appends `event`-verb payloads to `~/.shelbi/events.log`. Bare
    /// `shelbi daemon` (no subcommand) is the foreground entry that
    /// launchd/systemd call into. The `install`/`uninstall`/`status`/
    /// `restart` subcommands manage that platform supervisor on the
    /// user's behalf.
    Daemon {
        #[command(subcommand)]
        cmd: Option<commands::daemon::DaemonCmd>,
    },
    /// Attach this terminal to a workspace's session, rendered full-screen.
    /// Detach with the configured key (default Ctrl+]).
    Attach {
        /// Name of the workspace (session) to attach to.
        workspace: String,
        /// Key that detaches from the session. Default `ctrl-]`.
        #[arg(long, default_value = "ctrl-]")]
        detach_key: String,
    },
    /// Initialize a Shelbi project. `shelbi init -y` detects the repository,
    /// runner, and workspace plan, then scaffolds it without prompts.
    /// If both Claude and Codex are installed, add `--runner claude` or
    /// `--runner codex`.
    ///
    /// Without `-y`, Shelbi preserves the legacy config-location flow and
    /// offers a choice of *global* mode (config lives at
    /// ~/.shelbi/projects/<name>.yaml) or *in-repo* mode (shared config
    /// committed at <repo>/.shelbi/project.yaml so teammates get it on
    /// clone). Pass `--pick-up` on a cloned repo carrying an existing
    /// in-repo config to register it into your local registry. See
    /// `site/content/docs/concepts/config-modes.mdx` for the full
    /// on-disk layout, migration, and pick-up worked example.
    Init(commands::init::Args),
    /// Run the onboarding wizard. Walks through project setup (auto-filled
    /// from the current git checkout when present) and writes
    /// ~/.shelbi/projects/<name>.yaml. Idempotent — setup is skipped when a
    /// project is already on disk.
    Wizard,
    /// Start the orchestrator agent as the project's orchestrator session.
    Orchestrate(commands::orchestrate::Args),
    /// Machine-readable orchestrator transport primitives.
    Orchestrator {
        #[command(subcommand)]
        cmd: commands::orchestrator::OrchestratorCmd,
    },
    /// Respawn the shelbi-owned panes (sidebar + tasks/machines)
    /// AND the orchestrator pane in place so a freshly installed binary
    /// takes effect — and edits to the orchestrator's instructions /
    /// preamble land without a manual tear-down. The previous
    /// orchestrator is asked to write `agents/orchestrator/handoff.md`
    /// covering its in-flight state; the new instance ingests that
    /// file (then deletes it), so reload carries the orchestrator's
    /// mid-thought context forward. Workspace panes are left alone —
    /// they re-shell into shelbi on every call and pick up the new
    /// binary automatically.
    ///
    /// Pass a target to reload just one part in place without bouncing
    /// the whole hub: `chat` (the orchestrator pane — respawned with its
    /// handoff carried forward), `tasks`, `activity`, `sidebar`, or
    /// `workspace <name>` for a single worker pane. Omitting the target
    /// (or `all`) is the whole-hub reload above.
    Reload {
        /// What to reload: chat, tasks, activity, sidebar, workspace, or
        /// all (default). Omit for the whole hub.
        #[arg(value_name = "TARGET")]
        target: Option<String>,
        /// Workspace name — required when TARGET is `workspace`.
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// (internal) Own the Codex app-server, exact orchestrator thread, and
    /// remote TUI for one project. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__codex-orchestrator")]
    CodexOrchestrator {
        project: String,
        /// Carry the already-claimed first-project welcome into the native
        /// bridge. Hidden with the internal command and never set by users.
        #[arg(long, hide = true)]
        first_launch: bool,
    },
    /// Toggle Zen Mode or run its primitives. `shelbi zen on/off/pause` flip
    /// the trust boundary for auto-promotion and exact-provenance auto-merge.
    /// `probe` reports facts about a finished branch (checks,
    /// conflict, diff size, danger paths). `pr-create/ci-watch/pr-merge` are
    /// single-purpose PR primitives the orchestrator sequences per its
    /// Merge Conditions prompt policy.
    Zen {
        #[command(subcommand)]
        cmd: commands::zen::ZenCmd,
    },
    /// Build the workspace on the declared minimum Rust version (the
    /// `rust-version` in Cargo.toml), mirroring CI's `msrv` job
    /// (`cargo +<rust-version> check --workspace --all-targets --locked`). Meant
    /// as a `zen.checks.local` entry on Rust tracks so an MSRV break (a lockfile
    /// bump pulling in a dependency that needs newer Rust) is caught before
    /// handoff instead of only in CI. Reads the toolchain from the nearest
    /// Cargo.toml, installs it once if missing, and skips cleanly (exit 0) when
    /// the toolchain can't be provisioned rather than hard-failing.
    #[command(name = "msrv-check")]
    MsrvCheck,
    /// Manage the hub checkout's context-scoped default-branch commit guard
    /// (the Shelbi-managed `pre-commit` hook). `install`/`uninstall`/`status`.
    /// The hook only blocks commits from inside a Shelbi-managed agent pane —
    /// your own shell is never governed.
    Guard {
        #[command(subcommand)]
        cmd: commands::guard::GuardCmd,
    },
    /// Run a single-purpose workflow action primitive. `push-branch`,
    /// `open-pr`, `merge`, `close-pr`, `delete-branch`, and `restack`
    /// are the git/gh primitives the workflow `transitions:` block can
    /// sequence — each is idempotent and silently no-ops when there's
    /// nothing to do. `merge` also auto-fires `restack` on every
    /// not-`Done` child that depends on the merging task.
    Action {
        #[command(subcommand)]
        cmd: commands::action::ActionCmd,
    },
    /// (internal) Crash-recovery check the orchestrator pane wrapper
    /// runs once at start. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__zen-orch-start")]
    ZenOrchStart { project: String },
    /// (internal) Per-tick heartbeat refresh from the orchestrator pane
    /// wrapper's background loop. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__zen-heartbeat")]
    ZenHeartbeat { project: String },
    /// (internal) Start the on-demand hub daemon if it isn't already running,
    /// waiting for its socket. The CLI/TUI open paths call the same helper; this
    /// exposes it for ops and tests. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__ensure-daemon")]
    EnsureDaemon,
    /// (internal) Graceful-exit clear the orchestrator pane wrapper
    /// runs after the agent returns. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__zen-orch-exit")]
    ZenOrchExit { project: String },
    /// (internal) Best-effort crash-record capture the orchestrator pane
    /// wrapper runs on every exit path (agent return + SIGHUP trap). Writes a
    /// post-mortem only for a genuine crash. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__orch-record-exit")]
    OrchRecordExit {
        project: String,
        /// Short exit token — `exit:<code>` or `signal:SIG<NAME>`.
        reason: String,
        /// Reserved; the orchestrator runs as a session now, so no pane id is
        /// captured. Absent/empty yields a record with no output tail.
        pane: Option<String>,
    },
    /// (internal) Launch wrapper the Review agent runs in place of a workflow
    /// `review:` serve command. Starts the command in its own session /
    /// process group and records its pgid to `$SHELBI_REVIEW_PGID_FILE` so any
    /// teardown path can reap the whole server tree (server + grandchildren)
    /// instead of orphaning it to launchd. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__review-serve")]
    ReviewServe {
        /// The serve command and its args — everything after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        cmd: Vec<String>,
    },
    /// (internal) The per-session process (remove-tmux backend): own one PTY
    /// and one headless terminal emulator, answer terminal queries with no
    /// client attached, serve clients on a Unix socket, and on child exit write
    /// `exit.json` + `final.txt`. Normally launched detached by a client via
    /// `shelbi_session::spawn_detached`, never run by hand. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__session")]
    SessionProcess(commands::session::Args),
    /// (internal) Bridge one stdio channel to every session on this machine
    /// (remove-tmux remote backend). Started by the hub over `ssh <host> shelbi
    /// relay`; it reads/writes the relay protocol on stdin/stdout and holds no
    /// session state. Not for direct use.
    #[command(hide = true)]
    Relay(commands::relay::Args),
    /// Inspect and drive the session backend directly: `ls`, `new`, `kill`,
    /// `send`, `snapshot`. A debug surface over `shelbi-client` /
    /// `shelbi-session` (`shelbi attach <workspace>` is the rendered client).
    Session {
        #[command(subcommand)]
        cmd: commands::session_cli::SessionCmd,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let init_has_project_root = matches!(cli.cmd.as_ref(), Some(Cmd::Init(_)))
        && init_project_root_was_explicit(std::env::args_os());
    // `init` has historically used `--root` for the project root while the
    // top-level CLI uses the same global spelling for Shelbi's state root.
    // clap exposes that shared value in both structs. Preserve conventional
    // scope: before `init` it is the global state root; after `init` it is the
    // project root. `$SHELBI_ROOT` can express a separate state root when the
    // project-root form is used.
    if !init_has_project_root {
        if let Some(root) = cli.root.clone() {
            // Stash before any helper reads the resolved root. `expand_tilde_*`
            // happens inside `resolve()` so the user can pass `~/scratch`.
            shelbi_state::set_root_override(root);
        }
    }
    if cli.yes {
        // Carried through the environment (like `--root` via the override
        // stash) so deep call sites — the daemon-restart offer in the
        // version gate — don't need `--yes` plumbed through every
        // subcommand's argument struct.
        std::env::set_var(commands::hub_version::ASSUME_YES_ENV, "1");
    }
    init_tracing(cli.cmd.as_ref());

    match cli.cmd {
        None => default_entry(cli.project.clone()),
        Some(Cmd::Status { cmd, full, handoff }) => {
            commands::status::run(cli.project, cmd, full, handoff)
        }
        Some(Cmd::Send { id, message }) => commands::send::run(cli.project, id, message),
        Some(Cmd::Message {
            status,
            id,
            kind,
            body,
            in_response_to,
            wait,
        }) => commands::message::run(cli.project, status, id, kind, body, in_response_to, wait),
        Some(Cmd::Open { name }) => commands::open::run(cli.project, name),
        Some(Cmd::Issue { cmd }) => commands::issue::run(cli.project, cmd),
        Some(Cmd::Task { cmd }) => {
            eprintln!(
                "warning: `shelbi task` is deprecated and will be removed in a future \
                 release — use `shelbi issue` instead."
            );
            commands::issue::run(cli.project, cmd)
        }
        Some(Cmd::IssueStore { cmd }) => commands::issue_store::run(cli.project, cmd),
        Some(Cmd::Workspace { cmd }) => commands::workspace::run(cli.project, cmd),
        Some(Cmd::Worker { cmd }) => {
            eprintln!("shelbi: 'worker' is deprecated; use 'workspace' instead.");
            commands::workspace::run(cli.project, cmd)
        }
        Some(Cmd::Agent { cmd }) => commands::agent::run(cli.project, cmd),
        Some(Cmd::Machine { cmd }) => commands::machine::run(cli.project, cmd),
        Some(Cmd::Workflow { cmd }) => commands::workflow::run(cli.project, cmd),
        Some(Cmd::Project { cmd }) => commands::project::run(cli.project, cmd),
        Some(Cmd::Config { cmd }) => {
            // `--project` carries `[env: SHELBI_PROJECT]`, so `cli.project` is
            // `Some` in every Shelbi session even without a CLI flag. `config`
            // needs to distinguish an env-derived project from an explicit one
            // so `--all` doesn't spuriously collide with the ambient value.
            let explicit_project = project_flag_was_explicit(std::env::args_os());
            commands::config::run(cli.project, explicit_project, cmd)
        }
        Some(Cmd::Doctor) => commands::doctor::run(cli.project),
        Some(Cmd::Events { cmd }) => commands::events::run(cmd),
        Some(Cmd::Daemon { cmd }) => commands::daemon::run(cmd),
        Some(Cmd::Zen { cmd }) => commands::zen::run(cli.project, cmd),
        Some(Cmd::MsrvCheck) => commands::msrv_check::run(),
        Some(Cmd::Guard { cmd }) => commands::guard::run(cli.project, cmd),
        Some(Cmd::Action { cmd }) => commands::action::run(cli.project, cmd),
        Some(Cmd::Attach {
            workspace,
            detach_key,
        }) => commands::session_cli::attach_workspace(cli.project, workspace, detach_key),
        Some(Cmd::Init(mut args)) => {
            if !init_has_project_root {
                // A global `--root` is propagated into the subcommand's field
                // by clap because both intentionally share the spelling.
                args.root = None;
            }
            commands::init::run(args, cli.yes)
        }
        Some(Cmd::Wizard) => commands::wizard::run(false).map(|_| ()),
        Some(Cmd::Orchestrate(args)) => commands::orchestrate::run(cli.project, args),
        Some(Cmd::Orchestrator { cmd }) => commands::orchestrator::run(cli.project, cmd),
        Some(Cmd::Reload { target, name }) => commands::reload::run(cli.project, target, name),
        Some(Cmd::CodexOrchestrator {
            project,
            first_launch,
        }) => {
            shelbi_orchestrator::wake::run_codex_bridge(&project, first_launch)
                .map_err(|e| anyhow::anyhow!(e.to_string()))
        }
        Some(Cmd::ReviewServe { cmd }) => commands::review_serve::run(cmd),
        Some(Cmd::SessionProcess(args)) => commands::session::run(args),
        Some(Cmd::Relay(args)) => commands::relay::run(args),
        Some(Cmd::Session { cmd }) => commands::session_cli::run(cli.project, cmd),
        Some(Cmd::ZenOrchStart { project }) => commands::zen_lifecycle::orch_start(&project),
        Some(Cmd::ZenHeartbeat { project }) => commands::zen_lifecycle::heartbeat(&project),
        Some(Cmd::EnsureDaemon) => {
            shelbi_state::ensure_daemon_running().map_err(|e| anyhow::anyhow!(e.to_string()))
        }
        Some(Cmd::ZenOrchExit { project }) => commands::zen_lifecycle::orch_exit(&project),
        Some(Cmd::OrchRecordExit {
            project,
            reason,
            pane,
        }) => commands::zen_lifecycle::orch_record_exit(
            &project,
            &reason,
            pane.as_deref().filter(|s| !s.is_empty()),
        ),
    }
}

/// Did `--project` / `-p` appear on the command line (as opposed to being
/// resolved from `$SHELBI_PROJECT`)? The top-level flag declares
/// `[env: SHELBI_PROJECT]`, so `cli.project` is `Some` in every Shelbi
/// session; commands that must treat an env-derived project differently from
/// an explicit one (currently `config`, for its `--all` mutual-exclusion
/// check) use this to tell them apart.
fn project_flag_was_explicit<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    args.into_iter().skip(1).any(|arg| {
        let arg = arg.as_ref().to_string_lossy();
        arg == "--project"
            || arg.starts_with("--project=")
            || arg == "-p"
            // Short flag with an attached value, e.g. `-pshelbi`. Guard against
            // long flags (`--…`) which never start a short group.
            || (arg.starts_with("-p") && !arg.starts_with("--"))
    })
}

fn init_project_root_was_explicit<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--root" | "--project" | "-p" => index += 2,
            "--yes" | "-y" => index += 1,
            "init" => {
                return args[index + 1..]
                    .iter()
                    .any(|arg| arg == "--root" || arg.starts_with("--root="));
            }
            _ => index += 1,
        }
    }
    false
}

/// `shelbi` with no subcommand. Dispatches based on what's on disk:
///
/// - `--project` / `SHELBI_PROJECT`, or a cwd inside a registered
///   project's `work_dir`, that resolves to a local registration → boot that
///   project's TUI.
/// - an explicit `--project` / `SHELBI_PROJECT` names a project with no
///   YAML on this machine → print a friendly note and fall through to
///   onboarding so the user can set up the project locally. We
///   deliberately re-derive the project name from the chosen root's
///   basename rather than re-using the missing name.
/// - cwd does not resolve to a registered project → onboarding for this cwd,
///   even when other projects already exist. The banner appears only when the
///   Shelbi home itself is new.
fn default_entry(explicit: Option<String>) -> Result<()> {
    let resolved = resolve_or_onboard(commands::require_project(explicit))?;
    let missing_project = match classify_default_route(resolved, project_registration_exists) {
        DefaultRoute::Dashboard(name) => {
            return shelbi_tui::run_main(&name).context("launching shelbi");
        }
        DefaultRoute::Onboarding { missing_project } => missing_project,
    };
    if let Some(name) = missing_project {
        eprintln!(
            "No local registration for project `{name}` on this machine — \
             let's set up a project here.\n"
        );
    }

    let home = shelbi_state::shelbi_home().map_err(|e| anyhow::anyhow!(e))?;
    let home_existed = home.exists();
    run_wizard_then_dispatch(!home_existed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DefaultRoute {
    Dashboard(String),
    Onboarding { missing_project: Option<String> },
}

fn classify_default_route<F>(resolved: Option<String>, is_registered: F) -> DefaultRoute
where
    F: FnOnce(&str) -> bool,
{
    match resolved {
        Some(name) if is_registered(&name) => DefaultRoute::Dashboard(name),
        missing_project => DefaultRoute::Onboarding { missing_project },
    }
}

/// Treat the ordinary "no project specified" miss as an onboarding route,
/// while preserving structured state errors. In particular, a cloned
/// in-repo project without its user-local registration must keep directing
/// the user to `shelbi init --pick-up`; silently replacing that config with a
/// new global project would corrupt the established config-mode workflow.
fn resolve_or_onboard(resolved: Result<String>) -> Result<Option<String>> {
    match resolved {
        Ok(name) => Ok(Some(name)),
        Err(error) if error.downcast_ref::<shelbi_core::Error>().is_some() => Err(error),
        Err(_) => Ok(None),
    }
}

/// Whether either supported local registration for `name` is on this
/// machine: the flat global YAML or an in-repo project's split local half.
/// This stays a path-only check so the configured-repository bypass does not
/// trigger the migrations performed by `load_project`.
fn project_registration_exists(name: &str) -> bool {
    if shelbi_core::validate_project_name(name).is_err() {
        return false;
    }
    match shelbi_state::projects_dir() {
        Ok(dir) => {
            dir.join(format!("{name}.yaml")).is_file()
                || dir.join(name).join("local.yaml").is_file()
        }
        Err(_) => false,
    }
}

/// Onboarding dispatcher when `default_entry` cannot resolve this cwd to a
/// locally registered project.
/// Prints the brand banner (only on a truly-fresh install), then runs the
/// shared detected-plan flow and launches the TUI only when that flow creates
/// a project.
///
/// `first_run` is true when `~/.shelbi/` did not exist before this
/// invocation; the banner only prints in that case.
///
/// Cancellation (`Ctrl-C` / `Esc`) and the plan card's explicit `q` both
/// resolve to `SetupOutcome::Quit` and exit cleanly without launching.
fn run_wizard_then_dispatch(first_run: bool) -> Result<()> {
    if first_run {
        wizard::print_banner();
    }
    commands::wizard::run_one_project_and_launch()
}

/// Initialize the tracing subscriber.
///
/// For the single-process TUI (bare `shelbi`) and the internal
/// `__codex-orchestrator` process we route output to `~/.shelbi/logs/tui.log`
/// instead of stderr. The process shares its TTY with ratatui's draw cycle, and
/// any stray stderr write corrupts the cursor position — leaving raw `tracing`
/// lines bleeding across the screen until the next full repaint (e.g. a
/// resize). For all other commands the default stderr writer is fine.
fn init_tracing(cmd: Option<&Cmd>) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("SHELBI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let is_tui = matches!(cmd, None | Some(Cmd::CodexOrchestrator { .. }));
    if is_tui {
        if let Some(file) = open_tui_log_file() {
            let _ = fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .try_init();
        } else {
            // Couldn't open the log file. Sink to nowhere rather than stderr
            // — silence is strictly better than bleeding onto the TUI.
            let _ = fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_writer(std::io::sink)
                .try_init();
        }
    } else {
        let _ = fmt().with_env_filter(filter).with_target(false).try_init();
    }
}

fn open_tui_log_file() -> Option<std::fs::File> {
    let home = shelbi_state::shelbi_home().ok()?;
    let dir = home.join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("tui.log"))
        .ok()
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::Parser;
    use commands::workspace::WorkspaceCmd;

    #[test]
    fn init_yes_parses_every_detected_plan_override() {
        let cli = Cli::parse_from([
            "shelbi",
            "init",
            "-y",
            "--project",
            "demo",
            "--root",
            "/tmp/demo",
            "--runner",
            "codex",
            "--default-branch",
            "develop",
            "--github-url",
            "https://github.com/example/demo.git",
            "--orchestrator-runner",
            "claude",
            "--issue-tracker",
            "github",
            "--github-repo",
            "example/demo",
        ]);
        assert!(cli.yes);
        assert_eq!(cli.root.as_deref(), Some(std::path::Path::new("/tmp/demo")));
        assert!(init_project_root_was_explicit([
            "shelbi",
            "init",
            "--root",
            "/tmp/demo"
        ]));
        let Some(Cmd::Init(args)) = cli.cmd else {
            panic!("expected init command");
        };
        assert_eq!(args.project.as_deref(), Some("demo"));
        assert_eq!(
            args.root.as_deref(),
            Some(std::path::Path::new("/tmp/demo"))
        );
        assert_eq!(args.runner, Some(wizard::Runner::Codex));
        assert_eq!(args.default_branch.as_deref(), Some("develop"));
        assert_eq!(
            args.github_url.as_deref(),
            Some("https://github.com/example/demo.git")
        );
        assert_eq!(args.orchestrator_runner, Some(wizard::Runner::Claude));
        assert_eq!(
            args.issue_tracker,
            Some(commands::init::IssueTrackerArg::Github)
        );
        assert_eq!(args.github_repo.as_deref(), Some("example/demo"));
    }

    #[test]
    fn init_root_scope_distinguishes_global_state_from_project_root() {
        assert!(!init_project_root_was_explicit([
            "shelbi", "--root", "/tmp/state", "init", "-y"
        ]));
        assert!(!init_project_root_was_explicit([
            "shelbi",
            "--project",
            "init",
            "init",
            "-y"
        ]));
        assert!(init_project_root_was_explicit([
            "shelbi",
            "init",
            "-y",
            "--root=/tmp/project"
        ]));
    }

    #[test]
    fn project_flag_explicit_only_for_cli_forms() {
        // Explicit on the command line, in every spelling.
        assert!(project_flag_was_explicit([
            "shelbi", "config", "lint", "--project", "demo"
        ]));
        assert!(project_flag_was_explicit([
            "shelbi",
            "config",
            "lint",
            "--project=demo"
        ]));
        assert!(project_flag_was_explicit(["shelbi", "config", "lint", "-p", "demo"]));
        assert!(project_flag_was_explicit(["shelbi", "config", "lint", "-pdemo"]));
        // No `--project` on the line → env-derived, treated as not explicit.
        assert!(!project_flag_was_explicit(["shelbi", "config", "lint", "--all"]));
        assert!(!project_flag_was_explicit(["shelbi", "config", "inventory"]));
        // A long flag that merely shares the `--p…` prefix must not match.
        assert!(!project_flag_was_explicit([
            "shelbi", "config", "lint", "--pretty"
        ]));
    }

    #[test]
    fn init_help_is_copyable_and_documents_detected_plan_flags() {
        let error = Cli::try_parse_from(["shelbi", "init", "--help"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        let help = error.to_string();
        for expected in [
            "shelbi init -y",
            "shelbi init -y --runner codex",
            "-y, --yes",
            "--runner <RUNNER>",
            "--project <PROJECT>",
            "--root <ROOT>",
            "--default-branch <BRANCH>",
            "--github-url <URL>",
            "--orchestrator-runner <RUNNER>",
        ] {
            assert!(help.contains(expected), "missing `{expected}` in:\n{help}");
        }
        assert!(
            help.contains("claude"),
            "runner values missing from:\n{help}"
        );
        assert!(
            help.contains("codex"),
            "runner values missing from:\n{help}"
        );
    }

    /// `shelbi worker list` resolves to the same handler as
    /// `shelbi workspace list` — clap parses both into the dispatch
    /// chain that ends in `commands::workspace::run`. The deprecation
    /// nag is a stderr side effect of the `Cmd::Worker` arm in `main`;
    /// the parse-side guarantee tested here is that the alias accepts
    /// every `WorkspaceCmd` subcommand.
    #[test]
    fn worker_alias_parses_into_workspace_subcommands() {
        for verb in ["list", "stop"] {
            let cli = match verb {
                "stop" => Cli::parse_from(["shelbi", "worker", verb, "alpha"]),
                _ => Cli::parse_from(["shelbi", "worker", verb]),
            };
            match cli.cmd {
                Some(Cmd::Worker {
                    cmd: WorkspaceCmd::List,
                }) if verb == "list" => {}
                Some(Cmd::Worker {
                    cmd: WorkspaceCmd::Stop { name, .. },
                }) if verb == "stop" && name == "alpha" => {}
                other => panic!("expected Cmd::Worker for `{verb}`, got {other:?}"),
            }
        }
    }

    /// `shelbi issue edit <id>` with no field flags parses into an `EditArgs`
    /// whose optional fields are all unset — the signal `edit` uses to fall
    /// back to `$EDITOR`. With field flags, the two-value `--sub`/`--sub-regex`
    /// options parse their pairs and the frontmatter flags populate.
    #[test]
    fn issue_edit_parses_bare_and_with_field_flags() {
        use commands::issue::IssueCmd;

        let bare = Cli::parse_from(["shelbi", "issue", "edit", "t1"]);
        match bare.cmd {
            Some(Cmd::Issue {
                cmd: IssueCmd::Edit(args),
            }) => {
                assert_eq!(args.id, "t1");
                assert!(args.title.is_none());
                assert!(args.body.is_none());
                assert!(!args.append);
                assert!(args.sub.is_empty());
                assert!(args.sub_regex.is_empty());
            }
            other => panic!("expected Issue::Edit, got {other:?}"),
        }

        let flags = Cli::parse_from([
            "shelbi",
            "issue",
            "edit",
            "t2",
            "--title",
            "New",
            "--sub",
            "a",
            "b",
            "--sub-regex",
            "c(\\d)",
            "d$1",
            "--branch",
            "feat/x",
        ]);
        match flags.cmd {
            Some(Cmd::Issue {
                cmd: IssueCmd::Edit(args),
            }) => {
                assert_eq!(args.title.as_deref(), Some("New"));
                assert_eq!(args.branch.as_deref(), Some("feat/x"));
                assert_eq!(args.sub, vec!["a", "b"]);
                assert_eq!(args.sub_regex, vec!["c(\\d)", "d$1"]);
            }
            other => panic!("expected Issue::Edit, got {other:?}"),
        }

        // `--sub` requires exactly two values.
        assert!(Cli::try_parse_from(["shelbi", "issue", "edit", "t3", "--sub", "only"]).is_err());
    }

    /// The deprecated `shelbi task` alias still parses into the same `IssueCmd`
    /// tree as `shelbi issue`, so existing scripts keep working through the
    /// deprecation window. The runtime notice is emitted in `run`.
    #[test]
    fn task_alias_still_parses_into_issue_cmd() {
        use commands::issue::IssueCmd;

        let aliased = Cli::parse_from(["shelbi", "task", "edit", "t1"]);
        match aliased.cmd {
            Some(Cmd::Task {
                cmd: IssueCmd::Edit(args),
            }) => assert_eq!(args.id, "t1"),
            other => panic!("expected Task::Edit (deprecated alias), got {other:?}"),
        }
    }

    /// `shelbi issue comment <id> "<text>"` parses into the id + text the store
    /// posts (plan Decision D4).
    #[test]
    fn issue_comment_parses_id_and_text() {
        use commands::issue::IssueCmd;

        let cli = Cli::parse_from(["shelbi", "issue", "comment", "fix-login", "looks good"]);
        match cli.cmd {
            Some(Cmd::Issue {
                cmd: IssueCmd::Comment { id, text },
            }) => {
                assert_eq!(id, "fix-login");
                assert_eq!(text, "looks good");
            }
            other => panic!("expected Issue::Comment, got {other:?}"),
        }

        // Both the id and the text are required positionals.
        assert!(Cli::try_parse_from(["shelbi", "issue", "comment", "fix-login"]).is_err());
    }

    /// `shelbi issue-store migrate --to github` parses into the target backend,
    /// the (default-off) dry-run flag, and the default pacing.
    #[test]
    fn issue_store_migrate_parses_target_and_dry_run() {
        use commands::issue_store::{IssueStoreCmd, MigrateTarget};

        let cli = Cli::parse_from(["shelbi", "issue-store", "migrate", "--to", "github"]);
        match cli.cmd {
            Some(Cmd::IssueStore {
                cmd: IssueStoreCmd::Migrate { to, dry_run, pace_secs },
            }) => {
                assert_eq!(to, MigrateTarget::Github);
                assert!(!dry_run, "dry_run defaults off");
                assert!(pace_secs > 0.0, "pacing defaults on for a bulk run");
            }
            other => panic!("expected IssueStore::Migrate, got {other:?}"),
        }

        // `file_system` is the reverse target; `--dry-run` flips the flag, and
        // `--pace-secs 0` disables pacing.
        let cli = Cli::parse_from([
            "shelbi",
            "issue-store",
            "migrate",
            "--to",
            "file_system",
            "--dry-run",
            "--pace-secs",
            "0",
        ]);
        match cli.cmd {
            Some(Cmd::IssueStore {
                cmd: IssueStoreCmd::Migrate { to, dry_run, pace_secs },
            }) => {
                assert_eq!(to, MigrateTarget::FileSystem);
                assert!(dry_run);
                assert_eq!(pace_secs, 0.0);
            }
            other => panic!("expected IssueStore::Migrate, got {other:?}"),
        }

        // `--to` is required, and only the live backends are accepted.
        assert!(Cli::try_parse_from(["shelbi", "issue-store", "migrate"]).is_err());
        assert!(
            Cli::try_parse_from(["shelbi", "issue-store", "migrate", "--to", "jira"]).is_err()
        );
    }

    /// `shelbi workspace list` is the canonical form and parses into
    /// `Cmd::Workspace` (no alias path).
    #[test]
    fn workspace_canonical_form_parses() {
        let cli = Cli::parse_from(["shelbi", "workspace", "list"]);
        match cli.cmd {
            Some(Cmd::Workspace {
                cmd: WorkspaceCmd::List,
            }) => {}
            other => panic!("expected Cmd::Workspace::List, got {other:?}"),
        }
    }

    /// `shelbi open <name>` is the top-level focus-or-create entry point
    /// used by the sidebar's Enter handler and the dispatch path. The
    /// `--as-pane` re-entry flag is hidden from `--help` but still
    /// parseable so the wrapper-spawn line from focus_or_create lands.
    #[test]
    fn open_parses_a_workspace_name() {
        let plain = Cli::parse_from(["shelbi", "open", "alpha"]);
        match plain.cmd {
            Some(Cmd::Open { ref name }) if name == "alpha" => {}
            other => panic!("expected Open {{ alpha }}, got {other:?}"),
        }
    }

    #[test]
    fn attach_parses_a_workspace_and_detach_key() {
        let a = Cli::parse_from(["shelbi", "attach", "alpha"]);
        match a.cmd {
            Some(Cmd::Attach {
                ref workspace,
                ref detach_key,
            }) if workspace == "alpha" && detach_key == "ctrl-]" => {}
            other => panic!("expected Attach {{ alpha, ctrl-] }}, got {other:?}"),
        }
        let b = Cli::parse_from(["shelbi", "attach", "beta", "--detach-key", "ctrl-q"]);
        match b.cmd {
            Some(Cmd::Attach {
                ref workspace,
                ref detach_key,
            }) if workspace == "beta" && detach_key == "ctrl-q" => {}
            other => panic!("expected Attach {{ beta, ctrl-q }}, got {other:?}"),
        }
    }

    /// The hidden `__session` process entry still parses after being renamed
    /// off the `Session` variant (now the user-facing `shelbi session` group),
    /// and it stays out of `--help`.
    #[test]
    fn session_process_entry_is_hidden_but_parseable() {
        let cli = Cli::parse_from([
            "shelbi",
            "__session",
            "--id",
            "abc",
            "--name",
            "demo/orch",
            "--cwd",
            "/tmp",
            "--cols",
            "80",
            "--rows",
            "24",
            "--",
            "/bin/sh",
        ]);
        assert!(matches!(cli.cmd, Some(Cmd::SessionProcess(_))));
        let help = Cli::try_parse_from(["shelbi", "--help"])
            .expect_err("--help exits through clap")
            .to_string();
        assert!(!help.contains("__session"), "internal entry leaked into help: {help}");
    }

    /// The user-facing `shelbi session` group parses each debug subcommand.
    #[test]
    fn session_group_parses_its_subcommands() {
        use commands::session_cli::SessionCmd;

        let ls = Cli::parse_from(["shelbi", "session", "ls"]);
        assert!(matches!(ls.cmd, Some(Cmd::Session { cmd: SessionCmd::Ls })));

        let new = Cli::parse_from([
            "shelbi", "session", "new", "--name", "demo/orch", "--", "/bin/sh", "-c", "exec cat",
        ]);
        match new.cmd {
            Some(Cmd::Session {
                cmd: SessionCmd::New { name, command, cols, rows, .. },
            }) => {
                assert_eq!(name, "demo/orch");
                assert_eq!(command, vec!["/bin/sh", "-c", "exec cat"]);
                assert_eq!((cols, rows), (80, 24), "defaults apply");
            }
            other => panic!("expected session new, got {other:?}"),
        }

        let send = Cli::parse_from(["shelbi", "session", "send", "alpha", "hi", "--enter"]);
        assert!(matches!(
            send.cmd,
            Some(Cmd::Session { cmd: SessionCmd::Send { enter: true, .. } })
        ));
    }

    #[test]
    fn codex_orchestrator_bridge_command_is_hidden_but_parseable() {
        let cli = Cli::parse_from(["shelbi", "__codex-orchestrator", "myapp"]);
        match cli.cmd {
            Some(Cmd::CodexOrchestrator {
                project,
                first_launch,
            }) if project == "myapp" && !first_launch => {}
            other => panic!("expected CodexOrchestrator {{ myapp }}, got {other:?}"),
        }

        let first = Cli::parse_from([
            "shelbi",
            "__codex-orchestrator",
            "myapp",
            "--first-launch",
        ]);
        assert!(matches!(
            first.cmd,
            Some(Cmd::CodexOrchestrator {
                project,
                first_launch: true,
            }) if project == "myapp"
        ));

        let help = Cli::try_parse_from(["shelbi", "--help"])
            .expect_err("--help exits through clap")
            .to_string();
        assert!(
            !help.contains("__codex-orchestrator"),
            "internal bridge command leaked into help: {help}"
        );
    }

    #[test]
    fn relay_command_is_hidden_but_parseable() {
        let cli = Cli::parse_from(["shelbi", "relay"]);
        assert!(
            matches!(cli.cmd, Some(Cmd::Relay(_))),
            "`shelbi relay` should parse as the relay command"
        );

        let help = Cli::try_parse_from(["shelbi", "--help"])
            .expect_err("--help exits through clap")
            .to_string();
        assert!(
            !help.contains("relay"),
            "the machine-facing relay command leaked into help: {help}"
        );
    }

    /// `project_registration_exists` is the predicate `default_entry` uses
    /// to decide whether an explicitly-named project is live (boot it) or
    /// missing on this machine (fall through to first-run). Both supported
    /// registry layouts count: a flat global YAML and the local half of an
    /// in-repo split project.
    #[test]
    fn project_registration_exists_supports_both_config_modes() {
        let _g = commands::test_support::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "shelbi-stale-marker-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(home.join("projects")).unwrap();
        let env = commands::test_support::EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);

        assert!(
            !project_registration_exists("nope"),
            "missing registration should be stale"
        );
        assert!(
            !project_registration_exists("../live"),
            "invalid project names must not escape the registry directory"
        );
        std::fs::write(home.join("projects/live.yaml"), "name: live\n").unwrap();
        assert!(
            project_registration_exists("live"),
            "flat global YAML should be live"
        );

        std::fs::create_dir_all(home.join("projects/split")).unwrap();
        std::fs::write(
            home.join("projects/split/local.yaml"),
            "repo: /tmp/split\nmachines: []\n",
        )
        .unwrap();
        assert!(
            project_registration_exists("split"),
            "split local registration should be live"
        );

        std::fs::create_dir_all(home.join("projects/incomplete")).unwrap();
        assert!(
            !project_registration_exists("incomplete"),
            "a project directory without local.yaml is not registered"
        );

        assert_eq!(
            classify_default_route(Some("live".to_string()), |_| true),
            DefaultRoute::Dashboard("live".to_string())
        );
        assert_eq!(
            classify_default_route(None, |_| true),
            DefaultRoute::Onboarding {
                missing_project: None
            },
            "an unresolved cwd must onboard instead of selecting an unrelated project"
        );
        assert_eq!(
            classify_default_route(Some("missing".to_string()), |_| false),
            DefaultRoute::Onboarding {
                missing_project: Some("missing".to_string())
            }
        );
    }

    #[test]
    fn onboarding_resolution_preserves_pick_up_errors() {
        let missing_local = shelbi_core::Error::ProjectNotPickedUp {
            name: "shared".into(),
            config_path: "/tmp/shared/.shelbi/project.yaml".into(),
            expected_local: "/tmp/home/projects/shared/local.yaml".into(),
        };
        let error = resolve_or_onboard(Err(anyhow::anyhow!(missing_local))).unwrap_err();
        assert!(error.to_string().contains("shelbi init --pick-up"));

        assert_eq!(
            resolve_or_onboard(Err(anyhow::anyhow!("no project specified"))).unwrap(),
            None
        );
        assert_eq!(
            resolve_or_onboard(Ok("configured".to_string())).unwrap(),
            Some("configured".to_string())
        );
    }

    /// A mistyped subcommand must be a parse *error*, not silently
    /// absorbed. Before F8 a bare `session` positional swallowed the typo
    /// (`shelbi statsu` parsed as `session = "statsu"`, `cmd = None`) and
    /// `default_entry` booted the TUI instead of erroring. With the dead
    /// positional gone, clap rejects the unknown token.
    #[test]
    fn mistyped_subcommand_is_a_parse_error() {
        for argv in [vec!["shelbi", "statsu"], vec!["shelbi", "tsk", "list"]] {
            assert!(
                Cli::try_parse_from(&argv).is_err(),
                "expected `{argv:?}` to be rejected, not parsed",
            );
        }
        // Bare `shelbi` (no subcommand) is still valid — it drives the
        // default TUI/first-run entry.
        assert!(Cli::try_parse_from(["shelbi"]).is_ok());
    }

    #[test]
    fn zen_pr_flow_commands_require_the_complete_probe_identity() {
        let required = [
            ("--match-repository", "github.com/acme/widgets"),
            ("--match-repository-id", "R_123"),
            ("--match-base-branch", "feature/app"),
            ("--match-base-commit", "base123"),
            ("--match-integration-commit", "integration123"),
            ("--match-head-commit", "head123"),
        ];
        for command in [
            ["shelbi", "zen", "pr-create", "task-1"],
            ["shelbi", "zen", "ci-watch", "42"],
            ["shelbi", "zen", "pr-merge", "42"],
        ] {
            for (omitted, _) in required {
                let mut argv = command.to_vec();
                for (flag, value) in required {
                    if flag != omitted {
                        argv.extend([flag, value]);
                    }
                }
                let err = Cli::try_parse_from(&argv).expect_err(
                    "a Zen PR flow operation with incomplete provenance must fail parsing",
                );
                assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument, "{err}");
                assert!(err.to_string().contains(omitted), "{err}");
            }
        }

        let create = Cli::parse_from([
            "shelbi",
            "zen",
            "pr-create",
            "task-1",
            "--match-repository",
            "github.com/acme/widgets",
            "--match-repository-id",
            "R_123",
            "--match-base-branch",
            "feature/app",
            "--match-base-commit",
            "base123",
            "--match-integration-commit",
            "integration123",
            "--match-head-commit",
            "head123",
        ]);
        assert!(matches!(
            create.cmd,
            Some(Cmd::Zen {
                cmd: commands::zen::ZenCmd::PrCreate {
                    task_id,
                    identity,
                },
            }) if task_id == "task-1"
                && identity.match_repository == "github.com/acme/widgets"
                && identity.match_repository_id == "R_123"
                && identity.match_base_branch == "feature/app"
                && identity.match_base_commit == "base123"
                && identity.match_integration_commit == "integration123"
                && identity.match_head_commit == "head123"
        ));

        let watch = Cli::parse_from([
            "shelbi",
            "zen",
            "ci-watch",
            "42",
            "--match-repository",
            "github.com/acme/widgets",
            "--match-repository-id",
            "R_123",
            "--match-base-branch",
            "feature/app",
            "--match-base-commit",
            "base123",
            "--match-integration-commit",
            "integration123",
            "--match-head-commit",
            "head123",
        ]);
        assert!(matches!(
            watch.cmd,
            Some(Cmd::Zen {
                cmd: commands::zen::ZenCmd::CiWatch {
                    pr_number: 42,
                    identity,
                    ..
                },
            }) if identity.match_repository == "github.com/acme/widgets"
                && identity.match_repository_id == "R_123"
                && identity.match_base_branch == "feature/app"
                && identity.match_base_commit == "base123"
                && identity.match_integration_commit == "integration123"
                && identity.match_head_commit == "head123"
        ));

        let merge = Cli::parse_from([
            "shelbi",
            "zen",
            "pr-merge",
            "42",
            "--match-repository",
            "github.com/acme/widgets",
            "--match-repository-id",
            "R_123",
            "--match-base-branch",
            "feature/app",
            "--match-base-commit",
            "base123",
            "--match-integration-commit",
            "integration123",
            "--match-head-commit",
            "head123",
        ]);
        assert!(matches!(
            merge.cmd,
            Some(Cmd::Zen {
                cmd: commands::zen::ZenCmd::PrMerge {
                    pr_number: 42,
                    identity,
                },
            }) if identity.match_repository == "github.com/acme/widgets"
                && identity.match_repository_id == "R_123"
                && identity.match_base_branch == "feature/app"
                && identity.match_base_commit == "base123"
                && identity.match_integration_commit == "integration123"
                && identity.match_head_commit == "head123"
        ));
    }

    /// `--root <path>` is a top-level global flag — accepted before *or*
    /// after the subcommand, and stashed into [`Cli::root`] either way.
    /// The actual override wiring is exercised in `shelbi-state`'s
    /// `root` module tests; this test just pins the parse surface.
    #[test]
    fn root_flag_parses_before_and_after_subcommand() {
        let pre = Cli::parse_from(["shelbi", "--root", "/tmp/r1", "status"]);
        assert_eq!(pre.root.as_deref(), Some(std::path::Path::new("/tmp/r1")));
        assert!(matches!(pre.cmd, Some(Cmd::Status { .. })));
        let post = Cli::parse_from(["shelbi", "status", "--root", "/tmp/r2"]);
        assert_eq!(post.root.as_deref(), Some(std::path::Path::new("/tmp/r2")));
        assert!(matches!(post.cmd, Some(Cmd::Status { .. })));
        let absent = Cli::parse_from(["shelbi", "status"]);
        assert!(absent.root.is_none());
    }
}
