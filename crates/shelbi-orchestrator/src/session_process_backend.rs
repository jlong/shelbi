//! [`SessionBackend`] over detached `shelbi __session` processes.
//!
//! This is the second implementation of the Phase 2 seam (the first,
//! [`TmuxBackend`](crate::session_backend::TmuxBackend), reproduces tmux byte
//! for byte). It drives the shared [`shelbi_client`] / [`shelbi_session`] stack:
//! every workspace is its own small session process owning one PTY and one
//! headless emulator, discovered by scanning `~/.shelbi/sessions/` rather than
//! through any central registry. It is selected only when the hidden dev flag
//! is on (see [`crate::session_backend::backend`]); tmux stays the default.
//!
//! **Addressing.** A caller names a target with a backend-neutral
//! [`SessionTarget`]; this backend maps it to a session *directory keyed by its
//! logical name* ([`Meta::name`](shelbi_session::Meta)). Because the targets
//! orchestrator call sites build still carry tmux-shaped names (a local project
//! session is `shelbi-<project>`, a workspace is a window named `<workspace>`),
//! [`session_name`] derives the plan's readable name from them deterministically
//! — a local workspace slot becomes `<project>/ws/<workspace>`. The derivation
//! is a pure function of the target, so spawn and every later lookup agree on
//! the name without a side channel. Native plan-shaped targets land when the
//! callers are rewritten at cutover.
//!
//! **Scope (Phase 2, behind the dev flag).**
//!
//! - **Local only.** Remote (`Host::Ssh`) session spawn is Phase 5
//!   (`rt-remote-spawn`): remote operations here report
//!   [`Liveness::Unreachable`] / an error rather than silently succeeding, so a
//!   `Host::Ssh` workspace is never mistaken for dead. A developer exercising
//!   the flag uses a local project.
//! - **Metadata / session env are not persisted.** tmux user options
//!   (`@shelbi-user-shell`) and the session environment (`SHELBI_PANE_orch`,
//!   the review `SHELBI_REVIEW_*` keys) have no session-process analogue yet;
//!   `get_metadata` / `get_env` return `Ok(None)` and `set_metadata` is a no-op.
//!   Every caller already treats the absent value as a safe default (no pinned
//!   orchestrator pane, no user-shell mark, no parked review interface), so this
//!   degrades to "rebuild from scratch" rather than misbehaving. Persisting
//!   these moves with the daemon/TUI in Phases 3–4.

use std::time::Duration;

use shelbi_client::{DiscoveredSession, SnapshotSource};
use shelbi_core::{Error, Host, Result};
use shelbi_proto::capability;
use shelbi_session::SpawnSpec;

use crate::session_backend::{
    injection_guard, InjectionGuard, Liveness, RespawnOutcome, SessionBackend, SessionTarget,
    SlotInfo, TargetParts,
};

/// Default PTY size for a freshly spawned session. A client reports its real
/// viewport on attach and the PTY reflows; until then the agent renders against
/// this, which is a comfortable default for the detectors.
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// [`SessionBackend`] backed by `shelbi __session` processes. Unit struct: all
/// state (the sessions directory, the injection-lock registry) is process-global.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionProcessBackend;

/// Derive the logical session name (the `meta.json` name a session is keyed by)
/// from a backend-neutral target. Pure, so spawn and every later lookup agree.
///
/// - A **slot** (`session`=`shelbi-<project>`, `window`=`<workspace>`) →
///   `<project>/ws/<workspace>`.
/// - A **session** (the project session `shelbi-<project>`) → `<project>/orch`,
///   the orchestrator session — the stand-in for "does the project session
///   exist?" that `local_session_exists` and the create-new-session probe ask.
/// - A **pane** handle → `pane/<id>` (panes have no session-process analogue;
///   present only so the derivation is total).
pub(crate) fn session_name(target: &SessionTarget) -> String {
    match target.name_parts() {
        TargetParts::Slot { session, window } => {
            format!("{}/ws/{}", project_of(&session), window)
        }
        TargetParts::Session { session } => format!("{}/orch", project_of(&session)),
        TargetParts::Pane { id } => format!("pane/{id}"),
    }
}

