//! tmux abstractions: sessions, windows, send-keys, capture-pane.
//!
//! All operations route through `shelbi-ssh`, so the same code works on a
//! local tmux server or one reached via SSH.

use std::sync::atomic::{AtomicU64, Ordering};

use shelbi_core::{Error, Host, Result, TmuxAddr};

/// Wrap a tmux `-t` target in the `=` exact-match prefix.
///
/// tmux resolves a bare `-t NAME` by trying, in order: exact match, then
/// unique *prefix*, then fnmatch. That prefix step is a footgun for us:
/// with sibling workspaces `bob` and `bob-2`, a torn-down `shelbi-w-bob`
/// prefix-matches the still-live `shelbi-w-bob-2`, so `has-session`
/// reports the dead session as alive and a later `send-keys` would paste
/// one agent's prompt into another agent's pane. A leading `=` forces an
/// exact match and disables the prefix/fnmatch fallbacks (verified
/// against tmux 3.6). Every `-t` we build for a session or window routes
/// through here.
fn exact(target: &str) -> String {
    format!("={target}")
}

// Argv builders, kept pure so the tests can assert the exact wire shape
// (in particular the `=` exact-match target and the `--` flag terminator)
// without a live tmux server — the same doctrine as this file's existing
// structural tests.

fn has_session_argv(name: &str) -> Vec<String> {
    vec!["tmux".into(), "has-session".into(), "-t".into(), exact(name)]
}

fn kill_window_argv(addr: &TmuxAddr) -> Vec<String> {
    vec![
        "tmux".into(),
        "kill-window".into(),
        "-t".into(),
        exact(&addr.target()),
    ]
}

/// Fast-path (local, single-line) send. `--` terminates flag parsing so a
/// payload beginning with `-` is delivered verbatim (F5).
fn send_keys_literal_argv(addr: &TmuxAddr, text: &str) -> Vec<String> {
    vec![
        "tmux".into(),
        "send-keys".into(),
        "-t".into(),
        exact(&addr.target()),
        "-l".into(),
        "--".into(),
        text.into(),
    ]
}

fn load_buffer_argv(buffer: &str) -> Vec<String> {
    vec![
        "tmux".into(),
        "load-buffer".into(),
        "-b".into(),
        buffer.into(),
        "-".into(),
    ]
}

fn paste_buffer_argv(buffer: &str, addr: &TmuxAddr) -> Vec<String> {
    vec![
        "tmux".into(),
        "paste-buffer".into(),
        "-p".into(),
        "-d".into(),
        "-b".into(),
        buffer.into(),
        "-t".into(),
        exact(&addr.target()),
    ]
}

