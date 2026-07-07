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
    // the user's terminal. NB: these options silence the forward-failed
    // warning on the *master open* too. That gap is closed out of band by
    // [`ensure_reverse_forward`], which cleans and verifies the forward
    // instead of relying on ssh's suppressed stderr.
    "-o",
    "ExitOnForwardFailure=no",
    "-o",
    "LogLevel=ERROR",
];

/// The static control options plus the per-invocation `ControlPath`, but
/// *without* the `-R` reverse forward — the forward spec is mode-dependent
/// (Unix vs TCP loopback) and layered on by the callers below.
fn base_control_opts() -> Vec<String> {
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
        .unwrap_or_else(|_| "~/.shelbi/ssh/%C".to_string());
    opts.push("-o".into());
    opts.push(format!("ControlPath={cp}"));
    opts
}

/// The `-R` reverse-forward spec to install for `hostname`, honoring the
/// persisted forward decision: a host that fell back to (or was pinned to)
/// TCP loopback gets `127.0.0.1:<port>:<hub.sock>`; everyone else gets the
/// default Unix-socket forward. `None` only when spec resolution itself fails
/// (no `$HOME`, etc.), in which case the master just won't carry the forward
/// this round.
fn forward_spec_for_host(hostname: &str) -> Option<String> {
    let spec = match shelbi_state::load_host_forward(hostname) {
        Some(shelbi_state::HostForward {
            mode: shelbi_core::ForwardMode::Tcp,
            port: Some(port),
        }) => shelbi_state::reverse_forward_spec_tcp(port),
        _ => shelbi_state::reverse_forward_spec(),
    };
    spec.map(|os| os.to_string_lossy().into_owned()).ok()
}

/// The full control options + the mode-aware `-R` reverse forward for a
/// shelbi-routed `ssh` invocation to `hostname`. Built fresh per call so a
/// `SHELBI_HOME`/`SHELBI_HUB_SOCK` override — or a forward-mode decision the
/// hub persisted after a TCP fallback — lands in the args without baking it
/// into a const.
///
/// The reverse forward exposes the hub daemon's `~/.shelbi/hub.sock` to the
/// remote side so remote workers can write to hub's events.log without an extra
/// outbound channel.
fn build_ssh_control_opts(hostname: &str) -> Vec<String> {
    let mut opts = base_control_opts();
    if let Some(spec) = forward_spec_for_host(hostname) {
        opts.push("-R".into());
        opts.push(spec);
    }
    opts
}

fn apply_ssh_control_opts(cmd: &mut Command, hostname: &str) {
    for opt in build_ssh_control_opts(hostname) {
        cmd.arg(opt);
    }
}

