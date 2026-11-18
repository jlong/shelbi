//! Linear rate-limit bookkeeping: a per-key budget file and a hub-wide
//! request log, the Linear counterparts of `gh_budget` / `gh_requests`
//! (simpler: Linear has two buckets per key and one reset per bucket).
//!
//! * `<shelbi-root>/linear-budget/<key-hash>.json`: the latest
//!   `X-RateLimit-Requests-*` / `X-RateLimit-Complexity-*` snapshot plus a
//!   `parked_until` set by a `RATELIMITED` response. The file is named by a
//!   hash of the key, never the key.
//! * `<shelbi-root>/linear-requests.log`: one line per attempt,
//!   `<rfc3339> op=<Operation> outcome=<ok|http<status>|err:<class>> ms=<n> caller=<name>`.
//!
//! Both are best-effort: a write failure is swallowed so bookkeeping can never
//! fail an API call.

use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::error::from_epoch;
use crate::gh_budget::{BudgetTier, TickPlan};
use super::transport::LinearResponse;

/// Directory (under the shelbi root) holding one budget file per API key.
pub const BUDGET_DIR: &str = "linear-budget";
/// Basename of the hub-wide request log under the shelbi root.
pub const REQUESTS_LOG_FILE: &str = "linear-requests.log";

/// How far into the future a park may reach, whatever the headers say.
const MAX_PARK_SECS: i64 = 60 * 60;

/// One rate-limit bucket's latest snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    /// When the bucket refills, epoch seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
}

impl BucketSnapshot {
    fn is_empty(&self) -> bool {
        *self == BucketSnapshot::default()
    }

    /// Fold a newer reading in, keeping fields the newer one lacks.
    fn merge(&mut self, newer: &BucketSnapshot) {
        self.limit = newer.limit.or(self.limit);
        self.remaining = newer.remaining.or(self.remaining);
        self.reset_at = newer.reset_at.or(self.reset_at);
    }

    /// The reset time when the bucket is exhausted.
    fn exhausted_reset(&self) -> Option<i64> {
        (self.remaining == Some(0)).then_some(self.reset_at).flatten()
    }
}

/// The rate-limit headers of one response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinearRateHeaders {
    /// `X-RateLimit-Requests-{Limit,Remaining,Reset}`.
    pub requests: BucketSnapshot,
    /// `X-RateLimit-Complexity-{Limit,Remaining,Reset}`.
    pub complexity: BucketSnapshot,
    /// `X-Complexity`: what this request cost.
    pub cost: Option<u64>,
}

impl LinearRateHeaders {
    /// Read the rate-limit headers off a response. Linear sends resets as
    /// epoch **milliseconds**; a value that already looks like seconds is kept.
    pub fn parse(resp: &LinearResponse) -> Self {
        let num = |name: &str| resp.header(name).and_then(|v| v.trim().parse::<u64>().ok());
        let reset = |name: &str| {
            num(name).map(|v| {
                let v = v as i64;
                if v > 100_000_000_000 {
                    v / 1000
                } else {
                    v
                }
            })
        };
        LinearRateHeaders {
            requests: BucketSnapshot {
                limit: num("X-RateLimit-Requests-Limit"),
                remaining: num("X-RateLimit-Requests-Remaining"),
                reset_at: reset("X-RateLimit-Requests-Reset"),
            },
            complexity: BucketSnapshot {
                limit: num("X-RateLimit-Complexity-Limit"),
                remaining: num("X-RateLimit-Complexity-Remaining"),
                reset_at: reset("X-RateLimit-Complexity-Reset"),
            },
            cost: num("X-Complexity"),
        }
    }

    /// When calls may resume after a rate-limit error: the reset of an
    /// exhausted bucket, else the latest reset reported, else `None`.
    pub fn resume_at(&self) -> Option<DateTime<Utc>> {
        let exhausted = [self.requests.exhausted_reset(), self.complexity.exhausted_reset()]
            .into_iter()
            .flatten()
            .max();
        exhausted
            .or_else(|| [self.requests.reset_at, self.complexity.reset_at].into_iter().flatten().max())
            .and_then(from_epoch)
    }
}

/// The persisted budget for one API key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearBudget {
    #[serde(default, skip_serializing_if = "BucketSnapshot::is_empty")]
    pub requests: BucketSnapshot,
    #[serde(default, skip_serializing_if = "BucketSnapshot::is_empty")]
    pub complexity: BucketSnapshot,
    /// Calls fail fast until this time (epoch seconds), set by a rate limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_until: Option<i64>,
    /// When the snapshot was last written (epoch seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

impl LinearBudget {
    /// The park-until time when it is still in the future.
    pub fn parked_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.parked_until
            .filter(|&t| t > now.timestamp())
            .and_then(from_epoch)
    }
}

