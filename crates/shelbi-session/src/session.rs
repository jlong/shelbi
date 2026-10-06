//! The `shelbi __session` process body: own one PTY and one headless emulator,
//! answer terminal queries, serve clients, and on child exit write `exit.json` +
//! `final.txt` and quit.
//!
//! This is the long-lived, already-detached process (the detaching is done by
//! [`crate::spawn::spawn_detached`] before `exec`). It:
//!
//! 1. holds the lifetime [`lock`](crate::lock) and writes `meta.json`,
//! 2. opens a PTY with `portable-pty` and spawns the child in it with the
//!    explicit, scrubbed login environment,
//! 3. runs a reader thread that feeds the emulator, answers terminal queries off
//!    it (the [`responder`](crate::responder)), retains recent bytes, optionally
//!    logs raw output, surfaces title/bell events, and broadcasts sequenced
//!    output to attached clients,
//! 4. serves clients on a Unix socket (the full session protocol — see
//!    [`crate::transport`]), with a debounced PTY resize that follows the most
//!    recently active client and a keepalive ping, and
//! 5. on child exit (or SIGTERM/SIGHUP) kills the child's **process group**,
//!    pushes the `exited` event, writes `exit.json` and `final.txt`, and exits.
//!
//! ## Output ordering and sizing
//!
//! Every output chunk and every in-band `resized` marker is assigned a sequence
//! number under one gate ([`Shared::output`]) and enqueued to clients while the
//! gate is held, so the two travel in one totally ordered stream. The PTY takes
//! the size of the most recently active client (last to send input), debounced
//! by [`RESIZE_DEBOUNCE`] so a drag of the window does not thrash the child.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use shelbi_proto::{ExtFrame, Frame, Output, Resized};

use crate::emulator::{EmuEvent, Emulator};
use crate::history::{RawLog, RawRing};
use crate::layout::SessionPaths;
use crate::lock::SessionLock;
use crate::meta::{ExitRecord, Meta};
use crate::responder::{ColorState, CursorSource, FixedCursor, KittyFlags, Responder};
use crate::transport::{info_data, serve_client, ClientRegistry};

/// Lines of scrollback included (above the last screen) in `final.txt`.
const FINAL_HISTORY_LINES: usize = 1000;

/// How long a size must hold steady before the PTY is resized to it. Coalesces a
/// burst of resizes (a window drag) into one child resize.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(20);

/// How often the session sends a keepalive `ping` to connected clients.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// Grace period after the child exits for client writers to flush the pushed
/// `exited` event before the process tears down.
const EXIT_FLUSH_GRACE: Duration = Duration::from_millis(100);

/// Set by the SIGTERM/SIGHUP handler to ask the main loop to shut down.
static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_sig: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
}

/// Arguments for [`run`] — what `shelbi __session` was launched with.
#[derive(Debug, Clone)]
pub struct RunArgs {
    /// Short directory id (computed by the spawner).
    pub id: String,
    /// Readable session name, recorded in `meta.json`.
    pub name: String,
    /// Working directory for the child.
    pub cwd: PathBuf,
    /// Initial screen size.
    pub cols: u16,
    /// Initial screen size.
    pub rows: u16,
    /// Task id this session serves, if any.
    pub task: Option<String>,
    /// Whether the project enabled the full raw output log.
    pub raw_output_log: bool,
    /// The child program and its arguments.
    pub child_argv: Vec<String>,
    /// Whether this session acts as a daemon watchdog (see
    /// [`crate::daemon_watchdog`]). True for a real `shelbi __session`; set
    /// `false` by in-process tests that drive [`run`] on a thread. The watchdog
    /// loops for the life of the process reading the global `$SHELBI_HOME`, so a
    /// test that mutates that env var per case (the standard per-test temp-home
    /// pattern) would otherwise race the watchdog's reads — a data race that
    /// stays hidden while a short run finishes before the first ~15s tick, but
    /// surfaces under a long, loaded `cargo test --workspace` where a tick lands
    /// mid-suite. A test session also has no daemon to keep alive.
    pub manage_daemon: bool,
}

/// The monotonic output-stream gate: it hands out a sequence number for each
/// output chunk and in-band resize marker, held across the enqueue so the two
/// travel in one totally ordered stream.
#[derive(Default)]
struct OutputGate {
    next_seq: u64,
}

