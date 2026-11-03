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

/// Answers one `query` request payload with one response payload. Runs on the
/// stream's worker thread on the far machine. The relay does not look inside
/// either payload; `shelbi_app::source::wire` defines them.
pub type QueryHandler = Arc<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>;

/// Why the relay refuses a control message from a remote window, or `None`
/// when it may be forwarded to the far daemon. `project` is the window's own
/// project (the [`RelayService::Control`] it opened).
///
/// Refused (maintainer decision, remote-window plan section 7): quitting Shelbi
/// on the far machine, reloading every far client, detaching every far client,
/// and quitting any project but the window's own. Every refused message
/// carries a request id, which the refusal answers.
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
        ClientMsg::Hello { .. }
        | ClientMsg::Subscribe { .. }
        | ClientMsg::Mutate(_)
        | ClientMsg::QuitProject { .. }
        | ClientMsg::ReviewSession(_)
        | ClientMsg::WorkspaceSession(_)
        | ClientMsg::ListClients { .. }
        | ClientMsg::DetachClients { .. } => None,
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
            Link::Query(inbox) => inbox
                .send(bytes.to_vec())
                .map_err(|_| io::Error::other("query worker stopped")),
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
            Link::Query(_) => {}
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
                // A panicking handler must not leave the client waiting
                // forever: it closes the stream instead.
                let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&payload)));
                let Ok(answer) = answer else { break 'stream };
                let Ok(framed) = frame_message(&answer) else { break 'stream };
                for piece in framed.chunks(DATA_CHUNK) {
                    let data = RelayFrame::Data {
                        stream,
                        bytes: piece.to_vec(),
                    };
                    if send_out(&out, &data).is_err() {
                        return;
                    }
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
    use shelbi_proto::control::{ClientInfo, ClientKind, MutationKind, MutationRequest};

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
