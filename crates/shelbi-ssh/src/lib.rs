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

/// Static fragment of the SSH connection-multiplexing options injected
/// into every SSH-routed command. With these set (combined with the
/// per-invocation ControlPath and reverse forward from
/// [`build_ssh_control_opts`]), the first invocation opens a master
/// socket and subsequent invocations reuse it — turning what would be
/// a ~1s TCP + TLS + auth handshake into a ~10ms write to a local Unix
/// socket. The sidebar polls workspaces every few seconds, so this is
/// the difference between "noticeable lag" and "imperceptible."
///
/// `ControlPersist=600` keeps the master alive for 10 minutes after
/// the last client closes, which spans most idle gaps in a normal
/// session.
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
const SSH_CONTROL_OPTS_STATIC: &[&str] = &[
    "-o",
    "ControlMaster=auto",
    "-o",
    "ControlPersist=600",
    "-o",
    "ConnectTimeout=5",
    "-o",
    "BatchMode=yes",
    // The ControlMaster opened on the first call inherits the `-R`
    // reverse forward; subsequent slave connections inherit the
    // multiplexed channel without re-requesting it. ExitOnForwardFailure=no
    // (the default) and LogLevel=ERROR keep duplicate-forward warnings
    // on slave reconnects from blocking the connection or polluting
    // the user's terminal — the only real "forwarding failed" case
    // worth surfacing is the master open, which falls through to the
    // command's regular stderr.
    "-o",
    "ExitOnForwardFailure=no",
    "-o",
    "LogLevel=ERROR",
];

/// The full set of `-o` options + the `-R` reverse forward for a
/// shelbi-routed `ssh` invocation. Built fresh per call so a SHELBI_HOME
/// or SHELBI_HUB_SOCK override picked up at process start lands in the
/// args without baking it into a const.
///
/// The reverse forward exposes the hub daemon's `~/.shelbi/hub.sock` to
/// the remote side as `/tmp/shelbi-hub.sock` (overridable via
/// `SHELBI_REMOTE_HUB_SOCK`). Remote workers — Phase 5 — write to that
/// path and the messages land in hub's events.log without an extra
/// outbound channel.
fn build_ssh_control_opts() -> Vec<String> {
    let mut opts: Vec<String> = SSH_CONTROL_OPTS_STATIC
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    // OpenSSH refuses to create the ControlPath's parent for us — a
    // missing `~/.shelbi/ssh/` surfaces as `unix_listener: cannot bind
    // to path …: No such file or directory` and the connection dies
    // before argv is transmitted. Materialize the directory (with 0700)
    // on every invocation; the call is cheap and idempotent, and it
    // rescues fresh installs and anyone who hand-cleaned `~/.shelbi/`.
    // Best-effort — if the helper errors out we still hand ssh the
    // ControlPath and let it surface its own diagnostic.
    let _ = shelbi_state::ensure_ssh_control_dir();
    // ControlPath under SHELBI_HOME so the hub's startup cleanup can
    // find these sockets without risking the user's hand-rolled CMs
    // under ~/.ssh/. Fall back to a sensible default if the helper
    // errors out (no $HOME, etc.) — better to start a fresh master per
    // call than to wedge the SSH path entirely.
    let cp = shelbi_state::ssh_control_path_template()
        .unwrap_or_else(|_| "~/.shelbi/ssh/%r@%h".to_string());
    opts.push("-o".into());
    opts.push(format!("ControlPath={cp}"));
    // Reverse forward: <remote>:<local>. Fall back to the in-spec
    // default pair on resolution failure so the connection still works
    // — the master just won't carry the forward this round.
    let rev = shelbi_state::reverse_forward_spec()
        .map(|os| os.to_string_lossy().into_owned())
        .ok();
    if let Some(spec) = rev {
        opts.push("-R".into());
        opts.push(spec);
    }
    opts
}

fn apply_ssh_control_opts(cmd: &mut Command) {
    for opt in build_ssh_control_opts() {
        cmd.arg(opt);
    }
}

