//! A small, typed client for Linear's GraphQL API, the foundation the Linear
//! issue-tracker backend is built on.
//!
//! ## Shape
//!
//! ```text
//!  LinearClient ── query / mutate / paginate ──► LinearRunner ──► curl ──► api.linear.app
//!     │   typed helpers: viewer, teams, team_states,      (swappable: FakeLinear in tests)
//!     │   team_projects, labels, team_members; issues,
//!     │   create/update_issue, upsert_attachment,
//!     │   issue_comments, create_comment
//!     ├── auth:    LINEAR_API_KEY → macOS keychain → tokens.yml   (SecretToken)
//!     ├── budget:  X-RateLimit-* headers → ~/.shelbi/linear-budget/<key>.json, park on RATELIMITED
//!     ├── log:     one line per request → ~/.shelbi/linear-requests.log (never the key)
//!     └── filters: LinearConnection (team/project/label/assignee refs) → ids, cached in
//!                  ~/.shelbi/projects/<name>/linear.json
//! ```
//!
//! * **No HTTP crate.** Requests go through `curl` (present on every supported
//!   host), via a [`LinearRunner`] closure so tests swap in a fake. The
//!   production runner ([`curl_runner`]) passes the `Authorization` header and
//!   the JSON body to `curl --config -` **on stdin**: the key never appears in
//!   argv (visible in `ps`), in a log line, or in an error.
//! * **Errors** are a typed [`LinearError`]: auth, forbidden, rate limited
//!   (with a park-until time), invalid input, other GraphQL errors, 5xx,
//!   transport (timeout / connect), decode, and filter-resolution misses. It
//!   converts into [`shelbi_core::Error`] with `?` for the `IssueStore` seam.
//! * **Retries.** [`LinearClient::query`] retries transient failures (5xx,
//!   timeouts, connection failures) with a doubling backoff.
//!   [`LinearClient::mutate`] retries only a failure where the request provably
//!   never left the host (a connect failure), because a timed-out or 5xx'd
//!   mutation may already have applied and replaying it could, for example,
//!   create a duplicate issue.
//! * **Rate limits.** Every response's `X-RateLimit-Requests-*` and
//!   `X-RateLimit-Complexity-*` headers are folded into a per-key budget file.
//!   A `RATELIMITED` error (or HTTP 429) parks the key until the reported reset;
//!   while parked, calls fail fast with [`LinearError::RateLimited`] without
//!   touching the network.
//!
//! ## Using it
//!
//! ```no_run
//! use shelbi_state::linear_client::{resolve_filters_cached, LinearClient};
//! # fn demo(conn: &shelbi_core::LinearConnection) -> Result<(), shelbi_state::linear_client::LinearError> {
//! let client = LinearClient::for_project(Some("shelbi"))?.caller("daemon");
//! let filters = resolve_filters_cached(&client, "shelbi", conn)?;
//! let states = client.team_states(&filters.team_id)?;
//! # let _ = states; Ok(()) }
//! ```
//!
//! A new query: write the GraphQL with an operation name, build a
//! [`LinearRequest`], and call [`LinearClient::query`] with a serde type for
//! its `data` (or [`LinearClient::paginate`] for a `nodes` / `pageInfo`
//! connection). Add a fixture under `tests/fixtures/linear/` and register it
//! in the fake (`fake.rs`) so consumers' tests answer it too.
//!
//! ## Testing against it
//!
//! With the `test-support` feature, [`FakeLinear`] answers requests by GraphQL
//! operation name from bundled JSON fixtures (or per-test overrides) and
//! records every call. `FakeLinear::new().install()` routes every
//! [`LinearClient`] built in the process (including ones the CLI or daemon
//! build from project config) to it and supplies a fake API key; dropping
//! the returned guard restores the real transport. Tests that install it must
//! serialize on their crate's env/test lock and mount their own `SHELBI_HOME`,
//! since the budget file and request log are written under it.

mod auth;
mod budget;
mod client;
mod error;
mod filters;
mod issues;
mod queries;
mod transport;

#[cfg(any(test, feature = "test-support"))]
mod fake;

pub use auth::{
    login_with_key, resolve_linear_key, store_linear_key, validate_key_shape, LinearLogin,
    KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE, LINEAR_API_KEY_ENV,
};
pub use budget::{
    read_budget, recent_requests, BucketSnapshot, LinearBudget, LinearRateHeaders,
    RequestLogEntry, BUDGET_DIR, REQUESTS_LOG_FILE,
};
pub use client::{LinearClient, RetryPolicy};
pub use error::{LinearError, LinearResult, TransportFailure};
pub use filters::{
    filter_cache_path, invalidate_filter_cache, resolve_filters, resolve_filters_cached,
    resolve_team, ResolvedLinearFilters, FILTER_CACHE_FILE,
};
pub(crate) use filters::{read_cache_doc, write_cache_doc};
pub use issues::{
    merge_attachment, IssueOrder, IssueQuery, LinearAttachment, LinearComment, LinearIssue,
    LinearRef, SHELBI_ATTACHMENT_PREFIX,
};
pub use queries::{
    Connection, LinearLabel, LinearProject, LinearProjectStatus, LinearState, LinearStateType,
    LinearTeam, LinearUser, PageInfo, PAGE_SIZE,
};
pub use transport::{curl_runner, LinearRequest, LinearResponse, LinearRunner, LINEAR_API_URL};

#[cfg(any(test, feature = "test-support"))]
pub use fake::{
    clear_test_linear_runner, fixture, set_test_linear_runner, test_keychain_value,
    test_runner_installed, FakeLinear, FakeLinearGuard, FakeReply, FAKE_API_KEY,
};

#[cfg(test)]
pub(crate) mod test_home {
    //! A throwaway `SHELBI_HOME` under the crate's shared test lock, for the
    //! tests in this module that write the budget file / request log.
    use std::path::PathBuf;
    use std::sync::MutexGuard;

    pub struct Home {
        pub path: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    pub fn fresh() -> Home {
        let guard = crate::test_lock::LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let path = std::env::temp_dir().join(format!(
            "shelbi-linear-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var("SHELBI_HOME", &path);
        Home { path, _guard: guard }
    }
}
