//! Capture the user's interactive login-shell environment.
//!
//! The daemon (and, per `rt-session-process`, every session process) must run
//! git, `gh`, SSH, and workflow actions with the user's real environment, not
//! the minimal one a detached spawn or an OS supervisor hands it. `.zshrc` is
//! where nvm, fnm, and Homebrew PATH setup usually live, and a plain `-l -c`
//! skips it, so the capture uses the interactive form `$SHELL -l -i -c env`.
//!
//! The result is captured once and cached for the process lifetime: a daemon
//! captures it at startup and overlays it onto its own environment, and a
//! session spawner reads the same map to build the child's explicit
//! environment. The parse is split out as a pure function so it is testable
//! without a shell.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Process-lifetime cache for a **successful** [`login_shell_env`] capture.
///
/// Only a capture that actually ran the login shell is stored. A failed or
/// timed-out capture is deliberately *not* cached, so a later call retries it
/// rather than being stuck with the fallback environment for the rest of the
/// process's life (`rt-login-env-capture-empty-path`: a single early timeout
/// used to poison every subsequent spawn from that process).
static CACHE: OnceLock<BTreeMap<String, String>> = OnceLock::new();

/// Hard deadline for the login-shell capture. An interactive login shell on a
/// headless host (a CI runner, for one) can wedge — a slow rc, a prompt with no
/// tty, or a background process started by the rc that inherits our capture pipe
/// and never closes it, which would otherwise leave us blocked reading stdout
/// *forever* even after the shell itself exits. The capture must never take
/// longer than this.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// The user's interactive login-shell environment.
///
/// A **successful** capture is taken once and cached for the process lifetime.
/// If the capture fails or times out, this returns the launcher's own
/// environment as a fallback (see [`fallback_env`]) and does **not** cache it,
/// so the next call retries the capture.
///
/// It never returns an empty map off a failure: callers like the session
/// spawner `env_clear()` and then apply this map wholesale, so an empty result
/// would hand the child *no* `PATH` and the agent (`claude`, `codex`, …) would
/// fail to launch with `command not found`.
pub fn login_shell_env() -> BTreeMap<String, String> {
    resolve_login_env(&CACHE, || {
        capture_env_bounded(&login_shell(), CAPTURE_TIMEOUT)
    })
}

/// The cache-or-fallback policy behind [`login_shell_env`], with the cache and
/// the capture both injected so it is testable without a real shell or the
/// process-global [`CACHE`].
///
/// * A cached (previously successful) capture is returned straight away.
/// * Otherwise `capture` runs. On success the result is cached and returned. On
///   failure it is **not** cached — so the next call retries — and the
///   launcher's own environment is returned as a non-empty fallback, with a
///   warning naming the capture timeout.
fn resolve_login_env<F>(cache: &OnceLock<BTreeMap<String, String>>, capture: F) -> BTreeMap<String, String>
where
    F: FnOnce() -> Option<BTreeMap<String, String>>,
{
    if let Some(cached) = cache.get() {
        return cached.clone();
    }
    match capture() {
        Some(env) => {
            // Store the first successful capture. A concurrent racer may have set
            // it already (then `set` is a no-op); either way the stored map wins.
            let _ = cache.set(env);
            cache.get().cloned().unwrap_or_else(fallback_env)
        }
        None => {
            tracing::warn!(
                "interactive login-shell env capture failed or timed out after {:?}; \
                 falling back to the launcher's own environment",
                CAPTURE_TIMEOUT
            );
            fallback_env()
        }
    }
}

/// `$SHELL`, or `/bin/sh` when it is unset.
fn login_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// The launcher's own environment, used as the fallback when the login-shell
/// capture fails or times out.
///
/// The launcher (the TUI, the daemon, a CLI) was itself started from a shell, so
/// its environment carries at least `PATH`/`HOME`/`USER`/`SHELL`/`LANG` — enough
/// for the spawned agent to find its binary. This is strictly better than the
/// empty map a failed capture used to yield, which left a child with no `PATH`.
fn fallback_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

