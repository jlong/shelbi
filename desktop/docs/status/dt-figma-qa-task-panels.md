# dt-figma-qa-task-panels

Pixel QA of the task panels (`dt-figma-task-panels`) against John's Figma
file (`pu3hxHgWoCDPCzJ4u6zTQK`), in both themes: the sidebar column of the
Review screen (`2165:275` dark, `2220:21066` light) and of the Workspace
screen (`2165:186`, `2220:21038`), and the Navigation component's
State=Review (`2220:17158`) and State=Workspace (`2220:18764`). Values came
from the Figma MCP: `get_design_context` on both component states and on the
dark Review frame's Navigation instance (`2220:17899`), `get_variable_defs`
on the frames and on the Button component's Success variants (`2220:17613`,
`2224:5698`, `2224:5734`), the exported icon SVGs (which carry each icon's
real stroke color in each mode), and the component states' screenshots as a
second reference for the Workspace panel (the Workspace frames draw the
mockup's "Orchestrator ⌘R", which the component overrides).

## Mismatch, before and after

`figma-diff.sh` share of pixels off by more than 10% in any channel. The
column crops are the panel column (x 0 to 279, the full height, `CORNER=28`);
the AE column is `magick compare -metric AE -fuzz 10%` on the same crop with
the traffic lights masked (the offscreen window draws none), the measure
`dt-figma-task-panels` reported.

| screen | column, before | column, after | AE before | AE after | whole frame, before | whole frame, after |
|---|---|---|---|---|---|---|
| review-dark | 1.96% | 1.85% | 764 (0.27%) | 702 (0.24%) | 1.82% | 0.94% |
| review-light | 2.16% | 2.14% | 911 (0.32%) | 876 (0.31%) | 1.87% | 1.08% |
| workspace-dark | 2.00% | 1.99% | 861 (0.30%) | 851 (0.30%) | 2.26% | 1.87% |
| workspace-light | 2.25% | 2.26% | 1074 (0.37%) | 1056 (0.37%) | 2.34% | 2.02% |

"Before" is the umbrella at `8c31a275` (where this task started); the
`dt-figma-qa-sidebar` font-smoothing and hotkey fixes, which landed on the
umbrella meanwhile, are inside "after". The whole-frame drop is mostly the
column divider (below): every main area sat 4pt right of the frames. Every
other screen's whole-frame number drops with it (Issues dark 8.52% to 6.19%,
Settings dark 2.38% to 1.68%, Activity dark 2.21% to 1.79%; table in
`frames-after/report.txt`).

