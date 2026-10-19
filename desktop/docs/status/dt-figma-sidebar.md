# dt-figma-sidebar

The desktop sidebar restyled to the Figma **Navigation** component,
State=Default (node `2164:4` on the sheet `2220:17016`, fileKey
`pu3hxHgWoCDPCzJ4u6zTQK`), in both themes. Values came from the Figma MCP
(`get_design_context` on `2164:4` and the awaiting-input glyph
`I2195:5760;2220:16331`, `get_variable_defs` on the dark and light frame
instances `2164:89` and `2220:20937`), with the fills Figma draws without a
variable measured from the 1x PNGs.

## What changed

- **Layout** (`views/sidebar.rs` `layout`): the shared rows
  (`SidebarModel::rows`, unchanged, so the TUI keeps its order) regrouped as
  the component draws them: the nav, then Ready for review, Queued for review,
  Workspaces. Each section header carries a count badge (the review sections;
  the component draws none on Workspaces) and a chevron; a click collapses or
  expands it, and the collapsed set is saved per project in
  `desktop-view.json` (`ProjectViewState::collapsed_sections`). The window
  selects, activates and restores over this list, so the keyboard walks what
  is drawn; collapsing keeps the selection on its row when that row is still
  drawn.
- **Titlebar** (52pt, every Figma frame's): the native traffic lights now
  center at (26, 26) (`traffic_light_position` (19, 19) for macOS 26's 14pt
  buttons, measured with a scratch `NSWindow`), with Search (opens the
  palette) and Toggle sidebar (32pt, 8pt corners) right-aligned and centered
  on them. The review and workspace panels inherit the same 52pt inset
  (`TRAFFIC_LIGHT_INSET = TITLEBAR_H`). The collapsed sidebar's toggle (in the
  main area) is the same sidebar glyph, centered on the lights beside them.
- **Project switcher**: the name (Base medium) and the up-down chevron on a
  28pt trigger, no initials badge, clear until hovered; the popover menu is
  unchanged (6pt below the trigger).
- **Nav**: Orchestrator ⌘O, Issues ⌘I, Changelog ⌘L. 34pt items, 6pt in,
  `chat-bubble-oval-left-ellipsis` / `rectangle-stack` / `bolt` Heroicons,
  the selected icon in `accent` (cyan dark, pink light). "Changelog" is the
  desktop's label for the Activity view (`nav_label`, also used by the Go
  menu); the shared model and the TUI still say "Activity". Machines stays
  out of the sidebar (palette, Go menu).
- **Chords** (`keys.rs` `nav_chord`): ⌘O / ⌘I / ⌘L open the views; ⌘1 to ⌘9
  open the rows the sidebar numbers (reviews, then working workspaces, in
  order; idle workspaces take none; numbered whether or not the section is
  collapsed). The window takes them ahead of a focused terminal, and the Go
  menu shows ⌘O / ⌘I / ⌘L on its items. Existing chords are unchanged.
- **Hints**: plain text (`style::hotkey`, X-Small `gray`, 2pt between ⌘ and
  the key), not keycaps.
- **Review rows**: the 12pt state glyph (cyan check serving, quiet dot
  queued, cyan play loading), the title X-Small medium, the second line
  X-Small muted `#401  branch`, ⌘N right. The number is the tracker's: a new
  `ReviewRow::number`, filled in `shelbi-orchestrator`'s `sidebar_model` from
  the board index (`shelbi_state::read_board_index_numbers`, remote backends
  only, read only when there are reviews). The serving location left the row
  (its right side is the hint); the Review view still shows it.
- **Workspace rows**: green play (working), the yellow `?` disc over a dark
  disc (awaiting input), a quiet dot (idle); the name Small medium loud when
  busy, Small foreground when idle; then the agent (X-Small semibold) and ⌘N,
  or "Idle" muted. States the component doesn't draw (permission, paused,
  loading) keep the shared glyph as text.
