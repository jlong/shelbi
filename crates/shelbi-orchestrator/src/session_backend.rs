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
//! (`<project>/ws/<workspace>`), or a stable pane handle. The session-process
//! backend maps the target to a session directory keyed by its logical name.
//!
//! The runtime is a session process per worker terminal
//! ([`crate::session_process_backend::SessionProcessBackend`]), reached through
//! the [`Backend`] newtype. A few inherent methods on [`Backend`] carry names
//! (`kill_window`, `kill_pane`, `live_pane_ids`, `spawn_local_pane`) retained
//! for their call sites; they map to killing / listing / spawning sessions.
//!
//! Callers reach the backend through [`backend()`]. The orchestrator-side call
//! sites (`workspace.rs`, `submit.rs`, `ready.rs`, `handoff.rs`, `load.rs`)
//! talk to the trait rather than to the backend's internals.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use shelbi_core::{Error, Host, Result};
use shelbi_session::SpawnSpec;

/// A backend-neutral handle to a worker's session, or a sub-target within one.
///
/// Callers address every session operation through this type. The
/// session-process backend maps it to a session directory keyed by the logical
/// name.
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

    /// The logical session name a session-scoped operation targets (probe by
    /// session, kill-session, session env, enumerate). Empty for a bare pane
    /// handle, which carries no session name.
    pub(crate) fn session_name(&self) -> &str {
        match &self.0 {
            Inner::Slot { session, .. } | Inner::Session { session } => session,
            Inner::Pane { .. } => "",
        }
    }

    /// The slot's role (the workspace name) for a slot target; `None` for a
    /// whole-session or pane target. Used where a caller enumerates the slots
    /// under a session and matches a specific role by name.
    pub(crate) fn slot_role(&self) -> Option<&str> {
        match &self.0 {
            Inner::Slot { window, .. } => Some(window),
            _ => None,
        }
    }

    /// A stable, human-readable key for this target — used for the per-target
    /// injection lock and delivery diagnostics: `session:window` for a slot, the
    /// bare name for a session, the pane id for a pane.
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

    /// Collapse to a `Result<bool>`: `Alive → Ok(true)`, `Dead → Ok(false)`,
    /// and `Unreachable → Err`. An unreachable probe is surfaced as an error so
    /// a caller gating a kill-to-clear-context step can't silently skip it on a
    /// blip.
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
    /// dispatch spawn, carrying per-dispatch env, is [`Backend::spawn_local_pane`]
    /// instead.
    fn spawn(&self, host: &Host, target: &SessionTarget, command: Option<&str>) -> Result<()>;

    // --- kill (process group) -------------------------------------------

    /// Kill the session the target names (its process group), by name. For a
    /// slot this kills the whole session the slot belongs to. Best-effort: an
    /// already-gone target is fine, but a transport failure is surfaced so an
    /// orphaned remote agent isn't silently left running.
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

/// The runtime session backend: a session process per worker terminal. Every
/// orchestrator call site reaches it through this one function.
pub fn backend() -> Backend {
    Backend(crate::session_process_backend::SessionProcessBackend)
}

/// The runtime backend: a session process per worker terminal. A thin newtype
/// over [`SessionProcessBackend`](crate::session_process_backend::SessionProcessBackend)
/// that implements [`SessionBackend`] by delegation and carries a few inherent
/// methods (`kill_window`, `kill_pane`, `live_pane_ids`, `spawn_local_pane`)
/// whose names are retained for their call sites: a window / pane id is a
/// logical session name to kill, the "live pane ids" are the live session
/// names, and the local dispatch spawns a session process.
pub struct Backend(crate::session_process_backend::SessionProcessBackend);

impl Backend {
    /// Kill the session whose logical name is `window_id` (what
    /// [`SessionBackend::enumerate_slots`] handed back as a slot `id`).
    pub fn kill_window(&self, host: &Host, window_id: &str) -> Result<()> {
        self.0.kill_by_name(host, window_id)
    }

    /// Best-effort kill the session whose logical name is `pane_id`.
    pub fn kill_pane(&self, host: &Host, pane_id: &str) -> Result<()> {
        self.0.kill_by_name(host, pane_id)
    }

    /// The logical names of every live session (so a caller confirming a
    /// specific handle can still match).
    pub fn live_pane_ids(&self, host: &Host) -> Result<Vec<String>> {
        self.0.live_session_names(host)
    }

    /// Reap "alive but not listening" zombie sessions sharing `target`'s logical
    /// name when a newer live sibling (a replacement) also exists — terminating
    /// each zombie's process and removing its directory, so duplicates don't
    /// accumulate. Local-only and best-effort; returns each reaped short id paired
    /// with the supervision action to log for it (`"reap-wedged"` for a session
    /// that accepts but never answers, `"reap-zombie"` for a refusing one). See
    /// [`SessionProcessBackend::reap_zombies`](crate::session_process_backend::SessionProcessBackend::reap_zombies).
    pub fn reap_zombie_duplicates(
        &self,
        host: &Host,
        target: &SessionTarget,
    ) -> Vec<(String, &'static str)> {
        if host.is_ssh() {
            return Vec::new();
        }
        self.0
            .reap_zombies(&crate::session_process_backend::session_name(target))
    }

