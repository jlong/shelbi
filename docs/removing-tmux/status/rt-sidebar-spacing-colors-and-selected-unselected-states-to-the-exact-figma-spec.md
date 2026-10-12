# rt-sidebar-spacing-colors-and-selected-unselected-states-to-the-exact-figma-spec

Done — sidebar restyled to the exact Figma spec in `crates/shelbi-tui`.

- Added exact Figma tokens to `theme.rs` (`BACKGROUND` #000000, `TEXT` #bababa,
  `TEXT_SELECTED` #ffffff, `MUTED` #7a7a7a, `ACCENT` #00a6b2, `BUSY_GREEN`
  #5acd25); set `SEARCH_BG` to the real #292929 (the painted black background
  makes it visible now); divider glyph `│`→`▏` and its resting colour to
  #414141.
- Sidebar now paints #000000 across its whole area; the half-block bleed rows
  blend against it. Project title drops to row 1 (blank row above). Nav
  selection fill + bleed are inset one column each side (matching the search
  box), confirmed against the Figma "Chat selected" node — not edge-to-edge.
- Colour pass: nav labels (unselected #bababa / selected white bold), search
  label + shortcut both #bababa, section headers / bullets / agent labels /
  branch / version muted, busy `⏵` green, ready `✓` bold cyan, project name
  bold cyan. Workspace/review selected rows keep their text colour; only the
  inset #3f3f3f fill marks them.
- Tests: rewrote the nav-selection render test for the inset bleed, updated the
  header/search-box tests for the new rows + token colours, added a
  selected-workspace-fill test and background/divider assertions, fixed the
  mod.rs search-box click row. `cargo test -p shelbi-tui` (530) + clippy clean.
  No `Cargo.lock` change; no shipped template touched → no config-upgrade
  sniffer needed.

Base contained the umbrella foundation (`crates/shelbi-proto`,
`docs/removing-tmux/README.md`).
