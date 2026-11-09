//! The issue-tracker step's checks, shared by every front end: build the
//! board config a new project writes ([`github_config`] /
//! [`file_system_config`]), detect the GitHub repo an origin names
//! ([`detect_github_repo`]), verify a GitHub board can be used before it's
//! written ([`github_preflight`], over the [`GhProbe`] seam so every failure
//! path is unit-tested), and refuse to strand a non-empty local board
//! ([`ensure_board_not_stranded`]). The prompts live in each front end.

use std::process::Command;

use anyhow::{bail, Result};
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

pub use shelbi_core::github::detect_github_repo;

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

pub use shelbi_app::new_project::GITHUB_DISCLOSURE;

/// The Issues step's check for a host: [`github_preflight`] on the real `gh`,
/// failing with the fix to show. Runs `gh` (and the network): keep it off
/// any UI thread.
pub fn preflight_github(repo: &str) -> std::result::Result<(), String> {
    github_preflight(repo.trim(), &mut RealGhProbe).map_err(|failure| failure.guidance())
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
