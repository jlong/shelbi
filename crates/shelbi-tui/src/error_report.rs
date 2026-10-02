//! Shared TUI error routing.
//!
//! Every user-facing error in the dashboard's views used to only flash in that
//! view's transient status line — the next message overwrote it and a restart
//! lost it, so a failure the user glanced away from was gone for good. #1369
//! added the persistent per-project error log
//! (`~/.shelbi/projects/<project>/error-log.jsonl`) and the sidebar's
//! unread-errors button, but only the sidebar ([`crate::App::log_error`]) wrote
//! to it.
//!
//! This module lifts that "status line *and* the persistent log" routing into
//! one place the kanban board, the review panel and the activity feed share, so
//! an error raised in any view survives the next status-line overwrite / a
//! restart and lights the unread-errors button. The sidebar is a *separate
//! process* from those panes (each `shelbi __tasks` / `__review` / `__activity`
//! pane is its own process), so it never sees an in-memory bump — it reconciles
//! the button from disk on its next refresh. Writing the log entry is therefore
//! all a view has to do.
//!
//! Every entry point is best-effort, exactly as [`shelbi_state::append_error`]'s
//! call-site contract requires: a failed log write is swallowed so a disk hiccup
//! can never crash a pane (the whole point of routing errors here).

use chrono::{DateTime, Duration, SecondsFormat, Utc};

/// Append one user-facing error to the project's persistent error log, tagged
/// with `source` (the subsystem that raised it — `kanban`, `review-panel`,
/// `activity`, `github`, `sidebar`). Best-effort: returns `true` when the write
/// landed, `false` when it was swallowed, so a caller that keeps its own
/// optimistic unread counter (the sidebar) can gate the bump on a real write.
/// Callers set their own status line separately — this only touches the log.
pub fn log_error(project: &str, source: &str, message: &str) -> bool {
    shelbi_state::append_error(project, message, Some(source)).is_ok()
}

/// Default quiet interval between repeat log entries while one transient failure
/// keeps recurring. A board refresh that fails every tick would otherwise write
/// one entry per tick and push useful history out of the 500-entry cap; with
/// this window a persistent outage logs at most a handful of entries an hour.
const DEFAULT_QUIET: Duration = Duration::minutes(5);

/// Coalesces a repeating transient failure — the GitHub board refresh failing
/// every tick is the motivating case — so a persistent outage produces a
/// *bounded* number of log entries instead of one per tick. One instance guards
/// one `(view, source)` stream; it is stateful and lives on the view so its
/// memory of the active failure survives across refresh ticks.
///
/// The decision logic is pure (it returns the line to log, or `None` to
/// coalesce) so the caller owns the actual disk write and the type is trivially
/// unit-testable with an injected `now`. It logs:
///
/// - the **first** failure immediately,
/// - a fresh entry when the message **text changes** (a different error is
///   worth its own line, and it resets the window),
/// - a periodic **"still failing"** entry once the quiet interval elapses,
///   carrying how long it has been failing and the occurrence count, and
/// - a single **"recovered"** note when the failure clears.
pub struct TransientErrorLog {
    /// Subsystem tag used in the recovered note and passed as the log `source`.
    source: String,
    /// Minimum gap between repeat entries while the same failure persists.
    quiet: Duration,
    /// The in-flight failure, or `None` when the stream is currently healthy.
    active: Option<ActiveFailure>,
}

/// Book-keeping for one in-flight transient failure.
struct ActiveFailure {
    /// The last message text seen; a change to this logs a new line.
    message: String,
    /// When this failure first started (the text last changed).
    since: DateTime<Utc>,
    /// When we last emitted a log line for it — gates the quiet interval.
    last_logged: DateTime<Utc>,
    /// How many times the failure has been observed, including coalesced ticks.
    occurrences: usize,
}

impl TransientErrorLog {
    /// A coalescer for `source` using the default quiet interval.
    pub fn new(source: impl Into<String>) -> Self {
        Self::with_quiet(source, DEFAULT_QUIET)
    }

    /// A coalescer with an explicit quiet interval — used by tests to make the
    /// repeat-after-quiet behaviour deterministic without wall-clock sleeps.
    pub fn with_quiet(source: impl Into<String>, quiet: Duration) -> Self {
        Self {
            source: source.into(),
            quiet,
            active: None,
        }
    }

