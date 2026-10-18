# dt-figma-qa-sidebar: pixel QA of the sidebar

Pixel QA of `dt-figma-sidebar` against John's frames: the sidebar column
(x 0 to 280) of every frame that shows the default sidebar (Orchestrator
2160:1307 / 2220:20936, Issues 2165:94 / 2220:20975, Issue detail 2172:722 /
2236:24534, Activity 2165:364 / 2220:21092, Workspace idle 2174:851 /
2220:21583, Machines 2165:456 / 2220:21211, Settings 2165:545 / 2220:21264,
Command palette 2171:628 / 2236:24244), and the Navigation component
State=Default (2164:4), dark and light. Values came from the Figma MCP
(`get_design_context` on 2164:4 and the dark frame's instance 2164:89,
`get_variable_defs` on 2164:89 and the light instance 2220:20937), and were
measured in the renders with ink scripts (first and last inked row and
column per run, peak color per run). Images:
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-sidebar/`
(`before/`, `after/`: side-by-side, heatmap and overlay per screen;
`states/`: the undrawn states; `zoom/`: close-ups). Not committed.

## Method

`cargo run --features visual-check -- --visual-figma <dir>`, then:

- **Sidebar column.** The reference and the render are both cropped to
  x 0 to 280 and diffed with `figma-diff.sh` (`CORNER=28`, which masks the
  window's rounded left corners). These are the numbers that measure this task.
- **Whole frame**, `figma-diff.sh` as is, for the record. Most of it is the
  main areas, which are the other QA tasks' business.

`--2x` needs a Retina display, and none is attached (John's main display is
a 1x external), so it falls back to 1x. Close inspection used 3x to 10x
nearest-neighbor zooms of the 1x renders.

The Navigation component's sheet (`navigation-component.png`) is the light
variant, and its State=Default instance is the column of the light
Orchestrator frame, pixel for pixel. The Orchestrator rows cover it.

## Mismatch, before and after

Share of the column's pixels whose largest channel differs by more than 10%.
"Hotkeys" is the intermediate render after the hotkey fix alone.

| screen | dark before | dark hotkeys | dark after | light before | light hotkeys | light after |
|---|---:|---:|---:|---:|---:|---:|
| orchestrator (= component Default) | 1.98% | 1.92% | 1.29% | 2.00% | 1.94% | 1.70% |
| issues | 1.97% | 1.91% | 1.28% | 2.00% | 1.93% | 1.69% |
| issue-detail | 2.05% | 1.99% | 1.38% | 3.55% | 3.46% | 3.28% |
| activity | 2.11% | 2.05% | 1.48% | 2.18% | 2.11% | 1.89% |
| workspace-idle | 2.01% | 2.01% | 1.45% | 2.06% | 2.07% | 1.85% |
| machines | 2.10% | 2.05% | 1.45% | 2.16% | 2.09% | 1.85% |
| settings | 2.10% | 2.05% | 1.45% | 2.16% | 2.09% | 1.85% |
| command-palette | 1.29% | 1.22% | 0.78% | 1.68% | 1.55% | 1.47% |

Whole frame, before and after: orchestrator 3.18 to 2.81% dark, 3.17 to
2.94% light; issues 9.09 to 8.58% / 6.99 to 6.78%; issue-detail 7.85 to
7.30% / 7.34 to 7.09%; activity 2.40 to 2.18% / 2.49 to 2.35%;
workspace-idle 0.86 to 0.73% / 0.84 to 0.78%; machines 1.48 to 1.28% /
1.51 to 1.40%; settings 2.59 to 2.36% / 2.64 to 2.51%; command-palette 2.40
to 2.12% / 2.44 to 2.34%; review 3.49 to 3.24% / 3.10 to 2.96%; workspace
3.28 to 3.02% / 3.26 to 3.10%. Every frame improves because the font
smoothing change reaches all text.

## What the measurements show

- **Geometry was already exact.** Every ink band in the column (switcher,
  the three nav items, each section header, review title and branch line,
  workspace row, the gear, the Zen label) starts and ends on the same rows
  as the frame's, in both themes. Left edges match to the pixel; the
  titlebar's Search and Toggle sidebar icons (212 to 227, 247 to 264), the
  gear (248 to 263) and the Zen label (101 to 121) match in x and y.
- **Flat fills and text colors** sample to the composite of the token: the
  sidebar `#2c3140` / `#f9f9fc`, the dark chip `#1e2330`, the light chip
  `#ffffff`, the Zen pill `#294036`, the green `#31d924` / `#00753d`, the
  loud `#020303`, the icons' `theme.gray`. Translucent fills differ by one
  8-bit level in a channel at most (the count badge `#43495a` against
  Figma's `#434859`, the light Zen pill `#ebf8ea` against `#ebf7ea`):
  Figma truncates when it composites an alpha fill, and gpui rounds. The
  tokens are Figma's.
- **The residual is glyph antialiasing.** What is left in the heatmaps is
  the edges of text, mostly the review rows (the largest regions, at x 31,
  y 264 / 395 / 437, are those rows' titles and branches). Figma's
  rasterizer and CoreText draw the same Geist outlines with a different
  gamma, and Figma uses John's installed Geist 1.2, whose advances are a
  little wider than the bundled 1.7.2 (`dt-figma-qa-activity`). The longest
  title ("Fix sidebar collapse state") ends 1pt short; every other run ends
  on the frame's pixel.

## Fixes

| file | change |
|---|---|
| `src/elements/exact_text.rs` (new) | A one-line label laid out at its shaped width. gpui's text element rounds its width up to a whole point (`size.width.ceil()`), so the hotkey's two labels came out about 1.5pt wider than Figma's, and every right-aligned item beside a hotkey ("Developer" on a working workspace) sat 3pt left of the frame. The element also puts its baseline where the inherited font would, so a glyph from another font sits on Geist's baseline, as in Figma. |
| `src/theme/style.rs` `hotkey` | Each key is an `exact_text`, and the modifier glyphs (⌘ ⌃ ⌥ ⇧ ⇪, which Geist lacks) are drawn in the system font. CoreText's default cascade for Geist drew ⌘ from a wider, heavier font (Lucida Grande's shape). Figma's ⌘ is SF's: 11.03pt advance at 12pt, its ink matching the frame (zoom: `cmd-glyph-ref-render-sf-lucida-hiragino-symbols.png`). The ⌘ and the key now land on the frame's pixels in the nav and workspace rows (⌘ 236 to 244, the key 249 to 254, rows 532 to 540) and within 1pt in the review rows; "Developer" ends at 222 against 223 (gpui still rounds that label's own width). |
| `src/platform/font_smoothing.rs` (new), `src/main.rs` | **Font smoothing off for the app (a decision for John to confirm, below).** Dark-theme text drew a weight heavier than the frames (Regular like Medium, Medium like Semibold): gpui copies macOS font smoothing, which thickens glyph stems by an amount that grows with the text color's luminance (`glyph_dilation_for_color`). One switch, `SMOOTH_FONTS = false`, applied at launch before any text draws. It writes `AppleFontSmoothing = 0` to the app's own defaults domain only (its bundle id, or `shelbi-desktop` when run unbundled), never the global domain. Setting it to `true` removes the override. The column's dark mismatch drops from about 2.0% to 1.3%. Light improves too, because the gray and muted text is light enough to have been thickened one level. |
| `src/visual_figma.rs`, `src/app.rs` | The undrawn states render as `state-sidebar-{reviews-collapsed,all-collapsed,zen-off,hidden}-<theme>.png` over the orchestrator chat, through two new offscreen hooks (`offscreen_toggle_section`, `offscreen_toggle_sidebar`). |

