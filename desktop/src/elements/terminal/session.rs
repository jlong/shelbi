//! The pane's link to a session: connect, then move bytes both ways off the
//! UI thread.
//!
//! [`connect`] blocks (discovery, the socket connect, the hello and `info`
//! round trips), so the pane runs it on the background executor. Once
//! connected:
//!
//! - **Output** arrives on `shelbi_client`'s reader thread as a std channel. A
//!   small forwarder thread moves each [`SessionEvent`] onto an async channel
//!   the pane's foreground task awaits, so the UI thread never blocks on a
//!   read.
//! - **Input** (keys, paste, resize) goes through [`SessionLink`], which hands
//!   each request to a writer thread that owns the connection. A socket write
//!   can block when the session stops draining it; that stall lands on the
//!   writer thread, never on the UI.
//!
//! Dropping the [`SessionLink`] ends the writer thread, which drops the
//! connection; the connection's own `Drop` closes the socket and joins its
//! reader, which ends the forwarder.

use std::io::ErrorKind;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use shelbi_client::{ClientError, Connection, DiscoveredSession, SessionEvent, SessionEvents};
use shelbi_proto::capability;
use shelbi_term::Size;

/// Where the pane's input goes. A trait so the pane can be driven by a fake in
/// tests; the real one is [`SessionLink`].
pub trait SessionIo: 'static {
    /// Raw bytes for the PTY (encoded keys, mouse reports, focus events).
    fn input(&self, bytes: Vec<u8>);
    /// Pasted text. `bracketed` is whether the program enabled bracketed
    /// paste, for a session too old to bracket it itself.
    fn paste(&self, text: String, bracketed: bool);
    /// This client's viewport size. The session applies the most recently
    /// active client's size, debounced on its side too.
    fn resize(&self, size: Size);
}

enum Request {
    Input(Vec<u8>),
    Paste(String, bool),
    Resize(Size),
}

/// The real [`SessionIo`]: requests queued to a writer thread that owns the
/// connection.
pub struct SessionLink {
    tx: mpsc::Sender<Request>,
}

impl SessionIo for SessionLink {
    fn input(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(Request::Input(bytes));
    }

    fn paste(&self, text: String, bracketed: bool) {
        let _ = self.tx.send(Request::Paste(text, bracketed));
    }

    fn resize(&self, size: Size) {
        let _ = self.tx.send(Request::Resize(size));
    }
}

/// A connected session, attached and streaming.
pub struct Connected {
    /// The input side ([`SessionLink`] for a real session).
    pub link: Box<dyn SessionIo + Send>,
    /// Output and pushed events, starting with the attach replay.
    pub events: UnboundedReceiver<SessionEvent>,
    /// The session's size at attach time.
    pub size: Size,
    /// The session's logical name (`project/workspace`).
    pub name: String,
    /// The program's working directory (where a clicked relative path
    /// resolves), when the attach knew it.
    pub cwd: Option<std::path::PathBuf>,
}

/// How long [`connect`] keeps retrying a session that is still starting.
const CONNECT_PATIENCE: Duration = Duration::from_secs(5);

/// Pause between connect attempts.
const CONNECT_RETRY: Duration = Duration::from_millis(200);

/// Why one connect attempt failed.
enum Failure {
    /// Possibly still starting (no live session by that name yet, or its
    /// socket not bound or not yet listening): worth another try.
    Transient(String),
    /// Won't fix itself (an ambiguous selector, a broken handshake).
    Fatal(String),
}

/// Find the live session `selector` names and attach to it, retrying for a
/// few seconds while it may still be starting (a session's directory appears
/// before its socket is bound, the same race the TUI's attach retries).
/// Blocking: run it on a background thread.
///
/// The selector is tried, in order, as an exact session name (`demo/orch`), an
/// exact short id, a short-id prefix, and the workspace segment of a name
/// (`orch`); a stage that matches several live sessions is an error naming
/// them.
pub fn connect(selector: &str) -> Result<Connected, String> {
    let deadline = Instant::now() + CONNECT_PATIENCE;
    loop {
        match attempt(selector) {
            Ok(c) => return Ok(c),
            Err(Failure::Fatal(message)) => return Err(message),
            Err(Failure::Transient(message)) if Instant::now() >= deadline => return Err(message),
            Err(Failure::Transient(_)) => std::thread::sleep(CONNECT_RETRY),
        }
    }
}

