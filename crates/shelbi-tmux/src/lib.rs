//! tmux abstractions: sessions, windows, send-keys, capture-pane.
//!
//! All operations route through `shelbi-ssh`, so the same code works on a
//! local tmux server or one reached via SSH.

use shelbi_core::{Host, Result, TmuxAddr};

/// Does a tmux session with this name exist on `host`?
pub fn has_session(host: &Host, name: &str) -> Result<bool> {
    let out = shelbi_ssh::run(host, ["tmux", "has-session", "-t", name])
        .map_err(shelbi_core::Error::Io)?;
    Ok(out.status.success())
}

/// Create a detached tmux session with one initial window. Caller is
/// expected to check `has_session` first if they don't want this to fail.
pub fn new_session(
    host: &Host,
    name: &str,
    window_name: &str,
    command: Option<&str>,
) -> Result<()> {
    let mut args: Vec<String> = vec![
        "tmux".into(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.into(),
        "-n".into(),
        window_name.into(),
    ];
    if let Some(cmd) = command {
        args.push(cmd.into());
    }
    shelbi_ssh::run_capture(host, &args)?;
    Ok(())
}

/// Create a new window inside an existing session.
pub fn new_window(
    host: &Host,
    session: &str,
    window_name: &str,
    command: Option<&str>,
) -> Result<TmuxAddr> {
    let mut args: Vec<String> = vec![
        "tmux".into(),
        "new-window".into(),
        "-d".into(),
        "-t".into(),
        format!("{session}:"),
        "-n".into(),
        window_name.into(),
    ];
    if let Some(cmd) = command {
        args.push(cmd.into());
    }
    shelbi_ssh::run_capture(host, &args)?;
    Ok(TmuxAddr {
        session: session.into(),
        window: window_name.into(),
    })
}

/// Kill a window in a session.
pub fn kill_window(host: &Host, addr: &TmuxAddr) -> Result<()> {
    shelbi_ssh::run_capture(host, ["tmux", "kill-window", "-t", &addr.target()])?;
    Ok(())
}

/// Buffer name used by [`send_line`] when routing multi-line payloads
/// through tmux's paste buffer. Namespaced so we don't collide with the
/// user's own buffers.
const PASTE_BUFFER: &str = "shelbi-send";

/// Send a string to the target's keyboard input, followed by Enter.
///
/// For single-line text we use `send-keys -l` so tmux treats it as
/// literal characters (avoiding key-name expansion like `C-c`) and Enter
/// is sent as a separate `Enter` keysym.
///
/// For multi-line text we instead stage the payload in a tmux paste
/// buffer and replay it with `paste-buffer -p`. `-p` wraps the content
/// in bracketed-paste markers so the receiving app (e.g. Claude) sees
/// one atomic paste rather than N individual Enter keypresses — which
/// matters over SSH, where send-key Enters arrive spaced out far enough
/// to defeat any heuristic paste detection. We also pipe the payload to
/// `load-buffer -` via stdin so embedded newlines don't get re-parsed by
/// the remote shell when ssh joins argv with single spaces.
pub fn send_line(host: &Host, addr: &TmuxAddr, text: &str) -> Result<()> {
    let target = addr.target();
    if text.contains('\n') {
        shelbi_ssh::run_with_stdin(
            host,
            ["tmux", "load-buffer", "-b", PASTE_BUFFER, "-"],
            text.as_bytes(),
        )?;
        shelbi_ssh::run_capture(
            host,
            [
                "tmux",
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                PASTE_BUFFER,
                "-t",
                &target,
            ],
        )?;
    } else {
        shelbi_ssh::run_capture(host, ["tmux", "send-keys", "-t", &target, "-l", text])?;
    }
    shelbi_ssh::run_capture(host, ["tmux", "send-keys", "-t", &target, "Enter"])?;
    Ok(())
}

/// Read the pane's current title, with the trailing newline trimmed. The
/// hub uses this to poll workers for state markers — claude's hooks write
/// `shelbi:<state>` to the title via OSC escapes (see
/// `shelbi-state::default_worker_settings.json`), and the parser in
/// `shelbi-state` peels the marker back off.
pub fn pane_title(host: &Host, addr: &TmuxAddr) -> Result<String> {
    let raw = shelbi_ssh::run_capture(
        host,
        [
            "tmux",
            "display-message",
            "-p",
            "-t",
            &addr.target(),
            "#{pane_title}",
        ],
    )?;
    Ok(raw.trim_end_matches(['\n', '\r']).to_string())
}

/// Capture the current visible content of the pane as plain text.
///
/// `-p` prints to stdout, `-J` joins wrapped lines.
pub fn capture(host: &Host, addr: &TmuxAddr) -> Result<String> {
    shelbi_ssh::run_capture(
        host,
        ["tmux", "capture-pane", "-p", "-J", "-t", &addr.target()],
    )
}

/// Capture including the scrollback. `-S -<lines>` includes `lines` lines of
/// history before the visible area.
pub fn capture_history(host: &Host, addr: &TmuxAddr, lines: usize) -> Result<String> {
    let start = format!("-{lines}");
    shelbi_ssh::run_capture(
        host,
        [
            "tmux",
            "capture-pane",
            "-p",
            "-J",
            "-S",
            &start,
            "-t",
            &addr.target(),
        ],
    )
}

#[cfg(test)]
mod tests {
    //! Structural tests: assert the argv we build for SSH-routed tmux is
    //! the right shape. We can't run live SSH in CI, but we can verify the
    //! command construction so the wire format doesn't silently drift.

    use super::*;

    fn ssh_args(cmd: std::process::Command) -> Vec<String> {
        let raw: Vec<String> = std::iter::once(cmd.get_program().to_string_lossy().into_owned())
            .chain(
                cmd.get_args()
                    .map(|a| a.to_string_lossy().into_owned()),
            )
            .collect();
        // For SSH-routed commands, strip the connection-multiplexing
        // options that shelbi-ssh prepends to every invocation. They're
        // an orthogonal concern (covered by shelbi-ssh's own tests) and
        // would otherwise force every structural test below to enumerate
        // them. Recognized by the leading `ssh` program; for local
        // commands we pass through unchanged.
        if raw.first().map(|s| s.as_str()) != Some("ssh") {
            return raw;
        }
        let mut out = Vec::with_capacity(raw.len());
        out.push(raw[0].clone());
        let mut i = 1;
        while i < raw.len() {
            if raw[i] == "-o" && i + 1 < raw.len() {
                let v = &raw[i + 1];
                if v.starts_with("ControlMaster=")
                    || v.starts_with("ControlPath=")
                    || v.starts_with("ControlPersist=")
                {
                    i += 2;
                    continue;
                }
            }
            out.push(raw[i].clone());
            i += 1;
        }
        out
    }

    #[test]
    fn local_send_keys_argv() {
        let cmd = shelbi_ssh::build_command(
            &Host::Local,
            [
                "tmux",
                "send-keys",
                "-t",
                "shelbi-myapp:w-x",
                "-l",
                "hello",
            ],
        );
        assert_eq!(
            ssh_args(cmd),
            vec![
                "tmux",
                "send-keys",
                "-t",
                "shelbi-myapp:w-x",
                "-l",
                "hello",
            ]
        );
    }

    #[test]
    fn remote_send_keys_argv() {
        let cmd = shelbi_ssh::build_command(
            &Host::Ssh {
                host: "m2.local".into(),
            },
            [
                "tmux",
                "send-keys",
                "-t",
                "shelbi-w-fix-login:agent",
                "-l",
                "hello",
            ],
        );
        // ssh m2.local -- tmux send-keys -t … -l hello
        assert_eq!(
            ssh_args(cmd),
            vec![
                "ssh",
                "m2.local",
                "--",
                "tmux",
                "send-keys",
                "-t",
                "shelbi-w-fix-login:agent",
                "-l",
                "hello",
            ]
        );
    }

    #[test]
    fn remote_new_session_argv() {
        // What `new_session` for a remote worker would build under the hood.
        let cmd = shelbi_ssh::build_command(
            &Host::Ssh {
                host: "m2.local".into(),
            },
            [
                "tmux",
                "new-session",
                "-d",
                "-s",
                "shelbi-w-fix-login",
                "-n",
                "agent",
                "cd /work/myapp/.shelbi/wt/fix-login && claude",
            ],
        );
        assert_eq!(
            ssh_args(cmd),
            vec![
                "ssh",
                "m2.local",
                "--",
                "tmux",
                "new-session",
                "-d",
                "-s",
                "shelbi-w-fix-login",
                "-n",
                "agent",
                "cd /work/myapp/.shelbi/wt/fix-login && claude",
            ]
        );
    }

    #[test]
    fn local_paste_buffer_argv() {
        // Multi-line payloads use `paste-buffer -p` so bracketed-paste
        // mode wraps the content and the receiving app treats it as one
        // atomic paste rather than N Enter keypresses.
        let cmd = shelbi_ssh::build_command(
            &Host::Local,
            [
                "tmux",
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                PASTE_BUFFER,
                "-t",
                "shelbi-myapp:w-x",
            ],
        );
        assert_eq!(
            ssh_args(cmd),
            vec![
                "tmux",
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                "shelbi-send",
                "-t",
                "shelbi-myapp:w-x",
            ]
        );
    }

    #[test]
    fn remote_load_buffer_argv() {
        // The payload itself is piped via stdin to `load-buffer -`, not
        // smuggled through argv — so this asserts only the command shape
        // and verifies `-` is the last positional (it reads from stdin).
        let cmd = shelbi_ssh::build_command(
            &Host::Ssh {
                host: "m2.local".into(),
            },
            ["tmux", "load-buffer", "-b", PASTE_BUFFER, "-"],
        );
        assert_eq!(
            ssh_args(cmd),
            vec![
                "ssh",
                "m2.local",
                "--",
                "tmux",
                "load-buffer",
                "-b",
                "shelbi-send",
                "-",
            ]
        );
    }

    #[test]
    fn remote_pane_title_argv() {
        // The hub-side worker poll uses display-message + #{pane_title}
        // to read the trailing `shelbi:<state>` marker. Make sure the
        // SSH-routed argv shape stays stable.
        let cmd = shelbi_ssh::build_command(
            &Host::Ssh {
                host: "m2.local".into(),
            },
            [
                "tmux",
                "display-message",
                "-p",
                "-t",
                "shelbi-w-fix-login:agent",
                "#{pane_title}",
            ],
        );
        assert_eq!(
            ssh_args(cmd),
            vec![
                "ssh",
                "m2.local",
                "--",
                "tmux",
                "display-message",
                "-p",
                "-t",
                "shelbi-w-fix-login:agent",
                "#{pane_title}",
            ]
        );
    }

    #[test]
    fn remote_capture_pane_argv() {
        let cmd = shelbi_ssh::build_command(
            &Host::Ssh {
                host: "m2.local".into(),
            },
            [
                "tmux",
                "capture-pane",
                "-p",
                "-J",
                "-t",
                "shelbi-w-fix-login:agent",
            ],
        );
        assert_eq!(
            ssh_args(cmd),
            vec![
                "ssh",
                "m2.local",
                "--",
                "tmux",
                "capture-pane",
                "-p",
                "-J",
                "-t",
                "shelbi-w-fix-login:agent",
            ]
        );
    }
}
