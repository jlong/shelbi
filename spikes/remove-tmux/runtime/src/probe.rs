//! The `probe` subcommand: a stand-in for the TUI, used by the nesting test.
//!
//! It is meant to run *inside* a multiplexer (tmux / Screen) whose outer
//! terminal is emulated by the test harness. It measures what the multiplexer
//! lets through:
//!
//!   * **kitty keyboard** — writes the "query flags" sequence `CSI ? u` and
//!     waits (raw mode, short timeout) for a `CSI ? <flags> u` reply. The
//!     harness answers that query as a kitty-capable terminal would, so a reply
//!     means the multiplexer forwarded both the query and the response; a
//!     timeout means it swallowed one of them.
//!   * **truecolor** — reports `$TERM` and `$COLORTERM` as the multiplexer set
//!     them, which is what crossterm/ratatui key truecolor-vs-256 off.
//!   * **OSC 52** — emits an OSC 52 clipboard write. The harness watches the
//!     outer stream to see whether the multiplexer passed it through.
//!
//! Results are written to `--out` (a file on the real filesystem) so they do
//! not have to be scraped back through the multiplexer's redraw.

use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

struct RawGuard {
    fd: libc::c_int,
    saved: libc::termios,
}

impl RawGuard {
    fn new(fd: libc::c_int) -> Option<Self> {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return None;
            }
            let saved = t;
            libc::cfmakeraw(&mut t);
            if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
                return None;
            }
            Some(RawGuard { fd, saved })
        }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
        }
    }
}

/// Read from `fd` until `deadline`, returning whatever bytes arrived.
fn read_until(fd: libc::c_int, deadline: Instant) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r <= 0 {
            break;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
        // A kitty reply ends in `u`; stop as soon as we have one.
        if out.contains(&b'u') {
            break;
        }
    }
    out
}

/// Does `bytes` contain a kitty flags reply `ESC [ ? <digits> u`?
fn has_kitty_reply(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' && bytes[i + 2] == b'?' {
            let mut j = i + 3;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'u' {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub fn run(args: &[String]) -> ExitCode {
    let mut out_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--out" {
            out_path = args.get(i + 1).map(PathBuf::from);
            i += 2;
        } else {
            i += 1;
        }
    }

    let term = std::env::var("TERM").unwrap_or_default();
    let colorterm = std::env::var("COLORTERM").unwrap_or_default();

    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut stdout = std::io::stdout();

    // kitty keyboard probe.
    let kitty = {
        let _guard = RawGuard::new(stdin_fd);
        // OSC 52 first (while we hold raw mode is fine; it is output only).
        let _ = stdout.write_all(b"\x1b]52;c;c2hlbGJpLXNwaWtl\x07");
        // Query current kitty flags.
        let _ = stdout.write_all(b"\x1b[?u");
        let _ = stdout.flush();
        if _guard.is_some() {
            let reply = read_until(stdin_fd, Instant::now() + Duration::from_millis(700));
            has_kitty_reply(&reply)
        } else {
            // No tty on stdin (should not happen under a PTY); cannot probe.
            false
        }
    };

    let truecolor = colorterm.eq_ignore_ascii_case("truecolor") || colorterm.eq_ignore_ascii_case("24bit");

    let body = format!(
        "kitty_keyboard={}\nterm={}\ncolorterm={}\ntruecolor_env={}\nosc52_emitted=true\n",
        if kitty { "yes" } else { "no" },
        term,
        colorterm,
        truecolor,
    );
    if let Some(p) = &out_path {
        let _ = fs::write(p, &body);
    } else {
        print!("{body}");
    }

    // Give the harness a beat to observe trailing output, then exit. (No
    // blocking read here: under a multiplexer nothing would ever send input,
    // and the findings file is already written.)
    let _ = stdout.flush();
    std::thread::sleep(Duration::from_millis(500));
    ExitCode::SUCCESS
}
