use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod commands;
mod wizard;

#[derive(Debug, Parser)]
#[command(
    name = "shelbi",
    version,
    about = "Open-source agent orchestrator for the terminal",
    long_about = None,
)]
struct Cli {
    /// Project to operate on. Defaults to the project named in $SHELBI_PROJECT
    /// or the closest `.shelbi/project` marker file in the current directory's
    /// ancestors.
    #[arg(long, short = 'p', global = true, env = "SHELBI_PROJECT")]
    project: Option<String>,

    /// Session (workspace) to load — only used when no subcommand is given.
    /// Defaults to $SHELBI_SESSION or "default".
    #[arg(env = "SHELBI_SESSION")]
    session: Option<String>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Spawn a new worker agent on a machine.
    Spawn(commands::spawn::Args),
    /// List active workers.
    List,
    /// Show the status of one or all workers.
    Status {
        /// Worker id; omit to list all.
        id: Option<String>,
    },
    /// Send a follow-up message to a running worker.
    Send {
        id: String,
        message: String,
    },
    /// Tail a worker's recent output.
    Tail {
        id: String,
        #[arg(long, default_value_t = 40)]
        lines: usize,
    },
    /// Show a worker's working-tree diff.
    Diff { id: String },
    /// Merge a worker's branch into the project's default branch.
    Merge {
        id: String,
        /// Push branch + open a GitHub PR instead of a local merge.
        #[arg(long)]
        pr: bool,
    },
    /// Archive a worker (keep the log, drop the worktree).
    Archive { id: String },
    /// Manage the project's Kanban task board.
    Task {
        #[command(subcommand)]
        cmd: commands::task::TaskCmd,
    },
    /// Inspect and control the project's declared worker pool.
    Worker {
        #[command(subcommand)]
        cmd: commands::worker::WorkerCmd,
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
    /// writes a starter `keys.yml`; `check` validates `~/.shelbi/keys.yml`
    /// and reports any errors/warnings.
    Config {
        #[command(subcommand)]
        cmd: commands::config::ConfigCmd,
    },
    /// Inspect the hub-global worker-state transition log.
    Events {
        #[command(subcommand)]
        cmd: commands::events::EventsCmd,
    },
    /// Check a task's branch into the machine's review work_dir and
    /// (re)launch a fresh review-claude pane there.
    Review(commands::review::Args),
    /// Attach the terminal to a worker's tmux pane.
    Attach { id: String },
    /// Scaffold ~/.shelbi/ and (optionally) a starter project YAML.
    Init(commands::init::Args),
    /// Run the onboarding wizard. Phase 1 names the assistant; Phase 2
    /// walks through project setup (auto-filled from the current git
    /// checkout when present) and writes ~/.shelbi/projects/<name>.yaml.
    /// Idempotent — phases whose answer is already on disk are skipped.
    Wizard,
    /// Start the orchestrator agent in the project's tmux session window 1.
    Orchestrate(commands::orchestrate::Args),
    /// Respawn the shelbi-owned panes (sidebar + tasks/review/machines) in
    /// place so a freshly installed binary takes effect. Leaves the
    /// orchestrator pane and worker panes alone — those re-shell into
    /// shelbi on every call and pick up the new binary automatically.
    Reload,
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
    /// (internal) Run the review-queue ratatui view inside the hidden
    /// stash pane. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__review")]
    ReviewView { project: String },
    /// (internal) Run the activity-feed ratatui view inside the hidden
    /// stash pane. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__activity")]
    Activity { project: String },
    /// Toggle Zen Mode or run its primitives. `shelbi zen on/off/pause` flip
    /// the trust boundary that lets the orchestrator auto-merge and
    /// auto-promote. `probe` reports facts about a finished branch (checks,
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
    init_tracing(cli.cmd.as_ref());

    match cli.cmd {
        None => default_entry(cli.project.clone()),
        Some(Cmd::Spawn(args)) => commands::spawn::run(cli.project, args),
        Some(Cmd::List) => commands::list::run(cli.project),
        Some(Cmd::Status { id }) => commands::status::run(cli.project, id),
        Some(Cmd::Send { id, message }) => commands::send::run(cli.project, id, message),
        Some(Cmd::Tail { id, lines }) => commands::tail::run(cli.project, id, lines),
        Some(Cmd::Diff { id }) => commands::diff::run(cli.project, id),
        Some(Cmd::Merge { id, pr }) => commands::merge::run(cli.project, id, pr),
        Some(Cmd::Archive { id }) => commands::archive::run(cli.project, id),
        Some(Cmd::Task { cmd }) => commands::task::run(cli.project, cmd),
        Some(Cmd::Worker { cmd }) => commands::worker::run(cli.project, cmd),
        Some(Cmd::Workflow { cmd }) => commands::workflow::run(cli.project, cmd),
        Some(Cmd::Project { cmd }) => commands::project::run(cmd),
        Some(Cmd::Config { cmd }) => commands::config::run(cli.project, cmd),
        Some(Cmd::Events { cmd }) => commands::events::run(cmd),
        Some(Cmd::Zen { cmd }) => commands::zen::run(cli.project, cmd),
        Some(Cmd::Action { cmd }) => commands::action::run(cli.project, cmd),
        Some(Cmd::Review(args)) => commands::review::run(cli.project, args),
        Some(Cmd::Attach { id }) => commands::attach::run(cli.project, id),
        Some(Cmd::Init(args)) => commands::init::run(args),
        Some(Cmd::Wizard) => commands::wizard::run(false).map(|_| ()),
        Some(Cmd::Orchestrate(args)) => commands::orchestrate::run(cli.project, args),
        Some(Cmd::Reload) => commands::reload::run(cli.project),
        Some(Cmd::Sidebar { project }) => shelbi_tui::run_sidebar(&project).context("sidebar"),
        Some(Cmd::Tasks { project }) => shelbi_tui::run_tasks(&project).context("tasks"),
        Some(Cmd::ReviewView { project }) => shelbi_tui::run_review(&project).context("review"),
        Some(Cmd::Activity { project }) => shelbi_tui::run_activity(&project).context("activity"),
        Some(Cmd::Popup) => commands::popup::run(),
        Some(Cmd::Palette { project }) => commands::palette::run(project),
        Some(Cmd::ZenOrchStart { project }) => commands::zen_lifecycle::orch_start(&project),
        Some(Cmd::ZenHeartbeat { project }) => commands::zen_lifecycle::heartbeat(&project),
        Some(Cmd::ZenOrchExit { project }) => commands::zen_lifecycle::orch_exit(&project),
    }
}

/// `shelbi` with no subcommand. Dispatches based on what's on disk:
///
/// - explicit `--project` / `SHELBI_PROJECT` / `.shelbi/project` marker →
///   boot that project's TUI (this branch is checked first so a marker
///   always wins over the picker — matches today's behavior).
/// - `~/.shelbi/` missing OR `~/.shelbi/projects/` empty → onboarding
///   wizard. After it completes, if exactly one project now exists boot
///   it directly; otherwise print a hint and exit.
/// - exactly one project YAML → boot it.
/// - two or more → project picker.
fn default_entry(explicit: Option<String>) -> Result<()> {
    if let Ok(p) = commands::require_project(explicit) {
        return shelbi_tui::run_main(&p).context("launching shelbi");
    }

    let home = shelbi_state::shelbi_home().map_err(|e| anyhow::anyhow!(e))?;
    let home_existed = home.exists();
    let projects = if home_existed {
        shelbi_state::list_projects().map_err(|e| anyhow::anyhow!(e))?
    } else {
        Vec::new()
    };

    if projects.is_empty() {
        return run_wizard_then_dispatch(!home_existed);
    }
    if projects.len() == 1 {
        return shelbi_tui::run_main(&projects[0].name).context("launching shelbi");
    }
    match commands::picker::pick_or_setup()? {
        commands::picker::PickerOutcome::Existing(p)
        | commands::picker::PickerOutcome::Created(p) => {
            shelbi_tui::run_main(&p).context("launching shelbi")
        }
        commands::picker::PickerOutcome::Cancelled => Ok(()),
    }
}

/// Run the wizard and, if it produced exactly one project, boot directly
/// into its TUI. Any other end-state (cancelled, zero projects, more than
/// one) exits cleanly — the user can re-run `shelbi` later.
///
/// `first_run` is true when `~/.shelbi/` did not exist before this
/// invocation; the wizard prints the brand banner only in that case.
fn run_wizard_then_dispatch(first_run: bool) -> Result<()> {
    match commands::wizard::run(first_run)? {
        commands::wizard::WizardOutcome::Cancelled => return Ok(()),
        commands::wizard::WizardOutcome::Completed => {}
    }
    let projects = shelbi_state::list_projects().map_err(|e| anyhow::anyhow!(e))?;
    if projects.len() == 1 {
        return shelbi_tui::run_main(&projects[0].name).context("launching shelbi");
    }
    println!("Run shelbi to launch, or shelbi project add to add another.");
    Ok(())
}

/// Initialize the tracing subscriber.
///
/// For internal ratatui subcommands (`__sidebar`, `__tasks`, `__review`,
/// `__activity`) we route output to `~/.shelbi/logs/tui.log` instead of
/// stderr. The TUI process
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
            | Some(Cmd::ReviewView { .. })
            | Some(Cmd::Activity { .. })
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
