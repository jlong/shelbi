//! Per-project persistent error log.
//!
//! Errors the TUI used to flash in the sidebar footer's status line (yellow
//! text that the next message overwrote and a restart lost) are appended here
//! instead, to `~/.shelbi/projects/<project>/error-log.jsonl` — one JSON object
//! per line (`{ts, message, source?}`), newest last. A sidecar
//! `error-log.read` file records the timestamp of the newest entry the user has
//! already seen, so the "unread errors" button survives restarts.
//!
//! The log is capped at [`ERROR_LOG_CAP`] entries, trimmed oldest-first on
//! every write, so it can't grow without bound. Writes serialize under a
//! sibling `error-log.lock` and land via [`crate::atomic_write`] (temp file +
//! rename), so a concurrent reader always sees a whole, consistent file and a
//! crashed writer never leaves a half-written log. Every entry point is
//! best-effort at the call site: the sidebar wraps [`append_error`] so a failed
//! log write can never crash the pane.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::Result;

use crate::{acquire_file_lock, atomic_write, ensure_dir, project_dir};

/// Maximum number of entries retained in a project's error log. Older entries
/// are dropped on write once the log grows past this, so an install that logs
/// steadily for months still keeps a bounded, recent tail.
pub const ERROR_LOG_CAP: usize = 500;

/// One recorded error. `ts` is an RFC3339 UTC timestamp with nanosecond
/// precision (`…Z`), which sorts lexicographically in chronological order — the
/// property the read-marker comparison relies on. `source` is an optional short
/// tag naming where the error came from (a subsystem / call site).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorLogEntry {
    pub ts: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl ErrorLogEntry {
    /// The entry's timestamp parsed back into a `DateTime`, or `None` when it
    /// isn't valid RFC3339 (a hand-edited or corrupt line). Callers that only
    /// need to display the raw string skip this.
    pub fn parsed_ts(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.ts)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }
}

/// `~/.shelbi/projects/<project>/error-log.jsonl`.
pub fn error_log_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("error-log.jsonl"))
}

/// Sibling read marker: `~/.shelbi/projects/<project>/error-log.read`. Holds
/// the timestamp of the newest entry the user has acknowledged (opened the log
/// on). Absent when nothing has ever been read, in which case every entry
/// counts as unread.
fn error_log_marker_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("error-log.read"))
}

/// Sibling advisory lock guarding the read-modify-write append. Every sidebar /
/// kanban / activity pane is a separate process that may log into the same
/// per-project file, so the trim-and-rewrite has to serialize across processes.
fn error_log_lock_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("error-log.lock"))
}

/// Parse a JSONL error log's bytes into entries, oldest-first. Blank lines and
/// individually unparseable lines are skipped rather than failing the whole
/// read — a single corrupt line must never hide the rest of the history.
fn parse_entries(text: &str) -> Vec<ErrorLogEntry> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ErrorLogEntry>(l).ok())
        .collect()
}

/// Read one project's error log, oldest-first. A missing log reads as empty
/// (the common case before the first error). No lock is taken: writes land via
/// an atomic rename, so a reader always sees a whole prior-or-next generation,
/// never a torn file.
pub fn read_errors(project: &str) -> Result<Vec<ErrorLogEntry>> {
    read_errors_at(&error_log_path(project)?)
}

fn read_errors_at(path: &Path) -> Result<Vec<ErrorLogEntry>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse_entries(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(shelbi_core::Error::Io(crate::annotate_io_error(path, e))),
    }
}

/// Append one error to a project's log, trimming to the newest [`ERROR_LOG_CAP`]
/// entries. Serializes under the per-project lock and rewrites the file
/// atomically, so concurrent writers never interleave and a reader never sees a
/// partial file. `message` is stored verbatim (JSON escapes any embedded
/// newlines, so the on-disk line stays one record).
pub fn append_error(project: &str, message: &str, source: Option<&str>) -> Result<()> {
    let entry = ErrorLogEntry {
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
        message: message.to_string(),
        source: source.map(str::to_string),
    };
    let path = error_log_path(project)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let _lock = acquire_file_lock(&error_log_lock_path(project)?)?;
    let mut entries = read_errors_at(&path)?;
    entries.push(entry);
    // Keep only the newest CAP entries; drop the oldest overflow.
    let start = entries.len().saturating_sub(ERROR_LOG_CAP);
    let mut buf = String::new();
    for e in &entries[start..] {
        // A serialize failure on a single entry is unexpected (the struct is
        // plain data); skip it rather than abort the whole write so one bad
        // entry can't wedge the log.
        if let Ok(line) = serde_json::to_string(e) {
            buf.push_str(&line);
            buf.push('\n');
        }
    }
    atomic_write(&path, buf.as_bytes())
}

/// Clear a project's error log and reset its read marker, so the log is empty
/// and nothing counts as unread. Both files are removed under the write lock.
pub fn clear_errors(project: &str) -> Result<()> {
    let path = error_log_path(project)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let _lock = acquire_file_lock(&error_log_lock_path(project)?)?;
    remove_if_present(&path)?;
    remove_if_present(&error_log_marker_path(project)?)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Io(crate::annotate_io_error(path, e))),
    }
}

