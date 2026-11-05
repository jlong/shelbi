//! View Diff's wait for its diff, shared by the TUI and the desktop app
//! (`review-diff-loading-state`).
//!
//! A review can be opened the moment it is assigned to a slot, while the load
//! is still checking its branch out, so the first `Ensure` for the diff session
//! can fail, and even a session that spawned shows nothing until the diff tool
//! draws. [`DiffLoad`] is the wait: the client sends `ensure` when told, tells
//! it the answers and what the attached pane shows, and draws
//! [`DiffLoad::view`]. It holds no timers and takes the time as an argument,
//! so each client drives it from its own loop and tests drive it by hand.
//!
//! - **Loading**: a failed ensure is retried with a short backoff, a silent
//!   session is left to draw, and the view reads [`LOADING_DIFF`].
//! - **Failed**: after [`DIFF_LOAD_TIMEOUT`] without a drawn diff the view
//!   reads [`DIFF_FAILED_TITLE`] with the real cause and a Retry. A diff that
//!   draws late still replaces it.
//! - **Ready**: the first drawn output ends the wait. A diff that is already
//!   there is ready inside [`LOADING_GRACE`], so it never flashes "Loading".
//!
//! Selecting the view again mid-wait never sends a second ensure while one is
//! in flight or a retry is scheduled ([`DiffLoad::select`]).

use std::time::{Duration, Instant};

/// How long a diff may take before the view says it couldn't load it.
pub const DIFF_LOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the view waits before saying "Loading": a diff that is already
/// there is attached and replayed well inside this.
pub const LOADING_GRACE: Duration = Duration::from_millis(300);

/// The first retry of a failed ensure waits this long; each later one doubles
/// it up to [`ENSURE_MAX_BACKOFF`].
const ENSURE_FIRST_BACKOFF: Duration = Duration::from_millis(250);
const ENSURE_MAX_BACKOFF: Duration = Duration::from_secs(2);

/// The view's text while it waits.
pub const LOADING_DIFF: &str = "Loading diff…";
/// The heading over the cause once it has given up.
pub const DIFF_FAILED_TITLE: &str = "Couldn't load the diff";
/// The label of the action that starts the wait over.
pub const DIFF_RETRY_LABEL: &str = "Retry";

/// The cause when the session is up but the tool never drew. A GUI difftool
/// is the likely reason, so the words send the reader to look for its window.
const SILENT_CAUSE: &str =
    "the diff tool is running but hasn't drawn anything here (a tool with its own window opens there)";
/// The cause when the daemon never answered the ensure.
const NO_ANSWER_CAUSE: &str = "the daemon hasn't answered yet";

/// What the client sees of the diff session in its pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSeen {
    /// The pane is showing the diff tool's output.
    Drawn,
    /// Not attached yet, connecting, restarting, or attached with a blank
    /// screen.
    NotYet,
    /// The session isn't there or stopped coming back, with why.
    Ended(String),
}

/// What the diff view draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLoadView {
    /// Too early to say "Loading": nothing new, so a ready diff doesn't flash.
    Settling,
    /// [`LOADING_DIFF`].
    Loading,
    /// [`DIFF_FAILED_TITLE`], this cause, and [`DIFF_RETRY_LABEL`].
    Failed { cause: String },
    /// The terminal.
    Ready,
}

/// Where the ensure stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ensure {
    /// Sent; the answer hasn't come back.
    InFlight,
    /// Failed; send it again at this time.
    RetryAt(Instant),
    /// Failed, and the wait is over: nothing more is sent until a retry.
    Stopped,
    /// The session is up.
    Done,
}

/// One wait for the diff of the review on screen.
#[derive(Debug, Clone)]
pub struct DiffLoad {
    started: Instant,
    ensure: Ensure,
    /// Failed ensures since the wait began, for the backoff.
    failures: u32,
    /// The pane is bound to the diff session.
    attached: bool,
    drawn: bool,
    timed_out: bool,
    /// The latest reason the diff isn't there.
    cause: Option<String>,
}

impl DiffLoad {
    /// A wait starting now, its ensure already on its way.
    pub fn start(now: Instant) -> Self {
        Self {
            started: now,
            ensure: Ensure::InFlight,
            failures: 0,
            attached: false,
            drawn: false,
            timed_out: false,
            cause: None,
        }
    }

