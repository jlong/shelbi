//! The system's Reduce Motion setting (System Settings ▸ Accessibility ▸
//! Display), which turns the window's slides into crossfades.

/// Whether the user asked macOS to reduce motion. Read when a transition
/// starts, so a change applies to the next one without a relaunch.
#[cfg(target_os = "macos")]
pub fn reduce_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

#[cfg(not(target_os = "macos"))]
pub fn reduce_motion() -> bool {
    false
}
