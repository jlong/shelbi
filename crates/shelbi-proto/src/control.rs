//! The **mutation control protocol** spoken over the daemon's control socket
//! (`control.sock`), distinct from both the session protocol ([`crate::frame`])
//! and the line-based `hub.sock` worker/event protocol.
//!
//! Shelbi makes the daemon the single owner of issue mutations (see the
//! "Removing tmux" plan, "The daemon executes mutations"). A client — `shelbi
//! issue move|start|assign|edit|add`, the review approve/reject commands, or a
//! `shelbi-app` executor — sends one [`ClientMsg::Mutate`] and receives a stream
//! of [`ServerMsg`]: zero or more [`ServerMsg::Line`] (the exact stdout/stderr
//! the in-process path would have printed, kept byte-identical by construction),
//! terminated by [`ServerMsg::Done`] or [`ServerMsg::Failed`]. The daemon
//! additionally pushes [`ServerMsg::Changed`] to every *other* connected client.
//!
//! Framing is the same length-prefix shape the session protocol uses —
//! `[len: u32 BE][json]` — but the payload is a whole serde-tagged message, and
//! there is no frozen-core guarantee: this protocol rides the exact-version
//! match the mutation guard already enforces, so it may change in lockstep with
//! the daemon/CLI version. Encoding and decoding here are **pure** (operate on
//! byte buffers); socket I/O lives in `shelbi-client` and the daemon.

use std::collections::BTreeMap;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::ProtoError;

/// Version of the control protocol this build speaks, carried in the hello.
/// Unlike the session [`crate::PROTOCOL_VERSION`] this is not a frozen core;
/// a mismatch is surfaced like the daemon-version mutation guard.
///
/// v2 adds [`ClientMsg::ReviewSession`] (the single-process TUI asks the daemon
/// to start/stop a review's editor/diff sessions — `rt-tui-review`), plus the
/// project/Shelbi quit and reload-clients messages
/// ([`ClientMsg::QuitProject`], [`ClientMsg::QuitShelbi`],
/// [`ClientMsg::ReloadClients`] / [`ServerMsg::Reexec`] — `rt-tui-project-quit`).
///
/// v3 adds [`ClientMsg::WorkspaceSession`] (the TUI asks the daemon to
/// start/stop a dev workspace's editor/diff content sessions for the workspace
/// sidebar — the dev-workspace twin of `ReviewSession`).
///
/// v4 makes **attached clients** first-class, so multiple clients can attach to
/// one running session and be listed / detached individually (the detach task):
/// [`ClientMsg::Subscribe`] carries an optional [`ClientInfo`] that registers the
/// connection as an attached UI client; [`ClientMsg::ListClients`] and
/// [`ClientMsg::DetachClients`] let a `shelbi detach` CLI (or the palette) list
/// and detach them; and [`ServerMsg::ClientList`] / [`ServerMsg::Detached`] /
/// [`ServerMsg::Detach`] carry the replies and the push that tells a client to
/// leave (agents keep running).
///
/// v5 adds [`MutationKind::Prioritize`] (reorder an issue within its column
/// through the daemon, so the desktop board's drag-to-reorder and the TUI's
/// reorder keys go through the same per-issue queue as every other mutation).
///
/// v6 adds [`WorkspaceSessionOp::EnsureShell`] / [`WorkspaceSessionOp::CloseShell`]:
/// the desktop app's Workspace view asks the daemon for a plain user shell in
/// the workspace's worktree, shown beside the agent (`dt-workspace-view`).
///
/// v7 adds [`ClientMsg::WriteState`]: the project-state writes a client used
/// to make on this machine's disk (the board's workspace filter and column
/// overrides, Zen Mode, the error log and its read mark, a pane's supervision
/// relaunch) go to the daemon, the single writer, which announces each with a
/// [`ServerMsg::Changed`]. A remote window sends the same message over the
/// relay (`rw-remote-state-writes`).
///
/// v8 adds the settings writes a window makes through the daemon, so a
/// window on a project on another machine makes them there (remote-window
/// P8): [`ClientMsg::SaveConfig`] (a config save the client planned, with the
/// content hash of every file it was planned against, refused as
/// [`MutationError::ConfigChanged`] when one changed since) and
/// [`ClientMsg::AppRequest`] / [`ServerMsg::Reply`] (the app's own requests:
/// workspace and machine changes, machine checks, an editor session; their
/// bodies are `shelbi_app::far`'s).
///
/// v9 adds [`StateWrite::RestartOrchestrator`]: Settings ▸ Orchestrator's
/// Restart orchestrator asks the project's daemon to hand off and replace the
/// orchestrator session, as `shelbi reload` does, workers left running.
pub const CONTROL_PROTOCOL_VERSION: u32 = 9;

/// Upper bound on a single control frame. Generous enough for a large issue body
/// in an `edit`/`add`, far below the session [`crate::MAX_FRAME_LEN`].
pub const MAX_CONTROL_FRAME_LEN: usize = 16 * 1024 * 1024;

const LEN_PREFIX: usize = 4;

/// Which standard stream a [`ServerMsg::Line`] belongs on, so the client can
/// reproduce the in-process path's stdout/stderr split exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// The `(status, revision)` the client was looking at when it issued the
/// mutation. The daemon rejects the command if the issue has moved on, and
/// rechecks this immediately before any irreversible step. There is no issue
/// etag today, so the revision is [`Issue::updated_at`] serialized as RFC3339.
///
/// [`Issue::updated_at`]: https://docs.rs/shelbi-core
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedState {
    /// The issue's status id as the client last read it.
    pub status: String,
    /// The issue's `updated_at` as RFC3339, the stand-in revision.
    pub updated_at: String,
}

