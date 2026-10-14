//! `shelbi-desktop --bench-record <session> <file.gz> [seconds]`: capture a
//! live session's output stream for the bench.
//!
//! Attaches as a passive client (it never sends input or a size, so the
//! session's PTY keeps the size its active client gave it), keeps the attach
//! replay and then every output chunk, and stops after `seconds` (default
//! 120) or when the session ends. The file is a capture (see
//! [`super::corpus::Capture`]): a one-line header with the session's size,
//! then the raw bytes, gzipped.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use shelbi_client::SessionEvent;

use super::corpus::CAPTURE_MAGIC;
use crate::elements::terminal::session;

pub fn run(args: &[String]) -> ! {
    let (Some(selector), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: shelbi-desktop --bench-record <session> <file.gz> [seconds]");
        std::process::exit(2);
    };
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);
    let connected = session::connect(selector).unwrap_or_else(|e| {
        eprintln!("bench-record: {e}");
        std::process::exit(1);
    });
    let size = connected.size;
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let sink = bytes.clone();
    let mut events = connected.events;
    std::thread::spawn(move || {
        futures::executor::block_on(async {
            while let Some(ev) = events.next().await {
                match ev {
                    SessionEvent::Output { data, .. } => sink.lock().unwrap().extend_from_slice(&data),
                    SessionEvent::Resync { replay, .. } => {
                        sink.lock().unwrap().extend_from_slice(&replay)
                    }
                    SessionEvent::Exited(_) => break,
                    _ => {}
                }
            }
        })
    });
    // The link (and so the connection) stays alive until we exit.
    let _link = connected.link;
    std::thread::sleep(Duration::from_secs(secs));
    let bytes = bytes.lock().unwrap().clone();
    let path = PathBuf::from(out);
    let file = std::fs::File::create(&path).expect("create the capture file");
    let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    writeln!(gz, "{CAPTURE_MAGIC} cols={} rows={}", size.cols, size.rows).unwrap();
    gz.write_all(&bytes).unwrap();
    gz.finish().unwrap();
    println!(
        "recorded {} bytes from {selector} ({}x{}) to {}",
        bytes.len(),
        size.cols,
        size.rows,
        path.display()
    );
    std::process::exit(0)
}
