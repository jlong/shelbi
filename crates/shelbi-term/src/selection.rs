//! Text selection and copy.
//!
//! Selection is Shelbi's own, driven by Shift+drag (and plain drag when the
//! program has not asked for the mouse). Copy uses OSC 52 so it works over SSH
//! and through an outer tmux or Screen, with a native clipboard fallback when
//! running locally.
//!
//! TODO (`rt-term`): implement the selection region model over the emulator
//! grid, text extraction, and the OSC 52 / native copy paths.