/// One in-place body substitution for [`EditSpec`], in command-line order.
/// Literal and regex variants interleave, so they share one ordered list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubOp {
    Literal { from: String, to: String },
    Regex { pattern: String, replacement: String },
}

/// A new issue to create. Everything the client could resolve locally (a body
/// read from `--description` or piped stdin) is already resolved into `body`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSpec {
    pub title: String,
    pub id: Option<String>,
    pub status: String,
    /// The resolved body, or `None` to default it to the title.
    pub body: Option<String>,
    pub depends_on: Vec<String>,
    pub prefers_machine: Option<String>,
    pub workflow: Option<String>,
    pub branch: Option<String>,
    /// Land the new issue at the top of its status's queue (priority 0)
    /// instead of the end, where an add lands by default. Defaulted so an
    /// older client's request still parses (and adds at the end).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub top: bool,
}

/// A non-interactive edit. The `$EDITOR` path and reading piped stdin / a
/// `--body-file` happen on the client; only the resolved result crosses the
/// wire. A `None` field is left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditSpec {
    pub title: Option<String>,
    /// Replace the whole body with this text (from `--body`/`--body-file`/stdin
    /// without `--append`).
    pub body_replace: Option<String>,
    /// Append this text to the existing body (the `--append` path).
    pub body_append: Option<String>,
    /// Ordered in-place substitutions.
    pub subs: Vec<SubOp>,
    pub allow_no_match: bool,
    pub workflow: Option<String>,
    pub branch: Option<String>,
    /// `Some(Some(m))` sets the prefers-machine hint, `Some(None)` clears it,
    /// `None` leaves it unchanged. The clear crosses the wire as an explicit
    /// `null`, which plain `Option<Option<_>>` would read back as "unchanged".
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "explicit_null")]
    pub prefers_machine: Option<Option<String>>,
    pub reason: Option<String>,
    /// The title this edit replaces: the edit is refused as stale if the
    /// stored title differs (compared trimmed). An app editor's save sends
    /// the title it was based on, so it never overwrites a change it hasn't
    /// seen, even where the revision can't tell (a remote tracker bumps it on
    /// its own). Absent from the CLI, and from an older client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_title: Option<String>,
    /// The body this edit replaces, likewise (compared without trailing
    /// whitespace, which the store folds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_body: Option<String>,
    /// Frontmatter keys to change, by name: `Some` sets the key, `None` removes
    /// it. A key a typed issue field owns (`branch`, `depends_on`, ...) is
    /// validated as that field; any other key is a free-form workflow param.
    /// `--param` / `--no-param` send param keys, and an `$EDITOR` save on a
    /// remote tracker sends whichever keys the user changed, so a concurrent
    /// change to any other key survives. Values are JSON because this crate
    /// doesn't know the issue types; the daemon decodes them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub frontmatter: BTreeMap<String, Option<serde_json::Value>>,
}

/// Read a present field as `Some(value)`, so an explicit `null` is `Some(None)`.
/// An absent field stays `None` through `#[serde(default)]`.
fn explicit_null<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// The mutation to perform. Carries the full CLI argument surface so the
/// daemon reproduces the in-process behavior exactly. The issue id (and the
/// project) ride on the enclosing [`MutationRequest`]; [`MutationKind::Add`]
/// carries its own optional id inside [`AddSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationKind {
    Move {
        to: String,
        reason: Option<String>,
        skip_transition_actions: bool,
    },
    Start {
        workspace: Option<String>,
        branch: Option<String>,
        reason: Option<String>,
        force: bool,
        /// The explicit `--take` claim: on a remote tracker, assign an issue
        /// that isn't the user's to them before starting it. Defaulted so an
        /// older client's request still parses (and never takes).
        #[serde(default)]
        take: bool,
    },
    Assign {
        to: String,
        force: bool,
    },
    Unassign,
    Add(Box<AddSpec>),
    Edit(Box<EditSpec>),
    Approve,
    Reject {
        reason: String,
    },
    /// Reorder the issue within its status column: land it at absolute slot
    /// `position` (0 is the top) in the column's canonical order (priority,
    /// then id), clamped into range by the store. Other cards shift.
    Prioritize {
        position: u32,
    },
}

impl MutationKind {
    /// A short noun for the `Changed` notification and logging.
    pub fn verb(&self) -> &'static str {
        match self {
            MutationKind::Move { .. } => "move",
            MutationKind::Start { .. } => "start",
            MutationKind::Assign { .. } => "assign",
            MutationKind::Unassign => "unassign",
            MutationKind::Add(_) => "add",
            MutationKind::Edit(_) => "edit",
            MutationKind::Approve => "approve",
            MutationKind::Reject { .. } => "reject",
            MutationKind::Prioritize { .. } => "prioritize",
        }
    }
}

/// One mutation request. `request_id` correlates the stream of replies (the
/// session protocol has no request ids; this one needs them because a single
/// connection may carry several in-flight mutations).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationRequest {
    pub request_id: u64,
    pub project: String,
    /// The target issue id. Empty for an [`MutationKind::Add`] with no explicit
    /// id (the daemon generates one).
    pub id: String,
    /// The `(status, revision)` the client saw; `None` skips the staleness
    /// gate (e.g. `add`, which has no prior state).
    pub expected: Option<ExpectedState>,
    pub kind: MutationKind,
}