    /// Record a transient failure observed at `now`. Returns `Some(line)` when a
    /// log entry should be appended (first failure, changed message, or the
    /// quiet interval elapsed), or `None` when this tick coalesces into the
    /// active failure and should not be logged.
    pub fn fail(&mut self, message: &str, now: DateTime<Utc>) -> Option<String> {
        match &mut self.active {
            // First failure in a healthy stream: log it verbatim and start the
            // window.
            None => {
                self.active = Some(ActiveFailure {
                    message: message.to_string(),
                    since: now,
                    last_logged: now,
                    occurrences: 1,
                });
                Some(message.to_string())
            }
            // A genuinely different error: log the new text and reset the window
            // so its own "still failing" cadence starts fresh.
            Some(active) if active.message != message => {
                active.message = message.to_string();
                active.since = now;
                active.last_logged = now;
                active.occurrences = 1;
                Some(message.to_string())
            }
            // Same error still failing: coalesce. Emit a "still failing" line
            // only once the quiet interval has elapsed since the last entry.
            Some(active) => {
                active.occurrences += 1;
                if now.signed_duration_since(active.last_logged) >= self.quiet {
                    active.last_logged = now;
                    Some(format!(
                        "{message} (still failing since {}, {} occurrences)",
                        active.since.to_rfc3339_opts(SecondsFormat::Secs, true),
                        active.occurrences,
                    ))
                } else {
                    None
                }
            }
        }
    }

    /// Clear the stream on recovery. Returns `Some(line)` exactly once — the
    /// first call after one or more failures — describing the recovery, or
    /// `None` when the stream was already healthy (so a steady success stream
    /// logs nothing).
    pub fn recover(&mut self) -> Option<String> {
        self.active.take().map(|active| {
            format!(
                "{} refresh recovered (was failing since {}, {} occurrences)",
                self.source,
                active.since.to_rfc3339_opts(SecondsFormat::Secs, true),
                active.occurrences,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn first_failure_logs_then_identical_ticks_coalesce() {
        let mut log = TransientErrorLog::with_quiet("github", Duration::minutes(5));

        // The first failure logs verbatim.
        assert_eq!(
            log.fail("refresh failed: unreachable", ts(0)).as_deref(),
            Some("refresh failed: unreachable"),
        );
        // Identical failures within the quiet window coalesce to nothing.
        assert_eq!(log.fail("refresh failed: unreachable", ts(10)), None);
        assert_eq!(log.fail("refresh failed: unreachable", ts(60)), None);
    }

    #[test]
    fn still_failing_logs_once_the_quiet_interval_elapses() {
        let mut log = TransientErrorLog::with_quiet("github", Duration::minutes(5));
        log.fail("refresh failed: unreachable", ts(0));
        // Just before the window closes, still coalesced.
        assert_eq!(log.fail("refresh failed: unreachable", ts(299)), None);
        // Once the window elapses, a single "still failing" line with the count.
        let line = log
            .fail("refresh failed: unreachable", ts(300))
            .expect("a still-failing line after the quiet interval");
        assert!(line.contains("still failing since"), "got: {line}");
        assert!(line.contains("occurrences"), "got: {line}");
        // And then it coalesces again until the next window.
        assert_eq!(log.fail("refresh failed: unreachable", ts(310)), None);
    }

    #[test]
    fn a_changed_message_logs_a_fresh_line() {
        let mut log = TransientErrorLog::with_quiet("github", Duration::minutes(5));
        log.fail("refresh failed: unreachable", ts(0));
        assert_eq!(
            log.fail("refresh failed: rate limited", ts(10)).as_deref(),
            Some("refresh failed: rate limited"),
            "a different error text logs immediately, not coalesced",
        );
    }

    #[test]
    fn recovery_logs_once_then_is_quiet() {
        let mut log = TransientErrorLog::with_quiet("github", Duration::minutes(5));
        log.fail("refresh failed: unreachable", ts(0));
        log.fail("refresh failed: unreachable", ts(10));

        let line = log.recover().expect("a recovered line after failures");
        assert!(line.starts_with("github refresh recovered"), "got: {line}");
        assert!(line.contains("occurrences"), "got: {line}");
        // A second recover (steady success) logs nothing.
        assert_eq!(log.recover(), None);
    }

    #[test]
    fn recovery_on_a_healthy_stream_logs_nothing() {
        let mut log = TransientErrorLog::new("github");
        assert_eq!(log.recover(), None);
    }
}
