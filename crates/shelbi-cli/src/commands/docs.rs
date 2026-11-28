//! `shelbi docs <command>`: read and write the project's docs, shaped like
//! `cstore` so agent prompts and muscle memory carry over.
//!
//! Every command goes through the hub's docs service
//! ([`shelbi_app::docs`], answered by `shelbi_orchestrator::docs`), which is
//! the docs' single writer: nothing here touches the docs' files. Paths are
//! relative to the docs' root.
//!
//! Output is text by default, JSON with `--format json` or `--agent`. Exit
//! codes follow `cstore`: 0 ok, 1 error, 2 not found, 3 invalid (bad
//! arguments included; `main` maps clap's own errors for `shelbi docs` to
//! 3), 4 conflict.
//!
//! The commands' rendering is split from their transport ([`Hub`]) so the
//! formatting is tested without a daemon; `tests/docs_cli.rs` runs every
//! command through a real one.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{json, Value};
use shelbi_app::docs::{
    Author, EntryKind, FailureKind, NodeKind, Resolution, SyncOp, DocsConflict, DocsEntry, DocsFailure, DocsNode,
    DocsReply, DocsRequest, DocsStatus,
};
use shelbi_core::{DocsConfig, DocsSource};

/// `shelbi docs --help`: the commands grouped as `cstore help` groups them.
pub const HELP_TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}

BROWSING
  tree        Show the docs' folders (and documents, with --documents)
  ls          List one folder's entries, or a document's headings
  read        Print a document, or one section of it
  cat         Print several documents (or every document in a folder)
  search      Search the docs' documents
  diff        Show the change not yet committed
  log         Show the docs' history

EDITING
  new         Create a document (path.md) or a folder
  edit        Open a document in $EDITOR and save it through the hub
  write       Replace a document's text, or one section's
  append      Add to the end of a document or section
  prepend     Add to the start of a document (after its title) or section

FILE MANAGEMENT
  mv          Move or rename a document or folder
  cp          Copy a document or folder
  rm          Remove a document or folder, or one section of a document

TASKS
  tasks       List the checkboxes in a document or folder
  check       Check a task by its text
  uncheck     Uncheck a task by its text

SYNC
  sync        Show the sync's status, or sync now, pause, resume
  resolve     Settle a document the sync couldn't merge (--ours, --theirs)
  init        Set up the docs (an orphan branch, or a repo folder)

APP INTEGRATION
  open        Show the docs (or a document) in the desktop app

COMMON FLAGS
  --project <name>     The project whose docs to use (else $SHELBI_PROJECT or the cwd's)
  --format json        Machine-readable JSON output
  --agent              JSON output, no color (for agents)
  --section \"## H\"     Target one section by its heading
  --head <n>           Only the first N lines (read, cat)
  --tail <n>           Only the last N lines (read, cat)
  --regex              Treat the search query as a regular expression
  --stats              Line, word and size counts (ls)

EXAMPLES
  shelbi docs tree --documents                     Every folder and document
  shelbi docs ls Plans --stats                     A folder's documents with counts
  shelbi docs read Plans/docs.md --section \"## Design\"
  shelbi docs new Plans/auth.md --title \"Auth\" --content \"## Goals\"
  cat notes.md | shelbi docs write Plans/auth.md   Replace the text (title kept)
  shelbi docs search \"PIX-\\d+\" --regex
  shelbi docs tasks Plans/ --pending               Open tasks across a folder
  shelbi docs sync now
  shelbi docs resolve Plans/auth.md --theirs       Settle a conflict

Paths are relative to the docs' root. Every read and write goes through the
hub, the docs' single writer, which commits and syncs on its own.
Exit codes: 0 ok, 1 error, 2 not found, 3 invalid, 4 conflict.

Run `shelbi docs <command> --help` for a command's flags and examples.

Options:
{options}
";

#[derive(Debug, Args)]
pub struct DocsArgs {
    /// Output format.
    #[arg(long, value_enum, global = true, default_value_t = Format::Text)]
    format: Format,
    /// Agent mode: JSON output, no color.
    #[arg(long, global = true)]
    agent: bool,
    #[command(subcommand)]
    cmd: DocsCmd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Text,
    Json,
}

#[derive(Debug, Subcommand)]
pub enum DocsCmd {
    /// Show the docs' folder structure as a tree.
    #[command(after_help = "\
EXAMPLES
  shelbi docs tree                     Every folder
  shelbi docs tree --documents         Folders and their documents
  shelbi docs tree Plans -L 1          One level under Plans
  shelbi docs tree --format json       JSON nodes (path, name, title, kind, children)")]
    Tree {
        /// The folder to start at (the docs' root when omitted).
        path: Option<String>,
        /// Descend at most N folder levels.
        #[arg(short = 'L', value_name = "N")]
        level: Option<usize>,
        /// Include documents.
        #[arg(long)]
        documents: bool,
    },
    /// List one folder's entries, or a document's headings.
    #[command(after_help = "\
EXAMPLES
  shelbi docs ls                       The docs' root
  shelbi docs ls Plans                 One folder
  shelbi docs ls Plans --stats         Its documents with line, word and size counts
  shelbi docs ls Plans/docs.md         A document's headings
  shelbi docs ls Plans/docs.md --stats Headings with counts")]
    Ls {
        /// A folder, or a document (`.md`) for its headings.
        path: Option<String>,
        /// Show line, word and size counts.
        #[arg(long)]
        stats: bool,
    },
    /// Print a document, or one section of it.
    #[command(after_help = "\
EXAMPLES
  shelbi docs read Plans/docs.md                       The whole document
  shelbi docs read Plans/docs.md --section \"## Design\" Just one section
  shelbi docs read Plans/docs.md --head 20             The first 20 lines
  shelbi docs read Plans/docs.md --tail 10             The last 10 lines
  shelbi docs read Plans/docs.md --format json         JSON with path and title")]
    Read {
        /// The document, relative to the docs' root.
        path: String,
        #[command(flatten)]
        section: SectionArg,
        #[command(flatten)]
        lines: LineArgs,
    },
    /// Print several documents, or every document in a folder.
    #[command(after_help = "\
EXAMPLES
  shelbi docs cat Plans/                         Every document in Plans
  shelbi docs cat Plans/a.md Plans/b.md          Two documents
  shelbi docs cat Plans/ --headers               With a `--- path ---` line before each
  shelbi docs cat Plans/ --head 50               The first 50 lines of it all")]
    Cat {
        /// Documents, or folders (each expands to the documents directly in
        /// it).
        #[arg(required = true)]
        paths: Vec<String>,
        /// Put a `--- path ---` line before each document.
        #[arg(long)]
        headers: bool,
        #[command(flatten)]
        lines: LineArgs,
    },
    /// Create a document (a path ending in `.md`) or a folder.
    #[command(after_help = "\
A document gets `# <title>` as its first line: --title, else one made from its
file name, unless its content starts with an H1 of its own. A folder's
--content is its description. Content may also be piped on stdin.

EXAMPLES
  shelbi docs new Design                               A folder
  shelbi docs new Design --content \"Design docs\"       A folder with a description
  shelbi docs new Plans/api.md --title \"API Design\"    A document
  cat notes.md | shelbi docs new Plans/notes.md        Content from stdin")]
    New {
        /// `path.md` for a document; any other path is a folder.
        path: String,
        /// The document's title (its H1).
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        content: ContentArg,
    },
    /// Open a document in $EDITOR, then save it through the hub.
    #[command(after_help = "\
The document is copied to a temporary file and opened in $EDITOR (else $VISUAL,
else vi). When the editor exits, a changed file is saved through the hub. If
the document changed elsewhere meanwhile, nothing is saved: the command exits 4
and prints where your copy is.

EXAMPLES
  shelbi docs edit Plans/docs.md
  EDITOR=\"code --wait\" shelbi docs edit Plans/docs.md")]
    Edit {
        /// The document to edit.
        path: String,
    },
    /// Replace a document's text, or one section's body.
    #[command(after_help = "\
Without --section the document's text is replaced, but its frontmatter and H1
stay unless the new content brings its own (--title sets the H1). With
--section only that section's body is replaced; its heading stays. Content
comes from --content or stdin (--content wins). The document must exist.

EXAMPLES
  shelbi docs write Plans/docs.md --content \"New body\"
  cat updated.md | shelbi docs write Plans/docs.md
  shelbi docs write Plans/docs.md --section \"## Goals\" --content \"- Ship it\"")]
    Write {
        path: String,
        #[command(flatten)]
        section: SectionArg,
        /// Set the document's H1 (not with --section).
        #[arg(long, conflicts_with = "section")]
        title: Option<String>,
        #[command(flatten)]
        content: ContentArg,
    },
    /// Add content to the end of a document or section.
    #[command(after_help = "\
With --section the content goes at the end of that section, before the next
heading at its level or higher. Frontmatter is never touched.

