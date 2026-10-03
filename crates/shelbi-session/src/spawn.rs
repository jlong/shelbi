//! Spawning a session **detached**, so it outlives whatever launched it.
//!
//! A client (the orchestrator, a CLI, the daemon) calls [`spawn_detached`] to
//! start a `shelbi __session` process that keeps running after the caller — or a
//! launching `ssh host ...` — returns. Per the Phase 0 runtime spike's recipe:
//!
//! * **macOS / the portable fallback:** `setsid(2)` in a `pre_exec` hook (there
//!   is no `setsid(1)` on macOS) puts the session in its own session with no
//!   controlling terminal, so closing the launcher's terminal cannot SIGHUP it.
//! * **Linux with logind (opt-in):** `systemd-run --user --scope` (with user
//!   lingering enabled) lets the session survive logout, which `setsid` alone
//!   cannot (`KillUserProcesses=yes` reaps it). It is **opt-in** via
//!   `SHELBI_SESSION_SYSTEMD_SCOPE`, and even then only taken when a live
//!   `systemd --user` manager is reachable — never auto-selected. Auto-selecting
//!   it off a reachable manager is unsafe: a headless/CI host that still runs a
//!   user manager (a GitHub `ubuntu-latest` runner is exactly this) would take
//!   the scope path in tests, where it has caused both hangs and
//!   runner-killing process-group kills. Default everywhere — tests, CI, macOS —
//!   is the `setsid` recipe, which already satisfies "survives the launcher".
//!   The scope path is `setsid` too, so the returned pid is always a session
//!   leader (a caller's `killpg(getpgid(pid))` can never hit the launcher's
//!   group).
//! * **All platforms:** every stdio fd is redirected to `/dev/null`. That is
//!   what lets a launching `ssh` return — with the channel's fds at EOF, sshd
//!   stops waiting — and the session runs on.
//!
//! The session process is handed an **explicit environment**
//! ([`shelbi_core::session_child_env`]): it never inherits the launcher's.

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::layout::{derive_id_now, SessionPaths};

/// What to spawn: the readable name, the child command, and the session's shape.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    /// Readable session name (`<project>/ws/<workspace>`, …) recorded in
    /// `meta.json`.
    pub name: String,
    /// Working directory for the child.
    pub cwd: PathBuf,
    /// Initial screen size.
    pub cols: u16,
    /// Initial screen size.
    pub rows: u16,
    /// Task id this session serves, if any.
    pub task: Option<String>,
    /// Enable the full raw output log (the project's opt-in). Off by default.
    pub raw_output_log: bool,
    /// The child program and its arguments.
    pub child_argv: Vec<String>,
}

/// A spawned, detached session: enough to find and talk to it.
#[derive(Debug, Clone)]
pub struct SpawnedSession {
    /// The session's short directory id.
    pub id: String,
    /// `~/.shelbi/sessions/<id>/`.
    pub dir: PathBuf,
    /// The socket clients connect to.
    pub sock: PathBuf,
    /// The detached process's pid (the launcher's view; the session reparents to
    /// init immediately).
    pub pid: u32,
}

impl SpawnedSession {
    fn from(paths: &SessionPaths, pid: u32) -> Self {
        Self {
            id: paths.id.clone(),
            dir: paths.dir.clone(),
            sock: paths.sock(),
            pid,
        }
    }
}

/// Spawn `spec` as a detached `shelbi __session` process and return once it is
/// launched (without waiting for it to finish).
///
/// The session directory is created up front, and the socket path is checked
/// against the length limit before launch so a too-deep home is a clear error.
pub fn spawn_detached(spec: &SpawnSpec) -> Result<SpawnedSession> {
    let exe = std::env::current_exe().context("resolving the shelbi executable")?;
    spawn_detached_with_exe(&exe, spec)
}

