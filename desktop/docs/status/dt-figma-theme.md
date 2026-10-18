# dt-figma-theme

The foundation of the Figma redesign: the desktop's palette, type scale and
terminal font now come from John's Figma file (`Shelbi`,
`pu3hxHgWoCDPCzJ4u6zTQK`) instead of Taskpage. Screen layouts are the later
`dt-figma-*` tasks' job; this one sets the roles, the type styles and the
shared shapes they build on. `docs/theme.md` describes the system.

## What changed

- **Palette** (`src/theme/palette.rs`): a `figma` module holds every Figma
  variable as `get_variable_defs` returned it, `(dark, light)` by its Figma
  name; `figma::measured` holds the colors Figma draws without a variable
  (sidebar tint, card, palette surface, Issue detail edge, scrim). Every role
  resolves to one of them. Roles were renamed to the Figma vocabulary where
  it is clearer: `surface` → `canvas`, `surface_recessed` → `sidebar`,
  `surface_raised` → `raised`, `surface_sunken` → `well`, `text` →
  `foreground`, `text_quiet` → `muted` (plus a new `gray`), `rule` →
  `divider`, the fill/control families → `background` / `background_hover` /
  `background_active`, `gold` / `purple` → `yellow` / `magenta`. New: `card`,
  `selection`, `search`, `accent` (`icon/select`), `icon_cyan/green/yellow/
  purple`, `focus_glow`, `theme_green`, `theme_cyan`, `dark_gray`, the
  Success and Danger button fills, `workflow_badge(_text)`, `shadow_ring`.
  Gone: the brand fill, control borders, `tab_selected(_ring)`, `wordmark`,
  `orange`, `pink`, the Taskpage hue scales.
- **Type** (`src/theme/style.rs`): `Type` names the nine Figma text styles
  (Heading, Base / Medium / Semibold, Small / Medium, X-Small / Medium /
  Semibold); `.type_style(Type::...)` sets Geist at the style's size, line
  height and weight. Every view's ad-hoc `text_xs` / `text_sm` / `text_base`
  / `text_lg` / `text_xl` (plus the weight beside it) became the matching
  style; the app root is `Type::Base`. The bundled Geist already had every
  weight needed (400, 500, 600, 700).
- **Terminal font**: Source Code Pro (Regular, Bold, It, BoldIt, OFL, Adobe's
  `release` branch at `803b7e23`) vendored in `assets/fonts/source-code-pro/`,
  registered like Geist Mono (gpui and CoreText), the default at 14pt with
  an 18/14 line-height floor. Geist Mono stays bundled as the first fallback.
  Settings reads "Blank uses Source Code Pro" with placeholders "Source Code
  Pro" and "14".
- **Shapes**: the nav chip and cards wear Figma's four-layer `shadow/raised
  card`; buttons, dropdowns, the search box and segmented controls are fill
  only on `theme.background` (no borders), 6px corners; focus is the 1px ring
  plus the 4px glow; the selected segment is `theme.selection`; keycaps are
  the Hotkey (canvas, gray label, `#acb4cf99` edge). `ButtonKind::Primary`
  is the Figma Success style (Figma has no solid brand button).
- **Views**: board cards and machine cards are `card`, board columns sit on
  the canvas; the Zen band is the Success fill with a `text/green` label; the
  unread-errors button is the Danger style; Settings' checked box is the loud
  color with the mark in the canvas; sidebar badges use the Navigation
  component's status icon colors; Activity's tags use `theme.ansi_*`.
- **Terminal colors**: ANSI green and cyan are `theme.green` / `theme.cyan`
  (the Orchestrator frame), yellow, blue, magenta and bright black
  `theme.ansi_*`; the default foreground is `theme.foreground` over the
  canvas.

## How the values were pulled

The Figma MCP was available: `get_variable_defs` on all five listed frames
plus Review light, Issue detail (both), Activity (both), Command palette
(both), Settings, and the Button component (`2220:17600`, for the hover /
active alphas and the Success / Danger styles); `get_design_context` on the
Navigation instance, the board toolbar and an issue card. Colors without a
variable were measured from the 1x PNGs (ImageMagick pixel and histogram
reads), and each was checked against a token composition where one fit
(the dark sidebar is `#acb4cf` at 10% over the canvas to the unit; a
test pins it).

## For John (decisions and flags)

- **Contrast exceptions.** Many Figma values fall short of AA where they are
  drawn; per the brief they are kept and listed in `docs/theme.md`
  ("Exceptions", 24 entries with worst ratios). The big ones: `theme.muted`
  (dark 3.67 on the canvas, 3.03 on the sidebar; light 3.44 on white),
  `theme.gray` in light (3.12 on white), `text/link` in light (3.88),
  `focus/default/ring` in light (2.53 on white), `theme.divider` as a text
  field's border (about 1.1 to 1.2), dark `theme.ansi_blue` and
  `ansi_dark_gray` off the canvas. The test fails on any new shortfall and on
  a stale entry.