/// Which content session of a review slot a [`ReviewSessionOp::Ensure`] targets.
/// The agent (chat) and dev server are spawned by the dispatch/resume path, not
/// here; this is only the editor and diff tool the review interface shows in a
/// terminal view, each a session named `<project>/review/<slot>/<role>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewRole {
    /// `$EDITOR` opened on the review worktree.
    Editor,
    /// The configured git difftool over the review branch's changes.
    Diff,
}

impl ReviewRole {
    /// The `<role>` component of the session name.
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewRole::Editor => "editor",
            ReviewRole::Diff => "diff",
        }
    }
}

/// What the client wants done with a review's content sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewSessionOp {
    /// Ensure the `role` session for this task's review slot is spawned and
    /// live, so the client can attach a terminal view to it. Idempotent: a
    /// live session is left as is.
    Ensure { role: ReviewRole },
    /// Tear the review interface for this task down: end its editor, diff, and
    /// dev-server sessions and free the slot's port. The agent (chat) session
    /// is left for the slot's normal teardown. Idempotent.
    Close,
    /// Load a *queued* review-column task onto the (free) review slot
    /// `workspace`: check out the branch there, run the status's enter
    /// transition to boot and health-check the dev server, and start the review
    /// agent session — the same load `shelbi`'s tmux review-load Enter performs
    /// (`shelbi_orchestrator::load::load_review_task`). On success the daemon
    /// publishes a `ReviewOpened` layout event so the client opens the native
    /// review interface now the slot is serving (`rt-tui-review-load-queued`).
    Load { workspace: String },
}

/// Client → daemon request to manage a review slot's content sessions
/// (`rt-tui-review`). Replies reuse [`ServerMsg::Done`] / [`ServerMsg::Failed`]
/// keyed by `request_id`, like a mutation; progress lines use
/// [`ServerMsg::Line`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSessionRequest {
    pub request_id: u64,
    pub project: String,
    /// The review-column task whose slot the sessions belong to.
    pub task: String,
    pub op: ReviewSessionOp,
}

/// What the client wants done with a dev workspace's editor/diff content
/// sessions (the workspace-sidebar task). The dev-workspace twin of
/// [`ReviewSessionOp`], keyed by the workspace name rather than a review task.
/// Reuses [`ReviewRole`] (`Editor` / `Diff`) — the roles are identical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceSessionOp {
    /// Ensure the `role` session for this workspace is spawned and live, so the
    /// client can attach a terminal view to it. Idempotent.
    Ensure { role: ReviewRole },
    /// End this workspace's editor and diff content sessions. The agent session
    /// is left running. Idempotent.
    Close,
    /// Ensure the workspace's **user shell** is spawned and live: a plain login
    /// shell in the worktree, named `<project>/ws/<workspace>/shell`, on the
    /// workspace's own machine (over SSH for a remote one). Unlike the
    /// `@shelbi-user-shell` slot shell `shelbi open` starts on an idle
    /// workspace, it sits *beside* the agent session and never occupies the
    /// slot, so dispatch is unaffected. Idempotent.
    EnsureShell,
    /// End the workspace's user shell. Idempotent.
    CloseShell,
}

/// Client → daemon request to manage a dev workspace's editor/diff content
/// sessions (the workspace-sidebar task). Replies reuse [`ServerMsg::Done`] /
/// [`ServerMsg::Failed`] keyed by `request_id`, like a mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSessionRequest {
    pub request_id: u64,
    pub project: String,
    /// The dev workspace whose content sessions these are.
    pub workspace: String,
    pub op: WorkspaceSessionOp,
}

/// A board column's persisted collapse override, for
/// [`StateWrite::SetColumnOverride`]. Mirrors `shelbi_state::KanbanColumnOverride`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnOverride {
    Collapsed,
    Expanded,
}

/// The persistent pane a [`StateWrite::RequestRelaunch`] resets the daemon's
/// supervision budget for. The daemon writes every relaunch key a pass could
/// consume for it (a workspace is governed by more than one pass).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelaunchPane {
    /// The project's orchestrator.
    Orchestrator,
    /// A workspace's agent pane (a dev workspace or a review slot).
    Workspace { workspace: String },
}

/// One write to a project's state files that the daemon makes on a client's
/// behalf (protocol v7). Each was a direct disk write in the client; behind
/// the daemon there is one writer, the change is announced to every other
/// client, and a remote window sends the same message over the relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateWrite {
    /// The board's workspace filter (`state.json`), in its persisted form;
    /// `None` clears it.
    SetWorkspaceFilter { filter: Option<String> },
    /// A board column's collapse override (`state.json`); `None` clears it.
    SetColumnOverride {
        workflow: String,
        status: String,
        value: Option<ColumnOverride>,
    },
    /// Flip Zen Mode (`state.json`) and log the change; `source` is the
    /// event's actor (`user:palette`).
    ToggleZen { source: String },
    /// Append an entry to the project's error log.
    AppendError {
        message: String,
        source: Option<String>,
    },
    /// Empty the project's error log (and its read mark).
    ClearErrors,
    /// Mark every entry in the project's error log read.
    MarkErrorsRead,
    /// Reset a persistent pane's supervision budget so the daemon relaunches
    /// it now (the reopen marker).
    RequestRelaunch { pane: RelaunchPane },
    /// Restart the project's orchestrator now (protocol v9): ask it for its
    /// handoff, end its session and bring a fresh one up, the way
    /// `shelbi reload` replaces it, without restarting the daemon or touching
    /// a worker. The reply waits for the replacement to launch.
    RestartOrchestrator,
}

