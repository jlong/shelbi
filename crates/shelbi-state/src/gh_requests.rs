//! Hub-wide GitHub **request log** — an append-only record of every `gh` API
//! request the store actually makes, tagged with the budget tier it spent and
//! the caller that made it. Phase 3 §6 of
//! `Plans/github-issue-caching-and-rate-limits.md`.
//!
//! ## Why a request log at all
//!
//! The per-token `gh-budget/` file ([`crate::gh_budget`]) records the *latest*
//! `remaining`/`reset` a response surfaced — a point-in-time snapshot. It cannot
//! answer "how fast are we spending, and who is spending it", which is exactly
//! what the two Phase 3 diagnostics need:
//!
//! - `shelbi status` shows requests in the last hour per budget alongside the
//!   remaining/reset snapshot.
//! - `shelbi doctor` warns when the *observed rate* would exhaust a budget in
//!   under 30 minutes and names the top callers so an operator knows which
//!   reader to throttle.
//!
//! So every governed read ([`crate::github_store`]'s GraphQL choke point) and
//! every REST write appends one line here as it happens. On a healthy hub this
//! is a trickle (the daemon is the single board reader), so the file stays tiny;
//! a runaway caller shows up immediately as a spike the doctor can name.
//!
//! ## Format and durability
//!
//! One line per request, hub-wide, under `<shelbi-root>/gh-requests.log`:
//!
//! ```text
//! <rfc3339> budget=<graphql|rest> caller=<name> outcome=<ok|err:<class>>
//! ```
//!
//! The `outcome` field distinguishes a request that reached GitHub and **spent**
//! budget (`ok`) from an **attempt** that failed without spending it — most
//! importantly `err:conn`, a connection-level failure where `gh` never reached
//! the API. Without it a dead network reads as a 34k/hr burn rate (the
//! 2026-09-08 incident); with it `shelbi doctor` projects exhaustion from spent
//! requests alone and reports failed attempts separately. A line written by an
//! older shelbi carries no `outcome` and is read back as `ok` (spent).
//!
//! Written with a single `O_APPEND` `write_all` (POSIX guarantees writes under
//! `PIPE_BUF` are atomic relative to other appenders, and a request line is well
//! under that), so concurrent appenders from the daemon, panes and CLI interleave
//! whole lines rather than tearing. Recording is **best-effort**: any IO error is
//! swallowed, because losing a diagnostic line must never fail a real API call.
//! Reads bound themselves to the tail of the file so an unbounded log never costs
//! a full slurp.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::gh_budget::Budget;

/// Basename of the hub-wide request log under the shelbi root.
pub const REQUESTS_LOG_FILE: &str = "gh-requests.log";

/// GitHub's per-hour ceiling on each budget: 5,000 REST requests and 5,000
/// GraphQL points. Used as the exhaustion denominator when the live `remaining`
/// snapshot is unknown, so the doctor can still project a time-to-exhaustion
/// from the observed rate alone.
pub const GITHUB_BUDGET_LIMIT: u64 = 5_000;

/// The tag written (and parsed) for a budget tier in the log.
pub fn budget_tag(budget: Budget) -> &'static str {
    match budget {
        Budget::Graphql => "graphql",
        Budget::Rest => "rest",
    }
}

/// Parse a budget tag back into a [`Budget`]; `None` for an unknown tag (a line
/// from a newer shelbi, say) so the reader skips it rather than guessing.
fn parse_budget(tag: &str) -> Option<Budget> {
    match tag {
        "graphql" => Some(Budget::Graphql),
        "rest" => Some(Budget::Rest),
        _ => None,
    }
}

/// The outcome of a `gh` request: whether it reached GitHub and spent budget, or
/// failed as an attempt (carrying a short error class for the diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The request reached GitHub and spent budget.
    Ok,
    /// The request failed with the given class ([`crate::gh_retry::error_class`]
    /// — `conn` / `ratelimit` / `transient` / `other`). A `conn` failure spent
    /// nothing; the others may have. Either way it is an *attempt*, not spend.
    Err(&'static str),
}

impl Outcome {
    /// The `outcome=` field token: `ok`, or `err:<class>`.
    fn tag(self) -> String {
        match self {
            Outcome::Ok => "ok".to_string(),
            Outcome::Err(class) => format!("err:{class}"),
        }
    }

    /// Whether this outcome spent budget (`ok`).
    pub fn is_ok(self) -> bool {
        matches!(self, Outcome::Ok)
    }
}