EXAMPLES
  shelbi docs append Plans/log.md --content \"- Fixed the bug\"
  echo \"## Notes\" | shelbi docs append Plans/docs.md
  shelbi docs append Plans/docs.md --section \"## Goals\" --content \"- One more\"")]
    Append {
        path: String,
        #[command(flatten)]
        section: SectionArg,
        #[command(flatten)]
        content: ContentArg,
    },
    /// Add content to the start of a document (after its frontmatter) or
    /// section (after its heading).
    #[command(after_help = "\
EXAMPLES
  shelbi docs prepend Plans/log.md --content \"Latest first.\"
  shelbi docs prepend Plans/docs.md --section \"## Goals\" --content \"- First\"")]
    Prepend {
        path: String,
        #[command(flatten)]
        section: SectionArg,
        #[command(flatten)]
        content: ContentArg,
    },
    /// Move or rename a document or folder.
    #[command(after_help = "\
A destination that is an existing folder receives the source inside it.

EXAMPLES
  shelbi docs mv Plans/old.md Plans/new.md       Rename a document
  shelbi docs mv Plans/doc.md Archive/           Move it into another folder
  shelbi docs mv Design \"Design System\"          Rename a folder")]
    Mv { source: String, destination: String },
    /// Copy a document or folder.
    #[command(after_help = "\
EXAMPLES
  shelbi docs cp Plans/template.md Plans/new.md
  shelbi docs cp Plans Archive/plans-2026")]
    Cp { source: String, destination: String },
    /// Remove a document or folder, or one section of a document.
    #[command(after_help = "\
Removed files stay in the docs' history (`shelbi docs log`); nothing goes to
the system Trash.

EXAMPLES
  shelbi docs rm Plans/old.md                          A document
  shelbi docs rm Archive                               A folder
  shelbi docs rm Plans/docs.md --section \"## Q2\"       One section")]
    Rm {
        path: String,
        #[command(flatten)]
        section: SectionArg,
    },
    /// Search the docs' documents.
    #[command(after_help = "\
Case-insensitive substring search by default. Each matching line prints as
`path:line: text`.

EXAMPLES
  shelbi docs search \"auth\"                          The whole docs
  shelbi docs search \"auth\" --folder Plans           One folder
  shelbi docs search \"auth\" --document Plans/a.md    One document
  shelbi docs search \"PIX-\\d+\" --regex              A regular expression
  shelbi docs search \"auth\" --format json")]
    Search {
        query: String,
        /// Only documents under this folder.
        #[arg(long, conflicts_with = "document")]
        folder: Option<String>,
        /// Only this document.
        #[arg(long)]
        document: Option<String>,
        /// Treat the query as a regular expression.
        #[arg(long)]
        regex: bool,
    },
    /// List the checkboxes (`- [ ]`, `- [x]`) in a document or folder.
    #[command(after_help = "\
EXAMPLES
  shelbi docs tasks Plans/docs.md                     Every task in a document
  shelbi docs tasks Plans/docs.md --pending           Only the open ones
  shelbi docs tasks Plans/                            Across a folder's documents
  shelbi docs tasks Plans/docs.md --section \"## Q2\"")]
    Tasks {
        /// A document, or a folder (its documents directly in it).
        path: String,
        #[command(flatten)]
        section: SectionArg,
        /// Only tasks not yet checked.
        #[arg(long)]
        pending: bool,
    },
    /// Check a task: the one whose text contains TEXT (case-insensitive).
    #[command(after_help = "\
Fails (exit 2) when no task matches and (exit 3) when more than one does.

EXAMPLES
  shelbi docs check Plans/docs.md \"Launch the CLI\"
  shelbi docs check Plans/docs.md \"Launch\" --section \"## Q2\"")]
    Check {
        path: String,
        text: String,
        #[command(flatten)]
        section: SectionArg,
    },
    /// Uncheck a task: the one whose text contains TEXT (case-insensitive).
    #[command(after_help = "\
EXAMPLES
  shelbi docs uncheck Plans/docs.md \"Launch the CLI\"
  shelbi docs uncheck Plans/docs.md \"Launch\" --section \"## Q2\"")]
    Uncheck {
        path: String,
        text: String,
        #[command(flatten)]
        section: SectionArg,
    },
    /// Show the docs (or a document or folder) in the desktop app.
    #[command(after_help = "\
Asks every client showing the project to open its Documentation there, then starts the
desktop app (macOS) unless --no-launch.

EXAMPLES
  shelbi docs open                     The docs
  shelbi docs open Plans/docs.md       One document
  shelbi docs open Plans --no-launch   Only tell an app that's already open")]
    Open {
        path: Option<String>,
        /// Don't start the desktop app; only tell the clients already open.
        #[arg(long)]
        no_launch: bool,
    },
    /// Show the change not yet committed, as a unified diff.
    #[command(after_help = "\
The hub commits a few seconds after the last change, so this is usually empty
or very recent.

EXAMPLES
  shelbi docs diff                     The whole docs
  shelbi docs diff Plans/docs.md       One document")]
    Diff {
        /// A document or folder (the whole docs when omitted).
        path: Option<String>,
    },
    /// Show the commits that changed the docs (or a document or folder).
    #[command(after_help = "\
EXAMPLES
  shelbi docs log                      The latest 20 commits
  shelbi docs log Plans/docs.md -n 5   A document's latest 5 (follows renames)
  shelbi docs log --format json")]
    Log {
        path: Option<String>,
        /// Show at most N commits.
        #[arg(short = 'n', long, value_name = "N", default_value_t = 20)]
        limit: usize,
    },
    /// Show the sync's status, or sync now, pause or resume it.
    #[command(after_help = "\
EXAMPLES
  shelbi docs sync                     The status (same as `sync status`)
  shelbi docs sync now                 Commit, pull and push, and wait for it
  shelbi docs sync pause               Stop syncing on the clock
  shelbi docs sync resume")]
    Sync {
        #[arg(value_enum, default_value_t = SyncAction::Status)]
        action: SyncAction,
    },
    /// Settle a document the sync couldn't merge: keep ours or take theirs.
    #[command(after_help = "\
A document edited here and elsewhere in a way the merge can't combine is held:
origin keeps the other version, this machine keeps ours, and the document
stays out of the sync until it's resolved. `shelbi docs sync status` lists the
held documents. Without --ours or --theirs this shows the document's hunks,
the runs of lines where the two versions differ; the Documentation in the desktop app
and the TUI can choose a side per hunk. Resolving commits \"Resolve merge
conflicts\" and syncs.

EXAMPLES
  shelbi docs resolve Plans/auth.md            Show the hunks
  shelbi docs resolve Plans/auth.md --ours     Keep this machine's version
  shelbi docs resolve Plans/auth.md --theirs   Take the version from elsewhere")]
    Resolve {
        path: String,
        /// Keep this machine's version whole.
        #[arg(long, conflicts_with = "theirs")]
        ours: bool,
        /// Take the version from elsewhere whole.
        #[arg(long)]
        theirs: bool,
    },
    /// Set up the project's docs: write the project YAML's `docs:` block and
    /// have the hub create it.
    #[command(after_help = "\
By default the docs are the orphan branch docs/planning, checked out by the hub
beside the repo and created (and pushed) when origin doesn't have it. With
--source folder it's a folder of the repo (docs unless --path says), whose
edits go to one rolling pull request. Run it again to change the source,
branch or folder; flags left out keep what the block already says.

