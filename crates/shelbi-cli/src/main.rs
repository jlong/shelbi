use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod commands;
mod project_root;
mod wizard;

#[derive(Debug, Parser)]
#[command(
    name = "shelbi",
    version,
    about = "Open-source agent orchestrator for the terminal",
    long_about = None,
)]
struct Cli {
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
    /// [legacy spawn flow] Spawn a one-shot agent on a machine. The
    /// workspace-based flow (`shelbi task start` + the project YAML's
    /// `workspaces:` block) is canonical now; `spawn` keeps working for
    /// projects that haven't migrated.
    Spawn(commands::spawn::Args),
    /// [legacy spawn flow] List agents written by `shelbi spawn`.
    /// `shelbi workspace list` is the canonical view of the workspace pool
    /// and what `shelbi send`/`task start` operate against.
    List,
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
    /// `send` injects keystrokes into a tmux pane; `message` appends a
    /// durable JSON record the workspace tails and acks.
    Message {
        /// Task id whose assigned workspace receives the message.
        id: String,
        /// Message kind.
        #[arg(value_enum)]
        kind: commands::message::MessageKind,
        /// Message body.
        body: String,
        /// Question id this message replies to (sets `in_response_to`).
        /// Typically paired with `kind = reply`.
        #[arg(long = "in-response-to", value_name = "QUESTION-ID")]
        in_response_to: Option<String>,
    },
    /// [legacy spawn flow] Tail a spawn-agent's recent output. Workspaces
    /// don't write to the legacy log — use `tmux capture-pane` against
    /// `shelbi-<project>:<workspace>` (local) or
    /// `shelbi-w-<workspace>:agent` (remote) instead.
    Tail {
        id: String,
        #[arg(long, default_value_t = 40)]
        lines: usize,
    },
    /// [legacy spawn flow] Show a spawn-agent's working-tree diff. For
    /// workspaces the worktree is at
    /// `<machine.work_dir>/.shelbi/wt/<workspace>` — run `git -C` there
    /// directly.
    Diff { id: String },
    /// Merge a workspace's branch into the project's default branch.
    Merge {
        id: String,
        /// Push branch + open a GitHub PR instead of a local merge.
        #[arg(long)]
        pr: bool,
    },
    /// [legacy spawn flow] Archive a spawn agent (keep the log, drop the
    /// worktree). Workspaces are durable slots — use
    /// `shelbi workspace stop <name>` to release the slot's in-flight task
    /// instead.
    Archive { id: String },
    /// Focus the workspace's tmux pane, creating it (with the agent
    /// running) if it doesn't exist yet. Single entry point for both
    /// the sidebar click-to-focus path and the dispatch path — the
    /// "exists?" check lives here so callers don't have to branch on it.
    ///
    /// For LOCAL workspaces, an empty pane is created with this same
    /// command re-entered under `--as-pane` (the wrapper that owns the
    /// agent subprocess and emits a `pane_alive=false` event on exit).
    ///
    /// For REMOTE workspaces, the pane is a proxy window that
    /// `ssh -t … tmux attach -t shelbi-w-<name>` into the workspace's
    /// own remote tmux session — the lifecycle wrapper isn't deployed
    /// to remote machines.
    Open {
        /// Name of the workspace to open.
        name: String,
        /// Internal re-entry flag. When set, this process *is* the
        /// pane's top-level command: it fork+execs the agent runner,
        /// waits, and emits the `pane_alive=false` event on any exit
        /// path (including SIGHUP/SIGTERM/SIGINT). Not for direct use.
        #[arg(long, hide = true)]
        as_pane: bool,
        /// Internal re-entry flag set by `shelbi task resume`. When set
        /// alongside `--as-pane`, the wrapper launches a claude runner with
        /// `--continue` so the pane reloads its prior conversation instead of
        /// starting cold. Not for direct use.
        #[arg(long, hide = true)]
        resume: bool,
    },
    /// Manage the project's Kanban task board.
    Task {
        #[command(subcommand)]
        cmd: commands::task::TaskCmd,
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
    /// Inspect and validate the user's keybinding configuration.
    /// `list-actions` shows every action's current chord(s); `dump-keybindings`
    /// writes a starter `keys.yaml`; `check` validates `~/.shelbi/keys.yaml`
    /// and reports any errors/warnings.
    Config {
        #[command(subcommand)]
        cmd: commands::config::ConfigCmd,
    },
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
    /// Attach the terminal to a workspace's tmux pane.
    Attach { id: String },
    /// Initialize a Shelbi project. `shelbi init -y` detects the repository,
    /// runner, tmux, and workspace plan, then scaffolds it without prompts.
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
    /// Start the orchestrator agent in the project's tmux session window 1.
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
    /// (internal) Run the sidebar ratatui process inside the dashboard's
    /// left pane. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__sidebar")]
    Sidebar { project: String },
    /// (internal) Run the Kanban tasks view inside the hidden stash pane.
    /// Not for direct use.
    #[command(hide = true)]
    #[command(name = "__tasks")]
    Tasks { project: String },
    /// (internal) Run the activity-feed ratatui view inside the hidden
    /// stash pane. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__activity")]
    Activity { project: String },
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
    /// Open the palette as a tmux popup. Bound to Ctrl+P by default.
    Popup,
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
    /// (internal) Graceful-exit clear the orchestrator pane wrapper
    /// runs after the agent returns. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__zen-orch-exit")]
    ZenOrchExit { project: String },
    /// (internal) Run the palette picker — meant to be invoked inside a
    /// `tmux display-popup`. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__palette")]
    Palette { project: String },
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
        Some(Cmd::Spawn(args)) => commands::spawn::run(cli.project, args),
        Some(Cmd::List) => commands::list::run(cli.project),
        Some(Cmd::Status { cmd, full, handoff }) => {
            commands::status::run(cli.project, cmd, full, handoff)
        }
        Some(Cmd::Send { id, message }) => commands::send::run(cli.project, id, message),
        Some(Cmd::Message {
            id,
            kind,
            body,
            in_response_to,
        }) => commands::message::run(cli.project, id, kind, body, in_response_to),
        Some(Cmd::Tail { id, lines }) => commands::tail::run(cli.project, id, lines),
        Some(Cmd::Diff { id }) => commands::diff::run(cli.project, id),
        Some(Cmd::Merge { id, pr }) => commands::merge::run(cli.project, id, pr),
        Some(Cmd::Archive { id }) => commands::archive::run(cli.project, id),
        Some(Cmd::Open {
            name,
            as_pane,
            resume,
        }) => commands::open::run(cli.project, name, as_pane, resume),
        Some(Cmd::Task { cmd }) => commands::task::run(cli.project, cmd),
        Some(Cmd::Workspace { cmd }) => commands::workspace::run(cli.project, cmd),
        Some(Cmd::Worker { cmd }) => {
            eprintln!("shelbi: 'worker' is deprecated; use 'workspace' instead.");
            commands::workspace::run(cli.project, cmd)
        }
        Some(Cmd::Agent { cmd }) => commands::agent::run(cli.project, cmd),
        Some(Cmd::Workflow { cmd }) => commands::workflow::run(cli.project, cmd),
        Some(Cmd::Project { cmd }) => commands::project::run(cli.project, cmd),
        Some(Cmd::Config { cmd }) => commands::config::run(cli.project, cmd),
        Some(Cmd::Events { cmd }) => commands::events::run(cmd),
        Some(Cmd::Daemon { cmd }) => commands::daemon::run(cmd),
        Some(Cmd::Zen { cmd }) => commands::zen::run(cli.project, cmd),
        Some(Cmd::Action { cmd }) => commands::action::run(cli.project, cmd),
        Some(Cmd::Attach { id }) => commands::attach::run(cli.project, id),
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
        Some(Cmd::Sidebar { project }) => shelbi_tui::run_sidebar(&project).context("sidebar"),
        Some(Cmd::Tasks { project }) => shelbi_tui::run_tasks(&project).context("tasks"),
        Some(Cmd::Activity { project }) => shelbi_tui::run_activity(&project).context("activity"),
        Some(Cmd::CodexOrchestrator {
            project,
            first_launch,
        }) => {
            shelbi_orchestrator::wake::run_codex_bridge(&project, first_launch)
                .map_err(|e| anyhow::anyhow!(e.to_string()))
        }
        Some(Cmd::Popup) => commands::popup::run(),
        Some(Cmd::Palette { project }) => commands::palette::run(project),
        Some(Cmd::ZenOrchStart { project }) => commands::zen_lifecycle::orch_start(&project),
        Some(Cmd::ZenHeartbeat { project }) => commands::zen_lifecycle::heartbeat(&project),
        Some(Cmd::ZenOrchExit { project }) => commands::zen_lifecycle::orch_exit(&project),
    }
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
/// For internal TUI-owning subcommands (`__sidebar`, `__tasks`,
/// `__activity`, `__codex-orchestrator`) we route output to
/// `~/.shelbi/logs/tui.log` instead of stderr. The process
/// shares its TTY with ratatui's draw cycle, and any stray stderr write
/// corrupts the cursor position — leaving raw `tracing` lines bleeding across
/// the sidebar until the next full repaint (e.g. a resize). For all other
/// commands the default stderr writer is fine.
fn init_tracing(cmd: Option<&Cmd>) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("SHELBI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let is_tui = matches!(
        cmd,
        Some(Cmd::Sidebar { .. })
            | Some(Cmd::Tasks { .. })
            | Some(Cmd::Activity { .. })
            | Some(Cmd::CodexOrchestrator { .. })
    );
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
            "--workspace-count",
            "3",
            "--workspace-preset",
            "toy-story",
            "--orchestrator-runner",
            "claude",
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
        assert_eq!(args.workspace_count, Some(3));
        assert_eq!(
            args.workspace_preset,
            Some(shelbi_core::WorkspaceNamePreset::ToyStory)
        );
        assert_eq!(args.orchestrator_runner, Some(wizard::Runner::Claude));
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
            "--workspace-count <COUNT>",
            "--workspace-preset <PRESET>",
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
    fn open_parses_with_and_without_as_pane() {
        let plain = Cli::parse_from(["shelbi", "open", "alpha"]);
        match plain.cmd {
            Some(Cmd::Open {
                ref name, as_pane, ..
            }) if name == "alpha" && !as_pane => {}
            other => panic!("expected Open {{ alpha, as_pane=false }}, got {other:?}"),
        }

        let wrapped = Cli::parse_from(["shelbi", "open", "delta", "--as-pane"]);
        match wrapped.cmd {
            Some(Cmd::Open {
                ref name, as_pane, ..
            }) if name == "delta" && as_pane => {}
            other => panic!("expected Open {{ delta, as_pane=true }}, got {other:?}"),
        }

        // `shelbi task resume` re-enters the wrapper with `--as-pane --resume`;
        // both flags parse together so the wrapper can select `--continue`.
        let resumed = Cli::parse_from(["shelbi", "open", "alpha", "--as-pane", "--resume"]);
        match resumed.cmd {
            Some(Cmd::Open {
                ref name,
                as_pane,
                resume,
                ..
            }) if name == "alpha" && as_pane && resume => {}
            other => panic!("expected Open {{ alpha, as_pane, resume }}, got {other:?}"),
        }
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

    /// `project_registration_exists` is the predicate `default_entry` uses
    /// to decide whether an explicitly-named project is live (boot it) or
    /// missing on this machine (fall through to first-run). Both supported
    /// registry layouts count: a flat global YAML and the local half of an
    /// in-repo split project.
    #[test]
    fn project_registration_exists_supports_both_config_modes() {
        let _g = commands::test_support::ENV_LOCK.lock().unwrap();
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
                && identity.match_head_commit == "head123"
        ));
    }

    /// `--root <path>` is a top-level global flag — accepted before *or*
    /// after the subcommand, and stashed into [`Cli::root`] either way.
    /// The actual override wiring is exercised in `shelbi-state`'s
    /// `root` module tests; this test just pins the parse surface.
    #[test]
    fn root_flag_parses_before_and_after_subcommand() {
        let pre = Cli::parse_from(["shelbi", "--root", "/tmp/r1", "list"]);
        assert_eq!(pre.root.as_deref(), Some(std::path::Path::new("/tmp/r1")));
        assert!(matches!(pre.cmd, Some(Cmd::List)));
        let post = Cli::parse_from(["shelbi", "list", "--root", "/tmp/r2"]);
        assert_eq!(post.root.as_deref(), Some(std::path::Path::new("/tmp/r2")));
        assert!(matches!(post.cmd, Some(Cmd::List)));
        let absent = Cli::parse_from(["shelbi", "list"]);
        assert!(absent.root.is_none());
    }
}