/// Like [`spawn_detached`], but with the `shelbi` executable named explicitly.
///
/// Production code uses [`spawn_detached`], which finds the running `shelbi`
/// binary via `current_exe`. This variant lets a test point at a built binary
/// (`CARGO_BIN_EXE_shelbi`) instead of the test harness.
pub fn spawn_detached_with_exe(
    exe: &std::path::Path,
    spec: &SpawnSpec,
) -> Result<SpawnedSession> {
    if spec.child_argv.is_empty() {
        anyhow::bail!("cannot spawn a session with an empty child command");
    }
    let id = derive_id_now(&spec.name);
    let paths = SessionPaths::resolve(&id)?;
    paths.check_socket_fits()?;
    std::fs::create_dir_all(&paths.dir)
        .with_context(|| format!("creating session dir {}", paths.dir.display()))?;

    let session_args = build_session_args(&id, spec);

    let mut cmd = detached_command(exe, &session_args)?;
    // The session process gets the explicit, scrubbed login environment — never
    // the launcher's.
    cmd.env_clear();
    cmd.envs(shelbi_core::session_child_env());
    // Propagate only Shelbi's own state-root overrides, so the session resolves
    // the same `~/.shelbi` the launcher is operating under (honors `--root` /
    // `$SHELBI_HOME` and keeps tests isolated). Everything else stays scrubbed.
    for key in ["SHELBI_HOME", "SHELBI_ROOT"] {
        if let Some(val) = std::env::var_os(key) {
            cmd.env(key, val);
        }
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawning detached session {}", paths.id))?;
    Ok(SpawnedSession::from(&paths, child.id()))
}

/// The argv for the `shelbi __session` invocation (everything after the
/// executable path).
fn build_session_args(id: &str, spec: &SpawnSpec) -> Vec<String> {
    let mut args = vec![
        "__session".to_string(),
        "--id".to_string(),
        id.to_string(),
        "--name".to_string(),
        spec.name.clone(),
        "--cwd".to_string(),
        spec.cwd.to_string_lossy().into_owned(),
        "--cols".to_string(),
        spec.cols.to_string(),
        "--rows".to_string(),
        spec.rows.to_string(),
    ];
    if let Some(task) = &spec.task {
        args.push("--task".to_string());
        args.push(task.clone());
    }
    if spec.raw_output_log {
        args.push("--raw-log".to_string());
    }
    args.push("--".to_string());
    args.extend(spec.child_argv.iter().cloned());
    args
}

/// Build the `Command` with the platform detach recipe and stdio redirected to
/// `/dev/null`. The returned command still needs its environment set by the
/// caller.
///
/// Both recipes `setsid` the spawned process into its own session: that is what
/// makes the returned [`SpawnedSession::pid`] a session (and process-group)
/// leader, so a caller that reaps the session with `killpg(getpgid(pid))` can
/// never signal the launcher's own group by accident. (A missing `setsid` on
/// the `systemd-run` path once took a whole CI runner down this way.)
fn detached_command(exe: &std::path::Path, session_args: &[String]) -> Result<Command> {
    // On Linux a systemd user scope survives logout (`KillUserProcesses=yes`
    // won't reap it), which `setsid` alone cannot guarantee. But it is opt-in:
    // auto-selecting it off a reachable user manager is unsafe, because a
    // *headless* host that nonetheless has a user manager (a GitHub
    // `ubuntu-latest` runner is exactly this) would take the scope path in
    // tests and CI, where it has caused both hangs and runner-killing
    // process-group kills. The production spawner opts in on a real logind host
    // by setting `SHELBI_SESSION_SYSTEMD_SCOPE`; everything else — tests, CI,
    // macOS, the portable path — uses the always-safe `setsid` recipe, which
    // already satisfies "survives the launcher exiting".
    #[cfg(target_os = "linux")]
    if systemd_scope_opted_in() && systemd_user_manager_available() {
        if let Some(systemd_run) = find_on_path("systemd-run") {
            let mut cmd = Command::new(systemd_run);
            cmd.arg("--user")
                .arg("--scope")
                .arg("--quiet")
                .arg("--collect")
                .arg("--")
                .arg(exe)
                .args(session_args);
            redirect_stdio_to_null(&mut cmd)?;
            setsid_before_exec(&mut cmd);
            return Ok(cmd);
        }
    }

    let mut cmd = Command::new(exe);
    cmd.args(session_args);
    redirect_stdio_to_null(&mut cmd)?;
    setsid_before_exec(&mut cmd);
    Ok(cmd)
}

/// Install a `pre_exec` hook that puts the child in its own session before
/// `exec`, so closing the launcher's terminal cannot SIGHUP it and the returned
/// pid is a session/process-group leader.
fn setsid_before_exec(cmd: &mut Command) {
    // SAFETY: setsid() is async-signal-safe and touches no Rust allocator state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Whether the caller opted into the Linux `systemd-run --user --scope` detach
/// recipe by setting `SHELBI_SESSION_SYSTEMD_SCOPE` to a truthy value. Default
/// (unset) is the `setsid` recipe.
#[cfg(target_os = "linux")]
fn systemd_scope_opted_in() -> bool {
    env_is_truthy(std::env::var("SHELBI_SESSION_SYSTEMD_SCOPE").ok().as_deref())
}

/// Parse an opt-in env var: truthy for `1`/`true`/`yes`/`on` (case- and
/// whitespace-insensitive), false otherwise (including unset). Kept pure and
/// platform-agnostic so it is testable off the Linux-only call site.
#[cfg(any(target_os = "linux", test))]
fn env_is_truthy(val: Option<&str>) -> bool {
    matches!(
        val.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Point all three stdio fds at `/dev/null`, so a launching `ssh` sees EOF and
/// returns while the session runs on.
fn redirect_stdio_to_null(cmd: &mut Command) -> Result<()> {
    let devnull = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .context("opening /dev/null")?;
    cmd.stdin(Stdio::from(devnull.try_clone().context("dup /dev/null")?));
    cmd.stdout(Stdio::from(devnull.try_clone().context("dup /dev/null")?));
    cmd.stderr(Stdio::from(devnull));
    Ok(())
}

/// Find an executable on `$PATH` (used for the Linux `systemd-run` detection).
#[cfg(target_os = "linux")]
fn find_on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|candidate| candidate.is_file())
}

/// Is a `systemd --user` manager reachable, so that `systemd-run --user` will
/// actually work rather than fail or hang?
///
/// Takes the systemd path only when **both** hold:
///
/// 1. The user manager's private control socket (`$XDG_RUNTIME_DIR/systemd/private`)
///    exists. It is created only while `systemd --user` is running, so its
///    absence proves there is no user manager (a CI runner, a bare SSH login
///    with no lingering). This check is cheap and never blocks.
/// 2. A short-deadline `systemctl --user is-system-running` confirms the manager
///    actually answers — guarding against a stale socket left by a manager that
///    has since died. Any state other than `offline` counts as reachable; a
///    timeout or launch failure counts as unavailable.
///
/// When either fails we return `false` and the caller uses the always-safe
/// `setsid` fallback. The whole probe is bounded by the deadline below, so it
/// can never hang the spawn.
#[cfg(target_os = "linux")]
fn systemd_user_manager_available() -> bool {
    let has_private_socket = std::env::var_os("XDG_RUNTIME_DIR")
        .map(|dir| PathBuf::from(dir).join("systemd/private").exists())
        .unwrap_or(false);
    if !has_private_socket {
        return false;
    }
    match probe_user_manager_state(std::time::Duration::from_secs(2)) {
        Some(state) => state.trim() != "offline",
        None => false,
    }
}

/// Run `systemctl --user is-system-running` with a hard deadline, returning the
/// state it prints (`running`, `degraded`, `offline`, …) if it exits in time.
///
/// Returns `None` if the probe cannot be launched or does not exit before the
/// deadline (in which case it is killed and reaped), so a wedged user bus turns
/// into a fast "unavailable" verdict instead of a hang.
#[cfg(target_os = "linux")]
fn probe_user_manager_state(timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;

    let mut child = Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                return Some(out);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_args_carry_every_field() {
        let spec = SpawnSpec {
            name: "demo/ws/alpha".into(),
            cwd: PathBuf::from("/tmp/wt"),
            cols: 100,
            rows: 40,
            task: Some("fix-login".into()),
            raw_output_log: true,
            child_argv: vec!["claude".into(), "--continue".into()],
        };
        let args = build_session_args("abc123", &spec);
        assert_eq!(args[0], "__session");
        assert!(args.windows(2).any(|w| w == ["--id", "abc123"]));
        assert!(args.windows(2).any(|w| w == ["--name", "demo/ws/alpha"]));
        assert!(args.windows(2).any(|w| w == ["--cols", "100"]));
        assert!(args.windows(2).any(|w| w == ["--rows", "40"]));
        assert!(args.windows(2).any(|w| w == ["--task", "fix-login"]));
        assert!(args.contains(&"--raw-log".to_string()));
        // The child argv follows the `--` separator, in order.
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(&args[sep + 1..], &["claude", "--continue"]);
    }

    #[test]
    fn raw_log_flag_absent_by_default() {
        let spec = SpawnSpec {
            name: "demo/shell/alpha".into(),
            cwd: PathBuf::from("/tmp"),
            cols: 80,
            rows: 24,
            task: None,
            raw_output_log: false,
            child_argv: vec!["/bin/zsh".into()],
        };
        let args = build_session_args("id", &spec);
        assert!(!args.contains(&"--raw-log".to_string()));
        assert!(!args.iter().any(|a| a == "--task"));
    }

    #[test]
    fn env_truthy_parsing() {
        for v in ["1", "true", "TRUE", "Yes", " on ", "on"] {
            assert!(env_is_truthy(Some(v)), "{v:?} should be truthy");
        }
        for v in [None, Some(""), Some("0"), Some("false"), Some("no"), Some("maybe")] {
            assert!(!env_is_truthy(v), "{v:?} should be falsy");
        }
    }

    #[test]
    fn empty_child_argv_is_rejected() {
        let spec = SpawnSpec {
            name: "x".into(),
            cwd: PathBuf::from("/"),
            cols: 80,
            rows: 24,
            task: None,
            raw_output_log: false,
            child_argv: vec![],
        };
        assert!(spawn_detached(&spec).is_err());
    }
}
