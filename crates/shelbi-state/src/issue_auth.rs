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
//!    (`run gh auth login or set GH_TOKEN`) — or, if the `gh` probe *ran and
//!    kept failing*, [`Error::GhTokenProbeFailed`] naming the real cause (a
//!    stalled keychain), so the operator isn't sent to `gh auth login` for a
//!    login that is fine.
//!
//! **Keychain resilience.** Step 2 is the one slow, flaky step: on macOS a busy
//! `securityd` can stall a keychain read for several seconds, and `gh` gives up
//! around 6 s, so a naive per-read spawn turns a transient stall into a failed
//! board refresh (and hammers the throttle further). Two mitigations, both in
//! this module: the probe **retries** a couple of times with a short backoff
//! before giving up, and a successful probe is **cached in-process** for
//! [`TOKEN_CACHE_TTL`] behind a single-flight lock — so a long-lived process (the
//! daemon, a TUI) pays for one keychain read, not one per board read. The cheap
//! steps (env, `tokens.yml`) stay live and keep their precedence; only the
//! keychain read is memoized. The cache re-probes on a 401
//! ([`invalidate_cached_token`], wired to `gh`'s 401 in the store) or a daemon
//! restart / reload ([`invalidate_all_cached_tokens`]).
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
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use shelbi_core::{Error, GhTokenProbeFailure, Project, Result};

use crate::ProjectPaths;

/// How long a resolved token stays usable without re-probing `gh`. A generous
/// window (a token rarely changes mid-process) that keeps a long-lived process —
/// the daemon, a TUI — off the keychain: it pays for one `gh auth token` spawn,
/// not one per board read, so a transient macOS keychain stall never reaches the
/// board reader. Re-resolution otherwise happens only on a 401
/// ([`invalidate_cached_token`]) or a daemon restart (a fresh process starts
/// with an empty cache; [`invalidate_all_cached_tokens`] clears it in place).
const TOKEN_CACHE_TTL: Duration = Duration::from_secs(30 * 60);

/// Extra `gh auth token` attempts after the first, with a short backoff between
/// them. A borderline keychain stall (4–5.5 s against gh's ~6 s give-up) often
/// clears within a second, so a couple of retries turn a failed refresh into a
/// slightly slower one; a genuinely down keychain still falls through to
/// `tokens.yml` and the typed [`Error::GhTokenProbeFailed`] promptly.
const GH_PROBE_RETRIES: u32 = 2;

/// Backoff before the Nth retry (0-based): 200 ms, then 400 ms.
fn gh_probe_backoff(retry: u32) -> Duration {
    Duration::from_millis(200u64 << retry)
}

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
    resolve_github_token_with(|k| std::env::var(k).ok(), cached_gh_auth_token, &token_file)
}

/// Resolve a GitHub auth token from a project *name* rather than a loaded
/// [`Project`]. The out-of-repo `tokens.yml` lives at
/// `~/.shelbi/projects/<name>/tokens.yml` — [`ProjectPaths::state_root`] is just
/// [`crate::project_dir`] of the name — so the [`GitHubStore`] can resolve its
/// token per call holding only the project name, never a full `Project` (which
/// would force a config load on every API call).
///
/// [`GitHubStore`]: crate::GitHubStore
pub fn resolve_github_token_by_name(project: &str) -> Result<SecretToken> {
    let token_file = crate::project_dir(project)?.join("tokens.yml");
    resolve_github_token_with(|k| std::env::var(k).ok(), cached_gh_auth_token, &token_file)
}

// --- in-process keychain-probe cache ---------------------------------------
//
// Only the *expensive* step is memoized: the `gh auth token` keychain read. The
// cheap steps — the `GH_TOKEN` / `GITHUB_TOKEN` env and the out-of-repo
// `tokens.yml` — stay live and keep their precedence, so a token exported or
// staged after the process started is still picked up, and only the OS keychain
// is spared a spawn per board read. The `gh` login is process-wide (not per
// project), so a single global slot serves every project's `gh` fallback.

/// The keychain token from a successful `gh auth token`, plus when it landed, so
/// [`GhProbeCache::get_or_probe`] can honor the TTL. Only a success is cached: a
/// failed probe is never stored, so a stall can't pin a failure for the TTL.
struct CachedProbe {
    token: String,
    resolved_at: Instant,
}

