# Heroicons at 18pt

The X-Small Button's left and right icons are the Heroicons
outline drawing at 18pt with a 1.25pt stroke: Figma scales the drawing and
keeps the outline set's 1.25pt line. These are the `24/outline` drawings
from `../heroicons/` with only `stroke-width` changed, from 1.5 to 1.6667
(1.25pt once the 24 grid is drawn at 18). A test checks each against its
20pt original.

- `arrow-left.svg`, `arrow-right.svg`: the component sheets' X-Small
  icons (the old issue detail's Move buttons; the task pane that replaced
  it puts its actions in a menu), and the Add Project wizard's arrow.
- `trash.svg`, `pencil.svg`, `ellipsis-vertical.svg`: Settings ▸ Workflows'
  row buttons (delete a status; a workflow's edit and its menu).
- `plus.svg`: the workflow editor's Add action button (a transition row's
  `+`, Workflow Transition Row 2277:1519).
- `x-mark.svg`: the Workflow Builder inspector's 24pt close button
  (Workflow Builder frames 2303:5936 and 2307:11276).
