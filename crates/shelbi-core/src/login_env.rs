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

/// Process-lifetime cache for [`login_shell_env`].
static CACHE: OnceLock<BTreeMap<String, String>> = OnceLock::new();

/// Hard deadline for the login-shell capture. An interactive login shell on a
/// headless host (a CI runner, for one) can wedge — a slow rc, a prompt with no
/// tty, or a background process started by the rc that inherits our capture pipe
/// and never closes it, which would otherwise leave us blocked reading stdout
/// *forever* even after the shell itself exits. The capture must never take
/// longer than this.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// The user's interactive login-shell environment, captured once and cached.
///
/// Returns an empty map if the login shell can't be run or exits non-zero;
/// callers overlay the result, so an empty map simply leaves their existing
/// environment untouched rather than clobbering it.
pub fn login_shell_env() -> &'static BTreeMap<String, String> {
    CACHE.get_or_init(capture_login_shell_env)
}

/// Run `$SHELL -l -i -c env` and parse its output. `$SHELL` falls back to
/// `/bin/sh` when unset. stdin is `/dev/null` so an interactive shell can't
/// block waiting for input, and stderr is discarded so prompt/rc noise never
/// reaches the caller's output.
///
/// Returns an empty map on any failure *or* if the capture exceeds
/// [`CAPTURE_TIMEOUT`]; callers overlay the result, so an empty map leaves their
/// environment untouched rather than clobbering it, and the process is never
/// left hanging on a wedged shell.
fn capture_login_shell_env() -> BTreeMap<String, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    capture_env_bounded(&shell, CAPTURE_TIMEOUT).unwrap_or_default()
}

/// Run `shell -l -i -c env` with a hard deadline and parse its output.
///
/// Why this isn't a plain [`Command::output`]: `output()` reads stdout to EOF,
/// and EOF only arrives once *every* writer of the pipe is closed. An
/// interactive login shell's rc can start a background process that inherits the
/// pipe and outlives the shell, so `output()` could block forever even after the
/// shell exits. Instead we:
///
/// * put the shell in its own process group (so job-control noise and any
///   children stay isolated from ours),
/// * read stdout on a detached thread that hands the bytes back over a channel,
///   and
/// * wait on that channel with a deadline — on timeout we kill the shell and
///   give up with an empty map rather than block.
fn capture_env_bounded(shell: &str, timeout: Duration) -> Option<BTreeMap<String, String>> {
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
/// * put the child in its own process group (so job-control noise and any
///   children stay isolated from ours),
/// * read stdout on a detached thread that hands the bytes back over a channel,
///   and
/// * wait on that channel with a deadline — on timeout we kill the child and
///   give up rather than block.
///
/// `stdin`/`stderr` are forced to `/dev/null` (an interactive shell can't block
/// on input, and rc/prompt noise never reaches us); stdout is captured.
fn bounded_capture_stdout(mut cmd: Command, timeout: Duration) -> Option<String> {
    use std::os::unix::process::CommandExt;

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A fresh process group (pgid = child pid): the child can't touch our
        // group, and anything it spawns is contained with it.
        .process_group(0)
        .spawn()
        .ok()?;

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
    build_session_env(login_shell_env().clone())
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
    fn rejects_non_name_keys() {
        assert!(split_assignment("=value").is_none());
        assert!(split_assignment("9bad=value").is_none());
        assert!(split_assignment("has space=value").is_none());
        assert!(split_assignment("no-equals").is_none());
        assert_eq!(split_assignment("OK_1=v"), Some(("OK_1", "v")));
    }
}
