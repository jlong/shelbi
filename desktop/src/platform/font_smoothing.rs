//! macOS font smoothing, off for this app so text draws at the weight John's
//! Figma frames show.
//!
//! With font smoothing on (the macOS default), CoreGraphics thickens glyph
//! stems by an amount that grows with the text color's luminance, and gpui
//! copies it (`glyph_dilation_for_color`, which reads `AppleFontSmoothing`
//! from the app's preferences once, before the first glyph). Light text on
//! the dark theme then reads a weight heavier than Figma draws it (Regular
//! like Medium); dark text on the light theme isn't thickened either way.
//! `dt-figma-qa-sidebar` turned it off (a decision for John to confirm):
//! dark text matches Figma, and terminal text is thinner than in apps that
//! thicken (Zed), the same as Ghostty's default.
//!
//! The setting goes only into the app's own defaults domain (its bundle id,
//! or the binary's name when run unbundled), never the global domain, so no
//! other app changes. To go back to native smoothing, set [`SMOOTH_FONTS`]
//! to `true`: [`apply`] then removes the app's override.

/// Whether the app draws text with macOS font smoothing (stem thickening).
pub const SMOOTH_FONTS: bool = false;

/// Write (or clear) the app's `AppleFontSmoothing` override per
/// [`SMOOTH_FONTS`]. Call at launch, before any text is drawn.
#[cfg(target_os = "macos")]
pub fn apply() {
    use objc2_foundation::{ns_string, NSUserDefaults};
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = ns_string!("AppleFontSmoothing");
    if SMOOTH_FONTS {
        defaults.removeObjectForKey(key);
    } else {
        defaults.setInteger_forKey(0, key);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn apply() {}