- **Footer**: the Settings gear (32pt) bottom right, the unread-errors button
  at the left when there are errors, and the Zen pill: 42pt (the component's
  height; the task said 40), inset 8pt, fully rounded, the Success tint over
  the canvas with a green "Zen mode on" (Figma's measured `#294036` /
  `#ebf7ea`), or the neutral fill with "Zen mode off"; a click toggles Zen.
  The status notes (the orchestrator's failure, the polling indicator, a
  version mismatch, a stale board) stack above the buttons only when there
  is one; the healthy version line moved into the Settings tooltip.
- **Glyphs**: the component's drawings with no Heroicons outline twin come
  from Figma. The 20pt sidebar toggle joins `assets/icons/figma/` (the
  palette task's set, drawn by `heroicon`); the 12pt state glyphs and the
  12pt chevron are `assets::glyphs` (`assets/icons/glyphs/`), drawn at
  Figma's 12pt by `assets::glyph`. The "no `svg()` outside `assets.rs`"
  guard covers both. Unused `view-columns`,
  `chevron-double-left` and `bars-3` were removed.
- **Theme**: the dark selected chip is flat (the component), Light keeps
  `shadow/raised card`; the sidebar's corners are 6pt (`NAV_RADIUS`); the
  Zen pill's contrast pairing is checked over the canvas it paints. The
  sidebar is 280pt at the default 28 columns (`COLUMN_PX` 9 → 10).

## Verification

- `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, each also with `--features visual-check` (481 pass after
  rebasing onto the board, font zoom, terminal screens, Settings and
  Machines restyles and the palette, Activity, terminal screens, Machines and Settings QA). New tests: `views/sidebar.rs` (layout order,
  numbering, the nine-row cap, counts and collapse, selection keys, labels,
  glyphs, geometry: 12pt insets, 34pt nav items 4pt apart, 32pt buttons 8pt
  from the edge, 28pt trigger, 42pt review rows, 26pt workspace rows, the
  42pt pill 8pt in; traffic lights at (19, 19)), `app_sidebar_tests.rs` (the
  chords, also from a collapsed section and ahead of a terminal; collapse
  persisting and restoring; the selection kept across a collapse; the Zen
  pill; Search and Toggle sidebar, and the main-area toggle bringing the
  column back; the Go menu's Changelog and chords), `keys.rs`.
- Root: `cargo clippy` on `shelbi-app`, `shelbi-state`,
  `shelbi-orchestrator`, `shelbi-tui` (`--all-targets -D warnings`),
  `cargo test -p shelbi-app` (the parity doc lists `ReviewRow::number`),
  `cargo check --workspace --locked` (no lockfile change).
- **Pixels**: `cargo run --features visual-check -- --visual-figma`, then
  the sidebar column (x < 280) cropped from the Orchestrator (dark) and
  Issues (light) renders side by side with the references: every row, header,
  badge, hint, glyph, the gear and the pill land on the reference's
  positions in both themes. After the rebase onto the board, terminal
  screens, Settings and Machines restyles, 0.32% (Orchestrator dark),
  0.28% (Issues light), 0.32% (Issues dark) and 0.34% (idle Workspace dark)
  of the column's pixels differ by more than 10% (`magick compare -metric
  AE -fuzz 10%` on the 280pt crop), the heavier text (below). Full-frame
  `figma-diff.sh`: orchestrator 3.18% dark, 3.17% light (5.89% and 4.68%
  before this change); the regions it lists inside the column on Issues and
  idle Workspace are the review rows' text weight, every other region is in
  a main area.
- The Review and Workspace screens with the panel in the column: the panel
  content now starts below the 52pt titlebar, clear of the moved lights (the
  panels' own restyle is their tasks').

## Known differences

- **Text rasterizes heavier than Figma's** across the app (gpui/CoreText at
  1x on these offscreen captures: Medium reads close to Semibold). Not a
  sidebar setting: the weights are the component's (`Type` styles); worth a
  look in the theme or with a Retina capture.
- The traffic lights are 23pt apart (AppKit's spacing; gpui keeps it), Figma
  draws them 20pt apart; only the close button's center is placed. Offscreen
  renders draw no traffic lights at all.
- In full screen AppKit hides the lights and gpui restores their frames; the
  titlebar row keeps its 52pt with the buttons at the right.
