//! `shelbi tracker <tracker> <subcommand>`: connect to and inspect an external
//! issue tracker, independent of the board itself (`shelbi issue`).
//!
//! Today: `shelbi tracker linear login|connect|status|sync|teams|projects|labels|users`
//! and `shelbi tracker disconnect`. The listing commands are how a user finds
//! the values for a project's `issue_tracker.linear` filters (team, project,
//! label, assignee); `connect` checks a connection and writes it (switching
//! from another tracker only when the old board is empty, or with `--force`);
//! `disconnect` puts the project back on the local file-system board;
//! `status` shows what a project's filters resolve to and how much rate-limit
//! budget is left.

use std::io::{BufRead, Write};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use shelbi_core::{IssueTrackerBackend, LinearConnection};
use shelbi_state::linear_client::{
    self, LinearBudget, LinearClient, LinearError, LinearTeam, LINEAR_API_KEY_ENV,
};

use shelbi_app::tracker_connect::{ConnectTracker, Connected, StatusSync, TrackerChoice};
use shelbi_orchestrator::tracker_connect::apply_tracker_connection;

use super::require_project;

/// The caller name this command writes to `linear-requests.log`.
const CALLER: &str = "cli";

#[derive(Debug, Subcommand)]
pub enum TrackerCmd {
    /// Linear: log in, check a project's connection, and list the teams,
    /// projects, labels and users to use as filters.
    #[command(after_long_help = LINEAR_HELP)]
    Linear {
        #[command(subcommand)]
        cmd: LinearCmd,
    },
    /// Disconnect the project's external tracker (GitHub or Linear): the
    /// project goes back to the local file-system board. Refused while the
    /// tracker's board has issues, unless --force (they stay in the
    /// tracker; `shelbi issue-store migrate --to file_system` moves them).
    Disconnect {
        /// Disconnect even though the tracker's board has issues.
        #[arg(long)]
        force: bool,
    },
}

