//! `FakeLinear`: a fixture-backed stand-in for Linear's API, for tests here
//! and in consumer crates (enable `shelbi-state`'s `test-support` feature).
//!
//! ```ignore
//! let fake = FakeLinear::new();          // answers from tests/fixtures/linear/*.json
//! fake.on("TeamStates", r#"{"data":...}"#);   // override one operation (sticky)
//! fake.queue_json("Teams", r#"{"data":...}"#); // a one-shot reply, used first
//! fake.queue("Viewer", FakeReply::Transport(TransportFailure::Timeout("t".into())));
//!
//! // Direct use: a client wired to the fake.
//! let client = fake.client();
//!
//! // Process-wide: every LinearClient built from now on (CLI, daemon, store)
//! // talks to the fake, and key resolution finds FAKE_API_KEY in a fake
//! // keychain (the real env and keychain are ignored). Dropping the guard
//! // restores the real transport.
//! let _guard = fake.install();
//! assert_eq!(fake.calls_for("Viewer").len(), 1);
//! ```
//!
//! Replies are chosen by GraphQL **operation name**: a queued reply for the
//! operation if any (first in, first out), else its sticky reply, else an
//! `errors[]` response with code `FAKE_UNHANDLED` naming the operation, so an
//! unanswered query fails loudly. Every request is recorded, so a test can
//! assert on operations and variables ([`LinearRequest::var`]).
//!
//! The install is process-global (a background thread may build its own
//! client), so tests that install must serialize on their crate's test lock,
//! and must mount their own `SHELBI_HOME` (the budget file and request log are
//! written under it).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use super::client::{LinearClient, RetryPolicy};
use super::error::TransportFailure;
use super::transport::{LinearRequest, LinearResponse, LinearRunner};
use crate::issue_auth::{SecretToken, TokenSource};

/// The API key the fake keychain holds after [`FakeLinear::install`].
pub const FAKE_API_KEY: &str = "lin_api_fake_for_tests";

/// The bundled fixture for an operation name, if there is one.
pub fn fixture(operation: &str) -> Option<&'static str> {
    Some(match operation {
        "Viewer" => include_str!("../../tests/fixtures/linear/viewer.json"),
        "Teams" => include_str!("../../tests/fixtures/linear/teams.json"),
        "TeamStates" => include_str!("../../tests/fixtures/linear/team_states.json"),
        "TeamProjects" => include_str!("../../tests/fixtures/linear/team_projects.json"),
        "Labels" => include_str!("../../tests/fixtures/linear/labels.json"),
        "TeamMembers" => include_str!("../../tests/fixtures/linear/team_members.json"),
        "Issues" => include_str!("../../tests/fixtures/linear/issues.json"),
        "IssueCreate" => include_str!("../../tests/fixtures/linear/issue_create.json"),
        "IssueUpdate" => include_str!("../../tests/fixtures/linear/issue_update.json"),
        "AttachmentCreate" => include_str!("../../tests/fixtures/linear/attachment_create.json"),
        "IssueComments" => include_str!("../../tests/fixtures/linear/issue_comments.json"),
        "CommentCreate" => include_str!("../../tests/fixtures/linear/comment_create.json"),
        "ViewerAccess" => include_str!("../../tests/fixtures/linear/viewer_access.json"),
        _ => return None,
    })
}

const FIXTURE_OPERATIONS: &[&str] = &[
    "Viewer",
    "Teams",
    "TeamStates",
    "TeamProjects",
    "Labels",
    "TeamMembers",
    "Issues",
    "IssueCreate",
    "IssueUpdate",
    "AttachmentCreate",
    "IssueComments",
    "CommentCreate",
    "ViewerAccess",
];

/// One canned reply.
#[derive(Debug, Clone)]
pub enum FakeReply {
    /// A 200 with this JSON body.
    Json(String),
    /// A full response (status, headers, body).
    Response(LinearResponse),
    /// A transport failure (no response).
    Transport(TransportFailure),
}

impl FakeReply {
    fn into_result(self) -> Result<LinearResponse, TransportFailure> {
        match self {
            FakeReply::Json(body) => Ok(LinearResponse::ok(body)),
            FakeReply::Response(r) => Ok(r),
            FakeReply::Transport(t) => Err(t),
        }
    }
}

#[derive(Default)]
struct FakeState {
    sticky: HashMap<String, FakeReply>,
    queued: HashMap<String, VecDeque<FakeReply>>,
    calls: Vec<LinearRequest>,
}

