//! Per-token GitHub rate-limit budget, shared hub-wide through a small file.
//!
//! ## The failure this exists to stop
//!
//! Before this module, when the primary REST budget (5,000/hr/token) was
//! exhausted every board read failed with a 403 and every caller retried on its
//! own cadence — six poller threads on a 5s tick, the sidebar, the daemon drain,
//! each CLI invocation — so `tui.log` filled with a thousand-plus 403s an hour
//! and `events.log` with `orphaned-*-reaped` lines driven by those failed reads.
//! Nothing looked at the rate-limit headers, so nothing knew *when* the budget
//! would reset, and nothing backed off until it did.
//!
//! ## What it does
//!
//! A rate-limit 403/429 response **parks** all reads on the affected budget for
//! that token until its reset time. While parked, a read short-circuits before
//! ever spawning `gh` — it returns the same typed rate-limit [`Error`] the real
//! call would, so the cache keeps serving its last snapshot marked stale, and no
//! new API request is made. The park is recorded in a tiny per-token JSON file
//! under `<shelbi-root>/gh-budget/`, so it is honored across every process on
//! the hub (daemon, panes, CLI), not just the one that hit the limit. The file
//! is keyed by a **hash** of the token, never the token itself.
//!
//! ## Two budgets, per token, hub-wide (plan Phase 3 §6)
//!
//! GitHub prices GraphQL reads (the board index and single-issue fetches) on a
//! **separate** 5,000-points-per-hour budget from the REST requests that writes
//! use. This file therefore carries **two** [`BudgetTier`]s — `graphql` and
//! `rest` — each with its own `remaining` / `reset_at` (recorded from every
//! response: `rateLimit { … }` in GraphQL, `x-ratelimit-*` headers from `gh api
//! --include` on REST) and its own `parked_until`. The daemon's governor reads
//! `graphql` to scale (or pause) the index-refresh tick; the write path reads
//! `rest` to reserve a floor for mutations. Several projects on one token share
//! one file, so the governor is per token, not per project.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Subdirectory of the shelbi root holding the per-token budget files.
const BUDGET_DIR: &str = "gh-budget";

/// Park window used only when the reset time can't be determined at all (the
/// error carried no reset header and the free `rate_limit` probe also failed).
/// Short so a wrongly-guessed park self-corrects quickly; the common path parks
/// until the real reset, minutes away, so this fallback rarely fires.
pub const DEFAULT_PARK_SECS: i64 = 60;

/// Which of a token's two independent budgets a call concerns: GraphQL reads
/// (the board index and single-issue fetches) or REST requests (writes and the
/// REST fallback list). Selects the [`BudgetTier`] every tier-aware function
/// reads or updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// The GraphQL points budget — the board-index reader and single-issue
    /// fetches.
    Graphql,
    /// The REST requests budget — mutations and the REST fallback list.
    Rest,
}

/// One budget's persisted state (GraphQL *or* REST). Both tiers share this shape
/// so the governor and the write reserve read the same fields off whichever one
/// they care about.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetTier {
    /// Requests/points remaining in the current window, last time a response
    /// surfaced it. The governor scales the tick off the `graphql` value; the
    /// write reserve refuses below a floor off the `rest` value.
    pub remaining: Option<i64>,
    /// When the current window resets (epoch seconds).
    pub reset_at: Option<i64>,
    /// While set and in the future, every read on this budget short-circuits
    /// without calling `gh`. Cleared implicitly: once `now` passes it,
    /// [`park_verdict`] reports not-parked and reads resume — no file rewrite
    /// needed.
    pub parked_until: Option<i64>,
}

/// One token's persisted rate-limit state: its GraphQL and REST budgets.
///
/// `#[serde(default)]` on each tier means a file written by an older shelbi
/// (which stored a single flat `remaining`/`reset_at`/`parked_until`) reads back
/// as two default (not-parked) tiers — dropping any in-flight park from before
/// the upgrade, which is harmless: the next live 403 re-parks.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RateLimitState {
    /// The GraphQL points budget.
    #[serde(default)]
    pub graphql: BudgetTier,
    /// The REST requests budget.
    #[serde(default)]
    pub rest: BudgetTier,
}

