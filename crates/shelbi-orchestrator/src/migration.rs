//! Phase 6 cutover: tmux → session-backend migration, tracked per workspace
//! and enforced at dispatch (plan "Phase 6: Cutover", `rt-cutover-migration`).
//!
//! The cutover makes the session-process backend the only runtime. Before it
//! may start an agent in a worktree, that worktree must be **proven idle** — a
//! surviving tmux agent and a fresh session-process agent editing the same
//! checkout is the hazard this module prevents. The durable per-workspace state
//! lives in the project's `state.json`
//! ([`shelbi_state::MigrationState`]); this module owns the logic that fills it
//! in and the gates that read it:
//!
//! - [`ensure_project_openable`] refuses to open a project whose legacy local
//!   `shelbi-<p>` / `_shelbi-<p>` tmux session still exists. That both prevents
//!   a second agent in the hub worktree and keeps an old sidebar's poller from
//!   running beside the new daemon's.
//! - [`run_migration_pass`] walks every declared workspace and records whether
//!   it is migrated. Local workspaces share the one project session, which the
//!   open gate has already proven gone, so they migrate. A remote workspace is
//!   migrated only once the hub reaches its machine and confirms its
//!   `shelbi-w-<ws>` session is absent — killing it first if the user agrees,
//!   and **re-checking** afterwards because today's teardown reports a remote
//!   kill as done even when it failed. An unreachable remote, a declined kill,
//!   or an unverified kill all leave the workspace pending.
//! - [`ensure_workspace_dispatchable`] refuses to dispatch onto a pending
//!   workspace with a message that says why and how to resolve it. The rest of
//!   the project keeps working.
//!
//! Every tmux query and kill runs against the **exact** session name (the
//! `=<name>` anchor, never a prefix) and goes through the [`MigrationProbe`]
//! seam so tests exercise the whole pass against a stub rather than the real
//! tmux server. This is the one place Shelbi still shells out to tmux — a
//! one-time check for a leftover session from the pre-cutover runtime.

use std::time::Duration;

use shelbi_core::{Error, Host, Project, Result, WorkspaceSpec};
use shelbi_state::MigrationState;

/// Wall-clock bound on each remote tmux probe, so an unreachable machine times
/// out (and its workspace stays pending) instead of hanging `shelbi open` on
/// the SSH connect.
const PROBE_DEADLINE: Duration = Duration::from_secs(10);

/// The tmux operations the migration pass and open gate need, behind a seam so
/// tests never touch the real tmux server (the hub runs inside its own live
/// `shelbi-shelbi` session). The production implementation
/// ([`RealMigrationProbe`]) shells out through `shelbi-ssh`; tests install a
/// stub.
pub trait MigrationProbe {
    /// Whether tmux is usable on the local hub at all. `false` means tmux is
    /// not installed, in which case no legacy local session can exist and every
    /// local workspace is trivially migrated.
    fn local_tmux_available(&self) -> bool;

    /// Whether a tmux session named exactly `name` exists on `host`. Returns
    /// `Err` when the query could not be answered (an SSH transport failure, a
    /// wedged connection) — never a false `Ok(false)` — so the caller can leave
    /// an unreachable remote pending instead of wrongly declaring it clean.
    fn session_exists(&self, host: &Host, name: &str) -> Result<bool>;

    /// Kill the tmux session named exactly `name` on `host`. Best-effort and
    /// intentionally returns nothing: the caller re-checks [`session_exists`]
    /// afterwards and never trusts the kill's own outcome.
    fn kill_session(&self, host: &Host, name: &str);
}

/// Production [`MigrationProbe`]: exact-match tmux queries shelled out through
/// `shelbi-ssh` (which routes local and SSH alike). This is the only remaining
/// place Shelbi talks to tmux — a one-time check for a leftover session from the
/// pre-cutover (tmux) runtime, kept so an upgrade migrates safely.
pub struct RealMigrationProbe;

impl RealMigrationProbe {
    /// Exact-match tmux target (`=<name>`, never a prefix) so a query or kill can
    /// only ever hit the session it was asked about — `=shelbi-w-bob` can't
    /// match a live `shelbi-w-bob-2`.
    fn exact(name: &str) -> String {
        format!("={name}")
    }
}

