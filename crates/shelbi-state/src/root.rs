//! Resolve the shelbi root directory — the global state dir that holds
//! `projects/`, `sessions/`, `logs/`, `workspaces/`, and `state.json`.
//!
//! Precedence (highest wins):
//!
//! 1. `--root <path>` flag — stashed by `shelbi-cli` via [`set_root_override`]
//!    before any subcommand runs.
//! 2. `$SHELBI_ROOT` env var.
//! 3. `$SHELBI_HOME` env var (legacy alias kept for in-flight tests + docs).
//! 4. Compile-time default baked at install time via `build.rs` and
//!    `env!("SHELBI_DEFAULT_ROOT")`. Empty when the binary was built
//!    without the install-script prompt, in which case we fall through.
//! 5. `~/.shelbi` as the final fallback.
//!
//! [`expand_tilde_str`] expands a leading `~/` against `$HOME` so the user
//! can type `~/work/.shelbi` in any of (1)/(2)/(3) and have it land in the
//! right place. Anything past the first segment is left alone.

use std::path::PathBuf;
use std::sync::OnceLock;

use shelbi_core::{Error, Result};

/// Where the resolved root came from. Returned alongside the path so
/// error messages (e.g. unwritable root) can name the precedence step
/// the user needs to adjust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// `--root` flag passed on the `shelbi` invocation.
    CliFlag,
    /// `$SHELBI_ROOT` env var.
    EnvRoot,
    /// `$SHELBI_HOME` legacy env var.
    EnvHome,
    /// Compile-time default baked at `cargo build` time.
    CompileTime,
    /// `~/.shelbi` fallback when no other source resolved.
    HomeFallback,
}

impl RootSource {
    /// One-line description suitable for error messages.
    pub fn describe(self) -> &'static str {
        match self {
            RootSource::CliFlag => "`--root` flag",
            RootSource::EnvRoot => "`$SHELBI_ROOT` env var",
            RootSource::EnvHome => "`$SHELBI_HOME` env var",
            RootSource::CompileTime => "install-time default (baked into the binary)",
            RootSource::HomeFallback => "`~/.shelbi` fallback",
        }
    }
}

static ROOT_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Compile-time default baked by `build.rs`. Empty when the binary was
/// built outside the install script (e.g. `cargo test`, dev `cargo build`).
const COMPILE_TIME_DEFAULT: &str = env!("SHELBI_DEFAULT_ROOT");

/// Record the `--root` flag value parsed by the top-level CLI. Idempotent —
/// only the first call wins, so a second invocation in the same process
/// (e.g. a TUI re-spawn that re-parses argv) doesn't trample the first.
/// Called from `shelbi-cli` before dispatch.
pub fn set_root_override(path: PathBuf) {
    let _ = ROOT_OVERRIDE.set(path);
}

/// Resolve the shelbi root using the precedence chain documented at the
/// top of this module. Returns the resolved path and the step that picked
/// it. Errors only when *every* step fails — concretely, when the home
/// directory can't be located and no other source was set.
pub fn resolve() -> Result<(PathBuf, RootSource)> {
    resolve_with(ROOT_OVERRIDE.get().cloned(), COMPILE_TIME_DEFAULT)
}

/// Pure-function form of [`resolve`] — the override + compile-time
/// default are passed in explicitly so tests can exercise every
/// precedence step without mutating the process-wide `OnceLock` or
/// rebuilding the crate with a different `SHELBI_DEFAULT_ROOT`.
pub(crate) fn resolve_with(
    cli_override: Option<PathBuf>,
    compile_time: &str,
) -> Result<(PathBuf, RootSource)> {
    if let Some(p) = cli_override {
        return Ok((expand_tilde_path(&p), RootSource::CliFlag));
    }
    if let Ok(s) = std::env::var("SHELBI_ROOT") {
        if !s.is_empty() {
            return Ok((expand_tilde_str(&s), RootSource::EnvRoot));
        }
    }
    if let Ok(s) = std::env::var("SHELBI_HOME") {
        if !s.is_empty() {
            return Ok((expand_tilde_str(&s), RootSource::EnvHome));
        }
    }
    if !compile_time.is_empty() {
        return Ok((expand_tilde_str(compile_time), RootSource::CompileTime));
    }
    let home = dirs::home_dir().ok_or_else(|| Error::Other("no home directory".into()))?;
    Ok((home.join(".shelbi"), RootSource::HomeFallback))
}

