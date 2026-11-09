//! The CLI's issue-tracker prompts. The checks behind them (the GitHub
//! preflight, the config builders, the stranded-board guard) live in the
//! shared creation engine, [`shelbi_orchestrator::project_create::issues`],
//! and are re-exported here.

use anyhow::{Context, Result};
use inquire::{Confirm, Select, Text};
use shelbi_core::IssueTrackerConfig;

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
    FileSystem,
}

impl std::fmt::Display for BackendChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendChoice::Github => f.write_str("GitHub Issues"),
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
    println!("  File system    private markdown files on this machine (no GitHub needed)");

    let options = vec![BackendChoice::Github, BackendChoice::FileSystem];
    // Pre-select GitHub only when we actually detected a GitHub repo to offer;
    // otherwise File system is the honest default.
    let starting_cursor = if suggestion.repo.is_some() { 0 } else { 1 };
    let choice = Select::new("Issue tracker:", options)
        .with_starting_cursor(starting_cursor)
        .prompt()
        .context("issue tracker selection")?;

    match choice {
        BackendChoice::FileSystem => Ok(file_system_config()),
        BackendChoice::Github => resolve_github_interactive(suggestion, probe),
    }
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

