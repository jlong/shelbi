//! Custom gpui elements.
//!
//! The terminal element is the core of the app: it paints the cell grid
//! from `shelbi-term` (background runs, shaped monospace text, cursor,
//! selection, search highlights), driven by emulator damage, and sends
//! input through `shelbi-term`'s shared encoder. Started by
//! **`dt-spike-terminal`** (which also settles the gpui channel), completed
//! by **`dt-terminal-complete`** (scrollback, selection, search, clipboard,
//! IME, links, fonts, mouse forwarding, accessibility text), and tuned by
//! **`dt-terminal-perf-multiclient`**.
//!
//! Licensing: Zed's `terminal` / `terminal_view` crates are GPL-3.0 and
//! Shelbi is MIT, so their code can't be copied or depended on.
//! `gpui-terminal` (MIT/Apache-2.0) is the permissive reference.
//!
//! [`exact_text`] is a one-line label at its shaped width, unrounded, for
//! rows that must land on Figma's positions (the sidebar's hotkeys).
//! [`clamped_text`] wraps and clamps with the real shaper, its last line
//! filled to the ellipsis (the task panels' description).
//!
//! [`controls`] holds the Figma form components (segmented control,
//! labeled control, textbox, picker, checkbox) every form composes.
//!
//! [`spinner`] is the Figma Spinner: the progress ring a running step shows.

pub mod clamped_text;
pub mod controls;
pub mod exact_text;
pub mod spinner;
pub mod terminal;