/// Did tmux's `has-session` exit code mean "no such session" (`Ok(false)`),
/// "session exists" (`Ok(true)`), or "couldn't even ask" (`None` → the
/// caller raises)?
///
/// tmux itself exits `0` when the session exists and `1` when it doesn't
/// (including "no server running" — no server means no session, which is
/// a legitimate negative, not an error). Anything else is not tmux
/// answering the question: an SSH transport failure surfaces as `255`,
/// and a process killed by signal reports `None`. Folding those into
/// `Ok(false)` — as the old `out.status.success()` did — is the F6 bug:
/// during a network blip a stale agent session carrying the PREVIOUS
/// task's context looks absent, the kill-to-clear-context invariant is
/// skipped, and the next task's prompt lands in the wrong context.
fn interpret_has_session(code: Option<i32>) -> Option<bool> {
    match code {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Does a tmux session with this name exist on `host`?
///
/// Returns `Err` (not `Ok(false)`) when the query couldn't be answered —
/// e.g. an SSH transport failure — so callers relying on this to gate a
/// kill-to-clear-context step don't silently skip it during a network
/// blip. See [`interpret_has_session`].
pub fn has_session(host: &Host, name: &str) -> Result<bool> {
    let argv = has_session_argv(name);
    let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
    match interpret_has_session(out.status.code()) {
        Some(exists) => Ok(exists),
        None => Err(Error::Command {
            cmd: argv.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
    }
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
        exact(&format!("{session}:")),
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

/// Does this tmux stderr mean the target window/session is already gone,
/// as opposed to a failure to reach the host?
///
/// tmux prints `can't find window: …` / `can't find session: …` when the
/// `-t` target no longer exists, and `no server running on …` when the
/// whole server has exited — all three mean "already torn down", which is
/// success for an idempotent teardown. An SSH transport failure prints
/// ssh's own diagnostic (`Connection refused`, `Connection timed out`,
/// …), none of which contain these substrings, so it stays an error.
fn is_missing_target(stderr: &str) -> bool {
    stderr.contains("can't find window")
        || stderr.contains("can't find session")
        || stderr.contains("no server running")
}

/// Kill a window in a session.
///
/// An already-gone window/session is treated as success (teardown is
/// idempotent), but a failure to reach the host is returned as `Err` so
/// callers don't silently leave an orphaned remote agent running — see
/// [`is_missing_target`].
pub fn kill_window(host: &Host, addr: &TmuxAddr) -> Result<()> {
    let argv = kill_window_argv(addr);
    let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if is_missing_target(&stderr) {
        return Ok(());
    }
    Err(Error::Command {
        cmd: argv.join(" "),
        status: out.status.to_string(),
        stderr: stderr.into_owned(),
    })
}

/// A tmux paste-buffer name unique to this invocation.
///
/// tmux buffers live on the (server-global) tmux server, so a
/// compile-time constant name would be shared across every concurrent
/// sender — hub dispatch and a manual `shelbi send` racing on the same
/// buffer would interleave `load-buffer`/`paste-buffer` pairs, delivering
/// one message to the wrong pane and losing the other with "no buffer"
/// (F4). Deriving the name from the process id plus a monotonic counter
/// keeps each `send_line` call on its own buffer. Still namespaced with
/// the `shelbi-send-` prefix so we don't collide with the user's buffers.
fn paste_buffer_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("shelbi-send-{pid}-{n}")
}

/// Decide whether [`send_line`] must stage `text` through a tmux paste
/// buffer (vs. the faster `send-keys -l` fast path).
///
/// Multi-line text always uses the buffer path so embedded newlines
/// don't get re-parsed as command separators by the remote shell. For
/// SSH-routed hosts we also force the buffer path on single-line text:
/// `send-keys -l TEXT` over SSH would be joined into ssh's argv with
/// single spaces, then re-tokenized by the remote shell — losing
/// embedded spaces (tmux concatenates `-l` literal-text args with no
/// separator, producing e.g. `cd/path/to/wt` instead of `cd /path/to/wt`)
/// and letting shell metacharacters (`&&`, `|`, `;`, `$`) escape into
/// the remote shell instead of being treated as literal input.
fn uses_buffer_path(host: &Host, text: &str) -> bool {
    text.contains('\n') || host.is_ssh()
}

/// Send a string to the target's keyboard input, followed by Enter.
///
/// For local single-line text we use `send-keys -l` so tmux treats it as
/// literal characters (avoiding key-name expansion like `C-c`) and Enter
/// is sent as a separate `Enter` keysym.
///
/// For multi-line text — and for ALL remote text — we instead stage the
/// payload in a tmux paste buffer and replay it with `paste-buffer -p`.
/// `-p` wraps the content in bracketed-paste markers so the receiving
/// app (e.g. Claude) sees one atomic paste rather than N individual
/// Enter keypresses — which matters over SSH, where send-key Enters
/// arrive spaced out far enough to defeat any heuristic paste detection.
/// We also pipe the payload to `load-buffer -` via stdin so embedded
/// whitespace and shell metacharacters don't get re-parsed by the remote
/// shell when ssh joins argv with single spaces — `send-keys -l` over
/// SSH would otherwise lose spaces (tmux concatenates literal-text args
/// with no separator) and let `&&`, `|`, etc. escape into the remote
/// shell.
pub fn send_line(host: &Host, addr: &TmuxAddr, text: &str) -> Result<()> {
    if uses_buffer_path(host, text) {
        let buffer = paste_buffer_name();
        shelbi_ssh::run_with_stdin(host, load_buffer_argv(&buffer), text.as_bytes())?;
        shelbi_ssh::run_capture(host, paste_buffer_argv(&buffer, addr))?;
    } else {
        shelbi_ssh::run_capture(host, send_keys_literal_argv(addr, text))?;
    }
    shelbi_ssh::run_capture(host, ["tmux", "send-keys", "-t", &exact(&addr.target()), "Enter"])?;
    Ok(())
}

/// Send a bare Enter keypress to the target — no text. Used to dismiss
/// modal prompts (e.g. claude's "trust this folder" dialog, whose default
/// selection is the affirmative option) without typing anything into them.
pub fn send_enter(host: &Host, addr: &TmuxAddr) -> Result<()> {
    shelbi_ssh::run_capture(host, ["tmux", "send-keys", "-t", &exact(&addr.target()), "Enter"])?;
    Ok(())
}

/// Read the pane's current title, with the trailing newline trimmed. The
/// hub uses this to poll workspaces for state markers — claude's hooks write
/// `shelbi:<state>` to the title via OSC escapes (see
/// `shelbi-state::default_workspace_settings.json.template`), and the parser in
/// `shelbi-state` peels the marker back off.
pub fn pane_title(host: &Host, addr: &TmuxAddr) -> Result<String> {
    let raw = shelbi_ssh::run_capture(
        host,
        [
            "tmux",
            "display-message",
            "-p",
            "-t",
            &exact(&addr.target()),
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
        ["tmux", "capture-pane", "-p", "-J", "-t", &exact(&addr.target())],
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
            &exact(&addr.target()),
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
        // options and the hub-side reverse forward that shelbi-ssh
        // prepends to every invocation. They're an orthogonal concern
        // (covered by shelbi-ssh's own tests) and would otherwise
        // force every structural test below to enumerate them.
        // Recognized by the leading `ssh` program; for local commands
        // we pass through unchanged.
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
                    || v.starts_with("ConnectTimeout=")
                    || v.starts_with("BatchMode=")
                    || v.starts_with("ExitOnForwardFailure=")
                    || v.starts_with("LogLevel=")
                {
                    i += 2;
                    continue;
                }
            }
            // `-R remote:local` is the reverse-forward that exposes
            // hub.sock to remote workers. Skip the flag plus its
            // single argument.
            if raw[i] == "-R" && i + 1 < raw.len() {
                i += 2;
                continue;
            }
            out.push(raw[i].clone());
            i += 1;
        }
        out
    }

    #[test]
    fn send_enter_argv_is_bare_enter() {
        // send_enter sends an Enter keysym with no `-l` literal text — that's
        // what lets it dismiss a modal without typing into it.
        let cmd = shelbi_ssh::build_command(
            &Host::Local,
            ["tmux", "send-keys", "-t", "=shelbi-w-bob:agent", "Enter"],
        );
        assert_eq!(
            ssh_args(cmd),
            vec!["tmux", "send-keys", "-t", "=shelbi-w-bob:agent", "Enter"]
        );
    }

    fn addr(session: &str, window: &str) -> TmuxAddr {
        TmuxAddr {
            session: session.into(),
            window: window.into(),
        }
    }

    #[test]
    fn local_send_keys_argv() {
        // The local fast path: `-t` carries the `=` exact-match prefix and
        // `--` terminates flags so a dash-leading payload is literal (F5).
        assert_eq!(
            send_keys_literal_argv(&addr("shelbi-myapp", "w-x"), "hello"),
            vec!["tmux", "send-keys", "-t", "=shelbi-myapp:w-x", "-l", "--", "hello"]
        );
    }

    #[test]
    fn dash_leading_payload_survives_flag_terminator() {
        // Regression (F5): without `--`, `send-keys -l "-R hello"` is
        // rejected as an invalid flag. The `--` sits immediately before
        // the payload so `-R hello` reaches the pane verbatim.
        let argv = send_keys_literal_argv(&addr("shelbi-myapp", "agent"), "-R hello");
        let dashdash = argv.iter().position(|a| a == "--").expect("missing --");
        assert_eq!(argv[dashdash + 1], "-R hello");
        // Nothing flag-like sits between `--` and the payload.
        assert_eq!(argv.last().unwrap(), "-R hello");
    }

    #[test]
    fn remote_new_session_argv() {
        // What `new_session` for a remote workspace would build under the hood.
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
        // atomic paste rather than N Enter keypresses. Target carries the
        // `=` exact-match prefix; the buffer name is per-invocation (F4).
        assert_eq!(
            paste_buffer_argv("shelbi-send-42-7", &addr("shelbi-myapp", "w-x")),
            vec![
                "tmux",
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                "shelbi-send-42-7",
                "-t",
                "=shelbi-myapp:w-x",
            ]
        );
    }

    #[test]
    fn load_buffer_argv_reads_stdin() {
        // The payload is piped via stdin to `load-buffer -`, not smuggled
        // through argv — `-` is the last positional.
        assert_eq!(
            load_buffer_argv("shelbi-send-42-7"),
            vec!["tmux", "load-buffer", "-b", "shelbi-send-42-7", "-"]
        );
    }

    #[test]
    fn per_invocation_buffer_names_are_unique() {
        // Regression (F4): a compile-time constant buffer name let
        // concurrent senders race on one server-global buffer. Each call
        // must mint a distinct name so load/paste pairs can't interleave.
        let a = paste_buffer_name();
        let b = paste_buffer_name();
        assert_ne!(a, b);
        assert!(a.starts_with("shelbi-send-"), "unexpected name: {a}");
    }

    #[test]
    fn sibling_prefix_targets_use_exact_match() {
        // Regression (F3): tmux resolves a bare `-t` by exact-then-prefix,
        // so a torn-down `shelbi-w-bob` would prefix-match the live
        // sibling `shelbi-w-bob-2`. The `=` prefix forces exact match and
        // must be present on every session/window target we build.
        assert_eq!(has_session_argv("shelbi-w-bob").last().unwrap(), "=shelbi-w-bob");
        assert_eq!(
            kill_window_argv(&addr("shelbi-w-bob", "agent")).last().unwrap(),
            "=shelbi-w-bob:agent"
        );
        assert_eq!(
            paste_buffer_argv("b", &addr("shelbi-w-bob", "agent")).last().unwrap(),
            "=shelbi-w-bob:agent"
        );
        assert_eq!(
            send_keys_literal_argv(&addr("shelbi-w-bob", "agent"), "x")[3],
            "=shelbi-w-bob:agent"
        );
    }

    #[test]
    fn has_session_discriminates_transport_failure() {
        // Regression (F6): tmux exits 0 (exists) / 1 (absent, incl. "no
        // server running"), but an SSH transport failure surfaces as 255
        // and a signal kill as None. Only 0/1 are real answers; anything
        // else must raise so a network blip can't masquerade as "absent".
        assert_eq!(interpret_has_session(Some(0)), Some(true));
        assert_eq!(interpret_has_session(Some(1)), Some(false));
        assert_eq!(interpret_has_session(Some(255)), None);
        assert_eq!(interpret_has_session(Some(2)), None);
        assert_eq!(interpret_has_session(None), None);
    }

    #[test]
    fn missing_target_is_distinct_from_transport_failure() {
        // Regression (F13): an already-gone window/session (or a stopped
        // server) is benign teardown; an unreachable host is a real error
        // that must not be swallowed as "already gone".
        assert!(is_missing_target("can't find window: agent"));
        assert!(is_missing_target("can't find session: shelbi-w-bob"));
        assert!(is_missing_target(
            "no server running on /tmp/tmux-1000/default"
        ));
        assert!(!is_missing_target(
            "ssh: connect to host devbox port 22: Connection refused"
        ));
        assert!(!is_missing_target("Connection timed out"));
    }

    #[test]
    fn remote_pane_title_argv() {
        // The hub-side workspace poll uses display-message + #{pane_title}
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
                "=shelbi-w-fix-login:agent",
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
                "=shelbi-w-fix-login:agent",
                "#{pane_title}",
            ]
        );
    }

    #[test]
    fn remote_single_line_uses_buffer_path() {
        // Regression: `tmux send-keys -l TEXT` over SSH loses embedded
        // spaces — the remote shell re-tokenizes ssh's space-joined argv,
        // tmux sees each space-separated word as a distinct `-l` literal,
        // and concatenates them with no separator (producing e.g.
        // `cd/home/jlong/...` instead of `cd /home/jlong/...`). Worse,
        // shell metacharacters like `&&` escape into the remote shell.
        // Force the buffer path for all SSH-routed text so the payload
        // travels through `load-buffer -` stdin instead of argv.
        let host = Host::Ssh {
            host: "devbox".into(),
        };
        let text = "cd /home/jlong/Workspaces/shelbi/.shelbi/wt/delta && exec \"${SHELL:-/bin/bash}\" -lc claude";
        assert!(uses_buffer_path(&host, text));
        // Single-line text without metachars still routes through the
        // buffer over SSH — the issue is structural to send-keys -l over
        // ssh, not specific to the metachars in the payload above.
        assert!(uses_buffer_path(&host, "hello world"));
    }

    #[test]
    fn local_single_line_uses_fast_path() {
        // Local invocations don't go through ssh re-tokenization, so the
        // fast `send-keys -l` path stays correct (and saves the extra
        // load-buffer + paste-buffer round-trips).
        assert!(!uses_buffer_path(&Host::Local, "hello world"));
        assert!(!uses_buffer_path(&Host::Local, "cd /tmp && ls"));
    }

    #[test]
    fn multiline_always_uses_buffer_path() {
        assert!(uses_buffer_path(&Host::Local, "line one\nline two"));
        assert!(uses_buffer_path(
            &Host::Ssh {
                host: "devbox".into(),
            },
            "line one\nline two",
        ));
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
                "=shelbi-w-fix-login:agent",
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
                "=shelbi-w-fix-login:agent",
            ]
        );
    }
}
