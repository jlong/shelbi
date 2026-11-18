//! `shelbi tracker <tracker> <subcommand>`: connect to and inspect an external
//! issue tracker, independent of the board itself (`shelbi issue`).
//!
//! Today: `shelbi tracker linear login|status|teams|projects|labels|users`.
//! The listing commands are how a user finds the values for a project's
//! `issue_tracker.linear` filters (team, project, label, assignee); `status`
//! shows what a project's filters resolve to and how much rate-limit budget is
//! left.

use std::io::{BufRead, Write};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use shelbi_core::{IssueTrackerBackend, LinearConnection};
use shelbi_state::linear_client::{
    self, LinearBudget, LinearClient, LinearError, LinearTeam, LINEAR_API_KEY_ENV,
};

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
}

const LINEAR_HELP: &str = "\
Getting started:
  1. Create a personal API key in Linear: Settings > Account > Security & access >
     Personal API keys.
  2. shelbi tracker linear login            (paste the key; it is checked, then
                                             stored in the macOS keychain)
  3. shelbi tracker linear teams            (find your team's key, e.g. ENG)
  4. Point the project at Linear in its config:

       issue_tracker:
         backend: linear
         linear:
           team: ENG          # required: team key, name, or id
           project: Shelbi    # optional: see `shelbi tracker linear projects`
           label: shelbi      # optional: see `shelbi tracker linear labels`
           assignee: me       # optional: me | email | user id

  5. shelbi tracker linear status           (check what the filters resolve to)

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
pub struct TeamListArgs {
    /// The team (key, name, or id). Defaults to the current project's
    /// `issue_tracker.linear.team`.
    #[arg(long)]
    team: Option<String>,
    /// Print JSON instead of a table.
    #[arg(long)]
    json: bool,
}

pub fn run(project_opt: Option<String>, cmd: TrackerCmd) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match cmd {
        TrackerCmd::Linear { cmd } => {
            let stdin = std::io::stdin();
            let mut input = stdin.lock();
            run_linear(project_opt, cmd, &mut input, &mut out)
        }
    }
}

/// The testable body of `shelbi tracker linear ...`.
fn run_linear(
    project_opt: Option<String>,
    cmd: LinearCmd,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    match cmd {
        LinearCmd::Login { key_stdin } => login(key_stdin, input, out),
        LinearCmd::Status { refresh, json } => status(project_opt, refresh, json, out),
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
        run_linear(project.map(str::to_string), cmd, &mut input, &mut out)?;
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
}
