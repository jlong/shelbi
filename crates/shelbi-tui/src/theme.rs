//! Shared visual constants for the TUI.

use ratatui::style::Color;

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