/// Which client's size the PTY should take, and the debounce bookkeeping.
#[derive(Default)]
struct Sizing {
    /// The last client to send input; the PTY follows its viewport.
    active: Option<u64>,
    /// Each connected client's last reported viewport.
    per_client: HashMap<u64, (u16, u16)>,
    /// The size the debouncer should settle on, if different from `applied`.
    target: Option<(u16, u16)>,
    /// Bumped on every resize request so the debouncer can tell a burst from a
    /// settled value.
    generation: u64,
    /// The size currently applied to the PTY.
    applied: (u16, u16),
    /// Set on teardown so the debounce thread exits.
    closed: bool,
}

/// State shared between the PTY reader thread, the resize debouncer, the
/// keepalive timer, and the per-client threads.
pub struct Shared {
    /// The authoritative emulator (cursor source for query replies, text source
    /// for snapshots / `final.txt`, and the title/mode source for `info`).
    pub emu: Mutex<Emulator>,
    /// Default colors answered to OSC 10/11 queries (dark until a client reports).
    pub colors: ColorState,
    /// Active kitty keyboard flags (tracked by the responder).
    pub kitty: KittyFlags,
    /// Recent raw output bytes.
    pub ring: Mutex<RawRing>,
    /// The output-stream gate (sequence numbers; ordered broadcast).
    output: Mutex<OutputGate>,
    /// Size arbitration + debounce state.
    sizing: Mutex<Sizing>,
    /// Wakes the resize debouncer.
    sizing_cv: Condvar,
    /// The session's metadata (`meta.json`), mutable via `set-meta`.
    pub meta: Mutex<Meta>,
    /// Where `meta.json` and friends live.
    paths: SessionPaths,
    /// The PTY master, kept for `resize`.
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// The child's stdin (PTY master write side). Responder replies and client
    /// input/paste both go here; the mutex keeps each write whole (input
    /// arbitration).
    writer: Mutex<Box<dyn Write + Send>>,
    /// The child's process group id, for group kill.
    child_pgid: libc::pid_t,
    /// Connected clients the reader broadcasts output to.
    pub clients: ClientRegistry,
}

