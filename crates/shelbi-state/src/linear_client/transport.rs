//! The request/response pair and the swappable runner that carries it. The
//! production runner calls Linear's API directly over HTTPS with a blocking
//! [`ureq`] agent (rustls); the API key travels only in the `Authorization`
//! header and never reaches an error, a log line or a `Debug` print.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use ureq::http::HeaderValue;

use super::error::TransportFailure;
use crate::issue_auth::SecretToken;

/// Linear's GraphQL endpoint.
pub const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

/// One GraphQL request. `operation` must match the operation name in `query`
/// (`query Viewer { ... }` → `Viewer`): Linear logs it, the request log records
/// it, and [`super::FakeLinear`] answers by it.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearRequest {
    pub operation: String,
    pub query: String,
    pub variables: Value,
}

impl LinearRequest {
    pub fn new(operation: impl Into<String>, query: impl Into<String>, variables: Value) -> Self {
        LinearRequest {
            operation: operation.into(),
            query: query.into(),
            variables,
        }
    }

    /// The JSON body POSTed to Linear.
    pub fn body(&self) -> String {
        json!({
            "operationName": self.operation,
            "query": self.query,
            "variables": self.variables,
        })
        .to_string()
    }

    /// A variable by name (`Value::Null` when unset), for fakes and asserts.
    pub fn var(&self, name: &str) -> &Value {
        self.variables.get(name).unwrap_or(&Value::Null)
    }
}

/// One HTTP response: status, headers (in arrival order, a repeated name's
/// values together at its first position), and body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinearResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl LinearResponse {
    /// A 200 carrying `body`.
    pub fn ok(body: impl Into<String>) -> Self {
        LinearResponse {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// Builder: replace the status.
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// Builder: append a header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The last value of header `name`, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Carries one request to Linear and returns the raw response, or a
/// [`TransportFailure`] when no response was read. HTTP and GraphQL errors are
/// *responses*, classified by the client, not runner failures.
pub type LinearRunner =
    Arc<dyn Fn(&LinearRequest, &SecretToken) -> Result<LinearResponse, TransportFailure> + Send + Sync>;

/// The production runner: a blocking HTTPS POST to [`LINEAR_API_URL`] over
/// the one shared [`ureq::Agent`] (see [`shared_agent`]).
pub fn http_runner() -> LinearRunner {
    Arc::new(|req, key| run_http(shared_agent(), LINEAR_API_URL, req, key))
}

/// The whole exchange's ceiling: connect, send, and read the full response.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The ceiling on connection setup (DNS, TCP, TLS), within [`REQUEST_TIMEOUT`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest response body read. Linear's complexity limits keep a page
/// far below this; a bigger body is an [`TransportFailure::Other`].
const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// The agent every production request goes through, built on first use, so
/// keep-alive connections (and their TLS sessions) are reused across requests.
fn shared_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| new_agent(REQUEST_TIMEOUT))
}

