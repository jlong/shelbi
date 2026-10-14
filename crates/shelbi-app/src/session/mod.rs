//! Showing a session in a client's main area: which session a view refers to,
//! how a client finds and attaches to it, and how it rides out the session
//! exiting or being relaunched. Shared by the TUI's `SessionManager` and the
//! desktop app's terminal views, so both clients behave the same.
//!
//! What lives here is toolkit-free and blocking: the clients run [`connect`]
//! and [`retry_connect`] off their UI thread (a worker thread in the TUI, the
//! background executor on the desktop) and feed the outcomes to their own
//! view state. The pieces:
//!
//! - [`SessionRef`] / [`discovery_name`]: the session a sidebar row or view
//!   names, and the backend's session name for it (`<project>/orch`, ...).
//! - [`connect`] / [`Connector`]: discovery plus the attach-with-replay, with
//!   the failure classification both clients rely on ([`ConnectFailure`]): a
//!   socket not bound yet is [`ConnectFailure::Starting`], and a missing
//!   orchestrator that is expected back (an exited one on disk, or a declared
//!   project) is [`ConnectFailure::Awaiting`], so a client that connects in the
//!   gap of a relaunch (`shelbi reload`, a crash, a daemon restart) keeps
//!   retrying instead of settling on a permanent attach error
//!   (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
//! - [`retry_connect`]: the bounded retry loop over those transient outcomes.
//! - [`RestartController`] ([`restart`]): the per-pane restart decision core
//!   over [`crate::supervision`], turning "the pane I'm showing died" into a
//!   restart, a backoff wait, a give-up, or standing down.
//! - [`Relauncher`] ([`relaunch`]): how a dead pane is brought back before
//!   the client re-attaches.
//! - The notices the views show ([`connecting_notice`], [`failed_notice`],
//!   [`restarting_notice`], [`gave_up_notice`], [`exit_line`]), so both
//!   clients word them identically.

pub mod relaunch;
pub mod restart;

use std::path::Path;
use std::time::{Duration, Instant};

use shelbi_client::{ClientError, Connection, SessionEvents};
use shelbi_proto::capability;

pub use relaunch::{
    NoopRelauncher, PersistentRelauncher, Relauncher, ReviewRelauncher,
    WorkspaceContentRelauncher,
};
pub use restart::{DeathAction, RestartController};

use crate::supervision::MAX_RESTARTS_IN_WINDOW;

/// A session's grid size in cells, as `info` reports it at attach time. Kept
/// here (rather than borrowing the emulator crate's `Size`) so this crate needn't
/// depend on the terminal emulator for one `(cols, rows)` pair; clients convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl GridSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

/// Which session a sidebar row or view refers to. The concrete session *name*
/// used for discovery is derived from the project ([`discovery_name`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionRef {
    /// The project's orchestrator chat session.
    Orchestrator,
    /// A dev workspace's session.
    Workspace(String),
    /// A review slot's content session: the editor or diff tool the review
    /// interface shows in a terminal view (`rt-tui-review`). `slot` is the
    /// review workspace, `role` is `editor` / `diff`. The daemon owns their
    /// lifetime; a client only attaches. The review slot's agent (chat) session
    /// is a [`SessionRef::Workspace`] of the slot name.
    Review { slot: String, role: String },
    /// A dev workspace's content session: the editor or diff tool the workspace
    /// sidebar shows in a terminal view. `workspace` is the dev workspace,
    /// `role` is `editor` / `diff`. The dev-workspace twin of
    /// [`SessionRef::Review`]: the daemon owns their lifetime; a client only
    /// attaches. The workspace's agent session is a [`SessionRef::Workspace`].
    WorkspaceContent { workspace: String, role: String },
}

impl SessionRef {
    /// A human label for the status line / title.
    pub fn display(&self) -> String {
        match self {
            SessionRef::Orchestrator => "orchestrator".to_string(),
            SessionRef::Workspace(w) => w.clone(),
            SessionRef::Review { slot, role } => format!("{slot} {role}"),
            SessionRef::WorkspaceContent { workspace, role } => format!("{workspace} {role}"),
        }
    }

