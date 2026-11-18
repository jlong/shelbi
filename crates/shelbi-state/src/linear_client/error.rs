//! Typed failures of a Linear API call, and how a response is classified.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use super::transport::LinearResponse;

/// The result of a Linear client call.
pub type LinearResult<T> = std::result::Result<T, LinearError>;

/// A transport-level failure: `curl` could not complete the exchange, so no
/// HTTP response was read. Returned by a [`super::LinearRunner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportFailure {
    /// The request timed out (`curl` exit 28). It may have reached Linear.
    Timeout(String),
    /// The connection could not be made (DNS, refused, TLS handshake): the
    /// request never reached Linear, so even a mutation is safe to retry.
    Connect(String),
    /// Anything else (curl missing, a broken pipe, an unreadable reply).
    Other(String),
}

impl TransportFailure {
    /// The short class written to the request log (`timeout` / `conn` / `other`).
    pub fn class(&self) -> &'static str {
        match self {
            TransportFailure::Timeout(_) => "timeout",
            TransportFailure::Connect(_) => "conn",
            TransportFailure::Other(_) => "other",
        }
    }

    fn detail(&self) -> &str {
        match self {
            TransportFailure::Timeout(d) | TransportFailure::Connect(d) | TransportFailure::Other(d) => d,
        }
    }
}

impl std::fmt::Display for TransportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportFailure::Timeout(_) => write!(f, "timed out ({})", self.detail()),
            TransportFailure::Connect(_) => write!(f, "could not connect ({})", self.detail()),
            TransportFailure::Other(_) => f.write_str(self.detail()),
        }
    }
}

/// Why a Linear call failed. Every message is safe to show a user: none
/// carries the API key.
#[derive(Debug, thiserror::Error)]
pub enum LinearError {
    /// A local failure before or around the call: no API key configured,
    /// unreadable `tokens.yml`, an IO error writing the filter cache.
    #[error(transparent)]
    Local(#[from] shelbi_core::Error),

    /// Linear rejected the API key (`AUTHENTICATION_ERROR` / HTTP 401).
    #[error(
        "Linear rejected the API key ({detail}); create a new personal API key and run \
         `shelbi tracker linear login`"
    )]
    Unauthenticated { detail: String },

    /// The key is valid but may not do this (`FORBIDDEN` / HTTP 403).
    #[error("Linear refused the request: {detail}")]
    Forbidden { detail: String },

    /// Linear's rate limit is exhausted (`RATELIMITED` / HTTP 429), or the key
    /// is still parked from an earlier one. Calls fail fast until `until`.
    #[error("Linear rate limit reached; requests are paused until {} ({detail})", until.format("%H:%M:%S UTC"))]
    RateLimited { until: DateTime<Utc>, detail: String },

    /// Linear rejected the request's input (`INVALID_INPUT`, a GraphQL
    /// validation failure, ...). Not retryable: the request itself is wrong.
    #[error("Linear rejected the request as invalid: {detail}")]
    InvalidInput { detail: String },

    /// Any other GraphQL error, with its `extensions.code` when present.
    #[error("Linear returned an error{}: {detail}", code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Graphql { code: Option<String>, detail: String },

    /// Linear answered with a 5xx. Transient: queries retry it.
    #[error("Linear server error (HTTP {status}): {detail}")]
    Server { status: u16, detail: String },

    /// `curl` could not complete the exchange.
    #[error("could not reach Linear: {0}")]
    Transport(TransportFailure),

    /// The response was not the JSON shape expected.
    #[error("unexpected response from Linear: {detail}")]
    Decode { detail: String },

    /// A filter reference (`team: ENG`, `label: shelbi`, ...) matched nothing,
    /// or matched more than one object. `available` lists the candidates.
    #[error("{}", resolve_message(what, wanted, available, *ambiguous))]
    Resolve {
        /// What was looked up: `team`, `project`, `label`, `assignee`.
        what: &'static str,
        /// The reference as written in the config.
        wanted: String,
        /// Human labels of the candidates (or of the matches, when ambiguous).
        available: Vec<String>,
        /// True when several objects matched rather than none.
        ambiguous: bool,
    },
}

fn resolve_message(what: &str, wanted: &str, available: &[String], ambiguous: bool) -> String {
    const SHOWN: usize = 15;
    let mut list = available.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if available.len() > SHOWN {
        list.push_str(&format!(", ... ({} more)", available.len() - SHOWN));
    }
    if ambiguous {
        format!("Linear {what} `{wanted}` is ambiguous: it matches {list}; use the id instead")
    } else if available.is_empty() {
        format!("no Linear {what} `{wanted}` (none are visible to this API key)")
    } else {
        format!("no Linear {what} `{wanted}`; available: {list}")
    }
}

