//! `shelbi issue <subcommand>` — Kanban board management.
//!
//! Issues are stored as `<shelbi_home>/projects/<project>/tasks/<id>.md`
//! files (markdown body + YAML frontmatter). The orchestrator creates
//! issues (typically into `backlog`); the user curates them through the
//! columns; workspaces pick up `todo` issues.
//!
//! Priorities within a column are contiguous integers 0..N. Any operation
//! that changes a column's membership renumbers it before returning, so
//! callers can treat `priority` as a stable position index.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args as ClapArgs, Subcommand};
use shelbi_state::IssueStore;
use shelbi_core::{
    default_workflow, Column, Issue, Owner, StatusCategory, Workflow,
};
use shelbi_proto::control::{AddSpec, EditSpec, MutationKind, SubOp};

use super::{mutate_client, require_project};

#[derive(Debug, Subcommand)]
pub enum IssueCmd {
    /// Create a new issue. Defaults to the backlog column.
    Add(AddArgs),
    /// List issues (all statuses, or one with `--status`).
    List {
        /// Restrict to a single status. `--column` accepted as a hidden
        /// alias for one release while older scripts catch up.
        #[arg(long = "status", alias = "column", value_name = "STATUS")]
        status: Option<String>,
        /// Show only unblocked todo items, in priority order. Useful for
        /// orchestrator agents and for users planning next work. Mutually
        /// exclusive with `--status`.
        #[arg(long, conflicts_with = "status")]
        ready: bool,
        /// Restrict to issues pinned to the named workflow. Issues with no
        /// explicit `workflow:` field are resolved through the project's
        /// configured `default_workflow:` (the canonical `default` only when
        /// the project sets none). Composes with `--column` and `--ready`.
        #[arg(long, value_name = "NAME")]
        workflow: Option<String>,
    },
    /// Print an issue's frontmatter + body, plus the resolved status of each
    /// `depends_on` entry.
    Show { id: String },
    /// Edit an issue's dependency list.
    Depends(DependsArgs),
    /// Move an issue to another status. An issue's position is a status id, so
    /// the destination may be ANY status the issue's workflow declares —
    /// including `canceled` / archived statuses and any status a user adds.
    /// A status the workflow doesn't declare errors, naming the ids it does.
    Move {
        id: String,
        #[arg(long, value_name = "STATUS")]
        to: String,
        /// Reason string recorded in `~/.shelbi/events.log`. The
        /// orchestrator parses this to identify auto-dispatch moves vs.
        /// user-driven ones. Defaults to `user:cli`.
        #[arg(long, value_name = "REASON")]
        reason: Option<String>,
        /// FOOTGUN / recovery escape hatch: advance the card to `--to`
        /// WITHOUT running the destination transition's actions (`merge`,
        /// `delete_branch`, `push_branch`, `open_pr`, `close_pr`, …). Use
        /// only when the underlying git work was already done out of band —
        /// e.g. a PR merged by hand — so the transition's `merge` would
        /// re-fail on an already-merged branch and strand the card. Skipping
        /// `merge` means the branch is genuinely NOT merged by Shelbi;
        /// skipping `push_branch`/`open_pr` means no PR is opened. The move is
        /// stamped `actions=skipped` in the event log so the board history is
        /// honest that side effects were bypassed.
        #[arg(long)]
        skip_transition_actions: bool,
    },
    /// Assign an issue to a workspace. Workspace must be declared in project YAML.
    Assign {
        id: String,
        #[arg(long, value_name = "WORKSPACE")]
        to: String,
        /// Override the review-slot guard. `issue assign` refuses a
        /// `review`-tagged workspace by default (review slots load handed-off
        /// branches via the review queue, not direct dispatch); pass `--force`
        /// to route a normal issue onto one anyway. The override is recorded on
        /// `~/.shelbi/events.log` for auditability.
        #[arg(long)]
        force: bool,
    },
    /// Clear an issue's workspace assignment.
    Unassign { id: String },
    /// Launch the assigned workspace on this issue: ensure the worktree is on
    /// the issue's branch, kill any existing workspace pane (clears context),
    /// start the runner with the issue's prompt. Moves the issue into
    /// `in_progress`. Pass `--workspace` to assign at the same time.
    Start {
        id: String,
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<String>,
        /// Override the generated branch name.
        #[arg(long)]
        branch: Option<String>,
        /// Reason string recorded in `~/.shelbi/events.log` when the
        /// column transitions into `in_progress`. The orchestrator uses
        /// this to identify auto-dispatch starts vs. user-driven ones.
        /// Defaults to `user:cli:start`.
        #[arg(long, value_name = "REASON")]
        reason: Option<String>,
        /// Override the review-slot guard. `issue start` refuses a
        /// `review`-tagged workspace by default (review slots load handed-off
        /// branches via the review queue, not direct dispatch); pass `--force`
        /// to launch a normal issue on one anyway. The override is recorded on
        /// `~/.shelbi/events.log` for auditability.
        #[arg(long)]
        force: bool,
    },
    /// Relaunch the assigned workspace on the issue it is ALREADY working,
    /// WITHOUT discarding progress. For a stalled or killed worker: recreates
    /// or reclaims the tmux pane and (for a claude runner) resumes the prior
    /// conversation via `--continue`, while preserving the worktree as-is —
    /// its branch, commits, and uncommitted changes stay put. Contrast with
    /// `start`, which wipes context and re-checks-out a clean branch. Restores
    /// the issue to `in_progress` if it drifted. Pass `--workspace` to target a
    /// specific workspace (defaults to the issue's `assigned_to`).
    Resume {
        id: String,
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<String>,
        /// Reason string recorded in `~/.shelbi/events.log` if the resume has
        /// to move the card back into `in_progress`. Defaults to
        /// `user:cli:resume`.
        #[arg(long, value_name = "REASON")]
        reason: Option<String>,
    },
    /// Re-order an issue within its column.
    Prio(PrioArgs),
    /// Revise an issue's content. With NO flags, opens the issue file in
    /// `$EDITOR` (the historical behavior). Any field flag switches to
    /// non-interactive mode: `--title`, a body source
    /// (`--body`/`--body-file`/stdin, optionally `--append`), a frontmatter
    /// field (`--workflow`/`--branch`/`--prefers-machine`/`--no-prefers-machine`),
    /// or in-place body substitutions (`--sub`/`--sub-regex`). Fields with a
    /// dedicated command — `move`, `prio`, `assign`, `depends` — are out of
    /// scope; use those instead.
    Edit(EditArgs),
    /// Delete an issue.
    Rm { id: String },
    /// Post a comment on an issue. Works against whichever issue-tracker
    /// backend the project is configured for (`file_system` by default,
    /// `github` for issues in a repo).
    Comment {
        /// The issue id (the stable shelbi slug).
        id: String,
        /// The comment text.
        text: String,
    },
}

#[derive(Debug, ClapArgs)]
pub struct AddArgs {
    /// Human-readable title.
    pub title: String,
    /// Override the auto-generated id (slugified from the title).
    #[arg(long)]
    pub id: Option<String>,
    /// Initial status. Defaults to `backlog`. Creating directly in a
    /// ready status (e.g. `--status todo`) skips triage and wakes the
    /// orchestrator just like moving a card into it. `--column` accepted
    /// as a hidden alias for one release while older scripts catch up.
    #[arg(
        long = "status",
        alias = "column",
        default_value = "backlog",
        value_name = "STATUS"
    )]
    pub status: String,
    /// Optional description. The body may also be piped on stdin
    /// (`shelbi issue add "Title" <<EOF ... EOF`); passing both is an
    /// error. If neither is given, the body defaults to the title (use
    /// `shelbi issue edit` to fill it in).
    #[arg(long, short)]
    pub description: Option<String>,
    /// Issue id this issue depends on. Repeat for multiple deps:
    /// `--depends-on a --depends-on b`. Repeat-flag chosen over
    /// comma-separated to avoid future escaping issues with ids that may
    /// contain commas or shell metacharacters.
    #[arg(long = "depends-on", value_name = "ID")]
    pub depends_on: Vec<String>,
    /// Hint to the orchestrator that this issue should be assigned to a
    /// workspace on this machine. Persisted in the issue frontmatter; the
    /// orchestrator decides whether to honor it.
    #[arg(long = "prefers-machine", value_name = "NAME")]
    pub prefers_machine: Option<String>,
    /// Workflow this issue runs under. Names a file in `workflows/<NAME>.yaml`.
    /// Omit to leave `workflow:` unset, which resolves at read time to the
    /// project's configured `default_workflow:` (the canonical `default` only
    /// when the project sets none).
    #[arg(long = "workflow", value_name = "NAME")]
    pub workflow: Option<String>,
    /// Pre-fill the issue's `branch:` frontmatter field. Omit to let the
    /// orchestrator generate a branch from workflow config, project config,
    /// or the GitHub username at dispatch time; supply a value to point the
    /// issue at an existing branch (the *release issue* pattern in
    /// `Plans/workflows.md` §12).
    #[arg(long = "branch", value_name = "BRANCH")]
    pub branch: Option<String>,
}

#[derive(Debug, ClapArgs)]
pub struct DependsArgs {
    /// Issue whose dependency list is being edited.
    pub id: String,
    /// Dependency id to add. Repeat for multiple.
    #[arg(long = "add", value_name = "DEP")]
    pub add: Vec<String>,
    /// Dependency id to remove. Repeat for multiple.
    #[arg(long = "remove", value_name = "DEP")]
    pub remove: Vec<String>,
}

#[derive(Debug, ClapArgs)]
pub struct PrioArgs {
    pub id: String,
    /// Move up one slot.
    #[arg(long, conflicts_with_all = ["down", "top", "bottom", "set"])]
    pub up: bool,
    /// Move down one slot.
    #[arg(long, conflicts_with_all = ["up", "top", "bottom", "set"])]
    pub down: bool,
    /// Move to the top of the column.
    #[arg(long, conflicts_with_all = ["up", "down", "bottom", "set"])]
    pub top: bool,
    /// Move to the bottom of the column.
    #[arg(long, conflicts_with_all = ["up", "down", "top", "set"])]
    pub bottom: bool,
    /// Move to a specific 0-based slot.
    #[arg(long, value_name = "N", conflicts_with_all = ["up", "down", "top", "bottom"])]
    pub set: Option<u32>,
}