impl Shared {
    /// Write raw bytes to the child (responder replies, client input, paste).
    /// Each call is whole and ordered relative to others (input arbitration).
    pub fn write_to_child(&self, bytes: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    /// Deliver `text` as a paste, wrapping it in bracketed-paste markers when the
    /// program enabled that mode, and writing it whole against other input.
    pub fn paste(&self, text: &str) {
        let bracketed = self.emu.lock().unwrap().bracketed_paste_active();
        let mut bytes = Vec::with_capacity(text.len() + 12);
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(text.as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        self.write_to_child(&bytes);
    }

    /// Render the screen (optionally with `history_lines` of scrollback) to text.
    pub fn snapshot_text(&self, history_lines: Option<u32>) -> String {
        let emu = self.emu.lock().unwrap();
        match history_lines {
            Some(n) => emu.screen_with_history(n as usize),
            None => emu.visible_text(),
        }
    }

    /// The [`InfoData`](shelbi_proto::InfoData) reply for an `info` request.
    pub fn info_data(&self) -> shelbi_proto::InfoData {
        info_data(self)
    }

    /// Update `meta.json`: set `name` and/or `task` (`Some("")` clears the task),
    /// leaving a `None` field unchanged. Best-effort write — a failure is logged
    /// and does not take down the session.
    pub fn set_meta(&self, name: Option<String>, task: Option<String>) {
        let json = {
            let mut meta = self.meta.lock().unwrap();
            if let Some(name) = name {
                meta.name = name;
            }
            if let Some(task) = task {
                meta.task = if task.is_empty() { None } else { Some(task) };
            }
            meta.to_json()
        };
        if let Ok(json) = json {
            let _ = std::fs::write(self.paths.meta(), json);
        }
    }

    /// Subscribe a client to the output stream, gaplessly: under the output gate
    /// (held across the emulator read, so no output slips between the replay and
    /// the subscription — see the lock-order note on [`Shared::emit_and_feed`])
    /// send the initial attach [`Resync`](shelbi_proto::Resync) — a full replay
    /// of the emulator's state — then mark it attached. Live output then resumes
    /// at exactly `seq`, with no gap or duplicate.
    pub fn attach(&self, ch: &Arc<crate::transport::ClientChannel>) {
        let gate = self.output.lock().unwrap();
        if ch.wants_resync() {
            let seq = gate.next_seq;
            let replay = self.emu.lock().unwrap().replay();
            if let Ok(bytes) = ExtFrame::Resync(shelbi_proto::Resync { seq, replay }).encode() {
                ch.enqueue_priority(&bytes);
            }
        }
        ch.mark_attached(true);
        drop(gate);
    }

    /// Unsubscribe a client from the output stream, dropping its queued output.
    pub fn detach(&self, ch: &Arc<crate::transport::ClientChannel>) {
        ch.mark_attached(false);
        ch.clear();
    }

    /// Record a client sending input: it becomes the active (sizing) client, and
    /// the PTY is asked to follow its viewport.
    pub fn on_input(&self, client_id: u64) {
        let mut st = self.sizing.lock().unwrap();
        if st.active != Some(client_id) {
            st.active = Some(client_id);
            if let Some(&size) = st.per_client.get(&client_id) {
                request_resize(&mut st, &self.sizing_cv, size);
            }
        }
    }

    /// Record a client's reported viewport. If it is (or becomes, when none is)
    /// the active client, the PTY is asked to follow it.
    pub fn on_resize(&self, client_id: u64, cols: u16, rows: u16) {
        let mut st = self.sizing.lock().unwrap();
        st.per_client.insert(client_id, (cols, rows));
        if st.active.is_none() || st.active == Some(client_id) {
            request_resize(&mut st, &self.sizing_cv, (cols, rows));
        }
    }

    /// Forget a disconnected client's sizing state.
    pub fn forget_client(&self, client_id: u64) {
        let mut st = self.sizing.lock().unwrap();
        st.per_client.remove(&client_id);
        if st.active == Some(client_id) {
            st.active = None;
        }
    }

    /// Resize the PTY and emulator, then announce the new size: an in-band
    /// [`Resized`] marker in the output stream (sequenced, so client emulators
    /// reflow at the same point) and an out-of-band `event-resized`.
    fn apply_resize(&self, cols: u16, rows: u16) {
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        // Resize the emulator and emit the in-band marker under the output gate
        // (lock order gate -> emu, as in `emit_and_feed`), so the emulator
        // reflows at exactly the `seq` the marker carries and output/resize stay
        // totally ordered.
        {
            let mut gate = self.output.lock().unwrap();
            self.emu.lock().unwrap().resize(cols, rows);
            let seq = gate.next_seq;
            gate.next_seq += 1;
            if self.clients.has_attached() {
                if let Ok(bytes) = ExtFrame::Resized(Resized { seq, cols, rows }).encode() {
                    self.clients.enqueue_inband_resized(&bytes);
                }
            }
        }
        // Out-of-band event.
        self.clients.push_event_resized(cols, rows);
    }

    /// Feed one rest-aligned chunk of PTY output into the emulator and emit it as
    /// one sequenced [`Output`] frame, **atomically under the output gate** (held
    /// across the emulator feed). Returns the post-feed cursor (for the
    /// [`responder`](crate::responder)) and the UI events the chunk produced.
    ///
    /// The gate is held across the feed so that [`Shared::attach`] and
    /// [`Shared::resync_base`], which read `(replay, next_seq)` under the same
    /// gate, always observe an emulator reflecting exactly the frames already
    /// emitted — the invariant that makes the replay/live boundary gapless and
    /// duplicate-free. The reader only ever feeds rest-aligned chunks (see
    /// [`crate::output_split`]), so a frame edge never lands inside a sequence
    /// and the emulator's state matches the frame boundaries exactly.
    ///
    /// Lock order is **gate before emu**, shared with `attach`, `resync_base`,
    /// and `apply_resize`; nothing acquires them in the opposite order, so there
    /// is no deadlock.
    fn emit_and_feed(&self, data: &[u8]) -> ((u16, u16), Vec<EmuEvent>) {
        let mut gate = self.output.lock().unwrap();
        let seq = gate.next_seq;
        gate.next_seq += 1;
        let result = {
            let mut emu = self.emu.lock().unwrap();
            emu.feed(data);
            (emu.cursor_1based(), emu.drain_events())
        };
        if self.clients.has_attached() {
            if let Ok(bytes) = Frame::Output(Output {
                seq,
                data: data.to_vec(),
            })
            .encode()
            {
                self.clients.enqueue_output(&bytes);
            }
        }
        result
    }

    /// The `(resume_seq, replay)` a backpressure [`Resync`](shelbi_proto::Resync)
    /// recovers a dropped client with: the sequence the next output will carry,
    /// plus a full replay of the emulator's current state. Read under the output
    /// gate held across the emulator read (lock order **gate before emu**, as in
    /// [`Shared::emit_and_feed`]), so the replay reflects exactly the frames with
    /// `seq < resume_seq` and live output resumes with no gap or duplicate.
    pub fn resync_base(&self) -> (u64, Vec<u8>) {
        let gate = self.output.lock().unwrap();
        let seq = gate.next_seq;
        let replay = self.emu.lock().unwrap().replay();
        drop(gate);
        (seq, replay)
    }

    /// Signal the child's whole process group (default SIGTERM).
    ///
    /// Only ever signals the child's **own** group. `portable-pty` `setsid`s the
    /// child into a fresh session, so its pgid equals its pid and differs from
    /// ours; we refuse pgid 0/1 (every process / init) and our own group so a
    /// failed-to-detach child can never take the session — or a test harness —
    /// down with it.
    pub fn kill_child_group(&self, signal: Option<i32>) {
        let sig = signal.unwrap_or(libc::SIGTERM);
        if self.child_pgid <= 1 {
            return;
        }
        let own_pgid = unsafe { libc::getpgid(0) };
        if self.child_pgid == own_pgid {
            return;
        }
        unsafe {
            libc::killpg(self.child_pgid, sig);
        }
    }
}

/// Request that the PTY settle on `size`, waking the debouncer. A no-op if it
/// already matches the applied size.
fn request_resize(st: &mut Sizing, cv: &Condvar, size: (u16, u16)) {
    if st.applied == size && st.target.is_none() {
        return;
    }
    st.target = Some(size);
    st.generation += 1;
    cv.notify_one();
}

/// Run the session to completion. Returns when the child has exited (or a
/// terminating signal was received) and `exit.json` / `final.txt` are written.
pub fn run(args: RunArgs) -> Result<()> {
    let paths = SessionPaths::resolve(&args.id)?;
    paths.check_socket_fits()?;
    std::fs::create_dir_all(&paths.dir)
        .with_context(|| format!("creating session dir {}", paths.dir.display()))?;

    // Hold the lifetime lock for as long as this function runs. Dropping it (on
    // any return path, including a panic unwinding to the caller) marks the
    // session dead.
    let _lock = SessionLock::acquire(&paths.lock())?;

    let meta = Meta {
        id: args.id.clone(),
        name: args.name.clone(),
        argv: args.child_argv.clone(),
        cwd: args.cwd.clone(),
        task: args.task.clone(),
        launched_at: chrono::Utc::now().to_rfc3339(),
        protocol_version: shelbi_proto::PROTOCOL_VERSION,
    };
    write_meta(&paths, &meta)?;

    install_signal_handlers();

    // Watch the hub daemon: with the service units retired, an open project's
    // sessions are what bring a crashed daemon back (see `daemon_watchdog`).
    // In-process tests opt out (`manage_daemon: false`): the watchdog's
    // long-lived env reads would race their per-test `$SHELBI_HOME` mutation.
    if args.manage_daemon {
        crate::daemon_watchdog::spawn(&args.name);
    }

    // --- PTY + child -----------------------------------------------------
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: args.rows,
            cols: args.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow::anyhow!("openpty failed: {e}"))?;

    let mut cmd = CommandBuilder::new(&args.child_argv[0]);
    cmd.args(&args.child_argv[1..]);
    cmd.cwd(&args.cwd);
    // Explicit environment: never inherit the launcher's. The scrubbed login
    // environment with Shelbi's own TERM variables.
    cmd.env_clear();
    for (k, v) in shelbi_core::session_child_env() {
        cmd.env(k, v);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow::anyhow!("spawning child failed: {e}"))?;
    // Our copy of the slave is no longer needed; drop it so the PTY collapses
    // cleanly when the child exits.
    drop(pair.slave);

    let child_pid = child.process_id().unwrap_or(0) as libc::pid_t;
    // portable-pty puts the child in its own session (setsid), so pgid == pid.
    let child_pgid = if child_pid > 0 {
        unsafe { libc::getpgid(child_pid) }
    } else {
        -1
    };

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| anyhow::anyhow!("cloning PTY reader failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| anyhow::anyhow!("taking PTY writer failed: {e}"))?;

    let shared = Arc::new(Shared {
        emu: Mutex::new(Emulator::new(args.cols, args.rows)),
        colors: ColorState::default(),
        kitty: KittyFlags::default(),
        ring: Mutex::new(RawRing::default()),
        output: Mutex::new(OutputGate::default()),
        sizing: Mutex::new(Sizing {
            applied: (args.cols, args.rows),
            ..Sizing::default()
        }),
        sizing_cv: Condvar::new(),
        meta: Mutex::new(meta),
        paths: paths.clone(),
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        child_pgid,
        clients: ClientRegistry::default(),
    });

    // Optional raw output log (owned by the reader thread).
    let raw_log = if args.raw_output_log {
        RawLog::enabled(&paths.raw_log()).with_context(|| "enabling raw output log")?
    } else {
        RawLog::disabled()
    };

    // --- background threads ---------------------------------------------
    spawn_reader_thread(shared.clone(), reader, raw_log);
    spawn_resize_debouncer(shared.clone());
    spawn_keepalive_thread(shared.clone());

    // --- socket server ---------------------------------------------------
    let sock_path = paths.sock();
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding session socket {}", sock_path.display()))?;
    spawn_accept_thread(shared.clone(), listener);

    // --- wait for the child ----------------------------------------------
    let exit = wait_for_child(&mut *child, &shared);

    // --- teardown --------------------------------------------------------
    // Reap the whole group in case the child left anything behind.
    shared.kill_child_group(Some(libc::SIGTERM));

    // Push the frozen-core `exited` event, then give writers a moment to flush.
    shared.clients.broadcast_exited(shelbi_proto::Exited {
        code: exit.code,
        signal: exit.signal,
        reason: exit.reason.clone(),
    });
    thread::sleep(EXIT_FLUSH_GRACE);

    // Stop the debouncer and wind down client writers.
    {
        let mut st = shared.sizing.lock().unwrap();
        st.closed = true;
        shared.sizing_cv.notify_all();
    }
    shared.clients.close_all();

    write_final(&paths, &shared);
    write_exit(&paths, &exit)?;
    let _ = std::fs::remove_file(&sock_path);
    Ok(())
}

/// The outcome of waiting for the child.
struct ChildExit {
    code: Option<i32>,
    signal: Option<i32>,
    reason: Option<String>,
}

fn wait_for_child(
    child: &mut (dyn portable_pty::Child + Send + Sync),
    shared: &Arc<Shared>,
) -> ChildExit {
    loop {
        if TERMINATE.load(Ordering::SeqCst) {
            shared.kill_child_group(Some(libc::SIGTERM));
            thread::sleep(Duration::from_millis(100));
            shared.kill_child_group(Some(libc::SIGKILL));
            let _ = child.wait();
            return ChildExit {
                code: None,
                signal: Some(libc::SIGTERM),
                reason: Some("session received a terminating signal".to_string()),
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return ChildExit {
                    code: Some(status.exit_code() as i32),
                    signal: None,
                    reason: None,
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                return ChildExit {
                    code: None,
                    signal: None,
                    reason: Some(format!("error waiting for child: {e}")),
                };
            }
        }
    }
}

/// Read the PTY master forever: split the stream at parser-rest boundaries, feed
/// the emulator, answer queries, surface title/bell events, retain bytes,
/// optionally log, and emit sequenced output. Runs until the PTY closes (child
/// exit), after which the thread ends.
fn spawn_reader_thread(shared: Arc<Shared>, mut reader: Box<dyn Read + Send>, mut raw_log: RawLog) {
    thread::spawn(move || {
        let mut responder = Responder::new(shared.kitty.clone(), shared.colors.clone());
        // Split the live stream only where the parser is at rest, so every
        // output frame edge (and the replay/live split a client resumes at)
        // falls at Ground, never inside a sequence or a UTF-8 char.
        let mut splitter = crate::output_split::RestSplitter::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let piece = splitter.push(&chunk[..n]);
            if !piece.is_empty() {
                process_output(&shared, &mut responder, &mut raw_log, &piece);
            }
        }
        // EOF: release any held trailing partial so no bytes are dropped (the
        // child is gone, nothing more can complete the sequence).
        let tail = splitter.flush();
        if !tail.is_empty() {
            process_output(&shared, &mut responder, &mut raw_log, &tail);
        }
    });
}

/// Process one rest-aligned chunk of child output: emit it as a sequenced frame
/// while feeding the emulator (atomically, so a concurrent attach/resync stays
/// gapless), answer any terminal queries it contains, surface title/bell, and
/// retain/log the bytes.
fn process_output(
    shared: &Arc<Shared>,
    responder: &mut Responder,
    raw_log: &mut RawLog,
    data: &[u8],
) {
    // Feed the emulator and emit the output frame atomically under the output
    // gate; the returned cursor is current, so the responder's replies are too.
    let (cursor, events) = shared.emit_and_feed(data);

    let answers = responder.scan(data, &FixedCursor(cursor.0, cursor.1));
    for answer in answers {
        shared.write_to_child(&answer.reply);
    }

    // Surface title/bell as pushed events.
    for event in events {
        match event {
            EmuEvent::Title(title) => shared.clients.push_event_title(&title),
            EmuEvent::Bell => shared.clients.push_event_bell(),
        }
    }

    // Retain recent bytes and (optionally) log the raw stream.
    shared.ring.lock().unwrap().push(data);
    raw_log.write(data);
}

/// Apply the most-recently-active client's size to the PTY, debounced so a burst
/// of resizes settles into one child resize.
fn spawn_resize_debouncer(shared: Arc<Shared>) {
    thread::spawn(move || loop {
        let gen0 = {
            let mut st = shared.sizing.lock().unwrap();
            while st.target.is_none() && !st.closed {
                st = shared.sizing_cv.wait(st).unwrap();
            }
            if st.closed {
                return;
            }
            st.generation
        };

        thread::sleep(RESIZE_DEBOUNCE);

        let settled = {
            let mut st = shared.sizing.lock().unwrap();
            if st.closed {
                return;
            }
            if st.generation != gen0 {
                // A newer request arrived during the wait; re-debounce.
                continue;
            }
            match st.target.take() {
                Some(size) if size != st.applied => {
                    st.applied = size;
                    Some(size)
                }
                _ => None,
            }
        };

        if let Some((cols, rows)) = settled {
            shared.apply_resize(cols, rows);
        }
    });
}

/// Periodically ping connected clients so a dead connection is noticed.
fn spawn_keepalive_thread(shared: Arc<Shared>) {
    thread::spawn(move || loop {
        thread::sleep(KEEPALIVE_INTERVAL);
        if shared.sizing.lock().unwrap().closed {
            return;
        }
        shared.clients.send_keepalives();
    });
}

/// Backoff after an `accept()` that failed on resource exhaustion
/// (`EMFILE`/`ENFILE`), so the loop doesn't hot-spin while file descriptors free
/// up. Short enough that a client reconnecting in that window still lands.
const ACCEPT_RETRY_BACKOFF: Duration = Duration::from_millis(50);

/// Accept client connections, one serving thread each.
///
/// The accept loop must never quietly abandon the listener while the process
/// lives on: a session that stops accepting connections but keeps running is the
/// "alive but not listening" zombie — discovery still reports it live (its lock
/// is held), so every client keeps choosing it and none can attach, stranding
/// them on "Connecting…" forever. So a **transient** `accept()` error (a client
/// that aborted between connect and accept, an interrupted syscall, or a
/// momentary fd exhaustion) is logged and retried, never fatal. A **fatal** one
/// (the listener fd itself is unusable and can't recover) asks the main loop to
/// tear the whole session down, so the process *exits* and releases its lock —
/// discovery then correctly reports it dead and supervision relaunches it. Keep
/// the listener, or exit: never linger as a zombie.
fn spawn_accept_thread(shared: Arc<Shared>, listener: UnixListener) {
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let shared = shared.clone();
                    thread::spawn(move || serve_client(stream, shared));
                }
                Err(e) if is_transient_accept_error(&e) => {
                    // The listener is still valid; keep serving. Back off only on
                    // resource exhaustion so the loop doesn't spin while fds free.
                    if is_resource_exhaustion(&e) {
                        thread::sleep(ACCEPT_RETRY_BACKOFF);
                    }
                    continue;
                }
                Err(_) => {
                    // The listener is unrecoverable. Rather than drop it and leave
                    // a zombie (process up, socket refusing), ask the main loop to
                    // shut the session down cleanly so it exits and is relaunched.
                    TERMINATE.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    });
}

