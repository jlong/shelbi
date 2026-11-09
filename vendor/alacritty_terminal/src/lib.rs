//! Alacritty - The GPU Enhanced Terminal.

#![warn(rust_2018_idioms, future_incompatible)]
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
#![cfg_attr(clippy, deny(warnings))]

pub mod event;
// SHELBI FORK: gated behind the off-by-default `tty` feature. See VENDORING.md.
#[cfg(feature = "tty")]
pub mod event_loop;
pub mod grid;
pub mod index;
pub mod selection;
// SHELBI FORK: gated behind the off-by-default `tty` feature. See VENDORING.md.
#[cfg(feature = "tty")]
pub mod sync;
pub mod term;
pub mod thread;
// SHELBI FORK: gated behind the off-by-default `tty` feature. See VENDORING.md.
#[cfg(feature = "tty")]
pub mod tty;
pub mod vi_mode;

pub use crate::grid::Grid;
pub use crate::term::Term;
pub use vte;
