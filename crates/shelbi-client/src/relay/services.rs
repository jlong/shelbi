//! The relay-side services of relay protocol v2, opened with
//! [`RelayFrame::OpenService`].
//!
//! A remote desktop window reaches a project on another machine through one
//! `shelbi relay` over one SSH stdio channel. Besides the session streams the
//! relay already bridges, it serves three named [`RelayService`]s:
//!
//! - **`query`**: request/response reads. Each message either way is framed
//!   `[len: u32 BE][payload]` (the shape `shelbi_proto::control::encode` uses);
//!   the payloads are opaque here. A [`QueryHandler`] the far CLI injects
//!   answers each request on a per-stream worker thread, so a slow read never
//!   holds up the channel's other streams or its keepalive.
//! - **`control`**: a bridge to the far daemon's `control.sock`. The relay
//!   decodes every frame the client sends and refuses the messages a remote
//!   window may not send ([`control_refusal`]), answering them itself with
//!   [`ServerMsg::Failed`]; the rest are forwarded byte for byte. The daemon's
//!   replies and pushes come back unchanged.
//! - **`changes`**: the relay opens the far `hub.sock`, writes only its own
//!   [`subscribe_line`] for the project, and forwards the notification lines
//!   down. Nothing the client writes on the stream ever reaches `hub.sock`, so
//!   a remote client cannot inject worker events.
//!
//! - **`put`**: one file a remote window drops on a far terminal. The client
//!   writes its bytes as one framed message; the relay saves them under a
//!   fresh directory in [`RelayServices::put_dir`] and answers the far path,
//!   which the window pastes. Capped at [`MAX_PUT_LEN`].
//!
//! None of them starts the far daemon: `control` and `changes` connect to its
//! sockets and fail the open with [`RelayFrame::OpenError`] when it is not
//! running.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use shelbi_proto::control::{self, ClientMsg, DetachSelector, MutationError, ServerMsg};
use shelbi_proto::{ProtoError, RelayFrame, RelayService};

use super::{
    forward_to_hub, send_out, Links, Peel, RelayServices, SharedOut, DATA_CHUNK, MAX_QUERY_LEN,
};
use crate::error::ClientError;

/// Answers one `query` request payload with one or more response payloads,
/// each handed to the reply callback in order as soon as it is ready (an
/// early answer, then the full one). Runs on the stream's worker thread on
/// the far machine. The relay does not look inside the payloads;
/// `shelbi_app::source::wire` defines them, including which reply is the
/// last.
pub type QueryHandler = Arc<dyn Fn(&[u8], &mut dyn FnMut(Vec<u8>)) + Send + Sync>;

/// The largest file the `put` service takes.
pub const MAX_PUT_LEN: usize = 64 * 1024 * 1024;

/// Why the relay refuses a control message from a remote window, or `None`
/// when it may be forwarded to the far daemon. `project` is the window's own
/// project (the [`RelayService::Control`] it opened).
///
/// Refused (maintainer decision, remote-window plan section 7): quitting Shelbi
/// on the far machine, reloading every far client, detaching every far client,
/// and quitting, or writing the state of, any project but the window's own.
/// Every refused message carries a request id, which the refusal answers.
pub fn control_refusal(msg: &ClientMsg, project: &str) -> Option<(u64, String)> {
    let why = |what: &str| format!("{what} isn't allowed from a remote window");
    match msg {
        ClientMsg::QuitShelbi { request_id } => Some((*request_id, why("Quitting Shelbi"))),
        ClientMsg::ReloadClients { request_id } => Some((*request_id, why("Reloading every client"))),
        ClientMsg::DetachClients {
            request_id,
            selector: DetachSelector::All,
            ..
        } => Some((*request_id, why("Detaching every client"))),
        ClientMsg::QuitProject {
            request_id,
            project: other,
        } if other != project => Some((
            *request_id,
            why(&format!("Quitting project `{other}` from a window on `{project}`")),
        )),
        ClientMsg::WriteState(req) if req.project != project => Some((
            req.request_id,
            why(&format!("Changing project `{}` from a window on `{project}`", req.project)),
        )),
        // Settings writes and the app's requests are the window's own
        // project's.
        ClientMsg::SaveConfig(r) if r.project != project => Some((
            r.request_id,
            why(&format!("Changing project `{}`'s settings from a window on `{project}`", r.project)),
        )),
        ClientMsg::AppRequest(r) if r.project != project => Some((
            r.request_id,
            why(&format!("Changing project `{}` from a window on `{project}`", r.project)),
        )),
        ClientMsg::Hello { .. }
        | ClientMsg::Subscribe { .. }
        | ClientMsg::Mutate(_)
        | ClientMsg::QuitProject { .. }
        | ClientMsg::ReviewSession(_)
        | ClientMsg::WorkspaceSession(_)
        | ClientMsg::ListClients { .. }
        | ClientMsg::DetachClients { .. }
        | ClientMsg::WriteState(_)
        | ClientMsg::SaveConfig(_)
        | ClientMsg::AppRequest(_) => None,
    }
}

