# dt-dropdown-chevron-small: one small chevron on every dropdown

Every button that opens a menu now draws Figma's `chevron-down-small`
(component 2342:37950) in its Right Icon slot, the chevron the Issues
Header's Workflow and Workspaces filters draw (2172:722), instead of the
Heroicons `chevron-down`. It is a 20pt drawing with a smaller, flatter
chevron (about 8.8 x 4.4), stroked at 1.25pt.

## How it works

- `assets/icons/figma/chevron-down-small.svg`: Figma's exported path,
  unchanged, stroked in `currentColor` (the export's fixed `#7B85A7` and
  0.9 opacity dropped so each button tints it). Listed in the `@figma`
  icons as `icons::CHEVRON_DOWN_SMALL`, at `ICON_SIZE` (20pt), so it sits
  in the Right Icon slot (12pt wide, the 20pt drawing hanging past it) as
  the old chevron did.
- `theme::style::DROPDOWN_CHEVRON` is the one constant a dropdown names,
  with two ways in: `ButtonContent::dropdown()` for a `content_button`,
  and `style::dropdown_slot(size, color)` for a hand-built button. Colors
  are each call site's as before (the style's `icon` color, or
  `button_icon`), so hover, pressed, disabled, both themes and Contrast
  are unchanged.

## Changed

| Dropdown | Where |
|---|---|
| Board filters (Workflow, Workspaces) | `style::filter_chip` |
| Button sheets' right icon (the visual harness's component sheets) | `style.rs` tests' `sheet_content`, `visual_figma.rs` |
| Agent editor's pickers (Preferred runner, Model, Reasoning effort, Permissions) | `agent_editor.rs` `select_button` callers |
| Workflow editor's pickers (Initial status, Owner, Agent, From / To, Merge strategy, Review mode, ...) | `workflow_editor.rs` `picker_button` |
| Workflow editor's action chips (Push branch, Open PR, ...) | `workflow_editor.rs` `action_chip` |
| Workflow builder's inspector pickers | `workflow_builder.rs` `picker` |
| Workflow builder's zoom menu | `workflow_builder.rs` |
| Settings ▸ Workflows' status Category picker | `workflows.rs` `category_picker` |
| Workspace dialog's Machine picker | `workspace_dialog.rs` |

## Unchanged

- The terminal search bar's older/newer buttons (`icons::CHEVRON_UP` /
  `CHEVRON_DOWN`), which step matches and open no menu: the Heroicons
  `chevron-down` stays vendored for the newer button.
- The sidebar's fold chevrons (`glyphs::CHEVRON_DOWN`, turned by
  `theme/fold.rs`), the project switcher's up-down chevron, and
  Settings ▸ Keybindings' context picker, which draws the up-down chevron
  as its frame does.
- The agent editor's Workflow assignments rows (`CHEVRON_RIGHT`: they open
  a workflow, not a menu).

## Tests

- `assets::tests::dropdowns_draw_the_small_chevron`: the constant is
  `CHEVRON_DOWN_SMALL`, its SVG carries Figma's path and stroke, and no
  source file outside `assets.rs` and the terminal names
  `icons::CHEVRON_DOWN` (so a new dropdown can't pick the old chevron).
- The existing icon checks cover the new file (20pt grid, `currentColor`,
  listed, drawn at a Figma size).

## Captures

`shelbi-desktop --visual-figma <dir>` before (origin/main) and after, in
`~/.shelbi/projects/shelbi/design-refs/dt-dropdown-chevron-small/`:
`before-after-<what>-<theme>.png` for the board filters, the agent
editor's pickers and the workflow editor's pickers and action chips, dark
and light (each before over after, cropped).
