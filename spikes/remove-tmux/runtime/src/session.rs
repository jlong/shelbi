//! The `session` subcommand: a stand-in for `shelbi __session`.
//!
//! It opens one PTY with `portable-pty`, spawns a child inside it, records its
//! own and the child's pid/pgid/sid to a status file, and then updates a marker
//! file on a fixed cadence so a test can watch it stay alive after the launcher
//! is gone. On SIGTERM it kills the child's whole process group and exits.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_sig: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
}

struct Args {
    status: Option<PathBuf>,
    marker: Option<PathBuf>,
    child: Vec<String>,
}

fn parse(args: &[String]) -> Args {
    let mut out = Args {
        status: None,
        marker: None,
        child: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--status" => {
                out.status = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--marker" => {
                out.marker = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--child" => {
                // Everything after --child is the child argv.
                out.child = args[i + 1..].to_vec();
                break;
            }
            _ => i += 1,
        }
    }
    out
}

pub fn run(args: &[String]) -> ExitCode {
    let args = parse(args);

    // Install the SIGTERM handler before we have anything to clean up.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
    }

    let pty = native_pty_system();
    let pair = match pty.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("openpty failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Default child keeps a grandchild alive in the same process group, so a
    // group kill has something non-trivial to reap.
    let mut cmd = if args.child.is_empty() {
        let mut c = CommandBuilder::new("/bin/sh");
        c.args(["-c", "sleep 100000 & exec sleep 100000"]);
        c
    } else {
        let mut c = CommandBuilder::new(&args.child[0]);
        c.args(&args.child[1..]);
        c
    };
    cmd.cwd(env_current_dir());

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spawn failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The slave handle is not needed once the child owns it; dropping it closes
    // our copy so the PTY collapses cleanly when the child exits.
    drop(pair.slave);

    let child_pid = child.process_id().unwrap_or(0) as libc::pid_t;
    // portable-pty puts the child in its own session via setsid(), so the
    // child is a process-group leader and pgid == pid.
    let child_pgid = unsafe { libc::getpgid(child_pid) };

    if let Some(path) = &args.status {
        let me = unsafe { libc::getpid() };
        let my_pgid = unsafe { libc::getpgid(0) };
        let my_sid = unsafe { libc::getsid(0) };
        let body = format!(
            "session_pid={me}\nsession_pgid={my_pgid}\nsession_sid={my_sid}\n\
             child_pid={child_pid}\nchild_pgid={child_pgid}\n"
        );
        let _ = fs::write(path, body);
    }

    // Drain the PTY master in the background so the child never blocks on a
    // full output buffer. We discard the bytes; this spike is about lifecycle.
    if let Ok(mut reader) = pair.master.try_clone_reader() {
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
            }
        });
    }

    // Liveness loop: bump the marker until we are told to terminate or the
    // child exits on its own.
    let mut tick: u64 = 0;
    loop {
        if TERMINATE.load(Ordering::SeqCst) {
            if child_pgid > 1 {
                unsafe {
                    libc::killpg(child_pgid, libc::SIGTERM);
                }
                thread::sleep(Duration::from_millis(100));
                unsafe {
                    libc::killpg(child_pgid, libc::SIGKILL);
                }
            }
            let _ = child.wait();
            return ExitCode::SUCCESS;
        }
        if let Ok(Some(_status)) = child.try_wait() {
            return ExitCode::SUCCESS;
        }
        if let Some(path) = &args.marker {
            let _ = fs::write(path, format!("{tick}\n"));
        }
        tick += 1;
        thread::sleep(Duration::from_millis(50));
    }
}

fn env_current_dir() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}