/// Run `shell -l -i -c env` with a hard deadline and parse its output.
///
/// Why this isn't a plain [`Command::output`]: `output()` reads stdout to EOF,
/// and EOF only arrives once *every* writer of the pipe is closed. An
/// interactive login shell's rc can start a background process that inherits the
/// pipe and outlives the shell, so `output()` could block forever even after the
/// shell exits. Instead we:
///
/// * put the shell in its own **session** with no controlling terminal (so
///   job-control noise and any children stay isolated from ours, *and* the
///   interactive shell never stops on SIGTTOU/SIGTTIN — see
///   [`bounded_capture_stdout`]),
/// * read stdout on a detached thread that hands the bytes back over a channel,
///   and
/// * wait on that channel with a deadline — on timeout we kill the shell and
///   give up with an empty map rather than block.
///
/// Exposed (not private) so a controlling-tty regression test can drive it
/// directly from a process that owns a pty.
pub fn capture_env_bounded(shell: &str, timeout: Duration) -> Option<BTreeMap<String, String>> {
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", "env"]);
    bounded_capture_stdout(cmd, timeout).map(|out| parse_env_output(&out))
}

/// Run `cmd` to completion with a hard deadline and return its stdout if it
/// exits successfully in time.
///
/// Why this isn't a plain [`Command::output`]: `output()` reads stdout to EOF,
/// and EOF only arrives once *every* writer of the pipe is closed. An
/// interactive login shell's rc can start a background process that inherits the
/// pipe and outlives the shell, so `output()` could block forever even after the
/// shell exits. Instead we:
///
/// * put the child in its own **session** with no controlling terminal (so
///   job-control noise and any children stay isolated from ours, and the shell
///   never stops fighting over a tty it should not see),
/// * read stdout on a detached thread that hands the bytes back over a channel,
///   and
/// * wait on that channel with a deadline — on timeout we kill the child and
///   give up rather than block.
///
/// `stdin`/`stderr` are forced to `/dev/null` (an interactive shell can't block
/// on input, and rc/prompt noise never reaches us); stdout is captured.
fn bounded_capture_stdout(mut cmd: Command, timeout: Duration) -> Option<String> {
    use std::os::unix::process::CommandExt;

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // `setsid`, not a plain `process_group(0)`: the capture shell must be
    // detached from any **controlling terminal**, not merely isolated into its
    // own group. When the launcher owns a controlling tty (the single-process
    // TUI opening a project), a `process_group(0)` child is a *background* group
    // on that tty, so the moment the interactive (`-i`) shell's rc touches
    // terminal modes it takes SIGTTOU/SIGTTIN and stops — wedging the capture
    // until the deadline and leaving every later spawn with an empty env
    // (`rt-login-env-capture-empty-path`). `setsid` gives the shell a brand-new
    // session with no controlling tty, so those signals never fire; it also
    // subsumes the group isolation (the child becomes session *and* group
    // leader, pgid == pid), matching the detached-spawn recipe elsewhere.
    //
    // SAFETY: `setsid(2)` is async-signal-safe and touches no shared state; it
    // runs in the forked child before exec. A freshly forked child is never
    // already a process-group leader, so the call does not fail in practice (and
    // its result is ignored regardless).
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().ok()?;

    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });

    // Wait for the child to exit, bounded by the deadline.
    let deadline = Instant::now() + timeout;
    let exited_ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() >= deadline => {
                // Wedged: kill the child and bail. (We can't reach a lingering
                // grandchild without libc, but the read below is bounded so we
                // never block on the pipe it may still hold.)
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break false,
        }
    };

    if !exited_ok {
        return None;
    }

    // The child exited cleanly; collect its output, but never block past the
    // deadline (a lingering grandchild could still hold the pipe open).
    let grace = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(500));
    rx.recv_timeout(grace).ok()
}

/// Terminal-identity variables scrubbed from a session child's environment.
///
/// A `shelbi __session` process owns its own fresh PTY, so any multiplexer or
/// terminal identity inherited through the login-shell capture (or leaked from
/// whatever launched the spawner) would be a lie to the child. Per the
/// remove-tmux plan's "Spawn" section these are stripped before Shelbi sets its
/// own terminal variables.
pub const SCRUBBED_TERMINAL_VARS: &[&str] = &["TMUX", "TMUX_PANE", "TERM_PROGRAM", "STY"];

/// Build the **explicit** environment for a `shelbi __session` child.
///
/// The session process never inherits the environment of whatever launched it;
/// it is handed this map instead. The map is the user's interactive
/// login-shell environment ([`login_shell_env`]) with:
///
/// * the terminal-identity variables in [`SCRUBBED_TERMINAL_VARS`] removed, and
/// * Shelbi's own terminal identity set: `TERM=xterm-256color`,
///   `COLORTERM=truecolor`, `TERM_PROGRAM=shelbi`.
///
/// No custom terminfo is used (`xterm-256color` is present everywhere), so there
/// is nothing to install on a remote host. This is the shared helper the daemon
/// and the session spawner both build the child environment from.
pub fn session_child_env() -> BTreeMap<String, String> {
    build_session_env(login_shell_env())
}