    /// The session [`crate::nav::View::Session`] names: `orch` is the
    /// orchestrator, anything else a dev workspace.
    pub fn from_view_name(name: &str) -> Self {
        if name == "orch" {
            SessionRef::Orchestrator
        } else {
            SessionRef::Workspace(name.to_string())
        }
    }
}

/// The session backend's discovery name for a ref within `project`.
pub fn discovery_name(project: &str, r: &SessionRef) -> String {
    match r {
        SessionRef::Orchestrator => format!("{project}/orch"),
        SessionRef::Workspace(w) => format!("{project}/ws/{w}"),
        SessionRef::Review { slot, role } => format!("{project}/review/{slot}/{role}"),
        SessionRef::WorkspaceContent { workspace, role } => {
            format!("{project}/ws/{workspace}/{role}")
        }
    }
}

/// A freshly connected, attached session: its connection, its event stream
/// (starting with the attach replay), and its size at attach time.
pub struct Connected {
    pub conn: Connection,
    pub events: SessionEvents,
    pub size: GridSize,
}

/// Identity for the idle-workspace placeholder: a declared workspace the client
/// opened that has no live session (and no exited one to show a last line for).
/// The main area shows this instead of a bare "couldn't attach" error so the
/// sidebar selection and the main view agree (`rt-tui-idle-workspace-open`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleInfo {
    /// The workspace name.
    pub name: String,
    /// The machine the workspace is declared on.
    pub machine: String,
    /// The worktree's current branch, when known (local workspaces only).
    pub branch: Option<String>,
}

impl IdleInfo {
    /// Just the name, for a client that can't (or needn't) resolve the rest.
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            machine: String::new(),
            branch: None,
        }
    }
}

/// Why a connect didn't yield a live session.
#[derive(Debug)]
pub enum ConnectFailure {
    /// A declared workspace with no session at all — the main area shows the
    /// idle-workspace placeholder rather than an error.
    Idle(IdleInfo),
    /// The session was just launched (its `meta.json` is on disk and discovery
    /// reports it alive) but its socket isn't accepting connections yet —
    /// `Connection::open` returned ENOENT / ECONNREFUSED. This is *transient*:
    /// [`retry_connect`] retries it with a short backoff rather than surfacing
    /// an error, because the session's `sock` is bound a beat after its dir
    /// appears (`rt-tui-attach-retry-unbound-socket`). The string is the error
    /// shown only if the retries run out.
    Starting(String),
    /// No live session exists yet, but one is *expected* to (re)appear, so this
    /// is transient like [`Starting`](Self::Starting): [`retry_connect`] keeps
    /// retrying on the [`RetryPolicy`] cadence and attaches as soon as a live
    /// session shows up. Raised for the orchestrator when the project declares
    /// one (its config loads) or an exited session with its name is already on
    /// disk — the shape of a relaunch in progress: a daemon restart SIGTERMs the
    /// old `<project>/orch` session and spawns a fresh one a beat later, and a
    /// client that connects in that gap would otherwise settle on a permanent
    /// "couldn't attach" error even though a healthy session arrives seconds
    /// later (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
    /// The string is the terminal error shown only if no live session appears
    /// before the deadline (an exited session's last output line, when any).
    Awaiting(String),
    /// Any other no-session / attach failure. The message is shown as-is and,
    /// for a session that *exited*, already carries its last output line (so a
    /// dead workspace or orchestrator surfaces why it's gone).
    Message(String),
}

/// How [`retry_connect`] retries a session whose socket isn't up yet
/// ([`ConnectFailure::Starting`]) or that is expected back
/// ([`ConnectFailure::Awaiting`]).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total time to keep retrying before giving up and surfacing the error.
    pub deadline: Duration,
    /// Pause between attempts.
    pub backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(15),
            backoff: Duration::from_millis(250),
        }
    }
}

