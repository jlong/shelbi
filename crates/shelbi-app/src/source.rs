//! The seam between a client's views and every read they make of a project.
//!
//! A window shows one project, named by a [`ProjectKey`] (its name and where
//! it lives: this machine, or a host reached over SSH). It owns a
//! [`ProjectHandle`]: the key, an `Arc<dyn` [`ProjectSource`]`>` its views ask
//! for every read of the project, and a [`ProjectLink`] for everything that
//! isn't a read (mutations and session ops, change and layout subscriptions,
//! the session connector). Views never call a `shelbi_state` or
//! `shelbi_orchestrator` reader for the project directly, so a window for a
//! project on another machine can draw the same views by swapping the source
//! and link behind them.
//!
//! The local source (`shelbi_orchestrator::source::LocalSource`) lives beside
//! the readers it wraps, since this crate must not depend on the orchestrator;
//! it is also what a far machine's `shelbi relay` answers a remote window's
//! queries with (over the `query` service, whose messages are [`wire`]), so
//! local and remote answers come from the same code. Only the local link
//! exists so far: it uses this machine's daemon sockets, as the clients
//! always have.
//!
//! What stays the viewer's own, and so is never asked of a source: the user
//! config (`load_user_config` / `update_user_config`), window state, recent
//! remotes, the update check, feedback, and bootstrap / discovery.
//!
//! Every query is blocking, like the readers it replaces, and runs where they
//! ran: off the UI thread, on a background executor or a worker thread. A
//! query whose reader returns a `Result` answers `Result<_, String>`: every
//! caller showed the error as text, and text crosses a wire as it is.

pub mod answers;
pub mod wire;

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use shelbi_core::Reachability;
use shelbi_proto::control::{ExpectedState, ReviewSessionOp, Stream, WorkspaceSessionOp};
use shelbi_state::agents_config::AgentsConfig;
use shelbi_state::changelog::ChangelogStamp;
use shelbi_state::keymap::{KeymapDiagnostic, Keymaps};
use shelbi_state::workflow_config::WorkflowConfig;
use shelbi_state::{ErrorLogEntry, LayoutEvent, ZenModeState};

pub use answers::{ReviewOpenInfo, ReviewOpenTarget, ReviewSlot, ReviewSlotOccupant, WorkspaceDetails};

use crate::activity::{ActivityData, ProjectInfo};
use crate::board::{BoardRead, ClosedPage};
use crate::board_search::{SearchClient, SearchFailure, SearchPage, SearchWorker};
use crate::changelog::ChangelogData;
use crate::changes::{ChangeSubscription, ChangeWake};
use crate::command::EditItem;
use crate::exec::{ExecError, ExecOutcome, Mutation};
use crate::layout_sub::LayoutSubscription;
use crate::session::Connector;
use crate::settings::summary::ProjectSummary;
use crate::settings::workflow_editor::ProjectGit;
use crate::settings::workspaces::PoolIndex;
use crate::view::{MachinesModel, SidebarModel};

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// Where a project lives.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProjectOrigin {
    /// This machine's Shelbi.
    Local,
    /// The Shelbi on `host`, reached over SSH (`host` as the user typed it).
    Remote { host: String },
}

/// A project as a window names it: its name and where it lives. Everything
/// a client keys by project (feeds, window slots, saved view state) keys by
/// this, so a remote project named like a local one never shares their state.
///
/// Serializes as the bare name for a local project (what the window session
/// file has always held, so an existing file reads unchanged) and as
/// `{"host": .., "name": ..}` for a remote one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectKey {
    pub origin: ProjectOrigin,
    pub name: String,
}

impl ProjectKey {
    /// `name` on this machine.
    pub fn local(name: impl Into<String>) -> Self {
        ProjectKey {
            origin: ProjectOrigin::Local,
            name: name.into(),
        }
    }

    /// `name` on `host`.
    pub fn remote(host: impl Into<String>, name: impl Into<String>) -> Self {
        ProjectKey {
            origin: ProjectOrigin::Remote { host: host.into() },
            name: name.into(),
        }
    }

    pub fn is_local(&self) -> bool {
        self.origin == ProjectOrigin::Local
    }

