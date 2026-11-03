//! A project on another machine, read and watched over that host's relay
//! link: [`RemoteSource`] answers every [`ProjectSource`] query by asking the
//! far `shelbi relay`'s `query` service (whose handler is the far machine's
//! own `LocalSource`, reading its own disk), and the subscriptions here wake
//! a remote window's feed from the far daemon:
//!
//! - the `changes` service carries the far hub's change and layout lines
//!   (what `hub.sock` carries locally, [`crate::changes`] and
//!   [`crate::layout_sub`]);
//! - the `control` service carries the far daemon's `Changed` notes (what
//!   the desktop's app-wide control watcher sees locally). The link
//!   subscribes there as [`ClientKind::Remote`], which the far daemon never
//!   tells to re-exec for a version difference (the window decided on the
//!   version before it opened). A deliberate reload (`shelbi reload` on the
//!   far machine) still arrives, as [`ChangeWake::Reload`]: the window
//!   reconnects and decides again whether it may stay native.
//!
//! Nothing here feeds the local app's own relaunch watch
//! (`desktop/src/platform/control.rs`), which only ever watches this
//! machine's daemon.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use shelbi_client::transport::{ShutdownHandle, Transport};
use shelbi_client::{ClientError, ControlClient, Notice, QueryStream, RelayChannel, RelayStream};
use shelbi_core::Reachability;
use shelbi_proto::control::{ClientInfo, ClientKind, ExpectedState};
use shelbi_proto::RelayService;
use shelbi_state::agents_config::AgentsConfig;
use shelbi_state::changelog::ChangelogStamp;
use shelbi_state::keymap::{KeymapDiagnostic, Keymaps};
use shelbi_state::workflow_config::WorkflowConfig;
use shelbi_state::{ErrorLogEntry, LayoutEvent, ZenModeState};

use super::wire::{Answer, AskError, Querier, Query};
use super::{ActivityWindow, ProjectKey, ProjectSource, ReviewOpenTarget, ReviewSlot, WorkspaceDetails};
use crate::activity::ActivityData;
use crate::board::{BoardOutcome, BoardRead, ClosedPage};
use crate::board_search::{SearchFailure, SearchPage};
use crate::changelog::ChangelogData;
use crate::changes::{ChangeSubscription, ChangeWake};
use crate::command::EditItem;
use crate::launch::RemoteLink;
use crate::layout_sub::LayoutSubscription;
use crate::settings::summary::ProjectSummary;
use crate::settings::workflow_editor::ProjectGit;
use crate::settings::workspaces::PoolIndex;
use crate::view::{MachinesModel, SidebarModel};

/// What a remote source and link need of a host's relay channel: its
/// `query` streams and its named services. [`RemoteLink`] is the real one
/// (one `ssh` per host); [`ChannelLink`] wraps any [`RelayChannel`] (tests
/// run an in-process relay).
pub trait RelayLink: Send + Sync {
    /// The host as the user typed it.
    fn host(&self) -> &str;
    /// A fresh `query` stream.
    fn open_query(&self) -> Result<QueryStream, ClientError>;
    /// A fresh stream to one of the relay's services.
    fn open_service(&self, service: RelayService) -> Result<RelayStream, ClientError>;
}

impl RelayLink for RemoteLink {
    fn host(&self) -> &str {
        RemoteLink::host(self)
    }
    fn open_query(&self) -> Result<QueryStream, ClientError> {
        self.channel().open_query()
    }
    fn open_service(&self, service: RelayService) -> Result<RelayStream, ClientError> {
        self.channel().open_service(service)
    }
}

/// A [`RelayLink`] over a relay channel already open to `host`.
pub struct ChannelLink {
    host: String,
    channel: RelayChannel,
}

impl ChannelLink {
    pub fn new(host: impl Into<String>, channel: RelayChannel) -> ChannelLink {
        ChannelLink {
            host: host.into(),
            channel,
        }
    }

    pub fn channel(&self) -> &RelayChannel {
        &self.channel
    }
}

