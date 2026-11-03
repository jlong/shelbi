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
//!   "message": "..."}}}`, carrying the request's id.
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

use crate::board::{BoardRead, ClosedPage};
use crate::changelog::ChangelogData;
use crate::overlay::palette_search::SearchCorpus;
use crate::view::{MachinesModel, SidebarModel};

/// Version of the query messages this build speaks. Advertised in the
/// remote listing; a remote window opens natively only when both sides match.
pub const QUERY_PROTOCOL_VERSION: u32 = 1;

/// One read a remote window asks of the far machine, by project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum Query {
    /// The sidebar model (`read_sidebar_model`).
    Sidebar { project: String },
    /// The config-only sidebar skeleton painted before the first full read
    /// (`read_config_skeleton`).
    ConfigSkeleton { project: String },
    /// The open board ([`crate::board::read_open_board`]).
    OpenBoard { project: String },
    /// The first page of closed issues ([`crate::board::read_closed_page`]).
    ClosedPage { project: String },
    /// The changelog ([`crate::changelog::read_changelog`]).
    Changelog { project: String },
    /// The palette's search corpus
    /// ([`crate::overlay::palette_search::read_corpus`]).
    SearchCorpus { project: String },
    /// The Machines view model (`read_machines_model`).
    Machines { project: String },
    /// Whether the project is registered on the far machine.
    ProjectExists { project: String },
}

impl Query {
    /// The query's wire name.
    pub fn name(&self) -> &'static str {
        match self {
            Query::Sidebar { .. } => "sidebar",
            Query::ConfigSkeleton { .. } => "config_skeleton",
            Query::OpenBoard { .. } => "open_board",
            Query::ClosedPage { .. } => "closed_page",
            Query::Changelog { .. } => "changelog",
            Query::SearchCorpus { .. } => "search_corpus",
            Query::Machines { .. } => "machines",
            Query::ProjectExists { .. } => "project_exists",
        }
    }

    /// The project the query reads.
    pub fn project(&self) -> &str {
        match self {
            Query::Sidebar { project }
            | Query::ConfigSkeleton { project }
            | Query::OpenBoard { project }
            | Query::ClosedPage { project }
            | Query::Changelog { project }
            | Query::SearchCorpus { project }
            | Query::Machines { project }
            | Query::ProjectExists { project } => project,
        }
    }
}

/// The answer to a [`Query`]: the variant of the same name, holding what the
/// reader returns locally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "answer", content = "value", rename_all = "snake_case")]
pub enum Answer {
    Sidebar(Option<SidebarModel>),
    ConfigSkeleton(Option<SidebarModel>),
    OpenBoard(Box<BoardRead>),
    ClosedPage(Option<ClosedPage>),
    Changelog(ChangelogData),
    SearchCorpus(SearchCorpus),
    Machines(MachinesModel),
    ProjectExists(bool),
}