/// The one line the `changes` service writes to the far `hub.sock`: the
/// `subscribe` verb for `project`. Shared with the local subscriber
/// (`shelbi_app::changes`) so both speak the same frame.
pub fn subscribe_line(project: &str) -> String {
    let project = serde_json::to_string(project).expect("a string serializes");
    format!("{{\"verb\":\"subscribe\",\"project\":{project}}}\n")
}

/// Frame one query message: `[len: u32 BE][payload]`.
pub(crate) fn frame_message(payload: &[u8]) -> Result<Vec<u8>, ClientError> {
    if payload.len() > MAX_QUERY_LEN {
        return Err(ProtoError::FrameTooLarge(payload.len()).into());
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Read one framed query message from `reader`; `None` on a clean EOF before
/// its first byte.
pub(crate) fn read_message(reader: &mut impl Read) -> Result<Option<Vec<u8>>, ClientError> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match reader.read(&mut len[got..])? {
            0 if got == 0 => return Ok(None),
            0 => return Err(ClientError::UnexpectedEof),
            n => got += n,
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_QUERY_LEN {
        return Err(ProtoError::FrameTooLarge(len).into());
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => ClientError::UnexpectedEof,
        _ => ClientError::Io(e),
    })?;
    Ok(Some(payload))
}

/// Take one whole framed message off the front of `buf`, if it holds one.
/// `Err` for a length past [`MAX_QUERY_LEN`].
fn take_message(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, ()> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_QUERY_LEN {
        return Err(());
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let payload = buf[4..4 + len].to_vec();
    buf.drain(..4 + len);
    Ok(Some(payload))
}

/// One open stream on the relay: where the client's bytes for it go.
pub(crate) enum Link {
    /// A session socket: bytes go straight in (the session reassembles frames).
    Session(UnixStream),
    /// The far `control.sock`, behind the denylist.
    Control(ControlLink),
    /// The far `hub.sock` subscription. Held only to shut it down; the
    /// client's bytes are dropped.
    Changes(UnixStream),
    /// The query worker's inbox.
    Query(Sender<Vec<u8>>),
    /// The `put` worker's inbox.
    Put(Sender<Vec<u8>>),
}

impl Link {
    /// Hand `bytes` the client sent on `stream` to this link. `Err` means the
    /// link is broken and the stream should be closed.
    pub(crate) fn accept(&mut self, stream: u32, bytes: &[u8], out: &SharedOut) -> io::Result<()> {
        match self {
            Link::Session(sock) => sock.write_all(bytes),
            Link::Control(link) => link.accept(stream, bytes, out),
            // One way: the client never writes to hub.sock.
            Link::Changes(_) => Ok(()),
            Link::Query(inbox) | Link::Put(inbox) => inbox
                .send(bytes.to_vec())
                .map_err(|_| io::Error::other("worker stopped")),
        }
    }

    /// Close the link's far end. A query worker stops when its inbox drops.
    pub(crate) fn shutdown(self) {
        match self {
            Link::Session(sock) | Link::Changes(sock) => {
                let _ = sock.shutdown(std::net::Shutdown::Both);
            }
            Link::Control(link) => {
                let _ = link.sock.shutdown(std::net::Shutdown::Both);
            }
            Link::Query(_) | Link::Put(_) => {}
        }
    }
}

/// The `control` service's client-to-daemon half: reassembles the client's
/// control frames, refuses denylisted ones, forwards the rest unchanged.
pub(crate) struct ControlLink {
    sock: UnixStream,
    /// The window's own project, for the `QuitProject` rule.
    project: String,
    /// Client bytes not yet making a whole frame.
    buf: Vec<u8>,
}

impl ControlLink {
    fn accept(&mut self, stream: u32, bytes: &[u8], out: &SharedOut) -> io::Result<()> {
        self.buf.extend_from_slice(bytes);
        loop {
            let (msg, consumed) = match control::decode::<ClientMsg>(&self.buf) {
                Ok(decoded) => decoded,
                Err(ProtoError::Incomplete { .. }) => return Ok(()),
                // A frame the far binary can't read can't be checked, so it is
                // never forwarded: the stream closes instead.
                Err(e) => return Err(io::Error::other(format!("control frame: {e}"))),
            };
            let frame: Vec<u8> = self.buf.drain(..consumed).collect();
            match control_refusal(&msg, &self.project) {
                None => self.sock.write_all(&frame)?,
                Some((request_id, message)) => {
                    let reply = control::encode(&ServerMsg::Failed {
                        request_id,
                        error: MutationError::Backend { message },
                    })
                    .map_err(io::Error::other)?;
                    send_out(out, &RelayFrame::Data { stream, bytes: reply })?;
                }
            }
        }
    }
}

/// Open `service` as `stream`: connect its far end, register its [`Link`],
/// answer `Opened` (or `OpenError`), and start its far-to-client forwarder.
pub(crate) fn open(
    stream: u32,
    service: RelayService,
    services: &RelayServices,
    out: &SharedOut,
    links: &Links,
) {
    let opened = match service {
        RelayService::Query => open_query(stream, services, out, links),
        RelayService::Control { project } => open_control(stream, project, services, links),
        RelayService::Changes { project } => open_changes(stream, &project, services, links),
        RelayService::Put { file } => open_put(stream, &file, services, out, links),
        RelayService::Unknown => Err("unknown relay service".to_string()),
    };
    match opened {
        Ok(forward) => {
            let _ = send_out(out, &RelayFrame::Opened { stream });
            // Start forwarding only after `Opened`, so the client never sees a
            // `Data` for a stream it doesn't know is open yet.
            if let Some(sock) = forward {
                let out = out.clone();
                let links = links.clone();
                std::thread::spawn(move || forward_to_hub(stream, sock, Peel::Raw, out, links));
            }
        }
        Err(error) => {
            let _ = send_out(out, &RelayFrame::OpenError { stream, error });
        }
    }
}

/// A connected far socket and its read half for the forwarder.
fn connect(path: Option<&std::path::Path>, what: &str) -> Result<(UnixStream, UnixStream), String> {
    let path = path.ok_or_else(|| format!("the {what} service isn't available on this relay"))?;
    let sock = UnixStream::connect(path)
        .map_err(|e| format!("the daemon isn't running (connect {}: {e})", path.display()))?;
    let read = sock.try_clone().map_err(|e| format!("clone socket: {e}"))?;
    Ok((sock, read))
}

fn open_control(
    stream: u32,
    project: String,
    services: &RelayServices,
    links: &Links,
) -> Result<Option<UnixStream>, String> {
    let (sock, read) = connect(services.control_socket.as_deref(), "control")?;
    let link = ControlLink {
        sock,
        project,
        buf: Vec::new(),
    };
    links.lock().unwrap().insert(stream, Link::Control(link));
    Ok(Some(read))
}

fn open_changes(
    stream: u32,
    project: &str,
    services: &RelayServices,
    links: &Links,
) -> Result<Option<UnixStream>, String> {
    let (mut sock, read) = connect(services.hub_socket.as_deref(), "changes")?;
    // The only bytes this stream ever writes to hub.sock.
    sock.write_all(subscribe_line(project).as_bytes())
        .map_err(|e| format!("subscribe: {e}"))?;
    links.lock().unwrap().insert(stream, Link::Changes(sock));
    Ok(Some(read))
}

/// A name `put` may save a dropped file as: `file`'s last component, with
/// nothing that could climb or hide it; `drop` when nothing is left.
pub fn put_file_name(file: &str) -> String {
    let base = file.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() {
        "drop".to_string()
    } else {
        clean
    }
}

/// A fresh, private directory under `root` for one dropped file.
fn fresh_put_dir(root: &std::path::Path) -> io::Result<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(root)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = root.join(format!(
        "{:x}-{:x}-{}",
        std::process::id(),
        nanos,
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    Ok(dir)
}

fn open_put(
    stream: u32,
    file: &str,
    services: &RelayServices,
    out: &SharedOut,
    links: &Links,
) -> Result<Option<UnixStream>, String> {
    let root = services
        .put_dir
        .clone()
        .ok_or_else(|| "the put service isn't available on this relay".to_string())?;
    let name = put_file_name(file);
    let (inbox, chunks) = channel::<Vec<u8>>();
    let out = out.clone();
    let worker_links = links.clone();
    std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut want: Option<usize> = None;
        while let Ok(bytes) = chunks.recv() {
            buf.extend_from_slice(&bytes);
            if want.is_none() && buf.len() >= 4 {
                let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                if len > MAX_PUT_LEN {
                    break;
                }
                want = Some(len);
            }
            if want.is_some_and(|len| buf.len() >= 4 + len) {
                break;
            }
        }
        let saved = match want {
            Some(len) if buf.len() >= 4 + len => fresh_put_dir(&root).and_then(|dir| {
                let path = dir.join(&name);
                std::fs::write(&path, &buf[4..4 + len]).map(|_| path)
            }),
            _ => Err(io::Error::other("the file didn't arrive whole")),
        };
        // The answer: the far path, or nothing (the client reads the close).
        if let Ok(path) = saved {
            if let Ok(framed) = frame_message(path.to_string_lossy().as_bytes()) {
                let _ = send_out(&out, &RelayFrame::Data { stream, bytes: framed });
            }
        }
        if worker_links.lock().unwrap().remove(&stream).is_some() {
            let _ = send_out(&out, &RelayFrame::Close { stream });
        }
    });
    links.lock().unwrap().insert(stream, Link::Put(inbox));
    Ok(None)
}

fn open_query(
    stream: u32,
    services: &RelayServices,
    out: &SharedOut,
    links: &Links,
) -> Result<Option<UnixStream>, String> {
    let handler = services
        .query
        .clone()
        .ok_or_else(|| "the query service isn't available on this relay".to_string())?;
    let (inbox, requests) = channel::<Vec<u8>>();
    let out = out.clone();
    let worker_links = links.clone();
    std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        'stream: while let Ok(bytes) = requests.recv() {
            buf.extend_from_slice(&bytes);
            loop {
                let payload = match take_message(&mut buf) {
                    Ok(Some(payload)) => payload,
                    Ok(None) => break,
                    Err(()) => break 'stream,
                };
                // Each reply goes out as soon as the handler gives it. A reply
                // that can't be framed or sent ends the stream.
                let mut sent = true;
                let mut reply = |answer: Vec<u8>| {
                    if !sent {
                        return;
                    }
                    let Ok(framed) = frame_message(&answer) else {
                        sent = false;
                        return;
                    };
                    for piece in framed.chunks(DATA_CHUNK) {
                        let data = RelayFrame::Data {
                            stream,
                            bytes: piece.to_vec(),
                        };
                        if send_out(&out, &data).is_err() {
                            sent = false;
                            return;
                        }
                    }
                };
                // A panicking handler must not leave the client waiting
                // forever: it closes the stream instead.
                let answered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handler(&payload, &mut reply)
                }));
                if answered.is_err() || !sent {
                    break 'stream;
                }
            }
        }
        // Ended by the client (its Close dropped our inbox: nothing to say) or
        // by a bad request (tell the client the stream is gone).
        if worker_links.lock().unwrap().remove(&stream).is_some() {
            let _ = send_out(&out, &RelayFrame::Close { stream });
        }
    });
    links.lock().unwrap().insert(stream, Link::Query(inbox));
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_proto::control::{
        ClientInfo, ClientKind, MutationKind, MutationRequest, StateWrite, StateWriteRequest,
    };

    fn write_state(project: &str) -> ClientMsg {
        ClientMsg::WriteState(StateWriteRequest {
            request_id: 5,
            project: project.into(),
            write: StateWrite::ToggleZen {
                source: "user:palette".into(),
            },
        })
    }

    fn refused(msg: ClientMsg) -> bool {
        control_refusal(&msg, "mine").is_some()
    }

    #[test]
    fn the_denylist_refuses_far_wide_actions_and_other_projects() {
        assert!(refused(ClientMsg::QuitShelbi { request_id: 1 }));
        assert!(refused(ClientMsg::ReloadClients { request_id: 2 }));
        assert!(refused(ClientMsg::DetachClients {
            request_id: 3,
            selector: DetachSelector::All,
            reason: "x".into(),
        }));
        assert!(refused(ClientMsg::QuitProject {
            request_id: 4,
            project: "theirs".into(),
        }));
        assert!(refused(write_state("theirs")), "another project's state is off limits");
        let (id, why) = control_refusal(&ClientMsg::QuitShelbi { request_id: 9 }, "mine").unwrap();
        assert_eq!(id, 9);
        assert!(why.contains("remote window"), "{why}");
    }

    #[test]
    fn the_denylist_lets_the_windows_own_work_through() {
        assert!(!refused(ClientMsg::Hello {
            protocol: 6,
            client_version: "0".into(),
        }));
        assert!(!refused(ClientMsg::QuitProject {
            request_id: 1,
            project: "mine".into(),
        }));
        assert!(!refused(ClientMsg::ListClients { request_id: 2 }));
        assert!(!refused(ClientMsg::DetachClients {
            request_id: 3,
            selector: DetachSelector::One {
                client_id: "c".into(),
            },
            reason: "x".into(),
        }));
        assert!(!refused(ClientMsg::Subscribe {
            info: Some(ClientInfo {
                client_id: "c".into(),
                kind: ClientKind::Remote,
                host: "h".into(),
                pid: None,
                project: Some("mine".into()),
            }),
        }));
        assert!(!refused(ClientMsg::Mutate(MutationRequest {
            request_id: 4,
            project: "mine".into(),
            id: "t-1".into(),
            expected: None,
            kind: MutationKind::Unassign,
        })));
        assert!(!refused(write_state("mine")), "the window's own project state is its to write");
    }

    #[test]
    fn the_subscribe_line_escapes_the_project() {
        assert_eq!(subscribe_line("demo"), "{\"verb\":\"subscribe\",\"project\":\"demo\"}\n");
        assert_eq!(
            subscribe_line("a\"b"),
            "{\"verb\":\"subscribe\",\"project\":\"a\\\"b\"}\n"
        );
    }

    #[test]
    fn query_messages_frame_and_unframe() {
        let framed = frame_message(b"{\"q\":1}").unwrap();
        let mut buf = framed.clone();
        buf.extend_from_slice(&framed[..3]);
        assert_eq!(take_message(&mut buf), Ok(Some(b"{\"q\":1}".to_vec())));
        assert_eq!(take_message(&mut buf), Ok(None));
        let mut reader = io::Cursor::new(framed);
        assert_eq!(read_message(&mut reader).unwrap(), Some(b"{\"q\":1}".to_vec()));
        assert_eq!(read_message(&mut reader).unwrap(), None);
        let mut huge = ((MAX_QUERY_LEN + 1) as u32).to_be_bytes().to_vec();
        assert_eq!(take_message(&mut huge), Err(()));
    }
}
