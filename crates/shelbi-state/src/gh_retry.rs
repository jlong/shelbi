//! Retry / backoff for the `gh` calls behind the GitHub issue-store seam.
//!
//! Every GitHub REST call the store makes goes through the `gh` CLI, and a bulk
//! run (a 588-card `issue-store migrate`) will trip GitHub's **secondary** rate
//! limit on content creation — 403/429 with, sometimes, a `Retry-After` hint —
//! long before it finishes. Before this module every such failure was an
//! immediate hard abort. [`RetryPolicy`] wraps a fallible `gh` operation and:
//!
//! * classifies the failure ([`classify`]) into *rate-limited* (retry, honoring
//!   any server-provided wait), *transient* (5xx / network — retry with
//!   exponential backoff + jitter), or *terminal* (401 / 404 / 422 validation —
//!   never retry, so a permanent error can't spin), and
//! * surfaces each wait to the operator through a notifier (the production
//!   default prints "rate limit hit; resuming in Ns" to stderr) rather than
//!   appearing hung.
//!
//! The sleep, jitter, and notify sinks are all injectable so the whole loop is
//! unit-tested with a fake clock and no real waiting. A GitHub issue creation is
//! the rate-limited unit, so the policy wraps the individual `gh` invocation
//! (not the higher-level [`crate::IssueStore::add`], which fans out into several
//! calls) — a retry re-issues exactly the one request that was throttled.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use shelbi_core::{Error, Result};

/// Injectable sleep sink (production: [`std::thread::sleep`]).
type SleepFn = Arc<dyn Fn(Duration) + Send + Sync>;
/// Injectable jitter: maps a computed backoff ceiling to the actual wait
/// (production: equal jitter; tests: identity, for deterministic assertions).
type JitterFn = Arc<dyn Fn(Duration) -> Duration + Send + Sync>;
/// Injectable notifier invoked once per wait, so the operator sees progress.
type NotifyFn = Arc<dyn Fn(&RetryNotice) + Send + Sync>;

/// Why a `gh` call is being retried — drives the operator-facing message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    /// A GitHub rate limit (primary or the content-creation *secondary* limit).
    RateLimited,
    /// A transient failure (5xx / network) that a re-request may clear.
    Transient,
}

/// One about-to-wait notification handed to the policy's notifier.
#[derive(Debug, Clone)]
pub struct RetryNotice {
    /// The attempt that just failed (1-based).
    pub attempt: u32,
    /// The policy's attempt ceiling, for a "attempt k/N" message.
    pub max_attempts: u32,
    /// How long the policy is about to sleep before the next attempt.
    pub wait: Duration,
    /// What kind of failure triggered the wait.
    pub kind: RetryKind,
}

/// How a failed `gh` call should be handled.
enum Disposition {
    /// Rate limited; the inner value is a server-provided wait hint if one was
    /// present in the error (`Retry-After` / `x-ratelimit-reset`).
    RateLimited(Option<Duration>),
    /// Transient (5xx / network) — retry with computed backoff.
    Transient,
    /// Permanent — never retry.
    Terminal,
}

/// A retry/backoff policy for `gh` calls. Cheap to clone (all sinks are `Arc`s),
/// so the wrapped runner a [`crate::GitHubStore`] holds can be shared freely.
#[derive(Clone)]
pub struct RetryPolicy {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
    sleep: SleepFn,
    jitter: JitterFn,
    notify: NotifyFn,
    /// When set, a rate-limit failure that carries *no* server wait hint
    /// (`Retry-After` / `x-ratelimit-reset`) is treated as terminal — fail fast
    /// rather than burning blind exponential backoff. The read policy sets this
    /// so a primary-limit `API rate limit exceeded` (which `gh` reports without
    /// a `Retry-After`) doesn't block a board read for minutes; the mutating
    /// policy leaves it clear so a bulk write can wait out a rolling window.
    hintless_rate_limit_is_terminal: bool,
}

