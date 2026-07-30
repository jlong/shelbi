//! Thin wrapper around `std::process::Command` that knows how to dispatch
//! either locally or over `ssh`.
//!
//! Why shell out to the host's `ssh` (instead of an in-process SSH crate
//! like `russh`): we want the user's existing `~/.ssh/config`, `ssh-agent`,
//! ProxyJump, etc. to "just work" — and we want one less thing to maintain.

use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use shelbi_core::Host;

/// How many times to attempt a transient master open before giving up, and the
/// base backoff between attempts. Both env-overridable so a chronically noisy
/// host can be given a longer leash without a rebuild:
/// `SHELBI_FORWARD_RETRY_ATTEMPTS` (clamped 1..=10) and
/// `SHELBI_FORWARD_RETRY_BACKOFF_MS` (clamped 0..=5000).
///
/// The forward health check runs on a slow (120s) cadence in its own
/// per-workspace thread, so the few hundred ms of bounded backoff a retry adds
/// on a blip is invisible to the rest of the poller — and far cheaper than the
/// event noise + missed worker→hub messages a spurious `master_open_failed`
/// costs. `master_open_failed` for devbox was observed recurring *transiently*
/// (direct ssh stayed healthy the whole time), which is exactly the shape a
/// short retry absorbs.
const DEFAULT_FORWARD_RETRY_ATTEMPTS: u32 = 3;
const DEFAULT_FORWARD_RETRY_BACKOFF_MS: u64 = 250;

fn forward_retry_attempts() -> u32 {
    parse_env_u64("SHELBI_FORWARD_RETRY_ATTEMPTS")
        .map(|v| v as u32)
        .unwrap_or(DEFAULT_FORWARD_RETRY_ATTEMPTS)
        .clamp(1, 10)
}

fn forward_retry_backoff_base() -> Duration {
    let ms = parse_env_u64("SHELBI_FORWARD_RETRY_BACKOFF_MS")
        .unwrap_or(DEFAULT_FORWARD_RETRY_BACKOFF_MS)
        .min(5_000);
    Duration::from_millis(ms)
}

fn parse_env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse::<u64>().ok()
}

/// The sleep schedule *between* `attempts` tries: `attempts - 1` delays that
/// grow exponentially from `base` (`base`, `2·base`, `4·base`, …). A single
/// attempt yields no delays. Split out as a pure function so the backoff shape
/// is unit-testable without shelling out or actually sleeping. The shift is
/// capped so a large configured attempt count can't overflow the multiplier.
fn backoff_delays(attempts: u32, base: Duration) -> Vec<Duration> {
    (0..attempts.saturating_sub(1))
        .map(|i| base.saturating_mul(1u32.checked_shl(i.min(16)).unwrap_or(u32::MAX)))
        .collect()
}

/// Run `op` (returns `true` on success) up to `attempts` times, sleeping the
/// [`backoff_delays`] schedule between failed tries. Returns the 1-based
/// attempt number that first succeeded, or `None` if every attempt failed.
/// `on_retry` runs after each *failed* attempt that will be retried — the Unix
/// path uses it to drop the half-open master so `ControlMaster=auto` reopens
/// fresh on the next try.
fn retry_master_open(
    attempts: u32,
    base: Duration,
    mut op: impl FnMut() -> bool,
    mut on_retry: impl FnMut(),
) -> Option<u32> {
    let delays = backoff_delays(attempts, base);
    for i in 0..attempts {
        if op() {
            return Some(i + 1);
        }
        if let Some(delay) = delays.get(i as usize) {
            on_retry();
            std::thread::sleep(*delay);
        }
    }
    None
}

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
    // (the default) keeps duplicate-forward warnings on slave reconnects
    // from blocking the connection. The `LogLevel` (appended by
    // [`base_control_opts`], default `ERROR`, overridable via
    // `SHELBI_SSH_LOG_LEVEL`) keeps those warnings from polluting the
    // user's terminal. NB: at the default level these options silence the
    // forward-failed warning on the *master open* too. That gap is closed
    // out of band by [`ensure_reverse_forward`], which cleans and verifies
    // the forward instead of relying on ssh's suppressed stderr.
    "-o",
    "ExitOnForwardFailure=no",
];

/// Keepalive options applied to every shelbi-managed ssh connection — both the
/// persistent ControlMaster and the one-shot no-forward maintenance / fallback
/// connections.
///
/// `ControlPersist=600` (in [`SSH_CONTROL_OPTS_STATIC`]) only governs how long
/// an *idle* master lingers; it says nothing about liveness *during* a transfer.
/// Without keepalive a single long remote operation (the observed 91k-file
/// `git worktree add` that lost its transport at 43%) or an idle NAT / Tailscale
/// blip silently drops the master, and the next multiplexed command fails with
/// `read from master failed: Broken pipe` / `Control socket connect(...): No
/// such file`.
///
/// `ServerAliveInterval=15` + `ServerAliveCountMax=4` makes ssh send a keepalive
/// every 15s and give up after 4 unanswered (~60s) — so a genuinely dead peer is
/// detected in a bounded window instead of hanging on the multi-minute OS TCP
/// timeout, while a live-but-quiet long transfer is held open. `TCPKeepAlive=yes`
/// keeps NAT/firewall middleboxes from reaping an idle-but-alive connection.
const SSH_KEEPALIVE_OPTS: &[&str] = &[
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=4",
    "-o",
    "TCPKeepAlive=yes",
];

