//! The session socket server: one Unix socket, many clients.
//!
//! The wire format is `shelbi-proto`'s length-prefixed frames. This module
//! implements **enough of the frozen core** to exercise a session end to end:
//! `hello`, `attach` (live output; full replay is `rt-replay`'s job), `input`,
//! `resize`, `snapshot`, and `kill`. Additive capabilities and attach-replay are
//! owned by `rt-protocol-client` / `rt-replay`; this is the server seam they
//! build on, kept deliberately small.
//!
//! Each client gets two threads: a reader (this module's [`serve_client`]) that
//! decodes request frames, and a writer fed by a bounded channel so a slow client
//! never blocks the PTY reader — if its buffer fills it is dropped (the plan's
//! backpressure rule), to be recovered by a fresh attach.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};

use shelbi_proto::{Frame, Hello, Resize, SnapshotData};

use crate::session::Shared;

/// How many output frames a lagging client may queue before it is dropped.
const CLIENT_BUFFER_FRAMES: usize = 1024;

/// A registered client's output channel.
struct Client {
    id: u64,
    tx: SyncSender<Vec<u8>>,
}

/// The set of attached clients the PTY reader broadcasts output to.
#[derive(Default)]
pub struct ClientRegistry {
    clients: Mutex<Vec<Client>>,
    next_id: AtomicU64,
}

impl ClientRegistry {
    /// Register a client, returning its id (used to deregister on disconnect).
    fn register(&self, tx: SyncSender<Vec<u8>>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.clients.lock().unwrap().push(Client { id, tx });
        id
    }

    /// Remove a client by id.
    fn deregister(&self, id: u64) {
        self.clients.lock().unwrap().retain(|c| c.id != id);
    }

    /// Broadcast already-encoded frame bytes to every attached client, dropping
    /// any whose buffer is full (it will recover on a fresh attach).
    pub fn broadcast(&self, frame_bytes: &[u8]) {
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|c| c.tx.try_send(frame_bytes.to_vec()).is_ok());
    }

    /// Number of attached clients (for tests / introspection).
    pub fn len(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// Whether any client is attached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Serve one client connection to completion (returns when the client
/// disconnects). Spawns a writer thread for pushed output and handles request
/// frames on this thread.
pub fn serve_client(stream: UnixStream, shared: Arc<Shared>) {
    let (tx, rx) = sync_channel::<Vec<u8>>(CLIENT_BUFFER_FRAMES);

    // Writer thread: drain the channel to the socket.
    let mut write_half = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let writer = std::thread::spawn(move || {
        for frame in rx.iter() {
            if write_half.write_all(&frame).is_err() {
                break;
            }
        }
    });

    let mut attached_id: Option<u64> = None;
    let _ = read_loop(stream, &shared, &tx, &mut attached_id);
    if let Some(id) = attached_id {
        shared.clients.deregister(id);
    }
    // Dropping `tx` ends the writer thread.
    drop(tx);
    let _ = writer.join();
}

/// Read and dispatch request frames until the client disconnects or errors.
fn read_loop(
    mut stream: UnixStream,
    shared: &Arc<Shared>,
    tx: &SyncSender<Vec<u8>>,
    attached_id: &mut Option<u64>,
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        // Drain any whole frames already buffered.
        loop {
            match Frame::decode(&buf) {
                Ok((frame, consumed)) => {
                    buf.drain(..consumed);
                    handle_frame(frame, shared, tx, attached_id);
                }
                Err(shelbi_proto::ProtoError::Incomplete { .. }) => break,
                Err(_) => {
                    // A malformed/unknown frame from a peer: stop serving it
                    // rather than risk desyncing the stream.
                    return Ok(());
                }
            }
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(()); // clean disconnect
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn handle_frame(
    frame: Frame,
    shared: &Arc<Shared>,
    tx: &SyncSender<Vec<u8>>,
    attached_id: &mut Option<u64>,
) {
    match frame {
        Frame::Hello(hello) => {
            // Adopt the client's reported colors so later color queries use real
            // values instead of the dark default.
            if let Some(colors) = hello.colors {
                shared.colors.set(
                    (colors.foreground.r, colors.foreground.g, colors.foreground.b),
                    (colors.background.r, colors.background.g, colors.background.b),
                );
            }
            // Reply with the session's hello (frozen core; no colors of its own).
            let reply = Hello {
                protocol_version: shelbi_proto::PROTOCOL_VERSION,
                colors: None,
                capabilities: vec![],
            };
            send(tx, &Frame::Hello(reply));
        }
        Frame::Attach(_attach) => {
            // Live output only; replay reconstruction is `rt-replay`. Register so
            // the PTY reader starts broadcasting to this client.
            if attached_id.is_none() {
                *attached_id = Some(shared.clients.register(tx.clone()));
            }
        }
        Frame::Input(input) => {
            shared.write_to_child(&input.data);
        }
        Frame::Resize(Resize { cols, rows }) => {
            shared.resize(cols, rows);
        }
        Frame::Snapshot(req) => {
            let text = shared.snapshot_text(req.history_lines);
            send(tx, &Frame::SnapshotData(SnapshotData { text }));
        }
        Frame::Kill(kill) => {
            shared.kill_child_group(kill.signal);
        }
        // Server never receives these; ignore.
        Frame::Output(_) | Frame::SnapshotData(_) | Frame::Exited(_) => {}
    }
}

/// Encode `frame` and push it to the client's writer channel (best-effort).
fn send(tx: &SyncSender<Vec<u8>>, frame: &Frame) {
    if let Ok(bytes) = frame.encode() {
        let _ = tx.try_send(bytes);
    }
}