impl RetryPolicy {
    /// The policy used by a real [`crate::GitHubStore`]: retry rate-limit and
    /// transient failures with exponential backoff (capped, jittered), honoring
    /// a server `Retry-After` when present, and print each wait to stderr. The
    /// attempt ceiling is generous so a bulk migration that trips the secondary
    /// limit can wait out a rolling window rather than aborting.
    pub fn production() -> Self {
        Self {
            max_attempts: 10,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(90),
            sleep: Arc::new(std::thread::sleep),
            jitter: Arc::new(equal_jitter),
            notify: Arc::new(default_notify),
            hintless_rate_limit_is_terminal: false,
        }
    }

    /// The policy for read-only (`GET`) `gh` calls: fail fast. A board read (the
    /// poller tick, the TUI, every `shelbi issue` command, the event drain) must
    /// never block for minutes behind backoff — when the *primary* hourly limit
    /// is exhausted `gh` reports `API rate limit exceeded` with no `Retry-After`,
    /// so a read under it returns immediately rather than stalling dialog
    /// detection, marker handling and dispatch. Only a transient 5xx/network
    /// blip (or a rate limit that *does* carry a server wait hint) gets a single
    /// short retry.
    pub fn reads() -> Self {
        Self {
            max_attempts: 2,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(2),
            sleep: Arc::new(std::thread::sleep),
            jitter: Arc::new(equal_jitter),
            notify: Arc::new(default_notify),
            hintless_rate_limit_is_terminal: true,
        }
    }

    /// A policy with injected sleep + notify sinks and identity jitter, for
    /// deterministic tests (no real waiting, exact backoff values).
    #[cfg(test)]
    pub fn for_test(max_attempts: u32, sleep: SleepFn, notify: NotifyFn) -> Self {
        Self {
            max_attempts,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(64),
            sleep,
            jitter: Arc::new(|c| c),
            notify,
            hintless_rate_limit_is_terminal: false,
        }
    }

    /// Like [`RetryPolicy::for_test`] but with the read policy's fail-fast
    /// classification (a hintless rate limit is terminal), so the `GET`-path
    /// behavior is asserted deterministically.
    #[cfg(test)]
    pub fn for_test_reads(max_attempts: u32, sleep: SleepFn, notify: NotifyFn) -> Self {
        Self {
            hintless_rate_limit_is_terminal: true,
            ..Self::for_test(max_attempts, sleep, notify)
        }
    }

    /// Run `op`, retrying per this policy until it succeeds, hits a terminal
    /// error, or exhausts `max_attempts` (in which case the last error is
    /// returned). A non-[`Error::Command`] error is always terminal — only a
    /// `gh` invocation failure carries a status/body to classify.
    pub fn run<T>(&self, mut op: impl FnMut() -> Result<T>) -> Result<T> {
        let mut attempt: u32 = 1;
        loop {
            let err = match op() {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };
            let (kind, hint) = match classify(&err) {
                Disposition::Terminal => return Err(err),
                // On the read policy a rate limit with no server wait hint (the
                // exhausted primary limit) is terminal: fail fast instead of
                // blocking a board read behind blind backoff.
                Disposition::RateLimited(None) if self.hintless_rate_limit_is_terminal => {
                    return Err(err)
                }
                Disposition::RateLimited(hint) => (RetryKind::RateLimited, hint),
                Disposition::Transient => (RetryKind::Transient, None),
            };
            if attempt >= self.max_attempts {
                return Err(err);
            }
            // A server-provided wait is authoritative and used verbatim;
            // otherwise fall back to jittered exponential backoff.
            let wait = hint.unwrap_or_else(|| (self.jitter)(self.backoff_ceiling(attempt)));
            (self.notify)(&RetryNotice {
                attempt,
                max_attempts: self.max_attempts,
                wait,
                kind,
            });
            (self.sleep)(wait);
            attempt += 1;
        }
    }

    /// The exponential backoff ceiling for a given attempt: `base * 2^(attempt-1)`
    /// capped at `max_delay`, saturating rather than overflowing at high attempts.
    fn backoff_ceiling(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX);
        self.base_delay
            .checked_mul(factor)
            .unwrap_or(self.max_delay)
            .min(self.max_delay)
    }
}

