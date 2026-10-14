//! The client's restart decision core for the one pane a view is showing.
//!
//! A client view (the TUI's `SessionManager`, the desktop's terminal views)
//! shows one session at a time. When that session's program exits, or a
//! reconnect finds no live session, the view asks this controller what to do,
//! one observation per tick. The controller feeds the shared crash-loop core
//! ([`crate::supervision::SupervisionState`], the daemon's own cap / backoff /
//! window) and folds in the two client-side facts the core doesn't know: the
//! pane being viewed always counts as wanted ("has work"), and a deliberate
//! teardown (`q` / Close / quit) is the one exit that is never restarted.
//!
//! The view owns the IO (the relaunch step, the reconnect) and its own
//! rendering; this module only decides, so the transitions are unit-testable
//! with an injected clock and no session.

use std::time::Instant;

use crate::supervision::{
    SupervisionAction, SupervisionInputs, SupervisionState, MAX_RESTARTS_IN_WINDOW,
};

/// What a view should do about the pane it shows having died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathAction {
    /// Relaunch (and re-attach) now. `attempt` is 1-based, for the
    /// "restarting (attempt/max)" notice.
    Restart { attempt: u32 },
    /// The crash-loop cap tripped: leave the pane dead and show the give-up
    /// notice until the user re-opens it.
    GiveUp,
    /// Inside the backoff between attempts: show "restarting
    /// (next_attempt/max)" so the user sees the pane is coming back.
    Backoff { next_attempt: u32 },
    /// The teardown was deliberate: stop supervising and clear the pane.
    StandDown,
    /// The pane never came up (a first-open connect failure): leave whatever
    /// the view shows (its failure) as it is.
    Leave,
}

/// Per-pane restart bookkeeping for a client view.
#[derive(Debug, Default)]
pub struct RestartController {
    state: SupervisionState,
    /// Set when the client is deliberately tearing the pane down, so the
    /// resulting exit is read as intentional and not restarted. Cleared
    /// whenever a (new or re-opened) target is shown.
    deliberate_close: bool,
}

impl RestartController {
    /// A controller over an existing core state (tests build a given-up one).
    pub fn with_state(state: SupervisionState) -> Self {
        Self {
            state,
            deliberate_close: false,
        }
    }

    /// The view started showing a target. `reopen` is true when the user
    /// re-selected the same target that had failed, given up, or is mid-restart
    /// (the "navigate away and back relaunches it" gesture): the crash-loop
    /// budget resets and stays armed, so the very next death relaunches at once.
    /// Any other show is a fresh binding with a clean budget.
    pub fn begin(&mut self, reopen: bool) {
        if reopen {
            self.state.request_relaunch();
        } else {
            self.state = SupervisionState::default();
        }
        self.deliberate_close = false;
    }

    /// Mark the pane as being torn down deliberately, so the exit that follows
    /// stands supervision down instead of restarting it. Cleared on the next
    /// [`begin`](Self::begin).
    pub fn note_deliberate_close(&mut self) {
        self.deliberate_close = true;
    }

    /// Withdraw a deliberate-teardown mark (the teardown it announced failed,
    /// so a later death is a crash again).
    pub fn clear_deliberate_close(&mut self) {
        self.deliberate_close = false;
    }

    /// Whether a deliberate teardown is pending.
    pub fn deliberate_close(&self) -> bool {
        self.deliberate_close
    }

    /// The bound pane is up. A pane that stays up past the recovery window
    /// forgets its crash history; a brief flap-back keeps it so the cap still
    /// trips.
    pub fn alive(&mut self, now: Instant) {
        self.state.decide(
            &SupervisionInputs {
                alive: true,
                intentional_shutdown: false,
                has_work: false,
            },
            now,
        );
    }

