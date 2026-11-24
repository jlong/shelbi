//! Elapsed time against an estimate, as both clients and the CLI show it: an
//! active issue's Estimate row (`2h · 1h 12m elapsed (60%)`) and the bar
//! along its board card, and a running job's bar on the Jobs screen (`shelbi
//! job run --estimate`). Both bars take their fraction, fill and overdue edge
//! from [`EstimateProgress`], so they read the same.
//!
//! The estimate lives in the awareness store, not the issue
//! ([`shelbi_state::awareness::Estimate`]): `issue start --estimate` (or a
//! workflow's `default_estimate`) and `issue estimate` set it, and leaving the
//! active category clears it. The board read carries the store's estimates
//! ([`crate::board::BoardRead::estimates`]) so a remote project's come over
//! the same link. Progress is computed against the clock at render time, so
//! it advances while the board stays open without a read changing.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::theme::Tone;

/// The color of overdue text (a job's Status and Duration, the activity
/// feed's `overdue` verb). The bars take the clients' warning color, the same
/// yellow.
pub const OVERDUE_TONE: Tone = Tone::Yellow;

/// The part of an awareness estimate a client shows: how long the issue was
/// expected to take, and when it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssueEstimate {
    pub estimate: Duration,
    pub started_at: DateTime<Utc>,
}

impl From<&shelbi_state::awareness::Estimate> for IssueEstimate {
    fn from(e: &shelbi_state::awareness::Estimate) -> Self {
        IssueEstimate {
            estimate: e.estimate,
            started_at: e.started_at,
        }
    }
}

impl IssueEstimate {
    /// Where the issue stands against its estimate at `now`. A start in the
    /// future (clock skew with a remote machine) reads as nothing elapsed.
    pub fn progress(&self, now: DateTime<Utc>) -> EstimateProgress {
        EstimateProgress {
            estimate: self.estimate,
            elapsed: (now - self.started_at).to_std().unwrap_or_default(),
        }
    }
}

/// An estimate and the time elapsed against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EstimateProgress {
    pub estimate: Duration,
    pub elapsed: Duration,
}

impl EstimateProgress {
    pub fn new(elapsed: Duration, estimate: Duration) -> Self {
        EstimateProgress { estimate, elapsed }
    }

    /// From `started` to `end` (now, for something still running). A skewed
    /// clock reads nothing elapsed, never negative.
    pub fn between(started: DateTime<Utc>, end: DateTime<Utc>, estimate: Duration) -> Self {
        Self::new((end - started).to_std().unwrap_or_default(), estimate)
    }

    /// At or past the estimate (the same edge `issue show` and the overdue
    /// alert use).
    pub fn overdue(&self) -> bool {
        self.elapsed >= self.estimate
    }

    /// Elapsed over estimate, held to `0.0..=1.0`: an overdue issue's bar is
    /// full.
    pub fn fraction(&self) -> f32 {
        if self.overdue() {
            return 1.0;
        }
        (self.elapsed.as_secs_f64() / self.estimate.as_secs_f64()).clamp(0.0, 1.0) as f32
    }

    /// Whole percent elapsed, rounded down (so `99%` still reads as on time).
    pub fn percent(&self) -> u32 {
        if self.estimate.is_zero() {
            return 100;
        }
        let pct = self.elapsed.as_millis().saturating_mul(100) / self.estimate.as_millis().max(1);
        u32::try_from(pct).unwrap_or(u32::MAX)
    }

    /// The Estimate row's value: `2h · 1h 12m elapsed (60%)`, or
    /// `2h · 2h 30m elapsed (overdue)`. Elapsed reads in whole minutes, the
    /// granularity the board repaints it at, and the percent is of the
    /// minutes shown.
    pub fn label(&self) -> String {
        let shown = EstimateProgress {
            estimate: self.estimate,
            elapsed: whole_minutes(self.elapsed),
        };
        let tail = if self.overdue() {
            "overdue".to_string()
        } else {
            format!("{}%", shown.percent())
        };
        format!("{} · {} elapsed ({tail})", spaced(self.estimate), spaced(shown.elapsed))
    }

    /// The CLI's sentence (`issue estimate`, `job list`, `job log`): `elapsed
    /// 20m, 25m left` / `elapsed 1h10m, overdue by 25m`, compact durations
    /// cut to the minute once past one.
    pub fn status_text(&self) -> String {
        let minute = |d: Duration| {
            let secs = d.as_secs();
            shelbi_core::display_duration(Duration::from_secs(if secs >= 60 { secs / 60 * 60 } else { secs }))
        };
        if self.overdue() {
            format!(
                "elapsed {}, overdue by {}",
                minute(self.elapsed),
                minute(self.elapsed - self.estimate)
            )
        } else {
            format!("elapsed {}, {} left", minute(self.elapsed), minute(self.estimate - self.elapsed))
        }
    }

    /// How many of a `width`-cell bar are filled: rounded down, so the last
    /// cell fills only at the estimate (and stays full past it).
    pub fn filled_cells(&self, width: usize) -> usize {
        if self.overdue() {
            return width;
        }
        ((self.fraction() as f64) * width as f64).floor() as usize
    }
}

/// `d` cut to whole minutes (`0m` under one).
fn whole_minutes(d: Duration) -> Duration {
    Duration::from_secs(d.as_secs() / 60 * 60)
}

