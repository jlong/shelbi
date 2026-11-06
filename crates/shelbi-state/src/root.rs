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

use std::path::{Path, PathBuf};
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
    let resolved = resolve_with(ROOT_OVERRIDE.get().cloned(), COMPILE_TIME_DEFAULT)?;
    if in_test_binary() {
        return Ok(isolate_test_home(resolved, SHARED_TEST_HOME.get()));
    }
    Ok(resolved)
}

/// A test binary's own home, for when no test has mounted one
/// ([`mount_shared_test_home`]).
static SHARED_TEST_HOME: OnceLock<PathBuf> = OnceLock::new();

/// Give this test binary one throwaway home that every thread falls back to,
/// removed when the process exits. A no-op outside a test binary.
///
/// For a binary whose fixtures reach the home from background threads that
/// outlive any one test, so a per-test home can't cover them: the two UI clients
/// start the same jobs a real window does. Call it before main, from a load-time
/// constructor: a later call would leave earlier resolves to the guard, in an
/// order that changes from run to run. Every other binary gets no such fallback,
/// and a test with no home of its own fails.
pub fn mount_shared_test_home() {
    extern "C" fn remove() {
        if let Some(home) = SHARED_TEST_HOME.get() {
            let _ = std::fs::remove_dir_all(home);
        }
    }
    if in_test_binary() {
        let home = std::env::temp_dir().join(format!("shelbi-test-home-{}", std::process::id()));
        let _ = SHARED_TEST_HOME.set(home);
        // SAFETY: registers a plain function that captures nothing.
        unsafe { libc::atexit(remove) };
    }
}

/// Suite-wide guard against a test resolving the developer's **live** hub home.
///
/// A test that reaches [`resolve`] with no explicit root, or with one naming the
/// live home, would read and write the real hub state, or find the live
/// daemon's sockets under it and have that daemon write for it. That is how
/// fixture projects (`demo`, `proj`, `beta`, `test-project`) landed in a
/// developer's `~/.shelbi/projects`. Every test mounts a throwaway home first;
/// the guard turns a silent leak into an immediate, self-describing panic.
///
/// It runs in every crate's test binaries ([`in_test_binary`]), not only this
/// crate's, and never in a shipped binary. To exercise the fallback *logic*
/// without tripping the guard, call the pure [`resolve_with`] directly.
fn isolate_test_home(
    resolved: (PathBuf, RootSource),
    shared: Option<&PathBuf>,
) -> (PathBuf, RootSource) {
    let (path, source) = resolved;
    if is_live_home(&path) && !fell_through(source) {
        panic!(
            "tests must set SHELBI_HOME to a throwaway directory: the {} names the live hub home {}",
            source.describe(),
            path.display(),
        );
    }
    if !fell_through(source) {
        return (path, source);
    }
    match shared {
        Some(home) => (home.clone(), RootSource::EnvHome),
        None => panic!(
            "tests must set SHELBI_HOME to a throwaway directory (use fresh_home()): \
             this test resolved the live hub home {} from the {}",
            path.display(),
            source.describe(),
        ),
    }
}

/// Nothing named a root, so the resolver landed on the machine's default.
fn fell_through(source: RootSource) -> bool {
    matches!(source, RootSource::CompileTime | RootSource::HomeFallback)
}

/// Whether this process is a `cargo test` harness binary, in any crate.
///
/// `cfg(test)` can't answer this: it is per compilation, so it is off for this
/// crate when another crate's tests link it. The harness is recognised by where
/// cargo puts it instead ([`is_cargo_test_binary_path`]). Compiled out of
/// release builds, so nothing a user runs can ever take the test paths.
#[cfg(debug_assertions)]
pub(crate) fn in_test_binary() -> bool {
    static IN_TEST_BINARY: OnceLock<bool> = OnceLock::new();
    *IN_TEST_BINARY.get_or_init(|| {
        cfg!(test)
            || std::env::current_exe()
                .map(|exe| is_cargo_test_binary_path(&exe))
                .unwrap_or(false)
    })
}

#[cfg(not(debug_assertions))]
pub(crate) fn in_test_binary() -> bool {
    false
}

