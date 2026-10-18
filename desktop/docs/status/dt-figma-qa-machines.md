# dt-figma-qa-machines: pixel QA of the Machines view

Status: done, 2026-10-18. Frames: Machines dark `2165:456` (main content
`2165:544`), light `2220:21211` (main content `2220:21213`). Values pulled
with the Figma MCP (`get_metadata`, `get_design_context`,
`get_variable_defs` on both main-content nodes), then checked against the
1x PNGs pixel by pixel.

Images (not committed):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-machines/`,
`before/` and `after/`, each with `full/` (the whole frame, `figma-diff.sh`
as is) and `main/` (the main area aligned, below): `*-side.png`,
`*-heat.png`, `*-overlay.png`, `report.txt`. `after/states/` holds the
renders of the states the frame doesn't draw, and
`branch-glyph-figma-vs-fallbacks.png`.

## Mismatch

`figma-diff.sh` at its defaults (FUZZ 10). "Main area" compares the frame's
main content (x 280 to 1440) with the render's (x 257 to 1417, after the
257pt Navigation-component sidebar), `CORNER=1`. Both start at their
content's left edge and the frame's content is 1183 wide, like the
render's, so the right-aligned branches and the footer count line up too.

| Screen | Theme | Whole frame before | after | Main area before | after |
|---|---|---|---|---|---|
| machines | dark | 3.89% | 3.74% | 0.76% | **0.55%** |
| machines | light | 2.73% | 2.66% | 0.78% | **0.69%** |

The whole-frame number is dominated by the sidebar (the 23pt narrower
Navigation component, `dt-figma-sidebar`'s), not this view. The residual in
the main area is glyph-edge antialiasing (gpui draws text a little heavier
than Figma; ink widths match, so the weights are the same) plus the known
font-version and fallback-glyph items below.

## Measured, after the fixes

Per text run, an ink bounding box (crop, trim at 15% fuzz) in both images,
and the furthest-from-background pixel as the run's color:

- **Position and size**: every run within 1pt of the frame (toolbar, machine
  headings, `local` / `ssh gpu.lan` / `● reachable`, tags and binary, every
  cell of every row, branches, footer hints and count). The selected row's
  band covers the same pixels (y 72 to 101, x 16 to the edge), radius 6.
  Padding, gaps and the row, heading and footer heights are the frame's.
- **Flat fills**: canvas, divider (`#2c3140` dark / `#f6f7fa` light over the
  canvas, 1 level of blend rounding on one channel) and the selection band
  (`#6d7388` vs `#6c7387`, `#dee1ec` both) are the Figma values.
- **Text colors**: exact in both themes for every run: `theme.white`
  names, `theme.green` working and reachable, `theme.cyan` review, `theme.muted`
  idle / location / binary / footer, `theme.workflow_badge_fg` tags,
  `theme.ansi_dark_gray` branches, and now `#7b85a7` runner and task.
- **Type**: Base Medium (toolbar title, machine names), Base Regular (row
  cells), X-Small Regular (everything else), as the frame's text styles.
  Ink widths match the frame to 1pt, so the weights are right.
- **Copy**: character for character, given the fixture fix below.

## Fixes

| File | What changed |
|---|---|
| `src/theme/palette.rs` | New role `table_text` (`GRAY_MACHINES`, `#7b85a7` opaque in both modes): the Machines frame sets `theme.gray` to `#7b85a7`, like the Activity and Settings frames, while the global `gray` is `#acb4cfe5` dark / `#7b85a7e5` light. The runner and task drew `#9ea5bf` dark (`#a5adc8` on the band) and `#8891b0` light against the frame's `#7b85a7`. Contrast checks: `table_text on canvas`, and on the selection band in place of `gray`; exceptions `table_text on` (dark and light) replace `dark: gray on selection`. `roles_are_the_figma_tokens` pins the value |
| `src/views/machines.rs` | The runner and task cells use `table_text` |
| `docs/theme.md` | The `table_text` role row and its two exception rows; `theme_cyan on selection` no longer claims to be the lowest pairing |
| `src/visual_figma.rs` | The Machines frame titles the two in-progress tasks "Add project switching" and "Session reconnect"; the Issues frame (same fixture) titles them "Add project switching to the desktop app" and "Handle reconnects after a daemon restart". The harness retitles them for the Machines renders only (`MACHINES_TITLES`, `retitle`) and restores them after |
| `src/visual_figma.rs`, `src/views/machines.rs` | The harness renders the undrawn states: `machines-unreachable` (the remote down, gpu-1 selected), `machines-idle-selected` (charlie), `machines-review-selected` (review). The reachability stub takes a value (`stub_reachability`) instead of always answering reachable |