#[derive(Debug, ClapArgs)]
pub struct EditArgs {
    /// Issue to edit.
    pub id: String,
    /// New display title. The issue's `id` stays stable — it is never
    /// re-slugified from a new title.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,
    /// Replace the body with this text. Mutually exclusive with
    /// `--body-file`, piped stdin, and the substitution flags.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// Replace the body with the contents of a file. Mutually exclusive with
    /// `--body`, piped stdin, and the substitution flags.
    #[arg(long = "body-file", value_name = "PATH")]
    pub body_file: Option<std::path::PathBuf>,
    /// Append the body source (`--body`/`--body-file`/stdin) to the existing
    /// body instead of replacing it. Mutually exclusive with the substitution
    /// flags.
    #[arg(long)]
    pub append: bool,
    /// Set the issue's workflow. Must name an existing `workflows/<NAME>.yaml`.
    #[arg(long = "workflow", value_name = "NAME")]
    pub workflow: Option<String>,
    /// Set the issue's `branch:` override.
    #[arg(long = "branch", value_name = "BRANCH")]
    pub branch: Option<String>,
    /// Set the prefers-machine hint. Mutually exclusive with
    /// `--no-prefers-machine`.
    #[arg(long = "prefers-machine", value_name = "NAME")]
    pub prefers_machine: Option<String>,
    /// Clear the prefers-machine hint. Mutually exclusive with
    /// `--prefers-machine`.
    #[arg(long = "no-prefers-machine")]
    pub no_prefers_machine: bool,
    /// Literal in-place substitution in the body: replace every occurrence of
    /// OLD with NEW. Repeatable; multiple `--sub`/`--sub-regex` apply in
    /// command-line order, each on the previous result. Two values (not
    /// `OLD=NEW`) so the strings may contain `=`, `,`, or shell
    /// metacharacters.
    #[arg(
        long,
        value_names = ["OLD", "NEW"],
        num_args = 2,
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    pub sub: Vec<String>,
    /// Regex in-place substitution in the body: replace every match of PATTERN
    /// with REPLACEMENT. REPLACEMENT may reference capture groups (`$1`,
    /// `${name}`). Repeatable and interleaves with `--sub` in command-line
    /// order.
    #[arg(
        long = "sub-regex",
        value_names = ["PATTERN", "REPLACEMENT"],
        num_args = 2,
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    pub sub_regex: Vec<String>,
    /// Don't error when a substitution matches zero occurrences (the default
    /// is to reject a stale OLD/PATTERN, writing nothing).
    #[arg(long = "allow-no-match")]
    pub allow_no_match: bool,
    /// Optional reason recorded in the emitted `edited` event. Defaults to
    /// `user:cli`.
    #[arg(long, value_name = "REASON")]
    pub reason: Option<String>,
}

pub fn run(project_opt: Option<String>, cmd: IssueCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    // Version gate: board mutations against a stale daemon (old binary
    // kept running across an upgrade) produce undiagnosable io errors —
    // refuse up front. Read-only views still work, with a warning.
    match &cmd {
        IssueCmd::List { .. } | IssueCmd::Show { .. } => super::hub_version::warn_on_mismatch(),
        _ => super::hub_version::ensure_daemon_matches_for_mutation()?,
    }
    match cmd {
        IssueCmd::Add(args) => add(&project, args),
        IssueCmd::List {
            status,
            ready,
            workflow,
        } => list(&project, status.as_deref(), ready, workflow.as_deref()),
        IssueCmd::Show { id } => show(&project, &id),
        IssueCmd::Depends(args) => depends(&project, args),
        IssueCmd::Move {
            id,
            to,
            reason,
            skip_transition_actions,
        } => mutate_client::run_mutation(
            &project,
            &id,
            MutationKind::Move {
                to,
                reason,
                skip_transition_actions,
            },
        ),
        IssueCmd::Assign { id, to, force } => {
            mutate_client::run_mutation(&project, &id, MutationKind::Assign { to, force })
        }
        IssueCmd::Unassign { id } => {
            mutate_client::run_mutation(&project, &id, MutationKind::Unassign)
        }
        IssueCmd::Start {
            id,
            workspace,
            branch,
            reason,
            force,
        } => mutate_client::run_mutation(
            &project,
            &id,
            MutationKind::Start {
                workspace,
                branch,
                reason,
                force,
            },
        ),
        IssueCmd::Resume {
            id,
            workspace,
            reason,
        } => resume(&project, &id, workspace.as_deref(), reason.as_deref()),
        IssueCmd::Prio(args) => prio(&project, args),
        IssueCmd::Edit(args) => edit(&project, args),
        IssueCmd::Rm { id } => rm(&project, &id),
        IssueCmd::Comment { id, text } => comment(&project, &id, &text),
    }
}

/// Post a comment onto an issue via the project's configured issue-tracker
/// backend (plan Decision D4 — comments are first-class). Routed through the
/// [`IssueStore`] seam so it works the same on `file_system` or `github`.
fn comment(project: &str, id: &str, text: &str) -> Result<()> {
    let store = cached_issue_store(project)?;
    let posted = store.add_comment(id, text).map_err(|e| anyhow!(e))?;
    println!("✓ commented on {id} (comment {})", posted.id);
    Ok(())
}

/// The project's configured issue-tracker backend as a **cached** live store.
/// Routes through [`shelbi_state::issue_store_for`] (the name-based cached read
/// entry point) so every CLI read/list command shares the one process cache
/// rather than constructing an uncached store; the project YAML's
/// `issue_tracker` block still selects and validates the backend (`file_system`
/// by default, `github` for issues in a repo).
fn cached_issue_store(project: &str) -> Result<Box<dyn IssueStore>> {
    shelbi_state::issue_store_for(project).map_err(|e| anyhow!(e))
}

/// The card that genuinely occupies `workspace_name`, if any — resolved from the
/// live, daemon-owned board (`board-index.json`, the same source
/// `shelbi workspace list` reads) rather than the per-process
/// `board-snapshot.json`.
///
/// Occupancy used to be read from
/// `cached_issue_store().list_in_status(in_progress)`, which on a remote backend
/// serves `board-snapshot.json` — a file only a *dispatch* rewrites. A
/// short-lived CLI one-shot serves that snapshot and kicks only a background
/// refresh it exits before completing, so a card that reached a terminal column
/// out-of-band (a merged PR) kept its stale `in_progress` entry indefinitely and
/// permanently locked its workspace out of every future dispatch.
///
/// The board index is open-only, so a card in a terminal column (`done` /
/// `canceled`) is already absent and can never hold a workspace. We additionally
/// filter to the active `in_progress` column, matching the previous guard's
/// intent, and return the whole card so a refusal can name its real column.
///
/// A cold board (no index published yet — a remote project whose daemon has not
/// run) yields `Ok(None)`: erring toward dispatchable is the intended direction
/// ("the stale side loses rather than blocking work"), and the
/// persist-before-spawn dispatch ordering still prevents a genuine
/// double-assignment.
fn workspace_occupied_by(
    project: &str,
    workspace_name: &str,
    exclude_id: &str,
) -> Result<Option<Issue>> {
    Ok(shelbi_state::read_board(project)
        .map_err(|e| anyhow!(e))?
        .into_issues()
        .into_iter()
        .map(|tf| tf.task)
        .find(|task| {
            task.column == Column::in_progress()
                && task.assigned_to.as_deref() == Some(workspace_name)
                && task.id != exclude_id
        }))
}

/// Refuse to dispatch or assign onto a workspace already running a *different*
/// in-flight issue. Shared by `issue assign`, `issue start`, and `issue resume`
/// so an assignment a dispatch would refuse is refused up front — the two can
/// never disagree, and a card is never left assigned to a workspace that cannot
/// run it. Sourced from live board state via [`workspace_occupied_by`], so a
/// stale snapshot entry can no longer manufacture a phantom occupant.
fn ensure_workspace_dispatchable(
    project: &str,
    workspace_name: &str,
    exclude_id: &str,
) -> Result<()> {
    if let Some(other) = workspace_occupied_by(project, workspace_name, exclude_id)? {
        bail!(
            "workspace `{workspace_name}` is already on issue `{}` ({}) — \
             move it to another column first",
            other.id,
            other.column,
        );
    }
    Ok(())
}

/// Whether `status` is a terminal (`done`/`canceled`) column. Those cards live
/// in the closed history the daemon's open `board-index.json` deliberately
/// omits, so a listing filtered to one reads it on demand through the store
/// rather than the index. Everything else is served from the index.
fn is_terminal_status(status: &Column) -> bool {
    matches!(
        status.category(),
        shelbi_core::StatusCategory::Done | shelbi_core::StatusCategory::Archived
    )
}

/// Load one issue through the configured backend, erroring when it doesn't
/// exist — the `Result<IssueFile>` shape the old `load_task` free function had,
/// so callers that expect the issue to be present read the same.
fn load_issue(project: &str, id: &str) -> Result<shelbi_state::IssueFile> {
    cached_issue_store(project)?
        .get(id)
        .map_err(|e| anyhow!(e))?
        .ok_or_else(|| anyhow!("issue `{id}` not found"))
}

/// `shelbi issue add` — resolve the body source (client-side stdin), build the
/// [`AddSpec`], and run the mutation. All creation logic (id generation, body
/// defaulting, the orchestrator wake) lives in `shelbi_orchestrator::mutate`.
fn add(project: &str, args: AddArgs) -> Result<()> {
    // Only consult stdin when NO explicit body source was passed. `-d` /
    // `--description` already carries the body, so touching `stdin()` would
    // pointlessly block a non-interactive caller whose stdin never reaches EOF.
    // Reading stdin is reserved for the `shelbi issue add "Title" <<EOF` spelling.
    let stdin_body = if add_should_read_stdin(&args) {
        read_piped_stdin()?
    } else {
        None
    };
    // Body precedence: `-d` and piped stdin are two spellings of the same input,
    // so supplying both is ambiguous — refuse rather than silently discard
    // either one. With neither, the body stays `None` and the library defaults
    // it to the title (so the "default to title" shaping lives in one place).
    let body = match (args.description, stdin_body) {
        (Some(_), Some(_)) => bail!(
            "both --description and piped stdin were given — pass the body one way \
             (drop -d, or close stdin)"
        ),
        (Some(d), None) => Some(d),
        (None, Some(s)) => Some(s.trim_end().to_string()),
        (None, None) => None,
    };
    let spec = AddSpec {
        title: args.title,
        id: args.id,
        status: args.status,
        body,
        depends_on: args.depends_on,
        prefers_machine: args.prefers_machine,
        workflow: args.workflow,
        branch: args.branch,
    };
    mutate_client::run_mutation(project, "", MutationKind::Add(Box::new(spec)))
}

/// Whether `add` should consult stdin for the body. False when an explicit
/// body source (`-d`/`--description`) was given — the caller already supplied
/// the body, so reading stdin would only risk blocking a non-interactive
/// invocation whose stdin never closes.
fn add_should_read_stdin(args: &AddArgs) -> bool {
    args.description.is_none()
}

/// Read piped stdin, if any. `None` when stdin is a terminal (interactive
/// invocation — nothing to read, and reading would block on the user).
/// A non-TTY stdin (pipe, heredoc, `/dev/null`) is drained to EOF; an
/// empty result is normalized to `None` so `< /dev/null` behaves like no
/// pipe at all.
///
/// Callers must only invoke this when the body is meant to come from stdin —
/// i.e. no explicit `--body`/`--body-file` (edit) or `-d` (add) was passed.
/// A non-TTY stdin that never reaches EOF (backgrounded process, inherited
/// pipe) blocks `read_to_string` forever, so gate the call on the absence of
/// an explicit body source (see `edit_should_read_stdin` /
/// `add_should_read_stdin`) rather than reading unconditionally.
fn read_piped_stdin() -> Result<Option<String>> {
    use std::io::{IsTerminal, Read};
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut buf = String::new();
    stdin
        .read_to_string(&mut buf)
        .context("reading piped stdin")?;
    if buf.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(buf))
}