/// Whether an `accept()` error is transient — the listener is still valid and
/// the loop should keep serving. A connection aborted between connect and accept
/// (`ECONNABORTED`), an interrupted syscall (`EINTR`), a spurious `WouldBlock`,
/// or a momentary descriptor exhaustion (`EMFILE`/`ENFILE`) must never take the
/// listener down.
fn is_transient_accept_error(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    if matches!(e.kind(), Interrupted | ConnectionAborted | WouldBlock) {
        return true;
    }
    is_resource_exhaustion(e)
}

/// Whether an `accept()` error is file-descriptor exhaustion (`EMFILE` /
/// `ENFILE`) — transient (fds free up) but worth a brief backoff so the loop
/// doesn't hot-spin. These have no stable [`std::io::ErrorKind`], so match the
/// raw OS error.
fn is_resource_exhaustion(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EMFILE) | Some(libc::ENFILE))
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, on_terminate as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_terminate as *const () as libc::sighandler_t);
    }
}

fn write_meta(paths: &SessionPaths, meta: &Meta) -> Result<()> {
    let json = meta.to_json().context("serializing meta.json")?;
    std::fs::write(paths.meta(), json)
        .with_context(|| format!("writing {}", paths.meta().display()))?;
    Ok(())
}

fn write_final(paths: &SessionPaths, shared: &Arc<Shared>) {
    let text = shared
        .emu
        .lock()
        .unwrap()
        .screen_with_history(FINAL_HISTORY_LINES);
    let _ = std::fs::write(paths.final_txt(), text);
}

