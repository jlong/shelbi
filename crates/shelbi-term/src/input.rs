//! Input encoding: keys, mouse, paste, and focus into PTY bytes.
//!
//! This turns a UI's input events into the raw bytes an
//! [`Input`](shelbi_proto::Input) frame carries, or the text a `paste` frame
//! carries. The plan calls for termwiz's key encoder (MIT) for key encoding,
//! plus mouse-coordinate translation to the pane, bracketed paste, and focus
//! events. The kitty keyboard protocol is in play because Claude Code's
//! Shift+Enter depends on it.
//!
//! **No dependency is added here yet.** termwiz is an input-encoding dependency,
//! not a terminal emulator, but to keep the foundation minimal it is left as a
//! documented choice for `rt-term`, which owns the full input model. Adding it
//! there keeps this crate's dependency set empty of anything a spike might
//! revisit.
//!
//! The agent keeps its mouse: when the program has turned on mouse reporting,
//! wheel, click, and drag are forwarded to it with coordinates translated to the
//! pane. Shift+wheel and Shift+drag are always Shelbi's own (scrollback and
//! selection).
//!
//! TODO (`rt-term`): add the termwiz key encoder, implement key/mouse/paste/
//! focus encoding, and the mouse-ownership split described above.
