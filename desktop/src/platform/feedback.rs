//! Contact Us: the feedback a user sends from the app (`dt-feedback-dialog`),
//! posted to the 32pixels.co endpoint ContextStore uses.
//!
//! The request is one JSON `POST` to [`ENDPOINT`] with `Content-Type` and
//! `Accept` both `application/json`. Its body is `appName` (`"Shelbi"`),
//! `email`, `type` (`bug`, `feature` or `other`), `message` and
//! `timestamp` (ISO 8601, UTC), plus `name` when one was given and, when the
//! user leaves "Include system info" on, `appVersion` and `macOSVersion`.
//! The answer decides what the dialog shows ([`classify`]): any 2xx is sent;
//! 422 is a validation failure, its `error` field the message; 429 is the
//! rate limit; anything else is a server error naming the status.
//!
//! [`FeedbackService`] is the seam: the live one sends through
//! `/usr/bin/curl` (as the update check does: present on every Mac, and it
//! keeps an HTTP stack out of the app) and reads the clock and the system;
//! tests set their own transport, so nothing reaches the real endpoint.
//! Sending blocks: callers run it on the background executor.
//!
//! The name and email given last are remembered for next time
//! ([`Remembered`]), per user, in `~/.shelbi/desktop-feedback.json`; never
//! in a project's config.
//!
//! `SHELBI_DESKTOP_FEEDBACK_URL` posts somewhere else instead (any URL curl
//! takes), for trying the dialog against a local server.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::version::APP_VERSION;

/// Where feedback goes.
#[cfg_attr(test, allow(dead_code))]
pub const ENDPOINT: &str = "https://32pixels.co/api/feedback";
/// The `appName` the endpoint files it under.
pub const APP_NAME: &str = "Shelbi";
/// The remembered name and email, in the Shelbi home.
pub const REMEMBERED_FILE: &str = "desktop-feedback.json";

#[cfg_attr(test, allow(dead_code))]
const URL_ENV: &str = "SHELBI_DESKTOP_FEEDBACK_URL";
#[cfg_attr(test, allow(dead_code))]
const TIMEOUT_SECS: &str = "20";
/// A server error's body is shown only when it is a short line of text (not
/// a page of HTML).
const SHOWN_BODY_MAX: usize = 200;

/// What the feedback is about: the dialog's segmented control.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    #[default]
    Bug,
    Feature,
    Other,
}

impl Kind {
    /// Every kind, in the control's order.
    pub const ALL: [Kind; 3] = [Kind::Bug, Kind::Feature, Kind::Other];

    /// The `type` the endpoint takes.
    pub fn wire(self) -> &'static str {
        match self {
            Kind::Bug => "bug",
            Kind::Feature => "feature",
            Kind::Other => "other",
        }
    }

    /// Its segment's label.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Bug => "Bug Report",
            Kind::Feature => "Feature Request",
            Kind::Other => "Other",
        }
    }
}

/// One validated piece of feedback, its fields trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// `None` when left empty.
    pub name: Option<String>,
    pub email: String,
    pub kind: Kind,
    pub message: String,
    /// Whether to add [`SystemInfo`].
    pub include_system_info: bool,
}

/// What "Include system info" adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    /// The app's version (the bundle's build number is the same string).
    pub app_version: String,
    /// `major.minor.patch`.
    pub macos_version: String,
}

impl SystemInfo {
    /// This app on this Mac.
    #[cfg_attr(test, allow(dead_code))]
    pub fn current() -> SystemInfo {
        SystemInfo {
            app_version: APP_VERSION.to_string(),
            macos_version: macos_version().unwrap_or_else(|| "unknown".to_string()),
        }
    }
}

/// The request body for `submission`, made at `timestamp`. `system` is
/// added only when the submission asks for it.
pub fn payload(submission: &Submission, timestamp: &str, system: Option<&SystemInfo>) -> Value {
    let mut body = Map::new();
    body.insert("appName".into(), APP_NAME.into());
    body.insert("email".into(), submission.email.clone().into());
    body.insert("type".into(), submission.kind.wire().into());
    body.insert("message".into(), submission.message.clone().into());
    body.insert("timestamp".into(), timestamp.into());
    if let Some(name) = submission.name.as_ref().filter(|n| !n.is_empty()) {
        body.insert("name".into(), name.clone().into());
    }
    if let (true, Some(system)) = (submission.include_system_info, system) {
        body.insert("appVersion".into(), system.app_version.clone().into());
        body.insert("macOSVersion".into(), system.macos_version.clone().into());
    }
    Value::Object(body)
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Why it didn't send. [`std::fmt::Display`] is what the dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackError {
    /// 422: the endpoint's own words.
    Validation(String),
    /// 429.
    RateLimited,
    /// Any other status, with the endpoint's message when it gave a short one.
    Server { status: u16, message: Option<String> },
    /// The request never got an answer (offline, a timeout).
    Network(String),
}

