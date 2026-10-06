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
//! **Remote workspaces (Phase 5, `rt-remote-spawn`).** A `Host::Ssh` target is
//! a session process on another machine, started over SSH and reached through
//! one relay per machine ([`crate::remote_session`]). Every remote operation
//! below delegates there: spawn runs `shelbi session new` on the remote, and
//! probe / send / snapshot / title / kill / resize / enumerate ride the relay. A
//! machine that cannot be reached reports [`Liveness::Unreachable`] (never
//! `Dead`), so an SSH blip never makes a live remote agent look absent.
//!
//! **Scope (Phase 2+, behind the dev flag).**
//!
//! - **Metadata / session env are not persisted.** tmux user options
//!   (`@shelbi-user-shell`) and the session environment (`SHELBI_PANE_orch`,
//!   the review `SHELBI_REVIEW_*` keys) have no session-process analogue yet;
//!   `get_metadata` / `get_env` return `Ok(None)` and `set_metadata` is a no-op.
//!   Every caller already treats the absent value as a safe default (no pinned
//!   orchestrator pane, no user-shell mark, no parked review interface), so this
//!   degrades to "rebuild from scratch" rather than misbehaving. Persisting
//!   these moves with the daemon/TUI in Phases 3–4.

use std::path::PathBuf;
use std::time::Duration;

use shelbi_client::{DiscoveredSession, SnapshotSource};
use shelbi_core::{Error, Host, Result};
use shelbi_proto::capability;
use shelbi_session::SpawnSpec;

use crate::remote_session;
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

}

impl SessionBackend for SessionProcessBackend {
    fn spawn(&self, host: &Host, target: &SessionTarget, command: Option<&str>) -> Result<()> {
        // `spawn` is the trait's *remote* workspace path (the local dispatch
        // uses the inherent `Backend::spawn_local_pane`). A remote session runs
        // the given launch command under a login shell; the dispatch path
        // (`deploy_and_spawn`) builds the full `cd … && … exec <runner>` line and
        // passes it here, having already resolved+gated the remote binary.
        if !host.is_ssh() {
            return Err(Error::Other(
                "local workspace spawn uses spawn_local_pane, not the neutral spawn".into(),
            ));
        }
        let cmd = command.ok_or_else(|| {
            Error::Other("remote session spawn needs a launch command".into())
        })?;
        let worktree = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let spec = remote_session::remote_launch_spec(
            session_name(target),
            worktree,
            None,
            cmd.to_string(),
        );
        let bin = remote_session::relay_bin_for_host(host);
        remote_session::spawn_remote_session(host, &bin, &spec)
    }

    fn kill(&self, host: &Host, target: &SessionTarget) -> Result<()> {
        let name = session_name(target);
        if host.is_ssh() {
            return remote_session::kill(host, &name);
        }
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
        let name = session_name(target);
        if host.is_ssh() {
            // Over the relay: Alive/Dead when the machine answers, Unreachable
            // when it cannot be reached (never collapsed to Dead).
            return remote_session::probe(host, &name);
        }
        // A local scan is a filesystem read: it always produces a definitive
        // answer, so the deadline is irrelevant and the probe is never
        // Unreachable. A name that no live session carries is Dead.
        match self.find_any(&name) {
            Some(s) if s.alive => Liveness::Alive,
            _ => Liveness::Dead,
        }
    }

    fn send_text(&self, host: &Host, target: &SessionTarget, text: &str) -> Result<()> {
        let name = session_name(target);
        if host.is_ssh() {
            return remote_session::send_text(host, &name, text);
        }
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
        let name = session_name(target);
        if host.is_ssh() {
            return remote_session::send_enter(host, &name);
        }
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
        let name = session_name(target);
        if host.is_ssh() {
            return remote_session::snapshot(host, &name, lines);
        }
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
        let name = session_name(target);
        if host.is_ssh() {
            // A relay bridges live sockets only — a dead remote session's
            // `final.txt` is not reachable this way. Return the live screen when
            // it is still up; otherwise this surfaces an error the crash-record
            // caller already tolerates.
            return remote_session::snapshot(host, &name, 0);
        }
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
        let name = session_name(target);
        if host.is_ssh() {
            return remote_session::title(host, &name);
        }
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
        let project = project_of(target.session_name()).to_string();
        if host.is_ssh() {
            // Over the relay: Some(slots) when the machine answered, None when it
            // could not be asked (distinct from "no sessions").
            return Ok(remote_session::enumerate(host, &project));
        }
        // tmux lists the windows inside the shared project session; here each
        // workspace is its own `<project>/ws/<workspace>` session. Enumerate the
        // live ones under this project, keyed so teardown can act on them:
        // `name` is the workspace (what `slot_ids_named` filters on) and `id` is
        // the full logical name `kill_window` kills.
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
        let name = session_name(target);
        if host.is_ssh() {
            return remote_session::resize(host, &name, cols, rows);
        }
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
            return remote_session::kill(host, name);
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
            return remote_session::live_session_names(host);
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

    /// An SSH seam that can never reach its machine: every relay open and launch
    /// fails. Stands in for an unreachable remote so the three-state behavior is
    /// deterministic (and no real `ssh` is spawned in a unit test).
    struct UnreachableSeam;
    impl remote_session::RemoteSsh for UnreachableSeam {
        fn launch(&self, _host: &Host, _bin: &str, _spec: &SpawnSpec) -> Result<()> {
            Err(Error::Other("unreachable (test)".into()))
        }
        fn open_relay(
            &self,
            _host: &Host,
            _bin: &str,
        ) -> Result<remote_session::RelayHandle> {
            Err(Error::Other("unreachable (test)".into()))
        }
    }

    #[test]
    fn remote_operations_against_an_unreachable_machine() {
        let _g = crate::test_lock::acquire();
        remote_session::set_test_seam(Some(std::sync::Arc::new(UnreachableSeam)));

        let b = SessionProcessBackend;
        let host = Host::Ssh { host: "box".into() };
        let t = SessionTarget::slot("shelbi-demo", "alice");

        // `spawn` with no command can't build a launch line at all.
        assert!(b.spawn(&host, &t, None).is_err());
        // With a command, the launch is attempted through the seam and fails.
        assert!(b.spawn(&host, &t, Some("exec claude")).is_err());
        // A relay we can't open surfaces as an error on ops, and crucially as
        // Unreachable (never Dead) on probe, so supervision won't redispatch.
        assert!(b.kill(&host, &t).is_err());
        assert!(b.send_text(&host, &t, "x").is_err());
        assert!(matches!(
            b.probe(&host, &t, None),
            Liveness::Unreachable { .. }
        ));
        // Enumerate reports "couldn't ask" (None), never an empty list that
        // would read as "no sessions".
        assert_eq!(b.enumerate_slots(&host, &t, None).unwrap(), None);

        remote_session::set_test_seam(None);
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
