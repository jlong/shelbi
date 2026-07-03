//! Auto-restart supervision for shelbi-managed panes.
//!
//! The sidebar poller (`shelbi-tui`) is the one persistent process that
//! watches every pane shelbi owns — the workspace agent panes and the
//! orchestrator pane. When one of them dies *unexpectedly* (a crash, not a
//! deliberate `workspace stop` / project close / clean user exit) it
//! relaunches it, re-dispatching the workspace's task or re-standing-up the
//! orchestrator, so a crashed pane comes back on its own instead of sitting
//! dead until the user notices.
//!
//! This module is the pure decision core: given a fresh liveness
//! observation plus the two discriminators the caller gathers (was the death
//! deliberate? is there still work to keep the pane up for?), it returns
//! whether to relaunch, give up, or do nothing — and it owns the
//! crash-loop backoff so a pane that can't stay up stops being restarted
//! after a few tries. All tmux/state I/O lives in the caller (the poller);
//! keeping the state machine pure makes the backoff + give-up rules unit
//! testable without spinning up panes.
//!
//! Crash-vs-deliberate discrimination is done off a dedicated no-restart
//! marker ([`shelbi_state::supervision_shutdown_key`]): the pane lifecycle
//! wrapper marks it on a clean/intentional exit, and every shelbi-initiated
//! teardown routes through it, so a death with no fresh marker is a crash.

use std::time::{Duration, Instant};

/// How many restarts inside [`CRASH_LOOP_WINDOW`] before we give up and
/// leave the pane for the user. The would-be `MAX_RESTARTS_IN_WINDOW + 1`th
/// restart within the window trips the give-up.
pub const MAX_RESTARTS_IN_WINDOW: usize = 3;

/// Sliding window for the crash-loop cap. Restarts older than this are
/// pruned, so a slow drip (one crash every few minutes) never accumulates
/// into a give-up — only a genuine tight loop does.
pub const CRASH_LOOP_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Base backoff between successive restarts. The Nth restart in the current
/// window must wait `BASE_BACKOFF * 2^(N-1)` since the previous one, so a
/// pane that keeps crashing is retried at 0s, then ≥5s, then ≥10s before
/// the cap trips — exponential backoff with a hard ceiling.
pub const BASE_BACKOFF: Duration = Duration::from_secs(5);

/// How long a relaunched pane must stay alive before we consider it
/// *recovered* and forget the crash-loop history. A pane that comes back
/// only briefly (our own restart, then another crash) keeps its history so
/// repeated fast crashes still trip the cap; one that survives past this
/// threshold resets to a clean slate so an unrelated crash much later
/// starts counting from zero.
pub const STABLE_RECOVERY: Duration = Duration::from_secs(60);

/// Per-pane supervision bookkeeping. Lives in-memory in the poller thread
/// that owns the pane (alongside `last_dialog` / `last_known`), so a poller
/// restart re-seeds it to `Default` — at worst that re-arms one restart for
/// a pane that was mid-crash-loop, which is acceptable.
#[derive(Debug, Default)]
pub struct SupervisionState {
    /// Have we ever observed this pane alive? We refuse to "adopt" a pane
    /// that was already dead when we started watching (a stale pane from a
    /// previous session, or one still bootstrapping) — supervision only
    /// kicks in for a pane we saw come up and then die.
    ever_alive: bool,
    /// Restart timestamps still inside the crash-loop window (pruned each
    /// decision). Length is the crash-loop counter.
    restarts: Vec<Instant>,
    /// Latched once we emit the give-up line, so we neither spam it nor
    /// resume restarting while the pane stays down. Cleared only when the
    /// pane recovers (stays alive past [`STABLE_RECOVERY`]).
    gave_up: bool,
}