The column residual is glyph rasterization and nothing else: every text
run's ink box lands on the frame's (title 97x12 at (19, 108), description
lines at x 19, More 27x9 at (19, 231), the views at y 324 to 476, Actions at
(20, 535), the buttons' ink at (19, 568) and (123, 568)), and every flat
fill, text and icon color samples to the Figma value (within one level where
Figma composites alpha by flooring and gpui by rounding). What remains is
Figma's antialiasing against CoreText's, and the frames' Geist v1.2 against
the bundled 1.7.2 (`dt-figma-qa-activity`: the worktree path inks 145pt wide
where the frame's is 148). Against the component screenshots (light, the
MCP's own rasterization) the column differs by 3.45% (Review) and 3.12%
(Workspace), with the same ink boxes, and the selected row's chip, ring and
shadow within one level of it on every row.

## Fixes

- **The description's last line holds what fits** (`elements/clamped_text.rs`,
  new; `views/task_panel.rs` `summary`). The frames end the fifth line
  "Don’t stop until…"; the panel cut it at "Don’t…". gpui's
  `text_ellipsis().line_clamp(n)` decides where to cut by summing
  per-character advances (no kerning), which runs wider than the shaped
  text: its estimate wrapped the full fourth line ("background. Use
  Alacritty for the terminal.") one word early, so its fifth line started a
  word late and was cut short. `clamped_text` wraps with the shaper, finds
  where the last allowed line really starts, and fits as much of the rest as
  the line holds beside the ellipsis. Expanded (More), the description is
  the plain wrapped string, as before.
- **Approve's label and icon are `#86d580` in Dark** (new role
  `success_label`, `SUCCESS_LABEL`; `style::button_colors` for
  `ButtonKind::Primary`, and gpui-kit's `primary_foreground`). The dark
  Review frame's Success button draws its label and its check in `#86d580`
  (the exported SVG's stroke, and the label's sampled ink), not
  `text/green`'s `#31d924`; Light keeps `#00753d`. The Zen pill keeps
  `success` (`text/green`), as both frames draw it.
- **The back arrow is the Button icon color** (`task_panel::back_button`,
  `theme.icon` to `theme.button_icon`): `#acb4cf` at 60% in Dark (it read
  at 90%, `#a1a9c3` where the frame has `#8189a0`), `theme.gray` in Light
  (unchanged).
- **The column's divider is 1pt** (`app.rs`, `sidebar-resize`). The resize
  handle was a 5pt strip after the 280pt column (4pt of the sidebar's fill,
  then the 1pt divider), so the column read 284pt and every main area
  started at x = 285; the frames put the divider at x = 280 and the main
  area at 281. The divider is now the only thing in the layout, drawn on the
  canvas as Figma draws it, and its grab room (`RESIZE_GRAB`, still 5pt) is
  an absolute strip over its own point and the column's last four, so the
  drag works as before (`app_column_tests` drag the divider's center).
- **The expanded description drops task-list checkboxes**
  (`shelbi_app::review::description_text`, shared): a list item's `[ ]` /
  `[x]` went through as text (More showed "[x] Native sidebar…"). The TUI's
  preview, which uses the same function, loses them too. A `[x]` that isn't
  in a list item stays.
- **Contrast exceptions renamed**: the Success label's pairings are now
  `success_label on hover/pressed button` (dark 4.35 and 3.58, up from 4.07
  and 3.35 with `text/green`; light 4.36 unchanged). `docs/theme.md` lists
  them and the new role.
- **Harness** (`visual_figma.rs`): the panel states below render as
  `state-review-*` / `state-workspace-*`, and `SHELBI_VISUAL_ONLY=panels`
  renders just the two screens and their states. `motion::reset_pointers`
  (visual-check only) clears every control's hover and press state when a
  window opens: `fill_layer` keys that state by id, not by window, so a
  button pressed in one offscreen window stayed pressed in the next.
- The fixture's body says "Don’t", with the frames' curly apostrophe.

## The states the frames don't draw

Checked by eye in both themes (`states/` in the QA folder), and sampled:

- **More**: the description expands in place to the whole body, wrapped at
  the same 18pt line in `theme.foreground`; the link reads Less and the
  worktree row and the views move down with it. The keyboard's row moves to
  the link (the accent pill at its leading edge).
- **⌘2 to ⌘5 (Review), ⌘2 to ⌘4 (Workspace)**: the chosen view takes the
  selected chip (the canvas, the raised-card shadow in Light, the
  `icon/select` icon, a loud Medium label) and the keyboard's pill.
  Web Browser (⌘4 in Review) opens the URL and leaves the reviewer shown, so
  its row wears the keyboard's `background` fill while Reviewer keeps the
  chip; Shell opens the split and holds the `background` fill with a loud
  label, as `dt-figma-task-panels` built it.
- **Approve and Reject, hovered and pressed**: the fills are exactly the
  Button component's `background-hover` and `background-active` over the
  sidebar (Light `#d6edd6` / `#c6e8c5` and `#f7c7df` / `#f7afd2`, Dark
  `#395a45` / `#3d6946` and `#632950` / `#772655`, Dark at the derived
  alphas); labels and icons don't change.

## Departures, flagged for John

- **Dark Success label `#86d580`** is a per-frame value with no Figma
  variable (the Button component only defines Light, where the label is
  `text/green`). Every Success button in Dark now uses it.
- **The light Workspace frame draws no column divider**; the light Review
  frame and both dark frames do (`theme.divider` at x = 280). The app draws
  it on every screen.
- **The active Diff / Editor row keeps its × (Close)** after the hotkey: an
  existing behavior the component doesn't draw (it draws only the agent
  selected, which has no ×).
- **The keyboard's pill** on the selected row (the harness renders the panel
  focused), and **"Developer ⌘1" over the Workspace frames' "Orchestrator
  ⌘R"**: unchanged departures from `dt-figma-task-panels` (the component
  wins).
- **The expanded description is one stream of prose**: the body's
  paragraphs, headings and checklist lines are joined (the shared
  `description_text`, as the TUI's preview). No frame draws the expanded
  state.
- **Dark hover / pressed fills** for Success and Danger are still derived
  (Figma defines only Light's), as `docs/theme.md` says.
- **No 2x render**: `--2x` found no Retina display (John's main display is
  1x), so close inspection used 3x and 6x point-sampled crops of the 1x
  renders (`zoom/`).

## Images

In `~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-figma-qa-task-panels/`:
`before/` and `after/` (column crops: side-by-side, heatmap, overlay,
report; `after/*-component-*` against the component screenshots),
`frames-before/` and `frames-after/` (whole frames), `states/` (every state
render plus `review-states-*`, `workspace-states-*` and `buttons-*` contact
sheets), `zoom/` (the top of the panel and the actions at 3x, reference
then render), and the two component screenshots.

## Verification

- `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings`
  (and with `--features visual-check`), `cargo test` (493 pass, rebased on
  `456dc4ef`). New tests: `clamped_text` (the last line starts where the
  unclamped text's fifth line does and one more word wouldn't fit; text
  that fits stays whole). `--visual-figma` passes all its checks.
- Root: `cargo clippy -p shelbi-app -p shelbi-tui --all-targets -- -D
  warnings`, `cargo test -p shelbi-app` (with the checkbox case in
  `description_text_strips_block_markdown`), `cargo test -p shelbi-tui
  review`, `cargo check --workspace --locked` (the root `Cargo.lock` is
  unchanged).
