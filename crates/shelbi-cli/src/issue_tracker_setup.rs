//! The CLI's issue-tracker prompts. The checks behind them (the GitHub
//! preflight, the config builders, the stranded-board guard) live in the
//! shared creation engine, [`shelbi_orchestrator::project_create::issues`],
//! and are re-exported here.

use anyhow::{Context, Result};
use inquire::{Confirm, Password, PasswordDisplayMode, Select, Text};
use shelbi_app::linear_setup::{self as ls, Filter, LinearChoice, PickOption};
use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig, LinearConnection};
use shelbi_orchestrator::project_create::linear;

pub use shelbi_app::new_project::GITHUB_DISCLOSURE;
pub use shelbi_orchestrator::project_create::issues::{
    file_system_config, github_config, github_preflight, GhProbe, RealGhProbe,
};

/// The user's suggested GitHub repo for onboarding — the `owner/repo` detected
/// from the origin remote, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GithubSuggestion {
    pub repo: Option<String>,
}

/// One row in the backend picker.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BackendChoice {
    Github,
    Linear,
    FileSystem,
}

impl std::fmt::Display for BackendChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendChoice::Github => f.write_str("GitHub Issues"),
            BackendChoice::Linear => f.write_str("Linear"),
            BackendChoice::FileSystem => f.write_str("File system"),
        }
    }
}

/// The shared interactive issue-tracker chooser. Presents GitHub Issues and
/// File system (GitHub pre-selected and repo pre-filled when the origin is a
/// GitHub repo), runs the GitHub preflight before returning, and on a preflight
/// failure prints the fix and offers to retry or fall back to File system — so
/// it never returns a GitHub config that won't load.
///
/// Returns the chosen, validated [`IssueTrackerConfig`]. A `None` is never
/// returned: a user who declines GitHub lands on File system.
pub fn choose_issue_tracker_interactive<P: GhProbe + ?Sized>(
    suggestion: &GithubSuggestion,
    probe: &mut P,
) -> Result<IssueTrackerConfig> {
    println!();
    println!("Where should this project's issues live?");
    println!(
        "  GitHub Issues  cards are GitHub issues your team can see and file \
         (requires `gh` logged in)"
    );
    println!(
        "  Linear         cards are Linear issues in one of your teams, optionally narrowed by \
         project, label or assignee (requires a Linear API key)"
    );
    println!("  File system    private markdown files on this machine (no GitHub needed)");

    let options = vec![BackendChoice::Github, BackendChoice::Linear, BackendChoice::FileSystem];
    // Pre-select GitHub only when we actually detected a GitHub repo to offer;
    // otherwise File system is the honest default.
    let starting_cursor = if suggestion.repo.is_some() { 0 } else { 2 };
    let choice = Select::new("Issue tracker:", options)
        .with_starting_cursor(starting_cursor)
        .prompt()
        .context("issue tracker selection")?;

    match choice {
        BackendChoice::FileSystem => Ok(file_system_config()),
        BackendChoice::Github => resolve_github_interactive(suggestion, probe),
        BackendChoice::Linear => match choose_linear_interactive(None, LinearConnection::default())? {
            Some(conn) => Ok(linear_config(conn)),
            None => {
                println!("  → Using File system instead.");
                Ok(file_system_config())
            }
        },
    }
}

/// A `linear` board config on `conn`.
pub fn linear_config(conn: LinearConnection) -> IssueTrackerConfig {
    IssueTrackerConfig {
        backend: IssueTrackerBackend::Linear,
        linear: Some(conn),
        ..IssueTrackerConfig::default()
    }
}

/// Run every read `choice` has due, with `project`'s key (`None`: the
/// environment's or the keychain's), as the UIs' hosts do.
fn drive(choice: &mut LinearChoice, project: Option<&str>) {
    while let Some(request) = choice.due() {
        choice.answer(linear::answer(project, request));
    }
}

/// A menu row: `ENG  Engineering`.
#[derive(Clone)]
struct Row(PickOption);

impl std::fmt::Display for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0.detail {
            Some(detail) if *detail != self.0.label => write!(f, "{}  ({detail})", self.0.label),
            _ => f.write_str(&self.0.label),
        }
    }
}

/// Pick one of `rows`, starting on the one `current` names (else the first).
fn pick(prompt: &str, rows: &[PickOption], current: Option<&str>, help: Option<&str>) -> Result<PickOption> {
    let cursor = current
        .and_then(|c| rows.iter().position(|r| r.matches(c)))
        .unwrap_or(0);
    let mut select = Select::new(prompt, rows.iter().cloned().map(Row).collect()).with_starting_cursor(cursor);
    if let Some(help) = help {
        select = select.with_help_message(help);
    }
    Ok(select.prompt().with_context(|| format!("{prompt} selection"))?.0)
}