/// The ssh `LogLevel` to run shelbi-routed commands at. Defaults to `ERROR`
/// (quiet — only genuine failures print), but `SHELBI_SSH_LOG_LEVEL` overrides
/// it so an operator diagnosing a ControlMaster / reverse-forward problem can
/// escalate to `DEBUG1` and see ssh's mux chatter (the
/// `mux_client_request_session: read from master failed…` line and friends)
/// instead of a blank `--- stderr ---`. Trimmed; an empty value falls back to
/// the `ERROR` default.
fn ssh_log_level() -> String {
    std::env::var("SHELBI_SSH_LOG_LEVEL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "ERROR".to_string())
}

/// The static control options plus the per-invocation `ControlPath`, but
/// *without* the `-R` reverse forward — the forward spec is mode-dependent
/// (Unix vs TCP loopback) and layered on by the callers below.
fn base_control_opts() -> Vec<String> {
    let mut opts: Vec<String> = SSH_CONTROL_OPTS_STATIC
        .iter()
        .chain(SSH_KEEPALIVE_OPTS.iter())
        .map(|s| (*s).to_string())
        .collect();
    // LogLevel rides in front of the ControlPath so a `SHELBI_SSH_LOG_LEVEL`
    // override (e.g. DEBUG1) surfaces ssh's mux/ControlMaster diagnostics that
    // the default ERROR level keeps quiet.
    opts.push("-o".into());
    opts.push(format!("LogLevel={}", ssh_log_level()));
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
        ("-o", "ControlMaster=no".to_string()),
        ("-o", "ConnectTimeout=5".to_string()),
        ("-o", "BatchMode=yes".to_string()),
        ("-o", format!("LogLevel={}", ssh_log_level())),
    ] {
        cmd.arg(flag).arg(value);
    }
    // Keepalive too: the non-multiplexed fallback is what carries a *long* op
    // (a fresh-connection `git worktree add` retry) when the master is
    // unrecoverable, so it needs the same dead-peer detection as the master.
    for opt in SSH_KEEPALIVE_OPTS {
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

/// Whether a failed command's [`Output`] is a recoverable *transport* failure
/// (the multiplexed ControlMaster died) rather than a genuine remote-command
/// error. See [`classify_mux_failure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxFailure {
    /// Not a mux/transport failure: the command succeeded, or it failed for a
    /// reason reopening the master won't fix (a non-255 remote exit, or a 255
    /// with a real ssh diagnostic like `Permission denied` / `Connection
    /// refused`). Surface it as-is; do NOT drop the master.
    None,
    /// A multiplexing/transport failure: exit 255 whose stderr carries a
    /// broken-pipe / no-such-file / connection-closed signature — or is blank,
    /// the classic broken/absent-ControlMaster fingerprint. Drop+reopen the
    /// master and retry; fall back to a fresh non-multiplexed connection if the
    /// reopen doesn't take.
    Transport,
}

/// Substrings ssh/mux emit when the *transport* — not the remote command — is
/// what failed. Matched case-insensitively (stderr is lowercased first) and
/// only ever consulted on an exit-255 failure, so a remote command that merely
/// printed one of these strings (and exited with its own non-255 code) is never
/// misread as a transport loss.
const MUX_FAILURE_MARKERS: &[&str] = &[
    // `mux_client_request_session: read from master failed: Broken pipe`
    "read from master failed",
    "broken pipe",
    // `mux_client_*` chatter in general (request_session, hello_exchange, …).
    "mux_client",
    // `Control socket connect(/…/%C): No such file or directory` /
    // `…: Connection refused` — the master socket is gone or unlistened.
    "control socket connect",
    "no such file",
    // The peer went away mid-transfer — a fresh connection may well succeed.
    "connection closed by remote host",
    "connection reset by peer",
];

/// Classify a command's [`Output`] for ControlMaster/mux recovery. A pure
/// function of the exit status + stderr so the drop/reopen/fallback decision is
/// unit-testable without shelling out.
///
/// Only an ssh-level exit 255 can be a transport failure: a remote command that
/// merely failed exits with its *own* code, which ssh passes through unchanged.
/// A 255 with a mux signature (or a blank 255 — the broken-master fingerprint
/// [`annotated_stderr`] already annotates) is [`MuxFailure::Transport`];
/// everything else — including a 255 carrying a real ssh auth/host diagnostic a
/// reopen can't fix — is [`MuxFailure::None`].
pub fn classify_mux_failure(output: &Output) -> MuxFailure {
    if output.status.success() {
        return MuxFailure::None;
    }
    if output.status.code() != Some(255) {
        return MuxFailure::None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lowered = stderr.to_ascii_lowercase();
    if lowered.trim().is_empty() {
        // Blank 255 — the classic broken/absent-ControlMaster fingerprint.
        return MuxFailure::Transport;
    }
    if MUX_FAILURE_MARKERS.iter().any(|m| lowered.contains(m)) {
        return MuxFailure::Transport;
    }
    MuxFailure::None
}

/// Run `argv` on `host` over a fresh, **non-multiplexed** connection: no
/// ControlMaster (`ControlMaster=no`, a new TCP+auth handshake) and no `-R`
/// reverse forward. This is the transport the bug report confirms stays
/// rock-solid, used as the last-resort fallback in [`run_resilient`] when the
/// managed master is unrecoverable. Because it drops the reverse forward, it is
/// for hub-side git/maintenance ops (fetch, worktree add) that don't need the
/// worker→hub channel — not worker-facing commands.
pub fn run_no_multiplex<I, S>(host: &Host, argv: I) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    match host {
        Host::Local => run(host, argv),
        Host::Ssh { host } => {
            let mut cmd = build_no_forward_command(host, argv);
            tracing::debug!(?cmd, host = %host, "ssh::run_no_multiplex");
            cmd.output()
        }
    }
}

/// Run `argv` on `host`, recovering from a transient ControlMaster/mux failure.
///
/// On a [`MuxFailure::Transport`] result the (half-open) master is dropped and
/// reopened — serialized per host via [`shelbi_state::lock_ssh_master`] so a
/// concurrent daemon/TUI/CLI can't tear it out mid-retry — and the command is
/// retried once over the freshly reopened master. If that *still* fails on the
/// transport, it falls back to a fresh non-multiplexed connection
/// ([`run_no_multiplex`]) for this one op, so a critical task-start operation
/// succeeds instead of erroring on a flaky mux. Non-transport failures (and
/// successes) are returned from the first attempt untouched.
///
/// [`Host::Local`] has no multiplexed transport, so this is a plain [`run`].
pub fn run_resilient<I, S>(host: &Host, argv: I) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_resilient_with_cleanup(host, argv, || {})
}

/// [`run_resilient`], but run `cleanup` before each recovery attempt (the
/// reopen retry and the non-multiplexed fallback). For a `git worktree add` that
/// lost its transport mid-checkout the partial worktree must be removed before
/// the add can be retried — `cleanup` is where the caller does that. `cleanup`
/// is not run on the happy path (first attempt succeeded or failed
/// non-transiently).
pub fn run_resilient_with_cleanup<I, S>(
    host: &Host,
    argv: I,
    mut cleanup: impl FnMut(),
) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    // Materialize argv once so it can be replayed across attempts.
    let argv: Vec<std::ffi::OsString> = argv
        .into_iter()
        .map(|s| s.as_ref().to_os_string())
        .collect();

    let hostname = match host {
        Host::Local => return run(host, &argv),
        Host::Ssh { host } => host.clone(),
    };

    let first = run(host, &argv)?;
    if classify_mux_failure(&first) != MuxFailure::Transport {
        return Ok(first);
    }
    tracing::warn!(
        host = %hostname,
        "ssh mux transport failure; dropping+reopening the ControlMaster and retrying"
    );
    cleanup();
    // Drop the half-open master under the per-host lock so `ControlMaster=auto`
    // opens a fresh one on the retry and a concurrent process can't race the
    // drop against its own reopen.
    {
        let _lock = shelbi_state::lock_ssh_master(&hostname);
        drop_master(&hostname);
    }
    let second = run(host, &argv)?;
    if classify_mux_failure(&second) != MuxFailure::Transport {
        return Ok(second);
    }
    tracing::warn!(
        host = %hostname,
        "ssh mux still failing after master reopen; falling back to a fresh non-multiplexed connection"
    );
    cleanup();
    run_no_multiplex(host, &argv)
}

/// How often [`run_with_deadline`] polls the child for exit. Small enough
/// that a fast probe isn't slowed noticeably; large enough not to spin.
const DEADLINE_POLL: Duration = Duration::from_millis(15);

/// Run a command like [`run`], but bound its total wall-clock time: if the
/// child hasn't exited within `deadline` it is killed and the call returns
/// `ErrorKind::TimedOut`.
///
/// This exists for hub-side *probes* (workspace liveness for `shelbi
/// workspace list` / `status --full`). `ConnectTimeout` + `BatchMode` bound
/// most SSH failure modes, but not all of them: Tailscale SSH's web-auth
/// interception accepts the TCP connection and then parks the session on a
/// "To authenticate, visit …" prompt that runs outside the openssh client,
/// so BatchMode never sees it and the child blocks forever. A wall-clock
/// deadline is the only bound that covers every such case.
///
/// stdin is `null` (a probe must never trigger an interactive prompt), and
/// stdout/stderr are drained on their own threads so a chatty child can't
/// deadlock the deadline loop on a full pipe buffer.
pub fn run_with_deadline<I, S>(
    host: &Host,
    argv: I,
    deadline: Duration,
) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = build_command(host, argv);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Put the child in its own process group so a timeout can kill the whole
    // tree, not just the direct child. Without this, killing e.g. a login
    // shell leaves its grandchildren (`cargo test` and everything it spawned)
    // running — and, worse, still holding the write ends of our stdout/stderr
    // pipes, so the reader threads below never see EOF and the deadline path
    // blocks until the orphans finish anyway. `process_group(0)` makes the
    // child a group leader (pgid == pid); we signal `-pid` on timeout.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    tracing::debug!(?cmd, host = ?host, ?deadline, "ssh::run_with_deadline");
    let mut child = cmd.spawn()?;

    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= deadline {
            // Kill the whole process group (best-effort — the child may have
            // exited in the gap), then reap so the long-lived hub daemon
            // doesn't accumulate zombies. The group signal reaches any
            // grandchildren the child spawned; `child.kill()` is the fallback
            // for the (non-unix / group-setup-failed) case where we only have
            // the direct child.
            #[cfg(unix)]
            {
                // Safety: `kill(2)` with a negative pid signals the process
                // group; no memory is touched. `child.id()` is the group
                // leader's pid because we set `process_group(0)` above.
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            // The kill closed the pipes, so the readers see EOF and finish.
            let _ = stdout_reader.join();
            // Surface whatever the child wrote to stderr *before* the kill —
            // a Tailscale-SSH web-auth wedge, for instance, prints its
            // "To authenticate, visit …" line and then parks, and that line is
            // the actionable diagnostic. Without folding it in, the timeout
            // error would be as blank as the `exit 255` case this task fixes.
            let partial = stderr_reader.join().unwrap_or_default();
            let partial = String::from_utf8_lossy(&partial);
            let partial = partial.trim();
            let msg = if partial.is_empty() {
                format!("command did not finish within {deadline:?}")
            } else {
                format!("command did not finish within {deadline:?}; stderr before kill: {partial}")
            };
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, msg));
        }
        std::thread::sleep(DEADLINE_POLL);
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
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
            // Never a blank tail: a broken/absent ControlMaster (`exit 255`
            // with empty stderr) gets a Shelbi-side annotation of the master
            // state instead. See [`annotated_stderr`].
            stderr: annotated_stderr(host, &output),
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
            // Annotated so a broken-master `exit 255` doesn't reach the
            // operator as a blank `--- stderr ---`. See [`annotated_stderr`].
            stderr: annotated_stderr(host, &output),
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