impl StateWrite {
    /// A short noun for the `Changed` notification and logging.
    pub fn verb(&self) -> &'static str {
        match self {
            StateWrite::SetWorkspaceFilter { .. } => "workspace-filter",
            StateWrite::SetColumnOverride { .. } => "column-override",
            StateWrite::ToggleZen { .. } => "zen",
            StateWrite::AppendError { .. } => "append-error",
            StateWrite::ClearErrors => "clear-errors",
            StateWrite::MarkErrorsRead => "mark-errors-read",
            StateWrite::RequestRelaunch { .. } => "request-relaunch",
            StateWrite::RestartOrchestrator => "restart-orchestrator",
        }
    }
}

/// Client → daemon request to make one [`StateWrite`] to `project`'s state
/// (protocol v7). Replies reuse [`ServerMsg::Done`] / [`ServerMsg::Failed`]
/// keyed by `request_id`; every other subscriber gets a [`ServerMsg::Changed`]
/// naming the project, with an empty issue id and [`StateWrite::verb`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateWriteRequest {
    pub request_id: u64,
    pub project: String,
    pub write: StateWrite,
}

/// Client → daemon: write a config save the client planned with its own
/// comment-preserving patcher (protocol v8). `plan` is a
/// `shelbi_state::config_write::Plan`: the content hash each file it touches
/// had when the client read it, then the writes in order. The daemon
/// validates it against the project (every path inside `surface`'s config,
/// every YAML re-parsed with its own loader), refuses it with
/// [`MutationError::ConfigChanged`] when a file changed since, and otherwise
/// writes it, each file atomically. Replies [`ServerMsg::Done`] or
/// [`ServerMsg::Failed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveConfigRequest {
    pub request_id: u64,
    pub project: String,
    /// The plan's surface (`workflows`, `agents`), for logs and the relay.
    pub surface: String,
    pub plan: serde_json::Value,
}

/// Client → daemon: one of the app's own requests about `project` that run
/// where the project lives (protocol v8): add, change or remove a workspace
/// or machine, check a machine's readiness, open a file in an editor
/// session. `body` is a `shelbi_app::far::Request`; the daemon answers with
/// [`ServerMsg::Reply`] carrying a `shelbi_app::far::Reply`, or
/// [`ServerMsg::Failed`] when it couldn't read the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRequest {
    pub request_id: u64,
    pub project: String,
    pub body: serde_json::Value,
}

/// What kind of UI client is attached to a session. Room is left for the
/// Desktop app attaching to a running CLI session (the detach task's
/// motivating case). [`ClientKind::Remote`] is a remote window's link: another
/// machine's app subscribing over this machine's relay, which the daemon never
/// tells to re-exec for a version difference (a deliberate reload still
/// reaches it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    /// The terminal dashboard (`shelbi`).
    Tui,
    /// The desktop app (future).
    Desktop,
    /// A remote window's link: another machine's app, subscribed over this
    /// machine's relay (`shelbi_app::source::remote`).
    Remote,
}

impl ClientKind {
    /// A short lowercase label for `shelbi detach --list`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ClientKind::Tui => "tui",
            ClientKind::Desktop => "desktop",
            ClientKind::Remote => "remote",
        }
    }
}

/// The identity a UI client announces when it subscribes, so the daemon can
/// register it in the attached-clients registry and a `shelbi detach` can list
/// and target it. The `client_id` is client-generated and stable for the
/// client's lifetime, so a client that reconnects (the daemon bounced) keeps the
/// same registry identity and knows its own id for "detach every client but me".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Stable, client-generated id (opaque, short enough to type).
    pub client_id: String,
    pub kind: ClientKind,
    /// The host the client runs on (its own hostname).
    pub host: String,
    /// The client's process id where it is local; `None` for a remote client.
    pub pid: Option<u32>,
    /// The project the client is currently showing, if any.
    pub project: Option<String>,
}

/// Which attached clients a [`ClientMsg::DetachClients`] targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetachSelector {
    /// Every attached client.
    All,
    /// Every attached client except the one with this id (the "detach other
    /// clients" case: the requester keeps its own window).
    AllExcept { client_id: String },
    /// Just the one client with this id.
    One { client_id: String },
}

/// A snapshot of one attached client, for [`ServerMsg::ClientList`] /
/// `shelbi detach --list`. [`ClientInfo`] plus the daemon-stamped attach time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedClient {
    pub client_id: String,
    pub kind: ClientKind,
    pub host: String,
    pub pid: Option<u32>,
    pub project: Option<String>,
    /// When the client attached, RFC3339 (daemon-stamped).
    pub attached_at: String,
}

