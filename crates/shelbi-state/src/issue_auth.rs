//! Token/auth resolution for remote issue-tracker backends.
//!
//! Remote backends (GitHub today; Jira / Linear later) need a credential to
//! reach their API, and the one thing that is easy to get dangerously wrong is
//! **letting that credential become committable to a repo.** This module
//! resolves a token the modern, safe way, per `Plans/pluggable-task-stores.md`
//! §4 + D2:
//!
//! **Resolution order — first hit wins, safest first:**
//! 1. `GH_TOKEN` / `GITHUB_TOKEN` in the process environment (CI / headless;
//!    shelbi never writes it down).
//! 2. The `gh` CLI's keychain auth via `gh auth token` (the recommended
//!    default — the secret lives in the OS keychain, nothing in shelbi to
//!    manage). Reuses the same `gh` login shelbi already relies on for PRs.
//! 3. `~/.shelbi/projects/<name>/tokens.yml` — user-local state, physically
//!    **outside any repo**, so there is nothing to `git add` and no way to leak
//!    it via `git add -f`. Must be `chmod 600`; a looser mode is refused.
//! 4. Otherwise a typed [`Error::MissingIssueTrackerAuth`] naming the fix
//!    (`run gh auth login or set GH_TOKEN`).
//!
//! The resolved value is a [`SecretToken`] whose `Debug` / `Display` are
//! redacted, so a token can never leak into a log, trace, or panic message by
//! accident — the raw bytes come out only through the explicit
//! [`SecretToken::expose`].
//!
//! This resolver is standalone: it is unit-tested and not yet wired to a live
//! backend. The GitHub `IssueStore` will call [`resolve_github_token`] when it
//! lands.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use shelbi_core::{Error, Project, Result};

use crate::ProjectPaths;

/// Where a resolved token came from. Carried on [`SecretToken`] so a caller (or
/// a test) can assert the resolution order held without ever touching the
/// secret itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// A `GH_TOKEN` / `GITHUB_TOKEN` process environment variable (the field
    /// names the exact variable that hit).
    Env(&'static str),
    /// The `gh` CLI keychain, read via `gh auth token`.
    GhCli,
    /// The out-of-repo `~/.shelbi/projects/<name>/tokens.yml` file.
    File,
}

impl fmt::Display for TokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenSource::Env(var) => write!(f, "${var}"),
            TokenSource::GhCli => f.write_str("`gh auth token`"),
            TokenSource::File => f.write_str("tokens.yml"),
        }
    }
}

/// A resolved auth token that never prints its own value.
///
/// `Debug` and `Display` are both redacted — they disclose only the
/// [`TokenSource`], never the secret — so interpolating a `SecretToken` into a
/// log line, a `tracing` span, an `assert!` message, or a panic is safe by
/// construction. The raw bytes are reachable only through [`expose`], which
/// exists so a backend can hand the token to an HTTP client but has to *ask*
/// for it explicitly.
///
/// It deliberately does not derive `Serialize`, so it can't be written into a
/// state file or wire payload by accident.
///
/// [`expose`]: SecretToken::expose
#[derive(Clone)]
pub struct SecretToken {
    value: String,
    source: TokenSource,
}

impl SecretToken {
    fn new(value: String, source: TokenSource) -> Self {
        SecretToken { value, source }
    }