/// The full board rendering used by both `shelbi issue list` (no flags)
/// and the `## Board` section of `shelbi status --full`. Emits every
/// column with counts and owner badges. Extracted so the bootstrap
/// snapshot doesn't fork a second copy of the render code.
pub(crate) fn print_board(project: &str) -> Result<()> {
    list(project, None, false, None)
}

fn list(
    project: &str,
    status_filter: Option<&str>,
    ready: bool,
    workflow_filter: Option<&str>,
) -> Result<()> {
    let project_yaml = shelbi_state::load_project(project).ok();
    // String-compare against the project-aware resolver so a filter of the
    // configured default matches issues with no `workflow:` field.
    let matches_workflow = |issue: &Issue| -> bool {
        match workflow_filter {
            Some(name) => {
                project_yaml
                    .as_ref()
                    .map(|p| shelbi_state::resolve_task_workflow_name(p, issue))
                    .unwrap_or_else(|| issue.workflow_or_default())
                    == name
            }
            None => true,
        }
    };

    if ready {
        let mut ready_tasks = shelbi_state::list_ready(project).map_err(|e| anyhow!(e))?;
        ready_tasks.retain(|tf| matches_workflow(&tf.task));
        if ready_tasks.is_empty() {
            println!("(no ready todo items)");
            return Ok(());
        }
        for tf in &ready_tasks {
            let owner = tf
                .task
                .assigned_to
                .as_deref()
                .map(|w| format!("  [{w}]"))
                .unwrap_or_default();
            println!("  {:<28} {}{owner}", tf.task.id, tf.task.title);
        }
        return Ok(());
    }

    // Any status id is a valid filter — normalized so `wip` matches
    // `in-progress`.
    let filter = status_filter.map(Column::from_status_id);

    // The open board comes from the daemon-owned `board-index.json` (§5), never
    // a backend sweep: `read_open_board_for_cli` reads the published file and
    // notes staleness when the daemon is behind. A terminal (`done`/`canceled`)
    // filter is the one exception — that history is loaded on demand and is not
    // in the open index — so it reads its column through the store's on-demand
    // done-history page (§4): the first 50 closed issues, served from the
    // long-TTL `done-history.json` cache with no request when the page is under
    // ten minutes old, never a full `state=closed` sweep.
    let all = match &filter {
        Some(col) if is_terminal_status(col) => cached_issue_store(project)?
            .list_in_status(col)
            .map_err(|e| anyhow!(e))?,
        Some(col) => super::read_open_board_for_cli(project)?
            .into_iter()
            .filter(|tf| &tf.task.column == col)
            .collect(),
        None => super::read_open_board_for_cli(project)?,
    };
    if all.is_empty() {
        println!("(no issues yet)");
        return Ok(());
    }
    // Blocked-status lookup is computed against the issue set read above. On the
    // open-only read a dependency that is already `done` is absent, so a task
    // waiting only on completed work can show the 🔒 badge even though it is
    // ready — an acceptable cosmetic trade for not sweeping the closed history
    // on every listing.
    let columns: std::collections::HashMap<String, Column> = all
        .iter()
        .map(|tf| (tf.task.id.clone(), tf.task.column.clone()))
        .collect();
    // The stock columns (always shown, even empty) plus any custom /
    // archived status an issue actually occupies, in board order.
    let mut cols: Vec<Column> = Column::core();
    for tf in &all {
        if !cols.contains(&tf.task.column) {
            cols.push(tf.task.column.clone());
        }
    }
    cols.sort_by(|a, b| (a.board_order(), a.as_str()).cmp(&(b.board_order(), b.as_str())));
    for col in &cols {
        if let Some(want) = &filter {
            if want != col {
                continue;
            }
        }
        let in_col: Vec<_> = all
            .iter()
            .filter(|tf| &tf.task.column == col && matches_workflow(&tf.task))
            .collect();
        println!("{col} ({})", in_col.len());
        for tf in in_col {
            let owner = tf
                .task
                .assigned_to
                .as_deref()
                .map(|w| format!("  [{w}]"))
                .unwrap_or_default();
            let badge = if tf.task.is_blocked(&columns) {
                " 🔒"
            } else {
                ""
            };
            println!("  {:<28} {}{owner}{badge}", tf.task.id, tf.task.title);
        }
    }
    Ok(())
}

fn show(project: &str, id: &str) -> Result<()> {
    // Render the issue fetched through the store, not the local task file: a
    // `github` backend never writes that file, so reading it would fail on a
    // GitHub-only issue. `render_task_file` reconstructs the exact
    // frontmatter+body representation from any backend's `IssueFile`.
    let store = cached_issue_store(project)?;
    let tf = store
        .get(id)
        .map_err(|e| anyhow!(e))?
        .ok_or_else(|| anyhow!("issue `{id}` not found"))?;
    let text = shelbi_state::render_task_file(&tf).map_err(|e| anyhow!(e))?;
    print!("{text}");

    // Footer: resolved depends_on. Done lazily after the frontmatter dump so
    // scripts grepping for frontmatter still get clean output above the line.
    if !tf.task.depends_on.is_empty() {
        // Build the id→column map from the same board read so the footer works
        // for every backend (not just the local filesystem).
        let columns: std::collections::HashMap<String, Column> = store
            .list()
            .map_err(|e| anyhow!(e))?
            .into_iter()
            .map(|t| (t.task.id, t.task.column))
            .collect();
        let parts: Vec<String> = tf
            .task
            .depends_on
            .iter()
            .map(|dep| match columns.get(dep) {
                Some(col) => format!("{dep} [{col}]"),
                None => format!("{dep} [missing]"),
            })
            .collect();
        if !text.ends_with('\n') {
            println!();
        }
        println!("→ depends on: {}", parts.join(", "));
        if tf.task.is_blocked(&columns) {
            println!("  status: 🔒 blocked");
        } else {
            println!("  status: ✓ ready");
        }
    }
    Ok(())
}

fn depends(project: &str, args: DependsArgs) -> Result<()> {
    if args.add.is_empty() && args.remove.is_empty() {
        bail!("specify at least one --add ID or --remove ID");
    }
    let mut tf = load_issue(project, &args.id)?;

    let mut updated: Vec<String> = tf.task.depends_on.clone();
    // Removals first so an --add of an id being removed lands at the end.
    if !args.remove.is_empty() {
        let drop: std::collections::HashSet<&str> =
            args.remove.iter().map(String::as_str).collect();
        updated.retain(|d| !drop.contains(d.as_str()));
    }
    for dep in &args.add {
        if !updated.iter().any(|d| d == dep) {
            updated.push(dep.clone());
        }
    }
    if updated == tf.task.depends_on {
        println!("(no change)");
        return Ok(());
    }
    tf.task.depends_on = updated;

    let store = cached_issue_store(project)?;
    let existing = store.list().map_err(|e| anyhow!(e))?;
    shelbi_state::validate_depends_on(&tf.task, &existing).map_err(|e| anyhow!(e))?;
    store
        .set_fields(
            &args.id,
            shelbi_state::IssueFields {
                depends_on: Some(tf.task.depends_on.clone()),
                ..Default::default()
            },
        )
        .map_err(|e| anyhow!(e))?;
    if tf.task.depends_on.is_empty() {
        println!("✓ {} now has no dependencies", args.id);
    } else {
        println!(
            "✓ {} depends on: {}",
            args.id,
            tf.task.depends_on.join(", ")
        );
    }
    Ok(())
}

/// Load the workflow assigned to `issue`. Project defaults are resolved via
/// project config; a workflow that can't be loaded — whatever the reason —
/// falls back to the canonical default workflow with a stderr warning.
///
/// The fail-soft is deliberate and load-bearing: this sits on the status
/// transition path (`issue move` / `issue start`), and a workflow YAML can be
/// absent through no fault of the project's config — a stale daemon
/// managing an older state layout the CLI doesn't expect (the observed
/// field failure: a 0.1 daemon under a 0.3.2 CLI), or an in-repo
/// `<repo>/.shelbi/workflows/` momentarily blipped by a git checkout.
/// Hard-failing here froze the whole board from the CLI (bare
/// `io: ENOENT`, no state change) while the poller — whose transition
/// path already swallows this load — kept working. Falling back mirrors
/// the poller's behavior; the warning keeps a genuinely misconfigured
/// workflow loud.
fn resolve_task_workflow(project: &str, issue: &Issue) -> Result<Workflow> {
    let project_yaml = shelbi_state::load_project(project).ok();
    let name = project_yaml
        .as_ref()
        .map(|p| shelbi_state::resolve_task_workflow_name(p, issue))
        .unwrap_or_else(|| issue.workflow_or_default());
    match shelbi_state::load_workflow(project, name) {
        Ok(wf) => Ok(wf),
        Err(e) => {
            eprintln!(
                "warning: workflow `{name}` could not be loaded ({e}); using built-in default"
            );
            Ok(default_workflow())
        }
    }
}

/// Resolve which agent should drive the workspace once it lands in the
/// active (in-progress) status. `shelbi issue start` is an explicit user
/// invocation — we don't gate on Zen here even when the active status
/// is `owner: user` (the user typed the command, that's the override).
/// Falls back to the bundled `developer` agent when the workflow can't
/// be loaded or has no active-category status (legacy workflows without
/// the two-field design); the worktree's agent context still deploys so
/// the bundled developer prompt + skills are wired up correctly.
/// The workflow status `shelbi issue start` lands a card in: the canonical
/// `in-progress`, resolved by **exact id**, with a fallback to the workflow's
/// first active-category status for a legacy/renamed workflow that doesn't
/// declare `in-progress`. `start` always moves the card to
/// [`Column::in_progress`], so this status's `agent:` / `tags:` are the ones
/// that govern the spawned pane — resolving by exact id keeps a workflow that
/// declares a custom active status *before* `in-progress` from mis-resolving
/// to that earlier status.
fn start_destination_status(workflow: &Workflow) -> Option<&shelbi_core::WorkflowStatus> {
    workflow
        .status(Column::in_progress().as_str())
        .or_else(|| {
            workflow
                .statuses
                .iter()
                .find(|s| s.category == StatusCategory::Active)
        })
}