/// What the supervisor should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionAction {
    /// Nothing to do — the pane is alive, idle, deliberately down, already
    /// given up, or still inside its backoff window.
    None,
    /// Relaunch the pane now (and re-dispatch its task, for a workspace).
    Restart,
    /// The crash-loop cap tripped — emit the give-up line once and stop.
    GiveUp,
}

/// The three inputs to one supervision decision, gathered by the caller.
/// `intentional_shutdown` and `has_work` are only consulted when
/// `!alive`, so the caller may leave them `false` for a live pane.
pub struct SupervisionInputs {
    /// Is the pane alive right now?
    pub alive: bool,
    /// Was the death deliberate (a fresh no-restart marker was consumed, or
    /// the caller otherwise knows this was not a crash)? Suppresses restart.
    pub intentional_shutdown: bool,
    /// Is there still work to keep the pane up for — an active task for a
    /// workspace, or "always" for the orchestrator? A dead idle workspace
    /// (e.g. a dev pane closed after its handoff) has nothing to relaunch.
    pub has_work: bool,
}

impl SupervisionState {
    /// Advance the state machine one tick and return the action to take.
    /// `now` is threaded in (rather than read from the clock) so the
    /// backoff / window / recovery timing is fully unit testable.
    pub fn decide(&mut self, i: &SupervisionInputs, now: Instant) -> SupervisionAction {
        if i.alive {
            self.ever_alive = true;
            // Consider the pane recovered once it has stayed up well past
            // the last restart, then forget the crash-loop history. A pane
            // that only flapped back briefly keeps its history so the cap
            // still trips.
            match self.restarts.last() {
                Some(&last) if now.duration_since(last) >= STABLE_RECOVERY => {
                    self.restarts.clear();
                    self.gave_up = false;
                }
                Some(_) => {}
                None => self.gave_up = false,
            }
            return SupervisionAction::None;
        }

        // Pane is dead from here on.
        if !self.ever_alive {
            // Never saw it alive — don't adopt a pre-existing dead pane.
            return SupervisionAction::None;
        }
        if i.intentional_shutdown {
            // Deliberate stop / close / clean exit: stand down until the
            // pane comes back on its own, and forget any crash history.
            self.reset();
            return SupervisionAction::None;
        }
        if !i.has_work {
            // Idle workspace (e.g. closed after a review handoff) — nothing
            // to relaunch for.
            self.reset();
            return SupervisionAction::None;
        }
        if self.gave_up {
            // Already gave up on this crash loop; wait for the user (or a
            // recovery) rather than retrying forever.
            return SupervisionAction::None;
        }

        self.restarts.retain(|&t| now.duration_since(t) < CRASH_LOOP_WINDOW);
        if self.restarts.len() >= MAX_RESTARTS_IN_WINDOW {
            self.gave_up = true;
            return SupervisionAction::GiveUp;
        }
        // Exponential backoff between attempts.
        if let Some(&last) = self.restarts.last() {
            let wait = BASE_BACKOFF * (1u32 << (self.restarts.len() - 1));
            if now.duration_since(last) < wait {
                return SupervisionAction::None;
            }
        }
        self.restarts.push(now);
        SupervisionAction::Restart
    }

    /// Drop the crash-loop history and un-latch give-up. Used when the pane
    /// went down for a non-crash reason (deliberate shutdown or idle), and
    /// `ever_alive` is dropped so we require a fresh sighting before
    /// supervising it again.
    fn reset(&mut self) {
        self.ever_alive = false;
        self.restarts.clear();
        self.gave_up = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dead_crash() -> SupervisionInputs {
        SupervisionInputs { alive: false, intentional_shutdown: false, has_work: true }
    }
    fn alive() -> SupervisionInputs {
        SupervisionInputs { alive: true, intentional_shutdown: false, has_work: false }
    }

    #[test]
    fn never_seen_alive_is_not_adopted() {
        let mut s = SupervisionState::default();
        // A pane that's dead the very first time we look at it (stale /
        // foreign / still bootstrapping) must not be restarted.
        let t = Instant::now();
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::None);
    }