fn attempt(selector: &str) -> Result<Connected, Failure> {
    let root = shelbi_state::sessions_dir().map_err(|e| Failure::Fatal(e.to_string()))?;
    let sessions =
        shelbi_client::list(&root).map_err(|e| Failure::Fatal(format!("listing sessions: {e}")))?;
    let session = resolve(&sessions, selector)?;

    let (conn, events) = Connection::open(&session.sock, None, capability::ALL).map_err(|e| {
        let message = format!("connecting to {}: {e}", session.meta.name);
        match &e {
            ClientError::Io(io)
                if matches!(
                    io.kind(),
                    ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::NotConnected
                ) =>
            {
                Failure::Transient(message)
            }
            _ => Failure::Fatal(message),
        }
    })?;
    let fatal =
        |what: &str, e: ClientError| Failure::Fatal(format!("{what} {}: {e}", session.meta.name));
    let info = conn.info().map_err(|e| fatal("querying", e))?;
    conn.attach(None).map_err(|e| fatal("attaching to", e))?;
    let connected = wire(
        conn,
        events,
        Size::new(info.cols.max(1), info.rows.max(1)),
        session.meta.name.clone(),
    )
    .map_err(Failure::Fatal)?;
    Ok(Connected {
        cwd: Some(session.meta.cwd.clone()),
        ..connected
    })
}

/// Move an attached connection onto the pane's threads: a forwarder that moves
/// the reader's events onto an async channel the UI awaits, and a writer that
/// owns the connection. Used by [`connect`] and by views that attach through
/// `shelbi_app::session` (the Chat view), so every pane talks to its session
/// the same way.
pub fn wire(
    conn: Connection,
    events: SessionEvents,
    size: Size,
    name: String,
) -> Result<Connected, String> {
    // Forward the reader's std channel onto an async one the UI awaits.
    let (event_tx, event_rx) = unbounded();
    let std_rx = events.into_inner();
    std::thread::Builder::new()
        .name("shelbi-desktop-session-events".into())
        .spawn(move || {
            while let Ok(ev) = std_rx.recv() {
                if event_tx.unbounded_send(ev).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;

    // The writer thread owns the connection.
    let supports_paste = conn.supports(capability::PASTE);
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("shelbi-desktop-session-writer".into())
        .spawn(move || {
            for req in rx {
                let sent = match req {
                    Request::Input(bytes) => conn.input(&bytes),
                    Request::Paste(text, bracketed) => {
                        if supports_paste {
                            conn.paste(&text)
                        } else {
                            // An old session can't bracket a paste itself.
                            conn.input(&shelbi_term::input::encode_paste(&text, bracketed))
                        }
                    }
                    Request::Resize(size) => conn.resize(size.cols.max(1), size.rows.max(1)),
                };
                if sent.is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;

    Ok(Connected {
        link: Box::new(SessionLink { tx }),
        events: event_rx,
        size,
        name,
        cwd: None,
    })
}

/// Resolve a selector to one live session (see [`connect`]).
fn resolve(sessions: &[DiscoveredSession], selector: &str) -> Result<DiscoveredSession, Failure> {
    // An exact name goes through the shared chooser, which picks the usable
    // session among duplicates (a relaunch beside a lingering zombie).
    if let Some(s) = shelbi_client::choose_session(sessions, selector) {
        return Ok(s);
    }
    let live: Vec<&DiscoveredSession> = sessions.iter().filter(|s| s.alive).collect();
    let stages: [&dyn Fn(&DiscoveredSession) -> bool; 3] = [
        &|s| s.short_id == selector,
        &|s| s.short_id.starts_with(selector),
        &|s| s.meta.name.rsplit('/').next() == Some(selector),
    ];
    for stage in stages {
        let hits: Vec<&&DiscoveredSession> = live.iter().filter(|s| stage(s)).collect();
        match hits.as_slice() {
            [] => continue,
            [one] => return Ok((**one).clone()),
            many => {
                let names: Vec<String> = many
                    .iter()
                    .map(|s| format!("{} ({})", s.short_id, s.meta.name))
                    .collect();
                return Err(Failure::Fatal(format!(
                    "`{selector}` matches several sessions: {}",
                    names.join(", ")
                )));
            }
        }
    }
    Err(Failure::Transient(format!(
        "no live session matching `{selector}` (try `shelbi session ls`)"
    )))
}