    /// View Diff was selected, showing `seen`. Starts a wait unless one is
    /// running (coming back to it mid-load resumes it) or the pane already
    /// shows the diff. A wait that ended, failed or drawn, starts over: its
    /// pane has gone with the view, or this is the re-select that retries.
    /// Returns whether the caller must send an ensure.
    pub fn select(load: &mut Option<DiffLoad>, seen: &DiffSeen, now: Instant) -> bool {
        match load {
            None => {
                *load = Some(Self::start(now));
                true
            }
            Some(l) if *seen == DiffSeen::Drawn => {
                l.observe(seen, now);
                l.attached = true;
                false
            }
            Some(l) if l.drawn || l.timed_out => l.retry(now),
            Some(_) => false,
        }
    }

    /// Start the wait over (Retry). Returns whether the caller must send an
    /// ensure: not while one is still in flight, whose answer serves this wait.
    pub fn retry(&mut self, now: Instant) -> bool {
        let in_flight = self.ensure == Ensure::InFlight;
        *self = Self::start(now);
        !in_flight
    }

    /// The ensure answered.
    pub fn ensure_answered(&mut self, result: Result<(), String>, now: Instant) {
        self.advance(now);
        match result {
            Ok(()) => {
                self.ensure = Ensure::Done;
                self.cause = None;
            }
            Err(cause) => {
                self.cause = Some(cause);
                self.failures += 1;
                self.ensure = if self.timed_out {
                    Ensure::Stopped
                } else {
                    Ensure::RetryAt(now + backoff(self.failures))
                };
            }
        }
    }

    /// Whether a scheduled retry is due: if so it counts as sent, and the
    /// caller must send it.
    pub fn take_due_ensure(&mut self, now: Instant) -> bool {
        self.advance(now);
        match self.ensure {
            Ensure::RetryAt(at) if now >= at => {
                self.ensure = Ensure::InFlight;
                true
            }
            _ => false,
        }
    }

    /// Whether the session is up and the pane isn't bound to it.
    pub fn needs_attach(&self) -> bool {
        self.ensure == Ensure::Done && !self.attached
    }

    /// The pane was bound to the diff session.
    pub fn mark_attached(&mut self) {
        self.attached = true;
    }

    /// Another view took the pane; the wait goes on behind it and attaches
    /// when View Diff comes back.
    pub fn left(&mut self) {
        self.attached = false;
    }

    /// What the pane shows now.
    pub fn observe(&mut self, seen: &DiffSeen, now: Instant) {
        match seen {
            DiffSeen::Drawn => {
                self.drawn = true;
                self.timed_out = false;
                self.cause = None;
            }
            DiffSeen::NotYet => {}
            DiffSeen::Ended(cause) => self.cause = Some(cause.clone()),
        }
        self.advance(now);
    }

    /// Give up if the time is up.
    pub fn advance(&mut self, now: Instant) {
        if self.drawn || self.timed_out || now < self.started + DIFF_LOAD_TIMEOUT {
            return;
        }
        self.timed_out = true;
        if matches!(self.ensure, Ensure::RetryAt(_)) {
            self.ensure = Ensure::Stopped;
        }
    }

    /// What to draw now.
    pub fn view(&self, now: Instant) -> DiffLoadView {
        if self.drawn {
            DiffLoadView::Ready
        } else if self.timed_out {
            DiffLoadView::Failed { cause: self.failure_cause() }
        } else if now < self.started + LOADING_GRACE {
            DiffLoadView::Settling
        } else {
            DiffLoadView::Loading
        }
    }

    /// Whether the client must keep calling in: a wait still running, or a
    /// failure on screen that a late diff can still replace. A wait behind
    /// another view only needs it while its ensure is outstanding.
    pub fn needs_tick(&self, active: bool) -> bool {
        !self.drawn && (active || matches!(self.ensure, Ensure::InFlight | Ensure::RetryAt(_)))
    }

    /// Whether the pane is bound to the diff session.
    pub fn is_attached(&self) -> bool {
        self.attached
    }

    fn failure_cause(&self) -> String {
        match (&self.cause, self.ensure) {
            (Some(cause), _) => cause.clone(),
            (None, Ensure::InFlight) => NO_ANSWER_CAUSE.to_string(),
            (None, _) => SILENT_CAUSE.to_string(),
        }
    }
}

