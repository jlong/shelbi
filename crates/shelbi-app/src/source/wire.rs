//! The `query` service's messages: what a remote window asks the far
//! machine's `shelbi relay` to read, and what comes back.
//!
//! Each message is one JSON object. On the relay stream it is framed
//! `[len: u32 BE][json]`, the shape `shelbi_proto::control::encode` uses; the
//! relay does the framing (`shelbi_client::relay::QueryStream` on this side,
//! the relay's query worker on the far side), so this module deals in the
//! JSON only. These types live here rather than in `shelbi-proto` because the
//! answers are `shelbi-app` read models, which `shelbi-proto` cannot depend on.
//!
//! - A [`Request`] is `{"id": n, "query": {"query": "<name>", ...args}}`.
//! - A [`Response`] is `{"id": n, "result": {"Ok": {"answer": "<name>",
//!   "value": ...}}}` or `{"id": n, "result": {"Err": {"error": "<kind>",
//!   "message": "..."}}}`, carrying the request's id. A request is answered
//!   by exactly one final reply, after any number marked `"early": true`
//!   (`activity_window` answers early with the titles already at hand,
//!   then in full once the tracker answers).
//!
//! The config models ([`WorkflowConfig`](shelbi_state::workflow_config::WorkflowConfig),
//! [`AgentsConfig`](shelbi_state::agents_config::AgentsConfig)) travel as the
//! files they are built from, and the key bindings as the project's
//! `keys.yaml` section, so the near side builds them with its own loader
//! (the same release, since a native window needs the exact version).
//!
//! The far side answers each request with [`respond`] (or a [`handler`] that
//! wraps it for `shelbi_client::RelayServices`); the near side asks with a
//! [`Querier`]. The answer types are the read models made serializable in
//! [`crate::wire`]; their decoding tolerates missing fields, so an additive
//! field needs no protocol bump. Adding, removing or retyping a query or an
//! answer bumps [`QUERY_PROTOCOL_VERSION`], which a host advertises in its
//! `RemoteListing` (`crate::launch::ssh::NativeCapability`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use shelbi_client::{ClientError, QueryStream};
use shelbi_core::Reachability;
use shelbi_proto::control::ExpectedState;
use shelbi_state::agents_config::AgentsFiles;
use shelbi_state::changelog::ChangelogStamp;
use shelbi_state::workflow_config::WorkflowFiles;
use shelbi_state::{ErrorLogEntry, ZenModeState};

use super::{ActivityWindow, ReviewOpenTarget, ReviewSlot, WorkspaceDetails};
use crate::board::{BoardRead, ClosedPage};
use crate::board_search::{SearchFailure, SearchPage};
use crate::changelog::ChangelogData;
use crate::command::EditItem;
use crate::overlay::palette_search::SearchCorpus;
use crate::settings::summary::ProjectSummary;
use crate::settings::workflow_editor::ProjectGit;
use crate::settings::workspaces::PoolIndex;
use crate::view::{MachinesModel, SidebarModel};

/// Version of the query messages this build speaks. Advertised in the
/// remote listing; a remote window opens natively only when both sides match.
pub const QUERY_PROTOCOL_VERSION: u32 = 1;

