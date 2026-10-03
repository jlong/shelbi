//! The client-side emulator seam.
//!
//! Each terminal view owns a client-side emulator fed by the session's output
//! stream. The client locks its emulator to the session's size (a `resized`
//! frame travels in the output stream) so every emulator reflows at the same
//! point in the byte stream.
//!
//! The concrete emulator crate is **not chosen here**: that is the
//! `rt-spike-emulator-replay` decision, between a vendored `alacritty_terminal`
//! and a `vt100`-family crate. This module defines the trait the renderer
//! depends on so the two UIs (ratatui, gpui) and the chosen emulator can be
//! wired up without either side knowing the other's concrete types.
//!
//! TODO (`rt-term`): back this with the chosen emulator; implement feeding
//! output, applying replay, and exposing the grid for rendering.

/// The grid a renderer needs from an emulator, in toolkit-neutral terms.
///
/// Skeleton: the shape is intentionally minimal here. `rt-term` expands it to
/// cells, colors, cursor, and mode flags once the emulator crate is chosen.
pub trait TerminalGrid {
    /// Visible size as `(cols, rows)`.
    fn size(&self) -> (u16, u16);
}

/// Feed output into, and read the grid out of, a client-side emulator.
///
/// TODO (`rt-term`): implement over the chosen emulator crate. `feed` advances
/// the parser (replies to queries are discarded); `grid` exposes the current
/// screen for rendering.
pub trait Emulator {
    /// The grid type this emulator exposes for rendering.
    type Grid: TerminalGrid;

    /// Feed a chunk of raw output bytes from the session.
    fn feed(&mut self, bytes: &[u8]);

    /// Borrow the current grid for rendering.
    fn grid(&self) -> &Self::Grid;
}