/// Below this share (percent) of a bucket's limit the daemon's board refresh
/// slows to `issue_tracker.budget.slow_refresh_secs`. The GitHub governor's
/// defaults (2,000 / 500 of 5,000 points) are the same shares.
pub const LINEAR_SLOW_BELOW_PCT: u64 = 40;
/// Below this share (percent) of a bucket's limit the board refresh pauses
/// until that bucket resets.
pub const LINEAR_PAUSE_BELOW_PCT: u64 = 10;

/// Which of a key's two rate-limit buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinearBucket {
    Requests,
    Complexity,
}

impl LinearBucket {
    /// The lowercase name used in status lines and events.
    pub fn as_str(self) -> &'static str {
        match self {
            LinearBucket::Requests => "requests",
            LinearBucket::Complexity => "complexity",
        }
    }
}

impl BucketSnapshot {
    /// Remaining as a percentage of the limit, when both are known.
    pub fn percent_left(&self) -> Option<u64> {
        match (self.remaining, self.limit) {
            (Some(r), Some(l)) if l > 0 => Some(r.saturating_mul(100) / l),
            _ => None,
        }
    }
}

impl LinearBudget {
    /// The bucket closest to running out: the lower share of its limit left,
    /// else (no limits known) the lower remaining. `None` when neither bucket
    /// has reported anything.
    pub fn tightest(&self) -> Option<(LinearBucket, BucketSnapshot)> {
        let buckets = [
            (LinearBucket::Requests, self.requests),
            (LinearBucket::Complexity, self.complexity),
        ];
        let known = buckets.into_iter().filter(|(_, b)| b.remaining.is_some());
        known.min_by_key(|(_, b)| (b.percent_left().unwrap_or(u64::MAX), b.remaining))
    }

    /// The budget as the GitHub governor's [`BudgetTier`] shape: the tightest
    /// bucket's remaining and reset, plus the park. What the daemon stamps on
    /// a stale board index, so the `quota resets HH:MM` banner works the same
    /// for both trackers.
    pub fn tier(&self) -> BudgetTier {
        let tight = self.tightest().map(|(_, b)| b).unwrap_or_default();
        BudgetTier {
            remaining: tight.remaining.map(|r| r as i64),
            reset_at: tight.reset_at,
            parked_until: self.parked_until,
            backoff_secs: None,
        }
    }
}

/// The daemon's board-refresh decision from a Linear key's budget: the Linear
/// twin of [`crate::gh_budget::tick_plan`], banded by share of each bucket's
/// limit rather than GitHub's point counts.
///
/// | state | plan |
/// | --- | --- |
/// | parked (a `RATELIMITED` reply) | pause until the park expires |
/// | unknown (no response read yet) | refresh every `base_secs` |
/// | tightest bucket above [`LINEAR_SLOW_BELOW_PCT`] | refresh every `base_secs` |
/// | above [`LINEAR_PAUSE_BELOW_PCT`] | refresh every `slow_secs` |
/// | below, reset still ahead | pause until that bucket resets |
/// | below, reset passed or unknown | refresh every `base_secs` (the window rolled) |
pub fn linear_tick_plan(budget: &LinearBudget, base_secs: u64, slow_secs: u64, now: i64) -> TickPlan {
    if let Some(until) = budget.parked_until.filter(|&t| t > now) {
        return TickPlan::Pause { until };
    }
    let base = TickPlan::Refresh(std::time::Duration::from_secs(base_secs));
    let Some((_, bucket)) = budget.tightest() else {
        return base;
    };
    let low = match bucket.percent_left() {
        Some(pct) if pct >= LINEAR_SLOW_BELOW_PCT => return base,
        Some(pct) if pct >= LINEAR_PAUSE_BELOW_PCT => {
            return TickPlan::Refresh(std::time::Duration::from_secs(slow_secs.max(base_secs)))
        }
        Some(_) => true,
        // No limit header: only an empty bucket is known to be low.
        None => bucket.remaining == Some(0),
    };
    match bucket.reset_at {
        Some(reset) if low && reset > now => TickPlan::Pause { until: reset },
        _ => base,
    }
}

fn budget_path(key_hash: &str) -> Option<PathBuf> {
    crate::shelbi_home()
        .ok()
        .map(|home| home.join(BUDGET_DIR).join(format!("{key_hash}.json")))
}

