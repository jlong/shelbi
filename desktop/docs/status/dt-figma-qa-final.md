# dt-figma-qa-final

The last pixel QA of the desktop redesign against John's Figma file
(`pu3hxHgWoCDPCzJ4u6zTQK`), after every `dt-figma-*` implementation and QA
task merged into `jlong/desktop-app`: all 20 frames (10 screens x dark and
light) at 1x and 2x, the components shared across screens, and the bundled
fonts. Values came from the Figma MCP (`get_metadata` on the frames,
`get_design_context` on the idle toolbar `2174:996`); measurements from
`--visual-figma` + `scripts/figma-diff.sh`, plus a line-geometry check (every
text line's first ink column and its top and bottom rows, frame against
render, in the sidebar column and the main area).

**The real-window walk is PENDING.** The Mac's screen was locked for the
whole task (a locked session draws no frames and refuses captures), so the
walk of the bundled app in a real window could not run. It is now one
command, `scripts/figma-window-walk.sh` (below); run it once the screen is
unlocked. What could be checked in the real app without pixels was (font
zoom, below).

Images (not committed):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-final/`:
`r1/` (the umbrella as this task found it), `r3/` (final, 1x), `r2x/` (final,
native 2880x2048) and `r2x-1x/` (those downsampled), each 1x set with
`diff/` (side-by-side, heatmap, overlay, `report.txt`); `lines-r3.txt` (the
line check, every frame); a few zoomed crops.

## The 20 frames

`figma-diff.sh` defaults: the share of pixels off by more than 10% in any
channel, whole 1440x1024 frame, corners masked. "Start" is the umbrella at
`d1779462`; "final" is this branch.

| frame | start | final, 1x | final, 2x | line geometry (sidebar / main), final 1x |
|---|---:|---:|---:|---|
| orchestrator-dark | 2.66% | 2.40% | 2.87% | exact / the off-grid transcript (departure 9) |
| orchestrator-light | 2.86% | 2.60% | 3.18% | exact / the same |
| issues-dark | 6.19% | 4.10% | 5.15% | exact / exact |
| issues-light | 5.46% | 5.50% | 6.15% | exact / exact |
| issue-detail-dark | 4.25% | 4.11% | 4.29% | exact / within 1 |
| issue-detail-light | 5.12% | 5.10% | 5.26% | within 1 / within 1 |
| activity-dark | 1.79% | 1.85% | 1.99% | exact / within 1, but the daemon row (departure 12) |
| activity-light | 2.04% | 2.09% | 2.33% | exact / within 1, a detail line 2 (departure 1) |
| review-dark | 0.94% | 0.94% | 1.25% | the panel's mockup labels (departure 6) / the composer rows (departure 9) |
| review-light | 1.08% | 1.08% | 1.48% | the same |
| workspace-dark | 1.87% | 0.80% | 0.99% | the panel's mockup labels / within 1 |
| workspace-light | 2.02% | 0.97% | 1.24% | the same / within 1 |
| workspace-idle-dark | 0.60% | 0.56% | 0.68% | charlie's row (departure 7) / within 1 |
| workspace-idle-light | 0.72% | 0.69% | 0.86% | the same / within 1 |
| machines-dark | 0.93% | 0.73% | 0.87% | exact / within 1 |
| machines-light | 1.16% | 0.97% | 1.18% | exact / within 1 |
| settings-dark | 1.68% | 1.62% | 1.81% | exact / within 1, but the size field (departure 13) |
| settings-light | 1.95% | 1.87% | 2.17% | exact / the same |
| command-palette-dark | 2.11% | 2.08% | 2.37% | exact / the palette panel within 1, but the Zen row's "off" (departure 15) and the placeholder's Geist drift; the transcript behind the scrim as Orchestrator's |
| command-palette-light | 2.34% | 2.30% | 2.06% | exact / the panel exact |

"Exact" and "within 1" are the worst first-ink and band-edge deltas in points
over every text line of the region. Every remaining mismatch is one of: glyph
antialiasing (Figma's rasterizer against CoreText's; more of it in Light,
where dark-on-white edges cross the 10% fuzz more often), the Geist 1.2
against 1.7.2 advance drift along a line (departure 1: the ink starts on the
frame's pixel and ends 1 to 4pt short), or a departure listed below. The
Issues and Issue detail frames are the highest because they hold the most
text (40 card lines and the detail body); their lines all start on the
frame's pixel.

**2x.** No display here is Retina (the built-in panel mirrors a 1x monitor),
so the 2x renders ran on a temporary HiDPI virtual display (see the note at
the end). `--visual-figma --2x` then produced native 2880x2048 frames, 0
checks failed. Figma's MCP caps screenshots at a frame's own size, so there
are no 2x references: the 2x frames were diffed downsampled (the numbers
above, 0.1 to 0.7 points over 1x, the cost of filtering a 2x raster down) and
inspected at full size: text, icons, the 1pt rules, the chip's shadow and the
cards' rings all land on device pixels; no blur or half-pixel seams. The line
check on the downsampled 2x frames gives the same results as 1x.

## Fixes in this pass

- **The Machines QA was gone.** The squash merge of `dt-figma-qa-activity`
  (#1778) carried the exact reverse of `dt-figma-qa-machines` (#1783): the
  `table_text` role, the Machines cells, the harness's Machines states, its
  `theme.md` rows and its status doc. Re-applied on top (keeping the Activity
  QA's `log_text`, `button_icon` and `success_label` rows). A scan of every
  `desktop/` commit since the harness for any later commit undoing an earlier
  one found only this pair.
- **The main area starts at x 280, except on Review** (`app.rs`). Only the
  Review frames draw a 1pt column rule (Figma: `Divider` at 280, the reviewer
  at 281); every other frame puts the main content at 280 with no rule (its
  only edge is the sidebar fill ending). `dt-figma-qa-task-panels` had drawn
  the rule on every screen, which put nine screens' main areas 1pt right
  (the whole Issues board's text showed in the heatmap). Now the rule is laid
  out only while the Review panel holds the column; elsewhere the column's
  edge keeps its 5pt grab strip over the column's last points, and a drag
  lights the edge. Issues dark 6.19% to 4.10%, Workspace 1.87% to 0.80%.
- **Enter reads `↵`** (`keys::chord_text`). The shared Mac spelling is `⏎`
  (U+23CE), which Geist lacks, so CoreText drew it from a heavier symbol face
  about 8pt wider and pushed the rest of the Issues footer right. Geist has
  `↵` (the frame's glyph). Every key hint the window draws (the Issues and
  Activity footers, the palette's chord, the sidebar's search box, the nav
  hotkeys) goes through it; native menus keep `format_chord`.
- **Shadows draw at Figma's softness** (`style::figma_blur`). Figma's blur is
  twice the Gaussian's sigma and gpui's `blur_radius` is the sigma, so the
  shared `shadow/raised card`, popover and dialog drops came out twice as
  soft (the light cards' falloff ran 1 to 2px long); the palette's own drop
  already halved. One helper for all of them; `theme.md` says so.
- **The idle page's Shell button** takes the frame's 14pt `command-line`
  (a new `heroicons-14` drawing, `COMMAND_LINE_SMALL`) and its 8 / 8 padding
  and gap, the follow-up `dt-figma-qa-terminal-screens` left for the 14pt
  icon support that `dt-figma-qa-activity` landed.
- **The docs index** (`docs/README.md`): a row for every `dt-figma-*` status
  doc that lacked one, and this one.

## Shared components across screens

- **The sidebar column** is pixel-identical (`magick compare`, 0 differing
  pixels outside each screen's selected row and the palette's scrim) across
  Orchestrator, Issues, Issue detail, Activity, Machines, Settings and the
  idle workspace.
- **Rules**: one `divider` token everywhere; it samples `#2c3240` dark and
  `#f7f7fa` light on every screen against Figma's `#2c3140` / `#f6f7fa` (one
  8-bit level: Figma floors alpha compositing, gpui rounds).
