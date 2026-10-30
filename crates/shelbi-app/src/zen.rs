//! Zen Mode's toggle, as the palette runs it.
//!
//! `ToggleZen` is a lifecycle mutation that never went through the daemon's
//! issue queue: it flips the project's Zen state on disk (after making sure
//! the daemon is the one this client expects). Both clients run these off
//! their UI thread.
//!
//! A successful toggle shows nothing: the sidebar's Zen band (and its absence)
//! is the indicator. A failure goes to the project's error log, which lights
//! the sidebar's error badge ([`report`]).

/// The error-log `source` tag for a failed toggle.
const ERROR_SOURCE: &str = "zen";

/// Whether the first-run intro should show before toggling Zen for
/// `project`: only on an off-to-on toggle, and only while the global
/// `zen_intro_seen` flag is unset.
pub fn should_show_intro(project: &str) -> bool {
    let project_off = shelbi_state::read_state(project)
        .map(|s| s.zen_mode == shelbi_state::ZenModeState::Off)
        .unwrap_or(true);
    project_off
        && !shelbi_state::read_global_state()
            .map(|s| s.zen_intro_seen)
            .unwrap_or(false)
}

/// Toggle Zen Mode for `project`.
pub fn toggle(project: &str) -> Result<(), String> {
    shelbi_state::ensure_daemon_matches_for_mutation().map_err(|e| e.to_string())?;
    shelbi_state::toggle_zen_mode(project, "user:palette").map_err(|e| e.to_string())?;
    Ok(())
}

/// Apply the intro's answer: persist "don't show again", then toggle on
/// confirm. Cancelling is a quiet `Ok`.
pub fn apply_intro(project: &str, confirmed: bool, dont_show_again: bool) -> Result<(), String> {
    if dont_show_again {
        let _ = shelbi_state::mark_zen_intro_seen();
    }
    if !confirmed {
        return Ok(());
    }
    toggle(project)
}

/// Settle a toggle's `result` for `project`: nothing to show on success; a
/// failure is recorded in the project's error log (the sidebar's error badge).
/// Returns a status line only when that log write itself failed, so the error
/// is never lost.
pub fn report(project: &str, result: Result<(), String>) -> Option<String> {
    let e = result.err()?;
    let message = format!("Zen toggle failed: {e}");
    match shelbi_state::append_error(project, &message, Some(ERROR_SOURCE)) {
        Ok(_) => None,
        Err(_) => Some(message),
    }
}