/// A TTL cache around the single `gh auth token` probe, with the probe run under
/// the lock so concurrent resolutions single-flight behind one spawn rather than
/// racing N of them at the throttled keychain.
struct GhProbeCache {
    inner: Mutex<Option<CachedProbe>>,
}

impl GhProbeCache {
    const fn new() -> Self {
        GhProbeCache {
            inner: Mutex::new(None),
        }
    }

    /// A live cached token short-circuits without running `probe`; otherwise the
    /// caller holds the lock (blocking peers, so they read this result instead
    /// of spawning their own `gh`) while `probe` runs, caching only a success.
    fn get_or_probe(&self, ttl: Duration, probe: impl FnOnce() -> GhProbe) -> GhProbe {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = guard.as_ref() {
            if cached.resolved_at.elapsed() < ttl {
                return GhProbe::Token(cached.token.clone());
            }
        }
        let result = probe();
        if let GhProbe::Token(tok) = &result {
            *guard = Some(CachedProbe {
                token: tok.clone(),
                resolved_at: Instant::now(),
            });
        }
        result
    }

    fn invalidate(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
}

/// The process-wide keychain-probe cache (`Mutex::new` is `const`, so no lazy
/// init is needed).
static GH_PROBE_CACHE: GhProbeCache = GhProbeCache::new();

/// The `gh auth token` probe, served from the process-wide TTL cache. This is
/// what the resolution chain calls in place of the raw [`gh_auth_token`] so a
/// board read after a successful resolution never re-spawns `gh` within the TTL.
fn cached_gh_auth_token() -> GhProbe {
    GH_PROBE_CACHE.get_or_probe(TOKEN_CACHE_TTL, gh_auth_token)
}

/// Drop the cached keychain token so the next resolution re-probes `gh`. Called
/// when a `gh` call returns a 401 — the held credential was revoked or rotated,
/// so the cache must not keep serving it. The `project` names the call context;
/// the cached `gh` login is process-wide, so this clears it for every project.
pub fn invalidate_cached_token(project: &str) {
    let _ = project;
    GH_PROBE_CACHE.invalidate();
}

/// Drop the cached keychain token. A daemon reload re-execs into a fresh process
/// (whose cache starts empty), so this exists for an in-process reload and as
/// the explicit "forget everything" counterpart to [`invalidate_cached_token`].
pub fn invalidate_all_cached_tokens() {
    GH_PROBE_CACHE.invalidate();
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
    gh: impl FnOnce() -> GhProbe,
    token_file: &Path,
) -> Result<SecretToken> {
    // 1. Environment — GH_TOKEN wins over GITHUB_TOKEN (gh's own precedence).
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(tok) = env(var).and_then(non_empty) {
            return Ok(SecretToken::new(tok, TokenSource::Env(var)));
        }
    }

    // 2. `gh` keychain auth — the recommended default; no secret on disk. A
    //    probe that *ran and kept failing* (a keychain stall) is remembered so
    //    it can become the typed error below rather than being flattened into a
    //    generic "no token found"; a `gh` that is simply absent or unauthed
    //    falls through silently, as before.
    let probe = gh();
    if let GhProbe::Token(tok) = &probe {
        if let Some(tok) = non_empty(tok.clone()) {
            return Ok(SecretToken::new(tok, TokenSource::GhCli));
        }
    }

    // 3. Out-of-repo tokens.yml (chmod 600).
    if let Some(tok) = read_token_file(token_file)? {
        return Ok(SecretToken::new(tok, TokenSource::File));
    }

    // 4. Nothing resolved. If the `gh` probe failed after its retries, name the
    //    real cause (exit status, elapsed, stderr tail) — a stalled keychain,
    //    not a missing login. Otherwise the classic actionable missing-auth
    //    error naming every place we looked.
    if let GhProbe::Failed(failure) = probe {
        return Err(Error::GhTokenProbeFailed(Box::new(GhTokenProbeFailure {
            attempts: failure.attempts,
            exit_status: failure.exit_status,
            elapsed: format_elapsed(failure.elapsed),
            stderr_tail: failure.stderr_tail,
            token_file: token_file.display().to_string(),
        })));
    }
    Err(Error::MissingIssueTrackerAuth {
        backend: "github",
        token_file: token_file.display().to_string(),
    })
}

/// The outcome of the (retried) `gh auth token` probe.
enum GhProbe {
    /// A token came back from the keychain (may still be blank — the caller
    /// runs it through [`non_empty`]).
    Token(String),
    /// `gh` is not usable here — the spawn failed (not installed) or it exited
    /// zero with empty output (not logged in cleanly). Fall through silently to
    /// the next resolution step, as the original probe did.
    Unavailable,
    /// `gh` ran and kept exiting non-zero across every attempt — the keychain
    /// read timed out or errored. Carries the cause for the typed error.
    Failed(GhProbeFailure),
}

