//! Thin wrapper around `std::process::Command` that knows how to dispatch
//! either locally or over `ssh`.
//!
//! Why shell out to the host's `ssh` (instead of an in-process SSH crate
//! like `russh`): we want the user's existing `~/.ssh/config`, `ssh-agent`,
//! ProxyJump, etc. to "just work" — and we want one less thing to maintain.

use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Output, Stdio};

use shelbi_core::Host;

/// SSH connection-multiplexing options injected into every SSH-routed
/// command. With these set, the first invocation opens a master socket and
/// subsequent invocations reuse it — turning what would be a ~1s TCP +
/// TLS + auth handshake into a ~10ms write to a local Unix socket. The
/// sidebar polls workspaces every few seconds, so this is the difference
/// between "noticeable lag" and "imperceptible."
///
/// `ControlPath` uses `%C` (a hash of host+user+port) so distinct
/// destinations don't collide on the same socket. `ControlPersist=600`
/// keeps the master alive for 10 minutes after the last client closes,
/// which spans most idle gaps in a normal session.
///
/// `ConnectTimeout=5` bounds the worst case when a workspace host is dead
/// or routed through a slow proxy — the poller spawns one thread per
/// workspace so a hung connect only freezes that workspace's thread, but we
/// still want it to fail fast and try again on the next poll instead of
/// piling up an OS-level TCP retry sequence (minutes long, by default).
///
/// `BatchMode=yes` keeps ssh from blocking on an interactive password /
/// passphrase prompt that no one will ever answer (we run from the
/// sidebar's tmux pane, not a tty). Public-key + ssh-agent auth still
/// works; only interactive fallbacks are suppressed. NB: this does NOT
/// prevent Tailscale-SSH's web-auth interception — that flow runs
/// outside the openssh client and ignores BatchMode. Hung Tailscale
/// auths are bounded by the per-workspace thread design instead.
///
/// Users with their own `ControlMaster` configuration in `~/.ssh/config`
/// see our `-o` flags take precedence (command-line `-o` overrides config),
/// which is the right call — we know our access pattern (many short
/// commands) better than a generic per-host config does.
const SSH_CONTROL_OPTS: &[&str] = &[
    "-o",
    "ControlMaster=auto",
    "-o",
    "ControlPath=~/.ssh/shelbi-cm-%C",
    "-o",
    "ControlPersist=600",
    "-o",
    "ConnectTimeout=5",
    "-o",
    "BatchMode=yes",
];

fn apply_ssh_control_opts(cmd: &mut Command) {
    for opt in SSH_CONTROL_OPTS {
        cmd.arg(opt);
    }
}

/// Build (but do not execute) a `Command` that will run the given argv on
/// `host`. The argv is treated as a single command line for SSH (joined with
/// spaces, no shell escaping yet — callers are expected to pass pre-escaped
/// arguments for now).
pub fn build_command<I, S>(host: &Host, argv: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let argv: Vec<_> = argv.into_iter().collect();
    match host {
        Host::Local => {
            let (head, tail) = argv
                .split_first()
                .expect("build_command requires at least one argv element");
            let mut cmd = Command::new(head.as_ref());
            cmd.args(tail.iter().map(|s| s.as_ref()));
            cmd
        }
        Host::Ssh { host } => {
            let mut cmd = Command::new("ssh");
            apply_ssh_control_opts(&mut cmd);
            cmd.arg(host);
            cmd.arg("--");
            for a in &argv {
                cmd.arg(a.as_ref());
            }
            cmd
        }
    }
}

/// Build a command intended to run a *PTY-bound* program (e.g. `$EDITOR`,
/// `tmux attach`). Adds `-t` for SSH so the remote side gets a TTY.
pub fn build_pty_command<I, S>(host: &Host, argv: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let argv: Vec<_> = argv.into_iter().collect();
    match host {
        Host::Local => {
            let (head, tail) = argv
                .split_first()
                .expect("build_pty_command requires at least one argv element");
            let mut cmd = Command::new(head.as_ref());
            cmd.args(tail.iter().map(|s| s.as_ref()));
            cmd
        }
        Host::Ssh { host } => {
            let mut cmd = Command::new("ssh");
            apply_ssh_control_opts(&mut cmd);
            cmd.arg("-t");
            cmd.arg(host);
            cmd.arg("--");
            for a in &argv {
                cmd.arg(a.as_ref());
            }
            cmd
        }
    }
}