/// Read the budget for a key hash ([`super::LinearClient::budget_key`]). A
/// missing or corrupt file reads as the default.
pub fn read_budget(key_hash: &str) -> LinearBudget {
    budget_path(key_hash)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_budget(key_hash: &str, budget: &LinearBudget) {
    let Some(path) = budget_path(key_hash) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = crate::ensure_dir(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(budget) {
        let _ = crate::atomic_write(&path, &bytes);
    }
}

/// Fold a response's headers into the budget file. A response that carries
/// none leaves the file untouched.
pub(crate) fn record_headers(key_hash: &str, headers: &LinearRateHeaders, now: DateTime<Utc>) {
    if headers.requests.is_empty() && headers.complexity.is_empty() {
        return;
    }
    let mut budget = read_budget(key_hash);
    budget.requests.merge(&headers.requests);
    budget.complexity.merge(&headers.complexity);
    budget.updated_at = Some(now.timestamp());
    write_budget(key_hash, &budget);
}

/// Park the key until `until` (capped at an hour out). Returns the time used.
pub(crate) fn park(key_hash: &str, until: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    let cap = now + chrono::Duration::seconds(MAX_PARK_SECS);
    let until = until.min(cap);
    let mut budget = read_budget(key_hash);
    budget.parked_until = Some(until.timestamp());
    budget.updated_at = Some(now.timestamp());
    write_budget(key_hash, &budget);
    until
}

/// Clear a park after a successful call.
pub(crate) fn clear_park(key_hash: &str) {
    let mut budget = read_budget(key_hash);
    if budget.parked_until.take().is_some() {
        write_budget(key_hash, &budget);
    }
}

fn log_path() -> Option<PathBuf> {
    crate::shelbi_home().ok().map(|h| h.join(REQUESTS_LOG_FILE))
}

fn log_token(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_whitespace() || c == '=' { '-' } else { c })
        .collect();
    if cleaned.is_empty() {
        "unknown".into()
    } else {
        cleaned
    }
}

/// Append one request line. `outcome` is `ok`, `http<status>` or `err:<class>`.
pub(crate) fn log_request(op: &str, outcome: &str, millis: u128, caller: &str, at: DateTime<Utc>) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = crate::ensure_dir(parent);
    }
    let line = format!(
        "{} op={} outcome={} ms={millis} caller={}\n",
        at.to_rfc3339(),
        log_token(op),
        log_token(outcome),
        log_token(caller),
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// One parsed request-log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestLogEntry {
    pub at: DateTime<Utc>,
    pub op: String,
    pub outcome: String,
    pub millis: u64,
    pub caller: String,
}

fn parse_line(line: &str) -> Option<RequestLogEntry> {
    let mut parts = line.split_whitespace();
    let at = DateTime::parse_from_rfc3339(parts.next()?).ok()?.with_timezone(&Utc);
    let mut entry = RequestLogEntry {
        at,
        op: String::new(),
        outcome: String::new(),
        millis: 0,
        caller: String::new(),
    };
    for field in parts {
        match field.split_once('=') {
            Some(("op", v)) => entry.op = v.into(),
            Some(("outcome", v)) => entry.outcome = v.into(),
            Some(("ms", v)) => entry.millis = v.parse().unwrap_or(0),
            Some(("caller", v)) => entry.caller = v.into(),
            _ => {}
        }
    }
    Some(entry)
}

