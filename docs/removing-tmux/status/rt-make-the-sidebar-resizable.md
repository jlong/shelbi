# rt-make-the-sidebar-resizable

**Status:** complete.

Brought back draggable sidebar resizing in the in-process TUI (the tmux build
had it; the ported shell drew a fixed-width sidebar).

- **Persistence** (`crates/shelbi-state/src/lib.rs`): added
  `SidebarPrefs::sidebar_width: Option<u16>` to the global
  `~/.shelbi/state.json` (next to the existing `collapsed_machines`), plus
  `set_sidebar_width`/`sidebar_width` helpers routed through
  `update_global_state` so other fields survive. Additive `#[serde(default,
  skip_serializing_if)]` — older binaries preserve it via the `extra`
  catch-all. Runtime state, not a shipped default template, so no
  config-upgrade sniffer is needed.
- **Clamp policy** (`crates/shelbi-app/src/nav.rs`): `SIDEBAR_MIN_COLS = 24`
  and `clamp_sidebar_width(desired, window_width)` = `desired.clamp(24,
  window/2)`, with the min collapsing to the max in a too-narrow window so the
  clamp is never inverted and the display shrinks without panicking.
- **Shell** (`crates/shelbi-tui/src/shell/mod.rs`): seed the saved width on
  `ShellState::new`; `draw` clamps the saved width to the live window for
  display only (narrow window shrinks on screen, saved value untouched);
  `handle_mouse` starts a drag on `Down(Left)` at the divider column (the
  sidebar's rightmost column), tracks it on `Drag(Left)`, and ends it on
  `Up(Left)`, persisting on release. The divider press is swallowed so it
  never selects a row or reaches the main pane.
- **Session reflow**: the main pane resize is driven by the existing
  `draw()` path — changing the width changes `main_rect`, which differs from
  `reported_main`, which calls `sessions.resize(...)`. No new resize wiring.

Tests: shelbi-state round-trip + field-preservation; shelbi-app clamp (in-range,
min, half-window, too-narrow, degenerate); shell press-drag-release updating
`sidebar_width` + persisting, divider click opening nothing, and saved width
restored on construction.

No `Cargo.lock` change (no new deps). `cargo build` + `cargo clippy --all-targets
-D warnings` green on shelbi-state/shelbi-app/shelbi-tui; all 116 shell tests
pass.
