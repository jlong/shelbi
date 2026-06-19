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
    /// Run the onboarding wizard. Phase 1 prompts for the assistant name
    /// and writes it to ~/.shelbi/shelbi.yaml. Idempotent — phases whose
    /// answer is already on disk are skipped.
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
    /// Open the palette as a tmux popup. Bound to Ctrl+P by default.
    Popup,
    /// (internal) Run the palette picker — meant to be invoked inside a
    /// `tmux display-popup`. Not for direct use.
    #[command(hide = true)]
    #[command(name = "__palette")]
    Palette { project: String },
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.cmd {
        None => {
            // Resolve the project the same way the CLI commands do —
            // explicit `--project`, then `.shelbi/project` marker. Falling
            // through to the picker only when neither resolves keeps the
            // happy path (one cwd, one marker) prompt-free.
            let project = match commands::require_project(cli.project.clone()) {
                Ok(p) => p,
                Err(_) => match commands::picker::pick_or_setup()? {
                    commands::picker::PickerOutcome::Existing(p) => p,
                    commands::picker::PickerOutcome::Created(p) => p,
                    commands::picker::PickerOutcome::Cancelled => return Ok(()),
                },
            };
            shelbi_tui::run_main(&project).context("launching shelbi")
        }
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
        Some(Cmd::Events { cmd }) => commands::events::run(cmd),
        Some(Cmd::Review(args)) => commands::review::run(cli.project, args),
        Some(Cmd::Attach { id }) => commands::attach::run(cli.project, id),
        Some(Cmd::Init(args)) => commands::init::run(args),
        Some(Cmd::Wizard) => commands::wizard::run(),
        Some(Cmd::Orchestrate(args)) => commands::orchestrate::run(cli.project, args),
        Some(Cmd::Reload) => commands::reload::run(cli.project),
        Some(Cmd::Sidebar { project }) => shelbi_tui::run_sidebar(&project).context("sidebar"),
        Some(Cmd::Tasks { project }) => shelbi_tui::run_tasks(&project).context("tasks"),
        Some(Cmd::ReviewView { project }) => shelbi_tui::run_review(&project).context("review"),
        Some(Cmd::Popup) => commands::popup::run(),
        Some(Cmd::Palette { project }) => commands::palette::run(project),
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("SHELBI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}