    #[test]
    fn crash_after_alive_triggers_one_restart() {
        let mut s = SupervisionState::default();
        let t = Instant::now();
        assert_eq!(s.decide(&alive(), t), SupervisionAction::None);
        // First crash → immediate restart (no prior attempt to back off from).
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::Restart);
    }

    #[test]
    fn intentional_shutdown_never_restarts() {
        let mut s = SupervisionState::default();
        let t = Instant::now();
        s.decide(&alive(), t);
        let stop = SupervisionInputs { alive: false, intentional_shutdown: true, has_work: true };
        assert_eq!(s.decide(&stop, t), SupervisionAction::None);
        // And it stays down on subsequent dead ticks (no marker anymore):
        // the reset dropped `ever_alive`, so a crash-shaped observation is
        // treated as a not-yet-seen pane, not a fresh crash.
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::None);
    }

    #[test]
    fn idle_workspace_is_not_restarted() {
        let mut s = SupervisionState::default();
        let t = Instant::now();
        s.decide(&alive(), t);
        let idle = SupervisionInputs { alive: false, intentional_shutdown: false, has_work: false };
        assert_eq!(s.decide(&idle, t), SupervisionAction::None);
    }

    #[test]
    fn crash_loop_gives_up_after_cap_then_stays_quiet() {
        let mut s = SupervisionState::default();
        let base = Instant::now();
        // Drive three restarts, each after its backoff has elapsed, with a
        // brief re-alive in between (as a real relaunch would produce).
        let mut t = base;
        for n in 0..MAX_RESTARTS_IN_WINDOW {
            assert_eq!(s.decide(&alive(), t), SupervisionAction::None);
            // Advance just past this attempt's backoff so it's allowed.
            t += BASE_BACKOFF * (1u32 << n) + Duration::from_secs(1);
            assert_eq!(
                s.decide(&dead_crash(), t),
                SupervisionAction::Restart,
                "restart {n} should fire"
            );
        }
        // The next crash (still inside the 5-min window) trips give-up once…
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::GiveUp);
        // …and then goes quiet — no repeated give-up spam, no more restarts.
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::None);
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::None);
    }

    #[test]
    fn backoff_holds_off_a_too_soon_retry() {
        let mut s = SupervisionState::default();
        let t = Instant::now();
        s.decide(&alive(), t);
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::Restart);
        // Immediately dead again, before the 2nd attempt's backoff: hold.
        s.decide(&alive(), t);
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::None);
        // Past the backoff: the 2nd restart is allowed.
        let later = t + BASE_BACKOFF + Duration::from_secs(1);
        s.decide(&alive(), later);
        assert_eq!(s.decide(&dead_crash(), later), SupervisionAction::Restart);
    }

    #[test]
    fn stable_recovery_resets_the_crash_counter() {
        let mut s = SupervisionState::default();
        let t = Instant::now();
        s.decide(&alive(), t);
        assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::Restart);
        // The relaunch comes up and stays alive past the recovery window…
        let recovered = t + STABLE_RECOVERY + Duration::from_secs(1);
        assert_eq!(s.decide(&alive(), recovered), SupervisionAction::None);
        // …so a fresh crash much later is a clean first restart again, not
        // the second attempt of the old loop.
        assert_eq!(s.decide(&dead_crash(), recovered), SupervisionAction::Restart);
    }

    #[test]
    fn slow_drip_never_accumulates_to_give_up() {
        let mut s = SupervisionState::default();
        let mut t = Instant::now();
        // One crash well past the recovery window each time: each is a fresh
        // first restart, the counter never climbs to the cap.
        for _ in 0..6 {
            assert_eq!(s.decide(&alive(), t), SupervisionAction::None);
            assert_eq!(s.decide(&dead_crash(), t), SupervisionAction::Restart);
            t += STABLE_RECOVERY + Duration::from_secs(5);
        }
    }
}