/// The project slug a `shelbi-<project>` session name carries. Local sessions
/// are `shelbi-<project>`; strip the prefix, leaving anything else untouched.
fn project_of(session: &str) -> &str {
    session.strip_prefix("shelbi-").unwrap_or(session)
}

impl SessionProcessBackend {
    /// Find the live session with the given logical name, if any. A dead
    /// session directory (lock not held) is skipped here — callers that want a
    /// dead session's final screen reach for it explicitly via the directory.
    fn find_live(&self, name: &str) -> Option<DiscoveredSession> {
        self.discover()
            .into_iter()
            .find(|s| s.alive && s.meta.name == name)
    }

    /// Find any session directory (alive or dead) with the given logical name.
    /// Prefers a live one when both exist (a stale directory not yet reaped
    /// alongside a fresh respawn).
    fn find_any(&self, name: &str) -> Option<DiscoveredSession> {
        let mut found: Option<DiscoveredSession> = None;
        for s in self.discover() {
            let matches = s.meta.name == name;
            if matches && (s.alive || found.is_none()) {
                let alive = s.alive;
                found = Some(s);
                if alive {
                    break;
                }
            }
        }
        found
    }

    /// Every session directory under the sessions root. A missing root (no
    /// session ever started) or a scan error yields an empty list — callers
    /// read that as "nothing is bound", matching the tmux backend's
    /// no-server branch.
    fn discover(&self) -> Vec<DiscoveredSession> {
        match shelbi_state::sessions_dir() {
            Ok(root) => shelbi_client::list(&root).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Open a short-lived connection to a session's socket, understanding the
    /// request capabilities this backend uses (`paste`, `info`). The session's
    /// own hello decides which it actually honors; unknown ones fall back to the
    /// frozen core.
    fn connect(&self, session: &DiscoveredSession) -> Result<shelbi_client::Connection> {
        let (conn, _events) =
            shelbi_client::Connection::open(&session.sock, None, &[capability::PASTE, capability::INFO])
                .map_err(|e| Error::Other(format!("connecting to session `{}`: {e}", session.meta.name)))?;
        Ok(conn)
    }

    /// Reject a remote host up front: the session backend is local-only until
    /// Phase 5 (`rt-remote-spawn`). Returns the error a `Result`-returning op
    /// should surface.
    fn remote_unsupported(op: &str) -> Error {
        Error::Other(format!(
            "session backend cannot {op} on a remote host yet (remote spawn is Phase 5 rt-remote-spawn)"
        ))
    }
}

impl SessionBackend for SessionProcessBackend {
    fn spawn(&self, _host: &Host, _target: &SessionTarget, _command: Option<&str>) -> Result<()> {
        // `spawn` is the trait's *remote* workspace path (the local dispatch
        // uses the inherent `Backend::spawn_local_pane`). Remote session spawn
        // is Phase 5 (`rt-remote-spawn`), so this is unsupported either way.
        Err(Self::remote_unsupported("spawn a session"))
    }

    fn kill(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("kill a session"));
        }
        let name = session_name(target);
        // Best-effort: an already-dead session is fine. Signal the child's
        // process group over the socket; the session writes its exit record and
        // exits once the child is gone.
        if let Some(session) = self.find_live(&name) {
            let conn = self.connect(&session)?;
            conn.kill(None)
                .map_err(|e| Error::Other(format!("killing session `{name}`: {e}")))?;
        }
        Ok(())
    }