/// A fake Linear API. Cheap to clone; clones share replies and the call log.
#[derive(Clone, Default)]
pub struct FakeLinear {
    state: Arc<Mutex<FakeState>>,
}

impl FakeLinear {
    /// A fake answering every bundled operation from its fixture.
    pub fn new() -> Self {
        let fake = Self::empty();
        for op in FIXTURE_OPERATIONS {
            fake.on(op, fixture(op).expect("bundled fixture"));
        }
        fake
    }

    /// A fake with no replies at all.
    pub fn empty() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answer `operation` with a 200 carrying `json`, every time.
    pub fn on(&self, operation: &str, json: impl Into<String>) -> &Self {
        self.on_reply(operation, FakeReply::Json(json.into()))
    }

    /// Answer `operation` with `reply`, every time.
    pub fn on_reply(&self, operation: &str, reply: FakeReply) -> &Self {
        self.lock().sticky.insert(operation.to_string(), reply);
        self
    }

    /// Answer the next `operation` with a 200 carrying `json`, once.
    pub fn queue_json(&self, operation: &str, json: impl Into<String>) -> &Self {
        self.queue(operation, FakeReply::Json(json.into()))
    }

    /// Answer the next `operation` with `reply`, once.
    pub fn queue(&self, operation: &str, reply: FakeReply) -> &Self {
        self.lock()
            .queued
            .entry(operation.to_string())
            .or_default()
            .push_back(reply);
        self
    }

    /// Every request so far, in order.
    pub fn calls(&self) -> Vec<LinearRequest> {
        self.lock().calls.clone()
    }

    /// The requests for one operation, in order.
    pub fn calls_for(&self, operation: &str) -> Vec<LinearRequest> {
        self.lock()
            .calls
            .iter()
            .filter(|c| c.operation == operation)
            .cloned()
            .collect()
    }

    /// Answer one request (what the runner calls).
    pub fn answer(&self, req: &LinearRequest) -> Result<LinearResponse, TransportFailure> {
        let mut state = self.lock();
        state.calls.push(req.clone());
        let queued = state
            .queued
            .get_mut(&req.operation)
            .and_then(VecDeque::pop_front);
        let reply = queued.or_else(|| state.sticky.get(&req.operation).cloned());
        match reply {
            Some(r) => r.into_result(),
            None => Ok(LinearResponse::ok(
                serde_json::json!({
                    "errors": [{
                        "message": format!("FakeLinear has no reply for operation `{}`", req.operation),
                        "extensions": { "code": "FAKE_UNHANDLED" }
                    }]
                })
                .to_string(),
            )
            .with_status(400)),
        }
    }

    /// A runner backed by this fake.
    pub fn runner(&self) -> LinearRunner {
        let fake = self.clone();
        Arc::new(move |req, _key| fake.answer(req))
    }

    /// A client wired straight to this fake (fake key, retries without delay).
    pub fn client(&self) -> LinearClient {
        LinearClient::with_runner(
            SecretToken::new(FAKE_API_KEY.into(), TokenSource::Keychain),
            self.runner(),
        )
        .retry_policy(RetryPolicy {
            attempts: 3,
            base_delay: Duration::ZERO,
        })
    }

    /// Route every [`LinearClient`] in the process to this fake and put
    /// [`FAKE_API_KEY`] in a fake keychain, until the guard drops.
    pub fn install(&self) -> FakeLinearGuard {
        let fake = self.clone();
        set_test_linear_runner(move |req, _key| fake.answer(req));
        set_fake_keychain(Some(Some(FAKE_API_KEY.to_string())));
        FakeLinearGuard { _private: () }
    }
}

/// Undoes [`FakeLinear::install`] on drop.
pub struct FakeLinearGuard {
    _private: (),
}

impl FakeLinearGuard {
    /// Replace the fake keychain's content (`None` = no key stored), e.g. to
    /// exercise the missing-key path or a fresh `login`.
    pub fn set_keychain(&self, key: Option<&str>) {
        set_fake_keychain(Some(key.map(str::to_string)));
    }
}

impl Drop for FakeLinearGuard {
    fn drop(&mut self) {
        clear_test_linear_runner();
        set_fake_keychain(None);
    }
}

// --- process-wide overrides --------------------------------------------------

static RUNNER_OVERRIDE: RwLock<Option<LinearRunner>> = RwLock::new(None);