/// One read a remote window asks of the far machine, by project: one per
/// [`ProjectSource`](super::ProjectSource) method (plus the palette's
/// search corpus), named and shaped like it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum Query {
    /// The sidebar model (`read_sidebar_model`).
    Sidebar { project: String },
    /// The config-only sidebar skeleton painted before the first full read
    /// (`read_config_skeleton`).
    ConfigSkeleton { project: String },
    /// Whether the project is registered on the far machine.
    ProjectExists { project: String },
    /// The project section of Settings ▸ General.
    ProjectSummary { project: String },
    /// The files Open in editor offers (far paths).
    EditTargets { project: String },
    /// The open board ([`crate::board::read_open_board`]).
    OpenBoard { project: String },
    /// The first page of closed issues ([`crate::board::read_closed_page`]).
    ClosedPage { project: String },
    /// The page of closed issues after `cursor`.
    MoreClosed { project: String, cursor: String },
    /// Page `page` of the tracker's search for `text`.
    Search { project: String, text: String, page: u32 },
    /// Issue `id`'s `(status, revision)` as the store has it now.
    IssueState { project: String, id: String },
    /// The Activity feed's newest `window` lines. Answered twice when titles
    /// wait on the tracker: an early answer, then the full one.
    ActivityWindow { project: String, window: usize },
    /// The changelog, or nothing when its stamp still equals `unless`.
    Changelog {
        project: String,
        #[serde(default)]
        unless: Option<ChangelogStamp>,
    },
    /// The palette's search corpus
    /// ([`crate::overlay::palette_search::read_corpus`]).
    SearchCorpus { project: String },
    /// The error log with unread marks.
    Errors { project: String },
    /// The project's Zen Mode state.
    ZenState { project: String },
    /// Workspace `workspace`'s details.
    WorkspaceDetails { project: String, workspace: String },
    /// How to open `task`'s review.
    ReviewOpenTarget { project: String, task: String },
    /// Every review slot with what's loaded on it.
    ReviewSlots { project: String },
    /// The Machines view model (`read_machines_model`).
    Machines { project: String },
    /// Whether `host` answers over SSH, probed from the far machine.
    MachineReachability { project: String, host: String },
    /// The files the project's workflow config is built from.
    WorkflowConfig { project: String },
    /// The project's git settings the workflow editor shows.
    ProjectGit { project: String },
    /// The files the project's agents config is built from.
    AgentsConfig { project: String },
    /// The project's machines and workspaces (Settings ▸ Workspaces).
    WorkspacesConfig { project: String },
    /// The project's section of the far machine's `keys.yaml`. The far
    /// user's own `defaults` are theirs and never sent.
    ProjectKeymap { project: String },
}

impl Query {
    /// The query's wire name.
    pub fn name(&self) -> &'static str {
        match self {
            Query::Sidebar { .. } => "sidebar",
            Query::ConfigSkeleton { .. } => "config_skeleton",
            Query::ProjectExists { .. } => "project_exists",
            Query::ProjectSummary { .. } => "project_summary",
            Query::EditTargets { .. } => "edit_targets",
            Query::OpenBoard { .. } => "open_board",
            Query::ClosedPage { .. } => "closed_page",
            Query::MoreClosed { .. } => "more_closed",
            Query::Search { .. } => "search",
            Query::IssueState { .. } => "issue_state",
            Query::ActivityWindow { .. } => "activity_window",
            Query::Changelog { .. } => "changelog",
            Query::SearchCorpus { .. } => "search_corpus",
            Query::Errors { .. } => "errors",
            Query::ZenState { .. } => "zen_state",
            Query::WorkspaceDetails { .. } => "workspace_details",
            Query::ReviewOpenTarget { .. } => "review_open_target",
            Query::ReviewSlots { .. } => "review_slots",
            Query::Machines { .. } => "machines",
            Query::MachineReachability { .. } => "machine_reachability",
            Query::WorkflowConfig { .. } => "workflow_config",
            Query::ProjectGit { .. } => "project_git",
            Query::AgentsConfig { .. } => "agents_config",
            Query::WorkspacesConfig { .. } => "workspaces_config",
            Query::ProjectKeymap { .. } => "project_keymap",
        }
    }

    /// The project the query reads.
    pub fn project(&self) -> &str {
        match self {
            Query::Sidebar { project }
            | Query::ConfigSkeleton { project }
            | Query::ProjectExists { project }
            | Query::ProjectSummary { project }
            | Query::EditTargets { project }
            | Query::OpenBoard { project }
            | Query::ClosedPage { project }
            | Query::MoreClosed { project, .. }
            | Query::Search { project, .. }
            | Query::IssueState { project, .. }
            | Query::ActivityWindow { project, .. }
            | Query::Changelog { project, .. }
            | Query::SearchCorpus { project }
            | Query::Errors { project }
            | Query::ZenState { project }
            | Query::WorkspaceDetails { project, .. }
            | Query::ReviewOpenTarget { project, .. }
            | Query::ReviewSlots { project }
            | Query::Machines { project }
            | Query::MachineReachability { project, .. }
            | Query::WorkflowConfig { project }
            | Query::ProjectGit { project }
            | Query::AgentsConfig { project }
            | Query::WorkspacesConfig { project }
            | Query::ProjectKeymap { project } => project,
        }
    }
}