    /// The raw token bytes. Named `expose` (not `as_str`) so every call site
    /// reads as a deliberate un-redaction — grep for `.expose()` to audit
    /// exactly where the secret is handled.
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// Where this token was resolved from.
    pub fn source(&self) -> TokenSource {
        self.source
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretToken")
            .field("source", &self.source)
            .field("value", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The out-of-repo `tokens.yml` shape. One optional block per backend, each
/// carrying only a `token:` field. The blocks are independent (not an
/// enum-tagged union) so a user can stage credentials for more than one backend
/// in the same file, mirroring [`shelbi_core::IssueTrackerConfig`]. Only the
/// selected backend's block is read.
#[derive(Debug, Default, Deserialize)]
struct TokensFile {
    #[serde(default)]
    github: Option<BackendToken>,
}

#[derive(Debug, Default, Deserialize)]
struct BackendToken {
    #[serde(default)]
    token: Option<String>,
}

/// Absolute path to a project's out-of-repo token file:
/// `~/.shelbi/projects/<name>/tokens.yml`. Routed through
/// [`ProjectPaths::state_root`] so it lands in user-local state regardless of
/// config mode — never inside the repo working tree.
pub fn token_file_path(project: &Project) -> Result<PathBuf> {
    Ok(project.state_root()?.join("tokens.yml"))
}

/// Resolve a GitHub auth token for `project`, walking the full resolution chain
/// against the real environment, the real `gh` CLI, and the project's real
/// out-of-repo `tokens.yml`. See the [module docs](self) for the order.
pub fn resolve_github_token(project: &Project) -> Result<SecretToken> {
    let token_file = token_file_path(project)?;
    resolve_github_token_with(|k| std::env::var(k).ok(), gh_auth_token, &token_file)
}

/// Testable core of [`resolve_github_token`]: the resolution chain with its
/// three external inputs injected.
///
/// * `env` — process environment lookup (`GH_TOKEN`, then `GITHUB_TOKEN`).
/// * `gh` — the `gh auth token` probe, called only if env missed.
/// * `token_file` — path to the out-of-repo `tokens.yml`, read only if env and
///   `gh` both missed.
///
/// Each source is tried in order and the first non-empty hit wins, so a
/// present-but-empty env var falls through rather than masking a real token
/// further down the chain.
fn resolve_github_token_with(
    env: impl Fn(&str) -> Option<String>,
    gh: impl FnOnce() -> Option<String>,
    token_file: &Path,
) -> Result<SecretToken> {
    // 1. Environment — GH_TOKEN wins over GITHUB_TOKEN (gh's own precedence).
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(tok) = env(var).and_then(non_empty) {
            return Ok(SecretToken::new(tok, TokenSource::Env(var)));
        }
    }

    // 2. `gh` keychain auth — the recommended default; no secret on disk.
    if let Some(tok) = gh().and_then(non_empty) {
        return Ok(SecretToken::new(tok, TokenSource::GhCli));
    }

    // 3. Out-of-repo tokens.yml (chmod 600).
    if let Some(tok) = read_token_file(token_file)? {
        return Ok(SecretToken::new(tok, TokenSource::File));
    }

    // 4. Nothing — a typed, actionable error naming every place we looked.
    Err(Error::MissingIssueTrackerAuth {
        backend: "github",
        token_file: token_file.display().to_string(),
    })
}

/// Trim a candidate token and drop it if the result is empty — an
/// exported-but-blank env var or an empty `token:` field must not shadow a real
/// credential further down the chain.
fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Read the GitHub token from an out-of-repo `tokens.yml`.
///
/// Returns `Ok(None)` when the file is absent or present but carries no
/// `github.token`, so the caller falls through to the missing-auth error.
/// Refuses (with [`Error::InsecureTokenFile`]) a unix file whose mode is looser
/// than `0600`.
fn read_token_file(path: &Path) -> Result<Option<String>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };

    ensure_private_permissions(path)?;

    let parsed: TokensFile = serde_yaml::from_str(&contents)?;
    Ok(parsed
        .github
        .and_then(|g| g.token)
        .and_then(non_empty))
}

