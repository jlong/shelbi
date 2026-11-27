# narrow-window: views that broke at half a screen

Filed by [`dl-parity`](dl-parity.md). Hyprland tiles a second window at half
the monitor (about 940pt on a 1920pt screen), and the app sets no minimum
size, so a narrow window on macOS shows the same. At 940pt the main area
beside the 280pt settings sidebar leaves a table about 596pt wide.

## What broke and what changed

| View | At 940pt | Now |
|---|---|---|
| Orchestrator onboarding | The 520pt and 620pt columns were fixed widths centered in a narrower main area, so their left edge (the titles, the pickers' text) ran off the area | Each column is as wide as the area inside its 40pt inset, at most its Figma width. The Harness and Default model menus are as wide as their column (it measures itself each frame) |
| Settings ▸ Agents, ▸ Workspaces | The fixed columns (runner or machine 140, statuses or tags 268, 56, 90, and their gaps) took 674pt, so the name column shrank to nothing and ran down the page a letter a line | `workflows::name_and_columns`: the name grows from 200pt and the columns sit in one group beside it. A table too narrow for both wraps the group onto its own line under the name, and the statuses or tags column shrinks there. The headings wrap at the same width, so they stay over their columns |
| Settings ▸ Workflows' Statuses | Name 316, ID 200 and Category 200 were fixed, so the table ran past its right edge | The columns shrink together, to at least 160, 80 and 120pt; "Used in" starts at 120pt, grows past it, and has no minimum. The handle and the name share one Name column, the heading's width, so they line up at every width |
| Settings ▸ Keybindings | The list and its groups were a fixed 640pt (plus the hover buttons' room), so the keycaps were cut off | The list and groups are at most those widths, the hover buttons keep their room inside the list, and a group's hint truncates before its title |

## Decisions

- **Layouts that adapt, not a minimum window size.** A tiling compositor
  sizes a tiled window itself and ignores a minimum, so a minimum would
  not have fixed the Omarchy case.
- **Wrap, don't squeeze, the Agents and Workspaces rows.** Shrinking every
  column to fit 596pt would truncate the runner to two letters. Wrapping
  keeps every column at its width, and only statuses and tags shrink, since
  they already wrap their chips.
- **No width measurement for the tables.** Flex wrap and shrink settle it
  in one layout pass, with no breakpoint state or second frame. Only the
  onboarding's menus read their column's last-painted width.

## Tests

- `app::settings_tests::the_agents_table_wraps_its_columns_under_the_name_in_a_narrow_window`
- `app::settings_tests::the_workspaces_table_wraps_its_columns_under_the_name_in_a_narrow_window`
- `app::settings_tests::the_statuses_table_fits_a_narrow_window`
- `app::settings_tests::the_keybindings_fit_a_narrow_window`
- `app::onboarding_tests::the_steps_fit_a_narrow_window`

Each resizes the test window to 940pt and checks that the row, its cells and
its buttons lie inside their table (or the main area), with the name and the
pickers keeping their room.