/// Apply only the conservative connection options needed for one-shot
/// maintenance commands. Deliberately avoids ControlMaster and `-R`:
/// these commands inspect or remove the reverse-forward landing socket,
/// so they must not create the socket as a side effect.
fn apply_ssh_no_forward_opts(cmd: &mut Command) {
    for (flag, value) in [
        ("-o", "ControlMaster=no"),
        ("-o", "ConnectTimeout=5"),
        ("-o", "BatchMode=yes"),
        ("-o", "LogLevel=ERROR"),
    ] {
        cmd.arg(flag).arg(value);
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
/// This closes F1/F2 from Shelbi ContextStore
/// docs/planning:reviews/adversarial-2026-07/process-boundaries.md: an unquoted
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
            apply_ssh_control_opts(&mut cmd, host);
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
            apply_ssh_control_opts(&mut cmd, host);
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
    // Capture (don't `?`) the write error. If the child died early — an
    // unreachable host or refused auth exits within milliseconds — a
    // payload larger than the pipe buffer hits EPIPE here. Returning on the
    // `?` would (a) leave the child unreaped: `Child`'s `Drop` doesn't
    // `wait`, so the long-lived hub daemon would accumulate `<defunct>` ssh
    // processes, and (b) surface a bare `BrokenPipe` while the real
    // diagnostic ("Connection refused", "Permission denied") sits unread in
    // the child's stderr (Shelbi ContextStore
    // docs/planning:reviews/adversarial-2026-07/process-boundaries.md F8).
    // Instead we record the
    // error, always drain to `wait_with_output` below (which reaps the
    // child), and fold its stderr into the returned error.
    let write_err = {
        let mut child_stdin = child.stdin.take().expect("stdin was piped");
        child_stdin.write_all(stdin).err()
        // child_stdin drops here, closing the pipe so a healthy child sees
        // EOF on stdin and can finish.
    };
    let output = child.wait_with_output().map_err(shelbi_core::Error::Io)?;
    if let Some(werr) = write_err {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // Prefer the child's own diagnostic when it left one; fall back to
        // the raw IO error only when stderr is empty (e.g. the write failed
        // for a reason unrelated to the child dying).
        if stderr.trim().is_empty() {
            return Err(shelbi_core::Error::Io(werr));
        }
        return Err(shelbi_core::Error::Command {
            cmd: cmd_str,
            status: output.status.to_string(),
            stderr,
        });
    }
    if !output.status.success() {
        return Err(shelbi_core::Error::Command {
            cmd: cmd_str,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The minimal `-o ControlPath=…` pair used by master *control* commands
/// (`ssh -O check` / `ssh -O exit`). These don't open a new connection —
/// they locate an existing master by its ControlPath — so they carry
/// neither the full connect-tuning options nor the `-R` reverse forward.
fn control_path_opt() -> Vec<String> {
    // Materialize the dir for parity with `build_ssh_control_opts`; a `-O`
    // command against a missing dir just reports "no control path" which is
    // exactly the "no master" answer we want anyway.
    let _ = shelbi_state::ensure_ssh_control_dir();
    let cp = shelbi_state::ssh_control_path_template()
        .unwrap_or_else(|_| "~/.shelbi/ssh/%C".to_string());
    vec!["-o".to_string(), format!("ControlPath={cp}")]
}

/// Tear down any ControlMaster for `hostname` (`ssh -O exit`). Best-effort:
/// a nonzero exit just means there was no master to close. We drop the
/// master before reopening so `ControlMaster=auto` opens a *fresh* one that
/// rebinds the `-R` forward, rather than silently reusing a master whose
/// forward failed to bind.
fn drop_master(hostname: &str) {
    let mut cmd = Command::new("ssh");
    for o in control_path_opt() {
        cmd.arg(o);
    }
    cmd.arg("-O").arg("exit").arg(hostname);
    let _ = cmd.output();
}

/// Open a fresh ControlMaster with the reverse-forward unlink option enabled.
/// Callers must drop the existing master first; applying
/// StreamLocalBindUnlink to ordinary multiplexed slave commands could replace
/// an already-healthy listener for only the lifetime of that slave.
fn open_master_with_stream_local_unlink(hostname: &str) -> std::io::Result<Output> {
    let mut cmd = Command::new("ssh");
    for o in build_ssh_control_opts(hostname) {
        cmd.arg(o);
    }
    cmd.arg("-o")
        .arg("StreamLocalBindUnlink=yes")
        .arg(hostname)
        .arg("--")
        .arg("true");
    tracing::debug!(?cmd, host = %hostname, "ssh::open_master_with_stream_local_unlink");
    cmd.output()
}

/// Open a fresh ControlMaster carrying a **TCP loopback** reverse forward
/// (`-R 127.0.0.1:<port>:<hub.sock>`) instead of the Unix-socket forward.
///
/// `ExitOnForwardFailure=yes` is the linchpin of port-collision handling: if
/// the remote can't bind `127.0.0.1:<port>` (already in use), ssh exits
/// nonzero and no master persists, so the caller sweeps to the next candidate
/// port. It's set *before* the static opts so its value wins over the
/// `ExitOnForwardFailure=no` we hand the normal (multiplexed-slave) path —
/// OpenSSH honors the first value seen for each option.
fn open_master_tcp(hostname: &str, port: u16) -> std::io::Result<Output> {
    let spec = match shelbi_state::reverse_forward_spec_tcp(port) {
        Ok(s) => s.to_string_lossy().into_owned(),
        Err(e) => {
            return Err(std::io::Error::other(e.to_string()));
        }
    };
    let mut cmd = Command::new("ssh");
    for a in build_tcp_master_args(hostname, &spec) {
        cmd.arg(a);
    }
    tracing::debug!(?cmd, host = %hostname, port, "ssh::open_master_tcp");
    cmd.output()
}

/// Assemble the argv (after the `ssh` program) for a TCP-loopback master open:
/// `ExitOnForwardFailure=yes` first so it wins over the static `=no`, then the
/// base control opts, then the TCP `-R` spec, then `<host> -- true`. Split out
/// so the arg shape is unit-testable without shelling out.
fn build_tcp_master_args(hostname: &str, spec: &str) -> Vec<String> {
    let mut args = vec!["-o".to_string(), "ExitOnForwardFailure=yes".to_string()];
    args.extend(base_control_opts());
    args.push("-R".to_string());
    args.push(spec.to_string());
    args.push(hostname.to_string());
    args.push("--".to_string());
    args.push("true".to_string());
    args
}

/// Does the reverse-forward landing socket exist on the remote? `test -S`
/// is true only for an existing socket node. Routed through the no-forward
/// maintenance path so the probe observes the socket without creating it.
fn remote_socket_present(host: &Host, remote_sock: &str) -> bool {
    match host {
        Host::Local => false,
        Host::Ssh { host } => {
            matches!(run_without_reverse_forward(host, ["test", "-S", remote_sock]), Ok(o) if o.status.success())
        }
    }
}

/// Is the landing socket *usable* by the login user — i.e. writable, so a
/// worker on the remote could actually `connect()` to it? This is the check
/// that distinguishes a healthy forward from the Tailscale-SSH wedge: there
/// tailscaled binds the socket `srw------- root root`, so `test -w` fails for
/// the login user even though `test -S` (a bare stat) still passes.
fn remote_socket_writable(host: &Host, remote_sock: &str) -> bool {
    match host {
        Host::Local => false,
        Host::Ssh { host } => {
            matches!(run_without_reverse_forward(host, ["test", "-w", remote_sock]), Ok(o) if o.status.success())
        }
    }
}

/// Did the `rm -f` cleanup fail with `EPERM` ("Operation not permitted")? That
/// is the fingerprint of the Tailscale-SSH wedge: a root-owned landing socket
/// in sticky `/tmp` that the login user cannot unlink.
fn cleanup_hit_eperm(cleanup: &std::io::Result<Output>) -> bool {
    matches!(cleanup, Ok(o) if !o.status.success()
        && String::from_utf8_lossy(&o.stderr).contains("Operation not permitted"))
}

/// Paths for which a cleanup-EPERM event has already been logged, so repeated
/// health checks against the same wedged socket log once — not once per retry
/// (Acceptance: "Repeated cleanup EPERM on the same path logs a single event").
fn eperm_logged() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static LOGGED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    LOGGED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Log a cleanup-EPERM event at most once per socket path.
fn log_eperm_once(hostname: &str, remote_sock: &str) {
    let mut set = eperm_logged()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if set.insert(remote_sock.to_string()) {
        let _ = shelbi_state::emit_event_body(&format!(
            "ssh reverse-forward host={hostname} remote_sock={remote_sock} \
             detail=stale_socket_cleanup_failed cleanup_stderr=Operation not permitted"
        ));
    }
}

/// Outcome of the Unix-socket forward attempt, so the caller can tell a
/// transient network failure (don't fall back) from the Tailscale-SSH wedge
/// (do fall back to TCP loopback when allowed).
enum UnixForwardOutcome {
    /// Forward is bound and the landing socket is usable.
    Ok,
    /// The master never opened — unreachable host, refused auth, connect
    /// timeout. NOT the wedge; surface it, do not fall back to TCP
    /// (`master_open_failed` with a connect timeout must not misfire).
    MasterOpenFailed,
    /// The master opened (network fine) but the landing socket is unusable —
    /// the root-owned-socket wedge. `detail` describes the exact shape.
    Wedged { detail: &'static str },
}

/// The Unix-socket reverse-forward repair path (the original behavior),
/// refactored to classify its result so [`ensure_reverse_forward`] can decide
/// whether to fall back to TCP.
fn ensure_unix_forward(host: &Host, hostname: &str, remote_sock: &str) -> UnixForwardOutcome {
    // Repair. Drop any existing master first: it may be a master whose
    // forward never bound (stale socket collided with the `-R`), and
    // `ControlMaster=auto` would otherwise reuse it and skip the rebind.
    drop_master(hostname);
    // Remove the stale landing socket. We only reach here when no live
    // master owns the forward, so any leftover socket file is a leak from a
    // dead master — safe to unlink. The cleanup command deliberately bypasses
    // shelbi's ControlMaster/`-R` wrapper; otherwise an absent socket can be
    // recreated by SSH and then immediately removed by this `rm`.
    let cleanup = run_without_reverse_forward(hostname, ["rm", "-f", remote_sock]);
    drop_master(hostname);

    // A cleanup EPERM is the smoking gun of the wedge — a root-owned socket the
    // login user can't unlink. Log it once per path (not per retry) regardless
    // of what the reopen below does.
    if cleanup_hit_eperm(&cleanup) {
        log_eperm_once(hostname, remote_sock);
    }

    // Reopen the master, rebinding `-R` against the now-clean path. `true`
    // is the cheapest remote command; the master opens (and ControlPersist
    // keeps it) as a side effect.
    let opened = match open_master_with_stream_local_unlink(hostname) {
        Ok(o) => o,
        // Failure to even spawn ssh is treated as a master-open failure — a
        // local/transient problem, not the wedge.
        Err(_) => return UnixForwardOutcome::MasterOpenFailed,
    };

    if !opened.status.success() {
        return UnixForwardOutcome::MasterOpenFailed;
    }

    // Master opened → network/auth are fine. If the landing socket is now
    // usable, we're done. Otherwise this is the wedge.
    let present = remote_socket_present(host, remote_sock);
    if present && remote_socket_writable(host, remote_sock) {
        return UnixForwardOutcome::Ok;
    }
    if cleanup_hit_eperm(&cleanup) {
        UnixForwardOutcome::Wedged {
            detail: "stale_socket_cleanup_failed",
        }
    } else if present {
        // Present but not writable — bound root-owned (Tailscale SSH).
        UnixForwardOutcome::Wedged {
            detail: "landing_socket_unwritable",
        }
    } else {
        // Master opened but no socket landed — a stricter server refused the
        // StreamLocalBind, or it was removed out from under us. TCP loopback
        // sidesteps the Unix-bind path entirely.
        UnixForwardOutcome::Wedged {
            detail: "landing_socket_missing",
        }
    }
}

/// Candidate loopback ports to try, starting from `start` (the port a previous
/// forward bound, if any) and sweeping the configured band on a collision.
fn tcp_candidate_ports(start: u16) -> Vec<u16> {
    let base = shelbi_state::TCP_FORWARD_PORT_BASE;
    let span = shelbi_state::TCP_FORWARD_PORT_SPAN;
    // Normalize `start` into the band so a stale/out-of-range persisted port
    // can't push us outside it.
    let first = if start >= base && start < base + span {
        start
    } else {
        base
    };
    let mut ports = Vec::with_capacity(span as usize);
    ports.push(first);
    for i in 0..span {
        let p = base + i;
        if p != first {
            ports.push(p);
        }
    }
    ports
}

/// Establish (or re-establish) a TCP loopback reverse forward for `hostname`.
///
/// A no-forward connectivity probe runs first: if the host is unreachable we
/// surface that immediately instead of hammering every candidate port — and,
/// critically, once the probe succeeds we *know* any subsequent master-open
/// failure is a port-bind collision (not a network fault), so sweeping ports
/// is safe and can't misfire on a transient timeout.
fn ensure_tcp_forward(hostname: &str) -> shelbi_core::Result<u16> {
    // Connectivity gate. `ControlMaster=no` one-shot, no forward — purely "can
    // we reach the host at all?"
    let reachable = matches!(
        run_without_reverse_forward(hostname, ["true"]),
        Ok(o) if o.status.success()
    );
    if !reachable {
        let _ = shelbi_state::emit_event_body(&format!(
            "ssh reverse-forward host={hostname} mode=tcp status=failed detail=master_open_failed"
        ));
        return Err(shelbi_core::Error::Other(format!(
            "ssh reverse forward to {hostname} could not be established over TCP loopback \
             (host unreachable); worker→hub messages will not be delivered"
        )));
    }

    let start = shelbi_state::load_host_forward(hostname)
        .and_then(|h| h.port)
        .unwrap_or(shelbi_state::TCP_FORWARD_PORT_BASE);

    for port in tcp_candidate_ports(start) {
        // Drop any master first so ControlMaster=auto opens a fresh one that
        // binds this candidate port's `-R`.
        drop_master(hostname);
        let opened = open_master_tcp(hostname, port);
        if matches!(&opened, Ok(o) if o.status.success()) {
            // ExitOnForwardFailure=yes guarantees the `-R` bound when the
            // master opened. Remember the mode + port so subsequent outbound
            // ssh (and the worker env) reuse this exact port. Success is silent
            // — like the Unix path, we only log failures, so the 120s rechecks
            // don't flood events.log.
            let _ = shelbi_state::save_host_forward(
                hostname,
                Some(shelbi_state::HostForward {
                    mode: shelbi_core::ForwardMode::Tcp,
                    port: Some(port),
                }),
            );
            return Ok(port);
        }
        // Network was already proven reachable, so a failure here is a bind
        // collision — try the next candidate port.
    }

    let _ = shelbi_state::emit_event_body(&format!(
        "ssh reverse-forward host={hostname} mode=tcp status=failed detail=tcp_forward_failed \
         no free loopback port in band"
    ));
    Err(shelbi_core::Error::Other(format!(
        "ssh reverse forward to {hostname} could not bind a TCP loopback port; \
         worker→hub messages will not be delivered"
    )))
}

/// Ensure the hub's reverse forward to `host` is bound and healthy, repairing
/// a stale-remote-socket wedge if one is present and falling back to a TCP
/// loopback forward when the Unix landing socket turns out to be unusable
/// (the Tailscale-SSH root-owned-socket condition).
///
/// Every shelbi-routed `ssh` invocation carries `-R <remote>:<local hub.sock>`
/// so remote workers can write to the hub's `events.log` over the multiplexed
/// channel. But `-R` to a Unix socket binds usefully only when the login user
/// owns the landing path. On hosts reached via Tailscale SSH, tailscaled (root)
/// binds it `srw------- root root`: unconnectable and unremovable by the login
/// user, so every retry re-wedges and leaks another root-owned socket. When we
/// detect that, we switch the host to a TCP loopback forward
/// (`-R 127.0.0.1:<port>:<hub.sock>`) and remember the decision so subsequent
/// forwards skip the failing Unix attempt.
///
/// `configured` is the per-machine `forward:` override from project YAML:
/// `Some(Tcp)` goes straight to TCP (no detection), `Some(Unix)` pins the Unix
/// forward and disables the fallback, `None` is auto (Unix first, fall back to
/// TCP on the wedge, remembering the choice).
///
/// This is a no-op for [`Host::Local`].
pub fn ensure_reverse_forward(
    host: &Host,
    configured: Option<shelbi_core::ForwardMode>,
) -> shelbi_core::Result<()> {
    let hostname = match host {
        Host::Local => return Ok(()),
        Host::Ssh { host } => host.clone(),
    };
    let remote_sock = shelbi_state::remote_hub_socket_path()
        .to_string_lossy()
        .into_owned();

    // Decide the mode to attempt and whether auto-fallback is permitted.
    let (mode, allow_fallback) = match configured {
        Some(shelbi_core::ForwardMode::Tcp) => (shelbi_core::ForwardMode::Tcp, false),
        Some(shelbi_core::ForwardMode::Unix) => (shelbi_core::ForwardMode::Unix, false),
        None => match shelbi_state::load_host_forward(&hostname) {
            Some(hf) => (hf.mode, true),
            None => (shelbi_core::ForwardMode::Unix, true),
        },
    };

    if mode == shelbi_core::ForwardMode::Tcp {
        return ensure_tcp_forward(&hostname).map(|_| ());
    }

    match ensure_unix_forward(host, &hostname, &remote_sock) {
        UnixForwardOutcome::Ok => Ok(()),
        UnixForwardOutcome::MasterOpenFailed => {
            // Transient / network — surface, never fall back (a connect
            // timeout is not the wedge).
            let _ = shelbi_state::emit_event_body(&format!(
                "ssh reverse-forward host={hostname} remote_sock={remote_sock} \
                 status=failed detail=master_open_failed"
            ));
            Err(shelbi_core::Error::Other(format!(
                "ssh reverse forward to {hostname} could not be verified (master_open_failed); \
                 worker→hub messages via {remote_sock} will not be delivered"
            )))
        }
        UnixForwardOutcome::Wedged { detail } => {
            if allow_fallback {
                // The Tailscale-SSH wedge. Switch this host to TCP loopback and
                // remember it so we stop re-attempting (and re-leaking) Unix.
                // Log the transition once (subsequent rechecks find the mode
                // already persisted and go straight to TCP without re-entering
                // this branch).
                match ensure_tcp_forward(&hostname) {
                    Ok(port) => {
                        let _ = shelbi_state::emit_event_body(&format!(
                            "ssh reverse-forward host={hostname} remote_sock={remote_sock} \
                             detail={detail} action=falling_back_to_tcp mode=tcp port={port} \
                             status=established"
                        ));
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else {
                // `forward: unix` pinned — respect it, don't silently switch.
                let _ = shelbi_state::emit_event_body(&format!(
                    "ssh reverse-forward host={hostname} remote_sock={remote_sock} \
                     status=failed detail={detail}"
                ));
                Err(shelbi_core::Error::Other(format!(
                    "ssh reverse forward to {hostname} could not be verified ({detail}); \
                     worker→hub messages via {remote_sock} will not be delivered"
                )))
            }
        }
    }
}

/// Run a remote maintenance/probe command without installing or reusing
/// Shelbi's reverse forward. This keeps health checks pure: `test -S` must
/// observe the landing socket, and `rm -f` must remove it, without the SSH
/// wrapper first binding a fresh one.
fn build_no_forward_command<I, S>(hostname: &str, argv: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new("ssh");
    apply_ssh_no_forward_opts(&mut cmd);
    cmd.arg(hostname);
    cmd.arg("--");
    for a in argv {
        cmd.arg(escape_for_wire(a.as_ref()));
    }
    cmd
}

fn run_without_reverse_forward<I, S>(hostname: &str, argv: I) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = build_no_forward_command(hostname, argv);
    tracing::debug!(?cmd, host = %hostname, "ssh::run_without_reverse_forward");
    cmd.output()
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
        let mut expected: Vec<String> = build_ssh_control_opts("m2.local");
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
        let mut expected: Vec<String> = build_ssh_control_opts("m2.local");
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
                || spec.starts_with(&format!(
                    "{}:",
                    shelbi_state::remote_hub_socket_path().display()
                )),
            "forward spec didn't start with remote socket path: {spec}",
        );
        // ControlPath lands under SHELBI_HOME so the hub's startup
        // cleanup can find these sockets.
        let cp_idx = args
            .iter()
            .position(|a| a.starts_with("ControlPath="))
            .expect("missing ControlPath");
        assert!(
            args[cp_idx].contains("/ssh/%C"),
            "ControlPath didn't carry the %C connection-hash template: {}",
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
        let args = [
            "printf",
            "[%s]\n",
            "a b",
            "#{pane_title}",
            "x && y",
            "p;q",
            "$HOME",
        ];
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
        let out = run_with_stdin(&Host::Local, ["cat"], payload.as_bytes()).expect("cat failed");
        assert_eq!(out, payload);
    }

    #[test]
    fn run_with_stdin_surfaces_child_stderr_on_broken_pipe() {
        // F8: a child that exits immediately without draining stdin models
        // an unreachable host / refused auth. A payload larger than the
        // pipe buffer (64 KiB on Linux, less on macOS) forces `write_all`
        // to hit EPIPE. We must reap the child (no zombie) and surface its
        // own stderr ("boom") rather than a bare BrokenPipe.
        let payload = vec![b'x'; 1 << 20]; // 1 MiB, well over any pipe buffer
        let err = run_with_stdin(
            &Host::Local,
            ["sh", "-c", "echo boom >&2; exit 7"],
            &payload,
        )
        .expect_err("expected failure from instantly-dying child");
        match err {
            shelbi_core::Error::Command { stderr, .. } => {
                assert!(stderr.contains("boom"), "stderr was: {stderr}");
            }
            other => panic!("expected Command error carrying child stderr, got: {other:?}"),
        }
    }

    #[test]
    fn ensure_reverse_forward_is_noop_for_local() {
        // Local hosts have no reverse forward to establish — the call must
        // short-circuit without shelling out to ssh.
        ensure_reverse_forward(&Host::Local, None).expect("local ensure should be Ok");
    }

    #[test]
    fn no_forward_maintenance_command_does_not_request_reverse_forward() {
        let cmd = build_no_forward_command("devbox", ["rm", "-f", "/tmp/shelbi-hub-501.sock"]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert!(
            !args.iter().any(|a| a == "-R"),
            "maintenance command must not create the socket it is repairing: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "ControlMaster=no"),
            "maintenance command must bypass shelbi's persistent ControlMaster: {args:?}"
        );
        assert!(
            args.windows(2).any(|w| w == ["--", "rm"]),
            "remote argv should still be sent after --: {args:?}"
        );
    }

    #[test]
    fn tcp_master_args_force_exit_on_forward_failure_and_carry_tcp_r() {
        // The TCP master open must (a) set ExitOnForwardFailure=yes so a bind
        // collision fails the open (letting the caller sweep ports), placed
        // BEFORE the static ExitOnForwardFailure=no so its value wins, and
        // (b) carry the loopback `-R` spec — never a Unix socket path.
        let spec = "127.0.0.1:47100:/home/u/.shelbi/hub.sock";
        let args = build_tcp_master_args("devbox", spec);

        // ExitOnForwardFailure appears with =yes first, =no (from the static
        // opts) only later. OpenSSH honors the first value.
        let yes = args
            .iter()
            .position(|a| a == "ExitOnForwardFailure=yes")
            .expect("missing ExitOnForwardFailure=yes");
        let no = args.iter().position(|a| a == "ExitOnForwardFailure=no");
        assert!(
            no.map_or(true, |n| yes < n),
            "=yes must precede =no so it wins: {args:?}"
        );

        // The forward is the TCP spec, and no `-R` carries a /tmp Unix socket.
        let r = args.iter().position(|a| a == "-R").expect("missing -R");
        assert_eq!(args[r + 1], spec);
        assert!(
            !args.iter().any(|a| a.contains("/tmp/shelbi-hub")),
            "TCP master must not reference a Unix landing socket: {args:?}"
        );

        // Ends with `<host> -- true` — the cheapest remote no-op that persists
        // the master.
        let tail = &args[args.len() - 3..];
        assert_eq!(tail, ["devbox", "--", "true"], "unexpected tail: {args:?}");
    }

    #[test]
    fn tcp_candidate_ports_starts_from_hint_then_sweeps_band() {
        let base = shelbi_state::TCP_FORWARD_PORT_BASE;
        let span = shelbi_state::TCP_FORWARD_PORT_SPAN;

        // A hint inside the band is tried first, then the rest of the band
        // (each port exactly once).
        let ports = tcp_candidate_ports(base + 3);
        assert_eq!(ports[0], base + 3, "hint must be tried first: {ports:?}");
        assert_eq!(ports.len(), span as usize, "one entry per band port");
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), span as usize, "no duplicate ports: {ports:?}");
        assert_eq!(*sorted.first().unwrap(), base);
        assert_eq!(*sorted.last().unwrap(), base + span - 1);

        // An out-of-band hint is normalized back to the band base.
        let ports = tcp_candidate_ports(1);
        assert_eq!(ports[0], base, "stale hint falls back to base: {ports:?}");
        assert_eq!(ports.len(), span as usize);
    }

    #[test]
    fn cleanup_hit_eperm_detects_operation_not_permitted() {
        // A cleanup that exits nonzero with "Operation not permitted" on stderr
        // is the Tailscale-SSH fingerprint.
        let eperm = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo \"rm: cannot remove '/tmp/x.sock': Operation not permitted\" >&2; exit 1")
            .output();
        assert!(cleanup_hit_eperm(&eperm));

        // A clean success is not EPERM.
        let ok = std::process::Command::new("sh").arg("-c").arg("true").output();
        assert!(!cleanup_hit_eperm(&ok));

        // A different failure (e.g. ordinary error) is not EPERM either.
        let other = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo 'some other error' >&2; exit 1")
            .output();
        assert!(!cleanup_hit_eperm(&other));
    }
}
