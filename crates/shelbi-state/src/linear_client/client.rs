//! [`LinearClient`]: one API key, one runner, retries, budget and logging.

use std::time::{Duration, Instant};

use chrono::Utc;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::budget::{self, LinearBudget, LinearRateHeaders};
use super::error::{classify, LinearError, LinearResult, TransportFailure};
use super::transport::{curl_runner, LinearRequest, LinearRunner};
use crate::issue_auth::{SecretToken, TokenSource};

/// How a call retries transient failures: up to `attempts` tries in all,
/// sleeping `base_delay`, then twice that, and so on, between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub base_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            attempts: 3,
            base_delay: Duration::from_millis(500),
        }
    }
}

impl RetryPolicy {
    /// One try, no retries.
    pub fn none() -> Self {
        RetryPolicy {
            attempts: 1,
            base_delay: Duration::ZERO,
        }
    }

    fn delay(&self, retry: u32) -> Duration {
        self.base_delay.saturating_mul(1u32 << retry.min(10))
    }
}

/// Whether a failed attempt may be replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replay {
    /// A read: replay any transient failure.
    Idempotent,
    /// A write: replay only when the request never left the host.
    WriteOnce,
}

/// A Linear API client bound to one API key.
///
/// Cheap to clone. Build one with [`LinearClient::for_project`] (resolves the
/// key the standard way) or [`LinearClient::new`] (a key in hand, e.g. during
/// `login`). Both pick up the test runner from
/// [`super::set_test_linear_runner`] / [`super::FakeLinear::install`] when one
/// is installed.
#[derive(Clone)]
pub struct LinearClient {
    key: SecretToken,
    key_hash: String,
    runner: LinearRunner,
    caller: String,
    retry: RetryPolicy,
}