/// The pure transform [`session_child_env`] applies to a captured environment:
/// scrub the terminal-identity variables, then set Shelbi's own. Split out from
/// the shell capture so it is testable without running a shell.
pub fn build_session_env(mut env: BTreeMap<String, String>) -> BTreeMap<String, String> {
    for key in SCRUBBED_TERMINAL_VARS {
        env.remove(*key);
    }
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    env.insert("TERM_PROGRAM".to_string(), "shelbi".to_string());
    env
}

/// Parse the newline-separated `KEY=VALUE` output of `env` into a map.
///
/// A line that starts with a valid shell variable name followed by `=` begins
/// a new variable; any other line is treated as a continuation of the previous
/// variable's value (environment values may contain embedded newlines), joined
/// back with `\n`. Lines before the first valid assignment are ignored.
pub fn parse_env_output(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut current: Option<String> = None;
    // `.lines()` strips the line terminator and does not yield a spurious
    // trailing empty element for a trailing newline (which would otherwise fold
    // a stray `\n` onto the last value).
    for line in text.lines() {
        match split_assignment(line) {
            Some((key, value)) => {
                map.insert(key.to_string(), value.to_string());
                current = Some(key.to_string());
            }
            None => {
                if let Some(key) = &current {
                    if let Some(existing) = map.get_mut(key) {
                        existing.push('\n');
                        existing.push_str(line);
                    }
                }
            }
        }
    }
    map
}

/// Split `KEY=VALUE` when `KEY` is a valid shell variable name
/// (`[A-Za-z_][A-Za-z0-9_]*`). Returns `None` for anything else, including a
/// bare word, a continuation line, or a `=value` with no name.
fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let eq = line.find('=')?;
    let (key, rest) = line.split_at(eq);
    let value = &rest[1..];
    if is_valid_name(key) {
        Some((key, value))
    } else {
        None
    }
}