/// Install a fake runner process-wide: every [`LinearClient::new`] /
/// [`LinearClient::for_project`] after this routes to `runner`. Pair with
/// [`clear_test_linear_runner`]. [`FakeLinear::install`] is the usual way in.
pub fn set_test_linear_runner<F>(runner: F)
where
    F: Fn(&LinearRequest, &SecretToken) -> Result<LinearResponse, TransportFailure>
        + Send
        + Sync
        + 'static,
{
    *RUNNER_OVERRIDE.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(runner));
}

/// Remove a runner installed by [`set_test_linear_runner`].
pub fn clear_test_linear_runner() {
    *RUNNER_OVERRIDE.write().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Whether a fake runner is installed process-wide.
pub fn test_runner_installed() -> bool {
    test_runner_override().is_some()
}

pub(crate) fn test_runner_override() -> Option<LinearRunner> {
    RUNNER_OVERRIDE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// `None`: the real keychain. `Some(slot)`: a fake keychain holding `slot`.
static FAKE_KEYCHAIN: Mutex<Option<Option<String>>> = Mutex::new(None);

fn set_fake_keychain(value: Option<Option<String>>) {
    *FAKE_KEYCHAIN.lock().unwrap_or_else(PoisonError::into_inner) = value;
}

/// What the fake keychain holds (`None` when empty or not installed), e.g. to
/// assert what `login` stored.
pub fn test_keychain_value() -> Option<String> {
    FAKE_KEYCHAIN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .flatten()
}

/// `Some(content)` while a fake keychain is installed.
pub(crate) fn fake_keychain_read() -> Option<Option<String>> {
    FAKE_KEYCHAIN.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Store into the fake keychain; false when none is installed.
pub(crate) fn fake_keychain_write(key: &str) -> bool {
    let mut slot = FAKE_KEYCHAIN.lock().unwrap_or_else(PoisonError::into_inner);
    match slot.as_mut() {
        Some(inner) => {
            *inner = Some(key.to_string());
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_client::{test_home, LinearError};

    #[test]
    fn queued_replies_come_first_then_the_sticky_one() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        fake.queue("Viewer", FakeReply::Transport(TransportFailure::Timeout("t".into())));
        // The client retries the timeout and lands on the fixture.
        assert_eq!(fake.client().viewer().unwrap().id, "user-ada");
        assert_eq!(fake.calls_for("Viewer").len(), 2);
    }

    #[test]
    fn an_unhandled_operation_fails_loudly() {
        let _home = test_home::fresh();
        let fake = FakeLinear::empty();
        let err = fake.client().viewer().unwrap_err();
        assert!(matches!(&err, LinearError::Graphql { code: Some(c), .. } if c == "FAKE_UNHANDLED"));
        assert!(err.to_string().contains("`Viewer`"), "{err}");
    }

    #[test]
    fn login_stores_only_a_key_linear_accepts() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        let guard = fake.install();
        guard.set_keychain(None);

        fake.queue_json(
            "Viewer",
            r#"{"errors":[{"message":"bad key","extensions":{"code":"AUTHENTICATION_ERROR"}}]}"#,
        );
        let err = crate::linear_client::login_with_key("lin_api_revoked").unwrap_err();
        assert!(matches!(err, LinearError::Unauthenticated { .. }), "{err:?}");
        assert_eq!(test_keychain_value(), None, "a rejected key is not stored");

        let login = crate::linear_client::login_with_key(" lin_api_good \n").unwrap();
        assert_eq!(login.viewer.id, "user-ada");
        assert!(login.stored_in.contains("shelbi-linear"), "{}", login.stored_in);
        assert_eq!(test_keychain_value().as_deref(), Some("lin_api_good"));
    }

    #[test]
    fn install_routes_new_clients_and_supplies_a_keychain_key() {
        let _home = test_home::fresh();
        let fake = FakeLinear::new();
        {
            let guard = fake.install();
            let client = LinearClient::for_project(None).unwrap();
            assert_eq!(client.key_source(), TokenSource::Keychain);
            assert_eq!(client.viewer().unwrap().id, "user-ada");
            assert_eq!(fake.calls().len(), 1);

            crate::linear_client::store_linear_key("lin_api_new").unwrap();
            assert_eq!(test_keychain_value().as_deref(), Some("lin_api_new"));

            guard.set_keychain(None);
            let err = LinearClient::for_project(None).unwrap_err();
            assert!(err.to_string().contains("shelbi tracker linear login"), "{err}");
        }
        assert!(test_runner_override().is_none());
        assert!(fake_keychain_read().is_none());
    }
}