impl MigrationProbe for RealMigrationProbe {
    fn local_tmux_available(&self) -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn session_exists(&self, host: &Host, name: &str) -> Result<bool> {
        // `tmux has-session -t =<name>`: exit 0 exists, 1 absent, anything else
        // is the transport failing — surfaced as Err (never a false Ok(false))
        // so the caller leaves an unreachable remote pending. Bounded so an
        // unreachable machine can't hang `shelbi open` on the SSH connect.
        let target = Self::exact(name);
        let out = shelbi_ssh::run_with_deadline(
            host,
            ["tmux", "has-session", "-t", target.as_str()],
            PROBE_DEADLINE,
        )
        .map_err(Error::Io)?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(Error::Other(format!(
                "could not determine whether tmux session `{name}` exists (transport failure)"
            ))),
        }
    }

    fn kill_session(&self, host: &Host, name: &str) {
        let target = Self::exact(name);
        let _ = shelbi_ssh::run(host, ["tmux", "kill-session", "-t", target.as_str()]);
    }
}

/// How the migration pass obtains the user's agreement before killing a remote
/// workspace's surviving tmux session. Called only when a live `shelbi-w-<ws>`
/// session is actually found; returning `false` leaves the workspace pending.
pub type KillConsent<'a> = dyn FnMut(&MigrationKill<'_>) -> bool + 'a;

/// The subject of a kill-consent prompt: which workspace, on which machine, and
/// the exact session name that would be killed.
pub struct MigrationKill<'a> {
    pub project: &'a str,
    pub workspace: &'a str,
    pub machine: &'a str,
    pub session: &'a str,
}

/// Local tmux session names for a project: the dashboard session `shelbi-<p>`
/// and the hidden views stash `_shelbi-<p>`.
fn local_session_names(project: &str) -> (String, String) {
    (format!("shelbi-{project}"), format!("_shelbi-{project}"))
}

/// Whether a surviving legacy local session blocks opening `project`.
///
/// A surviving `shelbi-<p>` or `_shelbi-<p>` tmux session (from the pre-cutover
/// runtime) means an old sidebar/poller is still running; opening on the new
/// runtime beside it would double-poll and could start a second agent in a
/// worktree the old session still holds. Refuse with a message that says how to
/// close it. A no-op when tmux is not installed (nothing can be left over).
pub fn ensure_project_openable(project: &str) -> Result<()> {
    ensure_project_openable_with(project, &RealMigrationProbe)
}

/// [`ensure_project_openable`] with an injected probe (hermetic tests).
pub fn ensure_project_openable_with(project: &str, probe: &dyn MigrationProbe) -> Result<()> {
    if !probe.local_tmux_available() {
        return Ok(());
    }
    let (main, stash) = local_session_names(project);
    // A transport failure for a *local* query shouldn't wedge opening; treat an
    // unanswerable probe as "not present" so only a confirmed live session
    // blocks the open.
    let main_live = probe.session_exists(&Host::Local, &main).unwrap_or(false);
    let stash_live = probe.session_exists(&Host::Local, &stash).unwrap_or(false);
    if main_live || stash_live {
        let which = if main_live { &main } else { &stash };
        return Err(Error::Other(format!(
            "project `{project}` still has a running tmux session `{which}` from the \
             previous (tmux) runtime. Close it first with `tmux kill-session -t ={which}`, \
             then reopen `{project}`. Opening on the new runtime beside the old session \
             would run two pollers at once and could start a second agent in a worktree \
             the old session still holds."
        )));
    }
    Ok(())
}

/// Outcome of migrating one workspace, for the pass's report and the event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMigration {
    pub workspace: String,
    pub state: MigrationState,
    /// Short stable reason token (e.g. `tmux-absent`, `session-gone`,
    /// `unreachable`, `kill-verified`, `kill-unverified`, `kill-declined`).
    pub detail: &'static str,
}