- **Derived, not Figma.** Figma defines Button hover / active only in Light;
  Dark uses the same +10 / +20 point steps over its 18% rest. The Danger
  label has no Dark value; Dark uses `#ff66ab` (`focus/danger/glow`). Chip
  tints are each hue at `theme.background`'s opacity. `search_current` is
  `icon/yellow` at 40% (Figma draws no current match).
- **Per-frame overrides.** `theme.divider` is `#acb4cf33` on the Issues
  frame (now `divider_strong`) and `#acb4cf1a` elsewhere; the selected row in
  Activity is `theme.background` in dark but `theme.selection` in light, as
  drawn; the dark Zen pill measures 18% green over the canvas, light 12%.
- **Cards.** The brief guessed about `#10131a` for dark cards; the frame and
  `get_design_context` give `#020303`, which is what the palette uses.
- **Primary buttons** are the green Success style now (e.g. "New issue" on
  the board). The Figma board draws "New issue" in the Default style; the
  board task can switch that call site to `Secondary`.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo test`: clean, 421 tests pass. No `crates/` change, so the root
  workspace checks don't apply.
- `cargo run --features visual-check -- --visual-views`, `--visual-workspace`
  and `--visual-check`, both themes: 0 checks failed. Compared side by side
  with `issues-*.png`, `settings-*.png`, `activity-*.png` and
  `workspace-*.png`: the dark canvas `#1e2330`, the `#2c3140` sidebar, the
  near-black cards, cyan accent and focus ring (pink accent, `#31ade9` ring in
  light) all match; layouts differ, as expected before the screen tasks. The
  terminal measures 8.40 x 18.00 px cells at the default (Source Code Pro 14
  on 18), and the fallback showcase (CJK, emoji, Nerd icons) finds every
  glyph.
- Rebased onto the umbrella after `dt-figma-visual-harness` merged
  (`35e32bb2`): the build, both clippy runs (with and without
  `--features visual-check`) and the 421 tests are still clean; the harness
  needed no role fixes.
- `cargo run --features visual-check -- --visual-figma /tmp/vf &&
  scripts/figma-diff.sh /tmp/vf`: 0 checks failed, 20 frames. The table
  below sets the post-theme mismatch beside the harness's baseline
  (`figma-diff.sh` defaults: FUZZ 10, GROW 6, CORNER 28).

## Figma diff: post-theme vs. the harness baseline

| screen | dark baseline | dark now | light baseline | light now | largest regions now (dark) |
|---|---:|---:|---:|---:|---|
| orchestrator | 21.00% | 5.89% | 5.07% | 4.68% | 780x258+2+6, 1438x85+2+936, 689x72+265+131 |
| issues | 46.83% | 26.52% | 10.44% | 9.17% | 1189x955+251+67, 659x259+2+5, 251x134+10+526 |
| issue-detail | 29.05% | 14.70% | 22.93% | 11.10% | 1320x1024+2+0, 133x164+1307+67, 212x87+11+437 |
| activity | 24.41% | 10.27% | 8.11% | 8.41% | 1179x253+261+108, 877x258+2+6, 251x134+10+526 |
| review | 20.44% | 3.86% | 3.68% | 3.44% | 251x304+12+41, 565x102+279+113, 908x76+242+946 |
| workspace | 21.60% | 3.69% | 3.82% | 3.68% | 256x224+12+101, 574x85+265+221, 674x67+265+149 |
| workspace-idle | 19.56% | 1.65% | 1.69% | 1.69% | 606x47+268+74, 611x44+291+139, 217x111+14+106 |
| machines | 21.90% | 22.17% | 5.95% | 6.06% | 1173x195+267+45, 1163x96+267+241, 412x257+2+7 |
| settings | 22.47% | 6.90% | 5.69% | 4.91% | 592x220+283+576, 1141x103+283+369, 269x252+2+12 |
| command-palette | 8.81% | 15.43% | 80.46% | 14.76% | 855x560+265+168, 1198x82+242+936, 541x120+241+6 |

The full-frame dark boxes are gone: the canvas and sidebar now match the
mockups, so what remains is layout and content (the cards' missing bodies
and tags, the board's column wells, the sidebar's sections, the palette's
placement), which the screen tasks own. Machines stays near 22% in dark
because its table rows sit on the near-black card surface where the mockup
draws them on the canvas. The palette's light number fell because the
render now dims the frame behind it the way the mockup does; what is left in
both themes is the panel itself, which the app draws taller, wider and lower
than the mockup, with its own rows and no Projects column. Neither dark frame
has a scrim, so the dark rise is that panel being compared against a
canvas that now matches; the baseline renders weren't kept, so I couldn't
confirm that from pixels.
