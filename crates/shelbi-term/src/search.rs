//! Search within scrollback.
//!
//! Search, like scrollback, is for sessions on the normal screen. It finds
//! matches in retained history and the visible screen and drives the scroll
//! offset to reveal them.
//!
//! TODO (`rt-term`): implement the match iterator over the emulator's lines and
//! the current-match navigation.