/// The answer to a [`Query`]: the variant of the same name, holding what the
/// source method returns locally (its `Ok` value, when it returns a
/// `Result<_, String>`: a reader's failure is a [`QueryError::Failed`]).
/// The config models travel as the files they are built from, so the near
/// side builds them with the same loader.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "answer", content = "value", rename_all = "snake_case")]
pub enum Answer {
    Sidebar(Option<SidebarModel>),
    ConfigSkeleton(Option<SidebarModel>),
    ProjectExists(bool),
    ProjectSummary(ProjectSummary),
    EditTargets(Vec<EditItem>),
    OpenBoard(Box<BoardRead>),
    ClosedPage(Option<ClosedPage>),
    MoreClosed(ClosedPage),
    Search(Result<SearchPage, SearchFailure>),
    IssueState(Option<ExpectedState>),
    ActivityWindow(Box<ActivityWindow>),
    Changelog(Option<ChangelogData>),
    SearchCorpus(SearchCorpus),
    Errors(Vec<(ErrorLogEntry, bool)>),
    ZenState(ZenModeState),
    WorkspaceDetails(Box<WorkspaceDetails>),
    ReviewOpenTarget(Box<ReviewOpenTarget>),
    ReviewSlots(Vec<ReviewSlot>),
    Machines(MachinesModel),
    MachineReachability(Reachability),
    WorkflowConfig(Box<WorkflowFiles>),
    ProjectGit(ProjectGit),
    AgentsConfig(Box<AgentsFiles>),
    WorkspacesConfig(PoolIndex),
    /// The project's `keys.yaml` section as YAML text; `None` when it has
    /// none.
    ProjectKeymap(Option<String>),
}

impl Answer {
    /// The answer's wire name, equal to the [`Query::name`] it answers.
    pub fn name(&self) -> &'static str {
        match self {
            Answer::Sidebar(_) => "sidebar",
            Answer::ConfigSkeleton(_) => "config_skeleton",
            Answer::ProjectExists(_) => "project_exists",
            Answer::ProjectSummary(_) => "project_summary",
            Answer::EditTargets(_) => "edit_targets",
            Answer::OpenBoard(_) => "open_board",
            Answer::ClosedPage(_) => "closed_page",
            Answer::MoreClosed(_) => "more_closed",
            Answer::Search(_) => "search",
            Answer::IssueState(_) => "issue_state",
            Answer::ActivityWindow(_) => "activity_window",
            Answer::Changelog(_) => "changelog",
            Answer::SearchCorpus(_) => "search_corpus",
            Answer::Errors(_) => "errors",
            Answer::ZenState(_) => "zen_state",
            Answer::WorkspaceDetails(_) => "workspace_details",
            Answer::ReviewOpenTarget(_) => "review_open_target",
            Answer::ReviewSlots(_) => "review_slots",
            Answer::Machines(_) => "machines",
            Answer::MachineReachability(_) => "machine_reachability",
            Answer::WorkflowConfig(_) => "workflow_config",
            Answer::ProjectGit(_) => "project_git",
            Answer::AgentsConfig(_) => "agents_config",
            Answer::WorkspacesConfig(_) => "workspaces_config",
            Answer::ProjectKeymap(_) => "project_keymap",
        }
    }
}

/// Why the far side could not answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum QueryError {
    /// The far side could not read the request at all: a query it does not
    /// know (version skew), or malformed JSON.
    Unsupported { message: String },
    /// The reader ran and failed.
    Failed { message: String },
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::Unsupported { message } | QueryError::Failed { message } => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for QueryError {}

