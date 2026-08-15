//! Shared visual constants for the TUI.

use ratatui::style::Color;
use std::time::Duration;

/// Background fill for the selected / focused row across the whole TUI —
/// the sidebar nav selection, the kanban card selection, and the filter
/// dropdowns all paint with this one colour so selection styling can't
/// drift between surfaces. Selected text sets an explicit white/bold
/// foreground so it stays readable on the gray. Kept deliberately dark
/// so it reads as a subtle fill rather than a coloured accent.
pub const SELECTION_BG: Color = Color::Rgb(63, 63, 63);

/// Background fill for the small workflow-name badge on kanban cards.
/// Kept a touch bluer/lighter than [`SELECTION_BG`] so the badge still
/// reads as a distinct chip when it lands on a selected card (whose row
/// is painted with `SELECTION_BG`); span backgrounds patch over the row
/// fill, so an identical colour would make the badge vanish on select.
pub const WORKFLOW_BADGE_BG: Color = Color::Rgb(58, 66, 88);

/// Foreground for the workflow badge text — a light near-white that
/// stays legible on [`WORKFLOW_BADGE_BG`] whether or not the card is
/// selected.
pub const WORKFLOW_BADGE_FG: Color = Color::Rgb(220, 223, 232);

/// The "green" token for the Command palette's project-status indicator.
/// A loaded-idle project's ring and an active project's pulsing fill both
/// build on this hue so the two states read as one family; only the
/// not-loaded ring swaps to [`PROJECT_STATUS_NEUTRAL`]. Kept as the same
/// terminal green the rest of the palette already paints "loaded" with, so
/// the indicator honors the existing palette color vocabulary.
pub const PROJECT_STATUS_GREEN: Color = Color::Green;

/// The neutral/unloaded outline token for the project-status indicator — the
/// same dim gray the palette uses for inert affordances (the `+ Add project`
/// row, dimmed metadata). Only a not-loaded project's ring uses it.
pub const PROJECT_STATUS_NEUTRAL: Color = Color::DarkGray;

/// Full breathing period of the active-project pulse: the fill eases from a
/// dim trough (near-transparent against the dark popup) up to full green and
/// back down once per cycle. Kept close to a calm ~1.6s breath rather than a
/// blink.
pub const PROJECT_PULSE_PERIOD: Duration = Duration::from_millis(1143);

/// Fill color for the active-project pulse at animation `phase` (wrapped into
/// `[0.0, 1.0)`). A raised cosine breathes the green channel between a dim
/// trough — which reads as near-transparent against the dark popup, standing
/// in for a fill alpha of ~0 in a terminal that has no real alpha — and full
/// green at the peak, easing at both extremes so the motion looks like
/// breathing rather than an on/off blink. The ring stays green throughout
/// (the glyph is a filled disc), matching the "green ring + cycling fill"
/// intent of the active state.
pub fn project_pulse_color(phase: f32) -> Color {
    // Wrap defensively so a caller passing raw elapsed-fraction (or a value
    // that drifted slightly past 1.0 through rounding) still maps onto one
    // clean cycle.
    let phase = phase.rem_euclid(1.0);
    // 0 → 1 → 0 across the cycle, eased at the extremes by the cosine.
    let alpha = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
    // Trough is a dim green (fill approaching transparent), peak is full
    // green. Interpolate the green channel; red/blue stay 0 so the hue holds.
    const TROUGH: f32 = 45.0;
    const PEAK: f32 = 220.0;
    let g = (TROUGH + (PEAK - TROUGH) * alpha).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(0, g, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_troughs_dim_and_peaks_full_green() {
        // phase 0 and 1.0 (wrap) sit at the trough; phase 0.5 at the peak.
        let trough = project_pulse_color(0.0);
        let peak = project_pulse_color(0.5);
        let wrap = project_pulse_color(1.0);
        assert_eq!(trough, wrap, "phase wraps cleanly at 1.0");
        match (trough, peak) {
            (Color::Rgb(0, lo, 0), Color::Rgb(0, hi, 0)) => {
                assert!(lo < hi, "peak green channel must exceed the trough");
                assert!(lo > 0, "trough stays a dim green, not fully black");
                assert!(hi >= 200, "peak reads as full green");
            }
            other => panic!("pulse must be a pure-green Rgb, got {other:?}"),
        }
    }

    #[test]
    fn pulse_is_symmetric_around_the_peak() {
        // The raised cosine is symmetric: a quarter before and after the peak
        // land on the same fill, so the breath in and out match.
        assert_eq!(project_pulse_color(0.25), project_pulse_color(0.75));
    }
}