/// How much of the tail to scan on a read. 512 KiB is tens of thousands of
/// request lines — far more than the last hour on any real hub, and enough that
/// the doctor's short observation window is always fully covered — while keeping
/// a runaway log from ever being slurped whole.
const READ_TAIL_BYTES: u64 = 512 * 1024;

/// The request log's path under the shelbi root, or `None` when the root can't
/// be resolved (recording is then simply disabled, exactly like the budget file).
fn log_path() -> Option<PathBuf> {
    crate::shelbi_home()
        .ok()
        .map(|home| home.join(REQUESTS_LOG_FILE))
}

/// Sanitize a caller label to a single log-safe token: no whitespace (which is
/// the field separator) and no newline (which would tear the record). Empty or
/// all-whitespace collapses to `unknown` so a line always carries a caller.
fn sanitize_caller(caller: &str) -> String {
    let cleaned: String = caller
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// Append one request record for `budget` made by `caller` with its `outcome`.
/// Best-effort: a missing root or any IO error is swallowed so a diagnostic
/// append can never fail the API call it is describing.
pub fn record_request(budget: Budget, caller: &str, outcome: Outcome) {
    record_request_at(budget, caller, outcome, Utc::now());
}

/// [`record_request`] with an explicit timestamp — the seam a test drives to
/// simulate a request stream without waiting real time.
pub fn record_request_at(budget: Budget, caller: &str, outcome: Outcome, at: DateTime<Utc>) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = crate::ensure_dir(parent);
    }
    let line = format!(
        "{} budget={} caller={} outcome={}\n",
        at.to_rfc3339(),
        budget_tag(budget),
        sanitize_caller(caller),
        outcome.tag(),
    );
    // One finished buffer, one `write_all`, under O_APPEND — see the module docs.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// One parsed request-log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestEntry {
    /// When the request was made.
    pub at: DateTime<Utc>,
    /// Which budget it concerns.
    pub budget: Budget,
    /// The caller label recorded for it.
    pub caller: String,
    /// Whether the request reached GitHub and spent budget (`outcome=ok`). A
    /// failed attempt (`outcome=err:*`, or an old line with no field read as
    /// `ok`) that spent nothing is `false` — so a dead network never inflates
    /// the observed spend rate.
    pub spent: bool,
    /// The error class for a failed attempt (`conn` / `ratelimit` / …), or
    /// `None` when the request spent budget.
    pub err_class: Option<String>,
}

/// Read the request records made within `window` before `now`, newest last.
/// Only the tail of the file is scanned ([`READ_TAIL_BYTES`]); a partial leading
/// line left by a mid-line seek fails to parse and is dropped. A missing or
/// unreadable log reads as no entries.
pub fn recent_entries(window: Duration, now: DateTime<Utc>) -> Vec<RequestEntry> {
    let Some(path) = log_path() else {
        return Vec::new();
    };
    let Ok(text) = read_tail(&path, READ_TAIL_BYTES) else {
        return Vec::new();
    };
    let cutoff = now - chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::zero());
    text.lines()
        .filter_map(parse_line)
        .filter(|e| e.at >= cutoff && e.at <= now)
        .collect()
}

/// Read at most the last `max_bytes` of `path` as lossy UTF-8, seeking to the
/// tail rather than slurping the whole file.
fn read_tail(path: &std::path::Path, max_bytes: u64) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    if start > 0 {
        f.seek(SeekFrom::Start(start))?;
    }
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Parse one `<rfc3339> budget=<tag> caller=<name>` line. `None` for a line that
/// doesn't carry a parseable timestamp and a known budget (a torn leading
/// fragment, or a line from a newer format).
fn parse_line(line: &str) -> Option<RequestEntry> {
    let mut tokens = line.split_whitespace();
    let at = DateTime::parse_from_rfc3339(tokens.next()?)
        .ok()?
        .with_timezone(&Utc);
    let mut budget = None;
    let mut caller = None;
    // Default to spent for a line with no `outcome=` field — the format an older
    // shelbi wrote, where every recorded request had reached GitHub.
    let mut spent = true;
    let mut err_class = None;
    for tok in tokens {
        if let Some(v) = tok.strip_prefix("budget=") {
            budget = parse_budget(v);
        } else if let Some(v) = tok.strip_prefix("caller=") {
            caller = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("outcome=") {
            if v == "ok" {
                spent = true;
            } else {
                spent = false;
                err_class = v.strip_prefix("err:").map(str::to_string);
            }
        }
    }
    Some(RequestEntry {
        at,
        budget: budget?,
        caller: caller.unwrap_or_else(|| "unknown".to_string()),
        spent,
        err_class,
    })
}

/// The observed rate on one budget over an observation window, with the callers
/// that drove it — the shape both `shelbi status` (count) and `shelbi doctor`
/// (rate + top callers) render.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetRate {
    /// Which budget these requests concern.
    pub budget: Budget,
    /// How many requests in the window **spent** budget (`outcome=ok`). This is
    /// the number the rate/exhaustion projection is built on — a failed attempt
    /// (a dead network) must not read as spend.
    pub count: usize,
    /// How many requests in the window were **failed attempts** (`outcome=err:*`)
    /// that (for `conn`) spent nothing — reported separately from `count`.
    pub failed: usize,
    /// The observation window, in seconds.
    pub window_secs: u64,
    /// Callers ordered by descending *spent* request count in the window.
    pub top_callers: Vec<(String, usize)>,
}

