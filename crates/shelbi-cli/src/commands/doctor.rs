//! `shelbi doctor` — runtime health checks for a project's GitHub API budget.
//!
//! Phase 3 §6 of `Plans/github-issue-caching-and-rate-limits.md` asks for a
//! `shelbi doctor` that "warns when the observed request rate would exhaust a
//! budget in under 30 minutes and names the top callers". This is a *runtime*
//! health report — the observed API request rate against the live budget — not a
//! configuration-file check, so it lives in its own command rather than in
//! `shelbi config lint` (whose contract is "exit 1 iff a config surface is
//! invalid"; a fluctuating rate warning has no place tripping that exit code).
//!
//! It reads two durable, side-effect-free sources:
//!
//! - the hub-wide request log ([`shelbi_state::gh_requests`]) for the observed
//!   rate and the callers driving it, and
//! - the per-token budget file ([`shelbi_state::gh_budget`]) for the live
//!   `remaining`, falling back to the published board index's last-seen budget.
//!
//! For each budget it projects a time-to-exhaustion (`remaining / rate`) and, when
//! that is under [`EXHAUSTION_WARN`], prints a warning naming the top callers so an
//! operator knows which reader to throttle.

use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

use shelbi_state::gh_budget::Budget;
use shelbi_state::gh_requests::{self, RequestEntry};

use super::require_project;

/// Time-to-exhaustion under which a budget's rate earns a warning: 30 minutes,
/// matching the plan's acceptance criterion.
const EXHAUSTION_WARN: Duration = Duration::from_secs(30 * 60);

/// How far back the observed request rate is measured. The rate itself is
/// computed over the *span* the observed requests actually cover (see
/// [`observed_rate`]), so this only bounds how much history is read.
const OBSERVATION_WINDOW: Duration = Duration::from_secs(3_600);

/// Entry point for `shelbi doctor [--project NAME]`. Read-only and side-effect
/// free; always exits 0 (it is a report, not a gate) — the warning is the signal.
pub fn run(project: Option<String>) -> Result<()> {
    let project = require_project(project)?;
    let cfg = shelbi_state::load_project(&project)
        .map_err(|e| anyhow!(e))?
        .issue_tracker;
    if !cfg.backend.is_remote() {
        println!(
            "doctor: project `{project}` uses a local ({}) issue backend — no API budget to check",
            cfg.backend
        );
        return Ok(());
    }

    let now = Utc::now();
    let entries = gh_requests::recent_entries(OBSERVATION_WINDOW, now);
    let budget = github_budget_snapshot(&project);

    println!("GitHub API budget health for `{project}`:");
    for (label, kind) in [("graphql", Budget::Graphql), ("rest", Budget::Rest)] {
        let remaining = budget.as_ref().and_then(|s| s.tier(kind).remaining);
        report_budget(label, kind, &entries, remaining, now);
    }
    Ok(())
}

/// Print one budget's line plus, when the projected exhaustion is under the
/// threshold, a warning naming the top callers.
fn report_budget(
    label: &str,
    kind: Budget,
    entries: &[RequestEntry],
    remaining: Option<i64>,
    now: DateTime<Utc>,
) {
    let mine: Vec<&RequestEntry> = entries.iter().filter(|e| e.budget == kind).collect();
    let count = mine.len();
    let remaining_str = remaining
        .map(|r| format!("{r} remaining"))
        .unwrap_or_else(|| "remaining unknown".to_string());

    let Some(rate) = observed_rate(&mine, now) else {
        // No traffic on this budget in the window: nothing to project.
        println!("  {label}: {count} requests in the last hour · {remaining_str} · idle");
        return;
    };

    let per_hour = (rate * 3_600.0).round() as i64;
    let remaining_for_projection = remaining.filter(|r| *r >= 0).map(|r| r as u64);
    let exhaust_secs = seconds_to_exhaustion(rate, remaining_for_projection);
    let exhaust_str = format_duration(exhaust_secs);
    println!(
        "  {label}: {} req/s (~{per_hour}/hr) · {remaining_str} · exhausts in ~{exhaust_str}",
        format_rate(rate),
    );

    if exhaust_secs < EXHAUSTION_WARN.as_secs_f64() {
        println!(
            "    WARNING: at the observed rate the {label} budget exhausts in ~{exhaust_str} \
             (under {} min).",
            EXHAUSTION_WARN.as_secs() / 60,
        );
        let callers = top_callers(&mine);
        if !callers.is_empty() {
            let named = callers
                .iter()
                .map(|(name, n)| format!("{name} ({n})"))
                .collect::<Vec<_>>()
                .join(", ");
            println!("    Top callers: {named}");
        }
    }
}

/// The observed requests-per-second for a budget's entries, measured over the
/// span the entries actually cover (oldest → now), floored at one second so a
/// burst clustered in a moment doesn't divide by ~zero. `None` when there are no
/// entries. Span-based (rather than dividing by the fixed window) so a short,
/// intense burst reads as its true instantaneous rate rather than being diluted
/// across an hour of otherwise-quiet history.
fn observed_rate(entries: &[&RequestEntry], now: DateTime<Utc>) -> Option<f64> {
    if entries.is_empty() {
        return None;
    }
    let oldest = entries.iter().map(|e| e.at).min()?;
    let span_secs = (now - oldest).num_seconds().max(1) as f64;
    Some(entries.len() as f64 / span_secs)
}

