//! Onboarding helpers for choosing a project's issue-tracker backend.
//!
//! When a project is set up, Shelbi asks where its board lives: on the local
//! file system (the markdown-on-disk default) or as GitHub issues. This module
//! holds the backend-agnostic pieces that both onboarding systems share — the
//! card-based `shelbi wizard` / `shelbi init -y` flow in [`crate::wizard`] and
//! the mode-aware interactive `shelbi init` / palette flow in
//! [`crate::commands::init`]:
//!
//! * [`detect_github_repo`] — derive `owner/repo` from an origin remote so the
//!   GitHub option can be pre-filled.
//! * [`github_preflight`] — verify `gh` is installed, authenticated, and the
//!   account can read issues and write labels on the repo, so Shelbi never
//!   writes a GitHub config that won't load. The check is split into a tiny
//!   [`GhProbe`] seam (the only part that shells out) and a pure classifier, so
//!   every failure path is unit-tested.
//! * [`github_config`] / [`file_system_config`] — build the
//!   [`IssueTrackerConfig`] block that gets written, validated the same way the
//!   loader validates it.
//! * [`choose_issue_tracker_interactive`] — the shared interactive selection
//!   (select → edit repo → preflight → retry/fall-back) used by both flows.
//! * [`file_system_board_has_issues`] — detect a non-empty local board so setup
//!   can point at `issue-store migrate` rather than stranding existing cards.

use std::process::Command;

use anyhow::{bail, Context, Result};
use inquire::{Confirm, Select, Text};
use shelbi_core::{GithubConnection, IssueTrackerBackend, IssueTrackerConfig};

/// Build a validated `file_system` tracker config — the shipped default. This
/// is the untouched [`IssueTrackerConfig::default`], so it round-trips to an
/// elided (absent) `issue_tracker:` block.
pub fn file_system_config() -> IssueTrackerConfig {
    IssueTrackerConfig::default()
}

/// Build a `github` tracker config for `owner/repo`, validated through
/// [`IssueTrackerConfig::validate`] so onboarding can never emit a block the
/// loader would reject.
pub fn github_config(repo: &str) -> Result<IssueTrackerConfig> {
    let cfg = IssueTrackerConfig {
        backend: IssueTrackerBackend::Github,
        github: Some(GithubConnection {
            repo: repo.trim().to_string(),
        }),
        ..IssueTrackerConfig::default()
    };
    cfg.validate().map_err(|error| anyhow::anyhow!(error))?;
    Ok(cfg)
}

/// Derive `owner/repo` from an origin remote URL when it points at
/// github.com, else `None`. Handles the SSH (`git@github.com:owner/repo.git`),
/// HTTPS (`https://github.com/owner/repo.git`), and `ssh://` forms, strips any
/// embedded credentials, and trims a trailing `.git`. A non-GitHub host (a
/// GitLab/Bitbucket/self-hosted remote) returns `None` so the caller offers
/// File system rather than an `owner/repo` the GitHub backend can't use.
pub fn detect_github_repo(remote_url: Option<&str>) -> Option<String> {
    let remote = remote_url?.trim();
    if remote.is_empty() {
        return None;
    }

    // Normalize every supported form down to `host/owner/repo...` and then
    // require the host to be github.com before taking the first two segments.
    let host_and_path = if let Some(rest) = remote.strip_prefix("git@") {
        // scp-like: git@host:owner/repo(.git)
        rest.split_once(':')
            .map(|(host, path)| format!("{host}/{path}"))?
    } else if let Some((_scheme, rest)) = remote.split_once("://") {
        // scheme://[user[:pw]@]host/owner/repo(.git)
        let rest = rest.rsplit_once('@').map(|(_, after)| after).unwrap_or(rest);
        // Drop any query/fragment where tokens hide.
        rest.split(['?', '#']).next().unwrap_or(rest).to_string()
    } else {
        return None;
    };

    let mut segments = host_and_path
        .trim_end_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty());
    let host = segments.next()?;
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let owner = segments.next()?;
    let repo = segments.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// The raw outcome of one `gh` invocation. The [`GhProbe`] seam returns this so