impl std::fmt::Debug for LinearClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearClient")
            .field("key", &self.key)
            .field("caller", &self.caller)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl LinearClient {
    /// A client for `key` over the production `curl` runner (or the installed
    /// test runner).
    pub fn new(key: SecretToken) -> Self {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(runner) = super::fake::test_runner_override() {
            return Self::with_runner(key, runner).retry_policy(RetryPolicy {
                attempts: 3,
                base_delay: Duration::ZERO,
            });
        }
        Self::with_runner(key, curl_runner())
    }

    /// A client whose key is resolved for `project` (env → keychain →
    /// `tokens.yml`; see [`super::resolve_linear_key`]). `None` skips the
    /// project's `tokens.yml`.
    pub fn for_project(project: Option<&str>) -> LinearResult<Self> {
        Ok(Self::new(super::resolve_linear_key(project)?))
    }

    /// A client over an explicit runner.
    pub fn with_runner(key: SecretToken, runner: LinearRunner) -> Self {
        let key_hash = crate::gh_budget::token_key(key.expose());
        LinearClient {
            key,
            key_hash,
            runner,
            caller: "shelbi".into(),
            retry: RetryPolicy::default(),
        }
    }

    /// Builder: the caller name written to the request log (`cli`, `daemon`,
    /// `tui`, ...).
    pub fn caller(mut self, caller: impl Into<String>) -> Self {
        self.caller = caller.into();
        self
    }

    /// Builder: replace the retry policy.
    pub fn retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Where the key came from (env, keychain, tokens.yml).
    pub fn key_source(&self) -> TokenSource {
        self.key.source()
    }

    /// The stable, non-secret hash naming this key's budget file.
    pub fn budget_key(&self) -> &str {
        &self.key_hash
    }

    /// This key's latest rate-limit snapshot.
    pub fn budget(&self) -> LinearBudget {
        budget::read_budget(&self.key_hash)
    }

    /// Run a read and deserialize its `data` into `T`. Retries transient
    /// failures per the [`RetryPolicy`].
    pub fn query<T: DeserializeOwned>(&self, req: &LinearRequest) -> LinearResult<T> {
        decode(self.execute(req, Replay::Idempotent)?)
    }

    /// Run a mutation and deserialize its `data` into `T`. Retries only a
    /// connection failure (the request never left the host); a timeout or 5xx
    /// is returned as-is, since the write may have applied.
    pub fn mutate<T: DeserializeOwned>(&self, req: &LinearRequest) -> LinearResult<T> {
        decode(self.execute(req, Replay::WriteOnce)?)
    }

    /// Run a read and return its raw `data`.
    pub fn query_value(&self, req: &LinearRequest) -> LinearResult<Value> {
        self.execute(req, Replay::Idempotent)
    }

    fn execute(&self, req: &LinearRequest, replay: Replay) -> LinearResult<Value> {
        let attempts = self.retry.attempts.max(1);
        let mut attempt = 0;
        loop {
            let result = self.attempt(req);
            attempt += 1;
            let err = match result {
                Ok(data) => return Ok(data),
                Err(err) => err,
            };
            let replayable = match replay {
                Replay::Idempotent => err.is_transient(),
                Replay::WriteOnce => {
                    matches!(err, LinearError::Transport(TransportFailure::Connect(_)))
                }
            };
            if !replayable || attempt >= attempts {
                return Err(err);
            }
            let delay = self.retry.delay(attempt - 1);
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
    }

    /// One try: park check, send, record headers, classify, log.
    fn attempt(&self, req: &LinearRequest) -> LinearResult<Value> {
        let now = Utc::now();
        let parked = self.budget().parked_at(now);
        if let Some(until) = parked {
            budget::log_request(&req.operation, "err:parked", 0, &self.caller, now);
            return Err(LinearError::RateLimited {
                until,
                detail: "still paused from an earlier rate limit".into(),
            });
        }

        let started = Instant::now();
        let response = (self.runner)(req, &self.key);
        let millis = started.elapsed().as_millis();
        let response = match response {
            Ok(r) => r,
            Err(failure) => {
                budget::log_request(
                    &req.operation,
                    &format!("err:{}", failure.class()),
                    millis,
                    &self.caller,
                    now,
                );
                return Err(LinearError::Transport(failure));
            }
        };

        let headers = LinearRateHeaders::parse(&response);
        budget::record_headers(&self.key_hash, &headers, now);
        let result = classify(&response, headers.resume_at(), now);
        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(e) => format!("err:{}", e.log_class()),
        };
        budget::log_request(&req.operation, &outcome, millis, &self.caller, now);
        match result {
            Err(LinearError::RateLimited { until, detail }) => {
                let until = budget::park(&self.key_hash, until, now);
                tracing::warn!(op = %req.operation, %until, "linear rate limited; parked");
                Err(LinearError::RateLimited { until, detail })
            }
            Ok(data) => {
                // A park that has run out is cleared on the first success.
                budget::clear_park(&self.key_hash);
                Ok(data)
            }
            other => other,
        }
    }
}