/// The blocking work of connecting to a session, injectable so a client's view
/// state can be tested against a connector that needs no real session.
pub trait Connector: Send + Sync {
    fn connect(&self, project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure>;
}

/// Discover the session `target` names in `project` on disk, open it, and
/// attach with a full replay. Blocking: run it off the UI thread.
///
/// `idle` resolves an idle workspace's placeholder identity (machine, branch)
/// when a declared workspace has no session at all; it runs only on that path.
pub fn connect(
    project: &str,
    target: &SessionRef,
    idle: impl FnOnce(&str, &str) -> IdleInfo,
) -> Result<Connected, ConnectFailure> {
    let root = shelbi_state::sessions_dir().map_err(|e| ConnectFailure::Message(e.to_string()))?;
    let want = discovery_name(project, target);
    let sessions =
        shelbi_client::list(&root).map_err(|e| ConnectFailure::Message(e.to_string()))?;
    // Several sessions can share a name (a relaunch racing a not-yet-reaped
    // predecessor, or a zombie lingering beside its replacement), so choose
    // the *usable* one deliberately — lock held and socket accepting, newest
    // first — rather than take whatever `list` returned first. Taking the
    // first `alive` one could land on the zombie, whose socket refuses the
    // connect (`rt-re-entering-a-review-fails-to-attach`).
    let sess = match shelbi_client::choose_session(&sessions, &want) {
        Some(s) => s,
        None => {
            // No live session. If a *dead* one with this name left a
            // `final.txt`, surface its last output line so the client explains
            // *why* the session isn't there — e.g. an orchestrator that exited
            // at launch with `command not found: claude` — instead of only "no
            // live session" (`rt-login-env-capture-empty-path`).
            let exited = sessions.iter().any(|s| s.meta.name == want && !s.alive);
            let last_line = sessions
                .iter()
                .find(|s| s.meta.name == want && !s.alive)
                .and_then(|s| last_final_line(&s.dir));
            // A declared workspace with no exited session is *idle*, not
            // failed: show its identity + how to start, not a terse error
            // (`rt-tui-idle-workspace-open`). A session that exited (or any
            // non-workspace target) keeps the explanatory message.
            if let (SessionRef::Workspace(name), None) = (target, &last_line) {
                return Err(ConnectFailure::Idle(idle(project, name)));
            }
            let msg = no_live_session_error(&want, last_line.as_deref());
            // The orchestrator is relaunched out of band — a daemon restart
            // SIGTERMs the old session and spawns a fresh one a beat later. If
            // the client connected in that gap it finds only the exited
            // session; rather than surfacing a terminal error the user must
            // quit past, keep retrying until the new session binds
            // (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
            // `orchestrator_declared` is computed lazily, only when it could
            // change the outcome (an exited session already proves one is
            // expected), to avoid a config load on every workspace miss.
            if should_await_session(target, exited, || orchestrator_declared(project)) {
                return Err(ConnectFailure::Awaiting(msg));
            }
            return Err(ConnectFailure::Message(msg));
        }
    };
    let (conn, events) = match Connection::open(&sess.sock, None, capability::ALL) {
        Ok(ce) => ce,
        // The session dir (with `meta.json`) appears before its `sock` is
        // bound, so a connect in that window fails with ENOENT (socket file
        // absent) or ECONNREFUSED (file there, not yet listening). A session
        // whose listener died *after* accepting — the zombie we may still land
        // on when a replacement is coming up — instead tears the connection
        // mid-hello: ENOTCONN, EPIPE, or ECONNRESET. Discovery told us a
        // session with this name is alive, so every one of these is "still
        // starting / racing a relaunch", not a terminal failure — signal a
        // retry so a usable session (or the reaped zombie's replacement) is
        // picked up on the next attempt (`rt-tui-attach-retry-unbound-socket`,
        // `rt-re-entering-a-review-fails-to-attach`).
        Err(ClientError::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            return Err(ConnectFailure::Starting(starting_error(&want)));
        }
        Err(e) => return Err(ConnectFailure::Message(e.to_string())),
    };
    let size = match conn.info() {
        Ok(info) => GridSize::new(info.cols.max(1), info.rows.max(1)),
        Err(_) => GridSize::new(80, 24),
    };
    conn.attach(None)
        .map_err(|e| ConnectFailure::Message(e.to_string()))?;
    Ok(Connected { conn, events, size })
}

/// Connect through `connector`, retrying the transient outcomes
/// ([`ConnectFailure::Starting`] / [`ConnectFailure::Awaiting`]) on `retry`'s
/// cadence until a terminal outcome or the deadline. On give-up the transient
/// signal becomes a [`ConnectFailure::Message`], so the caller only ever sees a
/// live binding, an idle workspace, or a message. Blocking: run it off the UI
/// thread.
pub fn retry_connect(
    connector: &dyn Connector,
    project: &str,
    target: &SessionRef,
    retry: RetryPolicy,
) -> Result<Connected, ConnectFailure> {
    // A just-launched session has its dir (with `meta.json`) on disk a beat
    // before its `sock` is bound, so a connect in that window returns
    // `Starting` — back off and try again rather than surfacing an error, until
    // the socket is up or the deadline passes
    // (`rt-tui-attach-retry-unbound-socket`). Every other outcome (a live
    // attach, an idle workspace, an exited session) is terminal and returned
    // at once.
    let started = Instant::now();
    let mut result = connector.connect(project, target);
    while matches!(
        &result,
        Err(ConnectFailure::Starting(_) | ConnectFailure::Awaiting(_))
    ) {
        if started.elapsed() >= retry.deadline {
            // Give up: turn the transient signal into a terminal error so the
            // user finally sees why the attach never completed.
            result = match result {
                // A socket that never bound — name the give-up explicitly,
                // since the carried message only says "socket not up yet".
                Err(ConnectFailure::Starting(msg)) => Err(ConnectFailure::Message(format!(
                    "{msg} — not reachable after {:?}",
                    retry.deadline
                ))),
                // A session that never (re)appeared — surface its carried
                // message as-is: the exited session's last output line, when
                // any, which already explains why nothing is there.
                Err(ConnectFailure::Awaiting(msg)) => Err(ConnectFailure::Message(msg)),
                other => other,
            };
            break;
        }
        std::thread::sleep(retry.backoff);
        result = connector.connect(project, target);
    }
    result
}

/// Whether a missing live session for `target` should be *awaited* — kept
/// retrying — rather than reported as a terminal failure. True only for the
/// orchestrator, and only when a session is expected: an exited one with its
/// name is already on disk (`exited`), or the project declares an orchestrator
/// (`declared`). A relaunch in that window brings a live session up within the
/// retry deadline, so a client never settles on a stale "couldn't attach"
/// error across an orchestrator relaunch
/// (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
///
/// `declared` is a closure so the (blocking) config load runs only when the
/// cheap `exited` signal didn't already settle it.
pub fn should_await_session(
    target: &SessionRef,
    exited: bool,
    declared: impl FnOnce() -> bool,
) -> bool {
    matches!(target, SessionRef::Orchestrator) && (exited || declared())
}

/// Whether `project` declares an orchestrator — i.e. its config loads. A
/// declared orchestrator is expected to have (or soon have) a live session.
/// Best-effort: a project whose config can't be read reads as "not declared"
/// and the connect falls back to the exited-session signal.
fn orchestrator_declared(project: &str) -> bool {
    shelbi_state::load_project(project).is_ok()
}

/// The error shown when no live session named `want` exists, appending a dead
/// session's last output line when one is available.
pub fn no_live_session_error(want: &str, last_line: Option<&str>) -> String {
    match last_line {
        Some(line) if !line.is_empty() => {
            format!("no live session `{want}` — last output: {line}")
        }
        _ => format!("no live session `{want}`"),
    }
}

/// The error shown when a just-launched session's socket never comes up within
/// the retry deadline.
pub fn starting_error(want: &str) -> String {
    format!("session `{want}` is starting (socket not up yet)")
}

/// The last non-empty line of a dead session's `<dir>/final.txt`, trimmed, or
/// `None` when the file is absent or blank. This is the exited session's final
/// screen line (e.g. `zsh:1: command not found: claude`).
pub fn last_final_line(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("final.txt")).ok()?;
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

/// The main-area notice while a first connect is in flight.
pub fn connecting_notice(target: &SessionRef) -> String {
    format!("Connecting to {}…", target.display())
}

/// The main-area notice for a connect that settled on a failure.
pub fn failed_notice(target: &SessionRef, error: &str) -> String {
    format!("Couldn't attach to {}: {error}", target.display())
}

/// The notice shown while a pane that exited is being brought back:
/// "Session exited — restarting (attempt/max)…". Shared by every client so the
/// wording stays identical.
pub fn restarting_notice(attempt: u32, max: u32) -> String {
    format!("Session exited — restarting ({attempt}/{max})…")
}

/// The notice shown when the restart budget is spent: names the cap and how to
/// bring the pane back ("select it again to relaunch"), and appends the last
/// output line when there is one so a stuck pane shows *why* it stopped. Shared
/// by every main-area / content render site so the wording stays identical.
pub fn gave_up_notice(last_line: &str) -> String {
    let base = format!(
        "Stopped after {} restarts — select it again to relaunch.",
        MAX_RESTARTS_IN_WINDOW
    );
    if last_line.trim().is_empty() {
        base
    } else {
        format!("{base} Last output: {}", last_line.trim())
    }
}

/// A one-line summary of why a live session's child exited, for the restart /
/// give-up notice. Prefers the session's own reason, then the exit status or
/// signal.
pub fn exit_line(exited: Option<&shelbi_proto::Exited>) -> String {
    match exited {
        Some(e) => {
            if let Some(r) = e.reason.as_deref() {
                if !r.trim().is_empty() {
                    return r.trim().to_string();
                }
            }
            if let Some(code) = e.code {
                return format!("exited with status {code}");
            }
            if let Some(sig) = e.signal {
                return format!("killed by signal {sig}");
            }
            "session exited".to_string()
        }
        None => "session exited".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn discovery_names_follow_the_backend_scheme() {
        assert_eq!(discovery_name("shelbi", &SessionRef::Orchestrator), "shelbi/orch");
        assert_eq!(
            discovery_name("shelbi", &SessionRef::Workspace("alpha".into())),
            "shelbi/ws/alpha"
        );
        assert_eq!(
            discovery_name(
                "shelbi",
                &SessionRef::Review {
                    slot: "review".into(),
                    role: "diff".into()
                }
            ),
            "shelbi/review/review/diff"
        );
        assert_eq!(
            discovery_name(
                "shelbi",
                &SessionRef::WorkspaceContent {
                    workspace: "alpha".into(),
                    role: "editor".into()
                }
            ),
            "shelbi/ws/alpha/editor"
        );
    }

    #[test]
    fn a_view_name_maps_to_its_session() {
        assert_eq!(SessionRef::from_view_name("orch"), SessionRef::Orchestrator);
        assert_eq!(
            SessionRef::from_view_name("alpha"),
            SessionRef::Workspace("alpha".into())
        );
    }

    #[test]
    fn no_live_session_error_appends_the_exited_sessions_last_line() {
        assert_eq!(
            no_live_session_error("contextstore/orch", Some("zsh:1: command not found: claude")),
            "no live session `contextstore/orch` — last output: zsh:1: command not found: claude"
        );
        assert_eq!(no_live_session_error("demo/orch", None), "no live session `demo/orch`");
        assert_eq!(
            no_live_session_error("demo/orch", Some("")),
            "no live session `demo/orch`"
        );
    }

    #[test]
    fn last_final_line_reads_the_last_nonblank_line_of_final_txt() {
        let dir = std::env::temp_dir().join(format!("shelbi-app-final-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(last_final_line(&dir), None, "no final.txt");
        std::fs::write(
            dir.join("final.txt"),
            "booting orchestrator\nzsh:1: command not found: claude\n\n   \n",
        )
        .unwrap();
        assert_eq!(
            last_final_line(&dir).as_deref(),
            Some("zsh:1: command not found: claude")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starting_error_reads_as_a_terminal_message() {
        assert_eq!(
            starting_error("demo/orch"),
            "session `demo/orch` is starting (socket not up yet)"
        );
    }

    #[test]
    fn should_await_session_only_awaits_an_expected_orchestrator() {
        // An orchestrator with an exited session on disk, or one the project
        // declares, is awaited (a relaunch is on its way).
        assert!(should_await_session(&SessionRef::Orchestrator, true, || false));
        assert!(should_await_session(&SessionRef::Orchestrator, false, || true));
        // An undeclared orchestrator with nothing on disk is not.
        assert!(!should_await_session(&SessionRef::Orchestrator, false, || false));
        // Never a workspace or content session.
        assert!(!should_await_session(
            &SessionRef::Workspace("w".into()),
            true,
            || true
        ));
        assert!(!should_await_session(
            &SessionRef::Review {
                slot: "r".into(),
                role: "diff".into()
            },
            true,
            || true
        ));
    }

    #[test]
    fn should_await_session_skips_the_declared_check_when_exited_settles_it() {
        let mut asked = false;
        assert!(should_await_session(&SessionRef::Orchestrator, true, || {
            asked = true;
            true
        }));
        assert!(!asked, "the config load is skipped when `exited` settles it");
    }

    #[test]
    fn restart_and_give_up_notices() {
        assert_eq!(
            connecting_notice(&SessionRef::Orchestrator),
            "Connecting to orchestrator…"
        );
        assert_eq!(
            failed_notice(&SessionRef::Orchestrator, "no live session `demo/orch`"),
            "Couldn't attach to orchestrator: no live session `demo/orch`"
        );
        let n = MAX_RESTARTS_IN_WINDOW;
        assert_eq!(
            restarting_notice(2, n as u32),
            format!("Session exited — restarting (2/{n})…")
        );
        assert_eq!(
            gave_up_notice("command not found: vim"),
            format!(
                "Stopped after {n} restarts — select it again to relaunch. \
                 Last output: command not found: vim"
            )
        );
        assert_eq!(
            gave_up_notice("   "),
            format!("Stopped after {n} restarts — select it again to relaunch.")
        );
    }

    #[test]
    fn exit_line_prefers_reason_then_status_then_signal() {
        use shelbi_proto::Exited;
        assert_eq!(
            exit_line(Some(&Exited {
                code: Some(1),
                signal: None,
                reason: Some("usage limit reached".into()),
            })),
            "usage limit reached"
        );
        assert_eq!(
            exit_line(Some(&Exited {
                code: Some(2),
                signal: None,
                reason: None,
            })),
            "exited with status 2"
        );
        assert_eq!(
            exit_line(Some(&Exited {
                code: None,
                signal: Some(9),
                reason: None,
            })),
            "killed by signal 9"
        );
        assert_eq!(exit_line(None), "session exited");
    }

    /// A connector that answers from a script, counting its calls.
    struct Scripted {
        calls: AtomicUsize,
        transient: usize,
        awaiting: bool,
    }

    impl Connector for Scripted {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.transient {
                return Err(if self.awaiting {
                    ConnectFailure::Awaiting("no live session `demo/orch`".into())
                } else {
                    ConnectFailure::Starting("session `demo/orch` is starting".into())
                });
            }
            Err(ConnectFailure::Message("terminal".into()))
        }
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            deadline: Duration::from_secs(5),
            backoff: Duration::from_millis(1),
        }
    }

    #[test]
    fn retry_connect_retries_transient_outcomes_until_a_terminal_one() {
        for awaiting in [false, true] {
            let c = Scripted {
                calls: AtomicUsize::new(0),
                transient: 3,
                awaiting,
            };
            match retry_connect(&c, "demo", &SessionRef::Orchestrator, fast()) {
                Err(ConnectFailure::Message(m)) => assert_eq!(m, "terminal"),
                _ => panic!("the terminal outcome is returned"),
            }
            assert_eq!(c.calls.load(Ordering::SeqCst), 4, "three retries, then the answer");
        }
    }

    #[test]
    fn retry_connect_gives_up_with_a_message_after_the_deadline() {
        let policy = RetryPolicy {
            deadline: Duration::from_millis(20),
            backoff: Duration::from_millis(2),
        };
        let starting = Scripted {
            calls: AtomicUsize::new(0),
            transient: usize::MAX,
            awaiting: false,
        };
        match retry_connect(&starting, "demo", &SessionRef::Orchestrator, policy) {
            Err(ConnectFailure::Message(m)) => {
                assert!(m.starts_with("session `demo/orch` is starting"), "{m}");
                assert!(m.contains("not reachable after"), "{m}");
            }
            _ => panic!("a never-bound socket gives up with a message"),
        }
        let awaiting = Scripted {
            calls: AtomicUsize::new(0),
            transient: usize::MAX,
            awaiting: true,
        };
        match retry_connect(&awaiting, "demo", &SessionRef::Orchestrator, policy) {
            Err(ConnectFailure::Message(m)) => assert_eq!(m, "no live session `demo/orch`"),
            _ => panic!("an orchestrator that never returns surfaces its carried message"),
        }
    }
}
