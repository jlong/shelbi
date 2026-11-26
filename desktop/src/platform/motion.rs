//! The system's Reduce Motion setting, which turns the window's slides into
//! crossfades: System Settings ▸ Accessibility ▸ Display on macOS, the
//! desktop's "Animations" switch on Linux (`enable-animations` off,
//! [`super::desktop_settings`]).

/// Whether the user asked macOS to reduce motion. Read when a transition
/// starts, so a change applies to the next one without a relaunch.
#[cfg(target_os = "macos")]
pub fn reduce_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// Whether the desktop turned animations off. Read when a transition
/// starts, from the settings portal's last answer (never blocking).
#[cfg(not(target_os = "macos"))]
pub fn reduce_motion() -> bool {
    !super::desktop_settings::values().animations
}