impl Answer {
    /// The answer's wire name, equal to the [`Query::name`] it answers.
    pub fn name(&self) -> &'static str {
        match self {
            Answer::Sidebar(_) => "sidebar",
            Answer::ConfigSkeleton(_) => "config_skeleton",
            Answer::OpenBoard(_) => "open_board",
            Answer::ClosedPage(_) => "closed_page",
            Answer::Changelog(_) => "changelog",
            Answer::SearchCorpus(_) => "search_corpus",
            Answer::Machines(_) => "machines",
            Answer::ProjectExists(_) => "project_exists",
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

/// The far side's reply to the [`Request`] with the same `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub result: Result<Answer, QueryError>,
}

/// Answer one request payload (far side): decode the [`Request`], run
/// `answer` on its query, and encode the [`Response`]. A request that does
/// not decode is answered [`QueryError::Unsupported`], under its id when the
/// id itself is readable (else 0), so the asker never hangs on it.
pub fn respond(payload: &[u8], answer: impl Fn(Query) -> Result<Answer, QueryError>) -> Vec<u8> {
    let response = match serde_json::from_slice::<Request>(payload) {
        Ok(request) => Response {
            id: request.id,
            result: answer(request.query),
        },
        Err(e) => {
            #[derive(Deserialize)]
            struct JustId {
                id: u64,
            }
            let id = serde_json::from_slice::<JustId>(payload).map_or(0, |r| r.id);
            Response {
                id,
                result: Err(QueryError::Unsupported {
                    message: format!("the far Shelbi can't read this query: {e}"),
                }),
            }
        }
    };
    serde_json::to_vec(&response).unwrap_or_else(|e| {
        let failed = Response {
            id: response.id,
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
    F: Fn(Query) -> Result<Answer, QueryError> + Send + Sync + 'static,
{
    Arc::new(move |payload: &[u8]| respond(payload, &answer))
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
    /// request and of the query's kind.
    pub fn ask(&mut self, query: Query) -> Result<Answer, AskError> {
        let id = self.next_id;
        self.next_id += 1;
        let name = query.name();
        let request = serde_json::to_vec(&Request { id, query })
            .map_err(|e| AskError::Protocol(format!("encoding the query: {e}")))?;
        let reply = self.stream.call(&request)?;
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
        Ok(answer)
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
            result: Ok(Answer::ProjectExists(true)),
        };
        assert_eq!(json(&ok), r#"{"id":7,"result":{"Ok":{"answer":"project_exists","value":true}}}"#);
        let err = Response {
            id: 8,
            result: Err(QueryError::Failed {
                message: "no".into(),
            }),
        };
        assert_eq!(json(&err), r#"{"id":8,"result":{"Err":{"error":"failed","message":"no"}}}"#);
        assert_eq!(serde_json::from_str::<Response>(&json(&ok)).unwrap(), ok);
        assert_eq!(serde_json::from_str::<Request>(&json(&request)).unwrap(), request);
    }

    #[test]
    fn every_query_name_matches_its_answer_and_wire_tag() {
        let p = || "p".to_string();
        let pairs = [
            (Query::Sidebar { project: p() }, Answer::Sidebar(None)),
            (Query::ConfigSkeleton { project: p() }, Answer::ConfigSkeleton(None)),
            (Query::ClosedPage { project: p() }, Answer::ClosedPage(None)),
            (Query::Changelog { project: p() }, Answer::Changelog(ChangelogData::default())),
            (Query::SearchCorpus { project: p() }, Answer::SearchCorpus(SearchCorpus::default())),
            (Query::Machines { project: p() }, Answer::Machines(MachinesModel::default())),
            (Query::ProjectExists { project: p() }, Answer::ProjectExists(false)),
        ];
        for (query, answer) in pairs {
            assert_eq!(query.name(), answer.name());
            assert_eq!(query.project(), "p");
            assert!(json(&query).contains(&format!("\"query\":\"{}\"", query.name())));
            assert!(json(&answer).contains(&format!("\"answer\":\"{}\"", answer.name())));
            assert_eq!(serde_json::from_str::<Answer>(&json(&answer)).unwrap(), answer);
        }
        assert_eq!(Query::OpenBoard { project: p() }.name(), "open_board");
    }

    #[test]
    fn respond_answers_and_reports_unreadable_requests_under_their_id() {
        let answer = |q: Query| match q {
            Query::ProjectExists { project } => Ok(Answer::ProjectExists(project == "here")),
            other => Err(QueryError::Failed {
                message: format!("no {}", other.name()),
            }),
        };
        let reply = |payload: &[u8]| -> Response { serde_json::from_slice(&respond(payload, answer)).unwrap() };

        let ok = reply(br#"{"id":1,"query":{"query":"project_exists","project":"here"}}"#);
        assert_eq!(ok.id, 1);
        assert_eq!(ok.result, Ok(Answer::ProjectExists(true)));

        let failed = reply(br#"{"id":2,"query":{"query":"changelog","project":"x"}}"#);
        assert_eq!(failed.result, Err(QueryError::Failed { message: "no changelog".into() }));

        // A query from a newer client: answered, under its id.
        let newer = reply(br#"{"id":3,"query":{"query":"teleport","project":"x"}}"#);
        assert_eq!(newer.id, 3);
        assert!(matches!(newer.result, Err(QueryError::Unsupported { .. })));

        let junk = reply(b"not json");
        assert_eq!(junk.id, 0);
        assert!(matches!(junk.result, Err(QueryError::Unsupported { .. })));
    }
}