impl RelayLink for ChannelLink {
    fn host(&self) -> &str {
        &self.host
    }
    fn open_query(&self) -> Result<QueryStream, ClientError> {
        self.channel.open_query()
    }
    fn open_service(&self, service: RelayService) -> Result<RelayStream, ClientError> {
        self.channel.open_service(service)
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// How many idle query streams a source keeps open for its next asks.
const IDLE_QUERIERS: usize = 4;

/// One project on a remote host, read by asking the far machine. Each query
/// is one round trip on a `query` stream; concurrent callers each get their
/// own stream, and finished streams are kept for the next ask.
pub struct RemoteSource {
    key: ProjectKey,
    link: Arc<dyn RelayLink>,
    idle: Mutex<Vec<Querier>>,
}

impl RemoteSource {
    /// Project `name` on `link`'s host.
    pub fn new(name: impl Into<String>, link: Arc<dyn RelayLink>) -> RemoteSource {
        RemoteSource {
            key: ProjectKey::remote(link.host(), name),
            link,
            idle: Mutex::new(Vec::new()),
        }
    }

    fn name(&self) -> String {
        self.key.name.clone()
    }

    fn ask(&self, query: Query) -> Result<Answer, AskError> {
        self.ask_staged(query, &mut |_| {})
    }

    fn ask_staged(&self, query: Query, early: &mut dyn FnMut(Answer)) -> Result<Answer, AskError> {
        let idle = self.idle.lock().unwrap_or_else(|p| p.into_inner()).pop();
        let mut querier = match idle {
            Some(querier) => querier,
            None => Querier::new(self.link.open_query()?),
        };
        let answer = querier.ask_staged(query, early);
        // A stream that failed or answered out of turn is dropped (closing
        // it); one whose query merely failed far side is still good.
        if matches!(answer, Ok(_) | Err(AskError::Query(_))) {
            let mut idle = self.idle.lock().unwrap_or_else(|p| p.into_inner());
            if idle.len() < IDLE_QUERIERS {
                idle.push(querier);
            }
        }
        answer
    }

    /// `error` as the text a `Result<_, String>` query answers: the far
    /// reader's own message, or what kept the far side from answering.
    fn text(&self, error: AskError) -> String {
        match error {
            AskError::Query(e) => e.to_string(),
            AskError::Link(e) => format!("couldn't reach {}: {e}", self.link.host()),
            AskError::Protocol(e) => format!("{} answered oddly: {e}", self.link.host()),
        }
    }
}

/// The `$variant` an answer holds, or the protocol error saying it isn't.
/// (The querier already checked the kind, so the error is for form's sake.)
macro_rules! take {
    ($asked:expr, $variant:path) => {
        match $asked {
            Ok($variant(value)) => Ok(value),
            Ok(other) => Err(AskError::Protocol(format!("unexpected `{}` answer", other.name()))),
            Err(e) => Err(e),
        }
    };
}

impl ProjectSource for RemoteSource {
    fn key(&self) -> &ProjectKey {
        &self.key
    }

    fn sidebar(&self) -> Option<SidebarModel> {
        take!(self.ask(Query::Sidebar { project: self.name() }), Answer::Sidebar)
            .ok()
            .flatten()
    }

    fn config_skeleton(&self) -> Option<SidebarModel> {
        take!(self.ask(Query::ConfigSkeleton { project: self.name() }), Answer::ConfigSkeleton)
            .ok()
            .flatten()
    }

    fn project_exists(&self) -> bool {
        take!(self.ask(Query::ProjectExists { project: self.name() }), Answer::ProjectExists).unwrap_or(false)
    }

    fn project_summary(&self) -> Result<ProjectSummary, String> {
        take!(self.ask(Query::ProjectSummary { project: self.name() }), Answer::ProjectSummary)
            .map_err(|e| self.text(e))
    }

    fn edit_targets(&self) -> Vec<EditItem> {
        take!(self.ask(Query::EditTargets { project: self.name() }), Answer::EditTargets).unwrap_or_default()
    }

    fn open_board(&self) -> BoardRead {
        match take!(self.ask(Query::OpenBoard { project: self.name() }), Answer::OpenBoard) {
            Ok(read) => *read,
            Err(e) => {
                let message = self.text(e);
                BoardRead {
                    project: None,
                    state_snapshot: None,
                    workflows: Err(message.clone()),
                    project_statuses: Err(message.clone()),
                    board: BoardOutcome::Failed(message),
                }
            }
        }
    }

    fn closed_page(&self) -> Option<ClosedPage> {
        take!(self.ask(Query::ClosedPage { project: self.name() }), Answer::ClosedPage)
            .ok()
            .flatten()
    }

    fn more_closed(&self, cursor: &str) -> Result<ClosedPage, String> {
        let query = Query::MoreClosed {
            project: self.name(),
            cursor: cursor.to_string(),
        };
        take!(self.ask(query), Answer::MoreClosed).map_err(|e| self.text(e))
    }

    fn search(&self, query: &str, page: u32) -> Result<SearchPage, SearchFailure> {
        let query = Query::Search {
            project: self.name(),
            text: query.to_string(),
            page,
        };
        match take!(self.ask(query), Answer::Search) {
            Ok(result) => result,
            Err(AskError::Link(_)) => Err(SearchFailure::Unreachable),
            Err(e) => Err(SearchFailure::Failed(self.text(e))),
        }
    }

    fn issue_state(&self, id: &str) -> Option<ExpectedState> {
        let query = Query::IssueState {
            project: self.name(),
            id: id.to_string(),
        };
        take!(self.ask(query), Answer::IssueState).ok().flatten()
    }

    fn activity_window(&self, window: usize, early: &mut dyn FnMut(ActivityWindow)) -> ActivityWindow {
        let query = Query::ActivityWindow {
            project: self.name(),
            window,
        };
        let mut on_early = |answer: Answer| {
            if let Answer::ActivityWindow(read) = answer {
                early(*read);
            }
        };
        match take!(self.ask_staged(query, &mut on_early), Answer::ActivityWindow) {
            Ok(read) => *read,
            Err(e) => ActivityWindow {
                data: ActivityData {
                    status: Some(self.text(e)),
                    window: Some(window),
                    ..ActivityData::default()
                },
                ..ActivityWindow::default()
            },
        }
    }

    fn changelog(&self, unless: Option<&ChangelogStamp>) -> Option<ChangelogData> {
        let query = Query::Changelog {
            project: self.name(),
            unless: unless.copied(),
        };
        take!(self.ask(query), Answer::Changelog).ok().flatten()
    }

    fn errors(&self) -> Result<Vec<(ErrorLogEntry, bool)>, String> {
        take!(self.ask(Query::Errors { project: self.name() }), Answer::Errors).map_err(|e| self.text(e))
    }

    fn zen_state(&self) -> Result<ZenModeState, String> {
        take!(self.ask(Query::ZenState { project: self.name() }), Answer::ZenState).map_err(|e| self.text(e))
    }

    fn workspace_details(&self, ws: &str) -> Result<WorkspaceDetails, String> {
        let query = Query::WorkspaceDetails {
            project: self.name(),
            workspace: ws.to_string(),
        };
        take!(self.ask(query), Answer::WorkspaceDetails)
            .map(|details| *details)
            .map_err(|e| self.text(e))
    }

    fn review_open_target(&self, task: &str) -> Result<ReviewOpenTarget, String> {
        let query = Query::ReviewOpenTarget {
            project: self.name(),
            task: task.to_string(),
        };
        take!(self.ask(query), Answer::ReviewOpenTarget)
            .map(|target| *target)
            .map_err(|e| self.text(e))
    }

    fn review_slots(&self) -> Result<Vec<ReviewSlot>, String> {
        take!(self.ask(Query::ReviewSlots { project: self.name() }), Answer::ReviewSlots).map_err(|e| self.text(e))
    }

    fn machines(&self) -> Result<MachinesModel, String> {
        take!(self.ask(Query::Machines { project: self.name() }), Answer::Machines).map_err(|e| self.text(e))
    }

    fn machine_reachability(&self, host: &str) -> Reachability {
        let query = Query::MachineReachability {
            project: self.name(),
            host: host.to_string(),
        };
        take!(self.ask(query), Answer::MachineReachability)
            .unwrap_or_else(|e| Reachability::Unreachable { error: self.text(e) })
    }

    fn workflow_config(&self) -> Result<WorkflowConfig, String> {
        let files = take!(self.ask(Query::WorkflowConfig { project: self.name() }), Answer::WorkflowConfig)
            .map_err(|e| self.text(e))?;
        WorkflowConfig::from_files(*files).map_err(|e| e.to_string())
    }

    fn project_git(&self) -> ProjectGit {
        take!(self.ask(Query::ProjectGit { project: self.name() }), Answer::ProjectGit).unwrap_or_default()
    }

    fn agents_config(&self) -> Result<AgentsConfig, String> {
        let files = take!(self.ask(Query::AgentsConfig { project: self.name() }), Answer::AgentsConfig)
            .map_err(|e| self.text(e))?;
        AgentsConfig::from_files(*files).map_err(|e| e.to_string())
    }

    fn workspaces_config(&self) -> Result<PoolIndex, String> {
        take!(self.ask(Query::WorkspacesConfig { project: self.name() }), Answer::WorkspacesConfig)
            .map_err(|e| self.text(e))
    }

    /// The viewer's own keys with the far project's section over them. When
    /// the far side can't say, just the viewer's keys.
    fn project_keymap(&self) -> (Keymaps, Vec<KeymapDiagnostic>) {
        let overlay = take!(self.ask(Query::ProjectKeymap { project: self.name() }), Answer::ProjectKeymap)
            .ok()
            .flatten();
        shelbi_state::keymap::load_keymaps_with_overlay(&self.key.name, overlay.as_deref())
    }
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

/// The pause between attempts to reopen a dropped service stream.
const RETRY: Duration = Duration::from_millis(1000);

/// The relay stream a subscription thread is reading now, so dropping the
/// subscription can close it: a relay stream's read has no timeout, so the
/// thread would otherwise wait on it forever.
#[derive(Clone, Default)]
pub(crate) struct OpenStream(Arc<Mutex<(bool, Option<ShutdownHandle>)>>);

impl OpenStream {
    /// Keep `shutdown` as the open stream's closer; run it now when the
    /// subscription already stopped.
    fn set(&self, shutdown: ShutdownHandle) {
        let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if slot.0 {
            drop(slot);
            shutdown();
        } else {
            slot.1 = Some(shutdown);
        }
    }

    /// Close the open stream, and any opened from now on.
    pub(crate) fn close(&self) {
        let shutdown = {
            let mut slot = self.0.lock().unwrap_or_else(|p| p.into_inner());
            slot.0 = true;
            slot.1.take()
        };
        if let Some(shutdown) = shutdown {
            shutdown();
        }
    }
}

/// Subscribe to `project`'s changes on `link`'s host: the far hub's
/// board/workspace change lines (`changes` service) and the far daemon's
/// `Changed` notes (`control` service, as [`ClientKind::Remote`]), both as
/// [`ChangeWake`]s, plus [`ChangeWake::Reload`] when the far daemon asks its
/// clients to reload. `connected` is `true` while the `changes` stream is
/// open; each stream reopens on its own after a drop.
pub fn subscribe_changes(
    link: Arc<dyn RelayLink>,
    project: &str,
    connected: Arc<AtomicBool>,
) -> (ChangeSubscription, Receiver<ChangeWake>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let lines = OpenStream::default();
    let control = OpenStream::default();
    let mut handles = Vec::new();
    {
        let (link, project, tx, stop, open) = (link.clone(), project.to_string(), tx.clone(), stop.clone(), lines.clone());
        handles.extend(spawn("shelbi-remote-changes", move || {
            line_loop(&*link, &project, &stop, &open, &connected, |buf| {
                crate::changes::drain_lines(buf, &project, &tx)
            })
        }));
    }
    {
        let (project, stop, open) = (project.to_string(), stop.clone(), control.clone());
        handles.extend(spawn("shelbi-remote-control", move || {
            control_loop(&*link, &project, &stop, &open, &tx)
        }));
    }
    (ChangeSubscription::remote(stop, handles, vec![lines, control]), rx)
}

/// Subscribe to `project`'s layout events on `link`'s host, from the far
/// hub's `changes` lines.
pub fn subscribe_layout(link: Arc<dyn RelayLink>, project: &str) -> (LayoutSubscription, Receiver<LayoutEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let open = OpenStream::default();
    let handle = {
        let (project, stop, open) = (project.to_string(), stop.clone(), open.clone());
        spawn("shelbi-remote-layout", move || {
            let connected = AtomicBool::new(false);
            line_loop(&*link, &project, &stop, &open, &connected, |buf| {
                crate::layout_sub::drain_lines(buf, &project, &tx)
            })
        })
    };
    (LayoutSubscription::remote(stop, handle, open), rx)
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    match thread::Builder::new().name(name.to_string()).spawn(body) {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!(error = %e, "could not start a remote subscription thread");
            None
        }
    }
}

/// Wait out [`RETRY`] in slices, returning early when `stop` is raised.
fn pause(stop: &AtomicBool) {
    for _ in 0..10 {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(RETRY / 10);
    }
}

/// Keep a `changes` stream for `project` open and hand what it carries to
/// `drain` (which splits out whole lines). `drain` failing means the
/// receiver is gone: the loop ends.
fn line_loop(
    link: &dyn RelayLink,
    project: &str,
    stop: &AtomicBool,
    open: &OpenStream,
    connected: &AtomicBool,
    mut drain: impl FnMut(&mut Vec<u8>) -> Result<(), ()>,
) {
    while !stop.load(Ordering::SeqCst) {
        let service = RelayService::Changes {
            project: project.to_string(),
        };
        let halves = link
            .open_service(service)
            .and_then(|stream| Box::new(stream).split());
        if let Ok((mut reader, _writer, shutdown)) = halves {
            open.set(shutdown);
            connected.store(true, Ordering::SeqCst);
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if drain(&mut buf).is_err() {
                            connected.store(false, Ordering::SeqCst);
                            return;
                        }
                    }
                }
            }
            connected.store(false, Ordering::SeqCst);
        }
        pause(stop);
    }
}

