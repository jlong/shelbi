//! Scrollback navigation.
//!
//! Shelbi's own scrollback exists for sessions on the **normal** screen. A
//! full-screen program (one on the alternate screen) has no scrollback to show;
//! that is the program's design, not something Shelbi overrides. Shift+wheel and
//! Shift+drag always drive this scrollback; plain wheel and drag drive it only
//! when the program has not asked for the mouse.
//!
//! TODO (`rt-term`): implement the scroll offset, clamping to retained history,
//! and the reset-to-bottom on new input.