impl std::fmt::Display for FeedbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedbackError::Validation(message) => f.write_str(message),
            FeedbackError::RateLimited => f.write_str("Too many submissions. Please try again later."),
            FeedbackError::Server { status, message: Some(m) } => write!(f, "Server error (HTTP {status}): {m}"),
            FeedbackError::Server { status, message: None } => write!(f, "Server error (HTTP {status})"),
            FeedbackError::Network(why) => write!(f, "Couldn't send your message: {why}"),
        }
    }
}

/// The response's `error` field, if it is JSON with one.
fn error_field(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value.get("error")?.as_str().map(str::to_string)
}

/// Sent, or why not.
pub fn classify(response: &HttpResponse) -> Result<(), FeedbackError> {
    match response.status {
        200..=299 => Ok(()),
        422 => Err(FeedbackError::Validation(
            error_field(&response.body).unwrap_or_else(|| "Validation failed".to_string()),
        )),
        429 => Err(FeedbackError::RateLimited),
        status => {
            let body = response.body.trim();
            let message = error_field(body).or_else(|| {
                (!body.is_empty() && body.len() <= SHOWN_BODY_MAX && !body.contains('<') && !body.contains('\n'))
                    .then(|| body.to_string())
            });
            Err(FeedbackError::Server { status, message })
        }
    }
}

/// Posts a JSON body and returns the answer, or why there was none.
pub type Transport = Arc<dyn Fn(&str) -> Result<HttpResponse, String> + Send + Sync>;

/// The name and email given last.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
}

impl Remembered {
    /// What `path` holds; empty when it's missing or unreadable.
    pub fn load(path: &Path) -> Remembered {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Write it to `path` (whole, through a temporary file).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }
}

/// How feedback is sent: the transport, the system's details, the clock and
/// where the name and email are remembered. A gpui global when a test sets
/// one; otherwise [`FeedbackService::live`].
#[derive(Clone)]
pub struct FeedbackService {
    pub transport: Transport,
    pub system: Arc<dyn Fn() -> SystemInfo + Send + Sync>,
    pub now: Arc<dyn Fn() -> String + Send + Sync>,
    /// `None`: nothing is remembered.
    pub remembered: Option<PathBuf>,
}

impl gpui::Global for FeedbackService {}

impl FeedbackService {
    /// The real endpoint (or `SHELBI_DESKTOP_FEEDBACK_URL`), this Mac, the
    /// clock, and the Shelbi home.
    #[cfg_attr(test, allow(dead_code))]
    pub fn live() -> FeedbackService {
        let url = std::env::var(URL_ENV)
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| ENDPOINT.to_string());
        FeedbackService {
            transport: Arc::new(move |body: &str| post_with_curl(&url, body)),
            system: Arc::new(SystemInfo::current),
            now: Arc::new(now_iso8601),
            remembered: shelbi_state::shelbi_home().ok().map(|h| h.join(REMEMBERED_FILE)),
        }
    }

    /// What a test's dialog sends through when the test set no service: no
    /// network, no file, a fixed clock and system. Nothing a test opens can
    /// reach the real endpoint.
    #[cfg(test)]
    pub fn offline() -> FeedbackService {
        FeedbackService {
            transport: Arc::new(|_: &str| Err("tests don't send feedback".to_string())),
            system: Arc::new(|| SystemInfo {
                app_version: APP_VERSION.to_string(),
                macos_version: "15.0.0".to_string(),
            }),
            now: Arc::new(|| "2026-10-27T12:00:00Z".to_string()),
            remembered: None,
        }
    }

    /// The name and email to start from.
    pub fn remembered(&self) -> Remembered {
        self.remembered.as_deref().map(Remembered::load).unwrap_or_default()
    }

    /// Remember `submission`'s name and email, then send it (blocking: run
    /// it off the UI thread).
    pub fn send(&self, submission: &Submission) -> Result<(), FeedbackError> {
        if let Some(path) = &self.remembered {
            let remembered = Remembered {
                name: submission.name.clone().unwrap_or_default(),
                email: submission.email.clone(),
            };
            if let Err(e) = remembered.save(path) {
                tracing::warn!(path = %path.display(), "couldn't remember the feedback name and email: {e}");
            }
        }
        let system = submission.include_system_info.then(|| (self.system)());
        let body = payload(submission, &(self.now)(), system.as_ref()).to_string();
        let response = (self.transport)(&body).map_err(FeedbackError::Network)?;
        classify(&response)
    }
}

