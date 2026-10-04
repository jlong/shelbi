//! The session seam.
//!
//! [`SessionBackend`] abstracts the *session operations* the orchestrator
//! performs against a worker's terminal — spawn, kill, a three-state liveness
//! probe, text/Enter injection, snapshot/history, title, metadata, enumerate,
//! respawn in place, read a dead session's final screen, resize, and a
//! per-target injection lock. It deliberately covers **session operations
//! only**: layout calls (`split-window`, `swap-pane`, and the rest) are not
//! abstracted here — they are deleted later in the remove-tmux effort, not
//! moved behind the seam.
//!
//! **Addressing is backend-neutral.** Callers name a target with
//! [`SessionTarget`] — a logical session (`<project>/orch`), a slot within one
//! (`<project>/ws/<workspace>`), or a stable pane handle — and never hand the
//! trait a [`TmuxAddr`]. The tmux backend maps a `SessionTarget` to a
//! `TmuxAddr` *internally*; a session-process backend
//! (`rt-backend-sessions`) maps the same target to a session directory keyed by
//! its logical name. Keeping `TmuxAddr` off the trait boundary is what lets a
//! non-tmux backend implement it.
//!
//! Today there is exactly one implementation, [`TmuxBackend`], which reproduces
//! the existing tmux behavior **byte for byte** — every method either delegates
//! to [`shelbi_tmux`] or builds the same `tmux` argv the orchestrator used
//! before this seam existed, routed through [`shelbi_ssh`] so a local tmux
//! server and one reached over SSH behave identically. There is intentionally
//! no behavior change in this phase: the seam exists so a session-process
//! backend can be dropped in later (`rt-backend-sessions`) behind a hidden
//! dev setting.
//!
//! A handful of operations are **genuinely tmux-topology-specific** — reaping a
//! window or a pane by its stable id, listing live pane ids, and the local
//! dispatch-pane spawn with its tmux `-e` env injection. tmux multiplexes all
//! local workspaces as windows in one shared session; the session-process
//! backend has no windows or panes at all, so these have no analogue there.
//! They live as **inherent methods on [`TmuxBackend`]** (not on the trait), and
//! callers reach them through the concretely-typed [`backend()`]. See each
//! method's doc comment.
//!
//! Callers reach the active backend through [`backend()`]. The orchestrator-side
//! call sites (`workspace.rs`, `submit.rs`, `ready.rs`, `handoff.rs`,
//! `load.rs`) talk to the trait rather than to tmux directly.

use std::collections::HashMap;
use std::process::Output;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use shelbi_core::{Error, Host, Result, TmuxAddr};
use shelbi_session::SpawnSpec;

/// A backend-neutral handle to a worker's session, or a sub-target within one.
///
/// Callers address every session operation through this type instead of a
/// [`TmuxAddr`], so a backend that is not tmux can interpret the target in its
/// own terms. The tmux backend maps a `SessionTarget` to a `TmuxAddr`
/// internally (see [`TmuxBackend`]); a session-process backend maps it to a
/// session directory keyed by the logical name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTarget(Inner);

/// How a [`SessionTarget`] is addressed. Private: callers construct via the
/// named constructors and the tmux backend maps via the `pub(crate)`
/// accessors, so no backend-specific shape leaks to the API.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Inner {
    /// A workspace slot: a logical session plus the role within it. The tmux
    /// backend maps the pair to `session:window` (local) or scopes metadata by
    /// `=session` (remote), choosing by [`Host`]; a session-process backend
    /// maps the pair to its own session directory.
    Slot { session: String, window: String },
    /// A whole session addressed by name, with no sub-role (the shared project
    /// session, an orchestrator session). tmux maps this to `=session`.
    Session { session: String },
    /// A stable pane handle that survives being moved between windows (a tmux
    /// pane id `%N`). Addressing a pane directly is a tmux concept; the few
    /// pane-addressed sends (the pinned orchestrator pane) route here.
    Pane { id: String },
}

impl SessionTarget {
    /// A workspace slot: a logical session plus the role (window) within it.
    pub fn slot(session: impl Into<String>, window: impl Into<String>) -> Self {
        SessionTarget(Inner::Slot {
            session: session.into(),
            window: window.into(),
        })
    }

    /// A whole session addressed by name.
    pub fn session(session: impl Into<String>) -> Self {
        SessionTarget(Inner::Session {
            session: session.into(),
        })
    }

    /// A stable pane handle (a tmux pane id `%N` today).
    pub fn pane(id: impl Into<String>) -> Self {
        SessionTarget(Inner::Pane { id: id.into() })
    }

    /// Classify an existing [`TmuxAddr`] into a `SessionTarget`. Transitional:
    /// orchestrator call sites still hold a `TmuxAddr` from workspace state, so
    /// they convert at the backend boundary. An address with an empty session is
    /// a bare pane/target handle (`%N`); otherwise it is a `session:window`
    /// slot — matching [`TmuxAddr::target`]'s own branch, so the round-trip
    /// through [`to_tmux_addr`](Self::to_tmux_addr) is identity.
    pub fn from_tmux_addr(addr: &TmuxAddr) -> Self {
        if addr.session.is_empty() {
            SessionTarget::pane(addr.window.clone())
        } else {
            SessionTarget::slot(addr.session.clone(), addr.window.clone())
        }
    }

    /// Map back to the `TmuxAddr` the tmux backend commands against. Inverse of
    /// [`from_tmux_addr`](Self::from_tmux_addr) for slots and panes. A
    /// session-only target maps to a `session`-with-empty-window addr (session
    /// operations read the name via [`session_name`](Self::session_name), not
    /// this).
    pub(crate) fn to_tmux_addr(&self) -> TmuxAddr {
        match &self.0 {
            Inner::Slot { session, window } => TmuxAddr {
                session: session.clone(),
                window: window.clone(),
            },
            Inner::Session { session } => TmuxAddr {
                session: session.clone(),
                window: String::new(),
            },
            Inner::Pane { id } => TmuxAddr::pane_id(id.clone()),
        }
    }

    /// The logical session name a session-scoped operation targets (probe by
    /// session, kill-session, session env, enumerate). Empty for a bare pane
    /// handle, which carries no session name.
    pub(crate) fn session_name(&self) -> &str {
        match &self.0 {
            Inner::Slot { session, .. } | Inner::Session { session } => session,
            Inner::Pane { .. } => "",
        }
    }