impl BudgetRate {
    /// Requests per second over the window (0 when the window is empty).
    pub fn per_sec(&self) -> f64 {
        if self.window_secs == 0 {
            return 0.0;
        }
        self.count as f64 / self.window_secs as f64
    }

    /// Seconds until this budget is exhausted at the observed rate, given the
    /// `remaining` snapshot (falling back to [`GITHUB_BUDGET_LIMIT`] when the
    /// live remaining is unknown). `None` when the rate is zero — nothing is
    /// being spent, so there is no exhaustion to project.
    pub fn seconds_to_exhaustion(&self, remaining: Option<u64>) -> Option<f64> {
        let rate = self.per_sec();
        if rate <= 0.0 {
            return None;
        }
        let budget_left = remaining.unwrap_or(GITHUB_BUDGET_LIMIT) as f64;
        Some(budget_left / rate)
    }
}

/// Group `entries` into a per-budget rate summary over `window`. One
/// [`BudgetRate`] per budget that appears in the window, each carrying its
/// caller breakdown newest-independent (ordered by count desc, then name).
pub fn summarize(entries: &[RequestEntry], window: Duration) -> Vec<BudgetRate> {
    use std::collections::HashMap;
    /// Per-budget accumulator: spent count, failed count, and spent-caller tally.
    #[derive(Default)]
    struct Acc {
        spent: usize,
        failed: usize,
        callers: HashMap<String, usize>,
    }
    let window_secs = window.as_secs();
    let mut per_budget: HashMap<&'static str, (Budget, Acc)> = HashMap::new();
    for e in entries {
        let tag = budget_tag(e.budget);
        let slot = per_budget
            .entry(tag)
            .or_insert_with(|| (e.budget, Acc::default()));
        if e.spent {
            slot.1.spent += 1;
            *slot.1.callers.entry(e.caller.clone()).or_insert(0) += 1;
        } else {
            slot.1.failed += 1;
        }
    }
    // Stable output order: graphql before rest.
    let mut rates: Vec<BudgetRate> = per_budget
        .into_values()
        .map(|(budget, acc)| {
            let mut top_callers: Vec<(String, usize)> = acc.callers.into_iter().collect();
            top_callers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            BudgetRate {
                budget,
                count: acc.spent,
                failed: acc.failed,
                window_secs,
                top_callers,
            }
        })
        .collect();
    rates.sort_by_key(|r| match r.budget {
        Budget::Graphql => 0,
        Budget::Rest => 1,
    });
    rates
}

