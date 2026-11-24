//! The models OpenCode lists, with each one's reasoning variants: what the
//! Default model pickers and the agent editor's Model and Reasoning effort
//! pickers offer for an OpenCode runner. Its `provider/model` ids depend on
//! the providers the user configured, so there's no fixed list
//! ([`shelbi_core::model_suggestions`] has none for it); this asks the
//! installed OpenCode.
//!
//! The probe runs once per process, off the UI thread, the first time a
//! picker asks ([`catalog`]), within [`DEADLINE`]. Until it answers the
//! catalog is [`Catalog::Probing`]; when it can't answer (OpenCode isn't
//! installed, too old, or no answer in time) it's [`Catalog::Unavailable`]
//! and the pickers take a typed model only. A client that draws a picker
//! waits on [`wait`] off its UI thread and redraws when it returns.
//!
//! How it asks: `opencode models` prints the `provider/model` ids but not
//! their variants, so the probe starts a private `opencode serve` (an
//! ephemeral loopback port, a one-off password, its own process group) and
//! reads `GET /api/model`, each model with its variants, then stops it.
//! Nothing in the user's OpenCode config or background service is touched.
//! If the server doesn't answer, `opencode models --standalone` gives the
//! ids alone (no effort is offered then).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use shelbi_core::{OpenCodeModel, ReasoningEffort};

/// How long the probe may take, end to end.
pub const DEADLINE: Duration = Duration::from_secs(5);

/// What OpenCode lists, as the pickers see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Catalog {
    /// The probe is still running.
    Probing,
    /// The models, sorted by id.
    Listed(Arc<[OpenCodeModel]>),
    /// No list: the pickers take a typed model only.
    Unavailable,
}