/// Run a command and return its captured output. Does not raise on non-zero
/// exit; callers inspect `Output::status`.
pub fn run<I, S>(host: &Host, argv: I) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = build_command(host, argv);
    tracing::debug!(?cmd, host = ?host, "ssh::run");
    cmd.output()
}

/// Run a command and return stdout as String on success, returning the
/// shelbi-core `Error::Command` variant on non-zero exit.
pub fn run_capture<I, S>(host: &Host, argv: I) -> shelbi_core::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let argv: Vec<_> = argv.into_iter().collect();
    let cmd_str = argv
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");

    let output = run(host, &argv).map_err(shelbi_core::Error::Io)?;
    if !output.status.success() {
        return Err(shelbi_core::Error::Command {
            cmd: cmd_str,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run a command with `stdin` piped in. Used to ferry payloads with
/// embedded newlines (e.g. `tmux load-buffer -`) without smuggling them
/// through argv, where the SSH wire would join args with single spaces
/// and the remote shell would re-parse newlines as command separators.
pub fn run_with_stdin<I, S>(host: &Host, argv: I, stdin: &[u8]) -> shelbi_core::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let argv: Vec<_> = argv.into_iter().collect();
    let cmd_str = argv
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");

    let mut cmd = build_command(host, &argv);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    tracing::debug!(?cmd, host = ?host, bytes = stdin.len(), "ssh::run_with_stdin");

    let mut child = cmd.spawn().map_err(shelbi_core::Error::Io)?;
    {
        let mut child_stdin = child
            .stdin
            .take()
            .expect("stdin was piped");
        child_stdin
            .write_all(stdin)
            .map_err(shelbi_core::Error::Io)?;
    }
    let output = child.wait_with_output().map_err(shelbi_core::Error::Io)?;
    if !output.status.success() {
        return Err(shelbi_core::Error::Command {
            cmd: cmd_str,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_command_args() {
        let cmd = build_command(&Host::Local, ["echo", "hi"]);
        assert_eq!(cmd.get_program(), "echo");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec!["hi"]);
    }

    #[test]
    fn ssh_command_args() {
        let cmd = build_command(
            &Host::Ssh {
                host: "m2.local".into(),
            },
            ["tmux", "new-session"],
        );
        assert_eq!(cmd.get_program(), "ssh");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // Control-master opts ride in front of every SSH invocation so
        // back-to-back hub→workspace commands reuse a single socket.
        let mut expected: Vec<String> = SSH_CONTROL_OPTS.iter().map(|s| s.to_string()).collect();
        expected.extend(["m2.local", "--", "tmux", "new-session"].map(String::from));
        assert_eq!(args, expected);
    }

    #[test]
    fn ssh_pty_command_uses_t_flag() {
        let cmd = build_pty_command(
            &Host::Ssh {
                host: "m2.local".into(),
            },
            ["vi", "foo.txt"],
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let mut expected: Vec<String> = SSH_CONTROL_OPTS.iter().map(|s| s.to_string()).collect();
        expected.extend(["-t", "m2.local", "--", "vi", "foo.txt"].map(String::from));
        assert_eq!(args, expected);
    }

    #[test]
    fn echo_runs_locally() {
        let out = run(&Host::Local, ["echo", "shelbi"]).expect("echo failed");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "shelbi");
    }

    #[test]
    fn run_with_stdin_pipes_payload_locally() {
        // `cat` echoes stdin to stdout — round-trips embedded newlines so
        // we know multi-line payloads survive the pipe end-to-end.
        let payload = "line one\nline two\nline three";
        let out = run_with_stdin(&Host::Local, ["cat"], payload.as_bytes())
            .expect("cat failed");
        assert_eq!(out, payload);
    }
}