/// Keep a `control` subscription for `project` open, as a remote client,
/// turning the far daemon's pushes into wakes.
fn control_loop(link: &dyn RelayLink, project: &str, stop: &AtomicBool, open: &OpenStream, tx: &Sender<ChangeWake>) {
    let client_id = shelbi_client::fresh_client_id();
    while !stop.load(Ordering::SeqCst) {
        let service = RelayService::Control {
            project: project.to_string(),
        };
        let subscribed = link
            .open_service(service)
            .and_then(|stream| Box::new(stream).split())
            .and_then(|(reader, writer, shutdown)| {
                open.set(shutdown);
                let reader: Box<dyn Read + Send> = reader;
                ControlClient::over(reader, writer, shelbi_state::CLIENT_VERSION)
            })
            .and_then(|client| {
                client.subscribe_as(ClientInfo {
                    client_id: client_id.clone(),
                    kind: ClientKind::Remote,
                    host: shelbi_client::hostname(),
                    pid: Some(std::process::id()),
                    project: Some(project.to_string()),
                })
            });
        if let Ok(mut subscription) = subscribed {
            loop {
                let wake = match subscription.recv() {
                    Ok(Some(Notice::Changed(note))) if note.project == project => ChangeWake::Board,
                    Ok(Some(Notice::Reexec { reason })) => ChangeWake::Reload { reason },
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                };
                let reload = matches!(wake, ChangeWake::Reload { .. });
                if tx.send(wake).is_err() {
                    return;
                }
                if reload {
                    // Reconnect: the window decides again on its next open.
                    break;
                }
            }
        }
        pause(stop);
    }
}