    /// The host a remote project lives on.
    pub fn host(&self) -> Option<&str> {
        match &self.origin {
            ProjectOrigin::Local => None,
            ProjectOrigin::Remote { host } => Some(host),
        }
    }

    /// Where this machine keeps the client's own state for the project (its
    /// saved view state): a local project's state directory, or for a remote
    /// one `~/.shelbi/remote/<host>/<project>/` here, never on the far side.
    pub fn state_dir(&self) -> shelbi_core::Result<PathBuf> {
        match &self.origin {
            ProjectOrigin::Local => shelbi_state::project_dir(&self.name),
            ProjectOrigin::Remote { host } => Ok(shelbi_state::shelbi_home()?
                .join("remote")
                .join(path_part(host))
                .join(path_part(&self.name))),
        }
    }
}

/// `part` as one path component: a separator or a leading dot can't climb
/// out of `~/.shelbi/remote/`.
fn path_part(part: &str) -> String {
    let safe: String = part
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    match safe.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None => safe,
    }
}

impl fmt::Display for ProjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.origin {
            ProjectOrigin::Local => f.write_str(&self.name),
            ProjectOrigin::Remote { host } => write!(f, "{}:{}", host, self.name),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum KeyForm {
    Local(String),
    Remote { host: String, name: String },
}

impl Serialize for ProjectKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.origin {
            ProjectOrigin::Local => KeyForm::Local(self.name.clone()),
            ProjectOrigin::Remote { host } => KeyForm::Remote {
                host: host.clone(),
                name: self.name.clone(),
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProjectKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match KeyForm::deserialize(deserializer)? {
            KeyForm::Local(name) => ProjectKey::local(name),
            KeyForm::Remote { host, name } => ProjectKey::remote(host, name),
        })
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// One Activity read: the newest `window` lines of the event log with their
/// task titles resolved, and the project config the feed is drawn with.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ActivityWindow {
    pub info: ProjectInfo,
    pub data: ActivityData,
    /// More titles wait than one pass resolves: reading again soon fills
    /// them in (a batch a pass).
    pub more_titles: bool,
}

/// Every read a client's views make of one project, each returning what the
/// reader it stands for returns. Blocking: call it off the UI thread.
///
/// A source answers for exactly one project, [`ProjectSource::key`].
pub trait ProjectSource: Send + Sync {
    /// The project this source answers for.
    fn key(&self) -> &ProjectKey;

    // --- sidebar and project ---------------------------------------------

    /// The sidebar model (`read_sidebar_model`): `None` when the project
    /// doesn't load.
    fn sidebar(&self) -> Option<SidebarModel>;
    /// The config-only sidebar skeleton a first frame paints
    /// (`read_config_skeleton`).
    fn config_skeleton(&self) -> Option<SidebarModel>;
    /// Whether the project's config loads.
    fn project_exists(&self) -> bool;
    /// The project section of Settings ▸ General.
    fn project_summary(&self) -> Result<ProjectSummary, String>;
    /// The files Open in editor offers (the palette's edit targets).
    fn edit_targets(&self) -> Vec<EditItem>;

    // --- board -------------------------------------------------------------

    /// The open board (`shelbi_app::board::read_open_board`).
    fn open_board(&self) -> BoardRead;
    /// The newest page of closed issues (`read_closed_page`).
    fn closed_page(&self) -> Option<ClosedPage>;
    /// The page of closed issues after `cursor` (the done column's "load
    /// more"): a live fetch from the tracker.
    fn more_closed(&self, cursor: &str) -> Result<ClosedPage, String>;
    /// Page `page` (1-based) of the tracker's search for `query`.
    fn search(&self, query: &str, page: u32) -> Result<SearchPage, SearchFailure>;
    /// Issue `id`'s `(status, revision)` as the store has it now: what a
    /// mutation sent without a client snapshot expects.
    fn issue_state(&self, id: &str) -> Option<ExpectedState>;

    // --- activity and history ----------------------------------------------