/// Reject a token file readable by group or other. Unix only — on other
/// platforms the OS model differs and this is a no-op.
#[cfg(unix)]
fn ensure_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)?.permissions().mode();
    // Any group/other permission bit set ⇒ a secret others can read.
    if mode & 0o077 != 0 {
        return Err(Error::InsecureTokenFile {
            path: path.display().to_string(),
            mode: mode & 0o777,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// The real `gh auth token` probe: ask the `gh` CLI for the token it holds in
/// the OS keychain. Returns `None` when `gh` is absent, not logged in, or emits
/// nothing — every such case falls through to the next resolution step rather
/// than erroring, so a machine without `gh` still reaches the `tokens.yml` /
/// typed-error path.
fn gh_auth_token() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    non_empty(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token file readable only by its owner, used by the happy-path file
    /// tests so `ensure_private_permissions` passes.
    fn write_private(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-issue-auth-{}-{}",
            std::process::id(),
            name,
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("tokens.yml")
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn no_gh() -> Option<String> {
        None
    }

    #[test]
    fn env_gh_token_wins_first() {
        let missing = tmp_path("env-first-missing");
        let _ = std::fs::remove_file(&missing);
        let tok = resolve_github_token_with(
            |k| (k == "GH_TOKEN").then(|| "env-ghp".to_string()),
            || panic!("gh must not be consulted once env hits"),
            &missing,
        )
        .unwrap();
        assert_eq!(tok.expose(), "env-ghp");
        assert_eq!(tok.source(), TokenSource::Env("GH_TOKEN"));
    }

    #[test]
    fn github_token_env_is_the_second_env_fallback() {
        let missing = tmp_path("github-token-env");
        let _ = std::fs::remove_file(&missing);
        let tok = resolve_github_token_with(
            |k| (k == "GITHUB_TOKEN").then(|| "gh-token-env".to_string()),
            no_gh,
            &missing,
        )
        .unwrap();
        assert_eq!(tok.expose(), "gh-token-env");
        assert_eq!(tok.source(), TokenSource::Env("GITHUB_TOKEN"));
    }

    #[test]
    fn blank_env_var_falls_through_to_gh() {
        let missing = tmp_path("blank-env");
        let _ = std::fs::remove_file(&missing);
        let tok = resolve_github_token_with(
            |k| (k == "GH_TOKEN").then(|| "   ".to_string()),
            || Some("gh-keychain".to_string()),
            &missing,
        )
        .unwrap();
        assert_eq!(tok.expose(), "gh-keychain");
        assert_eq!(tok.source(), TokenSource::GhCli);
    }

    #[test]
    fn gh_cli_is_tried_before_the_file() {
        let file = tmp_path("gh-before-file");
        write_private(&file, "github:\n  token: from-file\n");
        let tok = resolve_github_token_with(
            no_env,
            || Some("from-gh".to_string()),
            &file,
        )
        .unwrap();
        assert_eq!(tok.expose(), "from-gh");
        assert_eq!(tok.source(), TokenSource::GhCli);
    }

    #[test]
    fn file_is_the_last_resort_before_error() {
        let file = tmp_path("file-last");
        write_private(&file, "github:\n  token: file-tok\n");
        let tok = resolve_github_token_with(no_env, no_gh, &file).unwrap();
        assert_eq!(tok.expose(), "file-tok");
        assert_eq!(tok.source(), TokenSource::File);
    }

    #[test]
    fn missing_everything_is_a_typed_actionable_error() {
        let missing = tmp_path("all-missing");
        let _ = std::fs::remove_file(&missing);
        let err = resolve_github_token_with(no_env, no_gh, &missing).unwrap_err();
        match &err {
            Error::MissingIssueTrackerAuth { backend, token_file } => {
                assert_eq!(*backend, "github");
                assert_eq!(token_file, &missing.display().to_string());
            }
            other => panic!("expected MissingIssueTrackerAuth, got {other:?}"),
        }
        // The rendered message names the one-line fix.
        let msg = err.to_string();
        assert!(msg.contains("gh auth login"), "message must name the fix: {msg}");
        assert!(msg.contains("GH_TOKEN"), "message must name GH_TOKEN: {msg}");
    }

    #[test]
    fn file_with_no_github_block_falls_through_to_error() {
        let file = tmp_path("file-empty-block");
        write_private(&file, "jira:\n  token: ignore-me\n");
        let err = resolve_github_token_with(no_env, no_gh, &file).unwrap_err();
        assert!(matches!(err, Error::MissingIssueTrackerAuth { .. }));
    }

    #[test]
    fn token_is_redacted_in_debug_and_display() {
        let tok = SecretToken::new("ghp_supersecret_value".to_string(), TokenSource::GhCli);
        let debug = format!("{tok:?}");
        let display = format!("{tok}");
        for rendered in [&debug, &display] {
            assert!(
                !rendered.contains("ghp_supersecret_value"),
                "raw token leaked into `{rendered}`"
            );
            assert!(
                rendered.contains("redacted"),
                "expected a redaction marker in `{rendered}`"
            );
        }
        // The secret is still reachable through the explicit accessor.
        assert_eq!(tok.expose(), "ghp_supersecret_value");
    }

    #[test]
    fn missing_auth_error_never_contains_a_token() {
        // Even when a token *is* present, it must never appear in a rendered
        // error — sanity-check the missing-auth path stays value-free.
        let missing = tmp_path("no-token-in-error");
        let _ = std::fs::remove_file(&missing);
        let err = resolve_github_token_with(no_env, no_gh, &missing).unwrap_err();
        assert!(!err.to_string().contains("ghp_"));
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_token_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let file = tmp_path("loose-perms");
        std::fs::write(&file, "github:\n  token: leaky\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = resolve_github_token_with(no_env, no_gh, &file).unwrap_err();
        match err {
            Error::InsecureTokenFile { mode, .. } => assert_eq!(mode, 0o644),
            other => panic!("expected InsecureTokenFile, got {other:?}"),
        }
    }
}