impl Catalog {
    /// The listed ids (none while probing or unavailable).
    pub fn ids(&self) -> Vec<String> {
        match self {
            Catalog::Listed(models) => models.iter().map(|m| m.id.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// `model`'s entry, when listed.
    pub fn model(&self, model: &str) -> Option<&OpenCodeModel> {
        match self {
            Catalog::Listed(models) => models.iter().find(|m| m.id == model),
            _ => None,
        }
    }

    /// The efforts `model` takes: the ones it reports a same-named variant
    /// for. Empty for no model, an unlisted one, or one without them.
    pub fn efforts(&self, model: Option<&str>) -> Vec<ReasoningEffort> {
        model
            .and_then(|m| self.model(m))
            .map(|m| shelbi_core::efforts_for_variants(&m.variants))
            .unwrap_or_default()
    }

    /// The probe has answered (either way).
    pub fn settled(&self) -> bool {
        !matches!(self, Catalog::Probing)
    }
}

/// The catalog, starting the probe on the first call. Never blocks.
///
/// A test binary never probes the machine: it sees [`Catalog::Unavailable`]
/// unless its thread installed one ([`set_for_this_thread`]).
pub fn catalog() -> Catalog {
    if let Some(c) = OVERRIDE.with(|o| o.borrow().clone()) {
        return c;
    }
    if shelbi_state::in_test_binary() {
        return Catalog::Unavailable;
    }
    let cell = cell();
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    if !cell.started.swap(true, std::sync::atomic::Ordering::SeqCst) {
        *state = Catalog::Probing;
        std::thread::Builder::new()
            .name("opencode-models".into())
            .spawn(|| {
                let result = probe();
                let cell = self::cell();
                *cell.state.lock().unwrap_or_else(|e| e.into_inner()) = result;
                cell.ready.notify_all();
            })
            .ok();
    }
    state.clone()
}

/// The catalog once the probe has answered, or as it stands after
/// `timeout`. Blocking: for a client's background task that redraws its
/// picker when this returns. Starts the probe if nothing has.
pub fn wait(timeout: Duration) -> Catalog {
    let now = catalog();
    if now.settled() || OVERRIDE.with(|o| o.borrow().is_some()) {
        return now;
    }
    let cell = cell();
    let state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    let (state, _) = cell
        .ready
        .wait_timeout_while(state, timeout, |s| !s.settled())
        .unwrap_or_else(|e| e.into_inner());
    state.clone()
}

struct Cell {
    started: std::sync::atomic::AtomicBool,
    state: Mutex<Catalog>,
    ready: Condvar,
}

fn cell() -> &'static Cell {
    static CELL: OnceLock<Cell> = OnceLock::new();
    CELL.get_or_init(|| Cell {
        started: std::sync::atomic::AtomicBool::new(false),
        state: Mutex::new(Catalog::Probing),
        ready: Condvar::new(),
    })
}

thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Catalog>> = const { std::cell::RefCell::new(None) };
}

/// Make [`catalog`] answer `catalog` on this thread (`None` puts the probe
/// back): a test's, or the visual check's, stand-in for a real OpenCode.
pub fn set_for_this_thread(catalog: Option<Catalog>) {
    OVERRIDE.with(|o| *o.borrow_mut() = catalog);
}

/// A listed catalog of `(id, variants)`, for tests and the visual check.
pub fn listed(models: &[(&str, &[&str])]) -> Catalog {
    Catalog::Listed(
        models
            .iter()
            .map(|(id, variants)| OpenCodeModel {
                id: id.to_string(),
                variants: variants.iter().map(|v| v.to_string()).collect(),
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// The probe

/// Ask the installed OpenCode, within [`DEADLINE`].
fn probe() -> Catalog {
    let started = Instant::now();
    let Some((binary, path_env)) = locate() else {
        return Catalog::Unavailable;
    };
    let dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    probe_with(&binary, path_env.as_deref(), &dir, started + DEADLINE)
}

/// Ask `binary`, run in `dir`, by `deadline`: the server's models with
/// their variants, else `opencode models`' ids.
fn probe_with(binary: &Path, path_env: Option<&std::ffi::OsStr>, dir: &Path, deadline: Instant) -> Catalog {
    let listed = serve_models(binary, path_env, dir, deadline).or_else(|| models_command(binary, path_env, dir, deadline));
    match listed {
        Some(models) if !models.is_empty() => Catalog::Listed(sorted(models).into()),
        _ => Catalog::Unavailable,
    }
}

fn sorted(mut models: Vec<OpenCodeModel>) -> Vec<OpenCodeModel> {
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models
}

/// The `opencode` the requirements check finds (a located one, else the
/// login `PATH`'s), when it's one Shelbi drives, and that `PATH` for it.
fn locate() -> Option<(PathBuf, Option<std::ffi::OsString>)> {
    use crate::prereqs::{resolve, Binary, Platform, Probe, SystemProbe};
    let probe = SystemProbe::login();
    let located = shelbi_state::tool_paths::located();
    let (path, _) = resolve(Binary::OpenCode, &located, Platform::current(), &probe)?;
    let version = probe.version(&path).ok()?;
    if shelbi_core::opencode_version_supported(&version) == Some(false) {
        return None;
    }
    Some((path, std::env::join_paths(probe.path_dirs()).ok()))
}

/// A command for `binary` with the login `PATH`, autoupdate off, no input,
/// in its own process group (so stopping it stops its children).
fn command(binary: &Path, path_env: Option<&std::ffi::OsStr>, dir: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.current_dir(dir).env("OPENCODE_DISABLE_AUTOUPDATE", "1").stdin(Stdio::null());
    if let Some(path) = path_env {
        cmd.env("PATH", path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Stop a child started by [`command`]: its whole process group, asked
/// first and then made to.
fn stop(mut child: Child) {
    let pgid = child.id();
    shelbi_core::signal_process_group(pgid, false);
    let until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < until {
        if let Ok(Some(_)) = child.try_wait() {
            // The leader is gone; a helper it started may not be.
            shelbi_core::signal_process_group(pgid, true);
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    shelbi_core::signal_process_group(pgid, true);
    let _ = child.kill();
    let _ = child.wait();
}

/// Start a private `opencode serve`, read its models with their variants,
/// stop it. `None` when it doesn't answer by `deadline`.
fn serve_models(binary: &Path, path_env: Option<&std::ffi::OsStr>, dir: &Path, deadline: Instant) -> Option<Vec<OpenCodeModel>> {
    let password = one_off_password();
    let mut cmd = command(binary, path_env, dir);
    cmd.args(["serve", "--hostname", "127.0.0.1", "--port", "0"])
        .env("OPENCODE_PASSWORD", &password)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(stdout) = stdout {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(addr) = listening_addr(&line) {
                    let _ = tx.send(addr);
                    break;
                }
            }
        });
    }
    let found = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
        .and_then(|addr| poll_models(&addr, &password, dir, deadline));
    stop(child);
    found
}

/// `host:port` in `server listening on http://127.0.0.1:50807`.
fn listening_addr(line: &str) -> Option<String> {
    let rest = line.split("http://").nth(1)?;
    let addr: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == ':').collect();
    addr.contains(':').then_some(addr)
}

/// `GET /api/model` until it lists something: the catalog loads after the
/// server starts, so the first answer can be empty.
fn poll_models(addr: &str, password: &str, dir: &Path, deadline: Instant) -> Option<Vec<OpenCodeModel>> {
    let path = format!("/api/model?location%5Bdirectory%5D={}", percent_encode(&dir.to_string_lossy()));
    let auth = format!("Basic {}", base64(format!("opencode:{password}").as_bytes()));
    loop {
        if let Some(body) = http_get(addr, &path, &auth, deadline) {
            match parse_model_list(&body) {
                Some(models) if !models.is_empty() => return Some(models),
                Some(_) => {}
                // Not the shape Shelbi reads: ask `opencode models` instead.
                None => return None,
            }
        }
        if Instant::now() + Duration::from_millis(100) >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `opencode models --standalone`'s ids, without variants.
fn models_command(binary: &Path, path_env: Option<&std::ffi::OsStr>, dir: &Path, deadline: Instant) -> Option<Vec<OpenCodeModel>> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return None;
    }
    let mut cmd = command(binary, path_env, dir);
    cmd.args(["models", "--standalone"]).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(left).ok();
    stop(child);
    let ids = shelbi_core::parse_opencode_models(&out?);
    Some(ids.into_iter().map(|id| OpenCodeModel { id, variants: Vec::new() }).collect())
}

/// The models in a `GET /api/model` answer (`{"data":[{"providerID",
/// "id", "variants":[{"id"}], "enabled"}]}`): each enabled chat model as
/// `provider/id` with its variants' ids. `None` when it isn't that shape.
pub fn parse_model_list(body: &str) -> Option<Vec<OpenCodeModel>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = value.get("data")?.as_array()?;
    Some(
        data.iter()
            .filter(|m| m.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true))
            .filter_map(|m| {
                let provider = m.get("providerID")?.as_str()?;
                let id = m.get("id")?.as_str()?;
                let variants = m
                    .get("variants")
                    .and_then(|v| v.as_array())
                    .map(|vs| vs.iter().filter_map(|v| v.get("id")?.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                Some(OpenCodeModel { id: format!("{provider}/{id}"), variants })
            })
            .collect(),
    )
}

/// One `GET` over HTTP/1.1 on loopback: the body of a 200, else `None`.
fn http_get(addr: &str, path: &str, auth: &str, deadline: Instant) -> Option<String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return None;
    }
    let socket = addr.parse().ok()?;
    let mut stream = TcpStream::connect_timeout(&socket, left).ok()?;
    stream.set_read_timeout(Some(left)).ok()?;
    stream.set_write_timeout(Some(left)).ok()?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {auth}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    http_body(&raw)
}

/// The body of a raw HTTP/1.1 200 response (chunked or not).
fn http_body(raw: &[u8]) -> Option<String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body = &raw[split + 4..];
    if !head.lines().next()?.contains(" 200") {
        return None;
    }
    let chunked = head.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked"));
    let bytes = if chunked { dechunk(body)? } else { body.to_vec() };
    String::from_utf8(bytes).ok()
}

/// A chunked body's bytes.
fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = body.windows(2).position(|w| w == b"\r\n")?;
        let size_line = std::str::from_utf8(&body[..eol]).ok()?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        body = &body[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A password for one probe's server: never written anywhere, gone with it.
fn one_off_password() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    (0..2)
        .map(|_| {
            let mut h = RandomState::new().build_hasher();
            h.write_u128(Instant::now().elapsed().as_nanos() ^ u128::from(std::process::id()));
            format!("{:016x}", h.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_list_reads_ids_and_variants() {
        let body = r#"{"location":{"directory":"/x"},"data":[
            {"providerID":"opencode","id":"deepseek-v4.1-flash-free","enabled":true,
             "variants":[{"id":"low"},{"id":"high"},{"id":"max","settings":{}}]},
            {"providerID":"opencode","id":"big-pickle","enabled":true,"variants":[]},
            {"providerID":"anthropic","id":"off","enabled":false,"variants":[]}]}"#;
        let models = parse_model_list(body).unwrap();
        assert_eq!(
            models,
            [
                OpenCodeModel { id: "opencode/deepseek-v4.1-flash-free".into(), variants: vec!["low".into(), "high".into(), "max".into()] },
                OpenCodeModel { id: "opencode/big-pickle".into(), variants: vec![] },
            ]
        );
        assert_eq!(parse_model_list(r#"{"data":[]}"#), Some(vec![]));
        assert_eq!(parse_model_list("<html>"), None);
        assert_eq!(parse_model_list(r#"{"error":"x"}"#), None);
    }

    #[test]
    fn efforts_are_the_chosen_models_variants() {
        let c = listed(&[("p/a", &["minimal", "low", "high"]), ("p/b", &[])]);
        assert_eq!(c.efforts(Some("p/a")), [ReasoningEffort::Low, ReasoningEffort::High]);
        assert!(c.efforts(Some("p/b")).is_empty());
        assert!(c.efforts(Some("p/unlisted")).is_empty());
        assert!(c.efforts(None).is_empty());
        assert_eq!(c.ids(), ["p/a", "p/b"]);
        assert!(Catalog::Unavailable.ids().is_empty() && Catalog::Probing.efforts(Some("p/a")).is_empty());
    }

    #[test]
    fn a_test_binary_never_probes_and_a_thread_can_stand_in() {
        assert_eq!(catalog(), Catalog::Unavailable);
        set_for_this_thread(Some(listed(&[("p/a", &[])])));
        assert_eq!(catalog().ids(), ["p/a"]);
        assert_eq!(wait(Duration::from_millis(1)).ids(), ["p/a"]);
        set_for_this_thread(None);
        assert_eq!(catalog(), Catalog::Unavailable);
    }

    #[test]
    fn the_listening_line_gives_the_address() {
        assert_eq!(listening_addr("server listening on http://127.0.0.1:50807").as_deref(), Some("127.0.0.1:50807"));
        assert_eq!(listening_addr("server password abc"), None);
    }

    #[test]
    fn http_bodies_read_plain_and_chunked() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(http_body(plain).as_deref(), Some("{}"));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(http_body(chunked).as_deref(), Some("{\"a\":1}"));
        assert_eq!(http_body(b"HTTP/1.1 401 Unauthorized\r\n\r\nno"), None);
    }

    #[test]
    fn basic_auth_and_query_encoding() {
        assert_eq!(base64(b"opencode:pw"), "b3BlbmNvZGU6cHc=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(percent_encode("/Users/a b/x"), "/Users/a%20b/x");
    }

    /// The real probe against an installed OpenCode 2.x. Run it inside a
    /// sandbox (`HOME` and the `XDG_*` dirs pointed at scratch dirs):
    /// `SHELBI_OPENCODE_LIVE_BIN=<path to opencode> cargo test -p shelbi-app
    /// --lib opencode_models::tests::live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_probe_lists_models_with_variants() {
        let binary = PathBuf::from(std::env::var("SHELBI_OPENCODE_LIVE_BIN").expect("SHELBI_OPENCODE_LIVE_BIN"));
        let dir = std::env::temp_dir();
        let started = Instant::now();
        let catalog = probe_with(&binary, None, &dir, started + DEADLINE);
        println!("{:?} in {:?}", catalog, started.elapsed());
        let Catalog::Listed(models) = &catalog else { panic!("{catalog:?}") };
        assert!(models.iter().any(|m| !m.variants.is_empty()), "{models:?}");
        assert!(started.elapsed() < DEADLINE + Duration::from_secs(1));
        let fallback = models_command(&binary, None, &dir, Instant::now() + DEADLINE).unwrap();
        println!("opencode models: {:?}", fallback.iter().map(|m| &m.id).collect::<Vec<_>>());
        assert!(!fallback.is_empty());
    }
}