fn write_exit(paths: &SessionPaths, exit: &ChildExit) -> Result<()> {
    let rec = ExitRecord {
        code: exit.code,
        signal: exit.signal,
        exited_at: chrono::Utc::now().to_rfc3339(),
        reason: exit.reason.clone(),
    };
    let json = rec.to_json().context("serializing exit.json")?;
    std::fs::write(paths.exit(), json)
        .with_context(|| format!("writing {}", paths.exit().display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_accept_errors_do_not_take_the_listener_down() {
        use std::io::{Error, ErrorKind};
        // A client that aborted between connect and accept, an interrupted
        // syscall, and a spurious would-block are all transient: keep serving.
        assert!(is_transient_accept_error(&Error::from(
            ErrorKind::ConnectionAborted
        )));
        assert!(is_transient_accept_error(&Error::from(ErrorKind::Interrupted)));
        assert!(is_transient_accept_error(&Error::from(ErrorKind::WouldBlock)));
        // Descriptor exhaustion is transient too — and flagged for a backoff.
        let emfile = Error::from_raw_os_error(libc::EMFILE);
        assert!(is_transient_accept_error(&emfile));
        assert!(is_resource_exhaustion(&emfile));
        let enfile = Error::from_raw_os_error(libc::ENFILE);
        assert!(is_transient_accept_error(&enfile));
        assert!(is_resource_exhaustion(&enfile));
    }

    #[test]
    fn a_fatal_accept_error_is_not_classified_transient() {
        use std::io::Error;
        // An unrecoverable listener error (e.g. the fd is no longer a socket):
        // not transient, so the loop exits the process instead of spinning or
        // silently abandoning the listener.
        let fatal = Error::from_raw_os_error(libc::ENOTSOCK);
        assert!(!is_transient_accept_error(&fatal));
        assert!(!is_resource_exhaustion(&fatal));
    }
}
