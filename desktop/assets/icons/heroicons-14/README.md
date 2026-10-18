# Heroicons at 14pt

The Figma file draws a few icons at 14pt: the Activity frame's pickers
(`Icon / funnel`, `Icon / user`, node 2168:717 and 2168:721). Figma scales
the `24/outline` drawing to 14pt but keeps its 1.5pt stroke, so these are
the same Heroicons v2.2.0 drawings (MIT, see `../heroicons/LICENSE`) with
the stroke widened to `1.5 × 24 / 14 = 2.5714` on the 24 grid: drawn at
14pt, the stroke is 1.5pt, as Figma renders it.

To add one, copy the drawing from `../heroicons/`, change only
`stroke-width`, and add a row to the `@small` list in `src/assets.rs`.