/// Client → daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First frame on every connection. Lets the daemon version-gate.
    Hello {
        protocol: u32,
        client_version: String,
    },
    /// Subscribe this connection to [`ServerMsg::Changed`] /
    /// [`ServerMsg::Reexec`] / [`ServerMsg::Detach`] notifications. When `info`
    /// is `Some`, the connection is also registered as an **attached UI client**
    /// (listable and detachable); an info-less subscribe (an internal watcher)
    /// still receives the pushes but is not tracked in the client registry.
    Subscribe { info: Option<ClientInfo> },
    /// Run a mutation.
    Mutate(MutationRequest),
    /// Quit a project (removing-tmux Phase 4f): end that project's sessions,
    /// cancel its in-flight daemon jobs through the quit barrier, and mark it
    /// closed. The daemon replies [`ServerMsg::Done`] once the project has
    /// drained (or [`ServerMsg::Failed`]). Other projects are untouched.
    QuitProject { request_id: u64, project: String },
    /// Quit Shelbi entirely (Phase 4f): mark every open project closed, end all
    /// sessions, reply [`ServerMsg::Done`], then stop the daemon. Nothing
    /// restarts it — the projects are closed before the sessions end, so no
    /// session watchdog resurrects the daemon.
    QuitShelbi { request_id: u64 },
    /// Ask the daemon to tell every *subscribed* client to re-exec — the
    /// `shelbi reload` signal. The daemon broadcasts [`ServerMsg::Reexec`] to
    /// its subscribers and replies [`ServerMsg::Done`] to the requester.
    ReloadClients { request_id: u64 },
    /// Start or stop a review slot's editor/diff/server sessions
    /// (`rt-tui-review`). The daemon owns their lifetime so they outlive a
    /// client detach; the client only attaches terminal views.
    ReviewSession(ReviewSessionRequest),
    /// Start or stop a dev workspace's editor/diff content sessions (the
    /// workspace-sidebar task). The dev-workspace twin of [`Self::ReviewSession`]:
    /// the daemon owns their lifetime so they outlive a client detach; the client
    /// only attaches terminal views. Protocol v3.
    WorkspaceSession(WorkspaceSessionRequest),
    /// List the attached UI clients (protocol v4). The daemon replies
    /// [`ServerMsg::ClientList`]. Used by `shelbi detach --list`.
    ListClients { request_id: u64 },
    /// Detach the attached clients matching `selector` (protocol v4): the daemon
    /// pushes [`ServerMsg::Detach`] to each so it leaves cleanly (sessions and
    /// agents keep running), and replies [`ServerMsg::Detached`] with the count.
    /// `reason` is the one-line message the detached client prints on exit.
    DetachClients {
        request_id: u64,
        selector: DetachSelector,
        reason: String,
    },
    /// Make one write to a project's state files (protocol v7): the daemon is
    /// the single writer, and announces the change to every other subscriber.
    WriteState(StateWriteRequest),
    /// Write a config save the client planned (protocol v8).
    SaveConfig(SaveConfigRequest),
    /// One of the app's requests that run where the project lives
    /// (protocol v8).
    AppRequest(AppRequest),
}

impl ClientMsg {
    /// The project a request is about, for the requests that name one.
    pub fn project(&self) -> Option<&str> {
        match self {
            ClientMsg::Mutate(r) => Some(&r.project),
            ClientMsg::QuitProject { project, .. } => Some(project),
            ClientMsg::ReviewSession(r) => Some(&r.project),
            ClientMsg::WorkspaceSession(r) => Some(&r.project),
            ClientMsg::WriteState(r) => Some(&r.project),
            ClientMsg::SaveConfig(r) => Some(&r.project),
            ClientMsg::AppRequest(r) => Some(&r.project),
            ClientMsg::Hello { .. }
            | ClientMsg::Subscribe { .. }
            | ClientMsg::QuitShelbi { .. }
            | ClientMsg::ReloadClients { .. }
            | ClientMsg::ListClients { .. }
            | ClientMsg::DetachClients { .. } => None,
        }
    }
}

/// Why a mutation did not run (or could not be accepted). `Display` is the
/// operator-facing message; the CLI turns it into the same error text /
/// exit code the in-process path produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationError {
    /// The issue moved on since the client read it (expected-state gate or a
    /// recheck before an irreversible step).
    Stale {
        expected: ExpectedState,
        actual: ExpectedState,
    },
    /// The issue was not found.
    NotFound { id: String },
    /// The daemon and client are different versions; the client must relaunch.
    VersionMismatch { message: String },
    /// Anything the library returned as an error. `message` is verbatim.
    Backend { message: String },
    /// A [`ClientMsg::SaveConfig`] was refused: these files changed since
    /// the client read them (protocol v8). Nothing was written.
    ConfigChanged { paths: Vec<String> },
    /// The daemon is shutting down and takes no new work: the request did not
    /// start, so sending it again to the next daemon is safe. It waits for the
    /// jobs already running before it exits. No protocol bump: a client that
    /// can't read this variant fails the request either way, and nothing ran.
    ShuttingDown,
}

impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MutationError::Stale { expected, actual } => write!(
                f,
                "issue changed since you looked at it (you saw `{}` rev {}, it is now `{}` rev {}); \
                 nothing was changed — refresh and retry",
                expected.status, expected.updated_at, actual.status, actual.updated_at
            ),
            MutationError::NotFound { id } => write!(f, "issue `{id}` not found"),
            MutationError::VersionMismatch { message } => write!(f, "{message}"),
            MutationError::Backend { message } => write!(f, "{message}"),
            MutationError::ConfigChanged { paths } => {
                write!(f, "changed since it was loaded: {}", paths.join(", "))
            }
            MutationError::ShuttingDown => write!(
                f,
                "the hub daemon is shutting down; nothing was changed, so try again in a moment"
            ),
        }
    }
}

impl std::error::Error for MutationError {}