/// `<target>/<profile>/deps/<name>-<16 hex digits>`, where cargo builds a test
/// harness. A plain `cargo build` or `cargo run` binary runs from
/// `<target>/<profile>/<name>`, so it never matches.
#[cfg_attr(not(debug_assertions), allow(dead_code))]
fn is_cargo_test_binary_path(exe: &Path) -> bool {
    let in_deps = exe.parent().and_then(Path::file_name).is_some_and(|dir| dir == "deps");
    let hashed = exe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .is_some_and(|(_, hash)| hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()));
    in_deps && hashed
}

/// The home a daemon on this machine uses when nothing overrides it.
fn live_home() -> Option<PathBuf> {
    resolve_live_home(COMPILE_TIME_DEFAULT)
}

fn resolve_live_home(compile_time: &str) -> Option<PathBuf> {
    if !compile_time.is_empty() {
        return Some(expand_tilde_str(compile_time));
    }
    dirs::home_dir().map(|home| home.join(".shelbi"))
}

/// `~/.shelbi` by the password database, which a test can't move. Several swap
/// `$HOME` for a fixture's (so a spawned shell finds a stub `gh`), and the
/// developer's live hub is still there while they do.
fn account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: `getpwuid` returns null or a record valid until the next call on
    // this thread; the path is copied out before anything else runs.
    unsafe {
        let record = libc::getpwuid(libc::getuid());
        if record.is_null() || (*record).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*record).pw_dir);
        Some(Path::new(std::ffi::OsStr::from_bytes(dir.to_bytes())).join(".shelbi"))
    }
}

fn is_live_home(path: &Path) -> bool {
    [live_home(), account_home()]
        .into_iter()
        .flatten()
        .any(|live| path == live || same_existing_path(path, &live))
}