/// Now, as ISO 8601 in UTC to the second (`2026-10-27T12:00:00Z`, what
/// Foundation's `ISO8601DateFormatter` writes).
pub fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// `sw_vers -productVersion`, padded to `major.minor.patch`.
#[cfg_attr(test, allow(dead_code))]
fn macos_version() -> Option<String> {
    let out = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let version = String::from_utf8(out.stdout).ok()?;
    let version = version.trim();
    (!version.is_empty()).then(|| full_version(version))
}

/// `15.1` as `15.1.0`: the three parts `operatingSystemVersion` has.
fn full_version(version: &str) -> String {
    let parts = version.split('.').count();
    let mut v = version.to_string();
    for _ in parts..3 {
        v.push_str(".0");
    }
    v
}

/// Split curl's output (the body, then a line holding `%{http_code}`).
fn parse_curl_output(stdout: &str) -> Option<HttpResponse> {
    let (body, code) = stdout.rsplit_once('\n')?;
    let status = code.trim().parse().ok().filter(|s| *s != 0)?;
    Some(HttpResponse {
        status,
        body: body.to_string(),
    })
}

#[cfg_attr(test, allow(dead_code))]
fn post_with_curl(url: &str, body: &str) -> Result<HttpResponse, String> {
    let mut child = Command::new("/usr/bin/curl")
        .args(["--silent", "--show-error", "--location", "--request", "POST"])
        .args(["--max-time", TIMEOUT_SECS])
        .args(["--header", "Content-Type: application/json"])
        .args(["--header", "Accept: application/json"])
        .args(["--user-agent", &format!("shelbi-desktop/{APP_VERSION}")])
        .args(["--data-binary", "@-"])
        .args(["--write-out", "\n%{http_code}"])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl didn't run: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match parse_curl_output(&stdout) {
        Some(response) => Ok(response),
        None => {
            let why = String::from_utf8_lossy(&out.stderr).trim().trim_start_matches("curl: ").to_string();
            Err(if why.is_empty() { format!("curl exited with {}", out.status) } else { why })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn submission(name: Option<&str>, include_system_info: bool) -> Submission {
        Submission {
            name: name.map(str::to_string),
            email: "ada@example.com".into(),
            kind: Kind::Feature,
            message: "Add a dark dock icon".into(),
            include_system_info,
        }
    }

    fn system() -> SystemInfo {
        SystemInfo {
            app_version: "1.2.3".into(),
            macos_version: "15.1.0".into(),
        }
    }

    const NOW: &str = "2026-10-27T12:00:00Z";

    #[test]
    fn the_payload_without_system_info_is_the_required_fields() {
        let body = payload(&submission(None, false), NOW, Some(&system()));
        assert_eq!(
            body,
            serde_json::json!({
                "appName": "Shelbi",
                "email": "ada@example.com",
                "type": "feature",
                "message": "Add a dark dock icon",
                "timestamp": NOW,
            })
        );
    }

    #[test]
    fn the_payload_adds_the_name_and_system_info_when_given() {
        let body = payload(&submission(Some("Ada"), true), NOW, Some(&system()));
        assert_eq!(
            body,
            serde_json::json!({
                "appName": "Shelbi",
                "name": "Ada",
                "email": "ada@example.com",
                "type": "feature",
                "message": "Add a dark dock icon",
                "timestamp": NOW,
                "appVersion": "1.2.3",
                "macOSVersion": "15.1.0",
            })
        );
        let empty_name = payload(&submission(Some(""), false), NOW, None);
        assert!(empty_name.get("name").is_none());
    }

    #[test]
    fn each_kind_has_its_wire_type_and_label() {
        let wire: Vec<_> = Kind::ALL.iter().map(|k| (k.wire(), k.label())).collect();
        assert_eq!(wire, [("bug", "Bug Report"), ("feature", "Feature Request"), ("other", "Other")]);
        assert_eq!(Kind::default(), Kind::Bug);
    }

    fn answer(status: u16, body: &str) -> Result<(), FeedbackError> {
        classify(&HttpResponse { status, body: body.into() })
    }

    #[test]
    fn responses_classify_by_status() {
        assert_eq!(answer(200, ""), Ok(()));
        assert_eq!(answer(201, "{}"), Ok(()));
        assert_eq!(
            answer(422, r#"{"error":"Email is invalid"}"#),
            Err(FeedbackError::Validation("Email is invalid".into()))
        );
        assert_eq!(answer(422, "nope"), Err(FeedbackError::Validation("Validation failed".into())));
        assert_eq!(answer(429, ""), Err(FeedbackError::RateLimited));
        assert_eq!(answer(500, ""), Err(FeedbackError::Server { status: 500, message: None }));
        assert_eq!(
            answer(503, "upstream down"),
            Err(FeedbackError::Server { status: 503, message: Some("upstream down".into()) })
        );
        assert_eq!(
            answer(502, "<html><body>Bad Gateway</body></html>"),
            Err(FeedbackError::Server { status: 502, message: None })
        );
    }

    #[test]
    fn errors_read_as_the_dialog_shows_them() {
        assert_eq!(FeedbackError::RateLimited.to_string(), "Too many submissions. Please try again later.");
        assert_eq!(FeedbackError::Validation("Email is invalid".into()).to_string(), "Email is invalid");
        assert_eq!(FeedbackError::Server { status: 500, message: None }.to_string(), "Server error (HTTP 500)");
        assert_eq!(
            FeedbackError::Server { status: 503, message: Some("down".into()) }.to_string(),
            "Server error (HTTP 503): down"
        );
    }

    #[test]
    fn curl_output_splits_into_body_and_status() {
        assert_eq!(
            parse_curl_output("{\"ok\":true}\n200"),
            Some(HttpResponse { status: 200, body: "{\"ok\":true}".into() })
        );
        assert_eq!(parse_curl_output("\n429"), Some(HttpResponse { status: 429, body: String::new() }));
        // No answer at all: curl writes 000.
        assert_eq!(parse_curl_output("\n000"), None);
        assert_eq!(parse_curl_output(""), None);
    }

    #[test]
    fn versions_pad_to_three_parts() {
        assert_eq!(full_version("15"), "15.0.0");
        assert_eq!(full_version("15.1"), "15.1.0");
        assert_eq!(full_version("15.1.2"), "15.1.2");
    }

    #[test]
    fn the_timestamp_is_iso_8601_utc() {
        let now = now_iso8601();
        assert!(chrono::DateTime::parse_from_rfc3339(&now).is_ok(), "{now}");
        assert!(now.ends_with('Z'), "{now}");
    }

    #[test]
    fn send_remembers_the_name_and_email_and_posts_through_the_transport() {
        let dir = std::env::temp_dir().join(format!("shelbi-desktop-feedback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(REMEMBERED_FILE);
        let sent = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = sent.clone();
        let service = FeedbackService {
            transport: Arc::new(move |body: &str| {
                log.lock().unwrap().push(body.to_string());
                Ok(HttpResponse { status: 429, body: String::new() })
            }),
            system: Arc::new(system),
            now: Arc::new(|| NOW.to_string()),
            remembered: Some(path.clone()),
        };
        assert_eq!(service.remembered(), Remembered::default());
        assert_eq!(service.send(&submission(Some("Ada"), true)), Err(FeedbackError::RateLimited));
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let body: Value = serde_json::from_str(&sent[0]).unwrap();
        assert_eq!(body, payload(&submission(Some("Ada"), true), NOW, Some(&system())));
        assert_eq!(
            service.remembered(),
            Remembered { name: "Ada".into(), email: "ada@example.com".into() }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_transport_failure_is_a_network_error() {
        let service = FeedbackService {
            transport: Arc::new(|_: &str| Err("Could not resolve host: 32pixels.co".to_string())),
            system: Arc::new(system),
            now: Arc::new(|| NOW.to_string()),
            remembered: None,
        };
        let err = service.send(&submission(None, false)).unwrap_err();
        assert_eq!(err, FeedbackError::Network("Could not resolve host: 32pixels.co".into()));
        assert_eq!(err.to_string(), "Couldn't send your message: Could not resolve host: 32pixels.co");
    }
}