/// Where a dispatch lands the card, and thus which status's `agent:` / `tags:`
/// govern the spawned pane. Normally the canonical `in-progress` (see
/// [`start_destination_status`]) — but when the card is **already** parked in a
/// custom **agent-owned active** status (an intermediate gate like
/// `adversarial-review`, declared `owner: agent` + `category: active` between
/// `in-progress` and `review`), *that* status is the destination: the card
/// stays in the gate and the gate's own agent is dispatched onto it. Resolving
/// to `in-progress` instead would yank an `adversarial-review` card back out of
/// its gate and launch the developer rather than the gate's agent — the dispatch
/// half of the custom-active-status bug this fix closes.
///
/// Only an **agent-owned** active status short-circuits here. An `owner: user`
/// active status (or any non-active status) falls through to the canonical
/// resolution, so a human-driven column never auto-keeps a card in place — the
/// board reads exactly as it does for a single-active-status workflow, where the
/// card's current column is never a non-`in-progress` agent-owned active status.
fn dispatch_destination_status<'a>(
    workflow: &'a Workflow,
    current_column: &Column,
) -> Option<&'a shelbi_core::WorkflowStatus> {
    if let Some(status) = workflow.status(current_column.as_str()) {
        if status.category == StatusCategory::Active && status.owner == Owner::Agent {
            return Some(status);
        }
    }
    start_destination_status(workflow)
}

fn resolve_active_agent_for_dispatch(project: &str, issue: &Issue) -> Result<String> {
    use shelbi_orchestrator::dispatch::{resolve_dispatch_agent, DispatchDecision};
    use shelbi_state::DEVELOPER_AGENT;

    let workflow = resolve_task_workflow(project, issue)?;
    // The status the dispatch lands the card in governs the spawned pane — its
    // `agent:` field is the runner we want. Normally the canonical `in-progress`
    // (resolved by exact id); but a card already parked in a custom agent-owned
    // active gate keeps that gate as its destination, so the gate's own agent is
    // dispatched onto it rather than the developer (see
    // [`dispatch_destination_status`]).
    let active = dispatch_destination_status(&workflow, &issue.column);

    let zen_on = matches!(
        shelbi_state::read_state(project).map(|s| s.zen_mode),
        Ok(shelbi_state::ZenModeState::On),
    );

    let Some(status) = active else {
        // Workflow without an active status (rare; legacy minimal
        // workflows from before the two-field design). Fall back to the
        // built-in developer so the spawn path still mounts agent
        // context.
        return Ok(DEVELOPER_AGENT.to_string());
    };

    match resolve_dispatch_agent(status, zen_on) {
        DispatchDecision::Dispatch { agent } => Ok(agent),
        DispatchDecision::Skip(reason) => {
            // The CLI is the explicit-intent path: a `Skip` here means
            // the loader allowed a workflow whose active status has no
            // `agent:` (legacy, fully-human workflow). Fall back to the
            // developer agent so the spawn path still has *something*
            // to deploy, and surface the resolver's diagnostic so the
            // user knows why we didn't honor the workflow's wish.
            eprintln!(
                "shelbi: workflow `{}` active status had no dispatchable agent \
                 ({}); falling back to `{DEVELOPER_AGENT}`",
                workflow.name,
                reason.human_message(),
            );
            Ok(DEVELOPER_AGENT.to_string())
        }
    }
}

/// Sort a single column's listing into the canonical board order the stores use
/// *within* a column — priority ascending, then id — the final two keys of both
/// `list_tasks` (filesystem) and `sort_board` (github). `prio` applies it to the
/// index-derived listing so the position it computes is the position the store
/// re-derives under its lock from `list_in_status`: an index published out of
/// order (or a terminal page returned in some other order) can't then make the
/// CLI resolve a slot against a different ordering than the store acts on, which
/// would move the wrong card. The id tiebreak is what makes two cards that share
/// a priority reorder deterministically — both sides break the tie the same way.
fn sort_column_canonically(col: &mut [shelbi_state::IssueFile]) {
    col.sort_by(|a, b| {
        a.task
            .priority
            .cmp(&b.task.priority)
            .then_with(|| a.task.id.cmp(&b.task.id))
    });
}

/// Resolve the absolute destination slot a prio move lands `id` at within `col`
/// — the card's column, already in canonical order. Returns `Ok(None)` when
/// `col` doesn't contain `id` (the caller turns that into a column/workflow-named
/// error), or `Err` when no move flag was given. The slot is what `prio` hands
/// the store as `PrioMove::Set`; the store re-clamps it against its own live
/// column under the lock, so this never has to be the final word.
fn prio_slot_in(col: &[shelbi_state::IssueFile], id: &str, args: &PrioArgs) -> Result<Option<usize>> {
    let Some(pos) = col.iter().position(|x| x.task.id == id) else {
        return Ok(None);
    };
    let last = col.len().saturating_sub(1);
    let slot = if args.up {
        pos.saturating_sub(1)
    } else if args.down {
        (pos + 1).min(last)
    } else if args.top {
        0
    } else if args.bottom {
        last
    } else if let Some(n) = args.set {
        (n as usize).min(last)
    } else {
        bail!("specify one of --up, --down, --top, --bottom, --set N");
    };
    Ok(Some(slot))
}

fn prio(project: &str, args: PrioArgs) -> Result<()> {
    let tf = load_issue(project, &args.id)?;
    let status = &tf.task.column;
    // Build the column listing from the SAME source `issue list` / `show` read —
    // the daemon-owned `board-index.json` (via `read_open_board_for_cli`), never
    // the per-process `board-snapshot.json` that `list_in_status` serves on a
    // remote backend. That snapshot is only rewritten by a *dispatch*: `issue
    // move` patches the daemon index (so `list`/`show` show the card in its new
    // column at once) but never the snapshot, so a short-lived CLI one-shot right
    // after a move serves the stale copy — still placing the card in its old
    // column — and kicks only a background refresh it exits before finishing.
    // `prio` then couldn't reorder a card `list` plainly showed, on any workflow,
    // until the snapshot happened to refresh. Reading the index closes that
    // divergence (the same fix `workspace_occupied_by` got). A terminal
    // (`done`/`canceled`) column lives in the on-demand closed history the open
    // index omits, so that one case still reads through the store. Both sources
    // apply the same `sort_board` order (column, then priority, then id), so the
    // position lookup is deterministic even when two cards share a priority.
    let mut col: Vec<shelbi_state::IssueFile> = if is_terminal_status(status) {
        cached_issue_store(project)?
            .list_in_status(status)
            .map_err(|e| anyhow!(e))?
    } else {
        super::read_open_board_for_cli(project)?
            .into_iter()
            .filter(|f| &f.task.column == status)
            .collect()
    };
    // Resolve the slot against the same canonical within-column order the store
    // re-derives under its lock, so the two can't diverge and move the wrong card.
    sort_column_canonically(&mut col);
    let new_pos = prio_slot_in(&col, &args.id, &args)?.ok_or_else(|| {
        // The card resolved to `status` through its own live `get`, yet the
        // listing for that column doesn't contain it — a genuinely cold or
        // lagging board index, not a routing mismatch. Name the column and
        // workflow the lookup used so the operator can see where it looked.
        let workflow = shelbi_state::load_project(project)
            .ok()
            .map(|p| shelbi_state::resolve_task_workflow_name(&p, &tf.task).to_string())
            .unwrap_or_else(|| tf.task.workflow_or_default().to_string());
        anyhow!(
            "issue `{}` is in column `{status}` (workflow `{workflow}`) but that column's \
             listing doesn't contain it — the board index may be cold or lagging; retry once \
             the hub daemon has published it",
            args.id
        )
    })?;

    let store = cached_issue_store(project)?;
    store
        .set_priority(&args.id, shelbi_state::PrioMove::Set(new_pos as u32))
        .map_err(|e| anyhow!(e))?;
    println!("✓ {} now at slot {new_pos} in {status}", args.id);
    Ok(())
}

/// Undo the in_progress move `start` persisted before spawning, after the
/// spawn itself failed. Re-saves the pre-move frontmatter and renumbers
/// both the column we bumped the card out of and `in_progress` (where the
/// aborted card was briefly appended) so priorities stay contiguous.
fn rollback_start(project: &str, original: &Issue, _body: &str, prev_column: Column) -> Result<()> {
    let store = cached_issue_store(project)?;
    // Move the card back out of `in_progress` (where `start` appended it) and
    // restore its pre-dispatch owner/branch. `move_status` renumbers both the
    // source and destination columns so priorities stay contiguous.
    if prev_column != Column::in_progress() {
        store
            .move_status(&original.id, &prev_column, "rollback:start-failed")
            .map_err(|e| anyhow!(e))?;
    }
    store
        .set_fields(
            &original.id,
            shelbi_state::IssueFields {
                assigned_to: Some(original.assigned_to.clone()),
                branch: Some(original.branch.clone()),
                ..Default::default()
            },
        )
        .map_err(|e| anyhow!(e))?;
    Ok(())
}

/// Compose the dispatch event's `reason=` value by appending the
/// resolved agent name. `append_task_event` folds the embedded space into
/// an underscore so the final on-the-wire shape is
/// `<base>_agent=<agent>` — keeping the field readable to a human and to
/// the activity-feed parser without breaking the single-token contract.
fn dispatch_reason_with_agent(base: &str, agent: &str) -> String {
    format!("{base} agent={agent}")
}