/// How long to wait before retry number `failures` (from 1).
fn backoff(failures: u32) -> Duration {
    ENSURE_FIRST_BACKOFF
        .saturating_mul(2u32.saturating_pow(failures.saturating_sub(1)))
        .min(ENSURE_MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_ready_diff_opens_with_no_loading_flash() {
        let t0 = Instant::now();
        let mut load = DiffLoad::start(t0);
        assert_eq!(load.view(t0), DiffLoadView::Settling);
        load.ensure_answered(Ok(()), t0 + Duration::from_millis(40));
        assert!(load.needs_attach());
        load.mark_attached();
        assert!(!load.needs_attach());
        // Attached and replayed, all inside the grace: it never said Loading.
        let replayed = t0 + Duration::from_millis(90);
        assert_eq!(load.view(replayed), DiffLoadView::Settling);
        load.observe(&DiffSeen::Drawn, replayed);
        assert_eq!(load.view(replayed), DiffLoadView::Ready);
        assert!(!load.needs_tick(true), "the wait is over");
    }

    #[test]
    fn a_worktree_not_ready_is_retried_until_it_is_then_the_diff_replaces_loading() {
        let t0 = Instant::now();
        let mut load = DiffLoad::start(t0);
        load.ensure_answered(Err("the worktree isn't checked out yet".into()), t0 + Duration::from_millis(10));
        let later = t0 + SECOND;
        assert_eq!(load.view(later), DiffLoadView::Loading, "a failure is not an error yet");
        assert!(!load.needs_attach());

        // The retry comes due once, and counts as sent.
        assert!(load.take_due_ensure(later));
        assert!(!load.take_due_ensure(later));
        load.ensure_answered(Ok(()), later);
        assert!(load.needs_attach());
        load.mark_attached();

        // The session is up with nothing on screen yet: still Loading.
        load.observe(&DiffSeen::NotYet, later + SECOND);
        assert_eq!(load.view(later + SECOND), DiffLoadView::Loading);
        load.observe(&DiffSeen::Drawn, later + 2 * SECOND);
        assert_eq!(load.view(later + 2 * SECOND), DiffLoadView::Ready);
    }

    #[test]
    fn a_persistent_failure_gives_up_at_the_timeout_with_the_daemons_cause() {
        let t0 = Instant::now();
        let deadline = t0 + DIFF_LOAD_TIMEOUT;
        let mut load = DiffLoad::start(t0);
        load.ensure_answered(Err("git merge-base failed".into()), t0);
        let mut sends = 1;
        let mut now = t0;
        while now + Duration::from_millis(50) < deadline {
            now += Duration::from_millis(50);
            if load.take_due_ensure(now) {
                sends += 1;
                load.ensure_answered(Err("git merge-base failed".into()), now);
            }
            if now >= t0 + LOADING_GRACE {
                assert_eq!(load.view(now), DiffLoadView::Loading, "no error before the timeout");
            }
        }
        assert!((6..=12).contains(&sends), "a short backoff, capped: {sends} sends");

        load.advance(t0 + DIFF_LOAD_TIMEOUT);
        assert_eq!(
            load.view(t0 + DIFF_LOAD_TIMEOUT),
            DiffLoadView::Failed { cause: "git merge-base failed".into() }
        );
        // Nothing more is sent on its own.
        load.ensure_answered(Err("git merge-base failed".into()), t0 + DIFF_LOAD_TIMEOUT);
        assert!(!load.take_due_ensure(t0 + DIFF_LOAD_TIMEOUT + 60 * SECOND));
    }

    #[test]
    fn retry_starts_the_clock_over_and_can_succeed() {
        let t0 = Instant::now();
        let mut load = DiffLoad::start(t0);
        load.ensure_answered(Err("nope".into()), t0);
        let failed = t0 + DIFF_LOAD_TIMEOUT;
        load.advance(failed);
        assert!(matches!(load.view(failed), DiffLoadView::Failed { .. }));

        assert!(load.retry(failed), "nothing is in flight, so it sends one");
        assert_eq!(load.view(failed), DiffLoadView::Settling);
        assert_eq!(load.view(failed + SECOND), DiffLoadView::Loading);
        load.ensure_answered(Ok(()), failed + SECOND);
        assert!(load.needs_attach());
    }

    #[test]
    fn retry_while_an_ensure_is_in_flight_sends_no_second_one() {
        let t0 = Instant::now();
        let mut load = DiffLoad::start(t0);
        load.advance(t0 + DIFF_LOAD_TIMEOUT);
        assert_eq!(
            load.view(t0 + DIFF_LOAD_TIMEOUT),
            DiffLoadView::Failed { cause: NO_ANSWER_CAUSE.into() },
            "the daemon never answered"
        );
        assert!(!load.retry(t0 + DIFF_LOAD_TIMEOUT));
        // The answer it was waiting on serves the new wait.
        load.ensure_answered(Ok(()), t0 + DIFF_LOAD_TIMEOUT + SECOND);
        assert!(load.needs_attach());
    }

    #[test]
    fn a_silent_session_times_out_and_a_late_draw_replaces_the_error() {
        let t0 = Instant::now();
        let mut load = DiffLoad::start(t0);
        load.ensure_answered(Ok(()), t0);
        load.mark_attached();
        let late = t0 + DIFF_LOAD_TIMEOUT;
        load.observe(&DiffSeen::NotYet, late);
        assert_eq!(load.view(late), DiffLoadView::Failed { cause: SILENT_CAUSE.into() });
        assert!(load.needs_tick(true), "a late diff can still replace the error");

        load.observe(&DiffSeen::Drawn, late + SECOND);
        assert_eq!(load.view(late + SECOND), DiffLoadView::Ready);
    }

    #[test]
    fn a_session_that_ended_names_why() {
        let t0 = Instant::now();
        let mut load = DiffLoad::start(t0);
        load.ensure_answered(Ok(()), t0);
        load.mark_attached();
        load.observe(&DiffSeen::Ended("session exited (code 1)".into()), t0 + SECOND);
        assert_eq!(load.view(t0 + SECOND), DiffLoadView::Loading, "not before the timeout");
        load.advance(t0 + DIFF_LOAD_TIMEOUT);
        assert_eq!(
            load.view(t0 + DIFF_LOAD_TIMEOUT),
            DiffLoadView::Failed { cause: "session exited (code 1)".into() }
        );
    }

    #[test]
    fn coming_back_mid_load_sends_no_second_ensure() {
        let t0 = Instant::now();
        let mut load = None;
        assert!(DiffLoad::select(&mut load, &DiffSeen::NotYet, t0), "the first select sends");

        // Away and back while the ensure is in flight.
        let l = load.as_mut().unwrap();
        l.left();
        assert!(!DiffLoad::select(&mut load, &DiffSeen::NotYet, t0 + SECOND));

        // ...and while a retry is scheduled.
        let l = load.as_mut().unwrap();
        l.ensure_answered(Err("not yet".into()), t0 + SECOND);
        l.left();
        assert!(!DiffLoad::select(&mut load, &DiffSeen::NotYet, t0 + SECOND));

        // ...and once it's up: the answer waited for it, so it just attaches.
        let l = load.as_mut().unwrap();
        l.ensure_answered(Ok(()), t0 + 2 * SECOND);
        l.mark_attached();
        l.left();
        assert!(!DiffLoad::select(&mut load, &DiffSeen::NotYet, t0 + 2 * SECOND));
        assert!(load.as_ref().unwrap().needs_attach());
    }

    #[test]
    fn selecting_a_finished_wait_starts_over_unless_the_pane_still_shows_the_diff() {
        let t0 = Instant::now();
        let mut load = None;
        DiffLoad::select(&mut load, &DiffSeen::NotYet, t0);
        let l = load.as_mut().unwrap();
        l.ensure_answered(Ok(()), t0);
        l.mark_attached();
        l.observe(&DiffSeen::Drawn, t0);

        // The pane still shows it (a re-click, re-entering the review): no ensure.
        assert!(!DiffLoad::select(&mut load, &DiffSeen::Drawn, t0 + SECOND));
        assert_eq!(load.as_ref().unwrap().view(t0 + SECOND), DiffLoadView::Ready);

        // The pane went with another view: ensure again, the session may be gone.
        assert!(DiffLoad::select(&mut load, &DiffSeen::NotYet, t0 + SECOND));
        assert_eq!(load.as_ref().unwrap().view(t0 + SECOND), DiffLoadView::Settling);

        // Re-selecting a failed wait is a retry too.
        let later = t0 + SECOND + DIFF_LOAD_TIMEOUT;
        let l = load.as_mut().unwrap();
        l.ensure_answered(Err("no".into()), later);
        assert!(matches!(l.view(later), DiffLoadView::Failed { .. }));
        assert!(DiffLoad::select(&mut load, &DiffSeen::NotYet, later));
        assert_eq!(load.as_ref().unwrap().view(later), DiffLoadView::Settling);
    }

    #[test]
    fn a_wait_behind_another_view_ticks_only_while_its_ensure_is_outstanding() {
        let t0 = Instant::now();
        let mut load = DiffLoad::start(t0);
        assert!(load.needs_tick(false));
        load.ensure_answered(Err("no".into()), t0);
        assert!(load.needs_tick(false), "a retry is scheduled");
        load.ensure_answered(Ok(()), t0);
        assert!(!load.needs_tick(false));
        assert!(load.needs_tick(true));
    }

    #[test]
    fn the_backoff_grows_and_is_capped() {
        assert_eq!(backoff(1), Duration::from_millis(250));
        assert_eq!(backoff(2), Duration::from_millis(500));
        assert_eq!(backoff(3), Duration::from_secs(1));
        assert_eq!(backoff(4), Duration::from_secs(2));
        assert_eq!(backoff(40), Duration::from_secs(2));
    }
}
