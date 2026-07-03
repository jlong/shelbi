//! Shared visual constants for the TUI.

use ratatui::style::Color;

/// Background fill for the selected / focused row across the whole TUI —
/// the sidebar nav selection, the kanban card selection, and the filter
/// dropdowns all paint with this one colour so selection styling can't
/// drift between surfaces. Selected text sets an explicit white/bold
/// foreground so it stays readable on the gray. Kept deliberately dark
/// so it reads as a subtle fill rather than a coloured accent.
pub const SELECTION_BG: Color = Color::Rgb(63, 63, 63);
