# rt-restyle-the-nav-sidebar-to-match-the-figma-design — In review

Restyled the in-process TUI nav sidebar (`crates/shelbi-tui/src/shell/sidebar.rs`,
driven from `shell/mod.rs`) to match John's Figma "Navigation" design (node
`2072:735`).

## What landed

- **Header + search box.** The sidebar now opens with the project name in the
  accent cyan, then a filled (`color/search` #292929) search box carrying
  `🔍 Search` and a right-aligned palette-chord hint (the resolved
  `GlobalAction::OpenPalette` chord, e.g. `⌃P`, or `<unbound>`). Clicking the box
  opens the command palette via the existing `open_palette()` path
  (`SidebarView::search_box_hit` + a check in `handle_sidebar_mouse`).
- **Review rows.** The dim second line now shows `⎇ <branch>` truncated with `…`
  to fit; the title also ellipsis-truncates so nothing overlaps at the 24-col
  minimum. Badges/colors were already Figma-correct (✓ cyan serving, `·` muted
  queued) via `ReviewState::decoration`.
- **Footer.** Dropped the `^P palette  q quit` keybind line and the off-state
  `⌥Z Zen mode` hint (both reachable via the palette / their keys). Footer is now
  the dim `daemon X · cli Y` version line, a blank spacer, and the full-width
  green `ZEN MODE ON` band shown only while Zen is on. The unread-errors button
  is preserved (shows only when there are unread errors, so the clean state
  matches Figma exactly).
- Workspace rows already matched Figma (`⏵` green busy / `·` muted idle, with the
  right-aligned title-cased agent name or `idle`); no `WorkspaceBadge` change
  needed. The nav block keeps its half-row selection bleed, which the Figma
  component description confirms is intended.

Selection/keyboard nav, row click-routing (hit-testing re-derived from the new
geometry, including the two-line review rows), resizing, and the review-panel
swap are all unchanged.

## Tests

Render/buffer tests cover the header + search box (fill, label, chord,
`<unbound>`), busy vs idle workspace rows (green `⏵` / `idle`), ready vs queued
two-line review rows with `⎇` branch truncation, and the Zen band on/off (plus
version-line stability across the toggle). A shell test asserts a click on the
search box opens the palette. Existing sidebar-click tests updated for the
+2-row header offset. `cargo test -p shelbi-tui --lib` (510) and
`cargo clippy -p shelbi-tui --all-targets -- -D warnings` are green.

## Config-upgrade note

Rust-source only — no shipped `*.template` / default config / workflow /
instructions file changed, and no `Cargo.lock`/dependency change, so no
config-upgrade sniffer and no MSRV re-check are needed.