/// Seconds until a budget at `rate` req/s runs out, given `remaining` (falling
/// back to the full GitHub per-hour limit when the live remaining is unknown).
fn seconds_to_exhaustion(rate: f64, remaining: Option<u64>) -> f64 {
    let budget_left = remaining.unwrap_or(gh_requests::GITHUB_BUDGET_LIMIT) as f64;
    budget_left / rate
}

/// Callers for a budget's entries, ordered by descending request count (ties
/// broken by name), capped at the top three so the warning stays legible.
fn top_callers(entries: &[&RequestEntry]) -> Vec<(String, usize)> {
    use std::collections::HashMap;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for e in entries {
        *counts.entry(e.caller.as_str()).or_insert(0) += 1;
    }
    let mut ranked: Vec<(String, usize)> =
        counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(3);
    ranked
}

/// Read the token's persisted rate-limit budget for `project`, or `None` when
/// the token can't be resolved (then the projection uses the full limit).
fn github_budget_snapshot(project: &str) -> Option<shelbi_state::gh_budget::RateLimitState> {
    let token = shelbi_state::resolve_github_token_by_name(project).ok()?;
    let key = shelbi_state::gh_budget::token_key(token.expose());
    Some(shelbi_state::gh_budget::read_state(&key))
}

/// A compact rate: `10` for whole values, `0.5` otherwise.
fn format_rate(rate: f64) -> String {
    if (rate.round() - rate).abs() < 1e-9 {
        format!("{}", rate.round() as i64)
    } else {
        format!("{rate:.1}")
    }
}

/// A compact human duration for a seconds count: `8m`, `2h`, `45s`.
fn format_duration(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(caller: &str, budget: Budget, at: DateTime<Utc>) -> RequestEntry {
        RequestEntry {
            at,
            budget,
            caller: caller.to_string(),
        }
    }

    #[test]
    fn observed_rate_is_span_based() {
        let now = Utc::now();
        // 10 entries spanning 10s → 1 req/s.
        let entries: Vec<RequestEntry> = (0..10)
            .map(|i| entry("pollers", Budget::Rest, now - chrono::Duration::seconds(10 - i)))
            .collect();
        let refs: Vec<&RequestEntry> = entries.iter().collect();
        let rate = observed_rate(&refs, now).unwrap();
        assert!((rate - 1.0).abs() < 0.2, "≈1 req/s over a 10s span, got {rate}");
    }

    #[test]
    fn ten_per_second_exhausts_a_full_budget_in_well_under_thirty_minutes() {
        // The acceptance shape: 10 req/s against the 5,000 REST budget.
        let exhaust = seconds_to_exhaustion(10.0, Some(5_000));
        assert!(exhaust < EXHAUSTION_WARN.as_secs_f64(), "{exhaust}s should be < 1800s");
        assert!((exhaust - 500.0).abs() < 1e-6);
    }

    #[test]
    fn top_callers_ranks_by_count() {
        let now = Utc::now();
        let entries = [
            entry("pollers", Budget::Rest, now),
            entry("pollers", Budget::Rest, now),
            entry("write", Budget::Rest, now),
        ];
        let refs: Vec<&RequestEntry> = entries.iter().collect();
        let ranked = top_callers(&refs);
        assert_eq!(ranked[0], ("pollers".to_string(), 2));
        assert_eq!(ranked[1], ("write".to_string(), 1));
    }

    /// The acceptance path (plan Phase 3): a simulated ~10-requests-per-second log
    /// drives the observed rate high enough that the REST budget would exhaust in
    /// well under 30 minutes, so the doctor warns and names the top caller. Reads
    /// the real request log through `gh_requests`, so the whole read path is
    /// exercised, not just the arithmetic.
    #[test]
    fn a_ten_per_second_log_fires_the_warning_and_names_the_caller() {
        let _lock = crate::commands::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "shelbi-doctor-rate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let now = Utc::now();
        // 100 REST requests spread across ~10s → ≈10 req/s, all from `pollers`.
        for i in 0..100 {
            shelbi_state::gh_requests::record_request_at(
                Budget::Rest,
                "pollers",
                now - chrono::Duration::milliseconds(100 * (100 - i)),
            );
        }

        let entries = gh_requests::recent_entries(OBSERVATION_WINDOW, now);
        let rest: Vec<&RequestEntry> = entries.iter().filter(|e| e.budget == Budget::Rest).collect();
        let rate = observed_rate(&rest, now).expect("a rate over the burst");
        assert!(rate > 5.0, "≈10 req/s expected, got {rate}");

        // Against the full 5,000 REST budget this exhausts in ~500s — under 30 min.
        let exhaust = seconds_to_exhaustion(rate, Some(5_000));
        assert!(
            exhaust < EXHAUSTION_WARN.as_secs_f64(),
            "{exhaust}s should trip the < 30m warning"
        );
        // And the doctor names the caller driving it.
        let callers = top_callers(&rest);
        assert_eq!(callers[0].0, "pollers", "the warning names the top caller");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