/// Convenience: the per-budget rate summary over the last `window` before `now`,
/// reading the log directly. The one call both diagnostics make.
pub fn rates_in_window(window: Duration, now: DateTime<Utc>) -> Vec<BudgetRate> {
    summarize(&recent_entries(window, now), window)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct IsolatedHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        home: PathBuf,
    }
    impl IsolatedHome {
        fn new(tag: &str) -> Self {
            let lock = crate::test_lock::LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-gh-requests-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&home).unwrap();
            let prev = std::env::var("SHELBI_HOME").ok();
            std::env::set_var("SHELBI_HOME", &home);
            Self {
                _lock: lock,
                prev,
                home,
            }
        }
    }
    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn record_then_read_round_trips_and_filters_by_window() {
        let _iso = IsolatedHome::new("round-trip");
        let now = Utc::now();
        // Two recent, one well outside the window.
        record_request_at(Budget::Graphql, "board-refresh", Outcome::Ok, now - chrono::Duration::seconds(10));
        record_request_at(Budget::Rest, "write", Outcome::Ok, now - chrono::Duration::seconds(20));
        record_request_at(Budget::Graphql, "board-refresh", Outcome::Ok, now - chrono::Duration::seconds(7200));

        let recent = recent_entries(Duration::from_secs(3600), now);
        assert_eq!(recent.len(), 2, "the 2h-old entry is outside the 1h window");
        assert!(recent.iter().any(|e| e.budget == Budget::Rest && e.caller == "write" && e.spent));
    }

    #[test]
    fn outcome_field_round_trips_and_separates_spent_from_failed() {
        let _iso = IsolatedHome::new("outcome");
        let now = Utc::now();
        // Two spent GraphQL fetches, three failed connection attempts.
        record_request_at(Budget::Graphql, "issue-fetch", Outcome::Ok, now);
        record_request_at(Budget::Graphql, "issue-fetch", Outcome::Ok, now);
        for _ in 0..3 {
            record_request_at(Budget::Graphql, "issue-fetch", Outcome::Err("conn"), now);
        }
        let recent = recent_entries(Duration::from_secs(60), now);
        assert_eq!(recent.iter().filter(|e| e.spent).count(), 2, "two spent");
        let failed: Vec<&RequestEntry> = recent.iter().filter(|e| !e.spent).collect();
        assert_eq!(failed.len(), 3, "three failed attempts");
        assert!(failed.iter().all(|e| e.err_class.as_deref() == Some("conn")));

        // The rate summary counts spend and failure separately, so a dead network
        // never inflates the projected spend rate.
        let rate = summarize(&recent, Duration::from_secs(60));
        let gql = rate.iter().find(|r| r.budget == Budget::Graphql).unwrap();
        assert_eq!(gql.count, 2, "spent count drives the projection");
        assert_eq!(gql.failed, 3, "failed attempts reported separately");
    }

    #[test]
    fn an_old_line_with_no_outcome_reads_as_spent() {
        // Backward compatibility: a line written before the `outcome=` field
        // counts as spent, so historical logs still project correctly.
        let e = parse_line("2026-09-08T13:00:00Z budget=graphql caller=issue-fetch").unwrap();
        assert!(e.spent, "a field-less line is spent");
        assert_eq!(e.err_class, None);
    }

    #[test]
    fn summarize_counts_per_budget_and_ranks_callers() {
        let now = Utc::now();
        let spent = |budget, caller: &str| RequestEntry {
            at: now,
            budget,
            caller: caller.into(),
            spent: true,
            err_class: None,
        };
        let entries = vec![
            spent(Budget::Graphql, "board-refresh"),
            spent(Budget::Graphql, "board-refresh"),
            spent(Budget::Graphql, "issue-fetch"),
            spent(Budget::Rest, "write"),
        ];
        let rates = summarize(&entries, Duration::from_secs(60));
        assert_eq!(rates.len(), 2);
        // graphql sorts first.
        assert_eq!(rates[0].budget, Budget::Graphql);
        assert_eq!(rates[0].count, 3);
        assert_eq!(rates[0].top_callers[0], ("board-refresh".to_string(), 2));
        assert_eq!(rates[1].budget, Budget::Rest);
        assert_eq!(rates[1].count, 1);
    }

    #[test]
    fn exhaustion_projects_from_rate_and_remaining() {
        let rate = BudgetRate {
            budget: Budget::Rest,
            count: 600,
            failed: 0,
            window_secs: 60,
            top_callers: vec![("pollers".into(), 600)],
        };
        // 10 req/s. With 5,000 remaining → 500s ≈ 8.3 min.
        assert!((rate.per_sec() - 10.0).abs() < 1e-9);
        let secs = rate.seconds_to_exhaustion(Some(5_000)).unwrap();
        assert!((secs - 500.0).abs() < 1e-6, "5000 / 10 = 500s, got {secs}");
        // Unknown remaining falls back to the full limit.
        assert!(rate.seconds_to_exhaustion(None).is_some());
    }

    #[test]
    fn a_zero_rate_projects_no_exhaustion() {
        let rate = BudgetRate {
            budget: Budget::Graphql,
            count: 0,
            failed: 0,
            window_secs: 60,
            top_callers: Vec::new(),
        };
        assert_eq!(rate.per_sec(), 0.0);
        assert_eq!(rate.seconds_to_exhaustion(Some(10)), None);
    }

    #[test]
    fn a_caller_with_whitespace_is_sanitized_to_one_token() {
        let _iso = IsolatedHome::new("sanitize");
        let now = Utc::now();
        record_request_at(Budget::Graphql, "board refresh tick", Outcome::Ok, now);
        let recent = recent_entries(Duration::from_secs(60), now);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].caller, "board-refresh-tick", "spaces collapsed so the token stays whole");
    }
}