/// Read-only `ssh -O check` against the shelbi ControlPath for `hostname`,
/// returning ssh's own one-line verdict about the master. Unlike
/// [`drop_master`] this never opens or tears anything down — it only asks
/// whether a master is alive — so it is safe to run on a *failure* path to
/// explain a blank `--- stderr ---`. ssh writes the verdict to stderr
/// (`Master running (pid=…)` on success, `Control socket connect(…): No such
/// file or directory` when the socket is gone); we fall back to stdout and
/// then to the exit status so the caller always gets a non-empty string.
fn master_check(hostname: &str) -> String {
    let mut cmd = Command::new("ssh");
    for o in control_path_opt() {
        cmd.arg(o);
    }
    cmd.arg("-O").arg("check").arg(hostname);
    match cmd.output() {
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let stderr = stderr.trim();
            if !stderr.is_empty() {
                return stderr.to_string();
            }
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stdout = stdout.trim();
            if !stdout.is_empty() {
                return stdout.to_string();
            }
            format!("no output ({})", o.status)
        }
        Err(e) => format!("could not spawn `ssh -O check`: {e}"),
    }
}

/// Is a live ControlMaster answering for `hostname`? A read-only `ssh -O check`
/// against the shelbi ControlPath: exit 0 means a master is up (and, since its
/// `-R` reverse forward rides on that master, the forward listener is up too);
/// any other result means no live master. Never opens or tears anything down,
/// so it is safe to call from startup reconciliation without side effects.
///
/// This is the reality probe [`shelbi_state::reconcile_forward_state`] uses to
/// release stale/leaked `forward-modes.json` entries: a persisted TCP port whose
/// master is dead has no listener behind it, so keeping the entry would both
/// point workers at a dead port and let the next sweep miscount it as occupied.
pub fn master_alive(hostname: &str) -> bool {
    let mut cmd = Command::new("ssh");
    for o in control_path_opt() {
        cmd.arg(o);
    }
    cmd.arg("-O").arg("check").arg(hostname);
    matches!(cmd.output(), Ok(o) if o.status.success())
}

/// Format the Shelbi-side annotation used when a failed command left an empty
/// stderr. Pure so the message shape is unit-testable without shelling out:
/// callers pass the exit `code` and, for an ssh command, the `master_state`
/// that [`master_check`] probed (`None` for a local command, where there is no
/// ControlMaster to inspect).
fn format_blank_stderr_annotation(code: Option<i32>, master_state: Option<&str>) -> String {
    let exit = match code {
        Some(c) => format!("exit {c}"),
        None => "terminated by signal".to_string(),
    };
    match master_state {
        Some(state) => format!(
            "<shelbi: ssh produced no stderr ({exit}); \
             ControlMaster probe `ssh -O check`: {state}>"
        ),
        None => format!("<shelbi: command produced no stderr ({exit})>"),
    }
}

/// The stderr text to attach to a failed command's error, **guaranteed
/// non-empty** so a managed-SSH failure never surfaces as a blank
/// `--- stderr ---`. When the child left a diagnostic on stderr it is returned
/// verbatim (this is the common, informative case — `Permission denied`,
/// `Connection refused`, `fatal: …`). When stderr is blank on a *failing*
/// command — the classic `exit status: 255` from a broken/absent ControlMaster
/// — it is annotated: for an ssh host with a cheap read-only `ssh -O check`
/// probe of the same ControlPath (so the operator sees whether the transport
/// was alive), for a local host with the exit code alone.
///
/// A *successful* command with empty stderr returns the empty string unchanged
/// — there is nothing to explain — so this is only meaningful on the error path.
pub fn annotated_stderr(host: &Host, output: &Output) -> String {
    let raw = String::from_utf8_lossy(&output.stderr);
    if !raw.trim().is_empty() || output.status.success() {
        return raw.into_owned();
    }
    let code = output.status.code();
    match host {
        Host::Ssh { host } => {
            let state = master_check(host);
            format_blank_stderr_annotation(code, Some(&state))
        }
        Host::Local => format_blank_stderr_annotation(code, None),
    }
}

/// A one-line `exit <status> — <stderr-or-annotation>` summary of a failed
/// command's [`Output`], for callers (e.g. the git branch-cut path) that fold a
/// remote failure into an [`shelbi_core::Error::Other`] message rather than the
/// structured [`shelbi_core::Error::Command`]. Wraps [`annotated_stderr`] so
/// those hand-rolled messages carry the exit code and the ssh diagnostic (or
/// ControlMaster annotation) instead of a bare, blank tail.
pub fn describe_failure(host: &Host, output: &Output) -> String {
    let stderr = annotated_stderr(host, output);
    let stderr = stderr.trim();
    format!("{}: {stderr}", output.status)
}

/// Assemble the argv (after the `ssh` program) for a Unix-socket master open
/// that rebinds the `-R` reverse forward with `StreamLocalBindUnlink=yes`:
/// the full control opts (which carry the `-R` spec), then the unlink option,
/// then `<host> -- true`. Split out so the arg shape is unit-testable without
/// shelling out — mirrors [`build_tcp_master_args`].
///
/// `StreamLocalBindUnlink=yes` is what lets the master rebind cleanly when a
/// stale remote landing socket is already sitting at the `-R` target path:
/// OpenSSH unlinks it on bind instead of failing the forward with
/// `landing_socket_missing`. It rides only on this dedicated master open, not
/// on ordinary multiplexed slave commands — a slave carrying it could replace
/// an already-healthy listener for only the lifetime of that one command.
fn build_stream_local_unlink_master_args(hostname: &str) -> Vec<String> {
    let mut args = build_ssh_control_opts(hostname);
    args.push("-o".to_string());
    args.push("StreamLocalBindUnlink=yes".to_string());
    args.push(hostname.to_string());
    args.push("--".to_string());
    args.push("true".to_string());
    args
}