impl LinearError {
    /// True for a failure worth retrying as-is: a 5xx, or a transport timeout
    /// / connection failure. (A mutation retries only [`TransportFailure::Connect`];
    /// see [`super::LinearClient::mutate`].)
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            LinearError::Server { .. }
                | LinearError::Transport(TransportFailure::Timeout(_))
                | LinearError::Transport(TransportFailure::Connect(_))
        )
    }

    /// When rate limited, the time calls resume.
    pub fn parked_until(&self) -> Option<DateTime<Utc>> {
        match self {
            LinearError::RateLimited { until, .. } => Some(*until),
            _ => None,
        }
    }

    /// The short class written to the request log for this failure.
    pub fn log_class(&self) -> String {
        match self {
            LinearError::Local(_) => "local".into(),
            LinearError::Unauthenticated { .. } => "auth".into(),
            LinearError::Forbidden { .. } => "forbidden".into(),
            LinearError::RateLimited { .. } => "ratelimited".into(),
            LinearError::InvalidInput { .. } => "invalid".into(),
            LinearError::Graphql { .. } => "graphql".into(),
            LinearError::Server { status, .. } => format!("http{status}"),
            LinearError::Transport(t) => t.class().into(),
            LinearError::Decode { .. } => "decode".into(),
            LinearError::Resolve { .. } => "resolve".into(),
        }
    }
}

impl From<LinearError> for shelbi_core::Error {
    /// Flatten into the workspace error for the `IssueStore` seam. A local
    /// error passes through unchanged (so a missing key stays a typed
    /// [`shelbi_core::Error::MissingIssueTrackerAuth`]); the rest become
    /// [`shelbi_core::Error::Other`] carrying the user-facing message.
    fn from(err: LinearError) -> Self {
        match err {
            LinearError::Local(e) => e,
            other => shelbi_core::Error::Other(other.to_string()),
        }
    }
}

/// When a rate-limited key may call again, absent any reset header: one minute.
pub(crate) const DEFAULT_PARK_SECS: i64 = 60;

/// Classify one HTTP response into its `data` payload or a typed error.
///
/// GraphQL `errors[]` win over the HTTP status (Linear reports auth failures
/// and rate limits as HTTP 400 with an `extensions.code`), so the first
/// error's code decides; a response with no parseable `errors` falls back to
/// the status. `rate_reset` is the reset time read from the response headers,
/// used as the park-until for a rate-limit error.
pub(crate) fn classify(
    resp: &LinearResponse,
    rate_reset: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> LinearResult<Value> {
    let park_until = || {
        rate_reset
            .filter(|t| *t > now)
            .unwrap_or_else(|| now + chrono::Duration::seconds(DEFAULT_PARK_SECS))
    };
    let parsed: Option<Value> = serde_json::from_str(&resp.body).ok();

    if let Some(first) = parsed
        .as_ref()
        .and_then(|v| v.get("errors"))
        .and_then(Value::as_array)
        .and_then(|errs| errs.first())
    {
        let code = first
            .pointer("/extensions/code")
            .and_then(Value::as_str)
            .map(str::to_ascii_uppercase);
        let detail = error_detail(first);
        return Err(match code.as_deref() {
            Some("AUTHENTICATION_ERROR") | Some("UNAUTHENTICATED") => {
                LinearError::Unauthenticated { detail }
            }
            Some("FORBIDDEN") => LinearError::Forbidden { detail },
            Some("RATELIMITED") => LinearError::RateLimited {
                until: park_until(),
                detail,
            },
            Some("INVALID_INPUT")
            | Some("BAD_USER_INPUT")
            | Some("GRAPHQL_VALIDATION_FAILED")
            | Some("GRAPHQL_PARSE_FAILED") => LinearError::InvalidInput { detail },
            _ if resp.status >= 500 => LinearError::Server {
                status: resp.status,
                detail,
            },
            _ if resp.status == 429 => LinearError::RateLimited {
                until: park_until(),
                detail,
            },
            _ => LinearError::Graphql { code, detail },
        });
    }

    let snippet = || body_snippet(&resp.body);
    match resp.status {
        200..=299 => {}
        401 => return Err(LinearError::Unauthenticated { detail: format!("HTTP 401: {}", snippet()) }),
        403 => return Err(LinearError::Forbidden { detail: format!("HTTP 403: {}", snippet()) }),
        429 => {
            return Err(LinearError::RateLimited {
                until: park_until(),
                detail: "HTTP 429".into(),
            })
        }
        s if s >= 500 => return Err(LinearError::Server { status: s, detail: snippet() }),
        s => {
            return Err(LinearError::Graphql {
                code: None,
                detail: format!("HTTP {s}: {}", snippet()),
            })
        }
    }

    match parsed {
        Some(mut v) => match v.get_mut("data").map(Value::take) {
            Some(data) if !data.is_null() => Ok(data),
            _ => Err(LinearError::Decode {
                detail: "the response has no `data`".into(),
            }),
        },
        None => Err(LinearError::Decode {
            detail: format!("not JSON: {}", snippet()),
        }),
    }
}

/// `message`, plus the user-presentable message when Linear sent a distinct one.
fn error_detail(err: &Value) -> String {
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no message")
        .to_string();
    match err
        .pointer("/extensions/userPresentableMessage")
        .and_then(Value::as_str)
    {
        Some(user) if user != message => format!("{message}: {user}"),
        _ => message,
    }
}

/// A short, single-line excerpt of a response body for an error message.
fn body_snippet(body: &str) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "empty body".into();
    }
    match flat.char_indices().nth(160) {
        Some((i, _)) => format!("{}...", &flat[..i]),
        None => flat,
    }
}