    /// The Activity feed's newest `window` lines. A read whose titles wait on
    /// the tracker answers twice: `early` with the titles already at hand,
    /// then the return value once the tracker answers. The source keeps the
    /// title cache between reads, so each read only resolves ids it hasn't
    /// seen. The feed's filter is the view's and applies to what comes back.
    fn activity_window(&self, window: usize, early: &mut dyn FnMut(ActivityWindow)) -> ActivityWindow;
    /// The Changelog, or `None` when its directory's stamp still equals
    /// `unless` (nothing changed since the read that stamp came from).
    fn changelog(&self, unless: Option<&ChangelogStamp>) -> Option<ChangelogData>;
    /// The error log, newest first, each with whether it is unread.
    fn errors(&self) -> Result<Vec<(ErrorLogEntry, bool)>, String>;
    /// The project's Zen Mode state.
    fn zen_state(&self) -> Result<ZenModeState, String>;

    // --- workspaces and machines -------------------------------------------

    /// Workspace `ws`'s details (its machine, worktree, branch and task).
    fn workspace_details(&self, ws: &str) -> Result<WorkspaceDetails, String>;
    /// How to open `task`'s review: serving on a slot, or still queued.
    fn review_open_target(&self, task: &str) -> Result<ReviewOpenTarget, String>;
    /// Every review slot with what's loaded on it.
    fn review_slots(&self) -> Result<Vec<ReviewSlot>, String>;
    /// The Machines page.
    fn machines(&self) -> Result<MachinesModel, String>;
    /// Whether `host` answers over SSH, probed from the project's machine
    /// (the machine that would run agents there).
    fn machine_reachability(&self, host: &str) -> Reachability;

    // --- config ------------------------------------------------------------

    /// The project's workflows and statuses.
    fn workflow_config(&self) -> Result<WorkflowConfig, String>;
    /// The project's git settings the workflow editor shows (read with the
    /// workflow config's project YAML).
    fn project_git(&self) -> ProjectGit;
    /// The project's agents.
    fn agents_config(&self) -> Result<AgentsConfig, String>;
    /// The project's machines and workspaces (Settings ▸ Workspaces).
    fn workspaces_config(&self) -> Result<PoolIndex, String>;
    /// The key bindings in effect for the project and what the loader
    /// refused. Locally that's `keys.yaml` with the project's overlay.
    fn project_keymap(&self) -> (Keymaps, Vec<KeymapDiagnostic>);
}

/// A [`SearchClient`] over a project source, for the board's
/// [`SearchWorker`].
pub struct SourceSearch(pub Arc<dyn ProjectSource>);

impl SearchClient for SourceSearch {
    fn search(&self, _project: &str, query: &str, page: u32) -> Result<SearchPage, SearchFailure> {
        self.0.search(query, page)
    }
}

impl SearchWorker {
    /// The board search worker over `source`'s search.
    pub fn over(source: Arc<dyn ProjectSource>) -> SearchWorker {
        let project = source.key().name.clone();
        SearchWorker::spawn(project, Arc::new(SourceSearch(source)))
    }
}

// ---------------------------------------------------------------------------
// Everything that isn't a read
// ---------------------------------------------------------------------------

/// A window's handle to everything about its project that isn't a read:
/// issue mutations and the daemon's review / workspace session ops, the
/// change and layout subscriptions that wake its feed, and the connector its
/// terminal panes attach through. Cheap to clone.
///
/// The local link (the only one so far) uses this machine's daemon sockets.
#[derive(Clone)]
pub struct ProjectLink {
    key: ProjectKey,
    sessions: Arc<dyn Connector>,
}

impl fmt::Debug for ProjectLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectLink").field("key", &self.key).finish_non_exhaustive()
    }
}

impl ProjectLink {
    /// The link to `name` on this machine, its panes attaching through
    /// `sessions`.
    pub fn local(name: impl Into<String>, sessions: Arc<dyn Connector>) -> Self {
        ProjectLink {
            key: ProjectKey::local(name),
            sessions,
        }
    }

    pub fn key(&self) -> &ProjectKey {
        &self.key
    }

    /// The connector the project's terminal panes attach through.
    pub fn sessions(&self) -> Arc<dyn Connector> {
        self.sessions.clone()
    }

    /// Subscribe to the daemon's change notes for the project; `connected`
    /// says whether the subscription is open.
    pub fn subscribe_changes(&self, connected: Arc<AtomicBool>) -> (ChangeSubscription, Receiver<ChangeWake>) {
        crate::changes::spawn_observed(&self.key.name, connected)
    }