/// Convenience wrapper around [`resolve`] that drops the source. Use this
/// from code paths that just need the path; reach for `resolve()` when
/// you need to surface where the path came from in an error message.
pub fn root() -> Result<PathBuf> {
    resolve().map(|(p, _)| p)
}

/// Expand a leading `~/` against `$HOME`. Anything else is returned
/// verbatim. Bare `~` (no trailing slash) is also expanded for ergonomic
/// parity with shell tilde expansion.
pub fn expand_tilde_str(s: &str) -> PathBuf {
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(s)
}

/// [`expand_tilde_str`] applied to a `Path`. Falls back to the input
/// when the path is non-UTF-8 (no expansion possible).
pub fn expand_tilde_path(p: &std::path::Path) -> PathBuf {
    match p.to_str() {
        Some(s) => expand_tilde_str(s),
        None => p.to_path_buf(),
    }
}

/// The set of subdirectories that `shelbi init` and `shelbi reload`
/// guarantee exist under the resolved root. Listed in one place so the
/// two callers (and the self-heal tests) stay in lockstep.
pub const STANDARD_SUBDIRS: &[&str] = &["projects", "sessions", "agents", "logs", "workspaces"];

/// Create the resolved root + each entry in [`STANDARD_SUBDIRS`] when
/// missing. Idempotent — existing directories are left alone, and the
/// resolved path is returned so the caller can print it. Hard-fails with
/// an error that names the resolved path *and* the precedence step that
/// picked it when any directory can't be created (typically: parent dir
/// unwritable).
pub fn ensure_root_subdirs() -> Result<PathBuf> {
    let (root, source) = resolve()?;
    if let Err(e) = std::fs::create_dir_all(&root) {
        return Err(Error::Other(format!(
            "shelbi root {} is unwritable (from {}): {e}",
            root.display(),
            source.describe(),
        )));
    }
    for sub in STANDARD_SUBDIRS {
        let path = root.join(sub);
        if let Err(e) = std::fs::create_dir_all(&path) {
            return Err(Error::Other(format!(
                "could not create {} under shelbi root {} (from {}): {e}",
                sub,
                root.display(),
                source.describe(),
            )));
        }
    }
    // The `ssh/` ControlMaster dir needs `0700` perms (not the default
    // 0755 the other subdirs get), so it doesn't live in STANDARD_SUBDIRS
    // — this handles it with its own helper. Failing here is a hard
    // error for the same reason the loop above is: a fresh `shelbi init`
    // must leave a working layout behind, and hub→devbox SSH dispatch is
    // broken without this dir.
    if let Err(e) = crate::ssh_control::ensure_ssh_control_dir() {
        return Err(Error::Other(format!(
            "could not create ssh/ under shelbi root {} (from {}): {e}",
            root.display(),
            source.describe(),
        )));
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;

    fn clear_env() {
        std::env::remove_var("SHELBI_ROOT");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn cli_flag_overrides_env_root() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("SHELBI_ROOT", "/tmp/from-env-root");
        std::env::set_var("SHELBI_HOME", "/tmp/from-env-home");
        let (path, source) = resolve_with(Some(PathBuf::from("/tmp/from-flag")), "").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/from-flag"));
        assert_eq!(source, RootSource::CliFlag);
        clear_env();
    }

    #[test]
    fn cli_flag_expands_tilde() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        let (path, source) = resolve_with(Some(PathBuf::from("~/from-flag-tilde")), "").unwrap();
        let expected = dirs::home_dir().unwrap().join("from-flag-tilde");
        assert_eq!(path, expected);
        assert_eq!(source, RootSource::CliFlag);
        clear_env();
    }

    #[test]
    fn compile_time_default_wins_over_home_fallback() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        let (path, source) = resolve_with(None, "/tmp/baked-in").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/baked-in"));
        assert_eq!(source, RootSource::CompileTime);
        clear_env();
    }

    #[test]
    fn env_root_overrides_compile_time_default() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("SHELBI_ROOT", "/tmp/from-env");
        let (path, source) = resolve_with(None, "/tmp/baked-in").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/from-env"));
        assert_eq!(source, RootSource::EnvRoot);
        clear_env();
    }

    #[test]
    fn compile_time_default_tilde_expands() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        let (path, source) = resolve_with(None, "~/baked-tilde").unwrap();
        let expected = dirs::home_dir().unwrap().join("baked-tilde");
        assert_eq!(path, expected);
        assert_eq!(source, RootSource::CompileTime);
        clear_env();
    }

    #[test]
    fn env_root_overrides_env_home() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("SHELBI_ROOT", "/tmp/from-root");
        std::env::set_var("SHELBI_HOME", "/tmp/from-home");
        let (path, source) = resolve().unwrap();
        assert_eq!(path, PathBuf::from("/tmp/from-root"));
        assert_eq!(source, RootSource::EnvRoot);
        clear_env();
    }

    #[test]
    fn env_home_picked_when_root_unset() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("SHELBI_HOME", "/tmp/legacy");
        let (path, source) = resolve().unwrap();
        assert_eq!(path, PathBuf::from("/tmp/legacy"));
        assert_eq!(source, RootSource::EnvHome);
        clear_env();
    }

    #[test]
    fn empty_env_root_falls_through() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("SHELBI_ROOT", "");
        std::env::set_var("SHELBI_HOME", "/tmp/fallback-home");
        let (_, source) = resolve().unwrap();
        assert_eq!(source, RootSource::EnvHome);
        clear_env();
    }

    #[test]
    fn home_fallback_used_when_compile_time_default_empty() {
        // The test binary is built without the install-script prompt, so
        // COMPILE_TIME_DEFAULT is "" — assert resolution lands on the
        // home fallback when no env var is set.
        if !COMPILE_TIME_DEFAULT.is_empty() {
            return;
        }
        let _g = LOCK.lock().unwrap();
        clear_env();
        let (path, source) = resolve().unwrap();
        assert_eq!(source, RootSource::HomeFallback);
        let expected = dirs::home_dir().unwrap().join(".shelbi");
        assert_eq!(path, expected);
        clear_env();
    }

    #[test]
    fn tilde_expansion_in_env_root() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("SHELBI_ROOT", "~/scratch-shelbi");
        let (path, _) = resolve().unwrap();
        let expected = dirs::home_dir().unwrap().join("scratch-shelbi");
        assert_eq!(path, expected);
        clear_env();
    }

    #[test]
    fn tilde_expansion_in_env_home() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("SHELBI_HOME", "~/legacy-shelbi");
        let (path, _) = resolve().unwrap();
        let expected = dirs::home_dir().unwrap().join("legacy-shelbi");
        assert_eq!(path, expected);
        clear_env();
    }

    #[test]
    fn ensure_root_subdirs_materializes_standard_layout() {
        use std::os::unix::fs::PermissionsExt;
        let _g = LOCK.lock().unwrap();
        clear_env();
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-ensure-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SHELBI_ROOT", &tmp);
        let root = ensure_root_subdirs().unwrap();
        assert_eq!(root, tmp);
        for sub in STANDARD_SUBDIRS {
            assert!(
                tmp.join(sub).is_dir(),
                "expected {}/{sub} to exist",
                tmp.display(),
            );
        }
        // `ssh/` is intentionally NOT in STANDARD_SUBDIRS (different mode
        // than the others) but ensure_root_subdirs must still create it,
        // otherwise a fresh install can't dispatch to any remote host.
        let ssh_dir = tmp.join("ssh");
        assert!(ssh_dir.is_dir(), "expected ssh/ to exist");
        let mode = std::fs::metadata(&ssh_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected ssh/ to be 0700, got {mode:o}");
        // Second call is a no-op.
        ensure_root_subdirs().unwrap();
        clear_env();
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ensure_root_subdirs_hard_fails_on_unwritable_root() {
        let _g = LOCK.lock().unwrap();
        clear_env();
        // /dev/null is a non-directory; create_dir_all on it should fail
        // and surface the resolution source in the error message.
        std::env::set_var("SHELBI_ROOT", "/dev/null/cannot-be-a-shelbi-root");
        let err = ensure_root_subdirs().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/dev/null"), "{msg}");
        assert!(msg.contains("$SHELBI_ROOT"), "{msg}");
        clear_env();
    }
}
