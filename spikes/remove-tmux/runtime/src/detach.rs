//! The `detach-spawn` subcommand: a stand-in for the client that launches a
//! session detached.
//!
//! It re-spawns the given argv (normally `rt-runtime session ...`) with the
//! platform detach recipe, writes the spawned pid to `--pidfile`, and exits
//! immediately — modelling a launcher (or `ssh host ...`) that must return
//! while the session keeps running:
//!
//!   * `setsid()` in a `pre_exec` hook puts the session in a brand-new session
//!     with no controlling terminal, so closing the launcher's terminal cannot
//!     deliver SIGHUP to it (macOS has no `setsid(1)`, so we call the syscall).
//!   * All three stdio fds are redirected to `/dev/null`. This is what keeps a
//!     launching `ssh` from hanging: with the channel's stdout/stderr closed,
//!     sshd sees EOF and returns instead of waiting on the long-lived session.
//!   * On Linux we would additionally prefer `systemd-run --user --scope` (with
//!     lingering enabled) so logind's `KillUserProcesses=yes` does not reap the
//!     session at logout; `setsid` alone does not survive that. This spike does
//!     the detection but the fork path here is the portable `setsid` fallback.

use std::fs;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

struct Args {
    pidfile: Option<PathBuf>,
    argv: Vec<String>,
}

fn parse(args: &[String]) -> Args {
    let mut out = Args {
        pidfile: None,
        argv: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pidfile" => {
                out.pidfile = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--" => {
                out.argv = args[i + 1..].to_vec();
                break;
            }
            _ => i += 1,
        }
    }
    out
}

pub fn run(args: &[String]) -> ExitCode {
    let args = parse(args);
    if args.argv.is_empty() {
        eprintln!("detach-spawn: nothing after `--` to run");
        return ExitCode::from(2);
    }

    // Resolve the program. A bare `rt-runtime` re-runs this same binary.
    let program = if args.argv[0] == "rt-runtime" {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rt-runtime"))
    } else {
        PathBuf::from(&args.argv[0])
    };

    let devnull = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .expect("open /dev/null");
    let null_fd = devnull.as_raw_fd();

    let mut cmd = Command::new(program);
    cmd.args(&args.argv[1..]);
    cmd.stdin(Stdio::from(devnull.try_clone().unwrap()));
    cmd.stdout(Stdio::from(devnull.try_clone().unwrap()));
    cmd.stderr(Stdio::from(devnull.try_clone().unwrap()));

    // SAFETY: setsid() is async-signal-safe and touches no Rust allocator
    // state. It detaches the child into its own session before exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Belt and suspenders: also point stdio at /dev/null in-child, in
            // case a future tweak hands us an inherited fd.
            let _ = null_fd;
            Ok(())
        });
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("detach-spawn failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(path) = &args.pidfile {
        let _ = fs::write(path, format!("{}\n", child.id()));
    }

    // Do NOT wait on the child. Return at once, like a launcher that drops the
    // session and walks away.
    ExitCode::SUCCESS
}
