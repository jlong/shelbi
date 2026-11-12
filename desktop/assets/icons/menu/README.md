# The project menus' 16pt marks

The project switcher (Figma `Project switcher`, 2186:1692) and the command
palette's Projects column (2171:825) draw their rows' marks at 16pt, on a
16pt grid, as exported from John's Figma file (`Shelbi`, fileKey
`pu3hxHgWoCDPCzJ4u6zTQK`). Each paints in `currentColor`; the views tint
them (`icon_green` for the filled dot, `gray` for the rest).

- `circle-solid.svg`: an open project's dot (component `circle-solid`).
- `circle.svg`: a project that isn't open, a hollow ring (`circle`).
- `check-small-heavy.svg`: the current project's check
  (`check-small-heavy`, 2196:9086), a 2.5pt stroke.
- `plus.svg`: "Add project" (the Heroicons plus, drawn on the 16pt grid
  with a 1.25pt stroke).

- `x-mark.svg`: a project tab's close button, in its dot's place while
  the tab is hovered (the Heroicons x-mark on the 16pt grid with a 1.25pt
  stroke).

The terminal's "Jump to bottom" pill draws its arrow on the same grid, so
it sits at 16pt beside the pill's X-Small label:

- `arrow-down.svg`: the Heroicons arrow-down, drawn on the 16pt grid with a
  1.25pt stroke (the 24/outline path scaled by 2/3, as `plus.svg` is).