    /// Spawn the orchestrator as a detached session process — the
    /// session-backend branch of [`crate::ensure_dashboard`]
    /// ([`crate::orchestrator_session_spec`] builds the spec).
    pub fn spawn_orchestrator_session(&self, spec: SpawnSpec) -> Result<()> {
        self.0.spawn_session(spec).map(|_| ())
    }

    /// Spawn an arbitrary detached session process from a prepared spec — used
    /// for a user shell opened on an idle workspace.
    pub fn spawn_detached(&self, spec: SpawnSpec) -> Result<()> {
        self.0.spawn_session(spec).map(|_| ())
    }

    /// The local dispatch spawn: a detached session process carrying the
    /// per-dispatch environment. Remote hosts spawn via [`SessionBackend::spawn`].
    pub fn spawn_local_pane(
        &self,
        host: &Host,
        args: crate::workspace::LocalDispatchArgs<'_>,
    ) -> Result<()> {
        if host.is_ssh() {
            return Err(Error::Other(
                "session backend cannot spawn a local pane on a remote host".into(),
            ));
        }
        self.0.spawn_session(args.to_session_spawn_spec()).map(|_| ())
    }
}

/// Delegate every trait method to the inner session-process backend.
impl SessionBackend for Backend {
    fn spawn(&self, host: &Host, target: &SessionTarget, command: Option<&str>) -> Result<()> {
        self.0.spawn(host, target, command)
    }
    fn kill(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        self.0.kill(host, target)
    }
    fn probe(&self, host: &Host, target: &SessionTarget, deadline: Option<Duration>) -> Liveness {
        self.0.probe(host, target, deadline)
    }
    fn send_text(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        self.0.send_text(host, target, text)
    }
    fn send_enter(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        self.0.send_enter(host, target)
    }
    fn send_line(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        self.0.send_line(host, target, text)
    }
    fn snapshot(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        self.0.snapshot(host, target)
    }
    fn history(&self, host: &Host, target: &SessionTarget, lines: usize) -> Result<String> {
        self.0.history(host, target, lines)
    }
    fn final_screen(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        self.0.final_screen(host, target)
    }
    fn title(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        self.0.title(host, target)
    }
    fn get_metadata(
        &self,
        host: &Host,
        target: &SessionTarget,
        key: &str,
        deadline: Option<Duration>,
    ) -> Result<Option<String>> {
        self.0.get_metadata(host, target, key, deadline)
    }
    fn set_metadata(
        &self,
        host: &Host,
        target: &SessionTarget,
        key: &str,
        value: &str,
    ) -> Result<()> {
        self.0.set_metadata(host, target, key, value)
    }
    fn get_env(&self, host: &Host, target: &SessionTarget, var: &str) -> Result<Option<String>> {
        self.0.get_env(host, target, var)
    }
    fn enumerate_slots(
        &self,
        host: &Host,
        target: &SessionTarget,
        deadline: Option<Duration>,
    ) -> std::io::Result<Option<Vec<SlotInfo>>> {
        self.0.enumerate_slots(host, target, deadline)
    }
    fn respawn(&self, target: &SessionTarget, cmd: &str) -> RespawnOutcome {
        self.0.respawn(target, cmd)
    }
    fn resize(&self, host: &Host, target: &SessionTarget, cols: u16, rows: u16) -> Result<()> {
        self.0.resize(host, target, cols, rows)
    }
    fn injection_lock(&self, target: &SessionTarget) -> InjectionGuard {
        injection_guard(&target.label())
    }
}

/// Acquire the process-global injection lock keyed on `label`, blocking until
/// it is free and returning a guard that releases it on drop. Shared by every
/// caller (keyed on [`SessionTarget::label`]), so a paste into one target is
/// serialized no matter how many times [`backend`] was called.
pub(crate) fn injection_guard(label: &str) -> InjectionGuard {
    let mutex = target_injection_mutex(label);
    InjectionGuard {
        _guard: mutex.lock().unwrap_or_else(|p| p.into_inner()),
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

    // --- backend-neutral target labels ----------------------------------

    #[test]
    fn label_keys_slots_sessions_and_panes() {
        // The injection lock keys on `label()`: `session:window` for a slot, the
        // bare name for a session, the pane id for a pane.
        assert_eq!(
            SessionTarget::slot("shelbi-proj", "alice").label(),
            "shelbi-proj:alice"
        );
        assert_eq!(
            SessionTarget::session("shelbi-proj").label(),
            "shelbi-proj"
        );
        assert_eq!(SessionTarget::pane("%7").label(), "%7");
    }

    // --- three-state probe ----------------------------------------------

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

    // --- per-target injection lock --------------------------------------

    #[test]
    fn injection_lock_serializes_the_same_target_and_frees_others() {
        // A slot target keys its lock on `label()` == `session:window`.
        let held = backend().injection_lock(&SessionTarget::slot("shelbi-w-alice", "agent"));
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