/// The diagnostic tail of a `gh auth token` probe that exhausted its retries.
struct GhProbeFailure {
    attempts: u32,
    exit_status: String,
    elapsed: Duration,
    stderr_tail: String,
}

/// Render a probe duration for the operator, e.g. `6.0s`.
fn format_elapsed(elapsed: Duration) -> String {
    format!("{:.1}s", elapsed.as_secs_f64())
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

/// One attempt at the `gh auth token` probe — a single classification of a
/// single `gh` spawn. The retry loop in [`gh_auth_token_with`] turns a sequence
/// of these into a [`GhProbe`].
enum GhAttempt {
    /// A non-empty token from the keychain — resolve immediately.
    Token(String),
    /// `gh` is absent (spawn failed) or exited zero with empty output. Not worth
    /// retrying: fall through silently.
    Unavailable,
    /// `gh` ran and exited non-zero. Retryable — a keychain stall often clears
    /// within a second.
    Errored(GhProbeFailure),
}

/// The real single `gh auth token` spawn: ask the `gh` CLI for the token it
/// holds in the OS keychain, timing the call so a stall's elapsed time is
/// legible in the typed error.
fn gh_auth_token_once() -> GhAttempt {
    let started = Instant::now();
    let output = match Command::new("gh").args(["auth", "token"]).output() {
        Ok(output) => output,
        // Spawn failed — `gh` isn't installed / on PATH. Silent fall-through, as
        // the original probe did (this machine reaches `tokens.yml` instead).
        Err(_) => return GhAttempt::Unavailable,
    };
    if output.status.success() {
        return match non_empty(String::from_utf8_lossy(&output.stdout).into_owned()) {
            Some(tok) => GhAttempt::Token(tok),
            None => GhAttempt::Unavailable,
        };
    }
    GhAttempt::Errored(GhProbeFailure {
        attempts: 1,
        exit_status: output.status.to_string(),
        elapsed: started.elapsed(),
        stderr_tail: stderr_tail(&output.stderr),
    })
}

/// Keep the last line and at most ~200 bytes of a probe's stderr — enough to
/// carry `gh`'s actual message (`no oauth token found for github.com`) into the
/// typed error without dumping an unbounded blob into a log line.
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = last.trim();
    const MAX: usize = 200;
    if trimmed.len() <= MAX {
        trimmed.to_string()
    } else {
        format!("…{}", &trimmed[trimmed.len() - MAX..])
    }
}

/// The `gh auth token` probe with retries: spawn `gh`, and on a non-zero exit
/// (a keychain stall) back off briefly and try again, up to [`GH_PROBE_RETRIES`]
/// extra times, before giving up with the accumulated failure. Split from the
/// spawn and sleep so the retry policy is unit-testable with a scripted runner
/// and no real waiting.
fn gh_auth_token() -> GhProbe {
    gh_auth_token_with(gh_auth_token_once, std::thread::sleep)
}