    /// Subscribe to the daemon's layout events for the project (a review
    /// slot opening or closing).
    pub fn subscribe_layout(&self) -> (LayoutSubscription, Receiver<LayoutEvent>) {
        crate::layout_sub::spawn(&self.key.name)
    }

    /// Run an issue mutation, expecting the issue at `expected` (`None`: no
    /// expectation). Answers the issue's state right after the change.
    pub fn mutate(
        &self,
        mutation: &Mutation,
        expected: Option<ExpectedState>,
        on_line: &mut dyn FnMut(Stream, &str),
    ) -> Result<Option<ExpectedState>, ExecError> {
        crate::exec_daemon::execute_mutation_expecting(mutation, expected, on_line)
    }

    /// Ask the daemon to start or stop review `task`'s sessions.
    pub fn review_session(&self, task: &str, op: ReviewSessionOp, on_line: &mut dyn FnMut(Stream, &str)) -> ExecOutcome {
        crate::exec_daemon::review_session(&self.key.name, task, op, on_line)
    }

    /// Ask the daemon to start or stop workspace `ws`'s sessions.
    pub fn workspace_session(
        &self,
        ws: &str,
        op: WorkspaceSessionOp,
        on_line: &mut dyn FnMut(Stream, &str),
    ) -> ExecOutcome {
        crate::exec_daemon::workspace_session(&self.key.name, ws, op, on_line)
    }
}

/// What a window owns for the project it shows: the key, the source its
/// views read through, and the link for everything else. Cheap to clone.
#[derive(Clone)]
pub struct ProjectHandle {
    pub source: Arc<dyn ProjectSource>,
    pub link: ProjectLink,
}

impl fmt::Debug for ProjectHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectHandle").field("key", self.key()).finish_non_exhaustive()
    }
}

impl ProjectHandle {
    /// A handle over `source` and `link`, which must name the same project.
    pub fn new(source: Arc<dyn ProjectSource>, link: ProjectLink) -> Self {
        assert_eq!(source.key(), link.key(), "a project's source and link name the same project");
        ProjectHandle { source, link }
    }

    pub fn key(&self) -> &ProjectKey {
        self.source.key()
    }

    /// The project's name.
    pub fn name(&self) -> &str {
        &self.key().name
    }
}

/// A source that knows nothing and a link that reaches nothing, for tests
/// that need a window's handle for a key (a remote one included) without
/// reading or connecting.
#[cfg(any(test, feature = "test-support"))]
pub mod null {
    use super::*;
    use crate::board::BoardOutcome;
    use crate::session::{ConnectFailure, Connected, SessionRef};

    const NOTHING: &str = "a null source has nothing to read";

    impl ProjectHandle {
        /// A handle on `key` over [`NullSource`] and a link whose panes never
        /// connect.
        pub fn null(key: ProjectKey) -> ProjectHandle {
            ProjectHandle::new(
                Arc::new(NullSource { key: key.clone() }),
                ProjectLink {
                    key,
                    sessions: Arc::new(NullSessions),
                },
            )
        }
    }

    pub struct NullSessions;

    impl Connector for NullSessions {
        fn connect(&self, _project: &str, _target: &SessionRef) -> Result<Connected, ConnectFailure> {
            Err(ConnectFailure::Message(NOTHING.into()))
        }
    }

    /// Answers every query with nothing: no model, an error, or the
    /// built-in defaults.
    pub struct NullSource {
        pub key: ProjectKey,
    }

