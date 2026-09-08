//! Per-token GitHub rate-limit park state, shared hub-wide through a small file.
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
//! ## What it does (plan Phase 0, item 3)
//!
//! The first read-path 403/429 rate-limit response **parks** all board reads for
//! that token until its reset time. While parked, a read short-circuits before
//! ever spawning `gh` — it returns the same typed rate-limit [`Error`] the real
//! call would, so the cache keeps serving its last snapshot marked stale, and no
//! new API request is made. The park is recorded in a tiny per-token JSON file
//! under `<shelbi-root>/gh-budget/`, so it is honored across every process on
//! the hub (daemon, panes, CLI), not just the one that hit the limit. The file
//! is keyed by a **hash** of the token, never the token itself.
//!
//! Parking is *reactive* for Phase 0: it kicks in on the first 403, not on a
//! remaining-quota threshold. The proactive budget governor (adaptive tick,
//! reserve floors) is Phase 3; this module already records `remaining` /
//! `reset_at` when a header parse hands them over, so that governor has the data
//! it needs without a second store.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Subdirectory of the shelbi root holding the per-token budget files.
const BUDGET_DIR: &str = "gh-budget";

/// Park window used only when the reset time can't be determined at all (the
/// error carried no reset header and the free `rate_limit` probe also failed).
/// Short so a wrongly-guessed park self-corrects quickly; the common path parks
/// until the real reset, minutes away, so this fallback rarely fires.
pub const DEFAULT_PARK_SECS: i64 = 60;

/// One token's persisted rate-limit state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RateLimitState {
    /// Requests remaining in the current window, last time a header parse saw
    /// it. Informational for Phase 0; the Phase 3 governor reads it.
    pub remaining: Option<i64>,
    /// When the current window resets (epoch seconds), from `x-ratelimit-reset`.
    pub reset_at: Option<i64>,
    /// While set and in the future, every read short-circuits without calling
    /// `gh`. Cleared implicitly: once `now` passes it, [`park_verdict`] reports
    /// not-parked and reads resume — no file rewrite needed.
    pub parked_until: Option<i64>,
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

/// The pure park decision: given a state and the current epoch, the reset time
/// reads are parked until, or `None` when the token is free. Split out so the
/// "is this token parked right now" rule is unit-testable without the file.
pub fn park_verdict(state: &RateLimitState, now: i64) -> Option<i64> {
    match state.parked_until {
        Some(until) if until > now => Some(until),
        _ => None,
    }
}

/// Whether `key` is currently parked, and until when (epoch seconds). Reads the
/// file each call; reads are cache-gated so this is not on a hot path.
pub fn parked_until(key: &str, now: i64) -> Option<i64> {
    park_verdict(&read_state(key), now)
}

/// Process-global lock serializing [`park`] so N concurrent poller threads that
/// all see a 403 in the same window produce exactly **one** park transition
/// (and therefore one events.log line) rather than one per thread. Cross-process
/// racers still dedupe through the file check inside the lock, with only a small
/// window; within a process this makes it exact.
fn park_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Park `key` until `reset_at`. Returns `true` **iff this call transitioned the
/// token from not-parked to parked** — the signal the caller uses to log the
/// `board rate-limited` line exactly once per window. A call that finds the
/// token already parked returns `false` and rewrites nothing.
///
/// `reset_at` is clamped to strictly after `now` so a stale/past reset can't
/// produce a park that's already expired (which would re-fire on the next read).
pub fn park(key: &str, reset_at: i64, now: i64) -> bool {
    let _guard = park_lock().lock();
    let mut state = read_state(key);
    if park_verdict(&state, now).is_some() {
        return false; // already parked this window
    }
    state.reset_at = Some(reset_at);
    state.parked_until = Some(reset_at.max(now + 1));
    write_state(key, &state);
    true
}

/// Record the latest `remaining` / `reset_at` a header parse saw, without
/// parking. Phase 0 doesn't act on these, but capturing them keeps the file the
/// single source the Phase 3 governor will read. No-op when neither is present.
pub fn record_rate_limit(key: &str, headers: &RateLimitHeaders) {
    if headers.remaining.is_none() && headers.reset_at.is_none() {
        return;
    }
    let _guard = park_lock().lock();
    let mut state = read_state(key);
    if headers.remaining.is_some() {
        state.remaining = headers.remaining;
    }
    if headers.reset_at.is_some() {
        state.reset_at = headers.reset_at;
    }
    write_state(key, &state);
}

/// Clear any park recorded for `key`. Test-support and an explicit "resume now"
/// hook; production relies on the implicit expiry in [`park_verdict`].
#[cfg(any(test, feature = "test-support"))]
pub fn clear(key: &str) {
    let _guard = park_lock().lock();
    write_state(key, &RateLimitState::default());
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
        let state = RateLimitState {
            parked_until: Some(1_000),
            ..Default::default()
        };
        assert_eq!(park_verdict(&state, 999), Some(1_000), "before reset: parked");
        assert_eq!(park_verdict(&state, 1_000), None, "at reset: not parked");
        assert_eq!(park_verdict(&state, 1_001), None, "past reset: not parked");
    }

    #[test]
    fn park_verdict_is_never_parked_without_a_marker() {
        assert_eq!(park_verdict(&RateLimitState::default(), 0), None);
    }

    #[test]
    fn token_key_is_stable_and_hides_the_secret() {
        let secret = "ghp_supersecrettoken";
        let key = token_key(secret);
        assert_eq!(key, token_key(secret), "same token → same key across calls");
        assert!(!key.contains("secret"), "the key must not embed the token");
        assert_ne!(key, token_key("a-different-token"));
    }

    /// End-to-end park lifecycle against a real budget file: the first 403 in a
    /// window transitions to parked (returns `true`, so the caller logs the one
    /// `board rate-limited` line), a second 403 in the same window does not
    /// (returns `false`, so no duplicate line), the token reads as parked until
    /// its reset, and a fresh 403 after the reset opens a new window.
    #[test]
    fn park_transitions_once_per_window_and_expires_at_reset() {
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
        assert_eq!(parked_until(&key, 100), None, "a clean token is not parked");

        assert!(park(&key, 500, 100), "first 403 in the window transitions to parked");
        assert!(
            !park(&key, 500, 200),
            "a second 403 in the same window must not re-transition (no duplicate log)"
        );

        assert_eq!(parked_until(&key, 400), Some(500), "parked until the reset");
        assert_eq!(parked_until(&key, 500), None, "at the reset, reads resume");

        assert!(
            park(&key, 900, 500),
            "a 403 in the next window transitions again (a fresh log line)"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