## Undrawn states, by eye in both themes

- **Unreachable remote**: `● unreachable: ssh: connect to host gpu.lan port
  22: Operation timed out` in X-Small, in `red` (the Danger label,
  `#ff66ab` / `#b40041`), the role `theme.md` gives errors and failures. Same
  slot, size and gap as `● reachable`; the tags and binary line is
  unchanged. Consistent.
- **A selected row on each machine** (alpha on hub as drawn, charlie and
  review on hub, gpu-1 on gpu): the same band, radius and inset as the
  frame's selected row, every cell keeping its color. Consistent with the
  frame, but see the contrast flags.

## Departures and flags for John

1. **The selected dark row is very hard to read (flagged).** The frame
   draws the runner and task in `#7b85a7` on the `theme.selection` band
   (`#acb4cf8c`, a light gray over the dark canvas): 1.30:1. With the
   wrong (lighter) gray it had been 2.12. Also on that row: the branch
   (`theme.ansi_dark_gray`) 1.13, `review` cyan 1.60, `working` green 2.30,
   and an idle row's muted `idle` / `—` 1.11 (a state the frame doesn't
   draw). Kept as drawn and listed in `theme.md`'s exceptions; a darker
   band or loud text on it would fix this. Light is mild (2.80 to 4.1).
2. **The branch glyph `⎇` differs (font fallback).** Geist has no U+2387,
   so both sides fall back: Figma to a font of its own (a squat glyph, 8x5),
   the app to CoreText's (SF, 7x9). None of the installed fonts with the
   glyph (Apple Symbols, SF, STIX Two Math, Lucida Grande) draws Figma's
   shape; see `branch-glyph-figma-vs-fallbacks.png`. Same codepoint and
   position. Fixing it would mean an icon or a bundled symbol font in
   place of the shared `BRANCH_ICON` text, across Activity and Machines.
3. **gpu-1's runner reads `—`, the frame `Codex`** (carried over from
   `dt-figma-machines`): the frame is inconsistent (charlie, also idle,
   reads `—`), and an idle slot has no resolved runner.
4. **Text widths**: long runs come out up to 3pt shorter ("review: Desktop
   application" 175 vs 178), because Figma used John's installed Geist 1.2
   and the app bundles Geist 1.7.2 (`dt-figma-qa-activity` found this).
5. **The frame's main content is 1183 wide in a 1440 frame after a 280
   sidebar**, so Figma clips its right 23pt (the "5 workspaces" count and
   the right end of the first branch). The app lays the same 1183 out after
   the 257pt Navigation component, so nothing is clipped. Not a change.

## Verification

- `--visual-figma` before and after, both themes, `figma-diff.sh` whole
  frame and main-area aligned (table above). `--2x` fell back to 1x (no
  Retina display on this machine); close inspection used 4x point-scaled
  crops of the 1x renders.
- In `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D
  warnings` with and without `--features visual-check`, `cargo test` (451
  passed after the rebases onto `dt-figma-qa-palette` and `dt-figma-qa-settings`, including the palette
  contrast and theme-doc tests). The rebase conflicted only in
  `visual_figma.rs` (its module doc and the palette-states helper beside the
  Machines one; both kept); the renders and numbers are unchanged after it.
- No shared crate changed; the root `Cargo.lock` is untouched.