/// The Linear half of the Issues step at a terminal, over the same
/// [`LinearChoice`] the TUI and the desktop render: the API key (asked
/// for, checked and stored when this machine has none), then the team
/// (required), then the optional project, label and assignee filters, then
/// the preflight. `seed` preselects a connection. `None`: the user gave up
/// (an empty key, or no retry), and nothing was chosen.
pub fn choose_linear_interactive(project: Option<&str>, seed: LinearConnection) -> Result<Option<LinearConnection>> {
    let mut choice = LinearChoice::with_connection(seed);
    drive(&mut choice, project);
    loop {
        if choice.needs_key() {
            match choice.key_state() {
                ls::KeyState::Rejected(why) => println!("  ✗ {why}"),
                _ => println!("  {}", ls::KEY_MISSING),
            }
            let key = Password::new(&format!("{}:", ls::KEY_LABEL))
                .without_confirmation()
                .with_display_mode(PasswordDisplayMode::Hidden)
                .with_help_message("Linear > Settings > Account > Security & access > Personal API keys (empty to cancel)")
                .prompt()
                .context("reading the Linear API key")?;
            if key.trim().is_empty() {
                return Ok(None);
            }
            choice.set_key_input(key);
            choice.submit_key();
            drive(&mut choice, project);
            if let ls::KeyState::Valid { viewer: Some(viewer) } = choice.key_state() {
                println!("  ✓ Logged in to Linear as {viewer}.");
            }
            continue;
        }
        let teams = match choice.teams().loaded() {
            Some(teams) if !teams.is_empty() => teams.clone(),
            Some(_) => {
                println!("  ✗ This Linear API key can't see any teams.");
                return Ok(None);
            }
            None => {
                let why = choice.teams().error().unwrap_or("the teams didn't load").to_string();
                println!("  ✗ {why}");
                if !offer_retry("Try loading the Linear teams again?")? {
                    return Ok(None);
                }
                choice.retry();
                drive(&mut choice, project);
                continue;
            }
        };
        let current = Some(choice.team()).filter(|t| !t.is_empty());
        let team = pick(&format!("Linear {}:", ls::TEAM_LABEL.to_lowercase()), &teams, current, None)?;
        choice.select_team(&team.value);
        drive(&mut choice, project);
        for filter in Filter::ALL {
            let rows = choice.filter_options(filter);
            match rows {
                ls::Loadable::Loaded(rows) => {
                    let current = choice.filter(filter).map(str::to_string);
                    let row = pick(
                        &format!("{} filter:", filter.label()),
                        &rows,
                        current.as_deref(),
                        Some(ls::FILTERS_HELP),
                    )?;
                    choice.select_filter(filter, Some(&row.value));
                }
                other => {
                    let why = other.error().unwrap_or("not loaded").to_string();
                    println!("  ! {} list unavailable ({why}); no {} filter.", filter.label(), filter.key());
                }
            }
        }
        choice.want_preflight();
        drive(&mut choice, project);
        if let Some(summary) = choice.summary() {
            println!("  ✓ Linear {summary} is ready.");
            return Ok(Some(choice.connection()));
        }
        println!("  ✗ {}", choice.error().unwrap_or("the Linear connection didn't pass its check"));
        if !offer_retry("Choose again?")? {
            return Ok(None);
        }
    }
}

/// Ask whether to try again (default yes).
fn offer_retry(prompt: &str) -> Result<bool> {
    Confirm::new(prompt).with_default(true).prompt().context("retry prompt")
}

/// The GitHub branch of [`choose_issue_tracker_interactive`]: confirm/edit the
/// repo, preflight it, and loop on failure (retry or fall back to File system).
fn resolve_github_interactive<P: GhProbe + ?Sized>(
    suggestion: &GithubSuggestion,
    probe: &mut P,
) -> Result<IssueTrackerConfig> {
    let mut default_repo = suggestion.repo.clone().unwrap_or_default();
    loop {
        let repo = Text::new("GitHub repo (owner/repo):")
            .with_default(&default_repo)
            .prompt()
            .context("github repo prompt")?;
        let repo = repo.trim().to_string();
        default_repo = repo.clone();

        let cfg = match github_config(&repo) {
            Ok(cfg) => cfg,
            Err(error) => {
                println!("  ✗ {error}");
                if offer_fallback("Re-enter the repo?")? {
                    continue;
                }
                println!("  → Using File system instead.");
                return Ok(file_system_config());
            }
        };

        match github_preflight(&repo, probe) {
            Ok(()) => {
                println!("  ✓ GitHub repo {repo} is ready.");
                println!("  {GITHUB_DISCLOSURE}");
                return Ok(cfg);
            }
            Err(failure) => {
                println!("  ✗ {}", failure.guidance());
                if offer_fallback("Try a different repo or fix and retry?")? {
                    continue;
                }
                println!("  → Using File system instead.");
                return Ok(file_system_config());
            }
        }
    }
}

/// Ask whether to retry the GitHub path (`true`) or fall back to File system
/// (`false`). Defaults to retry so an accidental Enter doesn't silently abandon
/// the GitHub choice.
fn offer_fallback(prompt: &str) -> Result<bool> {
    Confirm::new(prompt)
        .with_default(true)
        .with_help_message("No falls back to File system")
        .prompt()
        .context("issue tracker fallback prompt")
}