fn decode<T: DeserializeOwned>(data: Value) -> LinearResult<T> {
    serde_json::from_value(data).map_err(|e| LinearError::Decode {
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_client::test_home;
    use crate::linear_client::LinearResponse;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    type Script = Arc<Mutex<Vec<Result<LinearResponse, TransportFailure>>>>;

    /// A client whose runner pops scripted replies front-first and counts calls.
    fn scripted(replies: Vec<Result<LinearResponse, TransportFailure>>) -> (LinearClient, Script) {
        let script: Script = Arc::new(Mutex::new(replies));
        let s = script.clone();
        let runner: LinearRunner = Arc::new(move |_, _| {
            let mut s = s.lock().unwrap();
            assert!(!s.is_empty(), "runner called more times than scripted");
            s.remove(0)
        });
        let key = SecretToken::new("lin_api_test".into(), TokenSource::Env("LINEAR_API_KEY"));
        let client = LinearClient::with_runner(key, runner)
            .caller("test")
            .retry_policy(RetryPolicy { attempts: 3, base_delay: Duration::ZERO });
        (client, script)
    }

    fn req() -> LinearRequest {
        LinearRequest::new("Viewer", "query Viewer { viewer { id } }", json!({}))
    }

    #[derive(serde::Deserialize, Debug)]
    struct V {
        viewer: Value,
    }

    #[test]
    fn query_retries_transient_failures_then_succeeds() {
        let _home = test_home::fresh();
        let (client, script) = scripted(vec![
            Err(TransportFailure::Timeout("t".into())),
            Ok(LinearResponse::ok("bad gateway").with_status(502)),
            Ok(LinearResponse::ok(r#"{"data":{"viewer":{"id":"u1"}}}"#)),
        ]);
        let v: V = client.query(&req()).unwrap();
        assert_eq!(v.viewer["id"], "u1");
        assert!(script.lock().unwrap().is_empty());
        let log = budget::recent_requests(Duration::from_secs(60), Utc::now());
        let outcomes: Vec<_> = log.iter().map(|e| e.outcome.as_str()).collect();
        assert_eq!(outcomes, ["err:timeout", "err:http502", "ok"]);
    }

    #[test]
    fn query_gives_up_after_the_policy_and_does_not_retry_permanent_errors() {
        let _home = test_home::fresh();
        let (client, _) = scripted(vec![
            Err(TransportFailure::Connect("c".into())),
            Err(TransportFailure::Connect("c".into())),
            Err(TransportFailure::Connect("c".into())),
        ]);
        let err = client.query::<V>(&req()).unwrap_err();
        assert!(matches!(err, LinearError::Transport(TransportFailure::Connect(_))));

        let (client, script) = scripted(vec![
            Ok(LinearResponse::ok(
                r#"{"errors":[{"message":"bad","extensions":{"code":"INVALID_INPUT"}}]}"#,
            )
            .with_status(400)),
            Ok(LinearResponse::ok("never reached")),
        ]);
        assert!(matches!(client.query::<V>(&req()), Err(LinearError::InvalidInput { .. })));
        assert_eq!(script.lock().unwrap().len(), 1, "no retry on invalid input");
    }

    #[test]
    fn mutate_replays_only_a_connect_failure() {
        let _home = test_home::fresh();
        let (client, _) = scripted(vec![
            Err(TransportFailure::Connect("c".into())),
            Ok(LinearResponse::ok(r#"{"data":{"viewer":{"id":"u1"}}}"#)),
        ]);
        assert!(client.mutate::<V>(&req()).is_ok());

        let (client, script) = scripted(vec![
            Err(TransportFailure::Timeout("t".into())),
            Ok(LinearResponse::ok("never reached")),
        ]);
        assert!(matches!(client.mutate::<V>(&req()), Err(LinearError::Transport(TransportFailure::Timeout(_)))));
        assert_eq!(script.lock().unwrap().len(), 1, "a timed-out write is not replayed");
    }

    #[test]
    fn rate_limit_parks_the_key_and_later_calls_fail_fast() {
        let _home = test_home::fresh();
        let reset_ms = (Utc::now().timestamp() + 600) * 1000;
        let (client, script) = scripted(vec![
            Ok(LinearResponse::ok(
                r#"{"errors":[{"message":"Rate limit exceeded","extensions":{"code":"RATELIMITED"}}]}"#,
            )
            .with_status(400)
            .with_header("X-RateLimit-Requests-Remaining", "0")
            .with_header("X-RateLimit-Requests-Reset", reset_ms.to_string())),
            Ok(LinearResponse::ok("never reached")),
        ]);
        let err = client.query::<V>(&req()).unwrap_err();
        let until = err.parked_until().expect("rate limited");
        assert_eq!(until.timestamp(), reset_ms / 1000);
        assert_eq!(client.budget().parked_until, Some(reset_ms / 1000));
        assert_eq!(client.budget().requests.remaining, Some(0));

        // Parked: no network call.
        let err = client.query::<V>(&req()).unwrap_err();
        assert_eq!(err.parked_until(), Some(until));
        assert_eq!(script.lock().unwrap().len(), 1);
    }

    #[test]
    fn headers_land_in_the_budget_and_the_key_never_reaches_disk() {
        let home = test_home::fresh();
        let (client, _) = scripted(vec![Ok(LinearResponse::ok(r#"{"data":{"viewer":{"id":"u1"}}}"#)
            .with_header("X-RateLimit-Requests-Remaining", "1400")
            .with_header("X-RateLimit-Requests-Limit", "1500"))]);
        client.query::<V>(&req()).unwrap();
        assert_eq!(client.budget().requests.remaining, Some(1400));
        assert!(!format!("{client:?}").contains("lin_api_test"));
        for entry in walk(&home.path) {
            let text = std::fs::read_to_string(&entry).unwrap_or_default();
            assert!(!text.contains("lin_api_test"), "key leaked into {}", entry.display());
            assert!(!entry.display().to_string().contains("lin_api_test"));
        }
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}
