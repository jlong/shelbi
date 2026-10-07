//! Shared visual constants for the TUI.

use ratatui::style::Color;
use std::time::Duration;

/// The sidebar's solid background (`color/background`, #000000). The sidebar
/// paints this across its whole area — including the half-block bleed rows,
/// whose glyphs carry the fill as their *foreground* on this background — so
/// the search fill and the selection fills read against a true black rather
/// than the terminal's own (often lighter) default background.
pub const BACKGROUND: Color = Color::Rgb(0, 0, 0);

/// Normal sidebar text (`color/gray`, #bababa): unselected nav labels, the
/// workspace and review names, and the search box's label and shortcut.
pub const TEXT: Color = Color::Rgb(186, 186, 186);

/// The selected nav label (`color/white`, #ffffff), drawn bold on the
/// selection fill. Only the nav label brightens on selection; the workspace
/// and review rows keep their normal [`TEXT`] colour under the fill.
pub const TEXT_SELECTED: Color = Color::Rgb(255, 255, 255);

/// Muted sidebar chrome (`color/muted`, #7a7a7a): section headers, the idle `·`
/// bullet, right-aligned agent / idle labels, the `⎇` branch line, and the
/// version line.
pub const MUTED: Color = Color::Rgb(122, 122, 122);

/// The sidebar accent (`color/cyan`, #00a6b2): the project name (bold) and the
/// ready-for-review `✓` (bold).
pub const ACCENT: Color = Color::Rgb(0, 166, 178);

/// A working workspace's `⏵` badge (`color/green`, #5acd25).
pub const BUSY_GREEN: Color = Color::Rgb(90, 205, 37);

/// Background fill for the selected / focused row across the whole TUI —
/// the sidebar nav selection, the kanban card selection, and the filter
/// dropdowns all paint with this one colour so selection styling can't
/// drift between surfaces. Selected text sets an explicit white/bold
/// foreground so it stays readable on the gray. Kept deliberately dark
/// so it reads as a subtle fill rather than a coloured accent.
pub const SELECTION_BG: Color = Color::Rgb(63, 63, 63);

/// Fill for the sidebar's search box (`color/search`, #292929) — a quieter gray
/// than [`SELECTION_BG`] so the box reads as an always-present input affordance
/// rather than an active selection. The sidebar now paints its whole area with
/// [`BACKGROUND`] (#000000), so this Figma token reads clearly against the true
/// black it sits on (it would all but vanish on a lighter terminal default like
/// John's Ghostty ~#1c1c1c, which the painted background removes from play).
pub const SEARCH_BG: Color = Color::Rgb(41, 41, 41);

/// The command palette's panel fill (`color/search` in the Figma design,
/// `#292929`). A touch lighter than the app's near-black background so the
/// palette reads as a raised, borderless panel floating over the view rather
/// than a framed box. The whole overlay rect is painted with this before any
/// text lands.
pub const PALETTE_BG: Color = Color::Rgb(41, 41, 41);

/// The palette's primary text (`color/foreground`, `#c6c6c6`): the `❯` prompt,
/// a typed query, and an unselected command's label. The selected command's
/// label brightens to white (see [`SELECTION_BG`]).
pub const PALETTE_FG: Color = Color::Rgb(198, 198, 198);

/// The palette's muted/secondary text (`color/muted`, `#7a7a7a`): command
/// descriptions, the right-aligned shortcut hint, the placeholder, the
/// Projects heading and its `·` bullets, and the footer hint line.
pub const PALETTE_MUTED: Color = Color::Rgb(122, 122, 122);

/// The palette's accent green (`color/green`, `#5acd25`): the block cursor's
/// fill in the search line, and a loaded project's status ring.
pub const PALETTE_GREEN: Color = Color::Rgb(90, 205, 37);

/// The review panel's accent cyan (`color/accent`, `#00a6b2` in the Figma):
/// the review status header beside the back button and the `More` link that
/// opens the task-description popover. Deliberately the brand teal, not the
/// terminal's ANSI cyan, so the review header reads as the same accent the
/// design pins.
pub const ACCENT_CYAN: Color = Color::Rgb(0, 166, 178);

/// Secondary foreground (`#bababa`): the review panel's worktree line and its
/// unselected nav labels. Sits just under [`PALETTE_FG`] (`#c6c6c6`, the
/// task-description body) so the chrome reads as present-but-quiet without
/// dropping to the dim [`PALETTE_MUTED`].
pub const FG_SECONDARY: Color = Color::Rgb(186, 186, 186);

/// The review panel's Reject red (`color/red`, `#e04f52` in the Figma). Paired
/// with [`PALETTE_GREEN`] (`#5acd25`) for Approve on the actions row.
pub const ACTION_RED: Color = Color::Rgb(224, 79, 82);

/// The glyph painted down the sidebar's rightmost column as the resize
/// drag handle and right-edge divider — a full-height left one-eighth block
/// (`▏`, the Figma `color/divider` rule) that reads as a thin border.
pub const DIVIDER_GLYPH: &str = "▏";

/// Resting color of the divider / drag-handle line (`color/divider`, #414141):
/// a dim gray that reads as a border rather than an accent until the pointer
/// finds it.
pub const DIVIDER_DIM: Color = Color::Rgb(65, 65, 65);

/// Highlight color of the drag-handle line while the pointer hovers it or a
/// resize drag is underway — the cyan accent the sidebar title already uses,
/// drawn bold so the whole rule brightens as an unmistakable affordance.
pub const DIVIDER_ACTIVE: Color = Color::Cyan;

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
/// the indicator honors the existing palette color vocabulary. Shares the
/// Figma brand green (`#5acd25`, see [`PALETTE_GREEN`]) so the project ring and
/// the search cursor read as the one accent.
pub const PROJECT_STATUS_GREEN: Color = PALETTE_GREEN;

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