/// What [`run_migration_pass`] recorded for a project.
#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    pub workspaces: Vec<WorkspaceMigration>,
}

impl MigrationReport {
    /// Workspaces still pending after the pass.
    pub fn pending(&self) -> impl Iterator<Item = &WorkspaceMigration> {
        self.workspaces
            .iter()
            .filter(|w| !w.state.is_migrated())
    }
}

/// Walk every declared workspace in `project` and record its migration state,
/// persisting each result and logging it to `events.log`. See the module docs
/// for the per-workspace rules. `consent` is consulted only when a live remote
/// `shelbi-w-<ws>` session is actually found.
///
/// Intended to run once a project has passed [`ensure_project_openable`] (so
/// the local session is already proven gone). Idempotent: a re-run re-probes
/// remotes and can flip a previously-pending remote to migrated once its
/// machine is reachable and clean.
pub fn run_migration_pass(
    project: &Project,
    consent: &mut KillConsent<'_>,
) -> Result<MigrationReport> {
    run_migration_pass_with(project, &RealMigrationProbe, consent)
}

/// [`run_migration_pass`] with an injected probe (hermetic tests).
pub fn run_migration_pass_with(
    project: &Project,
    probe: &dyn MigrationProbe,
    consent: &mut KillConsent<'_>,
) -> Result<MigrationReport> {
    let local_tmux = probe.local_tmux_available();
    let mut report = MigrationReport::default();
    for ws in &project.workspaces {
        let m = migrate_one_workspace(project, ws, probe, local_tmux, consent);
        persist_and_log(&project.name, &m);
        report.workspaces.push(m);
    }
    Ok(report)
}

fn migrate_one_workspace(
    project: &Project,
    ws: &WorkspaceSpec,
    probe: &dyn MigrationProbe,
    local_tmux: bool,
    consent: &mut KillConsent<'_>,
) -> WorkspaceMigration {
    let host = match project.machine(&ws.machine) {
        Some(m) => m.host(),
        // A workspace pointing at an unknown machine can't be dispatched to
        // anyway; leave it pending rather than claim a migration we didn't do.
        None => return pending(ws, "unknown-machine"),
    };

    match host {
        Host::Local => {
            if !local_tmux {
                return migrated(ws, "tmux-absent");
            }
            let (main, stash) = local_session_names(&project.name);
            // An unanswerable local probe is treated as live (conservative): we
            // don't want to declare a worktree clean we couldn't verify.
            let live = probe.session_exists(&Host::Local, &main).unwrap_or(true)
                || probe.session_exists(&Host::Local, &stash).unwrap_or(true);
            if live {
                pending(ws, "local-session-live")
            } else {
                migrated(ws, "session-gone")
            }
        }
        Host::Ssh { .. } => migrate_remote(project, ws, &host, probe, consent),
    }
}

fn migrate_remote(
    project: &Project,
    ws: &WorkspaceSpec,
    host: &Host,
    probe: &dyn MigrationProbe,
    consent: &mut KillConsent<'_>,
) -> WorkspaceMigration {
    let session = format!("shelbi-w-{}", ws.name);
    match probe.session_exists(host, &session) {
        // Reached the machine, session already gone → migrated.
        Ok(false) => migrated(ws, "session-absent"),
        // Couldn't reach the machine → stays pending.
        Err(_) => pending(ws, "unreachable"),
        // Session is live: needs the user's agreement to kill, then verify.
        Ok(true) => {
            let agreed = consent(&MigrationKill {
                project: &project.name,
                workspace: &ws.name,
                machine: &ws.machine,
                session: &session,
            });
            if !agreed {
                return pending(ws, "kill-declined");
            }
            let _ = shelbi_state::append_migration_event(
                &project.name,
                &ws.name,
                "killed",
                &session,
            );
            probe.kill_session(host, &session);
            // Never trust the kill's exit (teardown reports a remote kill as
            // done even when it failed): re-check and only migrate on a
            // confirmed absence.
            match probe.session_exists(host, &session) {
                Ok(false) => migrated(ws, "kill-verified"),
                Ok(true) => pending(ws, "kill-unverified"),
                Err(_) => pending(ws, "kill-unverified"),
            }
        }
    }
}

