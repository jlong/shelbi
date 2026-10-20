# Heroicons at 18pt

The X-Small Button's left and right icons (the issue detail's actions,
`Issue actions` in the Issue detail frame, 2172:938) are the Heroicons
outline drawing at 18pt with a 1.25pt stroke: Figma scales the drawing and
keeps the outline set's 1.25pt line. These are the `24/outline` drawings
from `../heroicons/` with only `stroke-width` changed, from 1.5 to 1.6667
(1.25pt once the 24 grid is drawn at 18). A test checks each against its
20pt original.

- `arrow-left.svg`, `arrow-right.svg`: the issue detail's Move buttons.
- `user.svg`, `pencil-square.svg`, `arrow-up-right.svg`: its Assign, Edit
  and Editor buttons.
- `play-solid.svg`: its Start button. Heroicons has no filled play, so this
  one is John's drawing, exported from the frame as Figma draws it (a fill
  in `currentColor` on the 18pt grid), not a widened Heroicon.
- `trash.svg`, `pencil.svg`, `ellipsis-vertical.svg`: Settings ▸ Workflows'
  row buttons (delete a status; a workflow's edit and its menu).