    fn probe(&self, host: &Host, target: &SessionTarget, _deadline: Option<Duration>) -> Liveness {
        if host.is_ssh() {
            return Liveness::Unreachable {
                reason: "session backend is local-only (remote is Phase 5 rt-remote-spawn)".into(),
            };
        }
        // A local scan is a filesystem read: it always produces a definitive
        // answer, so the deadline is irrelevant and the probe is never
        // Unreachable. A name that no live session carries is Dead.
        let name = session_name(target);
        match self.find_any(&name) {
            Some(s) if s.alive => Liveness::Alive,
            _ => Liveness::Dead,
        }
    }

    fn send_text(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("send text"));
        }
        let name = session_name(target);
        let session = self
            .find_live(&name)
            .ok_or_else(|| Error::Other(format!("no live session `{name}` to send to")))?;
        let conn = self.connect(&session)?;
        // Paste delivers the text WITHOUT a trailing Enter, with bracketed-paste
        // framing when the program enabled it (the frozen-core fallback is raw
        // input). The submit layer sequences the separate Enter after a settle.
        conn.paste(text)
            .map_err(|e| Error::Other(format!("pasting into session `{name}`: {e}")))
    }

    fn send_enter(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("send enter"));
        }
        let name = session_name(target);
        let session = self
            .find_live(&name)
            .ok_or_else(|| Error::Other(format!("no live session `{name}` to send Enter to")))?;
        let conn = self.connect(&session)?;
        conn.input(b"\r")
            .map_err(|e| Error::Other(format!("sending Enter to session `{name}`: {e}")))
    }

    fn send_line(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        // Text then Enter, over one connection (the injection lock the submit
        // layer holds keeps concurrent callers from interleaving).
        self.send_text(host, target, text)?;
        self.send_enter(host, target)
    }

    fn snapshot(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        self.history(host, target, 0)
    }

    fn history(&self, host: &Host, target: &SessionTarget, lines: usize) -> Result<String> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("snapshot"));
        }
        let name = session_name(target);
        let session = self
            .find_any(&name)
            .ok_or_else(|| Error::Other(format!("no session `{name}` to snapshot")))?;
        let history = if lines == 0 {
            None
        } else {
            Some(lines as u32)
        };
        // One call that reads a live session over its socket or a dead one from
        // `final.txt`, both in `capture-pane -p -J` shape (so the ready.rs /
        // submit.rs detectors read it identically to a tmux capture).
        let snap = shelbi_client::snapshot(&session, history)
            .map_err(|e| Error::Other(format!("snapshotting session `{name}`: {e}")))?;
        Ok(snap.text)
    }

    fn final_screen(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("read a final screen"));
        }
        let name = session_name(target);
        let session = self
            .find_any(&name)
            .ok_or_else(|| Error::Other(format!("no session `{name}` for a final screen")))?;
        // A true post-exit snapshot: a dead session's `final.txt`, or the live
        // screen if it is somehow still up (strictly better than tmux, which
        // keeps no post-exit buffer).
        let snap = shelbi_client::snapshot(&session, None)
            .map_err(|e| Error::Other(format!("reading final screen for `{name}`: {e}")))?;
        let _ = SnapshotSource::Final; // documents the dead-session source
        Ok(snap.text)
    }

    fn title(&self, host: &Host, target: &SessionTarget) -> Result<String> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("read a title"));
        }
        let name = session_name(target);
        // A dead session carries no live title (it is read from the title
        // event, which only a running emulator emits); report empty, which
        // `parse_pane_title_marker` reads as "no marker".
        let Some(session) = self.find_live(&name) else {
            return Ok(String::new());
        };
        let conn = self.connect(&session)?;
        // The OSC 2 `shelbi:<state>` marker the worker hooks write rides the
        // session title event unchanged into `InfoData.title`.
        let info = conn
            .info()
            .map_err(|e| Error::Other(format!("reading title for session `{name}`: {e}")))?;
        Ok(info.title.unwrap_or_default().trim_end().to_string())
    }

    fn get_metadata(
        &self,
        _host: &Host,
        _target: &SessionTarget,
        _key: &str,
        _deadline: Option<Duration>,
    ) -> Result<Option<String>> {
        // No session-process metadata store yet (Phase 3/4). An unset value is
        // the safe default every caller already handles (e.g. "not a user
        // shell"). Never an error, so a probe can't read it as a transport
        // failure.
        Ok(None)
    }

    fn set_metadata(
        &self,
        _host: &Host,
        _target: &SessionTarget,
        _key: &str,
        _value: &str,
    ) -> Result<()> {
        // No-op: see `get_metadata`. Stamping a mark that nothing reads back is
        // harmless, and the value would die with the session regardless.
        Ok(())
    }

    fn get_env(&self, _host: &Host, _target: &SessionTarget, _var: &str) -> Result<Option<String>> {
        // No queryable per-session environment store (tmux `show-environment`
        // has no analogue). Callers (`SHELBI_PANE_orch`, the review keys) treat
        // `None` as "nothing pinned" and rebuild, which is correct here.
        Ok(None)
    }

    fn enumerate_slots(
        &self,
        host: &Host,
        target: &SessionTarget,
        _deadline: Option<Duration>,
    ) -> std::io::Result<Option<Vec<SlotInfo>>> {
        if host.is_ssh() {
            // Couldn't ask on a remote host — distinct from "no sessions".
            return Ok(None);
        }
        // tmux lists the windows inside the shared project session; here each
        // workspace is its own `<project>/ws/<workspace>` session. Enumerate the
        // live ones under this project, keyed so teardown can act on them:
        // `name` is the workspace (what `slot_ids_named` filters on) and `id` is
        // the full logical name `kill_window` kills.
        let project = project_of(target.session_name()).to_string();
        let prefix = format!("{project}/ws/");
        let slots = self
            .discover()
            .into_iter()
            .filter(|s| s.alive)
            .filter_map(|s| {
                s.meta
                    .name
                    .strip_prefix(&prefix)
                    .map(|workspace| SlotInfo {
                        id: s.meta.name.clone(),
                        name: workspace.to_string(),
                    })
            })
            .collect();
        Ok(Some(slots))
    }

    fn respawn(&self, target: &SessionTarget, _cmd: &str) -> RespawnOutcome {
        // A session keeps the binary it started with; there is no respawn in
        // place. The orchestrator restart path (the only caller) already treats
        // a `Failed` outcome as "let a fresh rebuild happen", which is exactly
        // the session model (a crashed session is replaced by a new one, owned
        // by the daemon/supervision in Phase 3).
        RespawnOutcome::Failed {
            target: session_name(target),
            reason: "respawn-in-place has no session-process analogue; a session is replaced by a fresh spawn (Phase 3 supervision)".into(),
        }
    }

    fn resize(&self, host: &Host, target: &SessionTarget, cols: u16, rows: u16) -> Result<()> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("resize"));
        }
        let name = session_name(target);
        let Some(session) = self.find_live(&name) else {
            return Ok(());
        };
        let conn = self.connect(&session)?;
        conn.resize(cols, rows)
            .map_err(|e| Error::Other(format!("resizing session `{name}`: {e}")))
    }

    fn injection_lock(&self, target: &SessionTarget) -> InjectionGuard {
        // The same process-global registry the tmux backend uses, keyed on the
        // target label, so a paste can never interleave regardless of which
        // backend is active.
        injection_guard(&target.label())
    }
}

