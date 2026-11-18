//! The request/response pair and the swappable runner that carries it. The
//! production runner shells out to `curl`, feeding it a config on stdin so the
//! API key and body never touch argv.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde_json::{json, Value};

use super::error::TransportFailure;
use crate::issue_auth::SecretToken;

/// Linear's GraphQL endpoint.
pub const LINEAR_API_URL: &str = "https://api.linear.app/graphql";

/// Per-request ceiling handed to `curl --max-time`, in seconds.
const CURL_MAX_TIME_SECS: u32 = 30;

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

/// One HTTP response: status, headers (in arrival order), and body.
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

/// The production runner: `curl -sS --max-time 30 -D <tmp> --config -`, with
/// the URL, headers (including `Authorization`) and body in the stdin config.
pub fn curl_runner() -> LinearRunner {
    Arc::new(|req, key| run_curl(LINEAR_API_URL, req, key))
}

/// Quote a value for a curl config file: inside double quotes curl unescapes
/// `\\`, `\"`, `\n`, `\t`, `\r`, `\v`, so backslashes and quotes are escaped
/// (a JSON body's own `\n` escapes must reach Linear as the two characters).
fn config_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The curl config fed on stdin. Holds the secret: never logged.
fn curl_config(url: &str, req: &LinearRequest, key: &SecretToken) -> String {
    format!(
        "url = {}\nrequest = \"POST\"\nheader = \"Content-Type: application/json\"\n\
         header = {}\ndata-binary = {}\n",
        config_quote(url),
        config_quote(&format!("Authorization: {}", key.expose())),
        config_quote(&req.body()),
    )
}

fn header_file_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "shelbi-linear-hdr-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

fn run_curl(
    url: &str,
    req: &LinearRequest,
    key: &SecretToken,
) -> Result<LinearResponse, TransportFailure> {
    let header_path = header_file_path();
    let result = run_curl_with_header_file(url, req, key, &header_path);
    let _ = std::fs::remove_file(&header_path);
    result
}

fn run_curl_with_header_file(
    url: &str,
    req: &LinearRequest,
    key: &SecretToken,
    header_path: &std::path::Path,
) -> Result<LinearResponse, TransportFailure> {
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", &CURL_MAX_TIME_SECS.to_string(), "-D"])
        .arg(header_path)
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| TransportFailure::Other(format!("could not run curl: {e}")))?;

    // Write the config from a thread so a large body can't deadlock against
    // curl filling its stdout pipe.
    let config = curl_config(url, req, key);
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(config.as_bytes());
    });
    let output = child
        .wait_with_output()
        .map_err(|e| TransportFailure::Other(format!("curl did not finish: {e}")))?;
    let _ = writer.join();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let code = output.status.code().unwrap_or(-1);
        let detail = if stderr.is_empty() {
            format!("curl exit {code}")
        } else {
            format!("curl exit {code}: {stderr}")
        };
        return Err(match code {
            28 => TransportFailure::Timeout(detail),
            5 | 6 | 7 | 35 | 60 => TransportFailure::Connect(detail),
            _ => TransportFailure::Other(detail),
        });
    }

    let raw_headers = std::fs::read_to_string(header_path).unwrap_or_default();
    let (status, headers) = parse_header_dump(&raw_headers)
        .ok_or_else(|| TransportFailure::Other("curl wrote no HTTP status line".into()))?;
    Ok(LinearResponse {
        status,
        headers,
        body: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// Parse a `curl -D` dump into the final response's status and headers. A dump
/// can hold several blocks (a `100 Continue`, a proxy `CONNECT`); the last
/// `HTTP/` block is the real response.
pub(crate) fn parse_header_dump(raw: &str) -> Option<(u16, Vec<(String, String)>)> {
    let mut current: Option<(u16, Vec<(String, String)>)> = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("HTTP/") {
            let status = line.split_whitespace().nth(1)?.parse().ok()?;
            current = Some((status, Vec::new()));
        } else if let (Some((_, headers)), Some((name, value))) = (current.as_mut(), line.split_once(':')) {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_auth::TokenSource;

    #[test]
    fn body_carries_operation_query_and_variables() {
        let req = LinearRequest::new("Viewer", "query Viewer { viewer { id } }", json!({"a": 1}));
        let v: Value = serde_json::from_str(&req.body()).unwrap();
        assert_eq!(v["operationName"], "Viewer");
        assert_eq!(v["variables"]["a"], 1);
        assert_eq!(req.var("a"), &json!(1));
        assert_eq!(req.var("missing"), &Value::Null);
    }

    #[test]
    fn curl_config_quotes_so_the_body_round_trips() {
        let key = SecretToken::new("lin_api_SECRET".into(), TokenSource::Env("LINEAR_API_KEY"));
        let req = LinearRequest::new("Q", "query Q { a }", json!({"text": "line \"one\"\nback\\slash"}));
        let config = curl_config(LINEAR_API_URL, &req, &key);
        assert!(config.contains("header = \"Authorization: lin_api_SECRET\"\n"), "{config}");
        // Undo curl's quoting of the data-binary line and the JSON must survive.
        let line = config.lines().find(|l| l.starts_with("data-binary = ")).unwrap();
        let quoted = &line["data-binary = ".len()..];
        let mut unquoted = String::new();
        let mut chars = quoted[1..quoted.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next().unwrap() {
                    'n' => unquoted.push('\n'),
                    't' => unquoted.push('\t'),
                    'r' => unquoted.push('\r'),
                    other => unquoted.push(other),
                }
            } else {
                unquoted.push(c);
            }
        }
        assert_eq!(unquoted, req.body());
    }

    #[test]
    fn header_dump_takes_the_last_response_block() {
        let raw = "HTTP/1.1 100 Continue\r\n\r\nHTTP/2 200\r\ncontent-type: application/json\r\n\
                   X-RateLimit-Requests-Remaining: 1499\r\n\r\n";
        let (status, headers) = parse_header_dump(raw).unwrap();
        assert_eq!(status, 200);
        let resp = LinearResponse { status, headers, body: String::new() };
        assert_eq!(resp.header("x-ratelimit-requests-remaining"), Some("1499"));
        assert!(parse_header_dump("").is_none());
    }

    #[test]
    fn curl_runner_reports_a_connect_failure_without_leaking_the_key() {
        // Port 9 on localhost: nothing listens, so curl fails to connect fast.
        let key = SecretToken::new("lin_api_SECRET".into(), TokenSource::Env("LINEAR_API_KEY"));
        let req = LinearRequest::new("Viewer", "query Viewer { viewer { id } }", json!({}));
        match run_curl("http://127.0.0.1:9/graphql", &req, &key) {
            Err(TransportFailure::Connect(detail)) => assert!(!detail.contains("SECRET"), "{detail}"),
            other => panic!("expected a connect failure, got {other:?}"),
        }
    }
}