    /// The bound pane is dead (its program exited, or a reconnect found no live
    /// session). The pane being viewed is always wanted, so only a deliberate
    /// teardown keeps it down.
    pub fn dead(&mut self, now: Instant) -> DeathAction {
        let action = self.state.decide(
            &SupervisionInputs {
                alive: false,
                intentional_shutdown: self.deliberate_close,
                has_work: true,
            },
            now,
        );
        match action {
            // `restart_count` is post-increment here: the 1-based attempt.
            SupervisionAction::Restart => DeathAction::Restart {
                attempt: self.state.restart_count() as u32,
            },
            SupervisionAction::GiveUp => DeathAction::GiveUp,
            SupervisionAction::None if self.deliberate_close => DeathAction::StandDown,
            // Never seen alive: a first-open failure, not a crash.
            SupervisionAction::None if !self.state.ever_alive() => DeathAction::Leave,
            SupervisionAction::None => DeathAction::Backoff {
                next_attempt: self.state.restart_count() as u32 + 1,
            },
        }
    }

    /// Restarts currently counted against the cap.
    pub fn restart_count(&self) -> usize {
        self.state.restart_count()
    }

    /// The cap, for "N/max" notices.
    pub fn max_restarts() -> u32 {
        MAX_RESTARTS_IN_WINDOW as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervision::BASE_BACKOFF;
    use std::time::Duration;

    /// Crash a live pane `n` times, each past its backoff, returning the clock.
    fn crash_loop(c: &mut RestartController, mut t: Instant, n: usize) -> Instant {
        for i in 0..n {
            c.alive(t);
            t += BASE_BACKOFF * (1u32 << i) + Duration::from_secs(1);
            assert_eq!(
                c.dead(t),
                DeathAction::Restart {
                    attempt: i as u32 + 1
                }
            );
        }
        t
    }

    #[test]
    fn a_pane_that_never_came_up_is_left_alone() {
        let mut c = RestartController::default();
        c.begin(false);
        assert_eq!(c.dead(Instant::now()), DeathAction::Leave);
    }

    #[test]
    fn a_crash_restarts_at_once_then_backs_off() {
        let mut c = RestartController::default();
        c.begin(false);
        let t = Instant::now();
        c.alive(t);
        assert_eq!(c.dead(t), DeathAction::Restart { attempt: 1 });
        // The relaunched pane dies again straight away: wait out the backoff,
        // showing the attempt that comes next.
        c.alive(t);
        assert_eq!(c.dead(t), DeathAction::Backoff { next_attempt: 2 });
        assert_eq!(
            c.dead(t + BASE_BACKOFF),
            DeathAction::Restart { attempt: 2 }
        );
    }

    #[test]
    fn the_cap_gives_up_and_a_reopen_rearms_it() {
        let mut c = RestartController::default();
        c.begin(false);
        let t = crash_loop(&mut c, Instant::now(), MAX_RESTARTS_IN_WINDOW);
        assert_eq!(c.dead(t), DeathAction::GiveUp);
        // Given up: further deaths change nothing until the user re-opens.
        assert_eq!(
            c.dead(t + Duration::from_secs(1)),
            DeathAction::Backoff {
                next_attempt: MAX_RESTARTS_IN_WINDOW as u32 + 1
            }
        );
        c.begin(true);
        assert_eq!(c.restart_count(), 0, "the budget is reset");
        assert_eq!(
            c.dead(t + Duration::from_secs(2)),
            DeathAction::Restart { attempt: 1 },
            "a re-opened pane relaunches on its very next death"
        );
    }

    #[test]
    fn a_deliberate_close_stands_down_and_the_next_show_clears_it() {
        let mut c = RestartController::default();
        c.begin(false);
        let t = Instant::now();
        c.alive(t);
        c.note_deliberate_close();
        assert!(c.deliberate_close());
        assert_eq!(c.dead(t), DeathAction::StandDown);
        c.begin(false);
        assert!(!c.deliberate_close());
    }

    #[test]
    fn a_fresh_show_forgets_the_previous_targets_budget() {
        let mut c = RestartController::default();
        c.begin(false);
        let t = crash_loop(&mut c, Instant::now(), 3);
        assert_eq!(c.restart_count(), 3);
        c.begin(false);
        assert_eq!(c.restart_count(), 0);
        assert_eq!(c.dead(t), DeathAction::Leave, "a new binding must be seen alive first");
    }

    #[test]
    fn staying_up_past_recovery_forgets_the_crash_history() {
        let mut c = RestartController::default();
        c.begin(false);
        let t = crash_loop(&mut c, Instant::now(), 2);
        c.alive(t + crate::supervision::STABLE_RECOVERY);
        assert_eq!(c.restart_count(), 0);
    }
}