    impl ProjectSource for NullSource {
        fn key(&self) -> &ProjectKey {
            &self.key
        }
        fn sidebar(&self) -> Option<SidebarModel> {
            None
        }
        fn config_skeleton(&self) -> Option<SidebarModel> {
            None
        }
        fn project_exists(&self) -> bool {
            false
        }
        fn project_summary(&self) -> Result<ProjectSummary, String> {
            Err(NOTHING.into())
        }
        fn edit_targets(&self) -> Vec<EditItem> {
            Vec::new()
        }
        fn open_board(&self) -> BoardRead {
            BoardRead {
                project: None,
                state_snapshot: None,
                workflows: Err(NOTHING.into()),
                project_statuses: Err(NOTHING.into()),
                board: BoardOutcome::Cold { banner: None },
            }
        }
        fn closed_page(&self) -> Option<ClosedPage> {
            None
        }
        fn more_closed(&self, _cursor: &str) -> Result<ClosedPage, String> {
            Err(NOTHING.into())
        }
        fn search(&self, _query: &str, _page: u32) -> Result<SearchPage, SearchFailure> {
            Err(SearchFailure::Unsupported)
        }
        fn issue_state(&self, _id: &str) -> Option<ExpectedState> {
            None
        }
        fn activity_window(&self, _window: usize, _early: &mut dyn FnMut(ActivityWindow)) -> ActivityWindow {
            ActivityWindow::default()
        }
        fn changelog(&self, _unless: Option<&ChangelogStamp>) -> Option<ChangelogData> {
            None
        }
        fn errors(&self) -> Result<Vec<(ErrorLogEntry, bool)>, String> {
            Ok(Vec::new())
        }
        fn zen_state(&self) -> Result<ZenModeState, String> {
            Err(NOTHING.into())
        }
        fn workspace_details(&self, _ws: &str) -> Result<WorkspaceDetails, String> {
            Err(NOTHING.into())
        }
        fn review_open_target(&self, _task: &str) -> Result<ReviewOpenTarget, String> {
            Err(NOTHING.into())
        }
        fn review_slots(&self) -> Result<Vec<ReviewSlot>, String> {
            Ok(Vec::new())
        }
        fn machines(&self) -> Result<MachinesModel, String> {
            Err(NOTHING.into())
        }
        fn machine_reachability(&self, _host: &str) -> Reachability {
            Reachability::Unknown
        }
        fn workflow_config(&self) -> Result<WorkflowConfig, String> {
            Err(NOTHING.into())
        }
        fn project_git(&self) -> ProjectGit {
            ProjectGit::default()
        }
        fn agents_config(&self) -> Result<AgentsConfig, String> {
            Err(NOTHING.into())
        }
        fn workspaces_config(&self) -> Result<PoolIndex, String> {
            Err(NOTHING.into())
        }
        fn project_keymap(&self) -> (Keymaps, Vec<KeymapDiagnostic>) {
            shelbi_state::keymap::validate_keymaps_yaml("", None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_key_serializes_as_its_bare_name_and_a_remote_one_with_its_host() {
        let local = ProjectKey::local("shelbi");
        assert_eq!(serde_json::to_string(&local).unwrap(), r#""shelbi""#);
        assert_eq!(serde_json::from_str::<ProjectKey>(r#""shelbi""#).unwrap(), local);

        let remote = ProjectKey::remote("devbox", "shelbi");
        let json = serde_json::to_string(&remote).unwrap();
        assert_eq!(json, r#"{"host":"devbox","name":"shelbi"}"#);
        assert_eq!(serde_json::from_str::<ProjectKey>(&json).unwrap(), remote);
    }

    #[test]
    fn a_remote_project_named_like_a_local_one_is_another_key() {
        let local = ProjectKey::local("shelbi");
        let remote = ProjectKey::remote("devbox", "shelbi");
        assert_ne!(local, remote);
        let set: std::collections::HashSet<_> = [local.clone(), remote.clone(), local].into();
        assert_eq!(set.len(), 2);
        assert_eq!(remote.to_string(), "devbox:shelbi");
        assert_eq!(remote.host(), Some("devbox"));
        assert!(!remote.is_local());
    }

    #[test]
    fn a_remote_projects_state_dir_is_under_this_machines_remote_dir() {
        let _env = crate::test_env::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!("shelbi-key-{}", std::process::id()));
        let prior = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", &home);
        let dir = ProjectKey::remote("me@devbox", "shelbi").state_dir().unwrap();
        let escaped = ProjectKey::remote("../x", "../y").state_dir().unwrap();
        match prior {
            Some(p) => std::env::set_var("SHELBI_HOME", p),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        assert_eq!(dir, home.join("remote").join("me@devbox").join("shelbi"));
        assert_eq!(escaped, home.join("remote").join("_._x").join("_._y"));
    }
}
