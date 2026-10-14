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