/// Epoch seconds → UTC time (`None` when out of range).
pub(crate) fn from_epoch(secs: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(secs, 0).single()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        from_epoch(1_800_000_000).unwrap()
    }

    fn resp(status: u16, body: &str) -> LinearResponse {
        LinearResponse::ok(body).with_status(status)
    }

    fn gql_error(code: &str) -> String {
        format!(r#"{{"errors":[{{"message":"m","extensions":{{"code":"{code}","userPresentableMessage":"say it"}}}}]}}"#)
    }

    #[test]
    fn data_is_returned_on_success() {
        let v = classify(&resp(200, r#"{"data":{"viewer":{"id":"u"}}}"#), None, now()).unwrap();
        assert_eq!(v["viewer"]["id"], "u");
    }

    #[test]
    fn graphql_codes_classify_regardless_of_status() {
        let cases = [
            ("AUTHENTICATION_ERROR", "Unauthenticated"),
            ("FORBIDDEN", "Forbidden"),
            ("RATELIMITED", "RateLimited"),
            ("INVALID_INPUT", "InvalidInput"),
            ("SOMETHING_NEW", "Graphql"),
        ];
        for (code, want) in cases {
            let err = classify(&resp(400, &gql_error(code)), None, now()).unwrap_err();
            assert!(format!("{err:?}").starts_with(want), "{code}: {err:?}");
            // The user-presentable message rides along.
            assert!(err.to_string().contains("say it"), "{err}");
        }
    }

    #[test]
    fn rate_limit_parks_until_the_header_reset_or_a_minute() {
        let reset = now() + chrono::Duration::seconds(600);
        let err = classify(&resp(400, &gql_error("RATELIMITED")), Some(reset), now()).unwrap_err();
        assert_eq!(err.parked_until(), Some(reset));

        let err = classify(&resp(429, ""), None, now()).unwrap_err();
        assert_eq!(err.parked_until(), Some(now() + chrono::Duration::seconds(DEFAULT_PARK_SECS)));
        // A reset already in the past is no help.
        let err = classify(&resp(429, ""), Some(now()), now()).unwrap_err();
        assert_eq!(err.parked_until(), Some(now() + chrono::Duration::seconds(DEFAULT_PARK_SECS)));
    }

    #[test]
    fn http_statuses_classify_without_a_graphql_error() {
        assert!(matches!(classify(&resp(401, "nope"), None, now()), Err(LinearError::Unauthenticated { .. })));
        assert!(matches!(classify(&resp(403, ""), None, now()), Err(LinearError::Forbidden { .. })));
        let err = classify(&resp(502, "<html>bad gateway</html>"), None, now()).unwrap_err();
        assert!(matches!(err, LinearError::Server { status: 502, .. }));
        assert!(err.is_transient());
        assert!(matches!(classify(&resp(200, "not json"), None, now()), Err(LinearError::Decode { .. })));
        assert!(matches!(classify(&resp(200, r#"{"data":null}"#), None, now()), Err(LinearError::Decode { .. })));
    }

    #[test]
    fn transience_and_log_classes() {
        assert!(LinearError::Transport(TransportFailure::Timeout("t".into())).is_transient());
        assert!(LinearError::Transport(TransportFailure::Connect("c".into())).is_transient());
        assert!(!LinearError::Transport(TransportFailure::Other("o".into())).is_transient());
        assert!(!LinearError::InvalidInput { detail: String::new() }.is_transient());
        assert_eq!(LinearError::Server { status: 503, detail: String::new() }.log_class(), "http503");
    }

    #[test]
    fn resolve_errors_list_candidates() {
        let err = LinearError::Resolve {
            what: "team",
            wanted: "ENG".into(),
            available: vec!["DES (Design)".into(), "OPS (Ops)".into()],
            ambiguous: false,
        };
        assert_eq!(err.to_string(), "no Linear team `ENG`; available: DES (Design), OPS (Ops)");
    }

    #[test]
    fn local_errors_pass_through_to_the_core_error() {
        let core: shelbi_core::Error = LinearError::Local(shelbi_core::Error::Other("x".into())).into();
        assert!(matches!(core, shelbi_core::Error::Other(m) if m == "x"));
        let core: shelbi_core::Error = LinearError::Forbidden { detail: "d".into() }.into();
        assert!(core.to_string().contains("Linear refused"));
    }
}