/// [`shelbi_core::display_duration`] with a space between its units
/// (`1h 12m`), and `0m` for nothing.
fn spaced(d: Duration) -> String {
    if d.as_secs() == 0 {
        return "0m".to_string();
    }
    let compact = shelbi_core::display_duration(d);
    let mut out = String::with_capacity(compact.len() + 2);
    for ch in compact.chars() {
        if ch.is_ascii_digit() && out.ends_with(|c: char| c.is_ascii_alphabetic()) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// A minute-granularity stamp of `now`: a client whose estimates show
/// repaints when it changes, so the bars and elapsed times advance while
/// nothing else on the board does.
pub fn minute_of(now: DateTime<Utc>) -> i64 {
    now.timestamp().div_euclid(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(estimate_mins: u64, elapsed_secs: u64) -> EstimateProgress {
        EstimateProgress {
            estimate: Duration::from_secs(estimate_mins * 60),
            elapsed: Duration::from_secs(elapsed_secs),
        }
    }

    #[test]
    fn an_issue_under_its_estimate_shows_its_share_and_percent() {
        let p = at(120, 72 * 60);
        assert!(!p.overdue());
        assert!((p.fraction() - 0.6).abs() < 1e-6);
        assert_eq!(p.percent(), 60);
        assert_eq!(p.label(), "2h · 1h 12m elapsed (60%)");
    }

    #[test]
    fn an_issue_past_its_estimate_is_overdue_and_its_bar_is_full() {
        let p = at(120, 150 * 60);
        assert!(p.overdue());
        assert_eq!(p.fraction(), 1.0);
        assert_eq!(p.label(), "2h · 2h 30m elapsed (overdue)");
        assert_eq!(p.filled_cells(20), 20);
    }

    #[test]
    fn reaching_the_estimate_exactly_is_overdue() {
        let p = at(45, 45 * 60);
        assert!(p.overdue());
        assert_eq!(p.fraction(), 1.0);
        assert_eq!(p.label(), "45m · 45m elapsed (overdue)");
    }

    #[test]
    fn just_short_of_the_estimate_rounds_down_and_is_not_full() {
        let p = at(100, 100 * 60 - 1);
        assert!(!p.overdue());
        assert_eq!(p.percent(), 99);
        assert_eq!(p.filled_cells(10), 9);
        assert_eq!(p.label(), "1h 40m · 1h 39m elapsed (99%)");
    }

    #[test]
    fn elapsed_reads_in_whole_minutes() {
        assert_eq!(at(30, 59).label(), "30m · 0m elapsed (0%)");
        assert_eq!(at(30, 61).label(), "30m · 1m elapsed (3%)");
        assert_eq!(at(30, 0).filled_cells(10), 0);
    }

    #[test]
    fn a_bar_fills_by_share_rounded_down() {
        assert_eq!(at(120, 72 * 60).filled_cells(10), 6);
        assert_eq!(at(120, 72 * 60).filled_cells(0), 0);
        assert_eq!(at(60, 15 * 60).filled_cells(7), 1);
    }

    #[test]
    fn a_zero_estimate_is_overdue_at_once() {
        let p = EstimateProgress {
            estimate: Duration::ZERO,
            elapsed: Duration::ZERO,
        };
        assert!(p.overdue());
        assert_eq!(p.fraction(), 1.0);
        assert_eq!(p.percent(), 100);
    }

    #[test]
    fn progress_is_measured_from_the_start_and_a_future_start_is_nothing_elapsed() {
        let started_at: DateTime<Utc> = "2026-11-15T10:00:00Z".parse().unwrap();
        let est = IssueEstimate {
            estimate: Duration::from_secs(2 * 3600),
            started_at,
        };
        let now = started_at + chrono::Duration::minutes(72);
        assert_eq!(est.progress(now).elapsed, Duration::from_secs(72 * 60));
        let before = started_at - chrono::Duration::minutes(5);
        assert_eq!(est.progress(before).elapsed, Duration::ZERO);
    }

    #[test]
    fn the_cli_sentence_says_time_left_or_how_far_over() {
        assert_eq!(at(10, 252).status_text(), "elapsed 4m, 5m left");
        assert_eq!(at(10, 42).status_text(), "elapsed 42s, 9m left");
        assert_eq!(at(45, 70 * 60).status_text(), "elapsed 1h10m, overdue by 25m");
        let start: DateTime<Utc> = "2026-11-15T10:00:00Z".parse().unwrap();
        let skewed = EstimateProgress::between(start, start - chrono::Duration::minutes(3), Duration::from_secs(300));
        assert_eq!(skewed.elapsed, Duration::ZERO);
    }

    #[test]
    fn units_are_spaced() {
        assert_eq!(spaced(Duration::from_secs(3600 + 12 * 60)), "1h 12m");
        assert_eq!(spaced(Duration::from_secs(90)), "1m 30s");
        assert_eq!(spaced(Duration::from_secs(2 * 3600)), "2h");
        assert_eq!(spaced(Duration::ZERO), "0m");
    }

    #[test]
    fn the_minute_stamp_turns_over_on_the_minute() {
        let a: DateTime<Utc> = "2026-11-15T10:00:59Z".parse().unwrap();
        let b: DateTime<Utc> = "2026-11-15T10:01:00Z".parse().unwrap();
        assert_eq!(minute_of(a) + 1, minute_of(b));
    }
}