/// `shelbi issue resume` — relaunch the assigned workspace on the issue it is
/// already working, WITHOUT discarding the in-flight worktree. The recovery
/// counterpart to [`start`]: where `start` wipes context (kills the pane,
/// re-checks-out a clean branch) for a fresh dispatch, `resume` is for a
/// stalled or killed worker — the tmux session died, the pane wedged, or the
/// agent stopped mid-issue — and gets it going again on the SAME issue with its
/// commits and uncommitted changes intact.
///
/// The worktree is preserved as-is (see
/// [`shelbi_orchestrator::workspace::resume_workspace_on_task`]); we never cut
/// or reset the branch here. We only touch the board when the card has drifted
/// out of `in_progress` (a killed worker whose card someone moved back), in
/// which case we restore it — mirroring `start`'s persist-before-spawn ordering
/// and rollback-on-failure so a failed relaunch never strands the card.
fn resume(
    project: &str,
    id: &str,
    workspace_arg: Option<&str>,
    reason: Option<&str>,
) -> Result<()> {
    let project_yaml = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let tf = load_issue(project, id)?;

    // Resolve workspace: explicit --workspace wins; otherwise reuse the issue's
    // existing assignment. A resume without either has nothing to relaunch.
    let workspace_name = workspace_arg
        .map(str::to_string)
        .or_else(|| tf.task.assigned_to.clone())
        .ok_or_else(|| {
            anyhow!(
                "issue `{id}` has no assigned workspace — pass `--workspace NAME` (the \
                 workspace whose worktree holds the in-flight work)"
            )
        })?;
    let workspace = project_yaml.workspace(&workspace_name).ok_or_else(|| {
        anyhow!(
            "workspace `{workspace_name}` not declared in project `{project}` (known: {})",
            project_yaml
                .workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    // Refuse to clobber a DIFFERENT in-flight issue on the same workspace —
    // same guard as `start`, sourced from live board state rather than the
    // dispatch-only snapshot. Resuming this issue onto a workspace busy with
    // another would leave two agents racing one worktree.
    ensure_workspace_dispatchable(project, &workspace_name, id)?;

    // Resolve the branch WITHOUT cutting or resetting it: the branch already
    // exists (the worker created + committed on it). Prefer the issue's recorded
    // branch, falling back through workflow/project/GitHub prefix resolution.
    let workflow = shelbi_state::load_task_workflow(project, &project_yaml, &tf.task)
        .map_err(|e| anyhow!(e))?;
    let branch =
        shelbi_orchestrator::branch::branch_name_for_task(&project_yaml, Some(&workflow), &tf.task)
            .map_err(|e| anyhow!(e))?;

    // Same agent-resolution as `start` — the active status's agent under the
    // project's Zen state, developer as the fallback.
    let agent_name =
        resolve_active_agent_for_dispatch(project, &tf.task).map_err(|e| anyhow!(e))?;

    // Restore `in_progress` if the card drifted out of it. Persist BEFORE the
    // relaunch (same ordering rationale as `start`): the board should reflect
    // the in-flight work the instant the agent can exist. `original` snapshots
    // the pre-move frontmatter so a spawn failure rolls the card back.
    let original = tf.task.clone();
    let prev_column = tf.task.column.clone();
    let moved_into_progress = prev_column != Column::in_progress();
    let store = cached_issue_store(project)?;
    if moved_into_progress {
        store
            .move_status(id, &Column::in_progress(), reason.unwrap_or("user:cli"))
            .map_err(|e| anyhow!(e))?;
    }
    store
        .set_fields(
            id,
            shelbi_state::IssueFields {
                assigned_to: Some(Some(workspace_name.clone())),
                branch: Some(Some(branch.clone())),
                ..Default::default()
            },
        )
        .map_err(|e| anyhow!(e))?;

    println!("→ resuming {workspace_name} on {id} (branch: {branch}, agent: {agent_name})");
    let addr = match shelbi_orchestrator::workspace::resume_workspace_on_task(
        shelbi_orchestrator::workspace::StartSpec {
            project: &project_yaml,
            workspace,
            task_id: id,
            branch: &branch,
            task_body: &tf.body,
            agent: Some(agent_name.as_str()),
            launch_override: tf.task.launch.as_ref(),
        },
    ) {
        Ok(addr) => addr,
        Err(e) => {
            // Roll the card back only if we moved it — a resume of an
            // already-in-progress issue left the board untouched, so there's
            // nothing to undo.
            if moved_into_progress {
                if let Err(re) = rollback_start(project, &original, &tf.body, prev_column.clone()) {
                    eprintln!(
                        "warning: `{id}` was restored to in_progress but the resume failed and \
                         the rollback also failed ({re}); run `shelbi issue move {id} --to \
                         {prev_column}` to recover"
                    );
                }
            }
            return Err(anyhow!(e).context("resuming workspace"));
        }
    };

    // Only a card we actually moved records a dispatch event — a resume of an
    // already-in-progress issue leaves no misleading transition line.
    if moved_into_progress {
        let base_reason = reason.unwrap_or("user:cli:resume");
        let dispatched_reason = dispatch_reason_with_agent(base_reason, &agent_name);
        let workflow = shelbi_state::resolve_task_workflow_name(&project_yaml, &tf.task);
        if let Err(e) = shelbi_state::append_task_event(
            project,
            id,
            workflow,
            prev_column.clone(),
            Column::in_progress(),
            &dispatched_reason,
        ) {
            eprintln!("warning: append_task_event failed: {e}");
        }
    }

    println!("✓ {id} resumed on {workspace_name} ({})", addr.label());
    Ok(())
}

/// Whether any non-interactive field flag was supplied. When none were (and no
/// body arrived on stdin), `edit` falls back to opening `$EDITOR` — the
/// historical zero-flag behavior.
fn edit_has_field_flags(args: &EditArgs, stdin_body: &Option<String>) -> bool {
    args.title.is_some()
        || args.body.is_some()
        || args.body_file.is_some()
        || args.append
        || args.workflow.is_some()
        || args.branch.is_some()
        || args.prefers_machine.is_some()
        || args.no_prefers_machine
        || !args.sub.is_empty()
        || !args.sub_regex.is_empty()
        || stdin_body.is_some()
}

/// Whether `edit` should consult stdin for the body. False when an explicit
/// body source (`--body`/`--body-file`) was given — the caller already supplied
/// the body, so reading stdin would only risk blocking a non-interactive
/// invocation whose stdin never closes. Reading stdin is reserved for the
/// `shelbi issue edit x <<EOF` spelling, where the body comes from stdin.
fn edit_should_read_stdin(args: &EditArgs) -> bool {
    args.body.is_none() && args.body_file.is_none()
}

/// Reconstruct the command-line order of `--sub` / `--sub-regex` occurrences.
///
/// clap's derive can't preserve the *relative* order of two distinct
/// repeatable options into typed fields, but substitutions must apply in the
/// exact order the user typed them (each seeing the previous one's output). So
/// we recover that order by scanning the raw argv: each flag is followed by
/// exactly its two values (clap already validated arity and
/// `allow_hyphen_values` lets a value start with `-`), so we consume the two
/// tokens after each occurrence and skip everything else.
///
/// If the scan doesn't reproduce the clap-parsed multiset — the only way that
/// happens is an exotic attached-value form the tests don't use — we fall back
/// to a deterministic order (all `--sub` in parse order, then all
/// `--sub-regex`), which still satisfies "each sub sees the previous result".
fn ordered_substitutions<I, S>(argv: I, args: &EditArgs) -> Vec<SubOp>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let toks: Vec<String> = argv.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut scanned: Vec<SubOp> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let is_literal = toks[i] == "--sub";
        let is_regex = toks[i] == "--sub-regex";
        if (is_literal || is_regex) && i + 2 < toks.len() {
            let a = toks[i + 1].clone();
            let b = toks[i + 2].clone();
            scanned.push(if is_literal {
                SubOp::Literal { from: a, to: b }
            } else {
                SubOp::Regex {
                    pattern: a,
                    replacement: b,
                }
            });
            i += 3;
        } else {
            i += 1;
        }
    }

    let scanned_literals = scanned
        .iter()
        .filter(|op| matches!(op, SubOp::Literal { .. }))
        .count();
    let scanned_regex = scanned.len() - scanned_literals;
    if scanned_literals == args.sub.len() / 2 && scanned_regex == args.sub_regex.len() / 2 {
        return scanned;
    }

    // Fallback: reconstruct from the parsed pairs in a stable order.
    let mut ops = Vec::new();
    for pair in args.sub.as_chunks::<2>().0 {
        ops.push(SubOp::Literal {
            from: pair[0].clone(),
            to: pair[1].clone(),
        });
    }
    for pair in args.sub_regex.as_chunks::<2>().0 {
        ops.push(SubOp::Regex {
            pattern: pair[0].clone(),
            replacement: pair[1].clone(),
        });
    }
    ops
}

/// `shelbi issue edit` — resolve the body source (client-side stdin / file read
/// / `$EDITOR`), then build an [`EditSpec`] and run the mutation. Substitution
/// application, append composition, field validation, and the write live in
/// `shelbi_orchestrator::mutate`.
fn edit(project: &str, args: EditArgs) -> Result<()> {
    // Only consult stdin when NO explicit body source was passed. `--body` /
    // `--body-file` already carry the body, so touching `stdin()` would
    // pointlessly block a non-interactive caller whose stdin never reaches EOF
    // (backgrounded process, inherited pipe) — the hang this gate exists to
    // prevent. Reading stdin here is reserved for the `shelbi issue edit x <<EOF`
    // spelling, where the body is *meant* to come from stdin.
    let stdin_body = if edit_should_read_stdin(&args) {
        read_piped_stdin()?
    } else {
        None
    };
    // Zero-flag invocation preserves the historical behavior: open the file in
    // `$EDITOR`. Any field flag (or a piped body) switches to non-interactive
    // mode.
    if !edit_has_field_flags(&args, &stdin_body) {
        let path = shelbi_state::task_path(project, &args.id).map_err(|e| anyhow!(e))?;
        if !path.exists() {
            bail!("issue `{}` not found", args.id);
        }
        return super::launch_editor(&path);
    }
    if args.prefers_machine.is_some() && args.no_prefers_machine {
        bail!("--prefers-machine and --no-prefers-machine are mutually exclusive");
    }
    // Recover the exact command-line order of `--sub`/`--sub-regex` (clap can't).
    let subs = ordered_substitutions(
        std::env::args_os().map(|s| s.to_string_lossy().into_owned()),
        &args,
    );

    // Resolve the single body source from the flags/stdin available only to the
    // client (the daemon can't read this process's stdin or cwd files). Pass the
    // RAW text through — the library trims and composes (`--append`).
    let file_body = match &args.body_file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading --body-file {}", path.display()))?,
        ),
        None => None,
    };
    let sources: Vec<String> = [args.body.clone(), file_body, stdin_body]
        .into_iter()
        .flatten()
        .collect();
    if sources.len() > 1 {
        bail!(
            "multiple body sources given — pass the body exactly one way \
             (--body, --body-file, or piped stdin)"
        );
    }
    let source = sources.into_iter().next();
    let (body_replace, body_append) = match (source, args.append) {
        (None, true) => bail!("--append needs a body source (--body, --body-file, or piped stdin)"),
        (None, false) => (None, None),
        (Some(text), true) => (None, Some(text)),
        (Some(text), false) => (Some(text), None),
    };
    let prefers_machine = if let Some(m) = args.prefers_machine {
        Some(Some(m))
    } else if args.no_prefers_machine {
        Some(None)
    } else {
        None
    };
    let spec = EditSpec {
        title: args.title,
        body_replace,
        body_append,
        subs,
        allow_no_match: args.allow_no_match,
        workflow: args.workflow,
        branch: args.branch,
        prefers_machine,
        reason: args.reason,
    };
    mutate_client::run_mutation(project, &args.id, MutationKind::Edit(Box::new(spec)))
}