/// the classification in [`github_preflight`] stays a pure, testable function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhCommandResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The one IO seam the GitHub preflight needs. Real setup uses [`RealGhProbe`];
/// tests inject canned results to exercise every failure branch of
/// [`github_preflight`] without `gh` on PATH.
pub trait GhProbe {
    /// `gh --version` — `None` when `gh` is not installed / not on PATH.
    fn gh_version(&mut self) -> Option<String>;
    /// `gh api repos/<owner>/<repo>` — the repo-metadata read that proves auth,
    /// existence, issues-enabled, and push access all at once.
    fn gh_api_repo(&mut self, repo: &str) -> GhCommandResult;
}

/// Why a GitHub preflight failed, each variant carrying enough to print the
/// exact fix. Ordered as the preflight checks them: install → auth → repo
/// existence/access → issues-enabled → push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubPreflightError {
    /// `gh` is not installed / not on PATH.
    GhNotInstalled,
    /// `gh` is installed but not logged in (no usable token).
    NotAuthenticated,
    /// The repo doesn't exist or the account can't see it.
    RepoNotFound { repo: String },
    /// The repo exists but has its Issues tab disabled.
    IssuesDisabled { repo: String },
    /// The account can read the repo but lacks push access (so it can't create
    /// the `shelbi:*` labels the board needs).
    InsufficientPermissions { repo: String },
    /// `gh` ran but failed in a way we couldn't classify — surface the detail.
    ProbeFailed { detail: String },
}

impl GithubPreflightError {
    /// A short, user-facing fix for this failure.
    pub fn guidance(&self) -> String {
        match self {
            GithubPreflightError::GhNotInstalled => {
                "GitHub CLI (gh) was not found on PATH. Install it \
                 (https://cli.github.com), run `gh auth login`, then try again."
                    .to_string()
            }
            GithubPreflightError::NotAuthenticated => {
                "GitHub CLI (gh) is not authenticated. Run `gh auth login`, then try again."
                    .to_string()
            }
            GithubPreflightError::RepoNotFound { repo } => format!(
                "Repository `{repo}` was not found, or your GitHub account can't see it. \
                 Check the owner/repo spelling and that you have access."
            ),
            GithubPreflightError::IssuesDisabled { repo } => format!(
                "Repository `{repo}` has Issues disabled. Enable Issues in the repo's \
                 Settings (Features → Issues), then try again."
            ),
            GithubPreflightError::InsufficientPermissions { repo } => format!(
                "Your GitHub account can read `{repo}` but lacks push access, so Shelbi \
                 can't create the `shelbi:*` labels the board needs. Ask for write access, \
                 or choose File system."
            ),
            GithubPreflightError::ProbeFailed { detail } => {
                format!("Could not verify the GitHub repo: {detail}")
            }
        }
    }
}

