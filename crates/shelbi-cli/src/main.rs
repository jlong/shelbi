use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod commands;

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
    /// Scaffold ~/.shelbi/ and an initial project (stub).
    Init,
    /// Start the orchestrator agent in the project's tmux session window 1.
    Orchestrate(commands::orchestrate::Args),
}

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.cmd {
        None => {
            let session = cli.session.unwrap_or_else(|| "default".to_string());
            shelbi_tui::run(&session).context("running TUI")
        }
        Some(Cmd::Spawn(args)) => commands::spawn::run(cli.project, args),
        Some(Cmd::List) => commands::list::run(cli.project),
        Some(Cmd::Status { id }) => commands::status::run(cli.project, id),
        Some(Cmd::Send { id, message }) => commands::send::run(cli.project, id, message),
        Some(Cmd::Tail { id, lines }) => commands::tail::run(cli.project, id, lines),
        Some(Cmd::Diff { id }) => commands::diff::run(cli.project, id),
        Some(Cmd::Merge { id, pr }) => commands::merge::run(cli.project, id, pr),
        Some(Cmd::Archive { id }) => commands::archive::run(cli.project, id),
        Some(Cmd::Init) => commands::init::run(),
        Some(Cmd::Orchestrate(args)) => commands::orchestrate::run(cli.project, args),
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("SHELBI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}
