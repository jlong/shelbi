//! Zen Mode's toggle, as the palette runs it.
//!
//! `ToggleZen` is a lifecycle mutation that never went through the daemon's
//! issue queue: it asks the project's daemon to flip the project's Zen state
//! ([`StateWrite::ToggleZen`]), after making sure this machine's daemon is the
//! one this client expects. A remote window asks its host's daemon over the
//! relay. Both clients run these off their UI thread.
//!
//! A successful toggle shows nothing: the sidebar's Zen band (and its absence)
//! is the indicator. A failure goes to the project's error log, which lights
//! the sidebar's error badge ([`report`]).

use crate::source::ProjectLink;
use crate::state_write::{self, StateWrite};

/// The error-log `source` tag for a failed toggle.
const ERROR_SOURCE: &str = "zen";

/// Whether the first-run intro should show before toggling Zen for
/// `project`: only on an off-to-on toggle, and only while the global
/// `zen_intro_seen` flag is unset.
pub fn should_show_intro(project: &str) -> bool {
    intro_due(shelbi_state::read_state(project).map(|s| s.zen_mode).ok())
}

/// [`should_show_intro`] over the project's Zen state as the window's project
/// source read it (`None` when it couldn't be read, which counts as off). The
/// `zen_intro_seen` flag is the viewer's own, so it is always read here.
pub fn intro_due(project_zen: Option<shelbi_state::ZenModeState>) -> bool {
    let project_off = project_zen
        .map(|z| z == shelbi_state::ZenModeState::Off)
        .unwrap_or(true);
    project_off
        && !shelbi_state::read_global_state()
            .map(|s| s.zen_intro_seen)
            .unwrap_or(false)
}

/// Toggle Zen Mode for `project`, through the window's `link` (`None`: this
/// machine's daemon). A project on this machine first checks that its daemon
/// is this client's version; a remote window settled that before it opened.
pub fn toggle(project: &str, link: Option<&ProjectLink>) -> Result<(), String> {
    if !link.is_some_and(ProjectLink::is_remote) {
        shelbi_state::ensure_daemon_matches_for_mutation().map_err(|e| e.to_string())?;
    }
    state_write::write(
        project,
        link,
        StateWrite::ToggleZen {
            source: "user:palette".into(),
        },
    )
}

/// Apply the intro's answer: persist "don't show again" (the viewer's own
/// flag, on this machine), then toggle on confirm. Cancelling is a quiet `Ok`.
pub fn apply_intro(
    project: &str,
    link: Option<&ProjectLink>,
    confirmed: bool,
    dont_show_again: bool,
) -> Result<(), String> {
    if dont_show_again {
        let _ = shelbi_state::mark_zen_intro_seen();
    }
    if !confirmed {
        return Ok(());
    }
    toggle(project, link)
}

/// Settle a toggle's `result` for `project`: nothing to show on success; a
/// failure is recorded in the project's error log (the sidebar's error badge),
/// through the same daemon as the toggle. Returns a status line only when that
/// log write itself failed, so the error is never lost.
pub fn report(project: &str, link: Option<&ProjectLink>, result: Result<(), String>) -> Option<String> {
    let e = result.err()?;
    let message = format!("Zen toggle failed: {e}");
    let write = StateWrite::AppendError {
        message: message.clone(),
        source: Some(ERROR_SOURCE.to_string()),
    };
    match state_write::write(project, link, write) {
        Ok(()) => None,
        Err(_) => Some(message),
    }
}