- **One helper each** for the status glyph and heading (`style::status_icon`,
  `status_heading`, `panel_status_icon`), the nav row and chip, hotkey text
  (`style::hotkey` with `exact_text`), segments (`segment_chip`: Activity's
  kinds and Settings' choices), buttons (`button_base` / `button`), the keycap
  (one user, the detail), key hints (`keys::chord_text`, new) and shadows
  (`figma_blur`, new). Where two screens draw the same component differently
  the Figma file does too: the panels' In progress glyph is `icon/cyan`, the
  board's the accent; the Zen pill is 28pt in the Default state and 32pt
  under a panel; the board's filter is the 18pt Button, Activity's pickers
  the 14pt one; the footer bar is 28pt on Issues and 26pt on Activity and
  Machines (departure 4).

## Fonts and the bundle

- `scripts/bundle.sh` (ad-hoc signed here: no Developer ID in this session)
  builds `Shelbi.app`; the fonts are compiled into the binary
  (`include_bytes!` in `assets.rs`), so the bundle needs no `Resources/`
  copy. Checked: the release binary contains `Geist-Regular`, `-Medium`,
  `-SemiBold` and `SourceCodePro-Regular`, `-Bold` byte for byte. Source
  Code Pro isn't installed on this Mac (only the "for Powerline" family) and
  the renders' Geist widths are 1.7.2's, not the installed 1.2's, so the app
  draws the bundled faces.
- **Zoom in the bundled app** (sandboxed root, the app driven over AX and
  posted keys, no pixels needed): View ▸ Bigger 14 to 15 to 16, Smaller back
  to 14, Actual Size clears `terminal_font_size`; ⌘= / ⌘− / ⌘0 posted to the
  app do the same.
- **The font picker in the bundled app**: pending with the walk (it needs a
  pointer on the screen); the walk opens it, picks Geist Mono and checks the
  saved `terminal_font_family`. Its behavior is covered by the unit tests.

## The real-window walk (pending)

```sh
cd desktop
scripts/bundle.sh
cargo run --features visual-check -- --visual-figma /tmp/vf     # the offscreen frames, to compare with
scripts/figma-window-walk.sh --offscreen /tmp/vf
```

The script (header documents it) builds a sandboxed Shelbi root under `/tmp`
from the harness's fixtures (`--visual-figma-fixtures`, new: the board,
Activity, Machines and sidebar content the frames draw), with the hub and
control socket variables unset and a fake `claude` / `codex`; launches the
bundled app at 1440x1024; then in Auto (following the system), Dark and
Light, switched live with Settings' theme segments, visits all ten screens
and captures the window; opens the font picker, picks Geist Mono, zooms with
⌘= and resets with ⌘0, reading the saved settings back; and diffs the
captures against the references and the offscreen frames. Tested here up to
the lock gate (sandbox, fixtures, launch, the 1440x1024 window found, keys
reaching the app, cleanup leaving no process or directory behind).

