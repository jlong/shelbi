# dt-figma-issues-board

The Issues board restyled to John's Figma Issues frame (`Shelbi`,
`pu3hxHgWoCDPCzJ4u6zTQK`; dark `2165:94`, light `2220:20975`, main content
`2165:185` / `2220:20977`), in both themes. Values came from the Figma MCP
(`get_design_context` and `get_variable_defs` on both main-content nodes),
checked against the 1x reference PNGs pixel by pixel where the two disagreed.

## What changed

- **Toolbar** (`views/issues.rs` `render_header`): no board title. The two
  filters are the Figma Button at X-Small with a funnel (both filters; the
  workspace filter's user icon is gone): 24pt tall, 8pt across, the overlay
  fill, X-Small/Medium loud, no chevron. New issue at the right is the Small
  Default button, 28pt tall (`style::toolbar_button`), no longer the green
  Success button. 12pt left / 16pt right inside a 56pt bar, then the board's
  1pt rule (`divider_strong`, the Issues frame's override).
- **Shared style pieces** (`theme/style.rs`): `style::dropdown` is now the
  Figma X-Small button (Activity's filters, drawn the same way in its frame,
  pick this up too); `icon_slot` lays a 20pt Heroicon in Figma's narrower
  Icon slot (14pt / 12pt) so labels land where Figma puts them;
  `toolbar_button`; `status_icon` and `status_heading`.
- **Status glyphs** (`assets/icons/status/`, `assets::status_glyph`): Figma's
  `status-backlog` (dashed circle), `status-todo` (open), `status-2-of-4`
  (half), `status-3-of-4` (three quarters), `status-complete` (filled),
  downloaded from the frame and redrawn in `currentColor` on their 16pt grid,
  plus a struck-through circle for canceled (Figma draws none).
  `style::status_icon(category)` tints them: backlog and to do `icon`, in
  progress `accent` (cyan dark, pink light, as the frames draw it), review
  `icon_purple`, done `icon_green`, canceled `muted`. `style::status_heading`
  is glyph + caps label at X-Small/Semibold, loud on the current column; the
  `dt-figma-task-panels` task reuses it for the panels' "◐ IN PROGRESS" /
  "◕ REVIEW" heading. The asset guard (`only_the_icon_helper_draws_icons`)
  still holds: `svg()` stays in `assets.rs`.
- **Columns**: no well. 272pt wide, 12pt gutters, a 24pt heading (glyph,
  caps label, count in muted), 8pt to the cards, cards 8pt apart. The column
  body's scroll clip reaches 6pt past the column (negative margin, matching
  padding) so the selected card's glow isn't cut while cards keep the full
  272pt. The board has no bottom padding: a long column runs under the
  footer rule, as in Figma. The collapsed strip (not in Figma) lost its
  bordered well too: glyph, count and the caps label down the strip, a hover
  fill.
- **Cards**: `card` with `shadow/raised card`, 8pt corners, 12pt inside, 6pt
  between parts. Title Base/Medium loud, two lines; the title box overhangs
  6pt into the right padding (Figma's title is 254pt in a 248pt content box,
  which is why it keeps "desktop" on line one of "Write the theme docs for
  the desktop palette"). New: the excerpt, X-Small on Figma's 18pt leading in
  `foreground`, four lines. Meta row: the workflow badge (`workflow_badge` /
  `_text`, 4pt across, 2pt corners) then the branch in muted, no `⎇`.
  Dependencies keep their line. The `@workspace` line is gone (Figma has
  none; the detail and the workspace filter carry it). The selected card
  wears the focus ring and glow. The drag ghost is a card too.
- **Drop targets**: a legal column lights up with the overlay fill and a 1pt
  focus-ring-colored ring drawn as a shadow (no border, so nothing shifts);
  refused columns still dim; reorder targets keep their 4pt edge.
- **Footer**: a 1pt `divider` rule over a 28pt row, 16pt across, X-Small
  muted, the mockup's hints exactly (the `workflow` / `workspace` /
  `refresh` hints dropped; the keys still work). The status line stays at the
  right.
- **Excerpt data** (`crates/shelbi-app`): `IssueCard.summary`, built by
  `board::card_summary` (first prose paragraph of the body, headings, fences,
  comments and rules skipped, list/quote markers, emphasis, ticks and link
  targets dropped, capped at 280 chars). Parity row added (TUI: not shown).
- **Workflow badge on every card**: Figma draws `task` on every card of a
  one-workflow board; the shared `show_workflow_badges` rule (TUI) hides it
  there. The desktop now always draws it; the parity doc says so.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo test`: clean, 422 tests pass (asset guard, contrast, smoke, drag and
  drop, board state, scroll axes).
- Root: `cargo clippy -p shelbi-app -p shelbi-tui --all-targets -- -D
  warnings`, `cargo test -p shelbi-app` (incl. `parity_doc`), `cargo check
  --workspace --locked` (no `Cargo.lock` change).
- `board_scroll` tests' layout mirror updated to the new geometry (glow-room
  margins, no bottom padding); all nine scroll-axis tests pass.
- `--visual-views`: 0 checks failed (board, filter dropdown open, detail).
- `--visual-figma` + `scripts/figma-diff.sh`, before → after:

  | Frame | whole frame | main area, aligned |
  |---|---|---|
  | issues-dark | 26.52% → 14.82% | 3.96% → 1.86% |
  | issues-light | 9.17% → 9.01% | 3.19% → 1.68% |

  The whole-frame number is dominated by the sidebar, which is 256pt here
  against Figma's 280pt (the `dt-figma-sidebar` task): it shifts the whole
  board 24pt left. "Main area, aligned" crops the reference at x 280 and the
  render at x 256 (1136×1024) and counts pixels off by more than 10%. What
  remains is glyph anti-aliasing, the right-aligned New issue button (the
  main area is 24pt wider here), and the "Desktop application" excerpt (the
  fixture's body follows the detail, see `visual_figma.rs`). Card tops,
  heights and the column/heading rhythm match to the pixel (card 1 spans
  y 101 to 242 in both).

## Known differences

- **Excerpt width**: the dark frame fits "Reconnect to running sessions
  after the hub" (247pt before kerning) on one line, the light frame wraps it
  at "the". The code export says 244pt; the dark PNG needs the full 248pt
  content width. The excerpt fills the content width, so the dark frame's
  wraps match exactly and two light-frame excerpts wrap one word later than
  Figma's.
- **Filter icon**: Figma's funnel is 18pt; the app's icons are all 20pt
  Heroicons (the `dt-icons-mini-outline` rule), laid in Figma's 14pt slot so
  the labels still line up.
- **Text rasterization**: gpui draws Geist a touch heavier than Figma's
  export at 1x; widths and wraps match (checked against the fonts' advance
  widths).