## Undrawn states (checked by eye, both themes)

- **Sections collapsed** (Ready and Queued, then all three): the header keeps
  its 8pt top and 4pt bottom inside the Section's 8pt padding, the count
  badge stays, and the 12pt chevron turns to point right in the quiet glyph
  color. Collapsed sections stack 46pt apart (header 30pt plus the two
  paddings), and the rows below move up.
- **Zen off**: the same 42pt pill with the neutral `theme.background` fill
  and "Zen mode off" in `theme.foreground` (Small medium), centered on the
  same line as "Zen mode on".
- **Sidebar hidden**: the main area takes the column; its Toggle sidebar
  button (the same 20pt glyph in `theme.gray`) sits on the traffic lights'
  center line (icon ink y 19 to 32, the same as the sidebar's toggle), right
  of the lights, and the chat's content keeps the 52pt titlebar inset.

## Departures, flagged for John

1. **Font smoothing off (decision to confirm).** The orchestrator chose to
   match the frames (q-font-smoothing). Dark text now matches Figma.
   Terminal text is thinner than in apps that thicken (Zed), and the same as
   Ghostty's default. To revert, set `SMOOTH_FONTS` to `true` in
   `src/platform/font_smoothing.rs`.
2. **The mockups disagree with the component** in places, and the component
   wins (README): the Activity frame's nav reads "Activity ⌘1", the light
   Issue-detail frame draws Issues with the old view-columns icon in gray,
   and several frames hint ⌘1 on Orchestrator. The render draws Changelog
   ⌘L, the rectangle-stack in the accent and ⌘O. This is most of the light
   Issue-detail residual (3.28%).
3. **Workspace idle**: the frame's sidebar draws charlie busy (awaiting
   input, Developer ⌘6) with no row selected, while its main area shows
   charlie idle. The harness renders charlie idle and selected, as clicking
   an idle workspace does (`dt-figma-visual-harness`).
4. **Geist version**: Figma's text uses John's installed Geist 1.2
   (`dt-figma-qa-activity`), so long runs can end up to 1pt short here.
5. **Alpha compositing**: one 8-bit level on translucent fills (above).
   Figma's blending, not a token.
6. **Traffic lights** aren't drawn offscreen, so they show in every diff;
   their spacing (AppKit's 23pt against Figma's 20pt) is
   `dt-figma-sidebar`'s known difference.

## Verification

- `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings`
  (also with `--features visual-check`), `cargo test` (481 pass, with and
  without `visual-check`). No `Cargo.lock` change (the `NSUserDefaults`
  feature of the already-locked `objc2-foundation`).
- The renders: `--visual-figma` reports 0 failed checks in both themes.