    /// A stable, human-readable key for this target — used for the per-target
    /// injection lock and delivery diagnostics. Equals the pre-seam
    /// [`TmuxAddr::target`] for the slot (`session:window`) and pane (`%N`)
    /// cases, so the injection lock keys identically to before the seam.
    pub fn label(&self) -> String {
        match &self.0 {
            Inner::Slot { session, window } => format!("{session}:{window}"),
            Inner::Session { session } => session.clone(),
            Inner::Pane { id } => id.clone(),
        }
    }

    /// The target's shape, with its parts cloned out — for a non-tmux backend
    /// that derives its own addressing (a session directory name) from the
    /// target. Keeps the private [`Inner`] from leaking while giving the
    /// session-process backend what it needs. See
    /// [`session_process_backend::session_name`](crate::session_process_backend::session_name).
    pub(crate) fn name_parts(&self) -> TargetParts {
        match &self.0 {
            Inner::Slot { session, window } => TargetParts::Slot {
                session: session.clone(),
                window: window.clone(),
            },
            Inner::Session { session } => TargetParts::Session {
                session: session.clone(),
            },
            Inner::Pane { id } => TargetParts::Pane { id: id.clone() },
        }
    }
}

/// The three shapes of a [`SessionTarget`], exposed for a non-tmux backend's
/// name derivation without leaking the private [`Inner`]. Returned by
/// [`SessionTarget::name_parts`].
pub(crate) enum TargetParts {
    Slot { session: String, window: String },
    Session { session: String },
    Pane { id: String },
}

/// Three-state liveness for the session probe.
///
/// The distinction that matters: [`Unreachable`](Liveness::Unreachable) — the
/// machine couldn't be *asked* (an SSH transport failure, a wedged auth
/// handshake, a timed-out probe) — is **never** collapsed into
/// [`Dead`](Liveness::Dead). Treating "couldn't ask" as "not running" is the
/// F6 bug: during a network blip a stale agent session carrying the PREVIOUS
/// task's context looks absent, the kill-to-clear-context invariant is skipped,
/// and the next task's prompt lands in the wrong context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// The session/slot exists and is live.
    Alive,
    /// The session/slot definitively does not exist (tmux answered "no").
    Dead,
    /// The question could not be answered. `reason` is a one-line,
    /// human-readable cause suitable for a status row.
    Unreachable { reason: String },
}

impl Liveness {
    /// Is the session/slot definitively alive?
    pub fn is_alive(&self) -> bool {
        matches!(self, Liveness::Alive)
    }

    /// Collapse to the `Result<bool>` shape the pre-seam `has_session`
    /// callers expect: `Alive → Ok(true)`, `Dead → Ok(false)`, and
    /// `Unreachable → Err`. An unreachable probe is surfaced as an error so a
    /// caller gating a kill-to-clear-context step can't silently skip it on a
    /// blip — exactly as the old `shelbi_tmux::has_session` did.
    pub fn into_exists(self) -> Result<bool> {
        match self {
            Liveness::Alive => Ok(true),
            Liveness::Dead => Ok(false),
            Liveness::Unreachable { reason } => Err(Error::Other(reason)),
        }
    }
}

/// One live slot under a session, as returned by
/// [`SessionBackend::enumerate_slots`]. `id` is a **backend-opaque** stable
/// handle (a tmux window id `@<n>`, never spaced, for the tmux backend) that
/// addresses the slot even when its name has been rewritten mid-session (Claude
/// rewrites its window title); `name` is the slot's role name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotInfo {
    pub id: String,
    pub name: String,
}

/// Outcome of a respawn-in-place. Mirrors the pre-seam `PaneReloadStatus`
/// shape so the reload paths read the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RespawnOutcome {
    Respawned { target: String },
    Failed { target: String, reason: String },
}

/// A held per-target injection lock. Dropping it releases the lock.
///
/// The guard borrows a process-global, per-target mutex (see
/// [`SessionBackend::injection_lock`]). While it is held, no other caller can
/// acquire the lock for the *same* target, so two threads can't interleave a
/// paste into one pane; locks for different targets are independent.
pub struct InjectionGuard {
    _guard: MutexGuard<'static, ()>,
}

/// The session operations the orchestrator performs against a worker terminal.
/// See the module docs for scope. Every method addresses its target with a
/// backend-neutral [`SessionTarget`] (no `TmuxAddr`, no `std::process::Output`,
/// and no tmux argv crosses the boundary), so a backend that is not tmux can
/// implement it.
pub trait SessionBackend {
    // --- spawn -----------------------------------------------------------

    /// Create a detached session running `command` (when `Some`). Used for
    /// remote workspaces, where the session *is* the workspace. The local
    /// dispatch-pane spawn — a tmux window with `-e` env injection — is the
    /// tmux-topology-specific [`TmuxBackend::spawn_local_pane`] instead.
    fn spawn(&self, host: &Host, target: &SessionTarget, command: Option<&str>) -> Result<()>;

    // --- kill (process group) -------------------------------------------

    /// Kill the session the target names (its process group), by name. For a
    /// slot this kills the whole session the slot belongs to — correct where
    /// the slot *is* the session (a remote workspace). Local slot teardown,
    /// which must reap individual windows inside the shared project session,
    /// uses [`TmuxBackend::kill_window`] instead. Best-effort: an already-gone
    /// target is fine, but a transport failure is surfaced so an orphaned
    /// remote agent isn't silently left running.
    fn kill(&self, host: &Host, target: &SessionTarget) -> Result<()>;

    // --- three-state probe with deadline --------------------------------

    /// Does the target have a live allocation? Three-state: see [`Liveness`].
    /// A whole session probes by name; a local slot probes the window inside
    /// its project session; a remote slot probes the standalone session. A
    /// pane handle probes the live pane list. `deadline` bounds the underlying
    /// call (a wedged transport is killed and reported `Unreachable`, never
    /// `Dead`); `None` is unbounded.
    fn probe(&self, host: &Host, target: &SessionTarget, deadline: Option<Duration>) -> Liveness;

    // --- send text / Enter ----------------------------------------------