impl RateLimitState {
    /// The tier for `budget` — the read-side accessor the governor and reserve
    /// use.
    pub fn tier(&self, budget: Budget) -> &BudgetTier {
        match budget {
            Budget::Graphql => &self.graphql,
            Budget::Rest => &self.rest,
        }
    }

    /// The mutable tier for `budget`, for a record/park read-modify-write.
    fn tier_mut(&mut self, budget: Budget) -> &mut BudgetTier {
        match budget {
            Budget::Graphql => &mut self.graphql,
            Budget::Rest => &mut self.rest,
        }
    }
}

/// Rate-limit numbers parsed out of a `gh api --include` response's header
/// block. Both fields are optional so a partial or reordered header set still
/// yields whatever was present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitHeaders {
    /// `x-ratelimit-remaining`.
    pub remaining: Option<i64>,
    /// `x-ratelimit-reset` (epoch seconds).
    pub reset_at: Option<i64>,
}

/// Parse `x-ratelimit-remaining` / `x-ratelimit-reset` (case-insensitive) out of
/// a raw HTTP header block — the leading lines `gh api --include` prints before
/// the JSON body. Header names are matched case-insensitively (HTTP/2 lowercases
/// them, HTTP/1.1 title-cases them) and only the first occurrence of each wins.
/// Lines without a `:` and non-matching headers are ignored, so passing the
/// whole `--include` output (headers *and* body) is safe.
pub fn parse_rate_limit_headers(raw: &str) -> RateLimitHeaders {
    let mut out = RateLimitHeaders::default();
    for line in raw.lines() {
        // A blank line ends the header block in an HTTP response; stop so a
        // JSON body that happens to contain the substring can't be misread.
        if line.trim().is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "x-ratelimit-remaining" if out.remaining.is_none() => {
                out.remaining = value.parse().ok();
            }
            "x-ratelimit-reset" if out.reset_at.is_none() => {
                out.reset_at = value.parse().ok();
            }
            _ => {}
        }
    }
    out
}

