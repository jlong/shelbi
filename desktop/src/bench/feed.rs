//! `shelbi-desktop --bench-feed <file> <bytes/s> <go-file> [--loop | --repeat
//! N]`: the program a bench session runs.
//!
//! It turns off output post-processing on its terminal (so the PTY passes
//! the input's bytes through unchanged), waits for the bench to create
//! `<go-file>` (so the stream starts after the bench has attached), then
//! writes the file to the PTY, paced at `bytes/s` (0 = as fast as the PTY
//! takes it), `N` times (default once). With `--loop` it repeats forever;
//! otherwise it then creates `<go-file>.done` and sleeps, keeping the session
//! alive and quiet.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Bytes per write, about what a PTY read hands the session at a time.
const CHUNK: usize = 16 * 1024;

pub fn run(args: &[String]) -> ! {
    let (Some(file), Some(rate), Some(go)) = (args.first(), args.get(1), args.get(2)) else {
        eprintln!("usage: shelbi-desktop --bench-feed <file> <bytes/s> <go-file> [--loop]");
        std::process::exit(2);
    };
    let looped = args.iter().any(|a| a == "--loop");
    let repeat: usize = args
        .iter()
        .position(|a| a == "--repeat")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let rate: u64 = rate.parse().unwrap_or(0);
    let go = PathBuf::from(go);
    let bytes = std::fs::read(file).expect("read the input");
    raw_output();
    while !go.exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut out = std::io::stdout().lock();
    let started = Instant::now();
    let mut written = 0u64;
    let mut passes = 0;
    loop {
        for chunk in bytes.chunks(CHUNK) {
            if out.write_all(chunk).and_then(|_| out.flush()).is_err() {
                std::process::exit(0);
            }
            written += chunk.len() as u64;
            if rate > 0 {
                let due = Duration::from_secs_f64(written as f64 / rate as f64);
                if let Some(ahead) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(ahead);
                }
            }
        }
        passes += 1;
        if !looped && passes >= repeat {
            break;
        }
    }
    let _ = std::fs::write(go.with_extension("done"), b"");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Clear `OPOST` on stdout's terminal: no `\n` -> `\r\n` translation, so
/// the bytes the session reads are the input's.
fn raw_output() {
    // SAFETY: tcgetattr/tcsetattr on our own stdout with a struct we own.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(1, &mut t) == 0 {
            t.c_oflag &= !libc::OPOST;
            t.c_lflag &= !libc::ECHO;
            libc::tcsetattr(1, libc::TCSANOW, &t);
        }
    }
}