/// Open a fresh ControlMaster with the reverse-forward unlink option enabled.
/// Callers must drop the existing master first; applying
/// StreamLocalBindUnlink to ordinary multiplexed slave commands could replace
/// an already-healthy listener for only the lifetime of that slave.
fn open_master_with_stream_local_unlink(hostname: &str) -> std::io::Result<Output> {
    let mut cmd = Command::new("ssh");
    for a in build_stream_local_unlink_master_args(hostname) {
        cmd.arg(a);
    }
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

/// Per-host reverse-forward health as last *reported to the shared board log*.
/// The forward health check runs on a ~120s cadence per workspace; without this
/// gate a declared-but-unreachable machine appends a `status=failed` line to the
/// shared `events.log` every cycle. That both floods the board feed and starves
/// the project heartbeat, whose emitter debounces against any `events.log`
/// advance — one unreachable box was observed dropping the heartbeat to one beat
/// per ~4.5h. We instead append to `events.log` only on a health *state change*
/// (ok→fail, fail→ok, or a change of failure detail) and mirror every cycle's
/// result to `tui.log` via tracing so a recurring failure stays diagnosable.
///
/// Value is a short signature: `"ok"` for healthy, `"fail:<detail>"` otherwise.
fn forward_health() -> &'static std::sync::Mutex<std::collections::HashMap<String, String>> {
    static HEALTH: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, String>>,
    > = std::sync::OnceLock::new();
    HEALTH.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Record `hostname`'s latest forward-health `signature` and report whether it
/// differs from the previous cycle's. `true` means the state changed and the
/// caller should append to `events.log`; `false` means suppress the append (the
/// same result already surfaced last cycle).
fn forward_health_changed(hostname: &str, signature: &str) -> bool {
    let mut map = forward_health().lock().unwrap_or_else(|p| p.into_inner());
    match map.get(hostname) {
        Some(prev) if prev == signature => false,
        _ => {
            map.insert(hostname.to_string(), signature.to_string());
            true
        }
    }
}

/// A reverse-forward health check *failed*. Always mirror it to `tui.log` so a
/// recurring failure stays diagnosable, but append to the shared `events.log`
/// only on a state change — see [`forward_health`]. `signature` distinguishes
/// failure kinds (`master_open_failed`, `loopback_port_exhausted`, …) so a
/// change in the failure *shape* still re-emits.
fn report_forward_failure(hostname: &str, signature: &str, body: &str) {
    tracing::warn!(host = %hostname, signature, "reverse-forward health check failed: {body}");
    if forward_health_changed(hostname, signature) {
        let _ = shelbi_state::emit_event_body(body);
    }
}

/// Record that `hostname`'s forward is healthy, without emitting anything. Call
/// on every clean success so a later failure is seen as a fresh state change and
/// re-emits (a fail→ok→fail sequence must produce two failure lines, not one).
fn mark_forward_ok(hostname: &str) {
    let _ = forward_health_changed(hostname, "ok");
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
    // keeps it) as a side effect. Retried across transient blips with
    // backoff: `master_open_failed` was observed recurring transiently for
    // devbox while direct ssh stayed healthy, so a short retry absorbs the
    // flap instead of surfacing (and re-surfacing) a hard failure. Between
    // failed tries we drop the half-open master so `ControlMaster=auto`
    // genuinely reopens fresh rather than reusing a wedged socket.
    let attempts = forward_retry_attempts();
    let opened_on = retry_master_open(
        attempts,
        forward_retry_backoff_base(),
        || matches!(open_master_with_stream_local_unlink(hostname), Ok(o) if o.status.success()),
        || drop_master(hostname),
    );
    let Some(opened_attempt) = opened_on else {
        // Every attempt failed to even open the master — a local/transient
        // problem, not the wedge.
        return UnixForwardOutcome::MasterOpenFailed;
    };
    if opened_attempt > 1 {
        // Health visibility: the master open flickered but self-healed. Emit
        // the recovery distinctly so a recurring blip stays visible without
        // masquerading as a hard `master_open_failed`.
        let _ = shelbi_state::emit_event_body(&format!(
            "ssh reverse-forward host={hostname} remote_sock={remote_sock} \
             detail=master_open_recovered attempts={opened_attempt} status=established"
        ));
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

/// Substrings the ssh client emits (with `ExitOnForwardFailure=yes`) when a `-R`
/// bind is refused because the remote loopback port is already taken — the
/// genuine "this port is occupied" fingerprint. Matched case-insensitively.
///
/// Crucially distinct from a *transport* failure (a churning/broken
/// ControlMaster, a reset connection, an unreachable host): those exit nonzero
/// too, but say nothing about whether the port is free — misreading them as
/// occupancy is exactly the false-exhaustion bug (a mux hiccup on all 64 ports
/// reported "all 64 in use" when `ss` showed them free).
const TCP_FORWARD_COLLISION_MARKERS: &[&str] = &[
    // OpenSSH client, ExitOnForwardFailure=yes, remote refused the bind:
    //   "Warning: remote port forwarding failed for listen port 47100"
    "remote port forwarding failed",
    // Defensive synonyms seen across ssh/sshd versions and setups.
    "forwarding request failed",
    "address already in use",
    "cannot listen to port",
];

/// Outcome of a single TCP-loopback `-R` master open against one candidate port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TcpForwardOpen {
    /// The master opened and (via `ExitOnForwardFailure=yes`) the `-R` bound:
    /// the port is ours.
    Bound,
    /// The remote refused to bind the port — it is genuinely occupied. The sweep
    /// should advance to the next candidate.
    PortCollision,
    /// The master could not be established at all: a churning/broken
    /// ControlMaster, a reset connection, an unreachable host, a refused auth.
    /// This is NOT evidence the port is in use, so it must never be counted as an
    /// occupied port. The sweep aborts and reports a transient, recoverable
    /// failure the next ~120s recheck re-probes — rather than latching a bogus
    /// "band exhausted."
    Transport,
}

/// Classify a TCP-loopback master-open [`Output`]. Pure so the
/// free/collision/transient decision is unit-testable without a real ssh
/// transport. Success is [`TcpForwardOpen::Bound`]; a failure whose stderr
/// carries a port-bind-collision signature is [`TcpForwardOpen::PortCollision`];
/// every *other* failure (blank 255, mux churn, connection reset, unreachable,
/// refused auth) is [`TcpForwardOpen::Transport`] — a transport loss that says
/// nothing about port occupancy. Defaulting the unknown case to `Transport`
/// (not `PortCollision`) is deliberate: at worst we re-probe next cycle, which
/// recovers; the opposite mistake latches false exhaustion.
fn classify_tcp_forward_open(output: &Output) -> TcpForwardOpen {
    if output.status.success() {
        return TcpForwardOpen::Bound;
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if TCP_FORWARD_COLLISION_MARKERS.iter().any(|m| stderr.contains(m)) {
        return TcpForwardOpen::PortCollision;
    }
    TcpForwardOpen::Transport
}

/// The terminal decision of a candidate-port sweep. Pure result type so
/// [`decide_tcp_sweep`] can be tested independently of the side-effecting opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TcpSweepResult {
    /// A candidate bound — worker→hub delivery works on this port.
    Bound(u16),
    /// The transport flaked partway through the sweep (at this port). NOT
    /// exhaustion: the remaining ports would fail identically and say nothing
    /// about occupancy, so we bail with a recoverable error and re-probe.
    TransportUnstable(u16),
    /// Every candidate reported a *genuine* bind collision — real exhaustion.
    Exhausted,
}

/// Fold a lazily-evaluated sequence of `(port, outcome)` bind attempts into the
/// terminal [`TcpSweepResult`]. Stops at the first `Bound` (success) or
/// `Transport` (transient — do not keep sweeping a flaky transport as though the
/// ports were occupied); a `PortCollision` advances to the next candidate. Only
/// a run in which *every* candidate collided yields [`TcpSweepResult::Exhausted`]
/// — that is the sole path to a legitimate "band exhausted" report. Consuming an
/// iterator (rather than a materialized `Vec`) preserves the caller's laziness:
/// the expensive per-port open only runs until the decision is reached.
fn decide_tcp_sweep<I>(outcomes: I) -> TcpSweepResult
where
    I: IntoIterator<Item = (u16, TcpForwardOpen)>,
{
    for (port, outcome) in outcomes {
        match outcome {
            TcpForwardOpen::Bound => return TcpSweepResult::Bound(port),
            TcpForwardOpen::PortCollision => continue,
            TcpForwardOpen::Transport => return TcpSweepResult::TransportUnstable(port),
        }
    }
    TcpSweepResult::Exhausted
}

/// Attempt to bind the TCP-loopback forward on one `port`, absorbing a transient
/// ControlMaster blip. Drops any existing master first (so `ControlMaster=auto`
/// opens a fresh one that binds *this* port's `-R`), opens, and classifies. A
/// [`TcpForwardOpen::Transport`] result is retried with the shared backoff
/// schedule — a master churning mid-sweep must not be read as "port occupied" —
/// while a [`TcpForwardOpen::PortCollision`] (genuinely occupied) and a
/// [`TcpForwardOpen::Bound`] (success) both return immediately. A failure to
/// even spawn `ssh` is transport-shaped.
fn try_bind_tcp_forward_port(hostname: &str, port: u16) -> TcpForwardOpen {
    let attempts = forward_retry_attempts();
    let delays = backoff_delays(attempts, forward_retry_backoff_base());
    let mut outcome = TcpForwardOpen::Transport;
    for i in 0..attempts {
        drop_master(hostname);
        outcome = match open_master_tcp(hostname, port) {
            Ok(o) => classify_tcp_forward_open(&o),
            Err(_) => TcpForwardOpen::Transport,
        };
        match outcome {
            TcpForwardOpen::Bound | TcpForwardOpen::PortCollision => return outcome,
            TcpForwardOpen::Transport => {
                if let Some(delay) = delays.get(i as usize) {
                    std::thread::sleep(*delay);
                }
            }
        }
    }
    outcome
}

/// Candidate loopback ports to try, starting from `start` (the port a previous
/// forward bound, if any) and sweeping the configured band on a collision.
fn tcp_candidate_ports(start: u16) -> Vec<u16> {
    let base = shelbi_state::tcp_forward_port_base();
    let span = shelbi_state::tcp_forward_port_span();
    // Widen to u32 for the band arithmetic: the band is clamped so that
    // `base + span - 1 == u16::MAX` is legal, and computing `base + span` in
    // u16 there would overflow (panic in debug builds).
    let (base32, span32) = (base as u32, span as u32);
    // Normalize `start` into the band so a stale/out-of-range persisted port
    // can't push us outside it.
    let start32 = start as u32;
    let first = if start32 >= base32 && start32 < base32 + span32 {
        start
    } else {
        base
    };
    let mut ports = Vec::with_capacity(span as usize);
    ports.push(first);
    for i in 0..span32 {
        let p = (base32 + i) as u16;
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
    // we reach the host at all?" Retried across transient blips with backoff:
    // the whole point of the gate is to prove reachability so a later bind
    // failure is unambiguously exhaustion, not a network hiccup — so a *flaky*
    // probe must not be mistaken for an unreachable host.
    let attempts = forward_retry_attempts();
    let reached_on = retry_master_open(
        attempts,
        forward_retry_backoff_base(),
        || {
            matches!(
                run_without_reverse_forward(hostname, ["true"]),
                Ok(o) if o.status.success()
            )
        },
        || {},
    );
    let Some(gate_attempt) = reached_on else {
        report_forward_failure(
            hostname,
            "fail:tcp:master_open_failed",
            &format!(
                "ssh reverse-forward host={hostname} mode=tcp status=failed \
                 detail=master_open_failed attempts={attempts}"
            ),
        );
        return Err(shelbi_core::Error::Other(format!(
            "ssh reverse forward to {hostname} could not be established over TCP loopback \
             (host unreachable after {attempts} attempts); worker→hub messages will not be delivered"
        )));
    };
    if gate_attempt > 1 {
        // Health visibility: the probe flickered but self-healed. Emit the
        // recovery distinctly so a recurring blip is visible in events.log
        // without masquerading as a hard failure.
        let _ = shelbi_state::emit_event_body(&format!(
            "ssh reverse-forward host={hostname} mode=tcp detail=master_open_recovered \
             attempts={gate_attempt} status=established"
        ));
    }

    // Reclaim before allocating: drop any master we still own for this host so
    // the loopback port it was holding is released back into the band before we
    // start the sweep. Without this, a still-persisted master from the previous
    // recheck keeps "its" port bound and we'd needlessly skip past it.
    drop_master(hostname);

    let start = shelbi_state::load_host_forward(hostname)
        .and_then(|h| h.port)
        .unwrap_or_else(shelbi_state::tcp_forward_port_base);

    let candidates = tcp_candidate_ports(start);
    let band_len = candidates.len();
    // Sweep the band, binding the first free port. Each candidate is opened
    // lazily via the mapped iterator, so `decide_tcp_sweep` stops the moment it
    // reaches a decision — the expensive per-port master open only runs until a
    // port binds, the transport flakes, or the band is exhausted. Only a
    // *genuine* bind collision (`TcpForwardOpen::PortCollision`) advances to the
    // next port; a transport blip is retried per-port and, if it persists, aborts
    // the sweep as transient rather than being miscounted as an occupied port.
    let outcomes = candidates
        .into_iter()
        .map(|port| (port, try_bind_tcp_forward_port(hostname, port)));
    match decide_tcp_sweep(outcomes) {
        TcpSweepResult::Bound(port) => {
            // ExitOnForwardFailure=yes guarantees the `-R` bound when the master
            // opened. Remember the mode + port so subsequent outbound ssh (and
            // the worker env) reuse this exact port; this also *releases* any
            // stale port previously persisted for the host (overwrite). Success
            // is silent — like the Unix path, we only log failures, so the 120s
            // rechecks don't flood events.log.
            let _ = shelbi_state::save_host_forward(
                hostname,
                Some(shelbi_state::HostForward {
                    mode: shelbi_core::ForwardMode::Tcp,
                    port: Some(port),
                }),
            );
            // Healthy: record it so a later failure is seen as a state change
            // and re-emits (without this, fail→ok→fail would emit only once).
            mark_forward_ok(hostname);
            Ok(port)
        }
        TcpSweepResult::TransportUnstable(port) => {
            // A churning/broken transport, not a full band. Surface it
            // *distinctly* from genuine exhaustion so a re-probe can recover: the
            // per-workspace ~120s recheck re-enters this path and binds once the
            // master is healthy again. Never latches "all ports in use" — the bug
            // this fixes (a mux hiccup across every port read as 64 occupied
            // listeners when `ss` showed them free). State-change gated so a
            // recurring flap logs once, not once per cycle.
            report_forward_failure(
                hostname,
                "fail:tcp:forward_transport_unstable",
                &format!(
                    "ssh reverse-forward host={hostname} mode=tcp status=failed \
                     detail=forward_transport_unstable port={port}"
                ),
            );
            Err(shelbi_core::Error::Other(format!(
                "ssh reverse forward to {hostname} could not bind a TCP loopback port \
                 (transport unstable at port {port}); Shelbi will re-probe — \
                 worker→hub messages are delayed, not disabled"
            )))
        }
        TcpSweepResult::Exhausted => {
            // Genuine exhaustion: every port in the band reported a real bind
            // collision even though the host is reachable. Surface it distinctly
            // (`detail=loopback_port_exhausted`, with the band that was swept) so
            // an operator can tell "widen the band / find the port hog" apart
            // from "the transport flickered." Widening the band
            // (SHELBI_TCP_FORWARD_PORT_SPAN) or moving it
            // (SHELBI_TCP_FORWARD_PORT_BASE) is the remedy this points at.
            let base = shelbi_state::tcp_forward_port_base();
            let band_hi = base as u32 + band_len.saturating_sub(1) as u32;
            report_forward_failure(
                hostname,
                "fail:tcp:loopback_port_exhausted",
                &format!(
                    "ssh reverse-forward host={hostname} mode=tcp status=failed \
                     detail=loopback_port_exhausted band={base}-{band_hi} tried={band_len}"
                ),
            );
            Err(shelbi_core::Error::Other(format!(
                "ssh reverse forward to {hostname} could not bind a TCP loopback port \
                 (all {band_len} ports in band {base}-{band_hi} in use); \
                 worker→hub messages will not be delivered"
            )))
        }
    }
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
    // Serialize this host's master (re)creation against every other process —
    // the daemon poller, the TUI, a `task start` CLI — so a concurrent
    // drop_master + reopen here can't tear the master out from under another
    // party's in-flight command. Best-effort: if the lock can't be taken we
    // proceed rather than wedge the forward path (the pre-existing behavior).
    // Held for the whole ensure so the drop/reopen/probe sequence below is
    // atomic per host; distinct hosts still run concurrently.
    let _master_lock = shelbi_state::lock_ssh_master(&hostname);
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
        UnixForwardOutcome::Ok => {
            // Healthy: record it so a later failure re-emits (state change).
            mark_forward_ok(&hostname);
            Ok(())
        }
        UnixForwardOutcome::MasterOpenFailed => {
            // Transient / network — surface, never fall back (a connect
            // timeout is not the wedge). We only reach here after the retry
            // budget in `ensure_unix_forward` is exhausted, so record how many
            // attempts we burned: a `master_open_failed attempts=3` that keeps
            // recurring is a real outage, not a one-off blip the retry would
            // have caught. State-change gated so a steadily-unreachable host
            // logs once (not once per 120s cycle) — see `forward_health`.
            let attempts = forward_retry_attempts();
            report_forward_failure(
                &hostname,
                "fail:unix:master_open_failed",
                &format!(
                    "ssh reverse-forward host={hostname} remote_sock={remote_sock} \
                     status=failed detail=master_open_failed attempts={attempts}"
                ),
            );
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
                // this branch). `ensure_tcp_forward` marks the host healthy on
                // success, so the established line here follows the same
                // state-change discipline as every other outcome.
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
                // State-change gated so a persistently-wedged pinned host logs
                // once, not every cycle.
                report_forward_failure(
                    &hostname,
                    &format!("fail:unix:{detail}"),
                    &format!(
                        "ssh reverse-forward host={hostname} remote_sock={remote_sock} \
                         status=failed detail={detail}"
                    ),
                );
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
    fn run_with_deadline_returns_output_for_a_fast_child() {
        let out = run_with_deadline(&Host::Local, ["echo", "shelbi"], Duration::from_secs(10))
            .expect("fast echo must not time out");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "shelbi");
    }

    #[test]
    fn run_with_deadline_kills_a_hung_child_and_reports_timed_out() {
        // Models the Tailscale-SSH auth wedge: a child that accepts the
        // connection (spawns fine) and then never exits. The deadline must
        // kill it and surface TimedOut — well before the child's own 30s.
        let start = std::time::Instant::now();
        let err = run_with_deadline(&Host::Local, ["sleep", "30"], Duration::from_millis(200))
            .expect_err("hung child must time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "err: {err}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "deadline enforcement took {:?}",
            start.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_with_deadline_kills_the_whole_process_group() {
        // The real hub failure: the timed-out command is a *shell* that has
        // spawned a long-running grandchild (`cargo test` and its tree). If we
        // only kill the direct child, the grandchild keeps running AND keeps
        // the inherited stdout/stderr pipes open, so the deadline path blocks
        // on the pipe readers until the grandchild finishes on its own. The
        // process-group kill must reach the grandchild so this returns fast.
        let start = std::time::Instant::now();
        let err = run_with_deadline(
            &Host::Local,
            // `exec` would make sleep the direct child; we deliberately keep
            // the shell as the leader and sleep as a distinct grandchild.
            ["sh", "-c", "sleep 30"],
            Duration::from_millis(200),
        )
        .expect_err("hung grandchild must time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "err: {err}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "group kill must not block on the orphaned grandchild; took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn run_with_deadline_captures_nonzero_exit_and_stderr() {
        // A probe classifier needs the exit status and stderr of a child
        // that *did* answer (e.g. ssh exiting 255 with its diagnostic).
        let out = run_with_deadline(
            &Host::Local,
            ["sh", "-c", "echo denied >&2; exit 255"],
            Duration::from_secs(10),
        )
        .expect("child ran to completion");
        assert_eq!(out.status.code(), Some(255));
        assert!(String::from_utf8_lossy(&out.stderr).contains("denied"));
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
    fn stream_local_unlink_master_args_carry_the_unlink_option_and_reverse_forward() {
        // Issue #319, fix #2: the master that (re)binds the Unix `-R` forward
        // must set StreamLocalBindUnlink=yes so a pre-existing/stale remote
        // landing socket at the target path is unlinked on bind instead of
        // wedging the forward with a persistent `landing_socket_missing` loop.
        let args = build_stream_local_unlink_master_args("devbox");

        assert!(
            args.windows(2)
                .any(|w| w == ["-o", "StreamLocalBindUnlink=yes"]),
            "master open must carry StreamLocalBindUnlink=yes: {args:?}"
        );

        // It still carries the reverse forward it is repairing — the unlink
        // option is meaningless without the `-R` bind it protects.
        assert!(
            args.iter().any(|a| a == "-R"),
            "master open must still carry the -R reverse forward: {args:?}"
        );

        // Ends with `<host> -- true` — the cheapest remote no-op that opens
        // (and, via ControlPersist, keeps) the master.
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

    #[test]
    fn backoff_delays_grows_exponentially_and_has_attempts_minus_one_entries() {
        let base = Duration::from_millis(100);

        // A single attempt means no waiting — you just try once.
        assert!(backoff_delays(1, base).is_empty());
        assert!(backoff_delays(0, base).is_empty());

        // N attempts → N-1 sleeps, doubling from `base`.
        let d = backoff_delays(4, base);
        assert_eq!(
            d,
            vec![
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(400),
            ]
        );

        // A zero base disables the wait entirely regardless of attempt count.
        assert!(backoff_delays(5, Duration::ZERO)
            .iter()
            .all(|d| d.is_zero()));

        // A large attempt count can't overflow the shift/multiply.
        let big = backoff_delays(40, base);
        assert_eq!(big.len(), 39);
        assert!(big.iter().all(|d| *d <= Duration::MAX));
    }

    #[test]
    fn retry_master_open_reports_first_successful_attempt() {
        // Succeeds immediately → attempt 1, on_retry never fires.
        let mut retries = 0;
        let got = retry_master_open(3, Duration::ZERO, || true, || retries += 1);
        assert_eq!(got, Some(1));
        assert_eq!(retries, 0, "no retry callback when the first try wins");
    }

    #[test]
    fn retry_master_open_absorbs_a_transient_blip() {
        // Fails twice, then succeeds on the third attempt. on_retry fires once
        // per *failed* try that is followed by another attempt (2 here).
        let mut calls = 0;
        let mut retries = 0;
        let got = retry_master_open(
            3,
            Duration::ZERO,
            || {
                calls += 1;
                calls == 3
            },
            || retries += 1,
        );
        assert_eq!(got, Some(3), "self-heals on the third attempt");
        assert_eq!(calls, 3);
        assert_eq!(retries, 2, "one retry hook per failed-but-retried attempt");
    }

    #[test]
    fn retry_master_open_gives_up_after_exhausting_attempts() {
        // Never succeeds → None, and on_retry fires only between tries
        // (attempts - 1), never after the final failed attempt.
        let mut calls = 0;
        let mut retries = 0;
        let got = retry_master_open(
            3,
            Duration::ZERO,
            || {
                calls += 1;
                false
            },
            || retries += 1,
        );
        assert_eq!(got, None);
        assert_eq!(calls, 3, "used the whole attempt budget");
        assert_eq!(retries, 2, "no backoff sleep after the last attempt");
    }

    #[test]
    fn forward_retry_config_clamps_to_safe_bounds() {
        // Attempts clamp into 1..=10; a zero or absurd value can't disable the
        // single guaranteed try or spin forever.
        std::env::set_var("SHELBI_FORWARD_RETRY_ATTEMPTS", "0");
        assert_eq!(forward_retry_attempts(), 1);
        std::env::set_var("SHELBI_FORWARD_RETRY_ATTEMPTS", "9999");
        assert_eq!(forward_retry_attempts(), 10);
        std::env::set_var("SHELBI_FORWARD_RETRY_ATTEMPTS", "not-a-number");
        assert_eq!(forward_retry_attempts(), DEFAULT_FORWARD_RETRY_ATTEMPTS);
        std::env::remove_var("SHELBI_FORWARD_RETRY_ATTEMPTS");

        // Backoff clamps to <= 5s so a fat-fingered value can't stall the
        // per-workspace poller thread for minutes.
        std::env::set_var("SHELBI_FORWARD_RETRY_BACKOFF_MS", "100000");
        assert_eq!(forward_retry_backoff_base(), Duration::from_millis(5_000));
        std::env::set_var("SHELBI_FORWARD_RETRY_BACKOFF_MS", "garbage");
        assert_eq!(
            forward_retry_backoff_base(),
            Duration::from_millis(DEFAULT_FORWARD_RETRY_BACKOFF_MS)
        );
        std::env::remove_var("SHELBI_FORWARD_RETRY_BACKOFF_MS");
    }

    /// A steadily-unreachable declared machine must not append a
    /// `status=failed` line to the shared `events.log` on every ~120s health
    /// check — that flood is what starved the project heartbeat (whose emitter
    /// debounces against any `events.log` advance). The state-change gate under
    /// `report_forward_failure` collapses a recurring identical failure to a
    /// single board-log append; a genuine transition (fail→ok→fail, or a change
    /// of failure shape) is the only thing that re-emits.
    #[test]
    fn forward_health_gate_emits_once_per_state_change() {
        // Unique host so the process-global health map starts clean for this
        // test regardless of what else ran (the map is keyed by hostname).
        let host = format!("unreachable-devbox-{}", std::process::id());

        // First failure of a given shape is a state change → emit.
        assert!(
            forward_health_changed(&host, "fail:tcp:master_open_failed"),
            "first sighting of a failure must emit"
        );
        // The next four cycles report the *same* failure → all suppressed. This
        // is the anti-flood guarantee: 5 cycles, 1 board-log line.
        for _ in 0..4 {
            assert!(
                !forward_health_changed(&host, "fail:tcp:master_open_failed"),
                "a steady, unchanged failure must not re-emit every cycle"
            );
        }

        // Recovery is a state change (fail→ok)…
        assert!(forward_health_changed(&host, "ok"));
        assert!(!forward_health_changed(&host, "ok"), "steady health is quiet");
        // …and a fresh failure after recovery re-emits (ok→fail).
        assert!(
            forward_health_changed(&host, "fail:tcp:master_open_failed"),
            "a failure after recovery must emit again"
        );
        // A change of failure *shape* (different detail) also re-emits, so an
        // exhaustion doesn't hide behind a prior master-open failure.
        assert!(
            forward_health_changed(&host, "fail:tcp:loopback_port_exhausted"),
            "a different failure detail must emit"
        );
    }

    #[test]
    fn format_blank_stderr_annotation_is_never_blank() {
        // The ssh variant folds in the exit code and the ControlMaster probe
        // result — this is what replaces a blank `--- stderr ---` on a
        // broken-master `exit 255`.
        let ssh = format_blank_stderr_annotation(
            Some(255),
            Some("Control socket connect(/x): No such file or directory"),
        );
        assert!(!ssh.trim().is_empty());
        assert!(ssh.contains("255"), "must name the exit code: {ssh}");
        assert!(ssh.contains("ssh -O check"), "must name the probe: {ssh}");
        assert!(ssh.contains("No such file"), "must carry the probe result: {ssh}");

        // The local variant still annotates (no master to probe, but never blank).
        let local = format_blank_stderr_annotation(Some(255), None);
        assert!(!local.trim().is_empty());
        assert!(local.contains("255"), "must name the exit code: {local}");

        // A signal-terminated child (no exit code) is still described.
        let sig = format_blank_stderr_annotation(None, None);
        assert!(sig.contains("signal"), "signal death must be described: {sig}");
    }

    #[test]
    fn annotated_stderr_passes_through_a_real_diagnostic() {
        // When the child actually wrote to stderr, we return it verbatim —
        // the annotation only kicks in on the blank case.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo 'Permission denied (publickey).' >&2; exit 255")
            .output()
            .expect("sh failed");
        let got = annotated_stderr(&Host::Local, &output);
        assert!(got.contains("Permission denied"), "verbatim stderr: {got}");
        assert!(!got.contains("<shelbi:"), "no annotation when stderr present: {got}");
    }

    #[test]
    fn annotated_stderr_annotates_a_255_with_empty_stderr() {
        // Acceptance: given an Output with status 255 and empty stderr, the
        // formatter produces an annotated (non-blank) message. Local host so
        // the test is hermetic (no ssh spawn), but the code path is identical.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 255")
            .output()
            .expect("sh failed");
        assert_eq!(output.status.code(), Some(255));
        assert!(output.stderr.is_empty(), "precondition: blank stderr");

        let got = annotated_stderr(&Host::Local, &output);
        assert!(!got.trim().is_empty(), "annotation must never be blank");
        assert!(got.contains("255"), "annotation must name the exit code: {got}");
    }

    #[test]
    fn annotated_stderr_leaves_a_successful_command_empty() {
        // A success with no stderr has nothing to explain — don't manufacture
        // a spurious annotation (or spawn an `ssh -O check`) on the happy path.
        let output = std::process::Command::new("true").output().expect("true failed");
        assert!(annotated_stderr(&Host::Local, &output).is_empty());
    }

    #[test]
    fn describe_failure_carries_exit_code_and_annotation() {
        // The hand-rolled-message path (git branch cut) gets both the status
        // and a non-blank tail from one call.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 255")
            .output()
            .expect("sh failed");
        let desc = describe_failure(&Host::Local, &output);
        assert!(desc.contains("255"), "must carry the exit code: {desc}");
        assert!(!desc.trim_end_matches(':').trim().is_empty());
        // The annotation, not a bare trailing colon with nothing after it.
        assert!(desc.contains("<shelbi:"), "must carry the annotation: {desc}");
    }

    #[test]
    fn run_with_deadline_timeout_surfaces_partial_stderr() {
        // A child that writes a diagnostic and then hangs (the Tailscale-SSH
        // web-auth wedge shape) must have that line folded into the TimedOut
        // error, not discarded.
        let err = run_with_deadline(
            &Host::Local,
            ["sh", "-c", "echo 'To authenticate, visit https://…' >&2; sleep 30"],
            Duration::from_millis(300),
        )
        .expect_err("hung child must time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "err: {err}");
        assert!(
            err.to_string().contains("To authenticate"),
            "timeout error must surface pre-kill stderr: {err}"
        );
    }

    #[test]
    fn ssh_log_level_defaults_to_error_and_honors_override() {
        std::env::remove_var("SHELBI_SSH_LOG_LEVEL");
        assert_eq!(ssh_log_level(), "ERROR");

        std::env::set_var("SHELBI_SSH_LOG_LEVEL", "DEBUG1");
        assert_eq!(ssh_log_level(), "DEBUG1");

        // Whitespace-only / empty falls back to the quiet default.
        std::env::set_var("SHELBI_SSH_LOG_LEVEL", "   ");
        assert_eq!(ssh_log_level(), "ERROR");

        std::env::remove_var("SHELBI_SSH_LOG_LEVEL");

        // The chosen level rides in the control opts every ssh command carries.
        std::env::set_var("SHELBI_SSH_LOG_LEVEL", "DEBUG1");
        let opts = build_ssh_control_opts("devbox");
        assert!(
            opts.iter().any(|o| o == "LogLevel=DEBUG1"),
            "override must reach the ssh argv: {opts:?}"
        );
        std::env::remove_var("SHELBI_SSH_LOG_LEVEL");
    }

    #[test]
    fn master_control_opts_carry_keepalive() {
        // Acceptance: the master carries keepalive opts so a long remote op /
        // idle NAT blip doesn't silently drop it. They ride on every
        // control-master-carrying invocation via `base_control_opts`.
        let opts = build_ssh_control_opts("devbox");
        assert!(
            opts.iter().any(|o| o == "ServerAliveInterval=15"),
            "missing ServerAliveInterval: {opts:?}"
        );
        assert!(
            opts.iter().any(|o| o == "ServerAliveCountMax=4"),
            "missing ServerAliveCountMax: {opts:?}"
        );
        assert!(
            opts.iter().any(|o| o == "TCPKeepAlive=yes"),
            "missing TCPKeepAlive: {opts:?}"
        );
        // The TCP master open and the stream-local-unlink master open both build
        // on `base_control_opts`, so they inherit keepalive too.
        let tcp = build_tcp_master_args("devbox", "127.0.0.1:47100:/h/hub.sock");
        assert!(tcp.iter().any(|o| o == "ServerAliveInterval=15"));
        let unlink = build_stream_local_unlink_master_args("devbox");
        assert!(unlink.iter().any(|o| o == "ServerAliveInterval=15"));
    }

    #[test]
    fn no_forward_fallback_command_carries_keepalive() {
        // The non-multiplexed fallback carries a *long* op (a fresh-connection
        // `git worktree add` retry), so it needs the same dead-peer detection.
        let cmd = build_no_forward_command("devbox", ["git", "worktree", "add", "wt", "main"]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|a| a == "ServerAliveInterval=15"),
            "no-forward fallback must carry keepalive: {args:?}"
        );
        assert!(args.iter().any(|a| a == "TCPKeepAlive=yes"), "{args:?}");
    }

    /// Build an [`Output`] from a local `sh -c` so the mux-failure classifier
    /// can be exercised without a real ssh transport.
    fn output_from(script: &str) -> Output {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .expect("sh -c failed to run")
    }

    #[test]
    fn classify_mux_failure_flags_transport_losses() {
        // A blank 255 — the classic broken/absent-ControlMaster fingerprint.
        assert_eq!(
            classify_mux_failure(&output_from("exit 255")),
            MuxFailure::Transport
        );
        // The mux read-from-master broken-pipe signature.
        assert_eq!(
            classify_mux_failure(&output_from(
                "echo 'mux_client_request_session: read from master failed: Broken pipe' >&2; exit 255"
            )),
            MuxFailure::Transport
        );
        // The dead-control-socket signature.
        assert_eq!(
            classify_mux_failure(&output_from(
                "echo 'Control socket connect(/x/%C): No such file or directory' >&2; exit 255"
            )),
            MuxFailure::Transport
        );
        // Peer went away mid-transfer (the 43%-checkout shape).
        assert_eq!(
            classify_mux_failure(&output_from(
                "echo 'Connection closed by remote host' >&2; exit 255"
            )),
            MuxFailure::Transport
        );
    }

    #[test]
    fn classify_mux_failure_leaves_genuine_errors_alone() {
        // Success is never a transport failure.
        assert_eq!(classify_mux_failure(&output_from("true")), MuxFailure::None);
        // A remote command that merely failed exits with its OWN (non-255)
        // code — reopening the master wouldn't help, so don't.
        assert_eq!(
            classify_mux_failure(&output_from("echo 'fatal: bad ref' >&2; exit 128")),
            MuxFailure::None
        );
        // A 255 carrying a real ssh auth/host diagnostic a reopen can't fix.
        assert_eq!(
            classify_mux_failure(&output_from(
                "echo 'Permission denied (publickey).' >&2; exit 255"
            )),
            MuxFailure::None
        );
        assert_eq!(
            classify_mux_failure(&output_from("echo 'Connection refused' >&2; exit 255")),
            MuxFailure::None
        );
    }

    #[test]
    fn classify_tcp_forward_open_distinguishes_bind_from_transport() {
        // Success → the port bound; it's ours.
        assert_eq!(
            classify_tcp_forward_open(&output_from("true")),
            TcpForwardOpen::Bound
        );

        // The genuine bind-collision fingerprint (ExitOnForwardFailure=yes):
        // the remote refused to listen on the port because it is occupied.
        assert_eq!(
            classify_tcp_forward_open(&output_from(
                "echo 'Warning: remote port forwarding failed for listen port 47100' >&2; exit 255"
            )),
            TcpForwardOpen::PortCollision
        );

        // The false-exhaustion shapes: a churning ControlMaster and friends. A
        // blank 255, a mux read-from-master loss, a reset connection, and a
        // refused auth all exit nonzero but say NOTHING about port occupancy, so
        // they must be Transport (re-probe, recover) — never PortCollision (which
        // would let a mux hiccup across the whole band latch "all ports in use").
        for script in [
            "exit 255",
            "echo 'mux_client_request_session: read from master failed: Broken pipe' >&2; exit 255",
            "echo 'Connection reset by peer' >&2; exit 255",
            "echo 'Connection refused' >&2; exit 255",
            "echo 'Permission denied (publickey).' >&2; exit 255",
        ] {
            assert_eq!(
                classify_tcp_forward_open(&output_from(script)),
                TcpForwardOpen::Transport,
                "script should classify as Transport: {script}"
            );
        }
    }

    #[test]
    fn decide_tcp_sweep_binds_first_free_port() {
        use TcpForwardOpen::*;
        // A free band (first candidate binds) → Bound on that port.
        assert_eq!(
            decide_tcp_sweep([(47100, Bound)]),
            TcpSweepResult::Bound(47100)
        );
        // Genuine collisions are skipped until a free port is found.
        assert_eq!(
            decide_tcp_sweep([(47100, PortCollision), (47101, PortCollision), (47102, Bound)]),
            TcpSweepResult::Bound(47102)
        );
    }

    #[test]
    fn decide_tcp_sweep_transport_blip_does_not_latch_exhaustion() {
        use TcpForwardOpen::*;
        // A transport failure partway through is surfaced as transient (at the
        // port it happened on), NOT swept past as though the port were occupied
        // and NOT allowed to reach the exhaustion verdict.
        assert_eq!(
            decide_tcp_sweep([(47100, PortCollision), (47101, Transport), (47102, Bound)]),
            TcpSweepResult::TransportUnstable(47101),
        );
        // Even a transport blip on the very first candidate bails transient
        // rather than declaring the band exhausted.
        assert_eq!(
            decide_tcp_sweep([(47100, Transport)]),
            TcpSweepResult::TransportUnstable(47100),
        );
    }

    #[test]
    fn decide_tcp_sweep_reports_exhaustion_only_when_every_port_collides() {
        use TcpForwardOpen::*;
        // The ONLY path to "band exhausted": every candidate reported a genuine
        // bind collision. This is what makes a false-exhaustion report
        // impossible unless the ports really are all occupied.
        assert_eq!(
            decide_tcp_sweep([(47100, PortCollision), (47101, PortCollision)]),
            TcpSweepResult::Exhausted,
        );
        // An empty band is trivially exhausted (no candidate bound).
        assert_eq!(decide_tcp_sweep([]), TcpSweepResult::Exhausted);
    }

    #[test]
    fn decide_tcp_sweep_is_lazy_and_stops_at_the_decision() {
        use std::cell::Cell;
        use TcpForwardOpen::*;
        // The per-port open is expensive, so the fold must stop evaluating
        // candidates the moment it reaches a verdict. Prove it by counting how
        // many outcomes the lazily-mapped iterator actually produces.
        let evaluated = Cell::new(0);
        let ports = [47100u16, 47101, 47102, 47103];
        let result = decide_tcp_sweep(ports.iter().map(|&p| {
            evaluated.set(evaluated.get() + 1);
            // First candidate binds → nothing after it should be evaluated.
            (p, Bound)
        }));
        assert_eq!(result, TcpSweepResult::Bound(47100));
        assert_eq!(evaluated.get(), 1, "must not open ports past the first bind");
    }

    #[test]
    fn run_resilient_local_passes_through_without_recovery() {
        // Local has no multiplexed transport — resilient is a plain run, and a
        // success comes straight back.
        let out = run_resilient(&Host::Local, ["echo", "shelbi"]).expect("echo failed");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "shelbi");
    }

    #[test]
    fn run_resilient_with_cleanup_skips_cleanup_on_success() {
        // A first-attempt success must NOT run the cleanup closure (there is no
        // partial state to unwind on the happy path).
        let mut cleaned = 0;
        let out = run_resilient_with_cleanup(&Host::Local, ["true"], || cleaned += 1)
            .expect("true failed");
        assert!(out.status.success());
        assert_eq!(cleaned, 0, "cleanup must not fire on a clean first attempt");
    }
}