/// Verify a GitHub board can actually be used for `repo` before any config is
/// written: `gh` installed, authenticated, the repo visible with Issues
/// enabled, and push access for label creation. Pure given a [`GhProbe`].
pub fn github_preflight<P: GhProbe + ?Sized>(
    repo: &str,
    probe: &mut P,
) -> std::result::Result<(), GithubPreflightError> {
    if probe.gh_version().is_none() {
        return Err(GithubPreflightError::GhNotInstalled);
    }

    let result = probe.gh_api_repo(repo);
    if !result.success {
        return Err(classify_api_failure(repo, &result.stderr));
    }

    let value: serde_json::Value = serde_json::from_str(&result.stdout).map_err(|error| {
        GithubPreflightError::ProbeFailed {
            detail: format!("gh returned unparseable repo metadata: {error}"),
        }
    })?;

    // Push access gates label creation; read-only is not enough.
    let push = value
        .get("permissions")
        .and_then(|permissions| permissions.get("push"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !push {
        return Err(GithubPreflightError::InsufficientPermissions {
            repo: repo.to_string(),
        });
    }

    // A repo with the Issues tab disabled can't host a board.
    let has_issues = value
        .get("has_issues")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !has_issues {
        return Err(GithubPreflightError::IssuesDisabled {
            repo: repo.to_string(),
        });
    }

    Ok(())
}

/// Classify a failed `gh api repos/...` call from its stderr. Auth failures and
/// a missing/invisible repo get their own actionable variant; anything else is
/// surfaced verbatim rather than guessed at.
fn classify_api_failure(repo: &str, stderr: &str) -> GithubPreflightError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("gh auth login")
        || lower.contains("not logged")
        || lower.contains("authentication")
        || lower.contains("requires authentication")
        || lower.contains("401")
    {
        return GithubPreflightError::NotAuthenticated;
    }
    if lower.contains("404") || lower.contains("not found") {
        return GithubPreflightError::RepoNotFound {
            repo: repo.to_string(),
        };
    }
    let detail = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("gh reported an unspecified error")
        .to_string();
    GithubPreflightError::ProbeFailed { detail }
}

/// The real `gh`-shelling probe used outside tests.
pub struct RealGhProbe;

impl GhProbe for RealGhProbe {
    fn gh_version(&mut self) -> Option<String> {
        let output = Command::new("gh").arg("--version").output().ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn gh_api_repo(&mut self, repo: &str) -> GhCommandResult {
        match Command::new("gh")
            .args(["api", &format!("repos/{repo}")])
            .output()
        {
            Ok(output) => GhCommandResult {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            },
            Err(error) => GhCommandResult {
                success: false,
                stdout: String::new(),
                stderr: format!("could not run gh: {error}"),
            },
        }
    }
}

/// What a successful GitHub choice will do on first use, shown after the choice
/// is confirmed so the user isn't surprised by the labels and body metadata
/// Shelbi writes into their repo.
pub const GITHUB_DISCLOSURE: &str = "Shelbi will create `shelbi:id/*` and `shelbi:status/*` labels \
     on first use and store a small metadata block in each issue's body.";

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

/// Refuse to switch a project that already has a non-empty local board onto a
/// remote backend without migrating first: point at `issue-store migrate`
/// rather than leaving the existing cards stranded. A no-op when the chosen
/// backend is `file_system` or the project has no local board yet.
pub fn ensure_board_not_stranded(project: &str, tracker: &IssueTrackerConfig) -> Result<()> {
    if tracker.backend == IssueTrackerBackend::FileSystem {
        return Ok(());
    }
    if file_system_board_has_issues(project) {
        bail!(
            "project `{project}` already has issues on its local file_system board; switching to \
             {backend} here would strand them. Migrate them first:\n  \
             shelbi issue-store migrate --to {backend} --dry-run   # preview\n  \
             shelbi issue-store migrate --to {backend}             # then migrate",
            backend = tracker.backend,
        );
    }
    Ok(())
}

/// True when the project already has a non-empty local (file_system) board, so
/// switching it to GitHub would strand those cards unless they're migrated
/// first. Counts any task markdown under the project's tasks dir; a project
/// that was never set up (no tasks dir) is empty.
pub fn file_system_board_has_issues(project: &str) -> bool {
    let Ok(tasks_dir) = shelbi_state::tasks_dir(project) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(&tasks_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_owner_repo_from_every_github_remote_form() {
        for remote in [
            "git@github.com:jlong/shelbi.git",
            "https://github.com/jlong/shelbi.git",
            "https://github.com/jlong/shelbi",
            "ssh://git@github.com/jlong/shelbi.git",
            "https://oauth-token@github.com/jlong/shelbi.git",
            "git@github.com:jlong/shelbi",
        ] {
            assert_eq!(
                detect_github_repo(Some(remote)).as_deref(),
                Some("jlong/shelbi"),
                "failed for {remote}"
            );
        }
    }

    #[test]
    fn ignores_non_github_and_empty_remotes() {
        assert_eq!(detect_github_repo(None), None);
        assert_eq!(detect_github_repo(Some("")), None);
        assert_eq!(detect_github_repo(Some("   ")), None);
        assert_eq!(
            detect_github_repo(Some("git@gitlab.com:jlong/shelbi.git")),
            None
        );
        assert_eq!(
            detect_github_repo(Some("https://bitbucket.org/jlong/shelbi.git")),
            None
        );
        // A path that's too short to carry owner/repo.
        assert_eq!(detect_github_repo(Some("https://github.com/jlong")), None);
    }

    #[test]
    fn github_config_validates_and_round_trips() {
        let cfg = github_config("jlong/shelbi").unwrap();
        assert_eq!(cfg.backend, IssueTrackerBackend::Github);
        assert_eq!(cfg.github.as_ref().unwrap().repo, "jlong/shelbi");
        assert!(cfg.validate().is_ok());
        // A malformed repo is rejected up front, never written.
        assert!(github_config("not-a-repo").is_err());
        assert!(github_config("").is_err());
    }

    #[test]
    fn file_system_config_is_the_elided_default() {
        let cfg = file_system_config();
        assert_eq!(cfg.backend, IssueTrackerBackend::FileSystem);
        assert!(cfg.is_default());
    }

    /// A scripted probe that returns canned results per call.
    struct FakeProbe {
        version: Option<String>,
        api: GhCommandResult,
    }

    impl FakeProbe {
        fn ok_json(has_issues: bool, push: bool) -> GhCommandResult {
            GhCommandResult {
                success: true,
                stdout: format!(
                    "{{\"has_issues\": {has_issues}, \"permissions\": {{\"push\": {push}}}}}"
                ),
                stderr: String::new(),
            }
        }
    }

    impl GhProbe for FakeProbe {
        fn gh_version(&mut self) -> Option<String> {
            self.version.clone()
        }
        fn gh_api_repo(&mut self, _repo: &str) -> GhCommandResult {
            self.api.clone()
        }
    }

    fn fail(stderr: &str) -> GhCommandResult {
        GhCommandResult {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn preflight_passes_with_issues_and_push() {
        let mut probe = FakeProbe {
            version: Some("gh version 2.40.0".into()),
            api: FakeProbe::ok_json(true, true),
        };
        assert_eq!(github_preflight("jlong/shelbi", &mut probe), Ok(()));
    }

    #[test]
    fn preflight_flags_missing_gh() {
        let mut probe = FakeProbe {
            version: None,
            api: FakeProbe::ok_json(true, true),
        };
        assert_eq!(
            github_preflight("jlong/shelbi", &mut probe),
            Err(GithubPreflightError::GhNotInstalled)
        );
        assert!(GithubPreflightError::GhNotInstalled
            .guidance()
            .contains("Install"));
    }

    #[test]
    fn preflight_flags_unauthenticated() {
        let mut probe = FakeProbe {
            version: Some("gh version 2.40.0".into()),
            api: fail("gh: To get started with GitHub CLI, please run: gh auth login"),
        };
        assert_eq!(
            github_preflight("jlong/shelbi", &mut probe),
            Err(GithubPreflightError::NotAuthenticated)
        );
        assert!(GithubPreflightError::NotAuthenticated
            .guidance()
            .contains("gh auth login"));
    }

    #[test]
    fn preflight_flags_missing_repo() {
        let mut probe = FakeProbe {
            version: Some("gh version 2.40.0".into()),
            api: fail("gh: Not Found (HTTP 404)"),
        };
        assert_eq!(
            github_preflight("jlong/ghost", &mut probe),
            Err(GithubPreflightError::RepoNotFound {
                repo: "jlong/ghost".into()
            })
        );
    }

    #[test]
    fn preflight_flags_issues_disabled() {
        let mut probe = FakeProbe {
            version: Some("gh version 2.40.0".into()),
            api: FakeProbe::ok_json(false, true),
        };
        assert_eq!(
            github_preflight("jlong/shelbi", &mut probe),
            Err(GithubPreflightError::IssuesDisabled {
                repo: "jlong/shelbi".into()
            })
        );
    }

    #[test]
    fn preflight_flags_insufficient_permissions() {
        let mut probe = FakeProbe {
            version: Some("gh version 2.40.0".into()),
            api: FakeProbe::ok_json(true, false),
        };
        assert_eq!(
            github_preflight("jlong/shelbi", &mut probe),
            Err(GithubPreflightError::InsufficientPermissions {
                repo: "jlong/shelbi".into()
            })
        );
    }

    #[test]
    fn preflight_surfaces_unclassified_failures() {
        let mut probe = FakeProbe {
            version: Some("gh version 2.40.0".into()),
            api: fail("gh: connection reset by peer"),
        };
        assert!(matches!(
            github_preflight("jlong/shelbi", &mut probe),
            Err(GithubPreflightError::ProbeFailed { .. })
        ));
    }

    #[test]
    fn preflight_flags_unparseable_metadata() {
        let mut probe = FakeProbe {
            version: Some("gh version 2.40.0".into()),
            api: GhCommandResult {
                success: true,
                stdout: "not json".into(),
                stderr: String::new(),
            },
        };
        assert!(matches!(
            github_preflight("jlong/shelbi", &mut probe),
            Err(GithubPreflightError::ProbeFailed { .. })
        ));
    }
}
