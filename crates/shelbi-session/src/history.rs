//! Recent-output retention beyond the emulator's screen + scrollback.
//!
//! Two things, per the plan's "History" section:
//!
//! * a **bounded ring of recent raw bytes** — always kept, cheap, and used for
//!   crash tails and reconnect nudges, and
//! * an **optional full raw output log** on disk — **off by default**, enabled
//!   only when the project opts in (it is mostly repaint noise for full-screen
//!   agents and captures anything pasted, so it is a deliberate debug switch).
//!
//! The authoritative screen state lives in the [`emulator`](crate::emulator);
//! these are the raw-byte complements to it.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Default size of the recent-raw-bytes ring (256 KiB) — enough to hold a screen
/// or two of raw escape sequences without unbounded growth.
pub const DEFAULT_RING_BYTES: usize = 256 * 1024;

/// A fixed-capacity ring of the most recent raw output bytes. Oldest bytes are
/// dropped once the ring is full.
#[derive(Debug)]
pub struct RawRing {
    buf: VecDeque<u8>,
    cap: usize,
}

impl RawRing {
    /// A ring holding at most `cap` bytes.
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap.min(DEFAULT_RING_BYTES)),
            cap: cap.max(1),
        }
    }

    /// Append `bytes`, evicting the oldest to stay within capacity.
    pub fn push(&mut self, bytes: &[u8]) {
        // A write larger than the ring keeps only its tail.
        let tail = if bytes.len() > self.cap {
            &bytes[bytes.len() - self.cap..]
        } else {
            bytes
        };
        let overflow = (self.buf.len() + tail.len()).saturating_sub(self.cap);
        self.buf.drain(..overflow);
        self.buf.extend(tail.iter().copied());
    }

    /// The retained bytes, oldest first.
    pub fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }

    /// Number of bytes currently retained.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

impl Default for RawRing {
    fn default() -> Self {
        Self::new(DEFAULT_RING_BYTES)
    }
}

/// The optional on-disk raw output log. When disabled (the default), every
/// operation is a no-op, so the hot path writes nothing and never touches the
/// filesystem.
#[derive(Debug, Default)]
pub struct RawLog {
    file: Option<File>,
}

impl RawLog {
    /// A disabled log (writes are dropped).
    pub fn disabled() -> Self {
        Self { file: None }
    }

    /// Open (truncating) a raw log at `path`. Call only when the project enabled
    /// it; a failure to open is surfaced rather than silently downgraded, since
    /// the user explicitly asked for the log.
    pub fn enabled(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("opening raw output log {}", path.display()))?;
        Ok(Self { file: Some(file) })
    }

    /// Whether this log is actually writing anywhere.
    pub fn is_enabled(&self) -> bool {
        self.file.is_some()
    }

    /// Append raw bytes. A write error is swallowed (best-effort logging must
    /// never take down the session), but a disabled log short-circuits before any
    /// I/O at all.
    pub fn write(&mut self, bytes: &[u8]) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.write_all(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_only_the_most_recent_bytes() {
        let mut ring = RawRing::new(4);
        ring.push(b"ab");
        ring.push(b"cd");
        ring.push(b"ef");
        assert_eq!(ring.snapshot(), b"cdef");
        assert_eq!(ring.len(), 4);
    }

    #[test]
    fn ring_handles_a_single_write_larger_than_capacity() {
        let mut ring = RawRing::new(3);
        ring.push(b"abcdefg");
        assert_eq!(ring.snapshot(), b"efg");
    }

    #[test]
    fn disabled_log_writes_nothing() {
        let mut log = RawLog::disabled();
        assert!(!log.is_enabled());
        log.write(b"ignored"); // must not panic or touch disk
    }

    #[test]
    fn enabled_log_appends_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw.log");
        let mut log = RawLog::enabled(&path).unwrap();
        assert!(log.is_enabled());
        log.write(b"hello ");
        log.write(b"world");
        drop(log);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
    }
}