/// Equal jitter: sleep half the ceiling plus a random slice of the other half,
/// so retries spread out without ever collapsing to zero. The randomness is
/// cosmetic (spreading concurrent clients), so a cheap time-seeded mix is fine —
/// no `rand` dependency is pulled in for it.
fn equal_jitter(ceiling: Duration) -> Duration {
    let half = ceiling / 2;
    let span = half.as_nanos() as u64;
    half + Duration::from_nanos(pseudo_rand(span.saturating_add(1)))
}

/// A cheap time-seeded pseudo-random in `[0, bound)` (SplitMix64-style mix of
/// the current sub-second nanos). Not for anything security-sensitive.
fn pseudo_rand(bound: u64) -> u64 {
    if bound == 0 {
        return 0;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mut x = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x1234_5678_9ABC_DEF0);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x % bound
}

/// The production notifier: print each wait to stderr so a bulk run that stalls
/// on a rate limit reads as "resuming in Ns", not a hang.
fn default_notify(n: &RetryNotice) {
    let secs = n.wait.as_secs_f64().round() as u64;
    match n.kind {
        RetryKind::RateLimited => eprintln!(
            "shelbi: GitHub rate limit hit; resuming in {secs}s (attempt {}/{})",
            n.attempt, n.max_attempts
        ),
        RetryKind::Transient => eprintln!(
            "shelbi: transient GitHub error; retrying in {secs}s (attempt {}/{})",
            n.attempt, n.max_attempts
        ),
    }
}

/// Classify a `gh` failure. Only [`Error::Command`] (a real `gh` invocation
/// that exited non-zero) is classifiable; everything else is terminal. The
/// haystack is the combined status line + captured stderr/stdout, lowercased.
///
/// Order matters: a rate-limit 403 carries "secondary rate limit" / "rate
/// limit" text, so it is matched *before* the terminal fall-through — a plain
/// permission 403 ("resource not accessible") carries neither and stays
/// terminal, so we never spin on a permanent authorization error.
fn classify(err: &Error) -> Disposition {
    let Error::Command { stderr, status, .. } = err else {
        return Disposition::Terminal;
    };
    let hay = format!("{status}\n{stderr}").to_ascii_lowercase();

    let rate_limited = hay.contains("secondary rate limit")
        || hay.contains("rate limit") // "API rate limit exceeded"
        || hay.contains("http 429")
        || hay.contains("too many requests")
        || hay.contains("x-ratelimit-remaining: 0");
    if rate_limited {
        return Disposition::RateLimited(parse_wait_hint(stderr));
    }

    let transient = hay.contains("http 500")
        || hay.contains("http 502")
        || hay.contains("http 503")
        || hay.contains("http 504")
        || hay.contains("bad gateway")
        || hay.contains("service unavailable")
        || hay.contains("gateway time")
        || hay.contains("timeout")
        || hay.contains("timed out")
        || hay.contains("could not resolve host")
        || hay.contains("connection refused")
        || hay.contains("connection reset")
        || hay.contains("network is unreachable")
        || hay.contains("temporary failure")
        || hay.contains("i/o timeout")
        || hay.contains("dial tcp");
    if transient {
        return Disposition::Transient;
    }

    Disposition::Terminal
}

/// Parse a server-provided wait hint from an error body: a `Retry-After: <secs>`
/// header wins; otherwise an `x-ratelimit-reset: <epoch>` yields the seconds
/// until that reset (clamped at zero if already past). Returns `None` when
/// neither is present, in which case the caller falls back to backoff.
fn parse_wait_hint(text: &str) -> Option<Duration> {
    let lower = text.to_ascii_lowercase();
    if let Some(secs) = parse_after_key(&lower, "retry-after") {
        return Some(Duration::from_secs(secs));
    }
    if let Some(reset) = parse_after_key(&lower, "x-ratelimit-reset") {
        let now = Utc::now().timestamp();
        let delta = (reset as i64) - now;
        return Some(Duration::from_secs(delta.max(0) as u64));
    }
    None
}

