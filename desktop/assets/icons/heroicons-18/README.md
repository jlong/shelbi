# Heroicons at 18pt

The X-Small Button's left icon (the board's filters, `Board toolbar` in the
Figma Issues frame, 2166:580) is the Heroicons outline drawing at 18pt with
a 1.25pt stroke: Figma scales the drawing and keeps the outline set's
1.25pt line. These are the `24/outline` drawings from `../heroicons/` with
only `stroke-width` changed, from 1.5 to 1.6667 (1.25pt once the 24 grid is
drawn at 18). A test checks each against its 20pt original.

- `funnel.svg`: the board's workflow and workspace filters.
