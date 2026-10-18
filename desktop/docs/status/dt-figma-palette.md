# dt-figma-palette

The command palette now matches the Figma Command palette frame (dark
2171:628, light 2236:24244; overlay 2171:752 / 2236:24283), in both themes.
Values came from the Figma MCP (`get_design_context` on 2171:752,
`get_variable_defs` on both overlays), then were checked against the 1x
reference PNGs with ImageMagick.

## What changed

- **Geometry** (`src/views/palette.rs`): 788pt wide (a 580pt commands column,
  the 160pt Projects column, 16pt between them and on either side), 200pt
  down (`palette_top` now caps the 20% rule at the frame's 200pt), centered
  over the scrim. `raised` surface, 8pt corners, no ring (the frame has
  none), and the frame's `0 10 15 -3` drop in black at 25% (new role
  `palette_shadow`). On the frame's content the panel is 368pt tall, as
  drawn.
- **Query line**: 44pt, 16pt across, a `❯` prompt in `foreground`, the caret
  a 2x18pt bar in `text/cyan` (`link`), the placeholder in `muted`. No box,
  no magnifying glass, no rule under it.
- **Rows**: 32pt, 6pt across and between, 6pt corners. A 20pt icon in the
  icon color, the name in a 220pt column (Base Medium `foreground`), the
  description in Base `muted`. The selected row is the `background` fill
  (`#acb4cf2e` / `#acb4cf1f` over the panel, measured `#434859` /
  `#f0f1f6` in the PNGs), its name Base Semibold `text_loud`, its
  description `foreground`. The brand selection pill and the shortcut
  keycaps are gone (the frame shows neither). Hover still fades the fill in.
- **Projects column**: a "Projects" label in `muted` (8pt across, 4pt under),
  26pt rows (8pt across, 4pt gap): a green dot (`theme.green`) for an open
  project, a hollow one in `theme.ansi_dark_gray` for a closed one, the name
  in `foreground`; then "+ Add project". The current project's dot still
  breathes. The keyboard's row takes the `background` fill and loud text.
- **Footer**: X-Small `muted`, 8pt over, 12pt under, no rule:
  "↑↓ navigate · Enter activate · Tab projects · Esc / ⌃P close".
- **Default list and copy** (`palette::present`, applied to the registry's
  entries in `ShelbiWindow::palette_entries`): on an empty query the palette
  lists the frame's commands in its order and words: Orchestrator, Issues,
  Change log (the Activity view), Machines, Turn Zen Mode on/off (follows
  the state), Error log, Detach (spelled right; the frame reads "Detatch"),
  Close project (Quit Project), Quit Shelbi, each with the frame's
  description. Every other entry (the review rows, workspaces, Detach Other
  Clients, Switch Project, New Window, the editors, approve/reject) is
  hidden until a query matches it. The copy is desktop-only: the TUI and the
  menu bar keep the registry's titles.
- **Icons**: the frame's drawings. `chat-bubble-oval-left-ellipsis` and
  `rectangle-stack` are new vendored Heroicons (v2.2.0); the Zen toggle,
  the error log's page and the finish flag have no Heroicon, so they are
  vendored from the Figma file under `assets/icons/figma/` (20pt grid,
  `currentColor`), with a `toggle-on` (knob right) for "Turn Zen Mode off",
  which the frame doesn't draw. Detach takes `server-stack`, as the frame
  draws it. Entries off the frame get a fitting Heroicon; a workspace's or a
  review's icon keeps its sidebar status tint.
- **Keys**: Tab now crosses between the commands and the Projects column
  (John confirmed; the task body's "as today" was Right arrow). This is a
  desktop-only flag on the shared machine
  (`Palette::tab_switches_columns`); the TUI's Tab still focuses the sidebar,
  and a palette with no Projects column still hands Tab to the sidebar.
  Right / Left, ↑↓, Enter, Esc, ⌃P / ⌃Space / ⌘K, typing and clicks are
  unchanged.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo test`: clean, 424 tests pass. New tests: the blank-query list and
  copy, off-frame entries surfacing on a query, icon mapping, the panel and
  column widths, Tab into the Projects column and back (unit and through
  `ActiveOverlay`). The smoke test now types each command's palette label.
- Root: `cargo clippy -p shelbi-app --all-targets -- -D warnings`, `cargo
  test -p shelbi-app`, `cargo check --workspace --locked`: clean.
- `--visual-figma` in both themes (the harness now fills the Projects column
  with the frame's three projects), then `scripts/figma-diff.sh`:

  | screen | before (dt-figma-theme) | after |
  |---|---|---|
  | command-palette-dark | 15.43% | 3.86% |
  | command-palette-light | 14.76% | 3.25% |

  The remaining regions are outside the palette (the composer at the foot,
  the Codex card at the top, the sidebar). Inside the panel the side-by-side
  crops match row for row; the one copy difference is "Turn Zen Mode off",
  since the harness runs with Zen on (the frame's sidebar shows Zen on while
  its palette says "on", so the mockup disagrees with itself there).
