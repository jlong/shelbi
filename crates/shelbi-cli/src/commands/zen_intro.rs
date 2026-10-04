//! First-enable Zen Mode intro popover.
//!
//! The implementation moved to `shelbi_tui::overlay::zen_intro` so the
//! single-process TUI overlay (removing-tmux Phase 4d) and this legacy
//! in-alt-screen palette popover share one copy (state machine, step function,
//! and `render_intro`). This module re-exports it; the palette's event loop in
//! [`super::palette`] drives it unchanged.

pub use shelbi_tui::overlay::zen_intro::{render_intro, step_intro, IntroOutcome, IntroState};