/// Build (but do not execute) a `Command` that will run the given argv on
/// `host`.
///
/// Local dispatch hands each argv element straight to `exec` via
/// `std::process`, so no shell ever re-parses them. For `Host::Ssh` the
/// story is different: `ssh host -- a b c` joins the words after `--` with
/// single spaces into one command line and the *remote* login shell
/// re-tokenizes the result. So every SSH-routed argv element is passed
/// through [`shelbi_core::shell_escape`] first — that makes each element
/// survive the remote shell as exactly one literal word, giving the SSH
/// arm the same "argv is argv" semantics the local arm already has.
///
/// This closes F1/F2 from the process-boundaries review: an unquoted
/// `#{pane_title}` (comment-stripped by the remote shell) or a command
/// string containing `&&` / `;` / `$` / spaces no longer silently
/// re-parses on the far side. Callers must therefore pass *raw* argv and
/// must NOT pre-escape for the wire (see `orchestrator::git`).
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
                cmd.arg(escape_for_wire(a.as_ref()));
            }
            cmd
        }
    }
}

/// Shell-escape a single argv element for the SSH wire. Non-UTF-8 bytes are
/// carried through lossily — every argv shelbi builds is UTF-8 (tmux
/// targets, git refs, paths), and the alternative (refusing the byte) would
/// be worse than a replacement char in the rare pathological case.
fn escape_for_wire(a: &OsStr) -> String {
    shelbi_core::shell_escape(&a.to_string_lossy())
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
                cmd.arg(escape_for_wire(a.as_ref()));
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
        let mut expected: Vec<String> = build_ssh_control_opts();
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
        let mut expected: Vec<String> = build_ssh_control_opts();
        expected.extend(["-t", "m2.local", "--", "vi", "foo.txt"].map(String::from));
        assert_eq!(args, expected);
    }

    #[test]
    fn ssh_command_args_include_reverse_forward() {
        // Belt-and-suspenders pin on the Phase 4 behavior the hub
        // depends on: every outbound ssh command carries a `-R` flag
        // mapping the remote landing socket onto the hub's local
        // `hub.sock`. The master opened on the first call inherits the
        // forward; subsequent slaves multiplex over it.
        let cmd = build_command(
            &Host::Ssh {
                host: "m2.local".into(),
            },
            ["true"],
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let r_pos = args.iter().position(|a| a == "-R").expect("missing -R");
        let spec = &args[r_pos + 1];
        assert!(
            spec.starts_with("/tmp/shelbi-hub.sock:")
                || spec.starts_with(&format!("{}:", shelbi_state::remote_hub_socket_path().display())),
            "forward spec didn't start with remote socket path: {spec}",
        );
        // ControlPath lands under SHELBI_HOME so the hub's startup
        // cleanup can find these sockets.
        let cp_idx = args
            .iter()
            .position(|a| a.starts_with("ControlPath="))
            .expect("missing ControlPath");
        assert!(
            args[cp_idx].contains("/ssh/%r@%h"),
            "ControlPath didn't carry the %r@%h template: {}",
            args[cp_idx],
        );
    }

    #[test]
    fn echo_runs_locally() {
        let out = run(&Host::Local, ["echo", "shelbi"]).expect("echo failed");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "shelbi");
    }

    /// Extract the words `ssh` would join with spaces and send to the
    /// remote login shell: everything after the `--` separator in the argv
    /// `build_command` hands to the local `ssh` binary.
    fn remote_wire(host: &Host, argv: &[&str]) -> String {
        let cmd = build_command(host, argv);
        let parts: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let dd = parts
            .iter()
            .position(|a| a == "--")
            .expect("ssh argv is missing its `--` separator");
        parts[dd + 1..].join(" ")
    }

    #[test]
    fn ssh_argv_survives_remote_shell_byte_for_byte() {
        // F1/F2: args with spaces, comment markers, expansions, and command
        // separators must reach the remote program as distinct literal
        // words. We replay the exact wire ssh would emit through a local
        // `sh -c` (standing in for the remote login shell) and use
        // `printf '[%s]\n'` to bracket each received arg — proving both the
        // count and the bytes survive.
        let host = Host::Ssh {
            host: "devbox".into(),
        };
        let args = ["printf", "[%s]\n", "a b", "#{pane_title}", "x && y", "p;q", "$HOME"];
        let wire = remote_wire(&host, &args);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&wire)
            .output()
            .expect("sh -c failed to run");
        assert!(out.status.success(), "sh exited nonzero (wire: {wire})");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "[a b]\n[#{pane_title}]\n[x && y]\n[p;q]\n[$HOME]\n",
            "wire: {wire}",
        );
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
