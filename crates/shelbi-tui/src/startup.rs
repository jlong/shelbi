//! Startup timing and the pre-UI progress (`cli-startup-speed`).
//!
//! `shelbi` does a little work on the plain terminal before the shell takes the
//! screen (the cutover open gate, the migration pass and its consent prompt).
//! Each piece runs as a named step of [`progress`], the shared printer in
//! [`crate::progress`]: every step logs its duration to `tui.log`
//! (`startup phase=… ms=…`), so a slow open can be broken down after the fact,
//! TTY or not, and on a terminal a step outlasting
//! [`GRACE`](crate::progress::GRACE) draws under a `Starting Shelbi…` header.
//!
//! The phases after the shell is up (first frame, daemon, orchestrator) log
//! through [`log_phase`] against the same process clock ([`mark_launch`]).

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::progress::Progress;

static LAUNCHED: OnceLock<Instant> = OnceLock::new();

/// The header startup's progress draws under.
pub const TITLE: &str = "Starting Shelbi…";

/// Record the process's launch instant. Called first thing in `main`; later
/// calls are no-ops. [`since_launch`] falls back to the first call to it.
pub fn mark_launch() {
    let _ = LAUNCHED.set(Instant::now());
}

/// Time since [`mark_launch`].
pub fn since_launch() -> Duration {
    LAUNCHED.get_or_init(Instant::now).elapsed()
}

/// Log one startup phase's duration and the time since launch to `tui.log`.
pub fn log_phase(phase: &str, took: Duration) {
    tracing::info!(
        phase,
        ms = took.as_millis() as u64,
        at_ms = since_launch().as_millis() as u64,
        "startup"
    );
}

/// Startup's progress on stderr: draws when stderr is a terminal that wants
/// it, and logs every step as a startup phase either way.
pub fn progress() -> Progress {
    Progress::stderr(TITLE).with_log(log_phase)
}

/// Startup steps that log as phases but never draw.
pub fn silent() -> Progress {
    Progress::disabled().with_log(log_phase)
}