/// Read the first run of ASCII digits that follows `key` (and its `:`/whitespace
/// separator) in `hay`, parsed as a `u64`. `hay` is assumed already lowercased.
fn parse_after_key(hay: &str, key: &str) -> Option<u64> {
    let idx = hay.find(key)?;
    let rest = &hay[idx + key.len()..];
    let digits: String = rest
        .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn command_err(status: &str, detail: &str) -> Error {
        Error::Command {
            cmd: "gh api ...".into(),
            status: status.into(),
            stderr: detail.into(),
        }
    }

    /// A policy that records every wait it sleeps for (in seconds), never
    /// waiting in real time, so retry behavior is asserted instantly.
    fn recording_policy(max_attempts: u32) -> (RetryPolicy, Arc<Mutex<Vec<u64>>>) {
        let waits: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = waits.clone();
        let sleep: SleepFn = Arc::new(move |d: Duration| rec.lock().unwrap().push(d.as_secs()));
        let notify: NotifyFn = Arc::new(|_| {});
        (RetryPolicy::for_test(max_attempts, sleep, notify), waits)
    }

    #[test]
    fn rate_limit_with_retry_after_is_retried_and_completes_honoring_the_hint() {
        // Fail once with a 429 carrying `Retry-After: 2`, then succeed. The run
        // completes, and the single recorded wait is exactly the server hint.
        let (policy, waits) = recording_policy(5);
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let out: Result<&str> = policy.run(|| {
            let mut n = c.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Err(command_err("exit status: 1", "HTTP 429: Too Many Requests\nRetry-After: 2"))
            } else {
                Ok("done")
            }
        });
        assert_eq!(out.unwrap(), "done");
        assert_eq!(*calls.lock().unwrap(), 2, "one failed attempt + one success");
        assert_eq!(*waits.lock().unwrap(), vec![2], "honored Retry-After verbatim");
    }

    #[test]
    fn secondary_rate_limit_without_a_hint_backs_off_exponentially() {
        // No `Retry-After`: identity jitter makes the waits the pure exponential
        // ceiling (1s, 2s), and the third attempt succeeds.
        let (policy, waits) = recording_policy(5);
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let out: Result<&str> = policy.run(|| {
            let mut n = c.lock().unwrap();
            *n += 1;
            if *n < 3 {
                Err(command_err(
                    "exit status: 1",
                    "HTTP 403: You have exceeded a secondary rate limit.",
                ))
            } else {
                Ok("ok")
            }
        });
        assert_eq!(out.unwrap(), "ok");
        assert_eq!(*waits.lock().unwrap(), vec![1, 2], "base * 2^(attempt-1)");
    }

    #[test]
    fn terminal_validation_error_aborts_immediately_without_retrying() {
        // A 422 validation failure is permanent — one call, no waits.
        let (policy, waits) = recording_policy(5);
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let out: Result<&str> = policy.run(|| {
            *c.lock().unwrap() += 1;
            Err(command_err(
                "exit status: 1",
                "HTTP 422: Validation Failed\nname is too long (maximum is 50 characters)",
            ))
        });
        assert!(out.is_err());
        assert_eq!(*calls.lock().unwrap(), 1, "no retry on a terminal error");
        assert!(waits.lock().unwrap().is_empty());
    }

    #[test]
    fn permission_403_is_terminal_not_a_rate_limit() {
        // A 403 that is a permission error (no rate-limit text) must not be
        // mistaken for a throttle and retried.
        let (policy, _waits) = recording_policy(5);
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let _ = policy.run(|| {
            *c.lock().unwrap() += 1;
            Err::<(), _>(command_err(
                "exit status: 1",
                "HTTP 403: Resource not accessible by integration",
            ))
        });
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn unknown_gh_error_is_terminal_and_returns_after_a_single_attempt() {
        // An error the classifier does not recognize as rate-limit or transient
        // must fail fast — one attempt, no waits — so a migration never spins for
        // minutes on an error shape it doesn't understand (e.g. an unexpected body
        // during a GitHub outage). This locks the terminal fall-through in place.
        let (policy, waits) = recording_policy(5);
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let out: Result<()> = policy.run(|| {
            *c.lock().unwrap() += 1;
            Err(command_err(
                "exit status: 1",
                "HTTP 418: I'm a teapot — some body we did not anticipate",
            ))
        });
        assert!(out.is_err());
        assert_eq!(*calls.lock().unwrap(), 1, "unknown error is not retried");
        assert!(waits.lock().unwrap().is_empty());
    }

    #[test]
    fn non_command_error_is_terminal_and_returns_after_a_single_attempt() {
        // Only a `gh` invocation failure carries a status/body to classify; any
        // other error variant is unclassifiable and must abort after one attempt
        // rather than being retried blindly.
        let (policy, waits) = recording_policy(5);
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let out: Result<()> = policy.run(|| {
            *c.lock().unwrap() += 1;
            Err(Error::Other("not a gh command failure".into()))
        });
        assert!(out.is_err());
        assert_eq!(*calls.lock().unwrap(), 1, "non-Command error is not retried");
        assert!(waits.lock().unwrap().is_empty());
    }

    #[test]
    fn transient_network_error_is_retried_then_gives_up_after_max_attempts() {
        // A network error retries up to the ceiling; exhausting it returns the
        // last error (here max_attempts = 3 → 3 calls, 2 waits).
        let (policy, waits) = recording_policy(3);
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let out: Result<()> = policy.run(|| {
            *c.lock().unwrap() += 1;
            Err(command_err("exit status: 1", "could not resolve host: api.github.com"))
        });
        assert!(out.is_err());
        assert_eq!(*calls.lock().unwrap(), 3);
        assert_eq!(waits.lock().unwrap().len(), 2);
    }

    #[test]
    fn reads_policy_fails_fast_on_a_hintless_rate_limit() {
        // The exhausted primary limit surfaces as `API rate limit exceeded` with
        // no Retry-After. On the read policy that is terminal: one attempt, no
        // sleep — a board read never blocks behind backoff.
        let waits: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let rec = waits.clone();
        let sleep: SleepFn = Arc::new(move |d: Duration| rec.lock().unwrap().push(d.as_secs()));
        let policy = RetryPolicy::for_test_reads(2, sleep, Arc::new(|_| {}));
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let out: Result<()> = policy.run(|| {
            *c.lock().unwrap() += 1;
            Err(command_err(
                "exit status: 1",
                "HTTP 403: API rate limit exceeded for user ID 1.",
            ))
        });
        assert!(out.is_err());
        assert_eq!(*calls.lock().unwrap(), 1, "hintless rate limit is terminal on reads");
        assert!(waits.lock().unwrap().is_empty(), "no backoff on a read");
    }

    #[test]
    fn reads_policy_still_honors_a_rate_limit_that_carries_a_server_hint() {
        // A read that hits a limit *with* a Retry-After can wait it out once,
        // since the wait is server-bounded rather than blind backoff.
        let (policy, waits) = {
            let waits: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
            let rec = waits.clone();
            let sleep: SleepFn = Arc::new(move |d: Duration| rec.lock().unwrap().push(d.as_secs()));
            (RetryPolicy::for_test_reads(2, sleep, Arc::new(|_| {})), waits)
        };
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let out: Result<&str> = policy.run(|| {
            let mut n = c.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Err(command_err("exit status: 1", "HTTP 429: Too Many Requests\nRetry-After: 1"))
            } else {
                Ok("ok")
            }
        });
        assert_eq!(out.unwrap(), "ok");
        assert_eq!(*calls.lock().unwrap(), 2);
        assert_eq!(*waits.lock().unwrap(), vec![1], "honored the server hint once");
    }

    #[test]
    fn parse_wait_hint_reads_retry_after_and_reset() {
        assert_eq!(
            parse_wait_hint("HTTP 429\nRetry-After: 47"),
            Some(Duration::from_secs(47))
        );
        // x-ratelimit-reset in the far future yields a positive wait.
        let future = (Utc::now().timestamp() + 30) as u64;
        let hint = parse_wait_hint(&format!("HTTP 403\nx-ratelimit-reset: {future}")).unwrap();
        assert!(hint.as_secs() >= 25 && hint.as_secs() <= 31, "≈30s, got {hint:?}");
        // Neither present → no hint.
        assert_eq!(parse_wait_hint("HTTP 403: forbidden"), None);
    }
}