/// Whether `s` is a non-empty shell variable name.
fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_assignments() {
        let map = parse_env_output("PATH=/usr/bin:/bin\nHOME=/home/dev\nLANG=en_US.UTF-8\n");
        assert_eq!(map.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
        assert_eq!(map.get("HOME").map(String::as_str), Some("/home/dev"));
        assert_eq!(map.get("LANG").map(String::as_str), Some("en_US.UTF-8"));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn value_may_contain_equals() {
        let map = parse_env_output("LS_COLORS=di=34:ln=36\n");
        assert_eq!(map.get("LS_COLORS").map(String::as_str), Some("di=34:ln=36"));
    }

    #[test]
    fn continuation_lines_fold_into_the_previous_value() {
        // A multi-line value: the second physical line has no `KEY=` prefix,
        // so it belongs to the previous variable.
        let map = parse_env_output("GREETING=line one\nline two\nPATH=/bin\n");
        assert_eq!(
            map.get("GREETING").map(String::as_str),
            Some("line one\nline two")
        );
        assert_eq!(map.get("PATH").map(String::as_str), Some("/bin"));
    }

    #[test]
    fn empty_value_is_kept() {
        let map = parse_env_output("EMPTY=\nX=1\n");
        assert_eq!(map.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(map.get("X").map(String::as_str), Some("1"));
    }

    #[test]
    fn leading_noise_before_first_assignment_is_ignored() {
        // A shell that prints a banner before `env` output must not corrupt
        // the map or panic.
        let map = parse_env_output("welcome banner\nPATH=/bin\n");
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("PATH").map(String::as_str), Some("/bin"));
    }

    #[test]
    fn session_env_scrubs_terminal_identity_and_sets_shelbi_vars() {
        let mut base = BTreeMap::new();
        base.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
        base.insert("TMUX".to_string(), "/tmp/tmux-501/default,1,0".to_string());
        base.insert("TMUX_PANE".to_string(), "%3".to_string());
        base.insert("STY".to_string(), "12345.pts-0.host".to_string());
        base.insert("TERM_PROGRAM".to_string(), "iTerm.app".to_string());
        base.insert("TERM".to_string(), "screen-256color".to_string());

        let env = build_session_env(base);

        // Multiplexer / terminal identity scrubbed.
        assert!(!env.contains_key("TMUX"));
        assert!(!env.contains_key("TMUX_PANE"));
        assert!(!env.contains_key("STY"));
        // Shelbi's own terminal identity set (overriding any inherited value).
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
        assert_eq!(env.get("COLORTERM").map(String::as_str), Some("truecolor"));
        assert_eq!(env.get("TERM_PROGRAM").map(String::as_str), Some("shelbi"));
        // Unrelated variables survive untouched.
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
    }

    #[test]
    fn bounded_capture_returns_stdout_for_a_quick_command() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo PATH=/usr/bin:/bin"]);
        let out = bounded_capture_stdout(cmd, Duration::from_secs(5));
        assert_eq!(out.as_deref().map(str::trim), Some("PATH=/usr/bin:/bin"));
    }

    #[test]
    fn bounded_capture_does_not_hang_when_a_background_child_holds_the_pipe() {
        // The shell prints its output and exits, but leaves a long-lived
        // background process that inherited the stdout pipe. A plain
        // `Command::output()` would block on EOF for the full `sleep` here; the
        // bounded capture must return promptly instead (this is the exact
        // 35-minute CI-hang shape the timeout guards against).
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo X=1; sleep 120 &"]);
        let start = Instant::now();
        let out = bounded_capture_stdout(cmd, Duration::from_secs(2));
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "capture must not block on the lingering pipe holder (took {elapsed:?})"
        );
        // Whatever it returns (the line, or nothing after the deadline), it must
        // not have hung. If it did return output, it must be the printed line.
        if let Some(text) = out {
            assert!(text.contains("X=1"), "unexpected capture output: {text:?}");
        }
    }

    #[test]
    fn bounded_capture_reports_failure_for_a_nonzero_exit() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exit 3"]);
        assert_eq!(bounded_capture_stdout(cmd, Duration::from_secs(5)), None);
    }

    #[test]
    fn fallback_env_carries_the_launchers_path() {
        // The launcher always has a PATH; the fallback must forward it verbatim so
        // a spawned child (which `env_clear()`s and applies this map) is never left
        // with no PATH when the capture fails. Read the process PATH rather than
        // mutating it — the env is process-global and other tests run in parallel.
        let want = std::env::var("PATH").expect("the test process has a PATH");
        let fb = fallback_env();
        assert_eq!(fb.get("PATH"), Some(&want));
        assert!(!fb.is_empty());
    }

    #[test]
    fn a_forced_capture_failure_still_hands_a_child_a_nonempty_path() {
        // Criterion: a forced capture failure must not clobber the child env. The
        // session child env is `build_session_env(login_shell_env())`; on failure
        // `login_shell_env` returns the fallback, so the child still gets the
        // launcher's PATH rather than an empty one.
        let want = std::env::var("PATH").expect("the test process has a PATH");
        let cache = OnceLock::new();
        let env = resolve_login_env(&cache, || None);
        let child = build_session_env(env);
        assert_eq!(
            child.get("PATH"),
            Some(&want),
            "a failed capture must fall back to the launcher's PATH, not an empty map"
        );
    }

    #[test]
    fn a_failed_capture_is_not_cached_and_is_retried_until_it_succeeds() {
        use std::cell::Cell;

        let cache: OnceLock<BTreeMap<String, String>> = OnceLock::new();
        let calls = Cell::new(0usize);

        // First call: the capture fails. Result is the (non-empty) fallback, and
        // nothing is cached.
        let first = resolve_login_env(&cache, || {
            calls.set(calls.get() + 1);
            None
        });
        assert!(!first.is_empty(), "fallback env is non-empty");
        assert!(cache.get().is_none(), "a failed capture must not be cached");

        // Second call: the capture now succeeds. It was *retried* (ran again),
        // its result is returned, and it is cached.
        let mut good = BTreeMap::new();
        good.insert("PATH".to_string(), "/captured/bin".to_string());
        let second = resolve_login_env(&cache, || {
            calls.set(calls.get() + 1);
            Some(good.clone())
        });
        assert_eq!(second.get("PATH").map(String::as_str), Some("/captured/bin"));
        assert!(cache.get().is_some(), "a successful capture is cached");
        assert_eq!(calls.get(), 2, "the capture is retried after a failure");

        // Third call: served from the cache; the capture is not run again.
        let third = resolve_login_env(&cache, || {
            calls.set(calls.get() + 1);
            panic!("must not re-capture once a capture has succeeded");
        });
        assert_eq!(third.get("PATH").map(String::as_str), Some("/captured/bin"));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn rejects_non_name_keys() {
        assert!(split_assignment("=value").is_none());
        assert!(split_assignment("9bad=value").is_none());
        assert!(split_assignment("has space=value").is_none());
        assert!(split_assignment("no-equals").is_none());
        assert_eq!(split_assignment("OK_1=v"), Some(("OK_1", "v")));
    }
}
