# rt-review-workspace-sidebar-polish — done

Polished the shared review/workspace sidebar panel (`crates/shelbi-tui/src/panel.rs`)
to match the Figma and dropped every opaque chrome background.

- **Icon alignment**: the nav block now renders inset one column (like the main
  sidebar), so each switch icon starts at col 2 — lined up with the back button,
  worktree 📁, and ✅ Approve.
- **Inset selection**: the selected switch fill + its half-block bleed run col 1
  to width−1, with transparent gutters, matching the main nav after #1584.
- **Focusable More**: `More` is now a keyboard-navigable row (Back → More →
  Folder → switches → Approve/Reject in review; Back → More → Folder → switches
  in workspace), shows the selection-fill focus state when focused, and opens the
  task popover on Enter/Space. The `m` shortcut still works.
- **Transparent backgrounds**: removed the sidebar's `#000000` full-area fill and
  the divider's `#000000` bg; both now use `Color::Reset`. The panels already
  painted no background. Component fills (search box, selection blocks, back
  button, zen band, error button, overlays) are unchanged.

Render tests added/updated for all four: icon col-2 alignment, selection inset,
More focus traversal + focus state, and default-bg (`Color::Reset`) cells in the
main sidebar, review panel, workspace panel, and main area.

`cargo build --workspace`, `cargo clippy -p shelbi-tui --all-targets -D warnings`,
and `cargo test -p shelbi-tui` (575 tests) all pass. No Cargo.lock change.