/// The stored read-marker timestamp, or `None` when the log has never been
/// opened. A missing / empty marker means "everything is unread".
fn read_marker(project: &str) -> Result<Option<String>> {
    let path = error_log_marker_path(project)?;
    match fs::read_to_string(&path) {
        Ok(text) => {
            let trimmed = text.trim();
            Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(shelbi_core::Error::Io(crate::annotate_io_error(&path, e))),
    }
}

/// Mark every currently-recorded error as read: set the marker to the newest
/// entry's timestamp (or "now" when the log is empty), so `unread_error_count`
/// returns zero until a newer error arrives. Idempotent and best-effort at the
/// call site.
pub fn mark_errors_read(project: &str) -> Result<()> {
    let marker = read_errors(project)?
        .last()
        .map(|e| e.ts.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true));
    atomic_write(&error_log_marker_path(project)?, marker.as_bytes())
}

/// Count entries newer than the read marker — the number the sidebar's unread
/// button reflects. With no marker, every entry is unread. String comparison of
/// the RFC3339-with-`Z` timestamps is chronological, so this is a cheap filter.
pub fn unread_error_count(project: &str) -> Result<usize> {
    let entries = read_errors(project)?;
    Ok(match read_marker(project)? {
        Some(marker) => entries
            .iter()
            .filter(|e| e.ts.as_str() > marker.as_str())
            .count(),
        None => entries.len(),
    })
}

/// Split a project's errors into `(entries_newest_first, unread_count)` for the
/// viewer, reading the marker once. The returned entries are ordered newest
/// first (display order), each paired with whether it is unread. Reads the
/// marker *before* the caller marks the log read, so the ● unread markers still
/// reflect what was new when the log was opened.
pub fn read_errors_with_unread(project: &str) -> Result<Vec<(ErrorLogEntry, bool)>> {
    let marker = read_marker(project)?;
    let mut entries = read_errors(project)?;
    entries.reverse();
    Ok(entries
        .into_iter()
        .map(|e| {
            let unread = marker
                .as_deref()
                .map(|m| e.ts.as_str() > m)
                .unwrap_or(true);
            (e, unread)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-error-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn append_then_read_roundtrips_entries_oldest_first() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_error("demo", "first failed: boom", None).unwrap();
        append_error("demo", "second failed: bang", Some("zen")).unwrap();

        let entries = read_errors("demo").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "first failed: boom");
        assert_eq!(entries[1].message, "second failed: bang");
        assert_eq!(entries[1].source.as_deref(), Some("zen"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn unread_count_tracks_marker_across_a_reload() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // No marker yet: both entries are unread.
        append_error("demo", "a failed: x", None).unwrap();
        append_error("demo", "b failed: y", None).unwrap();
        assert_eq!(unread_error_count("demo").unwrap(), 2);

        // Opening the log marks everything read; the count drops to zero and —
        // crucially — stays zero when re-read from disk (survives a "restart").
        mark_errors_read("demo").unwrap();
        assert_eq!(unread_error_count("demo").unwrap(), 0);
        assert_eq!(unread_error_count("demo").unwrap(), 0);

        // A newer error becomes unread again; older acknowledged ones stay read.
        append_error("demo", "c failed: z", None).unwrap();
        assert_eq!(unread_error_count("demo").unwrap(), 1);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn read_with_unread_marks_only_entries_after_the_marker() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_error("demo", "old failed: 1", None).unwrap();
        mark_errors_read("demo").unwrap();
        append_error("demo", "new failed: 2", None).unwrap();

        let rows = read_errors_with_unread("demo").unwrap();
        // Newest first.
        assert_eq!(rows[0].0.message, "new failed: 2");
        assert!(rows[0].1, "the entry after the marker is unread");
        assert_eq!(rows[1].0.message, "old failed: 1");
        assert!(!rows[1].1, "the acknowledged entry is read");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn log_is_capped_and_trims_oldest_first() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        for i in 0..(ERROR_LOG_CAP + 25) {
            append_error("demo", &format!("failed: {i}"), None).unwrap();
        }
        let entries = read_errors("demo").unwrap();
        assert_eq!(entries.len(), ERROR_LOG_CAP, "log is capped at ERROR_LOG_CAP");
        // The oldest 25 were dropped; the newest is retained.
        assert_eq!(entries.first().unwrap().message, "failed: 25");
        assert_eq!(
            entries.last().unwrap().message,
            format!("failed: {}", ERROR_LOG_CAP + 24)
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn clear_empties_the_log_and_resets_unread() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_error("demo", "failed: gone", None).unwrap();
        clear_errors("demo").unwrap();
        assert!(read_errors("demo").unwrap().is_empty());
        assert_eq!(unread_error_count("demo").unwrap(), 0);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn corrupt_lines_are_skipped_not_fatal() {
        let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_error("demo", "failed: good", None).unwrap();
        // Splice a garbage line into the file; the good entry must still read.
        let path = error_log_path("demo").unwrap();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("this is not json\n");
        std::fs::write(&path, text).unwrap();

        let entries = read_errors("demo").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "failed: good");

        std::env::remove_var("SHELBI_HOME");
    }
}
