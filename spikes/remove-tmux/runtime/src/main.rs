//! Phase 0 runtime spike for "Removing tmux".
//!
//! One throwaway binary with a few subcommands that the integration tests in
//! `tests/` drive to retire two risks before Phase 1 commits to a design:
//!
//!   * **Process survival** — a session process must outlive the launcher that
//!     spawned it (setsid semantics on macOS; `systemd-run --user --scope` or
//!     `setsid` on Linux/logind), with all stdio redirected so a launching
//!     `ssh` does not hang, and its child's process group must die on kill.
//!   * **Nesting** — the TUI (here a tiny `probe` stand-in) running inside
//!     tmux and inside GNU Screen: kitty-keyboard passthrough, truecolor
//!     fallback, OSC 52 copy.
//!
//! `portable-pty` behaviors (controlling terminal, process-group handling,
//! group kill, descriptor leaks) are exercised directly as a library in
//! `tests/pty.rs`; this binary is only the moving parts a test cannot express
//! in-process.
//!
//! Throwaway. Findings live in `docs/removing-tmux/phase0/runtime.md`.

use std::env;
use std::process::ExitCode;

mod detach;
mod probe;
mod session;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(2)..];
    match cmd {
        "session" => session::run(rest),
        "detach-spawn" => detach::run(rest),
        "probe" => probe::run(rest),
        _ => {
            eprintln!(
                "usage: rt-runtime <session|detach-spawn|probe> [args]\n\
                 \n\
                 session       --status <f> --marker <f> [--child <argv...>]\n\
                 detach-spawn  --pidfile <f> -- rt-runtime session ...\n\
                 probe         --out <f>"
            );
            ExitCode::from(2)
        }
    }
}
