# dt-editor-back-spacing: a clear gap between the traffic lights and Back

The workflow editor holds the whole window (no sidebar), and its titlebar's
Back button sat 1pt from the zoom button. The row reserved a hard-coded
60pt "traffic lights" spacer inside 12pt of padding, plus the row's 8pt gap,
so Back's frame started at x = 80. The real lights end at
`sidebar::TRAFFIC_LIGHTS_RIGHT` = 79 (macOS 26's 14pt button frames, the
close button centered 26pt in, 9pt apart). That 60pt came from Figma's
traffic-lights drawing, which has smaller lights further left. It was never
tied to where the native lights actually are.

## Measured

Figma Header (Settings · Workflow Builder `2305:10965`, instance
`2331:8457`; the Workflow Form frame `2252:29281` uses the same component):

| | Figma | Real (macOS 26) |
|---|---|---|
| Lights | 12pt circles at x 16, 36, 56 (zoom ends at 68) | 14pt frames at x 19, 42, 65 (zoom ends at 79) |
| Back's frame | 12 + 60 + 8 = 80 | was 80, now 91 |
| Zoom edge to Back's frame | 12pt | was 1pt, now 12pt |
| Zoom edge to the arrow's ink | ~20pt (20pt icon centered in the 32pt button) | ~20pt |

The Toolbar Button has no fill at rest (Figma or code). Its hover fill
covers the whole 32pt frame, which is what touched the zoom button. The
spec's "~28pt" doesn't match the frame. The frame gives 12pt to Back's frame,
which is the low end of the spec's 12 to 16pt range, so 12pt it is.

## Changed

- `views/sidebar.rs`: `BESIDE_TRAFFIC_LIGHTS = TRAFFIC_LIGHTS_RIGHT + 12`
  (91pt), the one place a no-sidebar titlebar takes its left edge from. The
  12 is the private `TRAFFIC_LIGHTS_GAP`.
- `views/workflow_editor.rs`: the titlebar drops `TRAFFIC_LIGHTS_W`/`_H`
  and the spacer div, and pads left by `BESIDE_TRAFFIC_LIGHTS` (12pt right
  as before). The row is still 52pt with `items_center`, so the 32pt Back
  centers at 26pt, on the lights.
- `app.rs`: the collapsed sidebar's Show sidebar button moves from
  `TRAFFIC_LIGHTS_RIGHT + 8` (87pt) to `BESIDE_TRAFFIC_LIGHTS` (91pt). It
  wasn't as tight as Back was, but its hover fill sat 8pt from the zoom
  button. It now follows the same rule.
- `views/bootstrap.rs`: the relaunch bar sits at the window's top, above the
  shell, with its warning icon at x = 16, under the close button. It now
  pads left by `BESIDE_TRAFFIC_LIGHTS` and is at least `TITLEBAR_H` tall,
  so its content starts beside the lights and centers on them.

Also checked, unchanged: the bootstrap progress and failure screens (a
centered, 32pt-padded stage, nothing in the titlebar's corner). The update
bar sits in the main column, below the collapsed titlebar row or beside the
sidebar. The sidebar's and the settings sidebar's titlebars put their buttons
on the right.

## Tests

- `app::settings_tests::the_editor_takes_the_window_without_the_sidebar`:
  Back's left edge is `BESIDE_TRAFFIC_LIGHTS`, at least 12pt past
  `TRAFFIC_LIGHTS_RIGHT`, and its middle is at `TITLEBAR_H / 2` (the lights'
  center).
- `views::sidebar::tests::the_traffic_lights_center_where_figma_draws_them`
  pins `BESIDE_TRAFFIC_LIGHTS` at 91.
- The sidebar toggle tests' `expand_origin` reads `BESIDE_TRAFFIC_LIGHTS`.

## Renders

`SHELBI_VISUAL_ONLY=settings --visual-figma` re-rendered the editor frames
(`settings-workflow-builder*`, `settings-workflow-form`,
`state-workflow-*`). Offscreen renders don't draw the native lights. With
the real light frames overlaid, the header has the same spacing next to the
lights as `workflows/2-builder.png`. The whole header sits 11pt further right
than the reference, because the real lights end 11pt further right than
Figma's. `--visual-views` `notice-bars` shows the relaunch bar's new inset.
Checked by eye only. The reference has no lights to diff against at the real
positions.

Out of scope, noted only: the render shows the workflow's name as `task`
where the frame shows `Task`, and the Default badge's fill is darker than
the frame's.