fn gh_auth_token_with(
    mut run_once: impl FnMut() -> GhAttempt,
    mut sleep: impl FnMut(Duration),
) -> GhProbe {
    let mut last: Option<GhProbeFailure> = None;
    for retry in 0..=GH_PROBE_RETRIES {
        match run_once() {
            GhAttempt::Token(tok) => return GhProbe::Token(tok),
            GhAttempt::Unavailable => return GhProbe::Unavailable,
            GhAttempt::Errored(mut failure) => {
                // Count attempts cumulatively so the typed error reads
                // "failed after 3 attempt(s)", not "after 1".
                failure.attempts = retry + 1;
                last = Some(failure);
                if retry < GH_PROBE_RETRIES {
                    sleep(gh_probe_backoff(retry));
                }
            }
        }
    }
    // Every attempt errored — `Errored` is the only branch that loops, so `last`
    // is always `Some` here.
    match last {
        Some(failure) => GhProbe::Failed(failure),
        None => GhProbe::Unavailable,
    }
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

    fn no_gh() -> GhProbe {
        GhProbe::Unavailable
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
            || GhProbe::Token("gh-keychain".to_string()),
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
            || GhProbe::Token("from-gh".to_string()),
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

    // --- gh probe retry ---------------------------------------------------

    fn errored(exit: &str, elapsed_ms: u64, stderr: &str) -> GhAttempt {
        GhAttempt::Errored(GhProbeFailure {
            attempts: 1,
            exit_status: exit.to_string(),
            elapsed: Duration::from_millis(elapsed_ms),
            stderr_tail: stderr.to_string(),
        })
    }

    #[test]
    fn probe_retries_a_failure_then_succeeds() {
        let mut attempts = 0u32;
        let slept = std::cell::Cell::new(0u32);
        let probe = gh_auth_token_with(
            || {
                attempts += 1;
                if attempts == 1 {
                    errored("exit status: 1", 6000, "no oauth token found for github.com")
                } else {
                    GhAttempt::Token("recovered".to_string())
                }
            },
            |_| slept.set(slept.get() + 1),
        );
        assert!(matches!(probe, GhProbe::Token(t) if t == "recovered"));
        assert_eq!(attempts, 2, "second attempt should have run");
        assert_eq!(slept.get(), 1, "exactly one backoff before the retry");
    }

    #[test]
    fn probe_exhausts_retries_and_reports_the_cause() {
        let mut attempts = 0u32;
        let probe = gh_auth_token_with(
            || {
                attempts += 1;
                errored("exit status: 1", 6003, "no oauth token found for github.com")
            },
            |_| {},
        );
        assert_eq!(attempts, GH_PROBE_RETRIES + 1, "first attempt plus every retry");
        match probe {
            GhProbe::Failed(f) => {
                assert_eq!(f.attempts, GH_PROBE_RETRIES + 1);
                assert_eq!(f.exit_status, "exit status: 1");
                assert_eq!(f.stderr_tail, "no oauth token found for github.com");
            }
            other => panic!("expected Failed, got {:?}", matches!(other, GhProbe::Token(_))),
        }
    }

    #[test]
    fn probe_treats_absent_gh_as_unavailable_without_retrying() {
        let mut attempts = 0u32;
        let probe = gh_auth_token_with(
            || {
                attempts += 1;
                GhAttempt::Unavailable
            },
            |_| panic!("should not sleep when gh is simply unavailable"),
        );
        assert!(matches!(probe, GhProbe::Unavailable));
        assert_eq!(attempts, 1, "an unavailable gh is not retried");
    }

    #[test]
    fn a_transient_probe_failure_still_resolves_a_token() {
        // Acceptance: `gh auth token` fails once then succeeds → a read resolves
        // (no missing-auth error, so no `refresh-failed`).
        let missing = tmp_path("transient-probe");
        let _ = std::fs::remove_file(&missing);
        let mut attempts = 0u32;
        let tok = resolve_github_token_with(
            no_env,
            || {
                gh_auth_token_with(
                    || {
                        attempts += 1;
                        if attempts == 1 {
                            errored("exit status: 1", 6000, "stall")
                        } else {
                            GhAttempt::Token("gh-after-retry".to_string())
                        }
                    },
                    |_| {},
                )
            },
            &missing,
        )
        .unwrap();
        assert_eq!(tok.expose(), "gh-after-retry");
        assert_eq!(tok.source(), TokenSource::GhCli);
    }

    #[test]
    fn exhausted_probe_becomes_a_typed_cause_error() {
        // Acceptance: the typed error after exhausted retries names exit status,
        // elapsed time, and stderr tail — not "no token found".
        let missing = tmp_path("probe-failed-error");
        let _ = std::fs::remove_file(&missing);
        let err = resolve_github_token_with(
            no_env,
            || {
                GhProbe::Failed(GhProbeFailure {
                    attempts: 3,
                    exit_status: "exit status: 1".to_string(),
                    elapsed: Duration::from_millis(6003),
                    stderr_tail: "no oauth token found for github.com".to_string(),
                })
            },
            &missing,
        )
        .unwrap_err();
        match &err {
            Error::GhTokenProbeFailed(detail) => {
                assert_eq!(detail.attempts, 3);
                assert_eq!(detail.exit_status, "exit status: 1");
                assert_eq!(detail.elapsed, "6.0s");
                assert_eq!(detail.stderr_tail, "no oauth token found for github.com");
            }
            other => panic!("expected GhTokenProbeFailed, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("6.0s"), "elapsed must render: {msg}");
        assert!(msg.contains("keychain"), "message must name the real cause: {msg}");
    }

    #[test]
    fn a_probe_failure_does_not_mask_a_token_file() {
        // A stalled keychain must still let a staged `tokens.yml` win — the probe
        // failure only surfaces when nothing else resolves.
        let file = tmp_path("probe-failed-file-wins");
        write_private(&file, "github:\n  token: from-file\n");
        let tok = resolve_github_token_with(
            no_env,
            || {
                GhProbe::Failed(GhProbeFailure {
                    attempts: 3,
                    exit_status: "exit status: 1".to_string(),
                    elapsed: Duration::from_secs(6),
                    stderr_tail: "stall".to_string(),
                })
            },
            &file,
        )
        .unwrap();
        assert_eq!(tok.expose(), "from-file");
        assert_eq!(tok.source(), TokenSource::File);
    }

    #[test]
    fn stderr_tail_keeps_the_last_nonblank_line_bounded() {
        assert_eq!(stderr_tail(b"warning\nno oauth token found\n"), "no oauth token found");
        assert_eq!(stderr_tail(b"   \n"), "");
        let long = "x".repeat(500);
        let tail = stderr_tail(long.as_bytes());
        assert!(tail.starts_with('…'));
        assert!(tail.len() <= 200 + 4, "bounded near 200 bytes: {}", tail.len());
    }

    // --- keychain-probe cache --------------------------------------------
    //
    // Each test drives its own `GhProbeCache` instance, so the global cache and
    // parallel siblings can't perturb the hit counts.

    #[test]
    fn cache_hit_skips_the_probe_within_ttl() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = GhProbeCache::new();
        let probes = AtomicUsize::new(0);
        let probe = || {
            probes.fetch_add(1, Ordering::SeqCst);
            GhProbe::Token("t".to_string())
        };
        assert!(matches!(cache.get_or_probe(TOKEN_CACHE_TTL, probe), GhProbe::Token(_)));
        assert!(matches!(cache.get_or_probe(TOKEN_CACHE_TTL, probe), GhProbe::Token(_)));
        assert_eq!(probes.load(Ordering::SeqCst), 1, "second read served from cache");
    }

    #[test]
    fn a_zero_ttl_always_re_probes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = GhProbeCache::new();
        let probes = AtomicUsize::new(0);
        let probe = || {
            probes.fetch_add(1, Ordering::SeqCst);
            GhProbe::Token("t".to_string())
        };
        cache.get_or_probe(Duration::ZERO, probe);
        cache.get_or_probe(Duration::ZERO, probe);
        assert_eq!(probes.load(Ordering::SeqCst), 2, "an expired entry re-probes");
    }

    #[test]
    fn invalidation_forces_a_re_probe() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = GhProbeCache::new();
        let probes = AtomicUsize::new(0);
        let probe = || {
            probes.fetch_add(1, Ordering::SeqCst);
            GhProbe::Token("t".to_string())
        };
        cache.get_or_probe(TOKEN_CACHE_TTL, probe);
        cache.invalidate();
        cache.get_or_probe(TOKEN_CACHE_TTL, probe);
        assert_eq!(probes.load(Ordering::SeqCst), 2, "invalidation dropped the entry");
    }

    #[test]
    fn a_failed_probe_is_not_cached() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = GhProbeCache::new();
        let probes = AtomicUsize::new(0);
        let probe = || {
            probes.fetch_add(1, Ordering::SeqCst);
            GhProbe::Unavailable
        };
        assert!(matches!(cache.get_or_probe(TOKEN_CACHE_TTL, probe), GhProbe::Unavailable));
        assert!(matches!(cache.get_or_probe(TOKEN_CACHE_TTL, probe), GhProbe::Unavailable));
        assert_eq!(probes.load(Ordering::SeqCst), 2, "a failed probe is retried, not cached");
    }

    #[test]
    fn concurrent_resolutions_share_one_probe() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;
        let cache = GhProbeCache::new();
        let probes = AtomicUsize::new(0);
        let barrier = Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    let out = cache.get_or_probe(TOKEN_CACHE_TTL, || {
                        probes.fetch_add(1, Ordering::SeqCst);
                        // Hold the lock long enough that peers pile up behind it
                        // rather than each spawning their own probe.
                        std::thread::sleep(Duration::from_millis(50));
                        GhProbe::Token("shared".to_string())
                    });
                    assert!(matches!(out, GhProbe::Token(_)));
                });
            }
        });
        assert_eq!(
            probes.load(Ordering::SeqCst),
            1,
            "eight concurrent resolutions spawned one probe, not eight"
        );
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