fn rm(project: &str, id: &str) -> Result<()> {
    let tf = load_issue(project, id)?;
    let column = tf.task.column;
    let store = cached_issue_store(project)?;
    store.delete(id).map_err(|e| anyhow!(e))?;
    store.renumber(&column).map_err(|e| anyhow!(e))?;
    println!("✓ {id} deleted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::commands::test_support::ENV_LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-cli-issue-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        // Register the default `p` project so board commands can resolve an
        // `IssueStore` (the migrated CLI paths route reads/writes through the
        // project's configured backend, which requires a loadable project YAML).
        // Tests that need a richer project overwrite this via
        // `write_project_yaml*` with the same name.
        write_project_yaml(&p, "p");
        p
    }

    fn task_in(column: Column, id: &str) -> Issue {
        let now = Utc::now();
        Issue {
            id: id.into(),
            title: id.replace('-', " "),
            column,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    fn add_args(title: &str) -> AddArgs {
        AddArgs {
            title: title.into(),
            id: None,
            status: "backlog".into(),
            description: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            workflow: None,
            branch: None,
        }
    }

    #[test]
    fn add_stdin_gate_skips_stdin_when_description_given() {
        // Companion to the `edit` gate: `-d`/`--description` already carries the
        // body, so `add` must not block reading a stdin that never closes.
        let mut a = add_args("t");
        a.description = Some("inline".into());
        assert!(!add_should_read_stdin(&a), "--description must skip stdin");

        assert!(
            add_should_read_stdin(&add_args("t")),
            "no description must still consult stdin (heredoc spelling)"
        );
    }

    #[test]
    fn list_workflow_filter_composes_with_column_and_ready() {
        // Three issues across two workflows; verify the filter wiring on
        // each list mode (default / --column / --ready) returns Ok and
        // doesn't panic when the filter matches zero, one, or all issues.
        // Output assertions live behind a refactor (split compute from
        // render); the smoke test catches accidental regressions in the
        // wiring.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let mut a = task_in(Column::todo(), "a");
        a.workflow = Some("research".into());
        let b = task_in(Column::todo(), "b"); // no workflow → matches `default`
        let mut c = task_in(Column::backlog(), "c");
        c.workflow = Some("research".into());
        for t in [&a, &b, &c] {
            shelbi_state::save_task("p", t, "").unwrap();
        }

        // Workflow filter alone — should not error.
        list("p", None, false, Some("research")).unwrap();
        list("p", None, false, Some("default")).unwrap();
        list("p", None, false, Some("nonexistent")).unwrap();

        // Composes with --column.
        list("p", Some("todo"), false, Some("research")).unwrap();
        list("p", Some("backlog"), false, Some("research")).unwrap();

        // Composes with --ready.
        list("p", None, true, Some("research")).unwrap();
        list("p", None, true, Some("default")).unwrap();
        list("p", None, true, Some("nonexistent")).unwrap();

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn list_workflow_default_matches_tasks_without_explicit_workflow() {
        // `--workflow default` must match issues whose frontmatter omits
        // `workflow:` entirely — that's the contract `Issue::workflow_or_default`
        // promises and the contract callers (orchestrator, future TUI
        // filter) rely on. Verified by exercising the matcher closure
        // directly through the filter_workflow_name helper-equivalent
        // pattern used inside `list`.
        let no_explicit = task_in(Column::todo(), "n");
        assert_eq!(no_explicit.workflow_or_default(), "default");

        let mut research = task_in(Column::todo(), "r");
        research.workflow = Some("research".into());
        assert_eq!(research.workflow_or_default(), "research");
    }

    fn write_workflow(project: &str, name: &str, yaml: &str) {
        let dir = shelbi_state::workflows_dir(project).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.yaml")), yaml).unwrap();
    }

    fn write_project_yaml(home: &std::path::Path, name: &str) {
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(
            home.join(format!("projects/{name}.yaml")),
            format!(
                r#"name: {name}
repo: /tmp/{name}
default_branch: main
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: local
    kind: local
    work_dir: /tmp/{name}
workspaces:
  - {{ name: dev, machine: local, runner: claude }}
"#
            ),
        )
        .unwrap();
    }

    fn materialize_default_agents_for_test(project: &str) {
        // The workflow loader rejects `agent:` references that don't
        // point at a real `agents/<name>/` directory. The resolver's
        // workflow loads pass through that check, so the test fixture
        // has to materialize the default agent set just like a real
        // `shelbi init` does.
        shelbi_state::materialize_default_agents(project).unwrap();
    }

    #[test]
    fn dispatch_reason_appends_agent_segment_for_both_default_and_orchestrator_paths() {
        // Acceptance (a) — every event emitted when `shelbi issue start`
        // spawns a workspace must include `_agent=<name>` in `reason=`.
        // The helper composes the raw reason; `append_task_event` folds
        // the embedded space into the underscore that ends up on disk.

        // Default (user-driven) reason.
        let r = dispatch_reason_with_agent("user:cli:start", "developer");
        assert_eq!(r, "user:cli:start agent=developer");

        // Orchestrator-supplied reason (the auto-dispatch contract from
        // the default orchestrator playbook).
        let r =
            dispatch_reason_with_agent("orchestrator:auto-dispatch workspace=alpha", "developer");
        assert_eq!(
            r,
            "orchestrator:auto-dispatch workspace=alpha agent=developer"
        );

        // After the sanitizer runs (whitespace → underscore) the on-disk
        // shape becomes a single parseable token — that's what the
        // activity-feed parser keys off `_agent=` to extract.
        let sanitized: String = r
            .chars()
            .map(|c| if c.is_whitespace() { '_' } else { c })
            .collect();
        assert_eq!(
            sanitized,
            "orchestrator:auto-dispatch_workspace=alpha_agent=developer"
        );
    }

    #[test]
    fn start_event_line_carries_agent_segment_via_move_to_round_trip() {
        // Acceptance (a) end-to-end check: the on-disk line shape after
        // emission contains the `_agent=<name>` segment. We exercise the
        // emission path through `append_task_event` directly with the
        // composed reason (mirrors what `start()` writes) so the test
        // doesn't need to stand up a real tmux pane to spawn the
        // workspace.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let dispatched =
            dispatch_reason_with_agent("orchestrator:auto-dispatch workspace=alpha", "developer");
        shelbi_state::append_task_event(
            "demo",
            "demo-issue",
            "default",
            Column::todo(),
            Column::in_progress(),
            &dispatched,
        )
        .unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(
            log.contains(" reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer "),
            "log: {log}",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_active_agent_dispatches_developer_for_default_workflow() {
        // Acceptance criterion (a) from the issue: a default `shelbi issue
        // start` resolves the active status's agent and lands on
        // `developer`. The resolver doesn't care about Zen mode for an
        // `owner: agent` status, so this passes regardless of state.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents_for_test("p");
        write_workflow(
            "p",
            "default",
            r#"
name: default
statuses:
  - { id: backlog,     name: Backlog,    category: backlog,  owner: user                       }
  - { id: todo,        name: Todo,       category: ready,    owner: agent, agent: orchestrator }
  - { id: in-progress, name: InProgress, category: active,   owner: agent, agent: developer    }
  - { id: review,      name: Review,     category: handoff,  owner: user,  agent: orchestrator }
  - { id: done,        name: Done,       category: done,     owner: user                       }
"#,
        );

        let issue = task_in(Column::todo(), "t1");
        let agent = resolve_active_agent_for_dispatch("p", &issue).unwrap();
        assert_eq!(agent, "developer");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_active_agent_falls_back_to_developer_for_workflow_without_active() {
        // A legacy workflow without an `active`-category status (rare,
        // but possible with the historic minimal flow). The resolver
        // can't resolve through the workflow, so it falls back to the
        // bundled `developer` agent — that way the spawn path still
        // mounts agent context, instead of silently dispatching with
        // nothing.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents_for_test("p");
        write_workflow(
            "p",
            "default",
            r#"
name: default
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user }
  - { id: done,    name: Done,    category: done,    owner: user }
"#,
        );
        let issue = task_in(Column::todo(), "t2");
        let agent = resolve_active_agent_for_dispatch("p", &issue).unwrap();
        assert_eq!(agent, shelbi_state::DEVELOPER_AGENT);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A workflow with a custom agent-owned active gate (`adversarial-review`)
    /// between `in-progress` and `review`, plus an `owner: user` active status
    /// (`staging`) to prove that only *agent*-owned active gates auto-keep a
    /// card in place. This is the [add-to-workflow] topology the fix targets.
    const GATED_WORKFLOW: &str = r#"
name: default
statuses:
  - { id: backlog,            name: Backlog,           category: backlog, owner: user }
  - { id: todo,               name: Todo,              category: ready,   owner: agent, agent: orchestrator }
  - { id: in-progress,        name: InProgress,        category: active,  owner: agent, agent: developer }
  - { id: adversarial-review, name: AdversarialReview, category: active,  owner: agent, agent: adversarial-review }
  - { id: staging,            name: Staging,           category: active,  owner: user,  agent: qa }
  - { id: review,             name: Review,            category: handoff, owner: user }
  - { id: done,               name: Done,              category: done,    owner: user }
"#;

    #[test]
    fn dispatch_destination_keeps_agent_owned_active_gates_and_falls_through_otherwise() {
        // Pure resolver — the "dispatch path test harness" for criterion (a),
        // asserted without a live pane. No SHELBI_HOME needed.
        let wf = Workflow::from_yaml_str(GATED_WORKFLOW).unwrap();

        // A card parked in the agent-owned `adversarial-review` gate stays in
        // the gate: that status IS the destination, so the gate's own agent is
        // dispatched onto it in place.
        assert_eq!(
            dispatch_destination_status(&wf, &Column::from_status_id("adversarial-review"))
                .map(|s| s.id.as_str()),
            Some("adversarial-review"),
        );

        // A card in `todo` (or any non-active status) resolves to the canonical
        // `in-progress` — a single-active-status dispatch is unchanged (d).
        assert_eq!(
            dispatch_destination_status(&wf, &Column::todo()).map(|s| s.id.as_str()),
            Some("in-progress"),
        );

        // An `owner: user` active status must NOT short-circuit: a human-driven
        // column falls through to the canonical resolution, so it never
        // auto-keeps a card in place (criterion c — nothing auto-launches there).
        assert_eq!(
            dispatch_destination_status(&wf, &Column::from_status_id("staging"))
                .map(|s| s.id.as_str()),
            Some("in-progress"),
        );
    }

    /// The `statuses.yaml` catalog for the gated topology — the identity half
    /// (id/name/category) the workflow file below references. `load_workflow`
    /// requires it to exist and resolves the workflow's categories against it.
    const GATED_STATUSES: &str = r#"
statuses:
  - { id: backlog,            name: Backlog,           category: backlog }
  - { id: todo,               name: Todo,              category: ready }
  - { id: in-progress,        name: InProgress,        category: active }
  - { id: adversarial-review, name: AdversarialReview, category: active }
  - { id: review,             name: Review,            category: handoff }
  - { id: done,               name: Done,              category: done }
"#;

    /// The workflow file for the gated topology: references only (id + owner +
    /// agent), no repeated name/category. `adversarial` and `developer` are
    /// materialized by `materialize_default_agents`, so the agent references
    /// validate.
    const GATED_WORKFLOW_REFS: &str = r#"
name: default
statuses:
  - { id: backlog,            owner: user }
  - { id: todo,               owner: agent, agent: orchestrator }
  - { id: in-progress,        owner: agent, agent: developer }
  - { id: adversarial-review, owner: agent, agent: adversarial }
  - { id: review,             owner: user }
  - { id: done,               owner: user }
"#;

    fn write_statuses(project: &str, yaml: &str) {
        let dir = shelbi_state::workflows_dir(project).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("statuses.yaml"), yaml).unwrap();
    }

    #[test]
    fn resolve_active_agent_dispatches_the_gate_agent_for_a_parked_custom_active_status() {
        // Criterion (a), end to end through the real workflow loader: a card
        // already sitting in the agent-owned `adversarial-review` gate resolves
        // to that gate's agent, not the developer of `in-progress`. Asserted
        // through the dispatch resolver, not a spawned pane.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents_for_test("p");
        write_statuses("p", GATED_STATUSES);
        write_workflow("p", "default", GATED_WORKFLOW_REFS);

        let parked = task_in(Column::from_status_id("adversarial-review"), "t-gate");
        assert_eq!(
            resolve_active_agent_for_dispatch("p", &parked).unwrap(),
            "adversarial",
        );

        // Criterion (d): a card still in `todo` dispatches the canonical
        // developer, exactly as a single-active-status workflow does.
        let queued = task_in(Column::todo(), "t-queued");
        assert_eq!(
            resolve_active_agent_for_dispatch("p", &queued).unwrap(),
            "developer",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resume_without_assignment_or_workspace_flag_errors() {
        // A resume needs to know which workspace holds the in-flight work.
        // With no `assigned_to` and no `--workspace`, it must fail cleanly
        // (before touching any pane) rather than guessing.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        crate::commands::test_support::provision_hub_repo_for_project(&home, "p");

        shelbi_state::save_task("p", &task_in(Column::in_progress(), "orphan"), "").unwrap();
        let err = resume("p", "orphan", None, None).unwrap_err().to_string();
        assert!(err.contains("no assigned workspace"), "err: {err}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resume_rejects_unknown_workspace() {
        // An explicit `--workspace` that isn't declared in the project must
        // be rejected with the known-workspaces list, same as `start`/`assign`.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // provision_hub_repo_for_project declares no workspaces, so any name
        // is "unknown" — exactly the case under test.
        crate::commands::test_support::provision_hub_repo_for_project(&home, "p");

        shelbi_state::save_task("p", &task_in(Column::in_progress(), "t"), "").unwrap();
        let err = resume("p", "t", Some("ghost"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("workspace `ghost` not declared"), "err: {err}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // -------------------------------------------------------------------
    // issue edit (non-interactive)

    fn edit_args(id: &str) -> EditArgs {
        EditArgs {
            id: id.into(),
            title: None,
            body: None,
            body_file: None,
            append: false,
            workflow: None,
            branch: None,
            prefers_machine: None,
            no_prefers_machine: false,
            sub: Vec::new(),
            sub_regex: Vec::new(),
            allow_no_match: false,
            reason: None,
        }
    }

    #[test]
    fn edit_stdin_gate_skips_stdin_when_explicit_body_source() {
        // Regression guard for the hang where `edit` blocked on a stdin that
        // never reached EOF even though `--body`/`--body-file` already carried
        // the body. `edit_should_read_stdin` is the pure gate `edit()` keys off
        // of BEFORE ever touching `stdin()`, so pinning it here keeps the fix
        // from regressing without needing a real blocking stdin in the test.
        let mut a = edit_args("x");
        a.body = Some("inline".into());
        assert!(!edit_should_read_stdin(&a), "--body must skip stdin");

        let mut a = edit_args("x");
        a.body_file = Some(PathBuf::from("/tmp/whatever.md"));
        assert!(!edit_should_read_stdin(&a), "--body-file must skip stdin");

        // No explicit source: stdin is still the body channel (heredoc spelling).
        assert!(
            edit_should_read_stdin(&edit_args("x")),
            "no body source must still consult stdin"
        );
        let mut a = edit_args("x");
        a.append = true;
        assert!(
            edit_should_read_stdin(&a),
            "--append with no --body/--body-file still reads stdin as its source"
        );
    }

    #[test]
    fn ordered_substitutions_recovers_interleaved_cli_order() {
        // Pure function — no SHELBI_HOME / env setup needed.
        let mut args = edit_args("x");
        // Two --sub and one --sub-regex, interleaved on the command line.
        args.sub = vec!["a".into(), "b".into(), "e".into(), "f".into()];
        args.sub_regex = vec!["c".into(), "d".into()];
        let argv = [
            "shelbi",
            "task",
            "edit",
            "x",
            "--sub",
            "a",
            "b",
            "--sub-regex",
            "c",
            "d",
            "--sub",
            "e",
            "f",
        ];
        let ops = ordered_substitutions(argv, &args);
        assert_eq!(
            ops,
            vec![
                SubOp::Literal {
                    from: "a".into(),
                    to: "b".into()
                },
                SubOp::Regex {
                    pattern: "c".into(),
                    replacement: "d".into()
                },
                SubOp::Literal {
                    from: "e".into(),
                    to: "f".into()
                },
            ]
        );
    }

    // --- GitHub-backed command paths -----------------------------------------
    //
    // These exercise the real `assign` / `unassign` / `show` / `start` command
    // functions against a project whose `issue_tracker.backend` is `github`,
    // with a fake `gh` runner injected via `shelbi_state::set_test_gh_runner`
    // (the `test-support` feature). They prove the migrated command paths reach
    // the configured backend — not the local filesystem board — and that a
    // GitHub-only issue needs no local task markdown file to operate on.

    /// A project YAML selecting the GitHub backend (`owner/repo`) with one plain
    /// `dev` workspace. No `tasks/` directory is ever created for it.
    fn write_github_project_yaml(home: &std::path::Path, name: &str) {
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(
            home.join(format!("projects/{name}.yaml")),
            format!(
                r#"name: {name}
repo: /tmp/{name}
default_branch: main
issue_tracker:
  backend: github
  github:
    repo: owner/repo
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: local
    kind: local
    work_dir: /tmp/{name}
workspaces:
  - {{ name: dev, machine: local, runner: claude }}
"#
            ),
        )
        .unwrap();
    }

    /// Install a fake `gh` runner that answers every issue read with `issue_json`
    /// (a single JSONL object), an empty set for `/labels` and `/comments`, and
    /// `{}` for any mutating call — enough for the read-only + overlay-only
    /// command paths under test to run without a network or a real repo.
    fn install_gh_issue_runner(issue_json: String) {
        shelbi_state::set_test_gh_runner(move |args: &[&str]| -> shelbi_core::Result<String> {
            // The reworked `get` reads through GraphQL (search → single-issue),
            // so answer those queries by reshaping the same REST `issue_json`.
            if args.contains(&"graphql") {
                return Ok(gh_issue_to_graphql(args, &issue_json));
            }
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            if method != "GET" {
                return Ok("{}".to_string());
            }
            let path = args
                .iter()
                .find(|a| a.contains("repos/"))
                .copied()
                .unwrap_or("");
            if path.ends_with("/labels") || path.contains("/comments") {
                return Ok(String::new());
            }
            Ok(issue_json.to_string())
        });
    }

    /// Reshape one REST issue object into the GraphQL response the reworked
    /// `get`/`fetch`/`search` path expects, dispatching on the query name.
    fn gh_issue_to_graphql(args: &[&str], issue_json: &str) -> String {
        let joined = args.join(" ");
        let v: serde_json::Value =
            serde_json::from_str(issue_json.trim()).unwrap_or(serde_json::Value::Null);
        if joined.contains("IdSearch") {
            return match v.get("number").and_then(serde_json::Value::as_i64) {
                Some(n) => format!(r#"{{"data":{{"search":{{"nodes":[{{"number":{n}}}]}}}}}}"#),
                None => r#"{"data":{"search":{"nodes":[]}}}"#.to_string(),
            };
        }
        let label_nodes: Vec<serde_json::Value> = v
            .get("labels")
            .and_then(|l| l.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|l| serde_json::json!({ "name": l.get("name").cloned().unwrap_or(serde_json::Value::Null) }))
            .collect();
        let node = serde_json::json!({
            "number": v.get("number").cloned().unwrap_or(serde_json::json!(0)),
            "title": v.get("title").cloned().unwrap_or(serde_json::json!("")),
            "state": v.get("state").and_then(|s| s.as_str()).unwrap_or("open").to_uppercase(),
            "stateReason": v.get("state_reason").cloned().unwrap_or(serde_json::Value::Null),
            "createdAt": v.get("created_at").cloned().unwrap_or(serde_json::json!("2026-01-01T00:00:00Z")),
            "updatedAt": v.get("updated_at").cloned().unwrap_or(serde_json::json!("2026-01-01T00:00:00Z")),
            "body": v.get("body").cloned().unwrap_or(serde_json::json!("")),
            "labels": { "nodes": label_nodes },
        })
        .to_string();
        if joined.contains("IssuesByNumber") {
            return format!(r#"{{"data":{{"repository":{{"i0":{node}}}}}}}"#);
        }
        format!(r#"{{"data":{{"repository":{{"issue":{node}}}}}}}"#)
    }

    /// One GitHub issue object (JSONL) with the given shelbi id + status label.
    fn gh_issue_json(id: &str, status: &str) -> String {
        format!(
            r#"{{"number":7,"title":"{id}","body":"Prose for {id}.","state":"open","labels":[{{"name":"shelbi:id/{id}"}},{{"name":"shelbi:status/{status}"}}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}}"#
        )
    }

    #[test]
    fn show_renders_a_github_only_issue_with_no_local_file() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_github_project_yaml(&home, "gh");
        install_gh_issue_runner(gh_issue_json("t", "review"));

        // `show` reads through the store; there is no `tasks/t.md` on disk, and
        // the command must still succeed (the first-pass regression).
        show("gh", "t").expect("show renders a GitHub-only issue");
        assert!(
            !shelbi_state::task_path("gh", "t").unwrap().exists(),
            "no local task markdown file should be required or created"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // --- occupancy guard: live board, not the dispatch-only snapshot ----------

    /// An [`Issue`] assigned to `ws`, in `column` — the shape a workspace
    /// occupant takes on the board.
    fn task_assigned(id: &str, column: Column, ws: &str) -> Issue {
        Issue {
            assigned_to: Some(ws.to_string()),
            ..task_in(column, id)
        }
    }

    fn issue_file(task: Issue) -> shelbi_state::IssueFile {
        shelbi_state::IssueFile {
            task,
            body: String::new(),
            tracker_assignees: Vec::new(),
        }
    }

    /// Seed a fresh, current daemon index for the `gh` project (repo identity
    /// stamped so every reader accepts it).
    fn write_gh_index(board: Vec<shelbi_state::IssueFile>) {
        let mut idx = shelbi_state::BoardIndex::fresh(board);
        idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
        shelbi_state::write_board_index("gh", &idx).unwrap();
    }

    /// The incident: a card that finished (its PR merged, moving it to a terminal
    /// column out-of-band) was never cleared from `board-snapshot.json`, so the
    /// stale entry pinned its workspace as `in_progress` and locked it out of
    /// every future dispatch. The guard must read the live daemon index (which
    /// drops closed cards) instead, so the stranded snapshot entry is inert.
    #[test]
    fn workspace_occupied_by_ignores_a_done_card_stranded_in_the_stale_snapshot() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_github_project_yaml(&home, "gh");

        // Snapshot still lists a long-since-done card as in_progress on `dev`.
        shelbi_state::seed_board_snapshot_for_test(
            "gh",
            &[issue_file(task_assigned(
                "stranded-done",
                Column::in_progress(),
                "dev",
            ))],
        );
        // The live index is current: the card has closed and is absent.
        write_gh_index(Vec::new());

        assert!(
            workspace_occupied_by("gh", "dev", "next-card")
                .unwrap()
                .is_none(),
            "a stale snapshot entry must not manufacture a phantom occupant"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A genuine in-flight card is still reported, and the refusal names its real
    /// column. The card excluded by id is never its own occupant.
    #[test]
    fn workspace_occupied_by_reports_a_live_in_progress_card_with_its_real_column() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_github_project_yaml(&home, "gh");
        write_gh_index(vec![issue_file(task_assigned(
            "live-1",
            Column::in_progress(),
            "dev",
        ))]);

        let occ = workspace_occupied_by("gh", "dev", "next-card")
            .unwrap()
            .expect("a live in_progress card occupies the workspace");
        assert_eq!(occ.id, "live-1");
        assert_eq!(occ.column, Column::in_progress());

        // Excluding the card by id (a resume / same-slot restart) frees it.
        assert!(workspace_occupied_by("gh", "dev", "live-1")
            .unwrap()
            .is_none());

        // The shared bail wording carries the real column.
        let err = ensure_workspace_dispatchable("gh", "dev", "next-card")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("live-1") && err.contains("in_progress"),
            "err should name the blocking card and its real column: {err}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // --- prio: reorder from the live index, on any workflow ------------------

    /// A fully-defaulted [`PrioArgs`] with the one chosen move set — the shape
    /// `clap` produces once its `conflicts_with_all` groups are satisfied.
    fn prio_args(id: &str, up: bool, down: bool, top: bool, bottom: bool, set: Option<u32>) -> PrioArgs {
        PrioArgs { id: id.into(), up, down, top, bottom, set }
    }

    /// The reported bug: `shelbi issue prio` couldn't reorder a card that `issue
    /// list` / `show` plainly placed in a column, right after `issue move`, on a
    /// non-default workflow. Root cause: `prio` built its column listing from
    /// `list_in_status` — the per-process `board-snapshot.json`, which only a
    /// *dispatch* rewrites — while `list` / `show` read the daemon-owned
    /// `board-index.json` that `issue move` patches at once. Here the snapshot is
    /// stale (card still in `backlog`) while the index is current (card in the
    /// custom `qa-review` status, a status id no default workflow declares). The
    /// old code filtered the stale snapshot for `qa-review`, found nothing, and
    /// failed `not found in column listing`; the fix reads the index and reorders
    /// it — proving the path is workflow-agnostic and move-fresh.
    #[test]
    fn prio_reorders_from_the_live_index_not_the_stale_snapshot() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_github_project_yaml(&home, "gh");

        // The live backend `get` (prio's `load_issue`) and `set_priority`'s
        // column read both resolve the card into the custom `qa-review` status.
        install_gh_issue_runner(gh_issue_json("rt-daemon-lifecycle", "qa-review"));

        // Dispatch-only snapshot still shows the pre-move `backlog` column — the
        // shape right after an `issue move` the snapshot never saw. The OLD prio
        // filtered THIS for `qa-review` and found nothing.
        shelbi_state::seed_board_snapshot_for_test(
            "gh",
            &[issue_file(task_in(
                Column::from_status_id("backlog"),
                "rt-daemon-lifecycle",
            ))],
        );
        // The daemon index is current — exactly what `issue list` / `show` render.
        write_gh_index(vec![issue_file(task_in(
            Column::from_status_id("qa-review"),
            "rt-daemon-lifecycle",
        ))]);

        for args in [
            prio_args("rt-daemon-lifecycle", false, false, true, false, None), // --top
            prio_args("rt-daemon-lifecycle", true, false, false, false, None), // --up
            prio_args("rt-daemon-lifecycle", false, true, false, false, None), // --down
            prio_args("rt-daemon-lifecycle", false, false, false, true, None), // --bottom
            prio_args("rt-daemon-lifecycle", false, false, false, false, Some(1)), // --set 1
        ] {
            prio("gh", args)
                .expect("prio reorders a card the live index shows, despite a stale snapshot");
        }

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// One card in `column` with an explicit priority — the shape a column
    /// listing carries, so a reorder has real positions to resolve against.
    fn card_in(column: Column, id: &str, priority: u32) -> shelbi_state::IssueFile {
        issue_file(Issue {
            priority,
            ..task_in(column, id)
        })
    }

    /// Mirror of the store's documented reorder (`set_priority`: remove the card
    /// from its current slot, insert it at `slot`) so a test can assert the id
    /// order a computed slot produces — `prio` hands this exact slot to the store
    /// as `PrioMove::Set`, and the store re-derives the same index under its lock.
    fn order_after(col: &[shelbi_state::IssueFile], id: &str, slot: usize) -> Vec<String> {
        let mut ids: Vec<String> = col.iter().map(|f| f.task.id.clone()).collect();
        let idx = ids.iter().position(|x| x == id).expect("card is in the column");
        let moved = ids.remove(idx);
        ids.insert(slot, moved);
        ids
    }

    /// Point 1 of the fix's contract: every move resolves to a real, distinct
    /// slot in a column of three, and the slot is the one the store will act on.
    /// Uses the custom `qa-review` status (no default workflow declares it) so the
    /// move is also proven workflow-agnostic at the arithmetic layer.
    #[test]
    fn prio_slot_in_resolves_every_move_in_a_three_card_column() {
        let col_in = Column::from_status_id("qa-review");
        // Built deliberately out of priority order; `prio` sorts canonically first.
        let mut col = vec![
            card_in(col_in.clone(), "c", 2),
            card_in(col_in.clone(), "a", 0),
            card_in(col_in.clone(), "b", 1),
        ];
        sort_column_canonically(&mut col);
        assert_eq!(
            col.iter().map(|f| f.task.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"],
            "canonical order is priority asc, then id"
        );

        // Move the middle card `b` (slot 1 of 0..=2) each way.
        let slot = |up, down, top, bottom, set| {
            prio_slot_in(&col, "b", &prio_args("b", up, down, top, bottom, set))
                .unwrap()
                .unwrap()
        };
        assert_eq!(slot(false, false, true, false, None), 0, "--top → slot 0");
        assert_eq!(slot(true, false, false, false, None), 0, "--up from slot 1 → 0");
        assert_eq!(slot(false, true, false, false, None), 2, "--down from slot 1 → 2");
        assert_eq!(slot(false, false, false, true, None), 2, "--bottom → last slot 2");
        assert_eq!(slot(false, false, false, false, Some(0)), 0, "--set 0 → 0");
        assert_eq!(
            slot(false, false, false, false, Some(9)),
            2,
            "--set past the end clamps to the last slot"
        );

        // The resulting id order those slots produce, so the reorder — not just
        // the arithmetic — is observed.
        assert_eq!(order_after(&col, "b", 0), ["b", "a", "c"], "--top/--up lands b first");
        assert_eq!(order_after(&col, "b", 2), ["a", "c", "b"], "--down/--bottom lands b last");
    }

    /// Point 3 of the fix's contract: two cards that share a priority reorder
    /// deterministically. Both the index-derived listing `prio` reads and the
    /// store's own `list_in_status` break the tie on id (the final key of
    /// `sort_board` / `list_tasks`), so the slot `prio` computes is the slot the
    /// store acts on — the ordering can't diverge and move the wrong card. The ids
    /// are the two `priority: 13` cards from the original report.
    #[test]
    fn prio_slot_in_breaks_a_priority_tie_on_id_matching_the_store() {
        let col_in = Column::from_status_id("qa-review");
        let make = || {
            vec![
                card_in(col_in.clone(), "rt-daemon-lifecycle", 13),
                card_in(col_in.clone(), "rt-app-model", 13),
            ]
        };
        // Canonical order is id-stable regardless of the order the index lists
        // the tied cards in — so an index published in either tie order resolves
        // the same slot the store (which sorts the same way) will act on.
        let mut forward = make();
        sort_column_canonically(&mut forward);
        let mut reversed = make();
        reversed.reverse();
        sort_column_canonically(&mut reversed);
        let ids = |c: &[shelbi_state::IssueFile]| {
            c.iter().map(|f| f.task.id.clone()).collect::<Vec<_>>()
        };
        assert_eq!(ids(&forward), ids(&reversed), "tie order is id-stable, input-independent");
        assert_eq!(
            ids(&forward),
            ["rt-app-model", "rt-daemon-lifecycle"],
            "`rt-app-model` sorts before `rt-daemon-lifecycle` by id"
        );

        // `rt-daemon-lifecycle` sits at slot 1 by the tie; moving it up/top lands
        // it at slot 0, exactly where `issue list` (same ordering) would show it.
        let args = prio_args("rt-daemon-lifecycle", true, false, false, false, None);
        let slot = prio_slot_in(&forward, "rt-daemon-lifecycle", &args).unwrap().unwrap();
        assert_eq!(slot, 0, "--up on the second tied card → slot 0");
        assert_eq!(
            order_after(&forward, "rt-daemon-lifecycle", slot),
            ["rt-daemon-lifecycle", "rt-app-model"],
        );
    }

    /// Point 2 of the fix's contract (terminal branch): `prio` routes a
    /// `done`/`canceled`/archived status through `list_in_status` (the on-demand
    /// closed history the open index omits), and a non-terminal custom status
    /// through the daemon index. The slot arithmetic is shared (`prio_slot_in`)
    /// and covered above, so this pins only the branch selection — the one bit
    /// that differs between a terminal and a live column.
    #[test]
    fn prio_routes_terminal_statuses_through_the_store_listing() {
        assert!(is_terminal_status(&Column::from_status_id("done")), "done is terminal");
        assert!(
            is_terminal_status(&Column::from_status_id("canceled")),
            "canceled is terminal"
        );
        assert!(
            !is_terminal_status(&Column::from_status_id("qa-review")),
            "a custom active status reads the live index, not the closed history"
        );
        assert!(
            !is_terminal_status(&Column::from_status_id("todo")),
            "todo reads the live index"
        );
    }

    /// The missing-card path names the column and workflow it looked in instead
    /// of a bare message, so an operator can see where the lookup landed.
    #[test]
    fn prio_slot_in_reports_a_missing_card_as_none() {
        let col = vec![card_in(Column::from_status_id("qa-review"), "a", 0)];
        let args = prio_args("absent", false, false, true, false, None);
        assert!(
            prio_slot_in(&col, "absent", &args).unwrap().is_none(),
            "a card the listing doesn't contain resolves to None, not a slot"
        );
    }
}
