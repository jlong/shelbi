//! Zen Mode's toggle, as the palette runs it.
//!
//! `ToggleZen` is a lifecycle mutation that never went through the daemon's
//! issue queue: it flips the project's Zen state on disk (after making sure
//! the daemon is the one this client expects). Both clients run these off
//! their UI thread.

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

/// Toggle Zen Mode for `project`. Returns the status line to show ("Zen Mode
/// enabled" / "Zen Mode disabled").
pub fn toggle(project: &str) -> Result<String, String> {
    shelbi_state::ensure_daemon_matches_for_mutation().map_err(|e| e.to_string())?;
    let state = shelbi_state::toggle_zen_mode(project, "user:palette").map_err(|e| e.to_string())?;
    let word = if matches!(state, shelbi_state::ZenModeState::Off) {
        "disabled"
    } else {
        "enabled"
    };
    Ok(format!("Zen Mode {word}"))
}

/// Apply the intro's answer: persist "don't show again", then toggle on
/// confirm. Returns the status line.
pub fn apply_intro(project: &str, confirmed: bool, dont_show_again: bool) -> String {
    if dont_show_again {
        let _ = shelbi_state::mark_zen_intro_seen();
    }
    if !confirmed {
        return "Zen Mode: cancelled".to_string();
    }
    match toggle(project) {
        Ok(msg) => msg,
        Err(e) => format!("Zen toggle failed: {e}"),
    }
}