/// Request-log entries no older than `window`, oldest first. Reads at most the
/// last 256 KiB of the log.
pub fn recent_requests(window: std::time::Duration, now: DateTime<Utc>) -> Vec<RequestLogEntry> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 256 * 1024;
    let Some(path) = log_path() else {
        return Vec::new();
    };
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return Vec::new();
    }
    let cutoff = now - chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::zero());
    let mut lines = buf.lines();
    if start > 0 {
        lines.next(); // a torn first line
    }
    lines
        .filter_map(parse_line)
        .filter(|e| e.at >= cutoff)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_client::test_home;

    fn now() -> DateTime<Utc> {
        from_epoch(1_800_000_000).unwrap()
    }

    fn headers(remaining: &str) -> LinearResponse {
        LinearResponse::ok("{}")
            .with_header("X-RateLimit-Requests-Limit", "1500")
            .with_header("X-RateLimit-Requests-Remaining", remaining)
            .with_header("X-RateLimit-Requests-Reset", "1800000600000")
            .with_header("x-ratelimit-complexity-limit", "250000")
            .with_header("x-ratelimit-complexity-remaining", "249000")
            .with_header("x-ratelimit-complexity-reset", "1800000300000")
            .with_header("X-Complexity", "12")
    }

    #[test]
    fn headers_parse_with_millisecond_resets() {
        let h = LinearRateHeaders::parse(&headers("1499"));
        assert_eq!(h.requests, BucketSnapshot { limit: Some(1500), remaining: Some(1499), reset_at: Some(1_800_000_600) });
        assert_eq!(h.complexity.reset_at, Some(1_800_000_300));
        assert_eq!(h.cost, Some(12));
        // Nothing exhausted: the latest reset.
        assert_eq!(h.resume_at(), from_epoch(1_800_000_600));
        assert_eq!(LinearRateHeaders::parse(&LinearResponse::ok("")), LinearRateHeaders::default());
    }

    #[test]
    fn resume_at_prefers_the_exhausted_bucket() {
        let mut h = LinearRateHeaders::parse(&headers("1499"));
        h.complexity.remaining = Some(0);
        assert_eq!(h.resume_at(), from_epoch(1_800_000_300));
    }

    #[test]
    fn budget_file_records_parks_and_clears() {
        let _home = test_home::fresh();
        let key = "abc123";
        assert_eq!(read_budget(key), LinearBudget::default());

        record_headers(key, &LinearRateHeaders::parse(&headers("1499")), now());
        let b = read_budget(key);
        assert_eq!(b.requests.remaining, Some(1499));
        assert_eq!(b.complexity.limit, Some(250_000));

        // A park is capped at an hour.
        let until = park(key, now() + chrono::Duration::days(1), now());
        assert_eq!(until, now() + chrono::Duration::seconds(MAX_PARK_SECS));
        assert_eq!(read_budget(key).parked_at(now()), Some(until));
        assert_eq!(read_budget(key).parked_at(until), None, "expired at its own time");

        // A header-less response keeps the snapshot; clear_park drops the park only.
        record_headers(key, &LinearRateHeaders::default(), now());
        clear_park(key);
        let b = read_budget(key);
        assert_eq!((b.parked_until, b.requests.remaining), (None, Some(1499)));
    }

    fn budget(req: (u64, u64), cx: (u64, u64), reset: i64) -> LinearBudget {
        LinearBudget {
            requests: BucketSnapshot { limit: Some(req.1), remaining: Some(req.0), reset_at: Some(reset) },
            complexity: BucketSnapshot { limit: Some(cx.1), remaining: Some(cx.0), reset_at: Some(reset + 60) },
            parked_until: None,
            updated_at: None,
        }
    }

    #[test]
    fn tightest_bucket_is_the_lower_share_of_its_limit() {
        // 1000/1500 requests (66%) vs 50k/250k complexity (20%): complexity binds.
        let b = budget((1000, 1500), (50_000, 250_000), 1_800_000_600);
        let (which, bucket) = b.tightest().unwrap();
        assert_eq!((which, bucket.remaining), (LinearBucket::Complexity, Some(50_000)));
        let tier = b.tier();
        assert_eq!((tier.remaining, tier.reset_at), (Some(50_000), Some(1_800_000_660)));
        assert_eq!(LinearBudget::default().tightest(), None);
    }

    #[test]
    fn tick_plan_bands_by_share_and_parks() {
        let now = 1_800_000_000;
        let reset = now + 600;
        let secs = |p: TickPlan| match p {
            TickPlan::Refresh(d) => Some(d.as_secs()),
            TickPlan::Pause { .. } => None,
        };
        // Unknown budget and a healthy one run at the base cadence.
        assert_eq!(secs(linear_tick_plan(&LinearBudget::default(), 60, 120, now)), Some(60));
        assert_eq!(secs(linear_tick_plan(&budget((1400, 1500), (240_000, 250_000), reset), 60, 120, now)), Some(60));
        // The middle band slows.
        assert_eq!(secs(linear_tick_plan(&budget((450, 1500), (240_000, 250_000), reset), 60, 120, now)), Some(120));
        // The low band pauses until the binding bucket resets.
        let low = budget((1400, 1500), (10_000, 250_000), reset);
        assert_eq!(linear_tick_plan(&low, 60, 120, now), TickPlan::Pause { until: reset + 60 });
        // ...but a passed reset means the window rolled: refresh.
        assert_eq!(secs(linear_tick_plan(&low, 60, 120, reset + 61)), Some(60));
        // A park wins over everything, until it expires.
        let mut parked = budget((1400, 1500), (240_000, 250_000), reset);
        parked.parked_until = Some(now + 30);
        assert_eq!(linear_tick_plan(&parked, 60, 120, now), TickPlan::Pause { until: now + 30 });
        assert_eq!(secs(linear_tick_plan(&parked, 60, 120, now + 31)), Some(60));
    }

    #[test]
    fn request_log_round_trips_and_windows() {
        let _home = test_home::fresh();
        log_request("Viewer", "ok", 42, "cli", now() - chrono::Duration::hours(2));
        log_request("Teams", "err:timeout", 30_000, "the daemon", now());
        let raw = std::fs::read_to_string(log_path().unwrap()).unwrap();
        assert!(raw.contains(" op=Teams outcome=err:timeout ms=30000 caller=the-daemon\n"), "{raw}");
        let recent = recent_requests(std::time::Duration::from_secs(3600), now());
        assert_eq!(recent.len(), 1);
        assert_eq!((recent[0].op.as_str(), recent[0].millis), ("Teams", 30_000));
    }
}