    /// Send text to the target's input WITHOUT a trailing Enter.
    fn send_text(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()>;

    /// Send a bare Enter keypress (no text) — used to dismiss modal prompts.
    fn send_enter(&self, host: &Host, target: &SessionTarget) -> Result<()>;

    /// Send text followed by Enter.
    fn send_line(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()>;

    // --- snapshot / history / final screen ------------------------------

    /// The target's current visible content as plain text (wrapped lines joined).
    fn snapshot(&self, host: &Host, target: &SessionTarget) -> Result<String>;

    /// Snapshot including `lines` of scrollback before the visible area.
    fn history(&self, host: &Host, target: &SessionTarget, lines: usize) -> Result<String>;

    /// Read a dead session's final screen. For the tmux backend this is the
    /// current capture (a `remain-on-exit` pane still renders its last frame);
    /// a true post-exit snapshot arrives with the session-process backend.
    fn final_screen(&self, host: &Host, target: &SessionTarget) -> Result<String>;

    // --- title ----------------------------------------------------------

    /// The target's title (trailing newline trimmed) — carries the
    /// `shelbi:<state>` marker the worker hooks write.
    fn title(&self, host: &Host, target: &SessionTarget) -> Result<String>;

    // --- metadata -------------------------------------------------------

    /// Read a metadata value off a live target. Window-scoped for local
    /// workspace slots, session-scoped for remote ones. `Ok(None)` when the
    /// value is unset (or the target is gone); `Err` only on a transport
    /// failure. `deadline` bounds the call when `Some`.
    fn get_metadata(
        &self,
        host: &Host,
        target: &SessionTarget,
        key: &str,
        deadline: Option<Duration>,
    ) -> Result<Option<String>>;

    /// Stamp a metadata value onto a live target (same scoping as
    /// [`get_metadata`](Self::get_metadata)). The value dies with the target.
    fn set_metadata(
        &self,
        host: &Host,
        target: &SessionTarget,
        key: &str,
        value: &str,
    ) -> Result<()>;

    /// Read a variable from a session's environment (tmux `show-environment`).
    /// `Ok(None)` when unset/empty or the session is gone.
    fn get_env(&self, host: &Host, target: &SessionTarget, var: &str) -> Result<Option<String>>;

    // --- enumerate ------------------------------------------------------

    /// Enumerate the live slots bound under a session. `Ok(None)` when the
    /// command ran but the session/server is absent; `Err` only on a
    /// transport/spawn failure (so a probe can distinguish "no session" from
    /// "couldn't ask"). `deadline` bounds the call when `Some`. For the tmux
    /// backend a local slot is a window inside the shared project session, so
    /// this lists that session's windows.
    fn enumerate_slots(
        &self,
        host: &Host,
        target: &SessionTarget,
        deadline: Option<Duration>,
    ) -> std::io::Result<Option<Vec<SlotInfo>>>;

    // --- respawn in place / resize --------------------------------------

    /// Kill the process running in `target` and start `cmd` fresh in place,
    /// preserving the target's identity. Local only.
    fn respawn(&self, target: &SessionTarget, cmd: &str) -> RespawnOutcome;

    /// Resize the target to `cols` x `rows`.
    fn resize(&self, host: &Host, target: &SessionTarget, cols: u16, rows: u16) -> Result<()>;

    // --- per-target injection lock --------------------------------------

    /// Acquire the process-global injection lock for `target`, serializing
    /// concurrent pastes into the same target. Blocks until the lock is free;
    /// the returned guard releases it on drop. Locks for different targets are
    /// independent (keyed by [`SessionTarget::label`]).
    fn injection_lock(&self, target: &SessionTarget) -> InjectionGuard;
}

/// The active session backend, chosen by the hidden
/// [`session_backend_enabled`](shelbi_state::session_backend_enabled) dev flag.
///
/// Off (the default) → [`Backend::Tmux`], which delegates to [`TmuxBackend`]
/// byte for byte, so nothing changes. On → [`Backend::Session`], the
/// session-process backend. The flag is dev-only; tmux stays the runtime until
/// cutover. Every orchestrator call site reaches the active backend through
/// this one function, so the switch is a single read.
pub fn backend() -> Backend {
    if shelbi_state::session_backend_enabled() {
        Backend::Session(crate::session_process_backend::SessionProcessBackend)
    } else {
        Backend::Tmux(TmuxBackend)
    }
}

/// The active backend, selected at runtime by [`backend`]. Implements
/// [`SessionBackend`] by delegating to the chosen variant, and carries the few
/// tmux-topology-specific inherent methods (`kill_window`, `kill_pane`,
/// `live_pane_ids`, `spawn_local_pane`) so the handful of call sites that use
/// them compile against one type. On the session variant those map to the
/// session-process equivalent (there are no tmux windows or panes): a window /
/// pane id is a logical session name to kill, the "live pane ids" are the live
/// session names, and the local dispatch spawns a session process.
pub enum Backend {
    Tmux(TmuxBackend),
    Session(crate::session_process_backend::SessionProcessBackend),
}

impl Backend {
    /// tmux-only on the tmux backend; on the session backend, kill the session
    /// whose logical name is `window_id` (what [`SessionBackend::enumerate_slots`]
    /// handed back as a slot `id`).
    pub fn kill_window(&self, host: &Host, window_id: &str) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.kill_window(host, window_id),
            Backend::Session(b) => b.kill_by_name(host, window_id),
        }
    }