EXAMPLES
  shelbi docs init                           The orphan branch docs/planning
  shelbi docs init --branch docs             Another orphan branch
  shelbi docs init --source folder           The repo's docs/ folder
  shelbi docs init --path handbook           The repo's handbook/ folder")]
    Init {
        /// Where the docs live: `branch` (an orphan branch) or `folder` (a
        /// folder of the repo). Inferred from --branch or --path when left
        /// out.
        #[arg(long, value_enum)]
        source: Option<SourceArg>,
        /// The orphan branch (`--source branch`; default docs/planning).
        #[arg(long, value_name = "BRANCH")]
        branch: Option<String>,
        /// The repo folder, relative to its root (`--source folder`;
        /// default docs).
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SourceArg {
    Branch,
    Folder,
}

impl From<SourceArg> for DocsSource {
    fn from(s: SourceArg) -> Self {
        match s {
            SourceArg::Branch => DocsSource::Branch,
            SourceArg::Folder => DocsSource::Folder,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SyncAction {
    Status,
    Now,
    Pause,
    Resume,
}

#[derive(Debug, Args)]
pub struct SectionArg {
    /// Target one section by its heading, as written (e.g. "## Goals").
    #[arg(long, value_name = "HEADING")]
    section: Option<String>,
}

#[derive(Debug, Args)]
pub struct LineArgs {
    /// Only the first N lines.
    #[arg(long, value_name = "N")]
    head: Option<usize>,
    /// Only the last N lines (wins over --head).
    #[arg(long, value_name = "N")]
    tail: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ContentArg {
    /// The content (else stdin; `-` reads stdin explicitly). It may start
    /// with `-` (a list item, frontmatter).
    #[arg(long, allow_hyphen_values = true)]
    content: Option<String>,
}

/// Where requests go: this machine's hub in production.
pub trait Hub {
    fn ask(&self, req: DocsRequest) -> DocsReply;

    /// The project YAML's `docs:` block (`init` reads it first).
    fn docs_block(&self) -> Result<Option<DocsConfig>, Failure> {
        Err(Failure::failed("this hub can't read the project's settings"))
    }

    /// Write the project YAML's `docs:` block (`init`).
    fn write_docs_block(&self, _block: &DocsConfig) -> Result<(), Failure> {
        Err(Failure::failed("this hub can't write the project's settings"))
    }
}

/// This machine's hub daemon, started on demand.
struct LocalHub {
    project: String,
}

impl Hub for LocalHub {
    fn ask(&self, req: DocsRequest) -> DocsReply {
        shelbi_app::docs::ask_local(&self.project, req)
    }

    fn docs_block(&self) -> Result<Option<DocsConfig>, Failure> {
        let config = shelbi_state::workflow_config::WorkflowConfig::load(&self.project)
            .map_err(|e| Failure::failed(format!("reading the project's settings: {e}")))?;
        Ok(config.docs().cloned())
    }

    fn write_docs_block(&self, block: &DocsConfig) -> Result<(), Failure> {
        // The comment-preserving, hash-checked write Settings ▸ Documentation makes.
        let config = shelbi_state::workflow_config::WorkflowConfig::load(&self.project)
            .map_err(|e| Failure::failed(format!("reading the project's settings: {e}")))?;
        let next = config.set_docs(Some(block.clone())).map_err(|e| Failure::invalid(e.to_string()))?;
        next.save()
            .map(|_| ())
            .map_err(|e| Failure::failed(format!("writing the project's settings: {e}")))
    }
}

/// What a command ends with when it fails: a message and an exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Invalid,
            message: message.into(),
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Failed,
            message: message.into(),
        }
    }

    fn from_wire(f: DocsFailure) -> Self {
        Self {
            kind: f.kind,
            message: f.message,
        }
    }
}

type Outcome = Result<String, Failure>;

/// `shelbi docs …`: run the command and exit with its code.
pub fn run(project: Option<String>, args: DocsArgs) -> anyhow::Result<()> {
    let json = args.agent || args.format == Format::Json;
    let result = super::require_project(project)
        .map_err(|e| Failure::invalid(e.to_string()))
        .and_then(|project| {
            let hub = LocalHub { project };
            let input = StdinInput;
            execute(&hub, &input, args.cmd, json)
        });
    match result {
        Ok(out) => {
            print!("{out}");
            if !out.is_empty() && !out.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        Err(f) => {
            if json {
                eprintln!(
                    "{}",
                    json!({ "error": { "kind": kind_word(f.kind), "message": f.message } })
                );
            } else {
                eprintln!("shelbi docs: {}", f.message);
            }
            std::process::exit(f.kind.exit_code());
        }
    }
}

fn kind_word(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::NotFound => "not_found",
        FailureKind::Invalid => "invalid",
        FailureKind::Exists => "exists",
        FailureKind::Conflict => "conflict",
        FailureKind::NotSetUp => "not_set_up",
        FailureKind::Failed => "failed",
    }
}

/// Where content not given with `--content` comes from.
pub trait Input {
    /// Stdin, read to its end, when it's piped (`None` from a terminal).
    /// `wait` false only reads what is already there or arriving at once,
    /// so an optional read never blocks on a pipe held open with nothing in
    /// it (an agent's shell).
    fn stdin(&self, wait: bool) -> Result<Option<String>, Failure>;
    /// Run the editor on `file`, waiting for it to exit.
    fn edit(&self, file: &std::path::Path) -> Result<(), Failure>;
}

struct StdinInput;

impl Input for StdinInput {
    fn stdin(&self, wait: bool) -> Result<Option<String>, Failure> {
        let stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Ok(None);
        }
        if !wait && !stdin_ready(std::time::Duration::from_millis(250)) {
            return Ok(None);
        }
        let mut buf = String::new();
        stdin
            .lock()
            .read_to_string(&mut buf)
            .map_err(|e| Failure::failed(format!("reading stdin: {e}")))?;
        Ok(Some(buf))
    }

    fn edit(&self, file: &std::path::Path) -> Result<(), Failure> {
        let argv = editor_argv(
            file,
            std::env::var("EDITOR").ok().as_deref(),
            std::env::var("VISUAL").ok().as_deref(),
        );
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .map_err(|e| Failure::failed(format!("couldn't start the editor `{}`: {e}", argv[0])))?;
        if !status.success() {
            return Err(Failure::failed(format!(
                "the editor exited with {}; nothing was saved (your copy: {})",
                status,
                file.display()
            )));
        }
        Ok(())
    }
}

/// Whether stdin has something to read (or is at its end) within `wait`.
#[cfg(unix)]
fn stdin_ready(wait: std::time::Duration) -> bool {
    let mut fds = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one valid pollfd for fd 0, for the duration of the call.
    let n = unsafe { libc::poll(&mut fds, 1, ms) };
    n > 0 && fds.revents & (libc::POLLIN | libc::POLLHUP) != 0
}

#[cfg(not(unix))]
fn stdin_ready(_wait: std::time::Duration) -> bool {
    true
}

pub use shelbi_app::docs::editor_argv;

/// Run one command against `hub` and answer what it prints.
pub fn execute(hub: &dyn Hub, input: &dyn Input, cmd: DocsCmd, json: bool) -> Outcome {
    let by = Author::of_this_process();
    match cmd {
        DocsCmd::Tree { path, level, documents } => {
            let nodes = expect_tree(hub.ask(DocsRequest::Tree { path: clean_opt(path.clone()) }))?;
            Ok(render_tree(path.as_deref(), &nodes, level, documents, json))
        }
        DocsCmd::Ls { path, stats } => {
            let path = clean_opt(path);
            let entries = expect_entries(hub.ask(DocsRequest::List { path: path.clone() }))?;
            Ok(render_ls(&entries, stats, json))
        }
        DocsCmd::Read { path, section, lines } => {
            let path = clean(&path)?;
            let content = expect_doc(hub.ask(DocsRequest::Read {
                path: path.clone(),
                section: section.section,
            }))?;
            let content = shelbi_docs::markdown::compose::head_tail(&content, lines.head, lines.tail);
            Ok(if json {
                let title = shelbi_docs::markdown::sections::extract_title(&content).unwrap_or_default();
                pretty(&json!({ "path": path, "title": title, "content": content }))
            } else {
                content
            })
        }
        DocsCmd::Cat { paths, headers, lines } => cat(hub, &paths, headers, lines, json),
        DocsCmd::New { path, title, content } => {
            let path = clean(&path)?;
            let content = optional_content(input, content.content)?;
            let is_doc = path.ends_with(".md");
            if title.is_some() && !is_doc {
                return Err(Failure::invalid("--title is for a document (a path ending in .md)"));
            }
            expect_done(hub.ask(DocsRequest::New {
                path: path.clone(),
                content,
                title,
                by,
            }))?;
            let what = if is_doc { "document" } else { "folder" };
            Ok(done(json, &format!("Created {what}: {path}"), json!({ "created": what, "path": path })))
        }
        DocsCmd::Edit { path } => edit(hub, input, &clean(&path)?, by, json),
        DocsCmd::Write {
            path,
            section,
            title,
            content,
        } => {
            let path = clean(&path)?;
            let content = required_content(input, content.content)?;
            let req = match section.section {
                Some(h) => DocsRequest::Write {
                    path: path.clone(),
                    content,
                    section: Some(h),
                    by,
                },
                None => DocsRequest::Overwrite {
                    path: path.clone(),
                    content,
                    title,
                    by,
                },
            };
            expect_done(hub.ask(req))?;
            Ok(done(json, &format!("Wrote {path}"), json!({ "written": path })))
        }
        DocsCmd::Append { path, section, content } => {
            let path = clean(&path)?;
            let content = required_content(input, content.content)?;
            expect_done(hub.ask(DocsRequest::Append {
                path: path.clone(),
                content,
                section: section.section,
                by,
            }))?;
            Ok(done(json, &format!("Appended to {path}"), json!({ "appended": path })))
        }
        DocsCmd::Prepend { path, section, content } => {
            let path = clean(&path)?;
            let content = required_content(input, content.content)?;
            expect_done(hub.ask(DocsRequest::Prepend {
                path: path.clone(),
                content,
                section: section.section,
                by,
            }))?;
            Ok(done(json, &format!("Prepended to {path}"), json!({ "prepended": path })))
        }
        DocsCmd::Mv { source, destination } => {
            let (from, to) = (clean(&source)?, clean(&destination)?);
            expect_done(hub.ask(DocsRequest::Move {
                from: from.clone(),
                to: to.clone(),
                by,
            }))?;
            Ok(done(json, &format!("Moved {from} → {to}"), json!({ "moved": from, "to": to })))
        }
        DocsCmd::Cp { source, destination } => {
            let (from, to) = (clean(&source)?, clean(&destination)?);
            expect_done(hub.ask(DocsRequest::Copy {
                from: from.clone(),
                to: to.clone(),
                by,
            }))?;
            Ok(done(json, &format!("Copied {from} → {to}"), json!({ "copied": from, "to": to })))
        }
        DocsCmd::Rm { path, section } => {
            let path = clean(&path)?;
            let heading = section.section.clone();
            expect_done(hub.ask(DocsRequest::Remove {
                path: path.clone(),
                section: section.section,
                by,
            }))?;
            Ok(match heading {
                Some(h) => done(
                    json,
                    &format!("Removed section \"{h}\" from {path}"),
                    json!({ "removed": path, "section": h }),
                ),
                None => done(json, &format!("Removed: {path}"), json!({ "removed": path })),
            })
        }
        DocsCmd::Search {
            query,
            folder,
            document,
            regex,
        } => {
            if query.is_empty() {
                return Err(Failure::invalid("the search query is empty"));
            }
            let document = clean_opt(document);
            let folder = match &document {
                Some(doc) => doc.rsplit_once('/').map(|(dir, _)| dir.to_string()),
                None => clean_opt(folder),
            };
            let mut matches = expect_matches(hub.ask(DocsRequest::Search { query, folder, regex }))?;
            if let Some(doc) = &document {
                matches.retain(|m| &m.path == doc);
            }
            Ok(if json {
                pretty(&serde_json::to_value(&matches).unwrap_or(Value::Null))
            } else {
                matches.iter().map(|m| format!("{}:{}: {}\n", m.path, m.line, m.text.trim_end())).collect()
            })
        }
        DocsCmd::Tasks { path, section, pending } => {
            let path = clean(&path)?;
            let folder = path.is_empty() || !path.ends_with(".md");
            let tasks = match hub.ask(DocsRequest::Tasks {
                path,
                section: section.section,
                pending,
            }) {
                DocsReply::Tasks(t) => t,
                other => return Err(unexpected(other)),
            };
            Ok(if json {
                pretty(&serde_json::to_value(&tasks).unwrap_or(Value::Null))
            } else {
                tasks
                    .iter()
                    .map(|t| {
                        let mark = if t.checked { "[x]" } else { "[ ]" };
                        if folder {
                            format!("{}: {mark} {}\n", t.path, t.text)
                        } else {
                            format!("{mark} {}\n", t.text)
                        }
                    })
                    .collect()
            })
        }
        DocsCmd::Check { path, text, section } => set_task(hub, path, text, section, true, by, json),
        DocsCmd::Uncheck { path, text, section } => set_task(hub, path, text, section, false, by, json),
        DocsCmd::Open { path, no_launch } => {
            let path = clean_opt(path);
            expect_done(hub.ask(DocsRequest::Open { path: path.clone() }))?;
            if !no_launch {
                launch_desktop()?;
            }
            let shown = path.clone().unwrap_or_else(|| "the docs".into());
            Ok(done(json, &format!("Opened {shown} in the desktop app"), json!({ "opened": path })))
        }
        DocsCmd::Diff { path } => {
            let path = clean_opt(path);
            let diff = match hub.ask(DocsRequest::Diff { path: path.clone() }) {
                DocsReply::Text(t) => t,
                other => return Err(unexpected(other)),
            };
            Ok(if json {
                pretty(&json!({ "path": path, "diff": diff }))
            } else {
                diff
            })
        }
        DocsCmd::Log { path, limit } => {
            let commits = match hub.ask(DocsRequest::Log {
                path: clean_opt(path),
                limit,
            }) {
                DocsReply::Log(c) => c,
                other => return Err(unexpected(other)),
            };
            Ok(if json {
                pretty(&serde_json::to_value(&commits).unwrap_or(Value::Null))
            } else {
                commits
                    .iter()
                    .map(|c| {
                        let sha: String = c.sha.chars().take(7).collect();
                        let date = c.date.get(..16).unwrap_or(&c.date).replace('T', " ");
                        format!("{sha}  {date}  {}  {}\n", c.author, c.subject)
                    })
                    .collect()
            })
        }
        DocsCmd::Sync { action } => {
            let req = match action {
                SyncAction::Status => DocsRequest::Status,
                SyncAction::Now => DocsRequest::Sync(SyncOp::Now),
                SyncAction::Pause => DocsRequest::Sync(SyncOp::Pause),
                SyncAction::Resume => DocsRequest::Sync(SyncOp::Resume),
            };
            match hub.ask(req) {
                DocsReply::Status(s) => Ok(render_status(&s, json)),
                other => Err(unexpected(other)),
            }
        }
        DocsCmd::Resolve { path, ours, theirs } => {
            let path = clean(&path)?;
            let resolution = match (ours, theirs) {
                (true, _) => Resolution::Ours,
                (_, true) => Resolution::Theirs,
                _ => {
                    let conflict = match hub.ask(DocsRequest::Conflict { path: path.clone() }) {
                        DocsReply::Conflict(c) => c,
                        other => return Err(unexpected(other)),
                    };
                    return Ok(render_conflict(&conflict, json));
                }
            };
            let side = if ours { "ours" } else { "theirs" };
            expect_done(hub.ask(DocsRequest::Resolve {
                path: path.clone(),
                resolution,
                by,
            }))?;
            Ok(done(
                json,
                &format!("Resolved {path} ({side}); committed \"Resolve merge conflicts\""),
                json!({ "resolved": path, "side": side }),
            ))
        }
        DocsCmd::Init { source, branch, path } => init(hub, source.map(DocsSource::from), branch, path, json),
    }
}

/// `resolve <path>` without a side: the held document's hunks, ours above
/// theirs, with a few lines of context.
pub fn render_conflict(c: &DocsConflict, json: bool) -> String {
    use shelbi_app::docs_conflict::{context_shown, diff_of, hidden_label, revision};
    let diff = diff_of(c);
    if json {
        let hunks: Vec<Value> = diff
            .hunks
            .iter()
            .map(|h| json!({ "context_before": h.context_before, "ours": h.ours, "theirs": h.theirs }))
            .collect();
        return pretty(&json!({
            "path": c.path,
            "detected_at": c.detected_at,
            "ours_deleted": c.ours.is_none(),
            "theirs_deleted": c.theirs.is_none(),
            "revision": revision(c),
            "hunks": hunks,
            "trailing_context": diff.trailing_context,
        }));
    }
    let hunks = match diff.hunks.len() {
        1 => "1 hunk".to_string(),
        n => format!("{n} hunks"),
    };
    let mut out = format!("{} couldn't be merged ({hunks})\n", c.path);
    if c.ours.is_none() {
        out.push_str("Ours: deleted here\n");
    }
    if c.theirs.is_none() {
        out.push_str("Theirs: deleted elsewhere\n");
    }
    for (i, h) in diff.hunks.iter().enumerate() {
        out.push_str(&format!("\n@@ hunk {} @@\n", i + 1));
        let (context, hidden) = context_shown(&h.context_before, true);
        if hidden > 0 {
            out.push_str(&format!("  {}\n", hidden_label(hidden)));
        }
        for line in context {
            out.push_str(&format!("  {line}\n"));
        }
        out.push_str("<<<<<<< ours\n");
        for line in &h.ours {
            out.push_str(&format!("{line}\n"));
        }
        out.push_str("=======\n");
        for line in &h.theirs {
            out.push_str(&format!("{line}\n"));
        }
        out.push_str(">>>>>>> theirs\n");
    }
    out.push_str(&format!(
        "\nshelbi docs resolve {} --ours | --theirs\n",
        shell_word(&c.path)
    ));
    out
}

/// `path` as one shell word.
fn shell_word(path: &str) -> String {
    if path.chars().all(|ch| ch.is_ascii_alphanumeric() || "/._-".contains(ch)) {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

/// `init`: the block the flags ask for (over the one the project YAML has),
/// written when it changed, then the hub's setup.
fn init(
    hub: &dyn Hub,
    source: Option<DocsSource>,
    branch: Option<String>,
    path: Option<String>,
    json: bool,
) -> Outcome {
    let source = match (source, &branch, &path) {
        (Some(s), _, _) => Some(s),
        (None, Some(_), Some(_)) => {
            return Err(Failure::invalid("pass --branch (an orphan branch) or --path (a repo folder), not both"))
        }
        (None, Some(_), None) => Some(DocsSource::Branch),
        (None, None, Some(_)) => Some(DocsSource::Folder),
        (None, None, None) => None,
    };
    match source {
        Some(DocsSource::Folder) if branch.is_some() => {
            return Err(Failure::invalid("--branch is for --source branch; folder docs take --path"))
        }
        Some(DocsSource::Branch) if path.is_some() => {
            return Err(Failure::invalid("--path is for --source folder; branch docs take --branch"))
        }
        _ => {}
    }
    let existing = hub.docs_block()?;
    let mut block = existing.clone().unwrap_or_default();
    if let Some(s) = source {
        block.source = s;
    }
    if let Some(b) = branch {
        block.branch = Some(b.trim().to_string());
    }
    if let Some(p) = path {
        block.path = Some(p.trim().trim_end_matches('/').to_string()).filter(|p| !p.is_empty());
    }
    let block = shelbi_app::settings::docs::filled(block);
    block.validate().map_err(|e| Failure::invalid(e.to_string()))?;
    let wrote = existing.as_ref() != Some(&block);
    if wrote {
        hub.write_docs_block(&block)?;
    }
    let status = match hub.ask(DocsRequest::Init) {
        DocsReply::Status(s) => s,
        DocsReply::Failed(f) => {
            let mut f = Failure::from_wire(f);
            if wrote {
                f.message.push_str(" (the `docs:` block is written; the hub tries again on its own)");
            }
            return Err(f);
        }
        other => return Err(unexpected(other)),
    };
    if json {
        let mut value: Value = serde_json::from_str(&render_status(&status, true)).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert("wrote_block".into(), Value::Bool(wrote));
        }
        return Ok(pretty(&value));
    }
    let place = match block.source {
        DocsSource::Branch => format!("the orphan branch {}", block.worktree_branch()),
        DocsSource::Folder => format!("the repo folder {}/", block.folder().unwrap_or("docs")),
    };
    let lead = if wrote {
        format!("The docs are set up on {place}.\n")
    } else {
        format!("The docs were already set up on {place}.\n")
    };
    Ok(format!("{lead}{}", render_status(&status, false)))
}

/// A path as the hub takes it: no leading or trailing `/`. `""` (or `/`)
/// is the root.
fn clean(path: &str) -> Result<String, Failure> {
    Ok(path.trim().trim_matches('/').to_string())
}

fn clean_opt(path: Option<String>) -> Option<String> {
    path.map(|p| p.trim().trim_matches('/').to_string()).filter(|p| !p.is_empty())
}

/// `--content`, else stdin; refused when neither has anything.
fn required_content(input: &dyn Input, content: Option<String>) -> Result<String, Failure> {
    let text = match content.as_deref() {
        Some("-") | None => input.stdin(true)?,
        Some(_) => content,
    };
    match text {
        Some(t) if !t.is_empty() => Ok(t),
        _ => Err(Failure::invalid("no content: pass --content or pipe it on stdin")),
    }
}

/// `--content`, else whatever stdin already holds.
fn optional_content(input: &dyn Input, content: Option<String>) -> Result<Option<String>, Failure> {
    match content.as_deref() {
        Some("-") => input.stdin(true),
        Some(_) => Ok(content),
        None => Ok(input.stdin(false)?.filter(|t| !t.is_empty())),
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default() + "\n"
}

/// A write's confirmation: `line` as text, `{"ok": true, ...fields}` as JSON.
fn done(json: bool, line: &str, fields: Value) -> String {
    if json {
        let mut obj = serde_json::Map::new();
        obj.insert("ok".into(), Value::Bool(true));
        if let Value::Object(f) = fields {
            obj.extend(f);
        }
        pretty(&Value::Object(obj))
    } else {
        format!("{line}\n")
    }
}

fn unexpected(reply: DocsReply) -> Failure {
    match reply {
        DocsReply::Failed(f) => Failure::from_wire(f),
        other => Failure::failed(format!("the hub answered something else: {other:?}")),
    }
}

fn expect_done(reply: DocsReply) -> Result<(), Failure> {
    match reply {
        DocsReply::Done => Ok(()),
        other => Err(unexpected(other)),
    }
}

fn expect_doc(reply: DocsReply) -> Result<String, Failure> {
    match reply {
        DocsReply::Doc { content, .. } => Ok(content),
        other => Err(unexpected(other)),
    }
}

fn expect_tree(reply: DocsReply) -> Result<Vec<DocsNode>, Failure> {
    match reply {
        DocsReply::Tree(nodes) => Ok(nodes),
        other => Err(unexpected(other)),
    }
}

fn expect_entries(reply: DocsReply) -> Result<Vec<DocsEntry>, Failure> {
    match reply {
        DocsReply::Entries(e) => Ok(e),
        other => Err(unexpected(other)),
    }
}

fn expect_matches(reply: DocsReply) -> Result<Vec<shelbi_app::docs::DocsMatch>, Failure> {
    match reply {
        DocsReply::Matches(m) => Ok(m),
        other => Err(unexpected(other)),
    }
}

fn set_task(
    hub: &dyn Hub,
    path: String,
    text: String,
    section: SectionArg,
    checked: bool,
    by: Author,
    json: bool,
) -> Outcome {
    let path = clean(&path)?;
    if text.trim().is_empty() {
        return Err(Failure::invalid("the task text is empty"));
    }
    expect_done(hub.ask(DocsRequest::SetTask {
        path: path.clone(),
        text: text.clone(),
        section: section.section,
        checked,
        by,
    }))?;
    let verb = if checked { "Checked" } else { "Unchecked" };
    Ok(done(
        json,
        &format!("{verb} \"{text}\" in {path}"),
        json!({ "path": path, "task": text, "checked": checked }),
    ))
}

/// `cat`: each path a document, or a folder's documents directly in it.
fn cat(hub: &dyn Hub, paths: &[String], headers: bool, lines: LineArgs, json: bool) -> Outcome {
    use shelbi_docs::markdown::compose::head_tail;
    let mut docs: Vec<(String, String)> = Vec::new();
    for raw in paths {
        let path = clean(raw)?;
        let as_folder = path.is_empty() || raw.ends_with('/') || !path.ends_with(".md");
        if as_folder {
            match hub.ask(DocsRequest::List {
                path: Some(path.clone()).filter(|p| !p.is_empty()),
            }) {
                DocsReply::Entries(entries) => {
                    for e in entries.into_iter().filter(|e| e.kind == EntryKind::Document) {
                        let content = expect_doc(hub.ask(DocsRequest::Read {
                            path: e.path.clone(),
                            section: None,
                        }))?;
                        docs.push((e.path, content));
                    }
                    continue;
                }
                // A document without the `.md` a folder check assumed.
                DocsReply::Failed(f) if f.kind == FailureKind::NotFound && !path.is_empty() => {}
                other => return Err(unexpected(other)),
            }
        }
        let content = expect_doc(hub.ask(DocsRequest::Read {
            path: path.clone(),
            section: None,
        }))?;
        docs.push((path, content));
    }
    if json {
        let items: Vec<Value> = docs
            .iter()
            .map(|(p, c)| json!({ "path": p, "content": head_tail(c, lines.head, lines.tail) }))
            .collect();
        return Ok(pretty(&Value::Array(items)));
    }
    let mut parts = Vec::with_capacity(docs.len());
    for (i, (path, content)) in docs.iter().enumerate() {
        let mut part = String::new();
        if headers {
            if i > 0 {
                part.push('\n');
            }
            part.push_str(&format!("--- {path} ---\n\n"));
        }
        part.push_str(content);
        parts.push(part);
    }
    let joined = if headers { parts.join("") } else { parts.join("\n") };
    Ok(head_tail(&joined, lines.head, lines.tail))
}

/// `edit`: a temporary copy through `$EDITOR`, saved back unless the
/// document changed meanwhile.
fn edit(hub: &dyn Hub, input: &dyn Input, path: &str, by: Author, json: bool) -> Outcome {
    if !path.ends_with(".md") {
        return Err(Failure::invalid(format!("`{path}` isn't a document (a path ending in .md)")));
    }
    let original = expect_doc(hub.ask(DocsRequest::Read {
        path: path.to_string(),
        section: None,
    }))?;
    let name = path.rsplit('/').next().unwrap_or(path);
    let dir = std::env::temp_dir().join(format!("shelbi-docs-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| Failure::failed(format!("creating {}: {e}", dir.display())))?;
    let file: PathBuf = dir.join(name);
    std::fs::write(&file, &original).map_err(|e| Failure::failed(format!("writing {}: {e}", file.display())))?;
    input.edit(&file)?;
    let edited =
        std::fs::read_to_string(&file).map_err(|e| Failure::failed(format!("reading {}: {e}", file.display())))?;
    if edited == original {
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(done(json, &format!("No changes to {path}"), json!({ "path": path, "changed": false })));
    }
    let now = expect_doc(hub.ask(DocsRequest::Read {
        path: path.to_string(),
        section: None,
    }))?;
    if now != original {
        return Err(Failure {
            kind: FailureKind::Conflict,
            message: format!(
                "`{path}` changed elsewhere while you edited it; nothing was saved. Your copy is at {}",
                file.display()
            ),
        });
    }
    expect_done(hub.ask(DocsRequest::Write {
        path: path.to_string(),
        content: edited,
        section: None,
        by,
    }))?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(done(json, &format!("Saved {path}"), json!({ "path": path, "changed": true })))
}

/// Bring the desktop app up (macOS): it opens the Documentation the hub just
/// announced.
fn launch_desktop() -> Result<(), Failure> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let out = std::process::Command::new("open")
        .args(["-b", "dev.shelbi.desktop"])
        .output()
        .map_err(|e| Failure::failed(format!("couldn't start the desktop app: {e}")))?;
    if !out.status.success() {
        return Err(Failure::failed(format!(
            "told the open clients, but couldn't start the desktop app: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// `tree`: folders (and documents with `documents`), `level` folder levels
/// deep.
pub fn render_tree(root: Option<&str>, nodes: &[DocsNode], level: Option<usize>, documents: bool, json: bool) -> String {
    fn prune(nodes: &[DocsNode], depth: usize, level: Option<usize>, documents: bool) -> Vec<DocsNode> {
        nodes
            .iter()
            .filter(|n| documents || n.kind == NodeKind::Folder)
            .map(|n| DocsNode {
                children: if level.is_none_or(|max| depth < max) {
                    prune(&n.children, depth + 1, level, documents)
                } else {
                    Vec::new()
                },
                ..n.clone()
            })
            .collect()
    }
    let nodes = prune(nodes, 1, level, documents);
    if json {
        return pretty(&serde_json::to_value(&nodes).unwrap_or(Value::Null));
    }
    fn draw(nodes: &[DocsNode], prefix: &str, out: &mut String) {
        for (i, n) in nodes.iter().enumerate() {
            let last = i + 1 == nodes.len();
            out.push_str(prefix);
            out.push_str(if last { "└── " } else { "├── " });
            out.push_str(&n.name);
            if n.kind == NodeKind::Folder {
                out.push('/');
            }
            out.push('\n');
            draw(&n.children, &format!("{prefix}{}", if last { "    " } else { "│   " }), out);
        }
    }
    let mut out = format!("{}\n", root.map(|r| r.trim_matches('/')).filter(|r| !r.is_empty()).unwrap_or("."));
    draw(&nodes, "", &mut out);
    out
}

/// `ls`: a folder's entries (folders with a trailing `/`, then documents),
/// or a document's headings; with counts for `stats`.
pub fn render_ls(entries: &[DocsEntry], stats: bool, json: bool) -> String {
    let sections = entries.iter().any(|e| e.kind == EntryKind::Section);
    if json {
        let items: Vec<Value> = entries
            .iter()
            .map(|e| match e.kind {
                EntryKind::Folder => json!({
                    "type": "folder", "name": e.name, "path": e.path, "documents": e.documents,
                }),
                EntryKind::Document if stats => json!({
                    "type": "document", "name": e.name, "path": e.path, "title": e.title,
                    "lines": e.lines, "words": e.words, "bytes": e.bytes,
                }),
                EntryKind::Document => json!({
                    "type": "document", "name": e.name, "path": e.path, "title": e.title,
                }),
                EntryKind::Section if stats => json!({
                    "heading": e.name, "level": e.level, "lines": e.lines, "words": e.words,
                }),
                EntryKind::Section => json!({ "level": e.level, "text": e.name }),
            })
            .collect();
        return pretty(&Value::Array(items));
    }
    if sections {
        if !stats {
            return entries.iter().map(|e| format!("{}\n", e.name)).collect();
        }
        let width = entries
            .iter()
            .map(|e| e.name.chars().count() + 2 * e.level.saturating_sub(1) as usize)
            .max()
            .unwrap_or(0)
            .max(8);
        return entries
            .iter()
            .map(|e| {
                let label = format!("{}{}", "  ".repeat(e.level.saturating_sub(1) as usize), e.name);
                format!(
                    "{}({} lines, {} words)\n",
                    pad(&label, width + 4),
                    number(e.lines),
                    number(e.words)
                )
            })
            .collect();
    }
    let folders = entries.iter().filter(|e| e.kind == EntryKind::Folder);
    let docs: Vec<&DocsEntry> = entries.iter().filter(|e| e.kind == EntryKind::Document).collect();
    let mut out = String::new();
    for f in folders {
        out.push_str(&format!("{}/\n", f.name));
    }
    if !stats {
        for d in &docs {
            out.push_str(&format!("{}\n", d.name));
        }
        return out;
    }
    let width = docs.iter().map(|d| d.name.chars().count()).max().unwrap_or(0).max(8);
    let row = |label: &str, lines: usize, words: usize, bytes: u64| {
        format!(
            "{}{} lines  {} words  {}\n",
            pad(label, width + 2),
            pad(&number(lines), 8),
            pad(&number(words), 8),
            size(bytes)
        )
    };
    for d in &docs {
        out.push_str(&row(&d.name, d.lines, d.words, d.bytes));
    }
    out.push_str(&"─".repeat(width + 40));
    out.push('\n');
    out.push_str(&row(
        &format!("{} documents", docs.len()),
        docs.iter().map(|d| d.lines).sum(),
        docs.iter().map(|d| d.words).sum(),
        docs.iter().map(|d| d.bytes).sum(),
    ));
    out
}

/// `sync`: where the sync stands.
pub fn render_status(s: &DocsStatus, json: bool) -> String {
    if json {
        return pretty(&json!({
            "set_up": s.set_up,
            "state": s.state.to_string(),
            "paused": s.paused,
            "auto": s.auto,
            "source": s.source,
            "branch": s.branch,
            "root": s.root,
            "last_synced": s.last_synced,
            "conflicts": s.conflicts,
            "pull_request": s.pull_request,
        }));
    }
    if !s.set_up {
        return "This project has no docs yet; `shelbi docs init` sets them up.\n".into();
    }
    let mut state = s.state.to_string();
    if s.paused {
        state.push_str(" (paused)");
    } else if !s.auto {
        state.push_str(" (manual)");
    }
    let mut out = format!("Status:       {state}\n");
    let source = if s.source == "folder" { "folder" } else { "orphan branch" };
    out.push_str(&format!("Source:       {source} on {}\n", s.branch));
    out.push_str(&format!("Root:         {}\n", s.root));
    out.push_str(&format!(
        "Last synced:  {}\n",
        s.last_synced.as_deref().unwrap_or("not since the hub started")
    ));
    if let Some(pr) = &s.pull_request {
        out.push_str(&format!("Pull request: {pr}\n"));
    }
    if !s.conflicts.is_empty() {
        out.push_str("Conflicts:\n");
        for c in &s.conflicts {
            out.push_str(&format!("  {c}\n"));
        }
        out.push_str("Held out of the sync until resolved: shelbi docs resolve <path> --ours | --theirs\n");
    }
    out
}

fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

fn number(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// The command's name before Documentation was called that: `shelbi wiki`
/// stays a hidden alias of `shelbi docs`, so agent prompts, scripts and
/// muscle memory from then keep working.
pub const LEGACY_COMMAND: &str = "wiki";

/// Whether `args` (the process's, without the program name) invoke
/// `shelbi docs`: its first word that isn't a global flag is `docs` (or the
/// legacy [`LEGACY_COMMAND`]). `main`
/// uses it to end a bad `shelbi docs` command line with exit 3, as `cstore`
/// does, rather than clap's 2 (which is "not found" here).
pub fn is_docs_invocation<I: IntoIterator<Item = std::ffi::OsString>>(args: I) -> bool {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let Some(arg) = arg.to_str() else {
            return false;
        };
        match arg {
            "--root" | "--project" | "-p" => {
                args.next();
            }
            a if a.starts_with('-') => {}
            a => return a == "docs" || a == LEGACY_COMMAND,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::cell::RefCell;

    /// A hub that records requests and answers from a script.
    struct FakeHub {
        asked: RefCell<Vec<DocsRequest>>,
        answer: Box<dyn Fn(&DocsRequest) -> DocsReply>,
    }

    impl FakeHub {
        fn new(answer: impl Fn(&DocsRequest) -> DocsReply + 'static) -> Self {
            Self {
                asked: RefCell::new(Vec::new()),
                answer: Box::new(answer),
            }
        }
    }

    impl Hub for FakeHub {
        fn ask(&self, req: DocsRequest) -> DocsReply {
            let reply = (self.answer)(&req);
            self.asked.borrow_mut().push(req);
            reply
        }
    }

    struct FakeInput(Option<String>);

    impl Input for FakeInput {
        fn stdin(&self, _wait: bool) -> Result<Option<String>, Failure> {
            Ok(self.0.clone())
        }
        fn edit(&self, file: &std::path::Path) -> Result<(), Failure> {
            let text = std::fs::read_to_string(file).unwrap();
            std::fs::write(file, text.replace("old", "new")).unwrap();
            Ok(())
        }
    }

    #[derive(clap::Parser, Debug)]
    struct Harness {
        #[command(flatten)]
        args: DocsArgs,
    }

    fn parse(argv: &[&str]) -> DocsArgs {
        use clap::Parser;
        Harness::parse_from(std::iter::once("docs").chain(argv.iter().copied())).args
    }

    fn exec(hub: &FakeHub, stdin: Option<&str>, argv: &[&str]) -> Outcome {
        let args = parse(argv);
        let json = args.agent || args.format == Format::Json;
        execute(hub, &FakeInput(stdin.map(str::to_string)), args.cmd, json)
    }

    fn doc(path: &str, content: &str) -> DocsReply {
        DocsReply::Doc {
            path: path.into(),
            content: content.into(),
        }
    }

    /// A hub over an in-memory `docs:` block.
    struct ConfigHub {
        inner: FakeHub,
        block: RefCell<Option<DocsConfig>>,
        writes: RefCell<usize>,
    }

    impl Hub for ConfigHub {
        fn ask(&self, req: DocsRequest) -> DocsReply {
            self.inner.ask(req)
        }
        fn docs_block(&self) -> Result<Option<DocsConfig>, Failure> {
            Ok(self.block.borrow().clone())
        }
        fn write_docs_block(&self, block: &DocsConfig) -> Result<(), Failure> {
            *self.block.borrow_mut() = Some(block.clone());
            *self.writes.borrow_mut() += 1;
            Ok(())
        }
    }

    fn config_hub(block: Option<DocsConfig>) -> ConfigHub {
        ConfigHub {
            inner: FakeHub::new(|_| {
                DocsReply::Status(DocsStatus {
                    set_up: true,
                    branch: "docs/planning".into(),
                    source: "branch".into(),
                    ..DocsStatus::not_set_up()
                })
            }),
            block: RefCell::new(block),
            writes: RefCell::new(0),
        }
    }

    fn exec_on(hub: &dyn Hub, argv: &[&str]) -> Outcome {
        let args = parse(argv);
        let json = args.agent || args.format == Format::Json;
        execute(hub, &FakeInput(None), args.cmd, json)
    }

    #[test]
    fn init_writes_the_default_branch_block_and_asks_the_hub() {
        let hub = config_hub(None);
        let out = exec_on(&hub, &["init"]).unwrap();
        assert!(out.starts_with("The docs are set up on the orphan branch docs/planning."), "{out}");
        let block = hub.block.borrow().clone().unwrap();
        assert_eq!(block.source, DocsSource::Branch);
        assert_eq!(block.branch.as_deref(), Some("docs/planning"), "the default is spelled out");
        assert_eq!(hub.inner.asked.borrow().as_slice(), [DocsRequest::Init]);
        // Again: nothing to write, the hub is asked again.
        let out = exec_on(&hub, &["init"]).unwrap();
        assert!(out.starts_with("The docs were already set up"), "{out}");
        assert_eq!(*hub.writes.borrow(), 1);
    }

    #[test]
    fn init_sets_up_folder_mode_and_keeps_what_the_flags_leave_out() {
        let hub = config_hub(Some(DocsConfig {
            sync: shelbi_core::DocsSyncMode::Manual,
            ..DocsConfig::default()
        }));
        exec_on(&hub, &["init", "--source", "folder"]).unwrap();
        let block = hub.block.borrow().clone().unwrap();
        assert_eq!((block.source, block.path.as_deref()), (DocsSource::Folder, Some("docs")));
        assert_eq!(block.sync, shelbi_core::DocsSyncMode::Manual, "kept");
        // --path alone means folder mode.
        let hub = config_hub(None);
        exec_on(&hub, &["init", "--path", "handbook/"]).unwrap();
        let block = hub.block.borrow().clone().unwrap();
        assert_eq!((block.source, block.path.as_deref()), (DocsSource::Folder, Some("handbook")));
        let json: Value = serde_json::from_str(&exec_on(&hub, &["init", "--format", "json"]).unwrap()).unwrap();
        assert_eq!(json["wrote_block"], Value::Bool(false));
        assert_eq!(json["set_up"], Value::Bool(true));
    }

    #[test]
    fn init_refuses_a_bad_value_or_mixed_flags_before_writing() {
        for argv in [
            &["init", "--path", "/abs"][..],
            &["init", "--source", "folder", "--path", "../x"],
            &["init", "--branch", "has space"],
            &["init", "--source", "folder", "--branch", "docs"],
            &["init", "--source", "branch", "--path", "docs"],
            &["init", "--branch", "docs", "--path", "docs"],
        ] {
            let hub = config_hub(None);
            let err = exec_on(&hub, argv).unwrap_err();
            assert_eq!(err.kind.exit_code(), 3, "{argv:?}: {}", err.message);
            assert!(hub.block.borrow().is_none() && hub.inner.asked.borrow().is_empty(), "{argv:?}");
        }
        let err = exec_on(&config_hub(None), &["init", "--path", "../x"]).unwrap_err();
        assert!(err.message.contains("docs.path `../x`"), "{}", err.message);
    }

    #[test]
    fn write_without_a_section_asks_for_a_cstore_overwrite() {
        let hub = FakeHub::new(|_| DocsReply::Done);
        let out = exec(&hub, None, &["write", "/Plans/a.md", "--content", "Body", "--title", "A"]).unwrap();
        assert_eq!(out, "Wrote Plans/a.md\n");
        assert!(matches!(
            &hub.asked.borrow()[0],
            DocsRequest::Overwrite { path, content, title: Some(t), .. } if path == "Plans/a.md" && content == "Body" && t == "A"
        ));
        exec(&hub, Some("piped"), &["write", "Plans/a.md", "--section", "## Goals"]).unwrap();
        assert!(matches!(
            &hub.asked.borrow()[1],
            DocsRequest::Write { content, section: Some(h), .. } if content == "piped" && h == "## Goals"
        ));
    }

    #[test]
    fn a_write_with_no_content_is_invalid() {
        let hub = FakeHub::new(|_| DocsReply::Done);
        let err = exec(&hub, None, &["append", "Plans/a.md"]).unwrap_err();
        assert_eq!(err.kind.exit_code(), 3);
        assert!(hub.asked.borrow().is_empty());
    }

    #[test]
    fn hub_failures_keep_their_exit_codes() {
        let hub = FakeHub::new(|_| DocsReply::failed(FailureKind::NotFound, "not found: x.md"));
        let err = exec(&hub, None, &["read", "x.md"]).unwrap_err();
        assert_eq!((err.kind.exit_code(), err.message.as_str()), (2, "not found: x.md"));
        let hub = FakeHub::new(|_| DocsReply::failed(FailureKind::Conflict, "held"));
        assert_eq!(exec(&hub, None, &["rm", "x.md"]).unwrap_err().kind.exit_code(), 4);
    }

    #[test]
    fn read_slices_lines_and_answers_json() {
        let hub = FakeHub::new(|_| doc("a.md", "# A\n\none\ntwo\n"));
        assert_eq!(exec(&hub, None, &["read", "a.md", "--head", "1"]).unwrap(), "# A");
        let out = exec(&hub, None, &["read", "a.md", "--agent"]).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!((v["path"].as_str(), v["title"].as_str()), (Some("a.md"), Some("A")));
    }

    #[test]
    fn cat_expands_a_folder_and_adds_headers() {
        let hub = FakeHub::new(|req| match req {
            DocsRequest::List { .. } => DocsReply::Entries(vec![
                entry(EntryKind::Folder, "Sub", "Plans/Sub"),
                entry(EntryKind::Document, "a.md", "Plans/a.md"),
                entry(EntryKind::Document, "b.md", "Plans/b.md"),
            ]),
            DocsRequest::Read { path, .. } => doc(path, &format!("# {path}\n")),
            _ => DocsReply::Done,
        });
        let out = exec(&hub, None, &["cat", "Plans/", "--headers"]).unwrap();
        assert_eq!(out, "--- Plans/a.md ---\n\n# Plans/a.md\n\n--- Plans/b.md ---\n\n# Plans/b.md\n");
    }

    fn entry(kind: EntryKind, name: &str, path: &str) -> DocsEntry {
        DocsEntry {
            kind,
            name: name.into(),
            path: path.into(),
            title: name.trim_end_matches(".md").into(),
            level: if kind == EntryKind::Section { 2 } else { 0 },
            lines: 3,
            words: 1200,
            bytes: 2048,
            documents: 1,
        }
    }

    #[test]
    fn ls_lists_folders_then_documents_with_stats() {
        let entries = vec![
            entry(EntryKind::Folder, "Archive", "Archive"),
            entry(EntryKind::Document, "plan.md", "plan.md"),
        ];
        assert_eq!(render_ls(&entries, false, false), "Archive/\nplan.md\n");
        let stats = render_ls(&entries, true, false);
        assert!(stats.contains("plan.md   3        lines  1,200    words  2.0 KB"), "{stats}");
        assert!(stats.contains("1 documents"), "{stats}");
        let sections = vec![entry(EntryKind::Section, "## Goals", "plan.md")];
        assert_eq!(render_ls(&sections, false, false), "## Goals\n");
        assert!(render_ls(&sections, true, false).contains("  ## Goals    (3 lines, 1,200 words)"));
    }

    #[test]
    fn tree_draws_folders_and_prunes_by_depth() {
        let node = |path: &str, kind: NodeKind, children: Vec<DocsNode>| DocsNode {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            title: path.into(),
            kind,
            icon: None,
            children,
        };
        let nodes = vec![
            node(
                "Plans",
                NodeKind::Folder,
                vec![node("Plans/Old", NodeKind::Folder, vec![]), node("Plans/a.md", NodeKind::Document, vec![])],
            ),
            node("README.md", NodeKind::Document, vec![]),
        ];
        assert_eq!(render_tree(None, &nodes, None, false, false), ".\n└── Plans/\n    └── Old/\n");
        assert_eq!(
            render_tree(None, &nodes, None, true, false),
            ".\n├── Plans/\n│   ├── Old/\n│   └── a.md\n└── README.md\n"
        );
        assert_eq!(render_tree(None, &nodes, Some(1), true, false), ".\n├── Plans/\n└── README.md\n");
    }

    #[test]
    fn new_reads_optional_stdin_and_refuses_a_folder_title() {
        let hub = FakeHub::new(|_| DocsReply::Done);
        assert_eq!(exec(&hub, Some("From stdin"), &["new", "Plans/a.md"]).unwrap(), "Created document: Plans/a.md\n");
        assert!(matches!(
            &hub.asked.borrow()[0],
            DocsRequest::New { content: Some(c), title: None, .. } if c == "From stdin"
        ));
        assert_eq!(exec(&hub, None, &["new", "Design"]).unwrap(), "Created folder: Design\n");
        assert_eq!(exec(&hub, None, &["new", "Design", "--title", "x"]).unwrap_err().kind.exit_code(), 3);
    }

    #[test]
    fn edit_saves_a_changed_copy_and_refuses_when_changed_elsewhere() {
        let hub = FakeHub::new(|req| match req {
            DocsRequest::Read { .. } => doc("a.md", "# A\n\nold\n"),
            _ => DocsReply::Done,
        });
        assert_eq!(exec(&hub, None, &["edit", "a.md"]).unwrap(), "Saved a.md\n");
        assert!(matches!(
            hub.asked.borrow().last().unwrap(),
            DocsRequest::Write { content, section: None, .. } if content == "# A\n\nnew\n"
        ));

        let reads = std::cell::Cell::new(0);
        let hub = FakeHub::new(move |req| match req {
            DocsRequest::Read { .. } => {
                reads.set(reads.get() + 1);
                doc("a.md", if reads.get() == 1 { "# A\n\nold\n" } else { "# A\n\nsomeone else\n" })
            }
            _ => DocsReply::Done,
        });
        let err = exec(&hub, None, &["edit", "a.md"]).unwrap_err();
        assert_eq!(err.kind.exit_code(), 4, "{}", err.message);
        assert!(!hub.asked.borrow().iter().any(|r| matches!(r, DocsRequest::Write { .. })));
    }

    #[test]
    fn search_by_document_narrows_to_it() {
        let hub = FakeHub::new(|_| {
            DocsReply::Matches(vec![
                shelbi_app::docs::DocsMatch { path: "P/a.md".into(), line: 3, text: "auth here".into(), start: 0, end: 4 },
                shelbi_app::docs::DocsMatch { path: "P/b.md".into(), line: 1, text: "auth".into(), start: 0, end: 4 },
            ])
        });
        assert_eq!(exec(&hub, None, &["search", "auth", "--document", "P/a.md"]).unwrap(), "P/a.md:3: auth here\n");
        assert!(matches!(&hub.asked.borrow()[0], DocsRequest::Search { folder: Some(f), .. } if f == "P"));
    }

    #[test]
    fn the_status_reads_as_lines_or_json() {
        let status = DocsStatus {
            set_up: true,
            state: shelbi_app::docs::SyncState::Conflict,
            conflicts: vec!["Plans/a.md".into()],
            paused: true,
            auto: true,
            source: "branch".into(),
            branch: "docs/planning".into(),
            root: "/w".into(),
            last_synced: None,
            pull_request: None,
        };
        let text = render_status(&status, false);
        assert!(text.contains("Status:       conflict (paused)"), "{text}");
        assert!(text.contains("orphan branch on docs/planning"), "{text}");
        assert!(text.contains("  Plans/a.md"), "{text}");
        let v: Value = serde_json::from_str(&render_status(&status, true)).unwrap();
        assert_eq!(v["state"], "conflict");
        assert_eq!(v["conflicts"][0], "Plans/a.md");
    }

    fn held() -> DocsConflict {
        DocsConflict {
            path: "Plans/a.md".into(),
            base: Some("# A\n\nBase.\n".into()),
            ours: Some("# A\n\nOurs.\n".into()),
            theirs: Some("# A\n\nTheirs.\n".into()),
            detected_at: "2026-11-27T00:00:00Z".into(),
        }
    }

    #[test]
    fn resolve_takes_a_whole_side_or_shows_the_hunks() {
        let hub = FakeHub::new(|req| match req {
            DocsRequest::Conflict { .. } => DocsReply::Conflict(held()),
            _ => DocsReply::Done,
        });
        let out = exec(&hub, None, &["resolve", "/Plans/a.md", "--theirs"]).unwrap();
        assert!(out.starts_with("Resolved Plans/a.md (theirs)"), "{out}");
        assert!(matches!(
            &hub.asked.borrow()[0],
            DocsRequest::Resolve { path, resolution: Resolution::Theirs, .. } if path == "Plans/a.md"
        ));
        exec(&hub, None, &["resolve", "Plans/a.md", "--ours"]).unwrap();
        assert!(matches!(&hub.asked.borrow()[1], DocsRequest::Resolve { resolution: Resolution::Ours, .. }));

        // No side: the hunks, and nothing resolved.
        let out = exec(&hub, None, &["resolve", "Plans/a.md"]).unwrap();
        assert!(out.contains("Plans/a.md couldn't be merged (1 hunk)"), "{out}");
        assert!(out.contains("<<<<<<< ours\nOurs.\n=======\nTheirs.\n>>>>>>> theirs"), "{out}");
        assert!(out.contains("shelbi docs resolve Plans/a.md --ours | --theirs"), "{out}");
        assert_eq!(hub.asked.borrow().len(), 3);
        let v: Value = serde_json::from_str(&exec(&hub, None, &["resolve", "Plans/a.md", "--agent"]).unwrap()).unwrap();
        assert_eq!(v["hunks"][0]["theirs"][0], "Theirs.");
        assert_eq!(v["revision"], shelbi_app::docs_conflict::revision(&held()));
    }

    #[test]
    fn resolve_refuses_both_sides_and_keeps_the_conflict_exit_code() {
        use clap::Parser;
        assert!(Harness::try_parse_from(["docs", "resolve", "a.md", "--ours", "--theirs"]).is_err());
        let hub = FakeHub::new(|_| DocsReply::failed(FailureKind::Conflict, "`a.md` isn't in conflict"));
        assert_eq!(exec(&hub, None, &["resolve", "a.md", "--ours"]).unwrap_err().kind.exit_code(), 4);
    }

    #[test]
    fn the_editor_comes_from_editor_then_visual_then_vi() {
        let f = std::path::Path::new("/tmp/a.md");
        assert_eq!(editor_argv(f, Some("code --wait"), Some("vim")), ["code", "--wait", "/tmp/a.md"]);
        assert_eq!(editor_argv(f, Some(" "), Some("vim")), ["vim", "/tmp/a.md"]);
        assert_eq!(editor_argv(f, None, None), ["vi", "/tmp/a.md"]);
    }

    #[test]
    fn a_docs_invocation_is_found_past_global_flags() {
        let os = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(is_docs_invocation(os(&["docs", "read"])));
        assert!(is_docs_invocation(os(&["-p", "demo", "--yes", "docs"])));
        assert!(is_docs_invocation(os(&["--root", "/x", "docs", "--bogus"])));
        assert!(!is_docs_invocation(os(&["issue", "docs"])));
        assert!(!is_docs_invocation(os(&["--project", "docs", "status"])));
        // The old name is still a `shelbi docs` command line.
        assert!(is_docs_invocation(os(&["wiki", "read"])));
        assert!(is_docs_invocation(os(&["-p", "demo", "wiki", "--bogus"])));
    }

    /// Every subcommand is in the grouped overview and has examples.
    #[test]
    fn every_command_is_grouped_and_has_examples() {
        let cmd = Harness::command();
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            assert!(
                HELP_TEMPLATE.lines().any(|l| l.trim_start().starts_with(&format!("{name} "))),
                "`{name}` is missing from the grouped help"
            );
            let after = sub.get_after_help().map(|h| h.to_string()).unwrap_or_default();
            assert!(after.contains("EXAMPLES\n  shelbi docs "), "`{name}` has no examples");
        }
    }
}