/// A change the daemon announces to other connected clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeNote {
    pub project: String,
    pub id: String,
    /// The mutation verb ([`MutationKind::verb`]).
    pub verb: String,
    /// The issue's status after the change (best-effort; empty if unknown, e.g.
    /// a deleted issue).
    pub status: String,
    /// The issue's `updated_at` after the change, RFC3339 (empty if unknown).
    pub updated_at: String,
}

impl ChangeNote {
    /// The post-change `(status, revision)`, the shape a follow-up mutation
    /// expects; `None` when the issue was gone by the time it was re-read.
    pub fn state(&self) -> Option<ExpectedState> {
        (!self.status.is_empty()).then(|| ExpectedState {
            status: self.status.clone(),
            updated_at: self.updated_at.clone(),
        })
    }
}

/// Daemon → client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Reply to [`ClientMsg::Hello`].
    Hello {
        protocol: u32,
        daemon_version: String,
    },
    /// A line of user-facing output for an in-flight mutation, to be printed
    /// verbatim on `stream`.
    Line {
        request_id: u64,
        stream: Stream,
        text: String,
    },
    /// The request finished successfully. For an issue mutation, `state` is
    /// the issue's `(status, revision)` right after the change, so the client
    /// that made it can send its next mutation of the same issue against it
    /// instead of the pre-change state it read (which the daemon would refuse
    /// as stale). `None` for every other request, for a mutation whose issue
    /// is gone, and from a daemon older than the field; it is optional on the
    /// wire both ways, so it needs no protocol version.
    Done {
        request_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<ExpectedState>,
        /// The issue the mutation acted on: for an add, the id the daemon
        /// created (so the client can follow up on it). `None` for every other
        /// request and from a daemon older than the field; optional on the
        /// wire both ways, like `state`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// The mutation failed (or was refused).
    Failed {
        request_id: u64,
        error: MutationError,
    },
    /// A change made by another client (broadcast; no `request_id`).
    Changed(ChangeNote),
    /// The client should re-exec (a TUI) or prompt for relaunch and send no
    /// further commands (the desktop app). Pushed to a subscriber when the
    /// daemon reloads ([`ClientMsg::ReloadClients`]) or when the subscriber's
    /// hello announced a version different from the daemon's (it is out of
    /// date). `reason` is a short operator-facing phrase. Broadcast; no
    /// `request_id`.
    Reexec { reason: String },
    /// Reply to [`ClientMsg::ListClients`]: the attached UI clients (protocol v4).
    ClientList {
        request_id: u64,
        clients: Vec<AttachedClient>,
    },
    /// Reply to [`ClientMsg::DetachClients`]: how many attached clients were told
    /// to detach (protocol v4).
    Detached { request_id: u64, count: u32 },
    /// The client should detach now — stop rendering and drop its connections,
    /// leaving every session and agent running (protocol v4). Pushed to a
    /// subscribed client the daemon was asked to detach. `reason` is the one-line
    /// message the client prints on exit. Broadcast to the targets; no
    /// `request_id`.
    Detach { reason: String },
    /// The answer to a [`ClientMsg::AppRequest`] (protocol v8): a
    /// `shelbi_app::far::Reply`.
    Reply {
        request_id: u64,
        body: serde_json::Value,
    },
    /// One step of a long request (a quit) starting, finishing, or being given
    /// up on, so the client can show live progress. No protocol bump: the
    /// daemon sends it only to a client that announced its own exact version
    /// in the hello, which is the only client sure to read it. Every other
    /// client sees the request's `Done` and nothing else, as before.
    Progress { request_id: u64, step: Step },
}

/// One step of a request the daemon reports progress on ([`ServerMsg::Progress`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Stable within one request, so a client can pair a step's finish with its
    /// start. Steps can overlap: independent ones run at the same time.
    pub id: String,
    /// What the step does, for people: "asked orchestrator for handoff".
    pub label: String,
    pub state: StepState,
}

/// Where a [`Step`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepState {
    /// The step has begun.
    Started,
    /// The step finished in `elapsed_ms`. `note` says how it went, when that
    /// is worth showing ("no handoff: orchestrator not running").
    Done {
        elapsed_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// The step stopped waiting after `elapsed_ms` without its condition being
    /// met. `waiting_on` names what never answered ("devbox", "handoff.md").
    GaveUp { elapsed_ms: u64, waiting_on: String },
}