    /// tmux-only on the tmux backend; on the session backend, best-effort kill
    /// the session whose logical name is `pane_id`.
    pub fn kill_pane(&self, host: &Host, pane_id: &str) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.kill_pane(host, pane_id),
            Backend::Session(b) => b.kill_by_name(host, pane_id),
        }
    }

    /// tmux-only on the tmux backend; on the session backend, the logical names
    /// of every live session (so a caller confirming a specific handle can
    /// still match).
    pub fn live_pane_ids(&self, host: &Host) -> Result<Vec<String>> {
        match self {
            Backend::Tmux(b) => b.live_pane_ids(host),
            Backend::Session(b) => b.live_session_names(host),
        }
    }

    /// Spawn the orchestrator as a detached session process — the
    /// session-backend branch of [`crate::ensure_dashboard`]
    /// ([`crate::orchestrator_session_spec`] builds the spec). Only the session
    /// backend supports it; the tmux backend brings the orchestrator up as a
    /// dashboard pane (`split-window`) instead and is never called here, so the
    /// `Tmux` variant surfaces an error rather than silently no-op'ing.
    pub fn spawn_orchestrator_session(&self, spec: SpawnSpec) -> Result<()> {
        match self {
            Backend::Session(b) => b.spawn_session(spec).map(|_| ()),
            Backend::Tmux(_) => Err(Error::Other(
                "orchestrator session spawn requires the session backend".into(),
            )),
        }
    }

    /// The local dispatch spawn: a tmux window with `-e` env injection on the
    /// tmux backend, or a detached session process carrying the same per-dispatch
    /// environment on the session backend.
    pub fn spawn_local_pane(
        &self,
        host: &Host,
        args: crate::workspace::LocalPaneTmuxArgs<'_>,
    ) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.spawn_local_pane(host, args),
            Backend::Session(b) => {
                if host.is_ssh() {
                    return Err(Error::Other(
                        "session backend cannot spawn a local pane on a remote host".into(),
                    ));
                }
                b.spawn_session(args.to_session_spawn_spec()).map(|_| ())
            }
        }
    }
}

/// Delegate every trait method to the active variant.
impl SessionBackend for Backend {
    fn spawn(&self, host: &Host, target: &SessionTarget, command: Option<&str>) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.spawn(host, target, command),
            Backend::Session(b) => b.spawn(host, target, command),
        }
    }
    fn kill(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.kill(host, target),
            Backend::Session(b) => b.kill(host, target),
        }
    }
    fn probe(&self, host: &Host, target: &SessionTarget, deadline: Option<Duration>) -> Liveness {
        match self {
            Backend::Tmux(b) => b.probe(host, target, deadline),
            Backend::Session(b) => b.probe(host, target, deadline),
        }
    }
    fn send_text(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.send_text(host, target, text),
            Backend::Session(b) => b.send_text(host, target, text),
        }
    }
    fn send_enter(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.send_enter(host, target),
            Backend::Session(b) => b.send_enter(host, target),
        }
    }
    fn send_line(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.send_line(host, target, text),
            Backend::Session(b) => b.send_line(host, target, text),
        }
    }
    fn snapshot(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        match self {
            Backend::Tmux(b) => b.snapshot(host, target),
            Backend::Session(b) => b.snapshot(host, target),
        }
    }
    fn history(&self, host: &Host, target: &SessionTarget, lines: usize) -> Result<String> {
        match self {
            Backend::Tmux(b) => b.history(host, target, lines),
            Backend::Session(b) => b.history(host, target, lines),
        }
    }
    fn final_screen(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        match self {
            Backend::Tmux(b) => b.final_screen(host, target),
            Backend::Session(b) => b.final_screen(host, target),
        }
    }
    fn title(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        match self {
            Backend::Tmux(b) => b.title(host, target),
            Backend::Session(b) => b.title(host, target),
        }
    }
    fn get_metadata(
        &self,
        host: &Host,
        target: &SessionTarget,
        key: &str,
        deadline: Option<Duration>,
    ) -> Result<Option<String>> {
        match self {
            Backend::Tmux(b) => b.get_metadata(host, target, key, deadline),
            Backend::Session(b) => b.get_metadata(host, target, key, deadline),
        }
    }
    fn set_metadata(
        &self,
        host: &Host,
        target: &SessionTarget,
        key: &str,
        value: &str,
    ) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.set_metadata(host, target, key, value),
            Backend::Session(b) => b.set_metadata(host, target, key, value),
        }
    }
    fn get_env(&self, host: &Host, target: &SessionTarget, var: &str) -> Result<Option<String>> {
        match self {
            Backend::Tmux(b) => b.get_env(host, target, var),
            Backend::Session(b) => b.get_env(host, target, var),
        }
    }
    fn enumerate_slots(
        &self,
        host: &Host,
        target: &SessionTarget,
        deadline: Option<Duration>,
    ) -> std::io::Result<Option<Vec<SlotInfo>>> {
        match self {
            Backend::Tmux(b) => b.enumerate_slots(host, target, deadline),
            Backend::Session(b) => b.enumerate_slots(host, target, deadline),
        }
    }
    fn respawn(&self, target: &SessionTarget, cmd: &str) -> RespawnOutcome {
        match self {
            Backend::Tmux(b) => b.respawn(target, cmd),
            Backend::Session(b) => b.respawn(target, cmd),
        }
    }
    fn resize(&self, host: &Host, target: &SessionTarget, cols: u16, rows: u16) -> Result<()> {
        match self {
            Backend::Tmux(b) => b.resize(host, target, cols, rows),
            Backend::Session(b) => b.resize(host, target, cols, rows),
        }
    }
    fn injection_lock(&self, target: &SessionTarget) -> InjectionGuard {
        // Keyed identically in both variants (both go through `injection_guard`
        // on `label()`), so the lock is stable regardless of the active backend.
        injection_guard(&target.label())
    }
}

/// [`SessionBackend`] over tmux, via [`shelbi_tmux`] and [`shelbi_ssh`]. Unit
/// struct: all state (the injection-lock registry) is process-global.
#[derive(Debug, Clone, Copy, Default)]
pub struct TmuxBackend;

impl TmuxBackend {
    // --- tmux-topology-only operations (deliberately NOT on the trait) ---
    //
    // tmux multiplexes all local workspaces as windows in one shared project
    // session, and a review swap can strand a pane outside its window. The
    // session-process backend has no windows or panes, so these have no
    // analogue there — they stay inherent on `TmuxBackend` and callers reach
    // them through the concretely-typed `backend()`.

    /// tmux-only: kill a window by its stable id (`@N`). Local workspaces are
    /// windows in the shared project session — a tmux topology detail with no
    /// session-process analogue — so this is off the trait. Local slot teardown
    /// reaps every window bound to the slot name (see
    /// [`SessionBackend::enumerate_slots`] + [`slot_ids_named`]).
    pub fn kill_window(&self, host: &Host, window_id: &str) -> Result<()> {
        shelbi_ssh::run(host, ["tmux", "kill-window", "-t", window_id]).map_err(Error::Io)?;
        Ok(())
    }

