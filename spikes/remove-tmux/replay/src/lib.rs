//! Phase 0 spike: full-state attach-replay across two terminal emulators, and
//! the empirical half of the emulator-crate decision.
//!
//! The question (plan, "Attach replay"): can we take a session's emulator,
//! serialize its full state, and rebuild an identical emulator in a freshly
//! attaching client, including the screen *underneath* a full-screen program?
//!
//! This crate drives two independent [`alacritty_terminal`] emulators:
//!
//! * `A` — the "session" emulator, fed a real (or crafted) output stream.
//! * `B` — the "client" emulator, built only from a replay byte stream that a
//!   serializer regenerates from `A`'s observable state.
//!
//! If `A` and `B` render identically after replay, the state survived the
//! round trip. The interesting case is the alternate screen: replay must carry
//! the *inactive* grid so that quitting the full-screen program reveals the
//! right screen. That grid is private in upstream `alacritty_terminal`; the
//! vendored fork adds a read-only `Term::inactive_grid()` accessor, which this
//! spike exercises.
//!
//! Modules:
//! * [`emu`] — alacritty wrapper + a toolkit-neutral [`Snapshot`] for diffing.
//! * [`serialize`] — regenerate a replay byte stream from an emulator.
//! * [`vt`] — the vt100 comparison candidate.
//! * [`boundary`] — split the live stream only where the parser is at rest.

pub mod boundary;
pub mod emu;
pub mod serialize;
pub mod vt;

pub use emu::{Cursor, Emu, Snapshot};