/// `a` and `b` are the same existing directory, seen through any symlink.
fn same_existing_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The socket path `var` overrides to, for [`crate::hub_socket_path`] and
/// [`crate::control_socket_path`]. A worker pane exports the live hub's socket
/// to its agents, so a `cargo test` run from one inherits it, and a test that
/// mounted its own home would still send to the live daemon. In a test binary
/// a socket beside the live home reads as unset: the test gets the socket in
/// its own home.
pub(crate) fn socket_override(var: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(var)?);
    let live_socket = in_test_binary() && path.parent().is_some_and(is_live_home);
    (!live_socket).then_some(path)
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
pub const STANDARD_SUBDIRS: &[&str] = &["projects", "sessions", "agents", "logs"];

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
        // An empty compile-time default (passed explicitly here) means neither
        // the install-time bake nor an env var supplied a root, so resolution
        // must land on the `~/.shelbi` fallback.
        let _g = LOCK.lock().unwrap();
        clear_env();
        // Exercise the fallback *logic* through the pure resolver: `resolve()`
        // itself is guarded under `cfg(test)` and would panic on an unset home
        // (that guard is what keeps tests off the live `~/.shelbi`), so the
        // fallback branch is only reachable via `resolve_with`.
        let (path, source) = resolve_with(None, "").unwrap();
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
    fn resolve_panics_when_no_home_is_set_under_test() {
        // The suite-wide guard: a test that reaches `resolve()` with no explicit
        // root would otherwise read/write the developer's live `~/.shelbi`.
        // In a test binary it must fail immediately with the fix instead — this
        // is exactly what keeps a fixture `test-project/` out of the real hub.
        //
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        let msg = resolve_panic_message();
        assert!(
            msg.contains("tests must set SHELBI_HOME"),
            "panic must name the fix, got: {msg:?}"
        );
    }

    /// What `resolve()` panics with under the guard, muted so the expected
    /// panic doesn't print a backtrace. Caught in-closure so the lock unwinds
    /// normally and never poisons the shared `LOCK`.
    fn resolve_panic_message() -> String {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(resolve);
        std::panic::set_hook(prev_hook);
        let err = result.expect_err("resolve() must panic under the guard");
        err.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| err.downcast_ref::<String>().cloned())
            .unwrap_or_default()
    }

    #[test]
    fn resolve_panics_when_an_env_var_names_the_live_home() {
        // A pane or shell that exports the live home must not satisfy the guard
        // just because the variable is set.
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_env();
        let live = live_home().unwrap();
        std::env::set_var("SHELBI_HOME", &live);
        let msg = resolve_panic_message();
        clear_env();
        assert!(msg.contains("tests must set SHELBI_HOME"), "{msg:?}");
        assert!(msg.contains(live.to_str().unwrap()), "panic must name the path: {msg:?}");
    }

    #[test]
    fn the_live_home_stays_live_while_a_test_swaps_home() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        const VAR: &str = "SHELBI_ROOT_TEST_SWAPPED_HOME";
        let account = account_home().unwrap();
        let saved = std::env::var_os("HOME");
        std::env::set_var("HOME", "/tmp/a-fixture-home");
        std::env::set_var(VAR, account.join("hub.sock"));
        let inherited = socket_override(VAR);
        std::env::remove_var(VAR);
        match saved {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(inherited, None, "the live hub's socket must not reach a test");
    }

    #[test]
    fn a_shared_test_home_catches_a_fall_through_but_not_a_named_root() {
        let shared = PathBuf::from("/tmp/shared-test-home");
        let (path, source) = isolate_test_home(
            (live_home().unwrap(), RootSource::HomeFallback),
            Some(&shared),
        );
        assert_eq!(path, shared);
        // Never a default source: `should_retire_units` reads that as a real install.
        assert_eq!(source, RootSource::EnvHome);
        let named = (PathBuf::from("/tmp/a-test-home"), RootSource::EnvHome);
        assert_eq!(isolate_test_home(named.clone(), Some(&shared)), named);
    }

    #[test]
    fn the_live_home_is_recognised_through_a_symlink() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = std::env::temp_dir().join(format!("shelbi-root-symlink-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let real = tmp.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(same_existing_path(&link, &real));
        assert!(!same_existing_path(&link, &tmp));
        assert!(!same_existing_path(&tmp.join("missing"), &tmp.join("missing")));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_compile_time_default_is_the_live_home_when_baked_in() {
        assert_eq!(resolve_live_home("/opt/shelbi"), Some(PathBuf::from("/opt/shelbi")));
        assert_eq!(resolve_live_home(""), dirs::home_dir().map(|h| h.join(".shelbi")));
    }

    #[test]
    fn only_a_hashed_binary_under_deps_is_a_test_harness() {
        let is = |p: &str| is_cargo_test_binary_path(Path::new(p));
        // Every crate's unit and integration harness, in any target layout.
        assert!(is("/w/target/debug/deps/shelbi_tui-0123456789abcdef"));
        assert!(is("/w/target/debug/deps/shelbi-0123456789abcdef"));
        assert!(is("/w/target/aarch64-apple-darwin/debug/deps/remote_sessions-fedcba9876543210"));
        // What a user or `cargo run` runs.
        assert!(!is("/w/target/debug/shelbi"));
        assert!(!is("/opt/homebrew/bin/shelbi"));
        assert!(!is("/Applications/Shelbi.app/Contents/MacOS/shelbi-desktop"));
        // Near misses: a binary that merely lives in a dir called `deps`, or
        // is hyphenated like a hash but isn't one.
        assert!(!is("/w/deps/shelbi"));
        assert!(!is("/w/deps/shelbi-desktop"));
        assert!(!is("/w/target/debug/shelbi-0123456789abcdef"));
    }

    #[test]
    fn a_socket_override_beside_the_live_home_reads_as_unset_under_test() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        const VAR: &str = "SHELBI_ROOT_TEST_SOCKET_OVERRIDE";
        std::env::set_var(VAR, live_home().unwrap().join("hub.sock"));
        assert_eq!(socket_override(VAR), None, "the live hub's socket must not reach a test");
        std::env::set_var(VAR, "/tmp/sandbox/hub.sock");
        assert_eq!(socket_override(VAR), Some(PathBuf::from("/tmp/sandbox/hub.sock")));
        std::env::remove_var(VAR);
        assert_eq!(socket_override(VAR), None);
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
