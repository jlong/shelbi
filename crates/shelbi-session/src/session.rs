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
//!    logs raw output, and broadcasts output to attached clients,
//! 4. serves clients on a Unix socket (hello / attach / input / resize /
//!    snapshot / kill — the frozen-core subset this task needs), and
//! 5. on child exit (or SIGTERM/SIGHUP) kills the child's **process group**,
//!    writes `exit.json` and `final.txt`, and exits.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use shelbi_proto::{Frame, Output};

use crate::emulator::Emulator;
use crate::history::{RawLog, RawRing};
use crate::layout::SessionPaths;
use crate::lock::SessionLock;
use crate::meta::{ExitRecord, Meta};
use crate::responder::{ColorState, CursorSource, FixedCursor, KittyFlags, Responder};
use crate::transport::{serve_client, ClientRegistry};

/// Lines of scrollback included (above the last screen) in `final.txt`.
const FINAL_HISTORY_LINES: usize = 1000;

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
}

/// State shared between the PTY reader thread and the per-client threads.
pub struct Shared {
    /// The authoritative emulator (also the cursor source for query replies and
    /// the text source for snapshots / `final.txt`).
    pub emu: Mutex<Emulator>,
    /// Default colors answered to OSC 10/11 queries (dark until a client reports).
    pub colors: ColorState,
    /// Active kitty keyboard flags (tracked by the responder).
    pub kitty: KittyFlags,
    /// Recent raw output bytes.
    pub ring: Mutex<RawRing>,
    /// Monotonic output sequence number.
    pub seq: AtomicU64,
    /// The PTY master, kept for `resize`.
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// The child's stdin (PTY master write side). Responder replies and client
    /// input both go here; the mutex keeps each write whole (input arbitration).
    writer: Mutex<Box<dyn Write + Send>>,
    /// The child's process group id, for group kill.
    child_pgid: libc::pid_t,
    /// Attached clients the reader broadcasts output to.
    pub clients: ClientRegistry,
}

impl Shared {
    /// Write raw bytes to the child (responder replies and client input). Each
    /// call is whole and ordered relative to others.
    pub fn write_to_child(&self, bytes: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    /// Resize the PTY and the emulator to a new viewport.
    pub fn resize(&self, cols: u16, rows: u16) {
        if let Ok(master) = self.master.lock() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        self.emu.lock().unwrap().resize(cols, rows);
    }

    /// Render the screen (optionally with `history_lines` of scrollback) to text.
    pub fn snapshot_text(&self, history_lines: Option<u32>) -> String {
        let emu = self.emu.lock().unwrap();
        match history_lines {
            Some(n) => emu.screen_with_history(n as usize),
            None => emu.visible_text(),
        }
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

    write_meta(&paths, &args)?;

    install_signal_handlers();

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
        seq: AtomicU64::new(0),
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        child_pgid,
        clients: ClientRegistry::default(),
    });

    // Optional raw output log (owned by the reader thread).
    let raw_log = if args.raw_output_log {
        RawLog::enabled(&paths.raw_log())
            .with_context(|| "enabling raw output log")?
    } else {
        RawLog::disabled()
    };

    // --- reader thread ---------------------------------------------------
    spawn_reader_thread(shared.clone(), reader, raw_log);

    // --- socket server ---------------------------------------------------
    // Remove any stale socket from a previous (dead) session at this id.
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

/// Read the PTY master forever: feed the emulator, answer queries, retain bytes,
/// optionally log, and broadcast to clients. Runs until the PTY closes (child
/// exit), after which the thread ends.
fn spawn_reader_thread(
    shared: Arc<Shared>,
    mut reader: Box<dyn Read + Send>,
    mut raw_log: RawLog,
) {
    thread::spawn(move || {
        let mut responder = Responder::new(shared.kitty.clone(), shared.colors.clone());
        let mut chunk = [0u8; 8192];
        loop {
            let n = match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let data = &chunk[..n];

            // Feed the emulator first so the cursor the responder reports is
            // current, then answer any queries in this chunk.
            let (cursor_row, cursor_col) = {
                let mut emu = shared.emu.lock().unwrap();
                emu.feed(data);
                emu.cursor_1based()
            };
            let answers = responder.scan(data, &FixedCursor(cursor_row, cursor_col));
            for answer in answers {
                shared.write_to_child(&answer.reply);
            }

            // Retain recent bytes and (optionally) log the raw stream.
            shared.ring.lock().unwrap().push(data);
            raw_log.write(data);

            // Broadcast sequenced output to attached clients.
            if !shared.clients.is_empty() {
                let seq = shared.seq.fetch_add(1, Ordering::Relaxed);
                if let Ok(bytes) = Frame::Output(Output {
                    seq,
                    data: data.to_vec(),
                })
                .encode()
                {
                    shared.clients.broadcast(&bytes);
                }
            } else {
                // Keep the sequence advancing so a later attach is monotonic.
                shared.seq.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
}

/// Accept client connections, one serving thread each.
fn spawn_accept_thread(shared: Arc<Shared>, listener: UnixListener) {
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let shared = shared.clone();
                    thread::spawn(move || serve_client(stream, shared));
                }
                Err(_) => break,
            }
        }
    });
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, on_terminate as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_terminate as *const () as libc::sighandler_t);
    }
}

fn write_meta(paths: &SessionPaths, args: &RunArgs) -> Result<()> {
    let meta = Meta {
        id: args.id.clone(),
        name: args.name.clone(),
        argv: args.child_argv.clone(),
        cwd: args.cwd.clone(),
        task: args.task.clone(),
        launched_at: chrono::Utc::now().to_rfc3339(),
        protocol_version: shelbi_proto::PROTOCOL_VERSION,
    };
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