What to look at in its output: the real traffic lights (offscreen draws
none), the window's titlebar band and corners, crispness on a Retina display
(run it with the window on one), Auto following a system appearance change,
and scrolling (the board and Activity).

## Departures from the Figma file, for John

Decided:

1. **Geist version.** The frames were laid out with the Geist 1.2 installed
   on John's Mac (`~/Library/Fonts/GeistVF.ttf`); the app bundles 1.7.2,
   whose advances are narrower, so long lines end 1 to 4pt short (and a
   column that follows a word, like Activity's message after the verb,
   starts up to 2pt early). Kept (orchestrator, q-geist-version): updating
   the installed Geist lets Figma draw the shipped metrics.
2. **Font smoothing off** for the app (`dt-figma-qa-sidebar`), so dark text
   has Figma's weight. To confirm.
3. **The component wins over the mockups** (README): "Changelog ⌘L" and ⌘O
   on Orchestrator, the panels' "Developer ⌘1" over "Orchestrator ⌘R", the
   light Issue detail frame's old Issues icon.

Inconsistencies in the Figma file (the app follows the frame it is on, or
the component):

4. **The footer bar** is 28pt on Issues, 26pt on Activity and Machines.
   Each screen matches its frame.
5. **The column rule** is drawn on Review only (above). Should every screen
   have it, or none?
