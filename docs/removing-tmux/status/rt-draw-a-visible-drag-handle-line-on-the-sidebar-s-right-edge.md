# rt-draw-a-visible-drag-handle-line-on-the-sidebar-s-right-edge

Done — a full-height `│` drag-handle line now paints down the sidebar's
rightmost column (the exact column `divider_col()` starts a drag from), dim at
rest and cyan/bold while hovered or dragging. Sidebar and review-panel content
render one column narrower so nothing overwrites or clips the line. Hover is
driven by `MouseEventKind::Moved` (any-motion tracking is already on via
`EnableMouseCapture`) and only repaints when the hover state flips.

Changed: `crates/shelbi-tui/src/theme.rs` (line glyph + dim/accent colors),
`crates/shelbi-tui/src/shell/mod.rs` (`divider_hover` state, hover tracking,
`sidebar_content_rect`, `render_divider`, narrowed content render + hit-test).
No shipped template/config touched, so no config-upgrade sniffer required;
`Cargo.lock` unchanged.