const LINEAR_HELP: &str = "\
Getting started:
  1. Create a personal API key in Linear: Settings > Account > Security & access >
     Personal API keys.
  2. shelbi tracker linear login            (paste the key; it is checked, then
                                             stored in the macOS keychain)
  3. shelbi tracker linear teams            (find your team's key, e.g. ENG)
  4. shelbi tracker linear connect --team ENG [--linear-project Shelbi]
         [--label shelbi] [--assignee me]
     (checks the connection, then writes it; at a terminal, leave out --team
     to pick from lists). Or edit the project config by hand:

       issue_tracker:
         backend: linear
         linear:
           team: ENG          # required: team key, name, or id
           project: Shelbi    # optional: see `shelbi tracker linear projects`
           label: shelbi      # optional: see `shelbi tracker linear labels`
           assignee: me       # optional: me | email | user id

  5. shelbi tracker linear status           (check what the filters resolve to)
  6. shelbi tracker linear sync --rewrite-workflows
                                            (take the team's statuses, and see how
                                             the workflows map onto them; add --yes
                                             to write both)

The API key is looked up in $LINEAR_API_KEY, then the macOS keychain (service
`shelbi-linear`), then the project's ~/.shelbi/projects/<name>/tokens.yml
(`linear: { token: ... }`, chmod 600). It is never written to the project config.";

#[derive(Debug, Subcommand)]
pub enum LinearCmd {
    /// Store a Linear personal API key for Shelbi to use.
    ///
    /// The key is checked against Linear first (a rejected key is never
    /// stored), then saved in the macOS keychain under service
    /// `shelbi-linear`, account `default`. Without `--key-stdin` you are
    /// prompted for it (input hidden).
    Login {
        /// Read the key from stdin instead of prompting, e.g.
        /// `pbpaste | shelbi tracker linear login --key-stdin`.
        #[arg(long)]
        key_stdin: bool,
    },
    /// Show the Linear connection: where the API key comes from, who it
    /// belongs to, the project's filters and what they resolve to, and the
    /// rate-limit budget left.
    Status {
        /// Re-resolve the filters instead of using the cached ids.
        #[arg(long)]
        refresh: bool,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Sync the project's statuses from its Linear team's workflow states.
    ///
    /// Writes `workflows/statuses.yaml` with one status per state, in Linear's
    /// order, and prints what changed. Ids are assigned once and survive a
    /// rename in Linear; categories you changed are kept. The board refresh
    /// also re-syncs when the team's states change.
    Sync {
        /// Show the changes without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Also map the project's workflows onto the synced statuses: a status
        /// Linear doesn't have becomes the first Linear status of the same
        /// category, keeping its owner, agent and tags. Shows a diff and
        /// writes nothing unless --yes is given.
        #[arg(long)]
        rewrite_workflows: bool,
        /// With --rewrite-workflows: write the statuses and the workflows.
        #[arg(long, requires = "rewrite_workflows", conflicts_with = "dry_run")]
        yes: bool,
    },
    /// Connect the current project to Linear: a team (required) and optional
    /// project, label and assignee filters.
    ///
    /// The connection is checked first (the key works, every filter
    /// resolves, the key can create issues in the team), then written as the
    /// project's only tracker. Switching from GitHub, or from a local board
    /// with issues, is refused unless the old board is empty or --force is
    /// given. At a terminal, leaving out --team picks the team and filters
    /// from lists; with --yes (or no terminal) --team is required.
    Connect(ConnectArgs),
    /// List the teams this API key can see (use one's key as `team:`).
    Teams {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// List a team's projects (use one's name as `project:`).
    Projects(TeamListArgs),
    /// List the labels usable on a team's issues: the team's own and the
    /// workspace's (use one's name as `label:`).
    Labels(TeamListArgs),
    /// List a team's members (use an email, or `me`, as `assignee:`).
    Users(TeamListArgs),
}

#[derive(Debug, Args)]
pub struct ConnectArgs {
    /// The team (key, name, or id; `shelbi tracker linear teams`).
    #[arg(long)]
    team: Option<String>,
    /// Optional: only this Linear project's issues (name or id). Named
    /// `--linear-project` because `--project` picks the Shelbi project.
    #[arg(long, value_name = "PROJECT")]
    linear_project: Option<String>,
    /// Optional: only issues with this label (name or id).
    #[arg(long)]
    label: Option<String>,
    /// Optional: only issues assigned to this person (`me`, an email, or a
    /// user id).
    #[arg(long)]
    assignee: Option<String>,
    /// Switch even though the current board has issues (they stay where
    /// they are, unseen by the project).
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
pub struct TeamListArgs {
    /// The team (key, name, or id). Defaults to the current project's
    /// `issue_tracker.linear.team`.
    #[arg(long)]
    team: Option<String>,
    /// Print JSON instead of a table.
    #[arg(long)]
    json: bool,
}

pub fn run(project_opt: Option<String>, cmd: TrackerCmd, assume_yes: bool) -> Result<()> {
    use std::io::IsTerminal;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let interactive = !assume_yes && std::io::stdin().is_terminal();
    match cmd {
        TrackerCmd::Linear { cmd } => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            run_linear(project_opt, cmd, interactive, &mut input, &mut out)
        }
        TrackerCmd::Disconnect { force } => disconnect(project_opt, force, interactive, &mut out),
    }
}

/// The testable body of `shelbi tracker linear ...`. `interactive`: a person
/// at a terminal can be asked (never with `--yes`).
fn run_linear(
    project_opt: Option<String>,
    cmd: LinearCmd,
    interactive: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    match cmd {
        LinearCmd::Login { key_stdin } => login(key_stdin, input, out),
        LinearCmd::Connect(args) => connect(project_opt, args, interactive, out),
        LinearCmd::Status { refresh, json } => status(project_opt, refresh, json, out),
        LinearCmd::Sync { dry_run, rewrite_workflows, yes } => {
            let project = require_project(project_opt)?;
            sync(&project, dry_run, rewrite_workflows, yes, out)
        }
        LinearCmd::Teams { json } => {
            let project = optional_project(project_opt)?;
            let teams = client(project.as_ref().map(|p| p.name.as_str()))?.teams()?;
            if json {
                return print_json(out, &teams);
            }
            table(
                out,
                &["KEY", "NAME", "ID"],
                teams.iter().map(|t| vec![t.key.clone(), t.name.clone(), t.id.clone()]),
            )
        }
        LinearCmd::Projects(args) => {
            let (client, team) = team_for(project_opt, args.team.as_deref())?;
            let projects = client.team_projects(&team.id)?;
            if args.json {
                return print_json(out, &projects);
            }
            writeln!(out, "Projects in {} ({}):", team.key, team.name)?;
            table(
                out,
                &["NAME", "STATUS", "ID"],
                projects
                    .iter()
                    .map(|p| vec![p.name.clone(), p.state().to_string(), p.id.clone()]),
            )
        }
        LinearCmd::Labels(args) => {
            let (client, team) = team_for(project_opt, args.team.as_deref())?;
            let labels = client.labels(&team.id)?;
            if args.json {
                return print_json(out, &labels);
            }
            writeln!(out, "Labels usable in {} ({}):", team.key, team.name)?;
            table(
                out,
                &["NAME", "KIND", "ID"],
                labels.iter().map(|l| {
                    let kind = if l.is_group { "group (not assignable)" } else { "" };
                    vec![l.name.clone(), kind.to_string(), l.id.clone()]
                }),
            )
        }
        LinearCmd::Users(args) => {
            let (client, team) = team_for(project_opt, args.team.as_deref())?;
            let users = client.team_members(&team.id)?;
            if args.json {
                return print_json(out, &users);
            }
            writeln!(out, "Members of {} ({}):", team.key, team.name)?;
            table(
                out,
                &["NAME", "EMAIL", "ID", ""],
                users.iter().map(|u| {
                    let name = if u.display_name.is_empty() { &u.name } else { &u.display_name };
                    let active = if u.active { "" } else { "inactive" };
                    vec![name.clone(), u.email.clone(), u.id.clone(), active.to_string()]
                }),
            )
        }
    }
}

/// `shelbi tracker linear connect`.
fn connect(project_opt: Option<String>, args: ConnectArgs, interactive: bool, out: &mut dyn Write) -> Result<()> {
    let name = require_project(project_opt)?;
    let conn = match args.team.filter(|t| !t.trim().is_empty()) {
        Some(team) => LinearConnection {
            team,
            project: args.linear_project,
            label: args.label,
            assignee: args.assignee,
        },
        None if interactive => {
            let seed = LinearConnection {
                project: args.linear_project,
                label: args.label,
                assignee: args.assignee,
                ..LinearConnection::default()
            };
            match crate::issue_tracker_setup::choose_linear_interactive(Some(&name), seed)? {
                Some(conn) => conn,
                None => bail!("project `{name}` was not connected to Linear"),
            }
        }
        None => bail!(
            "which team? pass --team <KEY> (`shelbi tracker linear teams` lists them); \
             --yes and a non-terminal stdin never prompt"
        ),
    };
    let req = ConnectTracker { choice: TrackerChoice::Linear(conn), force: args.force };
    let done = apply_tracker_connection(&name, &req).map_err(|e| anyhow!(e.to_string()))?;
    report_connected(out, &name, &done)
}

/// `shelbi tracker disconnect`.
fn disconnect(project_opt: Option<String>, force: bool, interactive: bool, out: &mut dyn Write) -> Result<()> {
    let name = require_project(project_opt)?;
    let project = shelbi_state::load_project(&name).map_err(|e| anyhow!("project `{name}` could not be loaded: {e}"))?;
    let current = project.issue_tracker.backend;
    if current == IssueTrackerBackend::FileSystem {
        writeln!(out, "Project {name} has no external tracker: it already uses the local file-system board.")?;
        return Ok(());
    }
    if interactive {
        let go = inquire::Confirm::new(&format!(
            "Disconnect {name} from {current}? It goes back to the local file-system board; the {current} issues stay there."
        ))
        .with_default(false)
        .prompt()
        .context("disconnect confirmation")?;
        if !go {
            bail!("project `{name}` was not disconnected");
        }
    }
    let req = ConnectTracker { choice: TrackerChoice::FileSystem, force };
    let done = apply_tracker_connection(&name, &req).map_err(|e| anyhow!(e.to_string()))?;
    report_connected(out, &name, &done)
}

fn report_connected(out: &mut dyn Write, name: &str, done: &Connected) -> Result<()> {
    if done.previous == done.backend {
        writeln!(out, "Updated {name}: {}.", done.summary)?;
    } else {
        writeln!(out, "Connected {name} to {} (was {}).", done.summary, done.previous)?;
    }
    writeln!(out, "  config:   {}", done.file)?;
    match &done.statuses {
        StatusSync::NotNeeded => {}
        StatusSync::Synced(what) => writeln!(out, "  statuses: {what}")?,
        StatusSync::Failed(why) => writeln!(out, "  statuses: not synced: {why}")?,
    }
    Ok(())
}

fn login(key_stdin: bool, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<()> {
    let raw = if key_stdin {
        let mut raw = String::new();
        input.read_to_string(&mut raw).context("reading the key from stdin")?;
        raw
    } else {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            bail!("stdin is not a terminal; pipe the key in with `shelbi tracker linear login --key-stdin`");
        }
        inquire::Password::new("Linear personal API key:")
            .without_confirmation()
            .with_display_mode(inquire::PasswordDisplayMode::Hidden)
            .with_help_message(
                "Linear > Settings > Account > Security & access > Personal API keys",
            )
            .prompt()
            .context("reading the key")?
    };
    let login = linear_client::login_with_key(&raw).map_err(linear_err)?;
    writeln!(out, "Logged in to Linear as {}.", login.viewer.label())?;
    writeln!(out, "The key is stored in {}.", login.stored_in)?;
    if std::env::var_os(LINEAR_API_KEY_ENV).is_some() {
        writeln!(
            out,
            "Note: ${LINEAR_API_KEY_ENV} is set in this shell and takes precedence over the stored key."
        )?;
    }
    Ok(())
}

/// The current project, when one is named or the cwd is inside one. An
/// explicitly named project that doesn't load is an error; no project at all
/// is fine (the listing commands work before a project is set up).
fn optional_project(project_opt: Option<String>) -> Result<Option<shelbi_core::Project>> {
    let explicit = project_opt.is_some();
    let Ok(name) = require_project(project_opt) else {
        return Ok(None);
    };
    match shelbi_state::load_project(&name) {
        Ok(p) => Ok(Some(p)),
        Err(e) if explicit => Err(anyhow!("project `{name}` could not be loaded: {e}")),
        Err(_) => Ok(None),
    }
}

fn client(project: Option<&str>) -> Result<LinearClient> {
    Ok(LinearClient::for_project(project).map_err(linear_err)?.caller(CALLER))
}

/// Keep the user-facing message as-is (no anyhow "caused by" noise).
fn linear_err(e: LinearError) -> anyhow::Error {
    anyhow!(e.to_string())
}

/// The client and the team for a per-team listing: `--team`, else the
/// project's configured Linear team.
fn team_for(project_opt: Option<String>, team: Option<&str>) -> Result<(LinearClient, LinearTeam)> {
    let project = optional_project(project_opt)?;
    let configured = project
        .as_ref()
        .and_then(|p| p.issue_tracker.linear.as_ref())
        .and_then(|l| l.team_ref().map(str::to_string));
    let team_ref = match (team, configured) {
        (Some(t), _) => t.to_string(),
        (None, Some(t)) => t,
        (None, None) => bail!(
            "which team? pass --team <KEY> (see `shelbi tracker linear teams`), or run inside a \
             project whose issue_tracker.linear.team is set"
        ),
    };
    let client = client(project.as_ref().map(|p| p.name.as_str()))?;
    let team = linear_client::resolve_team(&client, &team_ref).map_err(linear_err)?;
    Ok((client, team))
}

fn status(project_opt: Option<String>, refresh: bool, as_json: bool, out: &mut dyn Write) -> Result<()> {
    let project = optional_project(project_opt)?;
    let name = project.as_ref().map(|p| p.name.clone());
    let backend = project.as_ref().map(|p| p.issue_tracker.backend);
    let conn: Option<LinearConnection> = project.as_ref().and_then(|p| p.issue_tracker.linear.clone());

    let client = LinearClient::for_project(name.as_deref()).map(|c| c.caller(CALLER));
    let viewer = client.as_ref().ok().map(|c| c.viewer());
    let filters = match (&client, &conn, &name) {
        (Ok(c), Some(conn), Some(name)) if conn.team_ref().is_some() => {
            if refresh {
                linear_client::invalidate_filter_cache(name)?;
            }
            Some(linear_client::resolve_filters_cached(c, name, conn))
        }
        _ => None,
    };
    let budget = client.as_ref().ok().map(|c| c.budget());

    if as_json {
        let err_str = |e: &LinearError| e.to_string();
        let value = json!({
            "project": name,
            "backend": backend.map(|b| b.as_str()),
            "key": match &client {
                Ok(c) => json!({ "source": c.key_source().to_string() }),
                Err(e) => json!({ "error": err_str(e) }),
            },
            "viewer": match &viewer {
                Some(Ok(v)) => json!(v),
                Some(Err(e)) => json!({ "error": err_str(e) }),
                None => json!(null),
            },
            "filters": conn,
            "resolved": match &filters {
                Some(Ok(r)) => json!(r),
                Some(Err(e)) => json!({ "error": err_str(e) }),
                None => json!(null),
            },
            "budget": budget,
        });
        return print_json(out, &value);
    }

    writeln!(out, "Linear")?;
    match (&name, backend) {
        (Some(n), Some(IssueTrackerBackend::Linear)) => writeln!(out, "  project:   {n} (backend: linear)")?,
        (Some(n), Some(b)) => writeln!(
            out,
            "  project:   {n} (backend: {b}; Linear is not this project's tracker)"
        )?,
        _ => writeln!(out, "  project:   none (pass --project to check one)")?,
    }
    match &client {
        Ok(c) => writeln!(out, "  api key:   from {}", c.key_source())?,
        Err(e) => {
            writeln!(out, "  api key:   missing: {e}")?;
            return Ok(());
        }
    }
    match &viewer {
        Some(Ok(v)) => writeln!(out, "  viewer:    {} [{}]", v.label(), v.id)?,
        Some(Err(e)) => writeln!(out, "  viewer:    error: {e}")?,
        None => {}
    }
    match (&conn, &filters) {
        (Some(_), Some(Ok(r))) => {
            writeln!(out, "  team:      {} ({}) [{}]", r.team_key, r.team_name, r.team_id)?;
            let line = |name: &Option<String>, id: &Option<String>| match (name, id) {
                (Some(n), Some(i)) => format!("{n} [{i}]"),
                _ => "any".to_string(),
            };
            writeln!(out, "  project:   {}", line(&r.project_name, &r.project_id))?;
            writeln!(out, "  label:     {}", line(&r.label_name, &r.label_id))?;
            writeln!(out, "  assignee:  {}", line(&r.assignee_name, &r.assignee_id))?;
        }
        (Some(_), Some(Err(e))) => writeln!(out, "  filters:   error: {e}")?,
        (Some(_), None) => writeln!(out, "  filters:   issue_tracker.linear.team is not set")?,
        (None, _) if name.is_some() => {
            writeln!(out, "  filters:   none (no issue_tracker.linear block in the project config)")?
        }
        (None, _) => {}
    }
    if let Some(b) = &budget {
        writeln!(out, "  budget:    {}", budget_line(b))?;
    }
    Ok(())
}

/// `shelbi tracker linear sync`: the catalog diff, applied unless dry; with
/// `--rewrite-workflows`, the workflow diffs too, written only with `--yes`.
fn sync(project: &str, dry_run: bool, rewrite: bool, yes: bool, out: &mut dyn Write) -> Result<()> {
    use shelbi_state::linear_statuses;
    // A rewrite without --yes is a full dry run: writing the catalog alone
    // would leave the workflows it's about to show broken.
    let write_catalog = !dry_run && (!rewrite || yes);
    let synced = linear_statuses::sync(project, !write_catalog).map_err(|e| anyhow!("{e}"))?;

    if synced.changes.is_empty() {
        writeln!(out, "Statuses are up to date with Linear ({} statuses).", synced.after.statuses.len())?;
    } else {
        let verb = if synced.written { "Synced" } else { "Would sync" };
        writeln!(out, "{verb} statuses from Linear:")?;
        for change in &synced.changes {
            writeln!(out, "  {change}")?;
        }
    }

    if rewrite {
        let rewrites = linear_statuses::plan_workflow_rewrites(project, &synced.after, &synced.before)
            .map_err(|e| anyhow!("{e}"))?;
        if rewrites.is_empty() {
            writeln!(out, "\nWorkflows already use only these statuses.")?;
        }
        for r in &rewrites {
            print_rewrite(out, r)?;
        }
        if yes {
            let n = linear_statuses::apply_workflow_rewrites(&rewrites).map_err(|e| anyhow!("{e}"))?;
            writeln!(out, "\nWrote {n} workflow file{}.", if n == 1 { "" } else { "s" })?;
        } else {
            writeln!(
                out,
                "\nDry run: nothing was written. Re-run with --rewrite-workflows --yes to write the \
                 statuses and the workflows."
            )?;
        }
        return Ok(());
    }

    if !synced.broken.is_empty() {
        writeln!(out, "\nThese workflows name statuses the Linear team doesn't have:")?;
        for b in &synced.broken {
            writeln!(out, "  {}: {}", b.workflow, b.status)?;
        }
        writeln!(
            out,
            "Run `shelbi tracker linear sync --rewrite-workflows` to map them by category."
        )?;
    }
    if dry_run && !synced.changes.is_empty() {
        writeln!(out, "\nDry run: nothing was written.")?;
    }
    Ok(())
}

fn print_rewrite(out: &mut dyn Write, r: &shelbi_state::linear_statuses::WorkflowRewrite) -> Result<()> {
    use shelbi_state::linear_statuses::{line_diff, DiffLine};
    writeln!(out, "\nWorkflow `{}` ({}):", r.workflow, r.path.display())?;
    for (from, to) in &r.remapped {
        writeln!(out, "  {from} -> {to}")?;
    }
    for id in &r.merged {
        writeln!(out, "  {id}: folded into a status the workflow already lists")?;
    }
    for id in &r.unmapped {
        writeln!(
            out,
            "  {id}: no Linear status has its category; give one that category (Settings > \
             Workflows) or edit the workflow"
        )?;
    }
    if r.changes() {
        writeln!(out, "  --- {}", r.path.display())?;
        writeln!(out, "  +++ {} (rewritten)", r.path.display())?;
        for line in line_diff(&r.before, &r.after) {
            match line {
                DiffLine::Same(l) => writeln!(out, "    {l}")?,
                DiffLine::Removed(l) => writeln!(out, "  - {l}")?,
                DiffLine::Added(l) => writeln!(out, "  + {l}")?,
            }
        }
    }
    if let Some(e) = &r.error {
        writeln!(out, "  still invalid after the rewrite: {e}")?;
    }
    Ok(())
}

fn budget_line(b: &LinearBudget) -> String {
    let now = chrono::Utc::now();
    let bucket = |label: &str, s: &linear_client::BucketSnapshot| match (s.remaining, s.limit) {
        (Some(r), Some(l)) => Some(format!("{r}/{l} {label}")),
        (Some(r), None) => Some(format!("{r} {label}")),
        _ => None,
    };
    let mut parts: Vec<String> = [bucket("requests", &b.requests), bucket("complexity", &b.complexity)]
        .into_iter()
        .flatten()
        .collect();
    if parts.is_empty() {
        parts.push("no requests recorded yet".into());
    } else if let Some(reset) = b.requests.reset_at.and_then(|t| chrono::DateTime::from_timestamp(t, 0)) {
        parts.push(format!("resets {}", reset.with_timezone(&chrono::Local).format("%H:%M")));
    }
    if let Some(until) = b.parked_at(now) {
        parts.push(format!(
            "PAUSED until {} (rate limited)",
            until.with_timezone(&chrono::Local).format("%H:%M:%S")
        ));
    }
    parts.join(", ")
}

fn print_json<T: serde::Serialize + ?Sized>(out: &mut dyn Write, value: &T) -> Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

/// Left-aligned columns, two spaces apart; trailing blanks trimmed.
fn table(out: &mut dyn Write, header: &[&str], rows: impl Iterator<Item = Vec<String>>) -> Result<()> {
    let rows: Vec<Vec<String>> = rows.collect();
    if rows.is_empty() {
        writeln!(out, "  (none)")?;
        return Ok(());
    }
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let render = |cells: Vec<String>| {
        let mut line = String::new();
        for (i, cell) in cells.iter().enumerate() {
            line.push_str(cell);
            if i + 1 < cells.len() {
                line.push_str(&" ".repeat(widths[i] - cell.chars().count() + 2));
            }
        }
        line.trim_end().to_string()
    };
    writeln!(out, "{}", render(header.iter().map(|h| h.to_string()).collect()))?;
    for row in rows {
        writeln!(out, "{}", render(row))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK;
    use shelbi_state::linear_client::{test_keychain_value, FakeLinear};

    struct Home {
        path: std::path::PathBuf,
    }

    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn fresh_home() -> Home {
        let path = std::env::temp_dir().join(format!(
            "shelbi-cli-linear-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(path.join("projects")).unwrap();
        std::env::set_var("SHELBI_HOME", &path);
        Home { path }
    }

    fn register(home: &Home, name: &str, tracker: &str) {
        std::fs::write(
            home.path.join("projects").join(format!("{name}.yaml")),
            format!(
                "name: {name}\nrepo: /tmp/{name}\ndefault_branch: main\n\
orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n\
machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/{name}\nworkspaces: []\n{tracker}"
            ),
        )
        .unwrap();
    }

    const LINEAR_TRACKER: &str = "issue_tracker:\n  backend: linear\n  linear:\n    team: eng\n    \
project: Shelbi\n    label: bug\n    assignee: me\n";

    fn run(project: Option<&str>, cmd: LinearCmd, stdin: &str) -> Result<String> {
        let mut input = std::io::Cursor::new(stdin.as_bytes().to_vec());
        let mut out = Vec::new();
        run_linear(project.map(str::to_string), cmd, false, &mut input, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn login_validates_then_stores_the_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _home = fresh_home();
        let fake = FakeLinear::new();
        let guard = fake.install();
        guard.set_keychain(None);

        let out = run(None, LinearCmd::Login { key_stdin: true }, "lin_api_pasted\n").unwrap();
        assert!(out.contains("Logged in to Linear as ada <ada@example.com>."), "{out}");
        assert!(out.contains("shelbi-linear"), "{out}");
        assert_eq!(test_keychain_value().as_deref(), Some("lin_api_pasted"));
        assert_eq!(fake.calls_for("Viewer").len(), 1);

        let err = run(None, LinearCmd::Login { key_stdin: true }, "not a key").unwrap_err();
        assert!(err.to_string().contains("doesn't look like a Linear API key"), "{err}");
    }

    #[test]
    fn teams_lists_as_a_table_and_as_json() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _home = fresh_home();
        let fake = FakeLinear::new();
        let _guard = fake.install();

        let out = run(None, LinearCmd::Teams { json: false }, "").unwrap();
        assert_eq!(out, "KEY  NAME         ID\nDES  Design       team-des\nENG  Engineering  team-eng\n");
        let out = run(None, LinearCmd::Teams { json: true }, "").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v[1]["key"], "ENG");
    }

    #[test]
    fn per_team_lists_take_the_team_from_the_flag_or_the_project() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        register(&home, "lin", LINEAR_TRACKER);
        let fake = FakeLinear::new();
        let _guard = fake.install();

        let out = run(Some("lin"), LinearCmd::Projects(TeamListArgs { team: None, json: false }), "").unwrap();
        assert!(out.starts_with("Projects in ENG (Engineering):\n"), "{out}");
        assert!(out.contains("Shelbi  In Progress  proj-shelbi"), "{out}");

        let out = run(None, LinearCmd::Labels(TeamListArgs { team: Some("DES".into()), json: false }), "").unwrap();
        assert!(out.contains("Feature      group (not assignable)  label-feature"), "{out}");
        assert_eq!(fake.calls_for("Labels").last().unwrap().var("teamId"), "team-des");

        let out = run(None, LinearCmd::Users(TeamListArgs { team: Some("eng".into()), json: true }), "").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v.as_array().unwrap().iter().any(|u| u["email"] == "grace@example.com"));

        let err = run(None, LinearCmd::Users(TeamListArgs { team: None, json: false }), "").unwrap_err();
        assert!(err.to_string().contains("pass --team"), "{err}");
        let err = run(None, LinearCmd::Users(TeamListArgs { team: Some("OPS".into()), json: false }), "")
            .unwrap_err();
        assert!(err.to_string().contains("no Linear team `OPS`; available:"), "{err}");
    }

    #[test]
    fn status_shows_key_viewer_resolved_filters_and_budget() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        register(&home, "lin", LINEAR_TRACKER);
        let fake = FakeLinear::new();
        fake.on_reply(
            "Viewer",
            shelbi_state::linear_client::FakeReply::Response(
                shelbi_state::linear_client::LinearResponse::ok(
                    shelbi_state::linear_client::fixture("Viewer").unwrap(),
                )
                .with_header("X-RateLimit-Requests-Limit", "1500")
                .with_header("X-RateLimit-Requests-Remaining", "1499"),
            ),
        );
        let _guard = fake.install();

        let out = run(Some("lin"), LinearCmd::Status { refresh: false, json: false }, "").unwrap();
        for want in [
            "project:   lin (backend: linear)",
            "api key:   from the macOS keychain",
            "viewer:    ada <ada@example.com> [user-ada]",
            "team:      ENG (Engineering) [team-eng]",
            "project:   Shelbi [proj-shelbi]",
            "label:     Bug [label-bug]",
            "assignee:  ada <ada@example.com> [user-ada]",
            "budget:    1499/1500 requests",
        ] {
            assert!(out.contains(want), "missing {want:?} in:\n{out}");
        }
        assert!(home.path.join("projects/lin/linear.json").exists(), "filters cached");

        let out = run(Some("lin"), LinearCmd::Status { refresh: true, json: true }, "").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["backend"], "linear");
        assert_eq!(v["resolved"]["label_id"], "label-bug");
        assert_eq!(v["filters"]["assignee"], "me");
        assert_eq!(v["budget"]["requests"]["remaining"], 1499);
    }

    #[test]
    fn status_without_a_key_names_the_fix_and_makes_no_request() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        register(&home, "gh", "issue_tracker:\n  backend: github\n  github:\n    repo: o/r\n");
        let fake = FakeLinear::new();
        let guard = fake.install();
        guard.set_keychain(None);

        let out = run(Some("gh"), LinearCmd::Status { refresh: false, json: false }, "").unwrap();
        assert!(out.contains("backend: github; Linear is not this project's tracker"), "{out}");
        assert!(out.contains("api key:   missing:") && out.contains("shelbi tracker linear login"), "{out}");
        assert!(fake.calls().is_empty());

        let err = run(None, LinearCmd::Teams { json: false }, "").unwrap_err();
        assert!(err.to_string().contains("shelbi tracker linear login"), "{err}");
    }

    fn sync_cmd(dry_run: bool, rewrite_workflows: bool, yes: bool) -> LinearCmd {
        LinearCmd::Sync { dry_run, rewrite_workflows, yes }
    }

    #[test]
    fn sync_prints_the_catalog_diff_and_flags_broken_workflows() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        register(&home, "lin", "issue_tracker:\n  backend: linear\n  linear:\n    team: ENG\n");
        shelbi_state::scaffold_project_statuses("lin").unwrap();
        shelbi_state::scaffold_project_workflow("lin").unwrap();
        let fake = FakeLinear::new();
        let _guard = fake.install();
        let statuses = shelbi_state::statuses_path("lin").unwrap();
        let original = std::fs::read_to_string(&statuses).unwrap();

        let out = run(Some("lin"), sync_cmd(true, false, false), "").unwrap();
        assert!(out.contains("Would sync statuses from Linear:"), "{out}");
        assert!(out.contains("~ statuses are now managed by Linear"), "{out}");
        assert!(out.contains("+ triage (Triage, backlog)"), "{out}");
        assert!(out.contains("+ in-review (In Review, handoff)"), "{out}");
        assert!(out.contains("- review (Review)"), "{out}");
        assert!(out.contains("task: review"), "{out}");
        assert!(out.contains("Dry run: nothing was written."), "{out}");
        assert_eq!(std::fs::read_to_string(&statuses).unwrap(), original);

        let out = run(Some("lin"), sync_cmd(false, false, false), "").unwrap();
        assert!(out.starts_with("Synced statuses from Linear:"), "{out}");
        assert!(out.contains("--rewrite-workflows"), "{out}");
        assert!(std::fs::read_to_string(&statuses).unwrap().contains("managed_by: linear"));

        let out = run(Some("lin"), sync_cmd(false, false, false), "").unwrap();
        assert!(out.starts_with("Statuses are up to date with Linear (7 statuses)."), "{out}");
    }

    #[test]
    fn sync_rewrite_workflows_is_a_dry_run_until_yes() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        register(&home, "lin", "issue_tracker:\n  backend: linear\n  linear:\n    team: ENG\n");
        shelbi_state::scaffold_project_statuses("lin").unwrap();
        shelbi_state::scaffold_project_workflow("lin").unwrap();
        let fake = FakeLinear::new();
        let _guard = fake.install();
        let statuses = shelbi_state::statuses_path("lin").unwrap();
        let task = shelbi_state::workflow_path("lin", "task").unwrap();
        let (statuses_before, task_before) =
            (std::fs::read_to_string(&statuses).unwrap(), std::fs::read_to_string(&task).unwrap());

        let out = run(Some("lin"), sync_cmd(false, true, false), "").unwrap();
        assert!(out.contains("Would sync statuses from Linear:"), "{out}");
        assert!(out.contains("Workflow `task`"), "{out}");
        assert!(out.contains("  review -> in-review"), "{out}");
        assert!(out.contains("  - - id: review"), "{out}");
        assert!(out.contains("  + - id: in-review"), "{out}");
        assert!(!out.contains("still invalid"), "{out}");
        assert!(out.contains("Re-run with --rewrite-workflows --yes"), "{out}");
        assert_eq!(std::fs::read_to_string(&statuses).unwrap(), statuses_before);
        assert_eq!(std::fs::read_to_string(&task).unwrap(), task_before);

        let out = run(Some("lin"), sync_cmd(false, true, true), "").unwrap();
        assert!(out.contains("Synced statuses from Linear:"), "{out}");
        assert!(out.contains("Wrote 1 workflow file."), "{out}");
        shelbi_state::load_workflow("lin", "task").unwrap();
        let out = run(Some("lin"), sync_cmd(false, true, true), "").unwrap();
        assert!(out.contains("Workflows already use only these statuses."), "{out}");
    }

    fn connect_args(team: &str) -> ConnectArgs {
        ConnectArgs {
            team: Some(team.into()),
            linear_project: None,
            label: None,
            assignee: None,
            force: false,
        }
    }

    fn tracker_yaml(home: &Home, name: &str) -> String {
        std::fs::read_to_string(home.path.join("projects").join(format!("{name}.yaml"))).unwrap()
    }

    #[test]
    fn connect_checks_then_writes_one_linear_block_and_disconnect_reverts() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        register(&home, "app", "issue_tracker:\n  backend: github\n  github:\n    repo: o/r\n");
        let fake = FakeLinear::new();
        let _guard = fake.install();

        // --force switches without counting GitHub's issues (no `gh` call).
        let args = ConnectArgs { label: Some("bug".into()), assignee: Some("me".into()), force: true, ..connect_args("eng") };
        let out = run(Some("app"), LinearCmd::Connect(args), "").unwrap();
        assert!(
            out.starts_with("Connected app to Linear ENG (Engineering) · label Bug · assignee ada <ada@example.com> (was github)."),
            "{out}"
        );
        assert!(out.contains("statuses: 7 statuses from the Linear team"), "{out}");
        let p = shelbi_state::load_project("app").unwrap();
        assert_eq!(p.issue_tracker.connection_blocks(), vec!["linear"]);
        let l = p.issue_tracker.linear.unwrap();
        assert_eq!((l.team.as_str(), l.label.as_deref(), l.assignee.as_deref()), ("eng", Some("bug"), Some("me")));
        assert!(home.path.join("projects/app/linear.json").exists(), "filters cached");

        // An unknown filter is refused with the fix, nothing written.
        let args = ConnectArgs { label: Some("nope".into()), ..connect_args("eng") };
        let err = run(Some("app"), LinearCmd::Connect(args), "").unwrap_err().to_string();
        assert!(err.contains("No Linear label `nope`; available:") && err.contains("shelbi tracker linear labels"), "{err}");
        assert_eq!(shelbi_state::load_project("app").unwrap().issue_tracker.linear.unwrap().label.as_deref(), Some("bug"));

        // With --yes (not interactive) a team is required.
        let args = ConnectArgs { team: None, ..connect_args("") };
        let err = run(Some("app"), LinearCmd::Connect(args), "").unwrap_err().to_string();
        assert!(err.contains("pass --team"), "{err}");

        // Disconnect: the Linear board has issues, so it needs --force too.
        let mut out = Vec::new();
        let err = disconnect(Some("app".into()), false, false, &mut out).unwrap_err().to_string();
        assert!(err.contains("issues on Linear (eng)"), "{err}");
        assert!(err.contains("shelbi issue-store migrate --to file_system"), "{err}");
        disconnect(Some("app".into()), true, false, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("Connected app to the local file system (was linear)."), "{out}");
        assert!(!tracker_yaml(&home, "app").contains("issue_tracker"));
        let doc = std::fs::read_to_string(home.path.join("projects/app/linear.json")).unwrap_or_default();
        assert!(!doc.contains("\"resolved\""), "the filter cache went with it:\n{doc}");
    }

    #[test]
    fn connecting_a_local_board_with_issues_is_refused_with_migrate_guidance() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        register(&home, "local", "");
        let tasks = home.path.join("projects/local/tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(tasks.join("t-1.md"), "---\nid: t-1\n---\n").unwrap();
        let fake = FakeLinear::new();
        let _guard = fake.install();

        let err = run(Some("local"), LinearCmd::Connect(connect_args("ENG")), "").unwrap_err().to_string();
        assert!(err.contains("has 1 issue on the local file system"), "{err}");
        assert!(err.contains("shelbi issue-store migrate --to linear --dry-run"), "{err}");
        assert!(fake.calls().is_empty(), "refused before asking Linear anything");

        let out = run(Some("local"), LinearCmd::Connect(ConnectArgs { force: true, ..connect_args("ENG") }), "").unwrap();
        assert!(out.contains("Connected local to Linear ENG (Engineering) (was file_system)."), "{out}");

        let mut out = Vec::new();
        disconnect(Some("local".into()), true, false, &mut out).unwrap();
        disconnect(Some("local".into()), false, false, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("already uses the local file-system board"), "{out}");
    }
}