6. **The task panels' labels** in the Review and Workspace frames are the
   mockup's ("Orchestrator ⌘R"); the component's are drawn.
7. **The idle Workspace frame** draws charlie busy in its sidebar and idle in
   its main area; the app shows it idle and selected.
8. **The frames' main content is 1183pt wide** (the old 257pt sidebar), so
   Activity's tags and times reach x 1438, Machines clips its count, and the
   Issue detail overlay (946pt modal centered on 1183) can't be matched by
   a centered modal: the app keeps 80% of the real main area (928pt). Resize
   those frames' main content to 1160?
9. **The Orchestrator transcript is off the 18pt grid** and the composers
   pack four rows into 37pt (`dt-figma-qa-terminal-screens`); a terminal
   draws on rows.
10. **The selected card is 278pt** (its variant hugs), and the dark and light
    Issues frames wrap two excerpts differently (`dt-figma-qa-issues-board`).
11. **The Review card's excerpt** differs from its detail body; the fixture
    follows the detail.

Kept behavior over the mockup:

12. **Activity**: the daemon row has no "hub · cli" line (the event carries
    no host or version); the page keys read `⇞/⇟` (bound), not `⇧↑/⇧↓`.
13. **Settings**: the size field is a stepper at 14 (the frame's "13" text
    field predates Source Code Pro 14); the helper copy drops "Blank uses …"
    (the picker always holds a value).
14. **Issue detail**: the move keys read `H` / `L` (bound), not `l` / `r`.
15. **Copy**: "Detach" (not "Detatch"); the palette reads "Turn Zen Mode off"
    with Zen on.
16. **Task panels**: the active Diff / Editor row keeps its ×; the keyboard's
    pill on the focused row; the expanded description is one stream.
17. **Machines**: an idle slot's runner is `—`.

Rendering and platform residuals:

18. **Fallback glyphs** Geist and Source Code Pro lack: `⎇` (branch), `❯`,
    `⌃`, `✻` draw from CoreText's fallback, not Figma's.
19. **Alpha compositing**: one 8-bit level on translucent fills (rules, the
    dark scrim `#141820` vs `#151820`).
20. **Traffic lights**: AppKit spaces them 23pt, Figma 20pt; offscreen
    renders draw none (the walk will show the real ones).

Contrast below AA, kept as drawn and listed in `theme.md`: the selected dark
Machines row (`table_text` 1.30:1), `log_text` on the pickers (2.53), the
Button icon (2.58), the dark Success label's hover and pressed (4.35, 3.58).

## Checks

- `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings`
  (also with `--features visual-check`), `cargo test`. No `crates/` change,
  no `Cargo.lock` change.
- `--visual-figma`: 0 checks failed, both themes, at 1x and at 2x.
- `scripts/figma-window-walk.sh`: `bash -n`, and a dry run up to the lock
  gate as above.

## Note: the virtual display

The 2x renders used a HiDPI virtual display (CoreGraphics'
`CGVirtualDisplay`) created for the run, named "Shelbi QA 2x". With the
session locked, WindowServer kept it (and a second, 800x600, from an attempt
to release the first) listed after the owning processes exited. They should
go on unlock, or at the latest on logout; they hold no windows. The README's
`--2x` instructions don't use this; it isn't part of the tooling.
