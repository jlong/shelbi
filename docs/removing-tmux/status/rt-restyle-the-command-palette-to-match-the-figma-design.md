# rt-restyle-the-command-palette-to-match-the-figma-design

Status: ready for review.

Restyled the in-process command palette overlay to match John's Figma design
(node 2085:457): borderless filled panel (`#292929`, no box/title), a `❯` search
line with a green block cursor + dim placeholder, two-column command rows
(icon + bold label, dim description aligned to a fixed x, right-aligned shortcut,
`…` truncation), a full-width selected-row highlight bar across the commands
column, a Projects column (`●`/`○`/`+` status glyphs with dim `·` bullets,
separated by whitespace, no border), and a dim footer hint.

The shared `render`/`build_result_items`/`render_projects_column` in
`crates/shelbi-tui/src/overlay/palette.rs` are the only palette renderer left
(the tmux `__palette` popup is gone), so they were restyled directly. The
in-process `Palette` now carries the Projects column and its focus model:
`→` moves into the column, `←` back, Enter switches project (or opens Add
project), typing/Backspace pull focus back to the commands column.

Command descriptions were added in the registry (`shelbi-app/src/command.rs`);
nav emoji icons (💬/📋/⚡/🖥) are attached via `build_command_model`.

Keys preserved (no longer listed in the footer, per design): Tab → sidebar,
and the global Ctrl+H/Ctrl+L pane-focus moves (handled in the shell, untouched).
Esc and the palette chord (Ctrl+P) close, as before.

No `Cargo.lock` change, so no MSRV concern.

Rework (rebase onto `jlong/remove-tmux`): resolved two conflicts. In
`theme.rs`, #1568's sidebar `SEARCH_BG` and this change's palette constants are
both `#292929`; kept both (distinct names, same token). In `palette.rs`, #1564's
`footer` string and `footer_documents_selection_and_copy` test lost to the Figma
footer per the rework brief — the Figma footer stays exactly
`↑↓ navigate · → projects · Enter activate · Esc / Ctrl+P close`, and the stale
copy-footer test (old `Palette::new` signature, removed strings) was deleted;
drag-select/copy stay documented by #1564 in the install guide and its own
`terminal_view`/`pty_input` tests. `shelbi-tui` builds, clippy clean, 525 tests
pass.