impl SessionProcessBackend {
    /// Spawn a detached session running `spec`. The local dispatch path maps a
    /// [`LocalPaneTmuxArgs`](crate::workspace::LocalPaneTmuxArgs) to a
    /// [`SpawnSpec`] and calls this; a stand-alone spawn (tests) builds the
    /// spec directly. Errors carry the readable session name for diagnostics.
    pub(crate) fn spawn_session(&self, spec: SpawnSpec) -> Result<shelbi_session::SpawnedSession> {
        let name = spec.name.clone();
        shelbi_client::spawn(&spec)
            .map_err(|e| Error::Other(format!("spawning session `{name}`: {e}")))
    }

    /// Default initial size for a dispatched session (see [`DEFAULT_COLS`]).
    pub(crate) fn default_size() -> (u16, u16) {
        (DEFAULT_COLS, DEFAULT_ROWS)
    }

    /// Kill a session addressed directly by its logical name (not through a
    /// [`SessionTarget`]). Used by the enum's `kill_window` / `kill_pane`
    /// inherent methods, whose `id` is the logical name
    /// [`enumerate_slots`](SessionBackend::enumerate_slots) handed back.
    /// Best-effort: an already-gone session is fine.
    pub(crate) fn kill_by_name(&self, host: &Host, name: &str) -> Result<()> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("kill a session"));
        }
        if let Some(session) = self.find_live(name) {
            let conn = self.connect(&session)?;
            conn.kill(None)
                .map_err(|e| Error::Other(format!("killing session `{name}`: {e}")))?;
        }
        Ok(())
    }

    /// The logical names of every live session. Stands in for the tmux backend's
    /// `live_pane_ids` (there are no panes): a caller confirming a specific
    /// handle still gets a definitive membership answer.
    pub(crate) fn live_session_names(&self, host: &Host) -> Result<Vec<String>> {
        if host.is_ssh() {
            return Err(Self::remote_unsupported("list live sessions"));
        }
        Ok(self
            .discover()
            .into_iter()
            .filter(|s| s.alive)
            .map(|s| s.meta.name)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_target_maps_to_the_plan_workspace_name() {
        let t = SessionTarget::slot("shelbi-demo", "alice");
        assert_eq!(session_name(&t), "demo/ws/alice");
    }

    #[test]
    fn session_target_maps_to_the_orchestrator_name() {
        let t = SessionTarget::session("shelbi-demo");
        assert_eq!(session_name(&t), "demo/orch");
    }

    #[test]
    fn a_non_prefixed_session_keeps_its_name() {
        // A session name without the `shelbi-` prefix is left intact (so the
        // derivation is total and never panics on an unexpected shape).
        let t = SessionTarget::session("standalone");
        assert_eq!(session_name(&t), "standalone/orch");
    }

    #[test]
    fn pane_target_maps_to_a_pane_pseudo_name() {
        let t = SessionTarget::pane("%7");
        assert_eq!(session_name(&t), "pane/%7");
    }

    #[test]
    fn remote_operations_are_rejected_not_silently_successful() {
        let b = SessionProcessBackend;
        let host = Host::Ssh { host: "box".into() };
        let t = SessionTarget::slot("shelbi-demo", "alice");
        assert!(b.spawn(&host, &t, None).is_err());
        assert!(b.kill(&host, &t).is_err());
        assert!(b.send_text(&host, &t, "x").is_err());
        assert!(matches!(
            b.probe(&host, &t, None),
            Liveness::Unreachable { .. }
        ));
        // Enumerate reports "couldn't ask" (None), never an empty list that
        // would read as "no sessions".
        assert_eq!(b.enumerate_slots(&host, &t, None).unwrap(), None);
    }

    #[test]
    fn metadata_and_env_default_to_absent_without_erroring() {
        let b = SessionProcessBackend;
        let t = SessionTarget::slot("shelbi-demo", "alice");
        assert_eq!(b.get_metadata(&Host::Local, &t, "k", None).unwrap(), None);
        assert!(b.set_metadata(&Host::Local, &t, "k", "v").is_ok());
        assert_eq!(b.get_env(&Host::Local, &t, "SHELBI_PANE_orch").unwrap(), None);
    }

    #[test]
    fn respawn_reports_failed_so_the_caller_rebuilds() {
        let b = SessionProcessBackend;
        let outcome = b.respawn(&SessionTarget::pane("%1"), "cmd");
        assert!(matches!(outcome, RespawnOutcome::Failed { .. }));
    }
}