/// A query with the id its [`Response`] echoes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub query: Query,
}

/// The far side's reply to the [`Request`] with the same `id`. A request is
/// answered by exactly one reply without `early`, after any number with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    /// An early answer: another reply to the same request follows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub early: bool,
    pub result: Result<Answer, QueryError>,
}

/// How the far side answers one query: the final answer as the return
/// value, after handing any early ones to the callback.
pub trait Answerer: Fn(Query, &mut dyn FnMut(Answer)) -> Result<Answer, QueryError> {}
impl<F: Fn(Query, &mut dyn FnMut(Answer)) -> Result<Answer, QueryError>> Answerer for F {}

/// Answer one request payload (far side): decode the [`Request`], run
/// `answer` on its query, and hand each encoded [`Response`] to `reply`, any
/// early ones first. A request that does not decode is answered
/// [`QueryError::Unsupported`], under its id when the id itself is readable
/// (else 0), so the asker never hangs on it.
pub fn respond(payload: &[u8], answer: impl Answerer, reply: &mut dyn FnMut(Vec<u8>)) {
    let request = match serde_json::from_slice::<Request>(payload) {
        Ok(request) => request,
        Err(e) => {
            #[derive(Deserialize)]
            struct JustId {
                id: u64,
            }
            let id = serde_json::from_slice::<JustId>(payload).map_or(0, |r| r.id);
            return reply(encode(Response {
                id,
                early: false,
                result: Err(QueryError::Unsupported {
                    message: format!("the far Shelbi can't read this query: {e}"),
                }),
            }));
        }
    };
    let id = request.id;
    let result = answer(request.query, &mut |early: Answer| {
        reply(encode(Response {
            id,
            early: true,
            result: Ok(early),
        }))
    });
    reply(encode(Response {
        id,
        early: false,
        result,
    }))
}

/// `response` as JSON; an answer that won't encode goes out as the failure
/// saying so.
fn encode(response: Response) -> Vec<u8> {
    serde_json::to_vec(&response).unwrap_or_else(|e| {
        let failed = Response {
            id: response.id,
            early: response.early,
            result: Err(QueryError::Failed {
                message: format!("encoding the answer: {e}"),
            }),
        };
        serde_json::to_vec(&failed).expect("a plain error response serializes")
    })
}

/// [`respond`] as the relay's query handler, for
/// `shelbi_client::RelayServices::query`.
pub fn handler<F>(answer: F) -> shelbi_client::QueryHandler
where
    F: Answerer + Send + Sync + 'static,
{
    Arc::new(move |payload: &[u8], reply: &mut dyn FnMut(Vec<u8>)| respond(payload, &answer, reply))
}

/// Why [`Querier::ask`] got no answer.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    /// The relay stream or channel failed (the far side is unreachable, or
    /// closed the stream).
    #[error(transparent)]
    Link(#[from] ClientError),
    /// The far side answered with an error.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// The reply wasn't a well-formed answer to this request.
    #[error("bad query reply: {0}")]
    Protocol(String),
}

/// Asks queries over one relay `query` stream (near side). One request at a
/// time; open another stream for a concurrent asker.
pub struct Querier {
    stream: QueryStream,
    next_id: u64,
}

impl Querier {
    /// Ask over `stream` (from `RelayChannel::open_query`).
    pub fn new(stream: QueryStream) -> Querier {
        Querier { stream, next_id: 1 }
    }

    /// Ask `query` and wait for its answer, checked to be the answer to this
    /// request and of the query's kind. Early answers are skipped.
    pub fn ask(&mut self, query: Query) -> Result<Answer, AskError> {
        self.ask_staged(query, &mut |_| {})
    }