fn migrated(ws: &WorkspaceSpec, detail: &'static str) -> WorkspaceMigration {
    WorkspaceMigration {
        workspace: ws.name.clone(),
        state: MigrationState::Migrated,
        detail,
    }
}

fn pending(ws: &WorkspaceSpec, detail: &'static str) -> WorkspaceMigration {
    WorkspaceMigration {
        workspace: ws.name.clone(),
        state: MigrationState::Pending,
        detail,
    }
}

fn persist_and_log(project: &str, m: &WorkspaceMigration) {
    // Best-effort persistence: a failed state write leaves the workspace at its
    // prior recorded state (pending by default), which is the safe direction.
    let _ = shelbi_state::set_workspace_migration_state(project, &m.workspace, m.state);
    let _ = shelbi_state::append_migration_event(project, &m.workspace, m.state.as_str(), m.detail);
}

/// Refuse to dispatch an agent onto `workspace` while it is explicitly
/// [`MigrationState::Pending`] — its worktree may still be held by a tmux agent
/// from the pre-cutover runtime. Every dispatch path funnels through the launch
/// primitives that call this, so a pending workspace can never have an agent
/// started in it.
///
/// A workspace with **no** recorded migration entry reads as dispatchable: the
/// open-time migration pass records an explicit entry for every workspace
/// declared before cutover, so a missing entry means a workspace added after
/// cutover (`shelbi workspace add`) — created on the new runtime, with no
/// pre-cutover tmux agent that could be holding its worktree. Blocking it would
/// wedge dispatch forever.
pub fn ensure_workspace_dispatchable(project: &str, workspace: &str) -> Result<()> {
    if matches!(
        shelbi_state::workspace_migration_state(project, workspace)?,
        Some(MigrationState::Pending)
    ) {
        return Err(Error::Other(format!(
            "workspace `{workspace}` has not finished migrating off tmux, so a new agent \
             can't start in it yet. Its worktree may still be held by a tmux session from \
             the previous runtime. Reopen `{project}` to re-run migration; a remote \
             workspace stays pending until its machine is reachable and its \
             `shelbi-w-{workspace}` session is confirmed gone."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// What a stub probe answers for a session-existence query.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Answer {
        Exists,
        Absent,
        Unreachable,
    }

    struct StubProbe {
        tmux_available: bool,
        /// Whether a successful consent + kill actually clears the session
        /// (flips it to `Absent`). `false` models today's teardown bug: the
        /// kill "succeeds" but the session is still there on recheck.
        kill_effective: bool,
        answers: Mutex<HashMap<String, Answer>>,
        killed: Mutex<Vec<String>>,
    }

    impl StubProbe {
        fn new(tmux_available: bool, kill_effective: bool) -> Self {
            Self {
                tmux_available,
                kill_effective,
                answers: Mutex::new(HashMap::new()),
                killed: Mutex::new(Vec::new()),
            }
        }
        fn set(&self, name: &str, a: Answer) {
            self.answers.lock().unwrap().insert(name.to_string(), a);
        }
    }

    impl MigrationProbe for StubProbe {
        fn local_tmux_available(&self) -> bool {
            self.tmux_available
        }
        fn session_exists(&self, _host: &Host, name: &str) -> Result<bool> {
            match self
                .answers
                .lock()
                .unwrap()
                .get(name)
                .copied()
                .unwrap_or(Answer::Absent)
            {
                Answer::Exists => Ok(true),
                Answer::Absent => Ok(false),
                Answer::Unreachable => Err(Error::Other("unreachable".into())),
            }
        }
        fn kill_session(&self, _host: &Host, name: &str) {
            self.killed.lock().unwrap().push(name.to_string());
            if self.kill_effective {
                self.answers
                    .lock()
                    .unwrap()
                    .insert(name.to_string(), Answer::Absent);
            }
        }
    }

    fn always_consent(_: &MigrationKill<'_>) -> bool {
        true
    }

    fn never_consent(_: &MigrationKill<'_>) -> bool {
        false
    }

    /// A project with one local workspace (`alpha`, machine `hub`) and one
    /// remote workspace (`bob`, machine `remote`).
    fn test_project(name: &str) -> Project {
        use shelbi_core::*;
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        Project {
            session: Default::default(),
            name: name.into(),
            label: None,
            display_name: None,
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/demo".into(),
                    host: None,
                    tags: Vec::new(),
                    forward: None,
                },
                Machine {
                    name: "remote".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/home/u/demo".into(),
                    host: Some("remotehost".into()),
                    tags: Vec::new(),
                    forward: None,
                },
            ],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![
                WorkspaceSpec {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "bob".into(),
                    machine: "remote".into(),
                    tags: Vec::new(),
                    slot: None,
                },
            ],
            workspace_poll_interval_secs: 5,
            github_reconcile_interval_secs: 900,
            workspace_permissions_mode: Some("auto".into()),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            git: GitConfig::default(),
            review: ReviewConfig::default(),
            runners: Default::default(),
            agents: Default::default(),
            issue_tracker: Default::default(),
            detected_shapes: Vec::new(),
        }
    }

    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev_home: Option<std::ffi::OsString>,
        home: PathBuf,
    }
    impl HomeGuard {
        fn new(project: &str) -> Self {
            let lock = crate::test_lock::acquire();
            let home = std::env::temp_dir().join(format!(
                "shelbi-orch-migration-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(home.join("projects").join(project)).unwrap();
            let prev_home = std::env::var_os("SHELBI_HOME");
            std::env::set_var("SHELBI_HOME", &home);
            Self {
                _lock: lock,
                prev_home,
                home,
            }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    // -- open gate (acceptance #3) ------------------------------------------

    #[test]
    fn open_refused_when_main_or_stash_session_survives() {
        let _g = HomeGuard::new("demo");

        // A surviving `shelbi-demo` session refuses the open.
        let probe = StubProbe::new(true, true);
        probe.set("shelbi-demo", Answer::Exists);
        let err = ensure_project_openable_with("demo", &probe).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("shelbi-demo"), "names the live session: {msg}");
        assert!(
            msg.contains("tmux kill-session"),
            "says how to close it: {msg}"
        );

        // The hidden stash session alone also refuses.
        let probe = StubProbe::new(true, true);
        probe.set("_shelbi-demo", Answer::Exists);
        let err = ensure_project_openable_with("demo", &probe).unwrap_err();
        assert!(err.to_string().contains("_shelbi-demo"));

        // Neither session present → open allowed.
        let probe = StubProbe::new(true, true);
        ensure_project_openable_with("demo", &probe).unwrap();

        // tmux not installed → nothing can survive → open allowed.
        let probe = StubProbe::new(false, true);
        probe.set("shelbi-demo", Answer::Exists); // ignored: tmux unavailable
        ensure_project_openable_with("demo", &probe).unwrap();
    }

    // -- remote migration + verification (acceptance #4) --------------------

    #[test]
    fn local_migrates_and_remote_absent_migrates() {
        let _g = HomeGuard::new("demo");
        let p = test_project("demo");
        let probe = StubProbe::new(true, true);
        // No sessions anywhere (open gate already cleared local; remote clean).
        let report = run_migration_pass_with(&p, &probe, &mut always_consent).unwrap();

        assert_eq!(
            shelbi_state::workspace_migration_state("demo", "alpha").unwrap(),
            Some(MigrationState::Migrated)
        );
        assert_eq!(
            shelbi_state::workspace_migration_state("demo", "bob").unwrap(),
            Some(MigrationState::Migrated)
        );
        assert_eq!(report.pending().count(), 0);
    }

    #[test]
    fn remote_live_session_killed_and_verified_migrates() {
        let _g = HomeGuard::new("demo");
        let p = test_project("demo");
        let probe = StubProbe::new(true, /* kill_effective */ true);
        probe.set("shelbi-w-bob", Answer::Exists);

        let report = run_migration_pass_with(&p, &probe, &mut always_consent).unwrap();

        // The exact remote session was killed, then re-checked and confirmed
        // gone, so the workspace is migrated.
        assert_eq!(probe.killed.lock().unwrap().as_slice(), ["shelbi-w-bob"]);
        assert_eq!(
            shelbi_state::workspace_migration_state("demo", "bob").unwrap(),
            Some(MigrationState::Migrated)
        );
        assert_eq!(report.pending().count(), 0);
    }

    #[test]
    fn remote_kill_that_does_not_verify_stays_pending() {
        let _g = HomeGuard::new("demo");
        let p = test_project("demo");
        // kill_effective=false models the teardown bug: the kill "succeeds" but
        // the session is still there on recheck. Must stay pending.
        let probe = StubProbe::new(true, /* kill_effective */ false);
        probe.set("shelbi-w-bob", Answer::Exists);

        run_migration_pass_with(&p, &probe, &mut always_consent).unwrap();

        assert_eq!(probe.killed.lock().unwrap().as_slice(), ["shelbi-w-bob"]);
        assert_eq!(
            shelbi_state::workspace_migration_state("demo", "bob").unwrap(),
            Some(MigrationState::Pending),
            "an unverified kill must leave the workspace pending"
        );
    }

    #[test]
    fn remote_kill_declined_stays_pending_and_never_kills() {
        let _g = HomeGuard::new("demo");
        let p = test_project("demo");
        let probe = StubProbe::new(true, true);
        probe.set("shelbi-w-bob", Answer::Exists);

        run_migration_pass_with(&p, &probe, &mut never_consent).unwrap();

        assert!(
            probe.killed.lock().unwrap().is_empty(),
            "a declined kill must not touch the session"
        );
        assert_eq!(
            shelbi_state::workspace_migration_state("demo", "bob").unwrap(),
            Some(MigrationState::Pending)
        );
    }

    // -- unreachable remote + dispatch gate (acceptance #5, #6) -------------

    #[test]
    fn unreachable_remote_stays_pending_dispatch_refused_others_ok() {
        let _g = HomeGuard::new("demo");
        let p = test_project("demo");
        let probe = StubProbe::new(true, true);
        probe.set("shelbi-w-bob", Answer::Unreachable);

        run_migration_pass_with(&p, &probe, &mut always_consent).unwrap();

        // Unreachable remote stays pending; the local workspace migrated.
        assert_eq!(
            shelbi_state::workspace_migration_state("demo", "bob").unwrap(),
            Some(MigrationState::Pending)
        );
        assert_eq!(
            shelbi_state::workspace_migration_state("demo", "alpha").unwrap(),
            Some(MigrationState::Migrated)
        );

        // Dispatch to the pending remote is refused with a reason...
        let err = ensure_workspace_dispatchable("demo", "bob").unwrap_err();
        assert!(err.to_string().contains("bob"), "names the workspace");
        // ...while dispatch to the migrated local workspace is allowed.
        ensure_workspace_dispatchable("demo", "alpha").unwrap();
    }

    #[test]
    fn in_flight_workspace_redispatches_once_migrated() {
        // Acceptance #6: a workspace that was pending (its tmux agent still
        // live) is refused; once migration flips it to migrated, the same
        // dispatch gate lets the redispatch through.
        let _g = HomeGuard::new("demo");

        shelbi_state::set_workspace_migration_state("demo", "bob", MigrationState::Pending)
            .unwrap();
        assert!(ensure_workspace_dispatchable("demo", "bob").is_err());

        shelbi_state::set_workspace_migration_state("demo", "bob", MigrationState::Migrated)
            .unwrap();
        ensure_workspace_dispatchable("demo", "bob").unwrap();
    }

    #[test]
    fn absent_migration_entry_reads_as_dispatchable() {
        // A workspace with no recorded migration entry (e.g. added after cutover
        // via `shelbi workspace add`) must be dispatchable — only an explicit
        // `Pending` blocks. Blocking a never-recorded workspace would wedge its
        // dispatch forever.
        let _g = HomeGuard::new("demo");
        ensure_workspace_dispatchable("demo", "never-recorded").unwrap();
    }
}