/// Stable per-token filename key. Hashes the secret with a fixed-seed hasher so
/// the same token always maps to the same file *and the token itself is never
/// written to disk*. Not cryptographic — it only has to avoid collisions across
/// the handful of tokens one hub uses and stay stable across runs (which
/// `DefaultHasher::new`, seeded with fixed keys, does).
pub fn token_key(secret: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    secret.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The budget file for `key`, or `None` when the shelbi root can't be resolved
/// (then parking is simply disabled — reads behave exactly as they did before
/// this module, failing fast on a 403 with no park).
fn budget_path(key: &str) -> Option<PathBuf> {
    crate::shelbi_home()
        .ok()
        .map(|home| home.join(BUDGET_DIR).join(format!("{key}.json")))
}

/// Read the persisted state for `key`. Best-effort: a missing, unreadable, or
/// corrupt file reads as the default (not parked), so a torn write can never
/// wedge reads off — the worst case is one more live 403 that re-parks.
pub fn read_state(key: &str) -> RateLimitState {
    budget_path(key)
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Persist `state` for `key` atomically (temp file + rename). Best-effort: a
/// serialize/IO failure is swallowed — the next read just goes live and may
/// re-park.
fn write_state(key: &str, state: &RateLimitState) {
    let Some(path) = budget_path(key) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = crate::ensure_dir(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(state) {
        let _ = crate::atomic_write(&path, &bytes);
    }
}

/// The pure park decision: given a tier and the current epoch, the reset time
/// reads are parked until, or `None` when the budget is free. Split out so the
/// "is this budget parked right now" rule is unit-testable without the file.
pub fn park_verdict(tier: &BudgetTier, now: i64) -> Option<i64> {
    match tier.parked_until {
        Some(until) if until > now => Some(until),
        _ => None,
    }
}

/// Whether `budget` is currently parked for `key`, and until when (epoch
/// seconds). Reads the file each call; reads are cache-gated so this is not on a
/// hot path.
pub fn parked_until(key: &str, budget: Budget, now: i64) -> Option<i64> {
    park_verdict(read_state(key).tier(budget), now)
}

/// Process-global lock serializing budget-file writes so N concurrent threads
/// that all see a 403 in the same window produce exactly **one** park transition
/// (and therefore one events.log line) rather than one per thread, and so a
/// `park` and a `record` on the two tiers never lose each other's write.
/// Cross-process racers still dedupe through the file check inside the lock, with
/// only a small window; within a process this makes it exact.
fn park_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Park `budget` for `key` until `reset_at`. Returns `true` **iff this call
/// transitioned the budget from not-parked to parked** — the signal the caller
/// uses to log the `board rate-limited` line exactly once per window. A call
/// that finds the budget already parked returns `false` and rewrites nothing.
///
/// `reset_at` is clamped to strictly after `now` so a stale/past reset can't
/// produce a park that's already expired (which would re-fire on the next read).
pub fn park(key: &str, budget: Budget, reset_at: i64, now: i64) -> bool {
    let _guard = park_lock().lock();
    let mut state = read_state(key);
    if park_verdict(state.tier(budget), now).is_some() {
        return false; // already parked this window
    }
    let tier = state.tier_mut(budget);
    tier.reset_at = Some(reset_at);
    tier.parked_until = Some(reset_at.max(now + 1));
    write_state(key, &state);
    true
}

/// Record the latest `remaining` / `reset_at` seen for `budget`, without
/// parking. Keeps the file the single source the governor and write reserve
/// read. No-op when neither is present.
pub fn record(key: &str, budget: Budget, remaining: Option<i64>, reset_at: Option<i64>) {
    if remaining.is_none() && reset_at.is_none() {
        return;
    }
    let _guard = park_lock().lock();
    let mut state = read_state(key);
    let tier = state.tier_mut(budget);
    if remaining.is_some() {
        tier.remaining = remaining;
    }
    if reset_at.is_some() {
        tier.reset_at = reset_at;
    }
    write_state(key, &state);
}

/// Record REST headers ([`parse_rate_limit_headers`] output) into the `rest`
/// tier — the convenience the write path and the `/rate_limit` probe use.
pub fn record_rest_headers(key: &str, headers: &RateLimitHeaders) {
    record(key, Budget::Rest, headers.remaining, headers.reset_at);
}

/// Clear any park recorded for `key` on both budgets. Test-support and an
/// explicit "resume now" hook; production relies on the implicit expiry in
/// [`park_verdict`].
#[cfg(any(test, feature = "test-support"))]
pub fn clear(key: &str) {
    let _guard = park_lock().lock();
    write_state(key, &RateLimitState::default());
}

// --- the governor (plan Phase 3 §6) ------------------------------------------

/// The daemon's per-tick decision for a project's board-index refresh, from the
/// GraphQL budget behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickPlan {
    /// Run the refresh this tick, then wait `interval` before the next — the
    /// configured cadence in the healthy band, or the throttled cadence in the
    /// middle band.
    Refresh(Duration),
    /// Skip the refresh; the GraphQL budget is too low (or parked) to spend on a
    /// list read. `until` is the epoch the daemon should hold off until — the
    /// budget's reset (or the park expiry) — after which it resumes optimistically
    /// (the reset refills the budget). Consumers keep serving the last index,
    /// marked stale.
    Pause { until: i64 },
}

/// The governor thresholds, resolved from `issue_tracker.budget` + the project's
/// refresh cadence. Passed to [`tick_plan`] so the decision stays a pure
/// function of numbers, unit-testable without config or the clock.
#[derive(Debug, Clone, Copy)]
pub struct BudgetThresholds {
    /// GraphQL points above which `base_secs` runs unthrottled.
    pub graphql_high: u64,
    /// GraphQL points down to which the daemon slows (rather than pauses).
    pub graphql_medium: u64,
    /// The configured refresh cadence (seconds).
    pub base_secs: u64,
    /// The throttled cadence (seconds) for the middle band.
    pub slow_secs: u64,
}

/// Decide this tick from the GraphQL `tier` and the `thresholds` (plan §6 table):
///
/// | GraphQL remaining | plan |
/// | --- | --- |
/// | parked | pause until the park expires |
/// | `> high` (or unknown) | refresh at `base_secs` |
/// | `medium ..= high` | refresh at `slow_secs` |
/// | `< medium`, reset in the future | pause until `reset_at` |
/// | `< medium`, reset passed/unknown | refresh at `base_secs` (budget refilled) |
///
/// The last row is what lets a paused project self-heal: once the window resets
/// the low `remaining` is stale, so the governor refreshes once, which reads the
/// live `rateLimit` and repopulates the budget — back to the healthy band.
pub fn tick_plan(tier: &BudgetTier, thresholds: &BudgetThresholds, now: i64) -> TickPlan {
    // A park (from an actual 403/429) is the hardest signal: hold off until it
    // expires regardless of the last-seen `remaining`.
    if let Some(until) = park_verdict(tier, now) {
        return TickPlan::Pause { until };
    }
    let base = TickPlan::Refresh(Duration::from_secs(thresholds.base_secs));
    // An unknown budget (never read, or a cold hub) runs at the configured
    // cadence — the first read populates it.
    let Some(remaining) = tier.remaining else {
        return base;
    };
    let high = thresholds.graphql_high as i64;
    let medium = thresholds.graphql_medium as i64;
    if remaining > high {
        base
    } else if remaining > medium {
        TickPlan::Refresh(Duration::from_secs(thresholds.slow_secs))
    } else {
        // Low band: pause until the reset, then resume optimistically. A stale
        // (passed) or absent reset means the window has almost certainly rolled,
        // so refresh rather than pause forever on a number we can no longer trust.
        match tier.reset_at {
            Some(reset) if reset > now => TickPlan::Pause { until: reset },
            _ => base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lowercase_http2_style_headers() {
        let raw = "HTTP/2.0 403 Forbidden\n\
                   x-ratelimit-limit: 5000\n\
                   x-ratelimit-remaining: 0\n\
                   x-ratelimit-reset: 1700000000\n\
                   content-type: application/json\n\
                   \n\
                   {\"message\":\"API rate limit exceeded\"}";
        let h = parse_rate_limit_headers(raw);
        assert_eq!(h.remaining, Some(0));
        assert_eq!(h.reset_at, Some(1_700_000_000));
    }

    #[test]
    fn parses_titlecase_http1_style_headers() {
        let raw = "HTTP/1.1 200 OK\r\n\
                   X-RateLimit-Remaining: 4321\r\n\
                   X-RateLimit-Reset: 1699999999\r\n";
        let h = parse_rate_limit_headers(raw);
        assert_eq!(h.remaining, Some(4321));
        assert_eq!(h.reset_at, Some(1_699_999_999));
    }

    #[test]
    fn missing_headers_yield_none_not_zero() {
        let h = parse_rate_limit_headers("HTTP/2.0 200 OK\ncontent-type: application/json\n");
        assert_eq!(h.remaining, None);
        assert_eq!(h.reset_at, None);
    }

    #[test]
    fn stops_at_the_blank_line_so_the_body_is_never_scanned() {
        // A reset header appearing *inside* the JSON body must not be read as a
        // real header once the blank line has ended the header block.
        let raw = "HTTP/2.0 200 OK\n\
                   x-ratelimit-remaining: 10\n\
                   \n\
                   {\"x-ratelimit-reset\": 42}";
        let h = parse_rate_limit_headers(raw);
        assert_eq!(h.remaining, Some(10));
        assert_eq!(h.reset_at, None, "the body's field is not a header");
    }

    #[test]
    fn first_header_occurrence_wins() {
        let raw = "x-ratelimit-remaining: 5\nx-ratelimit-remaining: 999\n";
        assert_eq!(parse_rate_limit_headers(raw).remaining, Some(5));
    }

    #[test]
    fn park_verdict_is_parked_only_while_reset_is_in_the_future() {
        let tier = BudgetTier {
            parked_until: Some(1_000),
            ..Default::default()
        };
        assert_eq!(park_verdict(&tier, 999), Some(1_000), "before reset: parked");
        assert_eq!(park_verdict(&tier, 1_000), None, "at reset: not parked");
        assert_eq!(park_verdict(&tier, 1_001), None, "past reset: not parked");
    }

    #[test]
    fn park_verdict_is_never_parked_without_a_marker() {
        assert_eq!(park_verdict(&BudgetTier::default(), 0), None);
    }

    #[test]
    fn token_key_is_stable_and_hides_the_secret() {
        let secret = "ghp_supersecrettoken";
        let key = token_key(secret);
        assert_eq!(key, token_key(secret), "same token → same key across calls");
        assert!(!key.contains("secret"), "the key must not embed the token");
        assert_ne!(key, token_key("a-different-token"));
    }

    // --- governor bands -------------------------------------------------------

    fn thresholds() -> BudgetThresholds {
        BudgetThresholds {
            graphql_high: 2_000,
            graphql_medium: 500,
            base_secs: 30,
            slow_secs: 120,
        }
    }

    fn tier(remaining: Option<i64>, reset_at: Option<i64>) -> BudgetTier {
        BudgetTier {
            remaining,
            reset_at,
            parked_until: None,
        }
    }

    #[test]
    fn governor_unknown_budget_uses_the_base_cadence() {
        assert_eq!(
            tick_plan(&tier(None, None), &thresholds(), 1_000),
            TickPlan::Refresh(Duration::from_secs(30)),
        );
    }

    #[test]
    fn governor_high_band_uses_the_base_cadence() {
        // > 2,000 → configured 30s.
        assert_eq!(
            tick_plan(&tier(Some(2_500), None), &thresholds(), 1_000),
            TickPlan::Refresh(Duration::from_secs(30)),
        );
    }

    #[test]
    fn governor_middle_band_slows_to_120s() {
        // The acceptance seed: 1,500 remaining stretches the tick to 120s.
        assert_eq!(
            tick_plan(&tier(Some(1_500), None), &thresholds(), 1_000),
            TickPlan::Refresh(Duration::from_secs(120)),
        );
    }

    #[test]
    fn governor_low_band_pauses_until_the_reset() {
        // 300 remaining with a future reset pauses the index refresh.
        assert_eq!(
            tick_plan(&tier(Some(300), Some(5_000)), &thresholds(), 1_000),
            TickPlan::Pause { until: 5_000 },
        );
    }

    #[test]
    fn governor_low_band_with_a_passed_reset_resumes() {
        // The window has rolled: the low `remaining` is stale, so refresh once to
        // repopulate rather than pause forever.
        assert_eq!(
            tick_plan(&tier(Some(50), Some(900)), &thresholds(), 1_000),
            TickPlan::Refresh(Duration::from_secs(30)),
        );
    }

    #[test]
    fn governor_pauses_a_parked_budget_regardless_of_remaining() {
        let parked = BudgetTier {
            remaining: Some(4_999),
            reset_at: Some(9_000),
            parked_until: Some(4_000),
        };
        assert_eq!(
            tick_plan(&parked, &thresholds(), 1_000),
            TickPlan::Pause { until: 4_000 },
        );
    }

    /// End-to-end park lifecycle against a real budget file, per tier: the first
    /// 403 in a window transitions to parked (returns `true`, so the caller logs
    /// the one `board rate-limited` line), a second 403 in the same window does
    /// not (returns `false`, so no duplicate line), the budget reads as parked
    /// until its reset, and the *other* tier is untouched by the park.
    #[test]
    fn park_transitions_once_per_window_per_tier_and_leaves_the_other_alone() {
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-gh-budget-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let key = token_key("a-token");
        assert_eq!(parked_until(&key, Budget::Graphql, 100), None, "clean: not parked");

        assert!(
            park(&key, Budget::Graphql, 500, 100),
            "first 403 in the window transitions to parked"
        );
        assert!(
            !park(&key, Budget::Graphql, 500, 200),
            "a second 403 in the same window must not re-transition (no duplicate log)"
        );

        assert_eq!(parked_until(&key, Budget::Graphql, 400), Some(500), "parked until reset");
        assert_eq!(parked_until(&key, Budget::Graphql, 500), None, "at reset, reads resume");
        // The REST tier was never parked by the GraphQL 403.
        assert_eq!(parked_until(&key, Budget::Rest, 400), None, "other tier untouched");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `record` updates one tier's numbers without clobbering the other's, and
    /// persists across a read — the property the governor and write reserve rely
    /// on when GraphQL and REST responses interleave.
    #[test]
    fn record_updates_one_tier_and_preserves_the_other() {
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-gh-budget-record-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let key = token_key("rec-token");
        record(&key, Budget::Graphql, Some(1_500), Some(9_000));
        record_rest_headers(
            &key,
            &RateLimitHeaders {
                remaining: Some(42),
                reset_at: Some(8_000),
            },
        );

        let state = read_state(&key);
        assert_eq!(state.graphql.remaining, Some(1_500));
        assert_eq!(state.graphql.reset_at, Some(9_000));
        assert_eq!(state.rest.remaining, Some(42), "REST recorded independently");
        assert_eq!(state.rest.reset_at, Some(8_000));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