    /// [`ask`](Self::ask), handing each early answer to `early` as it
    /// arrives.
    pub fn ask_staged(&mut self, query: Query, early: &mut dyn FnMut(Answer)) -> Result<Answer, AskError> {
        let id = self.next_id;
        self.next_id += 1;
        let name = query.name();
        let request = serde_json::to_vec(&Request { id, query })
            .map_err(|e| AskError::Protocol(format!("encoding the query: {e}")))?;
        self.stream.send(&request)?;
        loop {
            let reply = self.stream.recv()?;
            let response: Response = serde_json::from_slice(&reply)
                .map_err(|e| AskError::Protocol(format!("decoding the answer: {e}")))?;
            if response.id != id {
                return Err(AskError::Protocol(format!(
                    "answer for request {} arrived for request {id}",
                    response.id
                )));
            }
            let answer = response.result?;
            if answer.name() != name {
                return Err(AskError::Protocol(format!(
                    "asked `{name}`, got a `{}` answer",
                    answer.name()
                )));
            }
            if !response.early {
                return Ok(answer);
            }
            early(answer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json<T: Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap()
    }

    #[test]
    fn requests_and_responses_have_a_pinned_shape() {
        let request = Request {
            id: 7,
            query: Query::OpenBoard {
                project: "demo".into(),
            },
        };
        assert_eq!(json(&request), r#"{"id":7,"query":{"query":"open_board","project":"demo"}}"#);
        let ok = Response {
            id: 7,
            early: false,
            result: Ok(Answer::ProjectExists(true)),
        };
        assert_eq!(json(&ok), r#"{"id":7,"result":{"Ok":{"answer":"project_exists","value":true}}}"#);
        let early = Response { early: true, ..ok.clone() };
        assert_eq!(
            json(&early),
            r#"{"id":7,"early":true,"result":{"Ok":{"answer":"project_exists","value":true}}}"#
        );
        let err = Response {
            id: 8,
            early: false,
            result: Err(QueryError::Failed {
                message: "no".into(),
            }),
        };
        assert_eq!(json(&err), r#"{"id":8,"result":{"Err":{"error":"failed","message":"no"}}}"#);
        assert_eq!(json(&serde_json::from_str::<Response>(&json(&ok)).unwrap()), json(&ok));
        assert_eq!(serde_json::from_str::<Request>(&json(&request)).unwrap(), request);
        let search = Query::Search {
            project: "demo".into(),
            text: "crash".into(),
            page: 2,
        };
        assert_eq!(json(&search), r#"{"query":"search","project":"demo","text":"crash","page":2}"#);
        // A changelog query from before `unless` still reads.
        assert_eq!(
            serde_json::from_str::<Query>(r#"{"query":"changelog","project":"p"}"#).unwrap(),
            Query::Changelog { project: "p".into(), unless: None }
        );
    }

    #[test]
    fn every_query_name_matches_its_answer_and_wire_tag() {
        let p = || "p".to_string();
        let s = || "s".to_string();
        let pairs = [
            (Query::Sidebar { project: p() }, Answer::Sidebar(None)),
            (Query::ConfigSkeleton { project: p() }, Answer::ConfigSkeleton(None)),
            (Query::ProjectExists { project: p() }, Answer::ProjectExists(false)),
            (Query::EditTargets { project: p() }, Answer::EditTargets(Vec::new())),
            (Query::ClosedPage { project: p() }, Answer::ClosedPage(None)),
            (
                Query::Search { project: p(), text: s(), page: 1 },
                Answer::Search(Err(SearchFailure::RateLimited)),
            ),
            (Query::IssueState { project: p(), id: s() }, Answer::IssueState(None)),
            (
                Query::ActivityWindow { project: p(), window: 5 },
                Answer::ActivityWindow(Box::default()),
            ),
            (Query::Changelog { project: p(), unless: None }, Answer::Changelog(None)),
            (Query::SearchCorpus { project: p() }, Answer::SearchCorpus(SearchCorpus::default())),
            (Query::Errors { project: p() }, Answer::Errors(Vec::new())),
            (Query::ZenState { project: p() }, Answer::ZenState(ZenModeState::default())),
            (
                Query::WorkspaceDetails { project: p(), workspace: s() },
                Answer::WorkspaceDetails(Box::default()),
            ),
            (
                Query::ReviewOpenTarget { project: p(), task: s() },
                Answer::ReviewOpenTarget(Box::new(ReviewOpenTarget::Queued { title: s() })),
            ),
            (Query::ReviewSlots { project: p() }, Answer::ReviewSlots(Vec::new())),
            (Query::Machines { project: p() }, Answer::Machines(MachinesModel::default())),
            (
                Query::MachineReachability { project: p(), host: s() },
                Answer::MachineReachability(Reachability::Unknown),
            ),
            (Query::WorkflowConfig { project: p() }, Answer::WorkflowConfig(Box::default())),
            (Query::ProjectGit { project: p() }, Answer::ProjectGit(ProjectGit::default())),
            (Query::AgentsConfig { project: p() }, Answer::AgentsConfig(Box::default())),
            (Query::WorkspacesConfig { project: p() }, Answer::WorkspacesConfig(PoolIndex::default())),
            (Query::ProjectKeymap { project: p() }, Answer::ProjectKeymap(Some("global: {}".into()))),
        ];
        for (query, answer) in pairs {
            assert_eq!(query.name(), answer.name());
            assert_eq!(query.project(), "p");
            assert!(json(&query).contains(&format!("\"query\":\"{}\"", query.name())));
            assert!(json(&answer).contains(&format!("\"answer\":\"{}\"", answer.name())));
            assert_eq!(serde_json::from_str::<Query>(&json(&query)).unwrap(), query);
            assert_eq!(json(&serde_json::from_str::<Answer>(&json(&answer)).unwrap()), json(&answer));
        }
        assert_eq!(Query::OpenBoard { project: p() }.name(), "open_board");
        assert_eq!(Query::ProjectSummary { project: p() }.name(), "project_summary");
        assert_eq!(Query::MoreClosed { project: p(), cursor: s() }.name(), "more_closed");
    }

    fn replies(payload: &[u8], answer: impl Answerer) -> Vec<Response> {
        let mut out = Vec::new();
        respond(payload, answer, &mut |bytes| out.push(serde_json::from_slice(&bytes).unwrap()));
        out
    }

    #[test]
    fn respond_answers_and_reports_unreadable_requests_under_their_id() {
        let answer = |q: Query, _: &mut dyn FnMut(Answer)| match q {
            Query::ProjectExists { project } => Ok(Answer::ProjectExists(project == "here")),
            other => Err(QueryError::Failed {
                message: format!("no {}", other.name()),
            }),
        };

        let ok = replies(br#"{"id":1,"query":{"query":"project_exists","project":"here"}}"#, answer);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].id, 1);
        assert!(matches!(ok[0].result, Ok(Answer::ProjectExists(true))));

        let failed = replies(br#"{"id":2,"query":{"query":"changelog","project":"x"}}"#, answer);
        assert_eq!(failed[0].result.as_ref().err(), Some(&QueryError::Failed { message: "no changelog".into() }));

        // A query from a newer client: answered, under its id.
        let newer = replies(br#"{"id":3,"query":{"query":"teleport","project":"x"}}"#, answer);
        assert_eq!(newer[0].id, 3);
        assert!(matches!(newer[0].result, Err(QueryError::Unsupported { .. })));

        let junk = replies(b"not json", answer);
        assert_eq!(junk[0].id, 0);
        assert!(matches!(junk[0].result, Err(QueryError::Unsupported { .. })));
    }

    #[test]
    fn an_early_answer_goes_out_first_and_the_last_reply_is_final() {
        let staged = |_: Query, early: &mut dyn FnMut(Answer)| {
            early(Answer::Errors(Vec::new()));
            Ok(Answer::ProjectExists(true))
        };
        let out = replies(br#"{"id":4,"query":{"query":"errors","project":"x"}}"#, staged);
        assert_eq!(out.len(), 2);
        assert!(out[0].early && out[0].id == 4);
        assert!(!out[1].early && out[1].id == 4);
    }
}
