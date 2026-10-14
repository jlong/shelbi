//! Riding out a lost connection to a session on another machine.
//!
//! A remote session is reached through its machine's relay over SSH
//! (`shelbi_client::relay`). When that SSH connection drops, every stream on it
//! ends at once, but the sessions themselves keep running on the remote: the
//! relay holds nothing, and a fresh one replays each session's screen on
//! reattach. So a remote pane whose connection closes with no exit is not dead,
//! it is *disconnected*, and the view should reconnect rather than spend a
//! restart.
//!
//! [`ReconnectController`] is that loop's decision core, toolkit-free and
//! clock-injected like [`super::RestartController`]: the view reports the link
//! lost, asks when the next attempt is due, and reports each attempt's outcome.
//! Unreachable attempts back off (doubling, capped) and never give up, since a
//! machine coming back is the normal end of an outage. An attempt that reaches
//! the machine but finds the session gone is a real death, which the view hands
//! to its restart controller instead.

use std::time::{Duration, Instant};

/// How quickly a disconnected view retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// The pause before the second attempt (the first is immediate).
    pub first_backoff: Duration,
    /// The longest pause between attempts.
    pub max_backoff: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            first_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
        }
    }
}

impl ReconnectPolicy {
    /// The pause after failed attempt `attempt` (1-based): `first_backoff`
    /// doubled per earlier failure, capped at `max_backoff`.
    pub fn backoff(&self, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(16);
        self.first_backoff
            .saturating_mul(1u32 << doublings)
            .min(self.max_backoff)
    }
}

/// Where a disconnected view stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Connected (or never disconnected).
    Connected,
    /// Attempt `attempt` is due at `due` (or in flight once taken).
    Waiting { attempt: u32, due: Instant },
    /// Attempt `attempt` is in flight.
    Trying { attempt: u32 },
}

/// The reconnect loop's state for one remote pane. See the module docs.
#[derive(Debug, Clone)]
pub struct ReconnectController {
    policy: ReconnectPolicy,
    phase: Phase,
    /// The latest reason the machine couldn't be reached, for the notice.
    reason: String,
}

impl Default for ReconnectController {
    fn default() -> Self {
        Self::new(ReconnectPolicy::default())
    }
}

impl ReconnectController {
    pub fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            phase: Phase::Connected,
            reason: String::new(),
        }
    }

    /// The connection was lost (the stream ended with no exit), or a first
    /// connect found the machine unreachable. Starts the loop with an
    /// immediate first attempt; while already reconnecting it changes nothing,
    /// so a duplicate report never resets the backoff.
    pub fn link_lost(&mut self, reason: &str, now: Instant) {
        self.reason = reason.to_string();
        if self.phase == Phase::Connected {
            self.phase = Phase::Waiting { attempt: 1, due: now };
        }
    }

    /// Whether the view is reconnecting (waiting for or running an attempt).
    pub fn is_reconnecting(&self) -> bool {
        self.phase != Phase::Connected
    }

    /// The 1-based attempt the view is waiting for or running (0 while
    /// connected), for the notice.
    pub fn attempt(&self) -> u32 {
        match self.phase {
            Phase::Connected => 0,
            Phase::Waiting { attempt, .. } | Phase::Trying { attempt } => attempt,
        }
    }

    /// Why the machine couldn't be reached, as last reported.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// How long until the next attempt is due: `Some(ZERO)` when it is due
    /// now, `None` when no attempt is waiting (connected, or one in flight).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        match self.phase {
            Phase::Waiting { due, .. } => Some(due.saturating_duration_since(now)),
            _ => None,
        }
    }

    /// Take the due attempt: returns its number and marks it in flight, or
    /// `None` when nothing is due yet.
    pub fn take_due(&mut self, now: Instant) -> Option<u32> {
        match self.phase {
            Phase::Waiting { attempt, due } if due <= now => {
                self.phase = Phase::Trying { attempt };
                Some(attempt)
            }
            _ => None,
        }
    }

    /// The attempt in flight found the machine still unreachable: schedule the
    /// next one after the backoff.
    pub fn unreachable(&mut self, reason: &str, now: Instant) {
        self.reason = reason.to_string();
        let attempt = self.attempt().max(1);
        self.phase = Phase::Waiting {
            attempt: attempt + 1,
            due: now + self.policy.backoff(attempt),
        };
    }

    /// The view is attached again (or the attempt reached the machine and the
    /// view moved on to a restart): leave the loop and reset the backoff.
    pub fn connected(&mut self) {
        self.phase = Phase::Connected;
        self.reason.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ReconnectPolicy {
        ReconnectPolicy {
            first_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(5),
        }
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let p = policy();
        let secs: Vec<u64> = (1..=5).map(|a| p.backoff(a).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 5, 5]);
        assert_eq!(p.backoff(u32::MAX), Duration::from_secs(5));
    }

    #[test]
    fn a_lost_link_tries_at_once_then_backs_off_until_it_reconnects() {
        let t0 = Instant::now();
        let mut rc = ReconnectController::new(policy());
        assert!(!rc.is_reconnecting());
        assert_eq!(rc.take_due(t0), None, "nothing to do while connected");

        rc.link_lost("ssh dropped", t0);
        assert!(rc.is_reconnecting());
        assert_eq!(rc.wait(t0), Some(Duration::ZERO), "the first attempt is immediate");
        assert_eq!(rc.take_due(t0), Some(1));
        assert_eq!(rc.wait(t0), None, "no attempt waits while one is in flight");
        assert_eq!(rc.take_due(t0), None);

        rc.unreachable("`gpu` is unreachable", t0);
        assert_eq!(rc.attempt(), 2);
        assert_eq!(rc.reason(), "`gpu` is unreachable");
        assert_eq!(rc.wait(t0), Some(Duration::from_secs(1)));
        assert_eq!(rc.take_due(t0), None, "not due inside the backoff");
        let t1 = t0 + Duration::from_secs(1);
        assert_eq!(rc.take_due(t1), Some(2));

        rc.unreachable("still down", t1);
        assert_eq!(rc.wait(t1), Some(Duration::from_secs(2)), "the backoff doubles");
        let t2 = t1 + Duration::from_secs(2);
        assert_eq!(rc.take_due(t2), Some(3));

        rc.connected();
        assert!(!rc.is_reconnecting());
        assert_eq!(rc.attempt(), 0);
        assert_eq!(rc.reason(), "");

        // A later drop starts over at an immediate first attempt.
        rc.link_lost("again", t2);
        assert_eq!(rc.take_due(t2), Some(1));
    }

    #[test]
    fn a_duplicate_loss_report_keeps_the_backoff() {
        let t0 = Instant::now();
        let mut rc = ReconnectController::new(policy());
        rc.link_lost("drop", t0);
        rc.take_due(t0);
        rc.unreachable("down", t0);
        rc.link_lost("drop again", t0);
        assert_eq!(rc.attempt(), 2, "still on attempt 2, not reset to 1");
        assert_eq!(rc.wait(t0), Some(Duration::from_secs(1)));
        assert_eq!(rc.reason(), "drop again");
    }

    #[test]
    fn it_never_gives_up_on_an_unreachable_machine() {
        let mut now = Instant::now();
        let mut rc = ReconnectController::new(policy());
        rc.link_lost("drop", now);
        for attempt in 1..=50 {
            assert_eq!(rc.take_due(now), Some(attempt));
            rc.unreachable("down", now);
            now += rc.wait(now).unwrap();
        }
        assert!(rc.is_reconnecting());
        assert_eq!(rc.wait(now - Duration::from_secs(5)), Some(Duration::from_secs(5)));
    }
}