/// An agent with `timeout` over the whole exchange. An HTTP error status is a
/// response, not an error (the client classifies 401/429/5xx), and a redirect
/// isn't followed: it comes back as the 3xx it is.
fn new_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
        .http_status_as_error(false)
        .max_redirects(0)
        .user_agent(format!("shelbi/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// POST `req` to `url` with `key` in the `Authorization` header, and read the
/// whole response. The key is marked sensitive, so `http`'s `Debug` of the
/// request never prints it, and no error built here quotes a header.
fn run_http(
    agent: &ureq::Agent,
    url: &str,
    req: &LinearRequest,
    key: &SecretToken,
) -> Result<LinearResponse, TransportFailure> {
    let mut auth = HeaderValue::from_str(key.expose()).map_err(|_| {
        TransportFailure::Other("the API key holds characters a header can't carry".into())
    })?;
    auth.set_sensitive(true);

    let mut response = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Authorization", auth)
        .send(req.body())
        .map_err(classify)?;

    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()
        .map_err(classify)?;
    Ok(LinearResponse {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// Sort a ureq error into the three [`TransportFailure`]s. `Connect` must mean
/// the request never reached Linear (the client retries mutations only on it),
/// so only failures that stop an exchange before anything is sent land there:
/// DNS, a refused or unroutable connect, a proxy that won't tunnel, and TLS.
fn classify(err: ureq::Error) -> TransportFailure {
    use std::io::ErrorKind;
    let detail = err.to_string();
    match err {
        ureq::Error::Timeout(_) => TransportFailure::Timeout(detail),
        ureq::Error::Io(e) if e.kind() == ErrorKind::TimedOut => TransportFailure::Timeout(detail),
        ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::ConnectProxyFailed(_)
        | ureq::Error::Tls(_)
        | ureq::Error::Rustls(_) => TransportFailure::Connect(detail),
        ureq::Error::Io(e)
            if matches!(
                e.kind(),
                ErrorKind::ConnectionRefused
                    | ErrorKind::HostUnreachable
                    | ErrorKind::NetworkUnreachable
                    | ErrorKind::NetworkDown
                    | ErrorKind::AddrNotAvailable
            ) =>
        {
            TransportFailure::Connect(detail)
        }
        _ => TransportFailure::Other(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_auth::TokenSource;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;

    #[test]
    fn body_carries_operation_query_and_variables() {
        let req = LinearRequest::new("Viewer", "query Viewer { viewer { id } }", json!({"a": 1}));
        let v: Value = serde_json::from_str(&req.body()).unwrap();
        assert_eq!(v["operationName"], "Viewer");
        assert_eq!(v["variables"]["a"], 1);
        assert_eq!(req.var("a"), &json!(1));
        assert_eq!(req.var("missing"), &Value::Null);
    }

    // ---- the real runner, against a local plain-HTTP server ----

    fn key() -> SecretToken {
        SecretToken::new("lin_api_SECRET".into(), TokenSource::Env("LINEAR_API_KEY"))
    }

    fn viewer() -> LinearRequest {
        LinearRequest::new(
            "Viewer",
            "query Viewer { viewer { id } }",
            json!({"text": "line \"one\"\nback\\slash"}),
        )
    }

    /// One request as the server read it.
    struct Seen {
        request_line: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    fn read_request(reader: &mut BufReader<TcpStream>) -> Seen {
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').unwrap();
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        let mut seen = Seen {
            request_line: request_line.trim_end().to_string(),
            headers,
            body: String::new(),
        };
        let len: usize = seen.header("content-length").map_or(0, |v| v.parse().unwrap());
        let mut body = vec![0; len];
        reader.read_exact(&mut body).unwrap();
        seen.body = String::from_utf8(body).unwrap();
        seen
    }

    /// Serve `responses` in order on ONE accepted connection (a second
    /// connection is never answered), returning what each request carried.
    fn serve(responses: Vec<String>) -> (String, JoinHandle<Vec<Seen>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            responses
                .into_iter()
                .map(|response| {
                    let seen = read_request(&mut reader);
                    writer.write_all(response.as_bytes()).unwrap();
                    writer.flush().unwrap();
                    seen
                })
                .collect()
        });
        (url, handle)
    }

    fn http_response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
        for (k, v) in headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(body);
        out
    }

    #[test]
    fn runner_posts_the_json_with_the_key_and_returns_a_429_as_a_response() {
        let body = r#"{"errors":[{"message":"rate limited","extensions":{"code":"RATELIMITED"}}]}"#;
        let (url, server) = serve(vec![http_response(
            "429 Too Many Requests",
            &[
                ("Content-Type", "application/json"),
                ("Retry-After", "30"),
                ("X-RateLimit-Requests-Remaining", "0"),
            ],
            body,
        )]);
        let req = viewer();
        let resp = run_http(&new_agent(Duration::from_secs(10)), &url, &req, &key())
            .expect("a 429 is a response, not a transport failure");

        assert_eq!(resp.status, 429);
        assert_eq!(resp.header("retry-after"), Some("30"));
        assert_eq!(resp.header("x-ratelimit-requests-remaining"), Some("0"));
        assert_eq!(resp.body, body);
        let names: Vec<&str> = resp.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            ["content-length", "content-type", "retry-after", "x-ratelimit-requests-remaining"],
            "every header, in arrival order"
        );

        let seen = server.join().unwrap().pop().unwrap();
        assert_eq!(seen.request_line, "POST /graphql HTTP/1.1");
        assert_eq!(seen.header("authorization"), Some("lin_api_SECRET"));
        assert_eq!(seen.header("content-type"), Some("application/json"));
        assert_eq!(
            seen.header("user-agent"),
            Some(concat!("shelbi/", env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(seen.body, req.body(), "the JSON body arrives byte for byte");
        let parsed: Value = serde_json::from_str(&seen.body).unwrap();
        assert_eq!(parsed["variables"]["text"], "line \"one\"\nback\\slash");
    }

    #[test]
    fn one_agent_reuses_its_connection_across_requests() {
        // The server answers two requests on one connection and never accepts a
        // second: the second request only succeeds over the kept-alive socket.
        let (url, server) = serve(vec![
            http_response("200 OK", &[], r#"{"data":{"n":1}}"#),
            http_response("500 Internal Server Error", &[], "oops"),
        ]);
        let agent = new_agent(Duration::from_secs(5));
        let first = run_http(&agent, &url, &viewer(), &key()).unwrap();
        let second = run_http(&agent, &url, &viewer(), &key()).unwrap();
        assert_eq!((first.status, first.body.as_str()), (200, r#"{"data":{"n":1}}"#));
        assert_eq!((second.status, second.body.as_str()), (500, "oops"));
        assert_eq!(server.join().unwrap().len(), 2);
    }

    #[test]
    fn a_server_that_never_answers_is_a_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/graphql", listener.local_addr().unwrap());
        // Accept and read, then hold the socket open without a reply.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            read_request(&mut reader);
            let _ = done_rx.recv();
            drop(stream);
        });
        let started = std::time::Instant::now();
        let result = run_http(&new_agent(Duration::from_millis(300)), &url, &viewer(), &key());
        let _ = done_tx.send(());
        server.join().unwrap();
        match result {
            Err(failure @ TransportFailure::Timeout(_)) => {
                assert!(!format!("{failure} {failure:?}").contains("SECRET"), "{failure:?}");
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    #[test]
    fn a_refused_connection_is_a_connect_failure_that_never_leaks_the_key() {
        // A port that was just free: nothing listens there now.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/graphql");
        match run_http(&new_agent(Duration::from_secs(5)), &url, &viewer(), &key()) {
            Err(failure @ TransportFailure::Connect(_)) => {
                let shown = format!("{failure} {failure:?}");
                assert!(!shown.contains("SECRET"), "{shown}");
            }
            other => panic!("expected a connect failure, got {other:?}"),
        }
    }

    #[test]
    fn errors_sort_into_connect_timeout_and_other() {
        use std::io::{Error as IoError, ErrorKind};
        let kind = |e: ureq::Error| match classify(e) {
            TransportFailure::Connect(_) => "connect",
            TransportFailure::Timeout(_) => "timeout",
            TransportFailure::Other(_) => "other",
        };
        // Never reached Linear: safe to retry a mutation.
        assert_eq!(kind(ureq::Error::HostNotFound), "connect");
        assert_eq!(kind(ureq::Error::ConnectionFailed), "connect");
        assert_eq!(kind(ureq::Error::Tls("handshake failed")), "connect");
        assert_eq!(kind(ureq::Error::ConnectProxyFailed("refused".into())), "connect");
        for k in [ErrorKind::ConnectionRefused, ErrorKind::NetworkUnreachable, ErrorKind::HostUnreachable] {
            assert_eq!(kind(ureq::Error::Io(IoError::from(k))), "connect", "{k:?}");
        }
        // May have reached Linear.
        assert_eq!(kind(ureq::Error::Timeout(ureq::Timeout::Global)), "timeout");
        assert_eq!(kind(ureq::Error::Io(IoError::from(ErrorKind::TimedOut))), "timeout");
        assert_eq!(kind(ureq::Error::Io(IoError::from(ErrorKind::ConnectionReset))), "other");
        assert_eq!(kind(ureq::Error::Io(IoError::from(ErrorKind::UnexpectedEof))), "other");
        assert_eq!(kind(ureq::Error::BodyExceedsLimit(1)), "other");
    }

    #[test]
    fn a_key_a_header_cannot_carry_fails_without_quoting_it() {
        let bad = SecretToken::new("lin_api_SE\nCRET".into(), TokenSource::Env("LINEAR_API_KEY"));
        // Never sent: the URL doesn't matter.
        match run_http(&new_agent(Duration::from_secs(1)), "http://127.0.0.1:9/", &viewer(), &bad) {
            Err(failure @ TransportFailure::Other(_)) => {
                assert!(!format!("{failure} {failure:?}").contains("CRET"), "{failure:?}");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }
}