/// Encode a message to its full wire bytes (`[len: u32 BE][json]`).
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    let payload = serde_json::to_vec(msg)?;
    if payload.len() > MAX_CONTROL_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(LEN_PREFIX + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decode a single message from the front of `buf`, returning it and the number
/// of bytes consumed so a caller streaming from a socket can drain frame by
/// frame. [`ProtoError::Incomplete`] means `buf` does not yet hold a whole
/// frame — read more and retry.
pub fn decode<T: DeserializeOwned>(buf: &[u8]) -> Result<(T, usize), ProtoError> {
    if buf.len() < LEN_PREFIX {
        return Err(ProtoError::Incomplete {
            needed: Some(LEN_PREFIX - buf.len()),
        });
    }
    let payload_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if payload_len > MAX_CONTROL_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge(payload_len));
    }
    let total = LEN_PREFIX + payload_len;
    if buf.len() < total {
        return Err(ProtoError::Incomplete {
            needed: Some(total - buf.len()),
        });
    }
    let msg = serde_json::from_slice(&buf[LEN_PREFIX..total])?;
    Ok((msg, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_mutate_request() {
        let msg = ClientMsg::Mutate(MutationRequest {
            request_id: 7,
            project: "p".into(),
            id: "t-1".into(),
            expected: Some(ExpectedState {
                status: "review".into(),
                updated_at: "2026-10-03T00:00:00Z".into(),
            }),
            kind: MutationKind::Move {
                to: "done".into(),
                reason: Some("user:tui".into()),
                skip_transition_actions: false,
            },
        });
        let bytes = encode(&msg).unwrap();
        let (back, n): (ClientMsg, usize) = decode(&bytes).unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!(back, msg);
    }

    #[test]
    fn round_trips_a_prioritize_request() {
        let msg = ClientMsg::Mutate(MutationRequest {
            request_id: 8,
            project: "p".into(),
            id: "t-2".into(),
            expected: None,
            kind: MutationKind::Prioritize { position: 3 },
        });
        let bytes = encode(&msg).unwrap();
        let (back, _): (ClientMsg, usize) = decode(&bytes).unwrap();
        assert_eq!(back, msg);
        assert_eq!(MutationKind::Prioritize { position: 0 }.verb(), "prioritize");
    }

    #[test]
    fn round_trips_every_state_write() {
        let writes = [
            StateWrite::SetWorkspaceFilter {
                filter: Some("ws:alpha".into()),
            },
            StateWrite::SetWorkspaceFilter { filter: None },
            StateWrite::SetColumnOverride {
                workflow: "task".into(),
                status: "done".into(),
                value: Some(ColumnOverride::Collapsed),
            },
            StateWrite::SetColumnOverride {
                workflow: "task".into(),
                status: "done".into(),
                value: None,
            },
            StateWrite::ToggleZen {
                source: "user:palette".into(),
            },
            StateWrite::AppendError {
                message: "Zen toggle failed: x".into(),
                source: Some("zen".into()),
            },
            StateWrite::ClearErrors,
            StateWrite::MarkErrorsRead,
            StateWrite::RequestRelaunch {
                pane: RelaunchPane::Orchestrator,
            },
            StateWrite::RequestRelaunch {
                pane: RelaunchPane::Workspace {
                    workspace: "alpha".into(),
                },
            },
        ];
        let mut verbs = std::collections::BTreeSet::new();
        for (n, write) in writes.into_iter().enumerate() {
            verbs.insert(write.verb());
            let msg = ClientMsg::WriteState(StateWriteRequest {
                request_id: n as u64,
                project: "p".into(),
                write,
            });
            let bytes = encode(&msg).unwrap();
            let (back, used): (ClientMsg, usize) = decode(&bytes).unwrap();
            assert_eq!(used, bytes.len());
            assert_eq!(back, msg);
        }
        assert_eq!(verbs.len(), 7, "each kind of write has its own verb");
    }

    #[test]
    fn decode_reports_incomplete_until_the_whole_frame_is_present() {
        let bytes = encode(&ServerMsg::Done { request_id: 1, state: None, id: None }).unwrap();
        // Just the length prefix, no payload yet.
        let err = decode::<ServerMsg>(&bytes[..LEN_PREFIX]).unwrap_err();
        assert!(matches!(err, ProtoError::Incomplete { .. }));
        // One byte short of the full frame.
        let err = decode::<ServerMsg>(&bytes[..bytes.len() - 1]).unwrap_err();
        assert!(matches!(err, ProtoError::Incomplete { .. }));
        // The whole frame decodes.
        let (_m, n): (ServerMsg, usize) = decode(&bytes).unwrap();
        assert_eq!(n, bytes.len());
    }

    #[test]
    fn round_trips_the_lifecycle_messages() {
        // The Phase 4f additions must survive a wire round-trip like every other
        // control message, so a quit/reload is never misparsed.
        let msgs = [
            ClientMsg::QuitProject {
                request_id: 3,
                project: "alpha".into(),
            },
            ClientMsg::QuitShelbi { request_id: 4 },
            ClientMsg::ReloadClients { request_id: 5 },
        ];
        for msg in msgs {
            let bytes = encode(&msg).unwrap();
            let (back, n): (ClientMsg, usize) = decode(&bytes).unwrap();
            assert_eq!(n, bytes.len());
            assert_eq!(back, msg);
        }
        let reexec = ServerMsg::Reexec {
            reason: "daemon reloaded".into(),
        };
        let bytes = encode(&reexec).unwrap();
        let (back, n): (ServerMsg, usize) = decode(&bytes).unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!(back, reexec);
    }

    #[test]
    fn round_trips_the_attached_client_messages() {
        // The v4 detach additions must survive a wire round-trip like every
        // other control message.
        let info = ClientInfo {
            client_id: "ab12cd".into(),
            kind: ClientKind::Tui,
            host: "studio".into(),
            pid: Some(4242),
            project: Some("alpha".into()),
        };
        let client_msgs = [
            ClientMsg::Subscribe {
                info: Some(info.clone()),
            },
            ClientMsg::Subscribe { info: None },
            ClientMsg::ListClients { request_id: 7 },
            ClientMsg::DetachClients {
                request_id: 8,
                selector: DetachSelector::All,
                reason: "Detached by 'shelbi detach' from studio.".into(),
            },
            ClientMsg::DetachClients {
                request_id: 9,
                selector: DetachSelector::AllExcept {
                    client_id: "ab12cd".into(),
                },
                reason: "r".into(),
            },
            ClientMsg::DetachClients {
                request_id: 10,
                selector: DetachSelector::One {
                    client_id: "ab12cd".into(),
                },
                reason: "r".into(),
            },
        ];
        for msg in client_msgs {
            let bytes = encode(&msg).unwrap();
            let (back, n): (ClientMsg, usize) = decode(&bytes).unwrap();
            assert_eq!(n, bytes.len());
            assert_eq!(back, msg);
        }

        let server_msgs = [
            ServerMsg::ClientList {
                request_id: 7,
                clients: vec![AttachedClient {
                    client_id: info.client_id.clone(),
                    kind: info.kind,
                    host: info.host.clone(),
                    pid: info.pid,
                    project: info.project.clone(),
                    attached_at: "2026-10-07T00:00:00Z".into(),
                }],
            },
            ServerMsg::Detached {
                request_id: 8,
                count: 2,
            },
            ServerMsg::Detach {
                reason: "Detached by 'shelbi detach' from studio.".into(),
            },
        ];
        for msg in server_msgs {
            let bytes = encode(&msg).unwrap();
            let (back, n): (ServerMsg, usize) = decode(&bytes).unwrap();
            assert_eq!(n, bytes.len());
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn done_carries_the_post_change_state_and_omits_it_when_absent() {
        let state = ExpectedState {
            status: "in-progress".into(),
            updated_at: "2026-10-15T10:00:05Z".into(),
        };
        let msg = ServerMsg::Done {
            request_id: 3,
            state: Some(state),
            id: Some("write-docs".into()),
        };
        let (back, _): (ServerMsg, usize) = decode(&encode(&msg).unwrap()).unwrap();
        assert_eq!(back, msg);

        // A bare Done (a daemon that predates the field, or a non-mutation
        // reply) still decodes, with no state; and one without a state is
        // written without the key, so an older client reads it unchanged.
        let old = br#"{"Done":{"request_id":4}}"#;
        let mut frame = (old.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(old);
        let (back, _): (ServerMsg, usize) = decode(&frame).unwrap();
        assert_eq!(back, ServerMsg::Done { request_id: 4, state: None, id: None });
        let bytes = encode(&ServerMsg::Done { request_id: 4, state: None, id: None }).unwrap();
        assert_eq!(&bytes[LEN_PREFIX..], old);
    }

    #[test]
    fn round_trips_the_settings_messages() {
        let msgs = [
            ClientMsg::SaveConfig(SaveConfigRequest {
                request_id: 11,
                project: "alpha".into(),
                surface: "workflows".into(),
                plan: serde_json::json!({"surface": "workflows", "checks": [], "ops": []}),
            }),
            ClientMsg::AppRequest(AppRequest {
                request_id: 12,
                project: "alpha".into(),
                body: serde_json::json!({"check_machine": {"host": "devbox"}}),
            }),
        ];
        for msg in msgs {
            let (back, _): (ClientMsg, usize) = decode(&encode(&msg).unwrap()).unwrap();
            assert_eq!(back.project(), Some("alpha"));
            assert_eq!(back, msg);
        }
        let replies = [
            ServerMsg::Reply {
                request_id: 12,
                body: serde_json::json!({"ok": true}),
            },
            ServerMsg::Failed {
                request_id: 11,
                error: MutationError::ConfigChanged {
                    paths: vec!["/p/workflows/task.yaml".into()],
                },
            },
            ServerMsg::Failed {
                request_id: 13,
                error: MutationError::ShuttingDown,
            },
        ];
        for msg in replies {
            let (back, _): (ServerMsg, usize) = decode(&encode(&msg).unwrap()).unwrap();
            assert_eq!(back, msg);
        }
        assert_eq!(
            MutationError::ConfigChanged {
                paths: vec!["a".into(), "b".into()]
            }
            .to_string(),
            "changed since it was loaded: a, b"
        );
    }

    /// An edit's "clear" has to survive the JSON wire, where it is a `null`.
    #[test]
    fn edit_spec_keeps_a_clear_apart_from_leaving_a_field_alone() {
        let round_trip = |spec: &EditSpec| -> EditSpec {
            serde_json::from_slice(&serde_json::to_vec(spec).unwrap()).unwrap()
        };

        let untouched = EditSpec::default();
        assert_eq!(round_trip(&untouched).prefers_machine, None);

        let cleared = EditSpec {
            prefers_machine: Some(None),
            ..Default::default()
        };
        assert_eq!(round_trip(&cleared).prefers_machine, Some(None));

        let set = EditSpec {
            prefers_machine: Some(Some("m".into())),
            frontmatter: BTreeMap::from([
                ("task".to_string(), Some(serde_json::json!("parent"))),
                ("old".to_string(), None),
            ]),
            ..Default::default()
        };
        assert_eq!(round_trip(&set), set);
    }

    #[test]
    fn two_frames_decode_back_to_back() {
        let mut buf = encode(&ServerMsg::Line {
            request_id: 1,
            stream: Stream::Stdout,
            text: "→ launching".into(),
        })
        .unwrap();
        buf.extend(encode(&ServerMsg::Done { request_id: 1, state: None, id: None }).unwrap());
        let (first, n1): (ServerMsg, usize) = decode(&buf).unwrap();
        let (second, n2): (ServerMsg, usize) = decode(&buf[n1..]).unwrap();
        assert!(matches!(first, ServerMsg::Line { .. }));
        assert!(matches!(second, ServerMsg::Done { request_id: 1, .. }));
        assert_eq!(n1 + n2, buf.len());
    }
}