    /// tmux-only: kill a pane by its stable id (`%N`). Used to reap a review
    /// agent stranded outside its window by a diff/editor pane swap — a tmux
    /// layout artifact the session-process backend can't produce — so this is
    /// off the trait.
    pub fn kill_pane(&self, host: &Host, pane_id: &str) -> Result<()> {
        shelbi_ssh::run(host, ["tmux", "kill-pane", "-t", pane_id]).map_err(Error::Io)?;
        Ok(())
    }

    /// tmux-only: every live pane id on the server (`list-panes -a`). Confirms a
    /// specific pane id — the pinned orchestrator pane — is still alive. Panes
    /// are a tmux concept, so this is off the trait.
    pub fn live_pane_ids(&self, host: &Host) -> Result<Vec<String>> {
        let out = shelbi_ssh::run(host, ["tmux", "list-panes", "-a", "-F", "#{pane_id}"])
            .map_err(Error::Io)?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// tmux-only: create the local dispatch pane — a window in the shared
    /// project session (or a fresh session), injecting the per-dispatch env the
    /// `--as-pane` wrapper inherits via tmux `-e`. The argv is built by the
    /// frozen [`crate::workspace::local_pane_tmux_argv`] constructor; this
    /// executes it. The session-process backend spawns a session process with
    /// an explicit captured environment instead — a different mechanism — so
    /// this is off the trait. The neutral remote spawn is
    /// [`SessionBackend::spawn`].
    pub fn spawn_local_pane(
        &self,
        host: &Host,
        args: crate::workspace::LocalPaneTmuxArgs<'_>,
    ) -> Result<()> {
        let argv = crate::workspace::local_pane_tmux_argv(args);
        shelbi_ssh::run_capture(host, &argv).map(|_| ())
    }

    // --- shared probe internals -----------------------------------------

    /// Does a session with this name exist? Three-state classification of a
    /// `has-session` run. The byte-for-byte pre-seam `has_session` body.
    fn probe_session(&self, host: &Host, session: &str, deadline: Option<Duration>) -> Liveness {
        let argv = vec![
            "tmux".to_string(),
            "has-session".to_string(),
            "-t".to_string(),
            format!("={session}"),
        ];
        liveness_from_run(run_argv(host, &argv, deadline), deadline)
    }

    /// Enumerate a session's windows (the byte-for-byte pre-seam
    /// `session_windows` body). `Ok(None)` when the command ran but the
    /// session/server is absent.
    fn list_session_windows(
        &self,
        host: &Host,
        session: &str,
        deadline: Option<Duration>,
    ) -> std::io::Result<Option<Vec<SlotInfo>>> {
        let argv = list_windows_argv(session);
        let out = run_argv(host, &argv, deadline)?;
        if !out.status.success() {
            // No session / no server → nothing to enumerate.
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(Some(parse_slot_list(&stdout)))
    }
}

impl SessionBackend for TmuxBackend {
    fn spawn(&self, host: &Host, target: &SessionTarget, command: Option<&str>) -> Result<()> {
        let addr = target.to_tmux_addr();
        shelbi_tmux::new_session(host, &addr.session, &addr.window, command)
    }

    fn kill(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        match &target.0 {
            // A bare pane handle maps to the tmux-only pane kill.
            Inner::Pane { id } => self.kill_pane(host, id),
            // A session (or the session a slot belongs to) is killed by name.
            Inner::Slot { session, .. } | Inner::Session { session } => {
                let t = format!("={session}");
                shelbi_ssh::run(host, ["tmux", "kill-session", "-t", &t]).map_err(Error::Io)?;
                Ok(())
            }
        }
    }

    fn probe(&self, host: &Host, target: &SessionTarget, deadline: Option<Duration>) -> Liveness {
        match &target.0 {
            // A slot: local workspaces are windows inside the shared project
            // session (the window is the slot); remote workspaces are standalone
            // sessions (the session is the slot).
            Inner::Slot { session, window } => match host {
                Host::Local => match self.list_session_windows(host, session, deadline) {
                    Ok(Some(windows)) => {
                        if windows.iter().any(|w| w.name == *window) {
                            Liveness::Alive
                        } else {
                            Liveness::Dead
                        }
                    }
                    // Command ran, no session/server → nothing is bound to the slot.
                    Ok(None) => Liveness::Dead,
                    Err(e) => Liveness::Unreachable {
                        reason: probe_error_reason(&e, deadline.unwrap_or(Duration::ZERO)),
                    },
                },
                Host::Ssh { .. } => self.probe_session(host, session, deadline),
            },
            // A whole session: existence by name.
            Inner::Session { session } => self.probe_session(host, session, deadline),
            // A pane handle: present in the live pane list?
            Inner::Pane { id } => match self.live_pane_ids(host) {
                Ok(ids) => {
                    if ids.iter().any(|p| p == id) {
                        Liveness::Alive
                    } else {
                        Liveness::Dead
                    }
                }
                Err(e) => Liveness::Unreachable {
                    reason: format!("probe failed: {e}"),
                },
            },
        }
    }

    fn send_text(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        shelbi_tmux::send_text(host, &target.to_tmux_addr(), text)
    }

    fn send_enter(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        shelbi_tmux::send_enter(host, &target.to_tmux_addr())
    }

    fn send_line(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        shelbi_tmux::send_line(host, &target.to_tmux_addr(), text)
    }

    fn snapshot(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        shelbi_tmux::capture(host, &target.to_tmux_addr())
    }

    fn history(&self, host: &Host, target: &SessionTarget, lines: usize) -> Result<String> {
        shelbi_tmux::capture_history(host, &target.to_tmux_addr(), lines)
    }

    fn final_screen(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        // tmux keeps no post-exit buffer; the current capture is the last
        // rendered frame of a `remain-on-exit` pane. A true dead-session
        // snapshot is a session-process-backend capability.
        shelbi_tmux::capture(host, &target.to_tmux_addr())
    }

    fn title(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        shelbi_tmux::pane_title(host, &target.to_tmux_addr())
    }

    fn get_metadata(
        &self,
        host: &Host,
        target: &SessionTarget,
        key: &str,
        deadline: Option<Duration>,
    ) -> Result<Option<String>> {
        let argv = show_options_argv(host, &target.to_tmux_addr(), key);
        let out = run_argv(host, &argv, deadline).map_err(Error::Io)?;
        Ok(option_value(&out))
    }

    fn set_metadata(
        &self,
        host: &Host,
        target: &SessionTarget,
        key: &str,
        value: &str,
    ) -> Result<()> {
        let argv = set_option_argv(host, &target.to_tmux_addr(), key, value);
        shelbi_ssh::run_capture(host, &argv)?;
        Ok(())
    }

    fn get_env(&self, host: &Host, target: &SessionTarget, var: &str) -> Result<Option<String>> {
        let session = target.session_name();
        let session_target = shelbi_tmux::session_target(session);
        let out = shelbi_ssh::run(host, ["tmux", "show-environment", "-t", &session_target, var])
            .map_err(Error::Io)?;
        Ok(parse_show_environment(&out, var))
    }

    fn enumerate_slots(
        &self,
        host: &Host,
        target: &SessionTarget,
        deadline: Option<Duration>,
    ) -> std::io::Result<Option<Vec<SlotInfo>>> {
        self.list_session_windows(host, target.session_name(), deadline)
    }

    fn respawn(&self, target: &SessionTarget, cmd: &str) -> RespawnOutcome {
        // Local only (matches the pre-seam `respawn_pane`): `-k` kills the
        // running process, the target's identity is preserved so swap-pane refs
        // stay valid.
        let t = target.label();
        let out = std::process::Command::new("tmux")
            .args(["respawn-pane", "-k", "-t", &t, "sh", "-c", cmd])
            .output();
        match out {
            Ok(o) if o.status.success() => RespawnOutcome::Respawned { target: t },
            Ok(o) => RespawnOutcome::Failed {
                target: t,
                reason: String::from_utf8_lossy(&o.stderr).trim().to_string(),
            },
            Err(e) => RespawnOutcome::Failed {
                target: t,
                reason: e.to_string(),
            },
        }
    }

    fn resize(&self, host: &Host, target: &SessionTarget, cols: u16, rows: u16) -> Result<()> {
        let tmux_target = shelbi_tmux::command_target(&target.to_tmux_addr());
        shelbi_ssh::run_capture(
            host,
            [
                "tmux",
                "resize-window",
                "-t",
                &tmux_target,
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ],
        )?;
        Ok(())
    }

    fn injection_lock(&self, target: &SessionTarget) -> InjectionGuard {
        injection_guard(&target.label())
    }
}

/// Acquire the process-global injection lock keyed on `label`, blocking until
/// it is free and returning a guard that releases it on drop. Shared by every
/// backend ([`TmuxBackend`] and the session-process backend both key on
/// [`SessionTarget::label`]), so a paste into one target is serialized no matter
/// which backend is active or how many times [`backend`] was called.
pub(crate) fn injection_guard(label: &str) -> InjectionGuard {
    let mutex = target_injection_mutex(label);
    InjectionGuard {
        _guard: mutex.lock().unwrap_or_else(|p| p.into_inner()),
    }
}

// ---------------------------------------------------------------------------
// tmux argv builders + output parsers (TmuxBackend internals)
// ---------------------------------------------------------------------------

/// Run a tmux argv, optionally bounded by a wall-clock `deadline`. `None` runs
/// unbounded via [`shelbi_ssh::run`]; `Some` routes through
/// [`shelbi_ssh::run_with_deadline`] so a wedged transport is killed and
/// reported as `ErrorKind::TimedOut`.
fn run_argv(host: &Host, argv: &[String], deadline: Option<Duration>) -> std::io::Result<Output> {
    match deadline {
        Some(d) => shelbi_ssh::run_with_deadline(host, argv, d),
        None => shelbi_ssh::run(host, argv),
    }
}

/// `tmux list-windows -t =<session> -F '#{window_id} #{window_name}'`.
fn list_windows_argv(session: &str) -> Vec<String> {
    vec![
        "tmux".into(),
        "list-windows".into(),
        "-t".into(),
        format!("={session}"),
        "-F".into(),
        "#{window_id} #{window_name}".into(),
    ]
}

/// `tmux show-options [-w] -v -t <target> <key>` — window-scoped for local
/// workspaces (the window is the slot), session-scoped for remote ones.
fn show_options_argv(host: &Host, addr: &TmuxAddr, key: &str) -> Vec<String> {
    match host {
        Host::Local => vec![
            "tmux".into(),
            "show-options".into(),
            "-w".into(),
            "-v".into(),
            "-t".into(),
            shelbi_tmux::command_target(addr),
            key.into(),
        ],
        Host::Ssh { .. } => vec![
            "tmux".into(),
            "show-options".into(),
            "-v".into(),
            "-t".into(),
            format!("={}", addr.session),
            key.into(),
        ],
    }
}

/// `tmux set-option [-w] -t <target> <key> <value>` — same scoping as
/// [`show_options_argv`].
fn set_option_argv(host: &Host, addr: &TmuxAddr, key: &str, value: &str) -> Vec<String> {
    match host {
        Host::Local => vec![
            "tmux".into(),
            "set-option".into(),
            "-w".into(),
            "-t".into(),
            shelbi_tmux::command_target(addr),
            key.into(),
            value.into(),
        ],
        Host::Ssh { .. } => vec![
            "tmux".into(),
            "set-option".into(),
            "-t".into(),
            format!("={}", addr.session),
            key.into(),
            value.into(),
        ],
    }
}

/// Interpret a `show-options -v` result: the trimmed value when the command
/// succeeded, `None` otherwise. Older tmux exits non-zero for an unset user
/// option — a plain "not set", not an error worth surfacing.
fn option_value(out: &Output) -> Option<String> {
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Parse `tmux show-environment -t <session> <var>` output into the variable's
/// value. tmux prints `VAR=value`, or a leading-`-` line (`-VAR`) when the
/// variable is explicitly unset. An empty value reads as `None`.
fn parse_show_environment(out: &Output, _var: &str) -> Option<String> {
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    if line.starts_with('-') {
        return None;
    }
    let (_, value) = line.split_once('=')?;
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Parse `tmux list-windows -F '#{window_id} #{window_name}'` output into
/// [`SlotInfo`]s. A slot name can carry spaces (Claude rewrites its window
/// title mid-session), so split only on the FIRST space: the id (`@<n>`, never
/// spaced) is the head and the untouched tail is the name.
fn parse_slot_list(stdout: &str) -> Vec<SlotInfo> {
    stdout
        .lines()
        .filter_map(|line| {
            let (id, name) = line.trim_end().split_once(' ')?;
            Some(SlotInfo {
                id: id.to_string(),
                name: name.to_string(),
            })
        })
        .collect()
}

/// The ids of every slot in `slots` whose name exactly equals `name`. Local
/// workspaces are windows inside the shared project session, so a slot can
/// (under a raced relaunch) accrete more than one window sharing its name;
/// teardown reaps all of them.
pub(crate) fn slot_ids_named(slots: &[SlotInfo], name: &str) -> Vec<String> {
    slots
        .iter()
        .filter(|w| w.name == name)
        .map(|w| w.id.clone())
        .collect()
}

/// Classify the result of a `has-session` run into [`Liveness`]. tmux exits 0
/// (exists) or 1 (absent, including "no server running"); any other exit is the
/// transport failing, not tmux answering, so it reads `Unreachable`. A spawn /
/// timeout error is likewise `Unreachable`, never `Dead`.
fn liveness_from_run(result: std::io::Result<Output>, deadline: Option<Duration>) -> Liveness {
    match result {
        Ok(out) => match out.status.code() {
            Some(0) => Liveness::Alive,
            Some(1) => Liveness::Dead,
            _ => Liveness::Unreachable {
                reason: transport_failure_reason(&out),
            },
        },
        Err(e) => Liveness::Unreachable {
            reason: probe_error_reason(&e, deadline.unwrap_or(Duration::ZERO)),
        },
    }
}

/// One-line reason for a probe that never produced an exit status. The timeout
/// case is worded for its dominant cause — an SSH session parked on an
/// interactive auth step that BatchMode can't suppress (Tailscale SSH's
/// web-auth flow runs outside the openssh client).
fn probe_error_reason(e: &std::io::Error, deadline: Duration) -> String {
    if e.kind() == std::io::ErrorKind::TimedOut {
        format!(
            "ssh probe timed out after {}s (interactive auth pending?)",
            deadline.as_secs()
        )
    } else {
        format!("probe failed: {e}")
    }
}

/// One-line reason for a probe whose transport answered with a non-tmux exit
/// (e.g. ssh's 255): prefer ssh's own first diagnostic line.
fn transport_failure_reason(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    match stderr.lines().find(|l| !l.trim().is_empty()) {
        Some(line) => line.trim().to_string(),
        None => format!("ssh exited {}", out.status),
    }
}

// ---------------------------------------------------------------------------
// Per-target injection lock registry
// ---------------------------------------------------------------------------

/// Return the process-global mutex for `target`, minting (and leaking) a fresh
/// one the first time a target is seen. The set of targets is bounded (one per
/// workspace/pane over the process's life), so leaking a tiny mutex per distinct
/// target is acceptable and buys a `'static` lifetime without reference counting.
fn target_injection_mutex(target: &str) -> &'static Mutex<()> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, &'static Mutex<()>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(existing) = map.get(target) {
        return existing;
    }
    let leaked: &'static Mutex<()> = Box::leak(Box::new(Mutex::new(())));
    map.insert(target.to_string(), leaked);
    leaked
}

/// Try to acquire the injection lock for `target` without blocking. `None` when
/// another holder has it. Primarily a test seam for the contention behavior.
#[cfg(test)]
fn try_injection_lock(target: &str) -> Option<InjectionGuard> {
    let mutex = target_injection_mutex(target);
    match mutex.try_lock() {
        Ok(guard) => Some(InjectionGuard { _guard: guard }),
        Err(std::sync::TryLockError::WouldBlock) => None,
        Err(std::sync::TryLockError::Poisoned(p)) => Some(InjectionGuard {
            _guard: p.into_inner(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a real `Output` with the given exit code — `ExitStatus` has no
    /// public constructor, so we harvest one from a `sh -c "exit N"`.
    fn fake_output(code: i32, stdout: &str, stderr: &str) -> Output {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exit {code}"))
            .status()
            .expect("sh must run");
        Output {
            status,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    // --- backend-neutral target round-trips -----------------------------

    #[test]
    fn target_round_trips_through_tmux_addr_as_identity() {
        // A slot and a pane classified from a TmuxAddr map back byte-identically,
        // so routing a pre-seam `TmuxAddr` through `SessionTarget` and back to the
        // tmux backend changes no command.
        let slot = TmuxAddr {
            session: "shelbi-proj".into(),
            window: "alice".into(),
        };
        let back = SessionTarget::from_tmux_addr(&slot).to_tmux_addr();
        assert_eq!((back.session.as_str(), back.window.as_str()), ("shelbi-proj", "alice"));

        let pane = TmuxAddr::pane_id("%7");
        let back = SessionTarget::from_tmux_addr(&pane).to_tmux_addr();
        assert!(back.session.is_empty());
        assert_eq!(back.window, "%7");
    }

    #[test]
    fn label_matches_the_pre_seam_target_string() {
        // The injection lock keys on `label()`, which must equal the pre-seam
        // `TmuxAddr::target()` for both address kinds so the lock keys identically.
        let slot = TmuxAddr {
            session: "shelbi-proj".into(),
            window: "alice".into(),
        };
        assert_eq!(
            SessionTarget::from_tmux_addr(&slot).label(),
            slot.target()
        );
        let pane = TmuxAddr::pane_id("%7");
        assert_eq!(SessionTarget::from_tmux_addr(&pane).label(), pane.target());
    }

    // --- three-state probe ----------------------------------------------

    #[test]
    fn has_session_classification_is_three_state() {
        // tmux answers 0 (exists) / 1 (absent). Anything else is the transport
        // failing — Unreachable, never Dead — so a network blip can't make a
        // stale session masquerade as "absent".
        assert_eq!(
            liveness_from_run(Ok(fake_output(0, "", "")), None),
            Liveness::Alive
        );
        assert_eq!(
            liveness_from_run(Ok(fake_output(1, "", "")), None),
            Liveness::Dead
        );
        assert!(matches!(
            liveness_from_run(Ok(fake_output(255, "", "boom")), None),
            Liveness::Unreachable { .. }
        ));
        let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline");
        assert!(matches!(
            liveness_from_run(Err(timed_out), Some(Duration::from_secs(5))),
            Liveness::Unreachable { .. }
        ));
    }

    #[test]
    fn unreachable_is_never_folded_into_dead() {
        // The F6 invariant: an unreachable probe collapses to an *error*, not a
        // false "does not exist".
        assert!(Liveness::Alive.into_exists().unwrap());
        assert!(!Liveness::Dead.into_exists().unwrap());
        assert!(Liveness::Unreachable {
            reason: "ssh blip".into()
        }
        .into_exists()
        .is_err());
    }

    #[test]
    fn probe_error_reason_words_the_timeout_for_the_auth_wedge() {
        let timeout = std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline");
        let reason = probe_error_reason(&timeout, Duration::from_secs(5));
        assert_eq!(
            reason,
            "ssh probe timed out after 5s (interactive auth pending?)"
        );

        // A non-timeout spawn failure keeps its own diagnostic.
        let other = std::io::Error::new(std::io::ErrorKind::NotFound, "no such binary");
        let reason = probe_error_reason(&other, Duration::from_secs(5));
        assert!(reason.contains("no such binary"), "reason: {reason}");
        assert!(!reason.contains("timed out"), "reason: {reason}");
    }

    #[test]
    fn transport_failure_reason_prefers_ssh_stderr_over_exit_status() {
        // ssh's own diagnostic (first non-blank line) is the best reason.
        let out = fake_output(255, "", "\nssh: connect to host devbox port 22: refused\n");
        assert_eq!(
            transport_failure_reason(&out),
            "ssh: connect to host devbox port 22: refused"
        );

        // No stderr at all → fall back to the exit status.
        let out = fake_output(255, "", "");
        assert!(
            transport_failure_reason(&out).contains("255"),
            "reason: {}",
            transport_failure_reason(&out)
        );
    }

    // --- metadata / enumerate parsers (relocated from workspace.rs, with
    //     equivalent assertions) ------------------------------------------

    #[test]
    fn option_value_requires_success_and_reports_the_trimmed_value() {
        // Replaces `user_shell_mark_set`: the pre-seam helper collapsed
        // "success && value == 1" into a bool; the clean backend returns the
        // value and leaves the "== 1" check to the caller. Same facts.
        assert_eq!(option_value(&fake_output(0, "1\n", "")).as_deref(), Some("1"));
        assert_eq!(option_value(&fake_output(0, "0\n", "")).as_deref(), Some("0"));
        assert_eq!(option_value(&fake_output(0, "", "")).as_deref(), Some(""));
        // Older tmux exits non-zero for an unset user option — plain "not set".
        assert_eq!(option_value(&fake_output(1, "1\n", "")), None);
    }

    #[test]
    fn slot_ids_matches_every_slot_with_the_slot_name() {
        // Two windows share the slot name `alice` (a raced relaunch left a
        // duplicate). Teardown must reap BOTH, so both ids come back.
        let listing = "@3 alice\n@7 orch\n@9 alice\n";
        assert_eq!(
            slot_ids_named(&parse_slot_list(listing), "alice"),
            vec!["@3".to_string(), "@9".to_string()],
        );
    }

    #[test]
    fn slot_ids_splits_on_first_space_so_spaced_names_still_match() {
        // Claude rewrites its window title, which can contain spaces. The id
        // (`@<n>`) never does, so splitting on the FIRST space keeps a spaced
        // name intact for the exact comparison.
        let listing = "@1 shelbi working\n@2 alice\n";
        assert_eq!(
            slot_ids_named(&parse_slot_list(listing), "shelbi working"),
            vec!["@1".to_string()],
        );
        assert!(slot_ids_named(&parse_slot_list(listing), "shelbi").is_empty());
    }

    #[test]
    fn slot_ids_empty_when_no_slot_carries_the_name() {
        let listing = "@4 orch\n@5 bob\n";
        assert!(slot_ids_named(&parse_slot_list(listing), "alice").is_empty());
        assert!(slot_ids_named(&parse_slot_list(""), "alice").is_empty());
    }

    #[test]
    fn show_environment_parses_value_and_unset_marker() {
        assert_eq!(
            parse_show_environment(&fake_output(0, "SHELBI_PANE_orch=%7\n", ""), "SHELBI_PANE_orch")
                .as_deref(),
            Some("%7")
        );
        // An explicitly-unset variable prints `-VAR`.
        assert_eq!(
            parse_show_environment(&fake_output(0, "-SHELBI_PANE_orch\n", ""), "SHELBI_PANE_orch"),
            None
        );
        // An empty value and a non-zero exit both read as absent.
        assert_eq!(
            parse_show_environment(&fake_output(0, "SHELBI_PANE_orch=\n", ""), "SHELBI_PANE_orch"),
            None
        );
        assert_eq!(
            parse_show_environment(&fake_output(1, "", ""), "SHELBI_PANE_orch"),
            None
        );
    }

    // --- per-target injection lock --------------------------------------

    #[test]
    fn injection_lock_serializes_the_same_target_and_frees_others() {
        // A slot target keys its lock on `label()` == `session:window`.
        let held = TmuxBackend.injection_lock(&SessionTarget::slot("shelbi-w-alice", "agent"));
        // The same target is contended while the guard is held.
        assert!(
            try_injection_lock("shelbi-w-alice:agent").is_none(),
            "a second lock on the same target must not be grantable while held"
        );
        // A different target is independent.
        assert!(
            try_injection_lock("shelbi-w-bob:agent").is_some(),
            "a different target's lock must be independent"
        );
        drop(held);
        // Once released, the target is grantable again.
        assert!(
            try_injection_lock("shelbi-w-alice:agent").is_some(),
            "the lock must be grantable again after the guard drops"
        );
    }

    #[test]
    fn injection_lock_returns_the_same_mutex_for_a_target() {
        let a = target_injection_mutex("shelbi-w-carol:agent") as *const _;
        let b = target_injection_mutex("shelbi-w-carol:agent") as *const _;
        let c = target_injection_mutex("shelbi-w-dave:agent") as *const _;
        assert!(std::ptr::eq(a, b), "same target must map to the same mutex");
        assert!(!std::ptr::eq(a, c), "distinct targets must map to distinct mutexes");
    }
}
