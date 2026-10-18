# dt-figma-issue-detail

The issue detail (the card's modal over the board) now matches the Figma
Issue detail frame (dark 2172:722, light 2236:24534; the modal is 2172:910
inside the overlay 2172:909), in both themes. Values came from the Figma MCP
(`get_design_context` on 2172:910, `get_variable_defs` on the dark overlay
and the light frame), then were checked against the 1x reference PNGs with
ImageMagick.

## What changed

- **Modal** (`src/views/task_detail.rs`): still 80% of the main area across
  and 85% down, centered (946 x 870 at x=399, y=77 in the 1440 x 1024 frame;
  the scrim was already right). It is now the `canvas` with a 1px `divider`
  border and 8pt corners, with no drop shadow. The PNGs show no shadow, and
  the edge measures as `divider` in both themes. This replaces the raised
  dialog surface the task body suggested: the frame wins.
- **Header**: 20pt across, 16pt over, 8pt under. The title is Geist Bold
  18 / 28 in `text_loud` (the frame's own size, not a named style), and it
  still wraps. The close button is the quiet 24pt icon button, placed so its
  x-mark sits where the frame draws it.
- **Field table**: Base, 4pt between rows, each row 20pt, 12pt under. The
  key is an 88pt `muted` column, 8pt gap, then the value in `foreground`.
  `status` and `workflow` are plain text now ("Review   priority 1",
  "task"), not chips. `created` / `updated` show the date only
  (`2026-10-12   updated 2026-10-16`). Unassigned, depends on / blocked by,
  PR and checklist keep their rows.
- **Status name**: the frame spells the status the way the workflow does
  ("Review"), and the model only carried the column label ("REVIEW"). So
  `shelbi_app::TaskDetailModel` gains `status_name`, which the desktop
  shows. The TUI keeps `status_label`. `docs/parity.md` has the row.
- **Action buttons**: the Figma Button at X-Small. The `background` fill
  still fades on hover and press. Each button is 26pt tall, 8pt across, 4pt
  gaps and 6pt corners, with an X-Small Medium label in `text_loud`. The
  icon sits in a 14pt slot in `icon`, and the key is one 16pt keycap: X-Small
  `gray` on the canvas, 4pt corners, with a 1px drop in the new
  `button_keycap_edge` role (the frame's `theme.search`: near black at 55%
  in Dark, `#acb4cf` at 60% in Light). Move right carries its arrow after
  the keycap, as drawn.
  - Labels follow the frame: Move, Move, Start, Assign, Edit, **Editor**
    (was "Open in editor", which is now its tooltip).
  - Keys are the real ones, cased the way the frame shows Editor's `E`: a
    shifted letter shows as its capital. So the buttons read Move `H`,
    Move `L`, Start `s`, Assign `a`, Edit `e`, Editor `E`. The frame's `l` /
    `r` on the Move buttons don't match any binding (the popover moves with
    H / L), so I kept the real keys.
  - Icons follow the frame. Start is a filled play, John's drawing from the
    frame, vendored as `assets/icons/figma/play-solid.svg` and scaled from
    its 18pt grid to 20pt. Editor is Heroicons `arrow-up-right` (newly
    vendored, v2.2.0). The outline `play` lost its last user and is gone.
- **Divider**: 1px `divider`, 20pt in from each side, 12pt over the body.
- **Body** (`src/views/markdown.rs`, `markdown::body`): still gpui-kit's
  Markdown text view, now gpui-base's `TextView` (through `gpui_kit::base`,
  no new dependency) so the detail can set the full `TextViewStyle`. Text is
  Base 14 / 18 in `foreground`. h1 is `Desktop/Heading` (20 / 28 bold); h2
  and below are `Desktop/Base/Medium`; all headings are `text_loud`. Every
  block is 16pt from the next, with no extra section gap over headings.
  Links, the selection, rules and code take palette roles.
- **Checkboxes**: a task list (every item `- [ ]` / `- [x]`, one paragraph
  each) renders as a custom block. Each item gets a 22pt row, 16pt from the
  next. The box is 14pt with 2pt corners and a 1px `muted` edge. A checked
  box is filled `text_loud` with the frame's 12pt check (its own path, in
  the canvas color). Item text stays `foreground` whether checked or not, as
  drawn. Item text is still Markdown (inline code and links work) and still
  selectable. Mixed lists, plain lists and lists with nested blocks keep the
  text view's own rendering.
- **Unchanged**: every action and its key, ✕, Esc and a click on the scrim
  to close, the in-place editor (Save `⌘S` / Cancel `Esc`, now in the same
  compact buttons), the empty-description state, and the pending and error
  lines (now spaced 12pt over the divider).

## Checked

- `--visual-figma` renders of `issue-detail-{dark,light}` against the
  reference PNGs. I diffed the 946 x 870 modal on its own: the render sits
  23pt left of the reference because its sidebar is 257pt against Figma's
  280pt, which is the sidebar task's. Pixels differing beyond 10% fuzz:
  - Dark: 2.08% before, 0.99% after.
  - Light: 2.27% before, 1.15% after.
  - What's left is glyph rasterization (Figma's Geist runs slightly wider
    than the bundled 1.7.2) and the H / L keycaps being wider than the
    mockup's l / r.
- Row rhythm matches the frame to the pixel in both themes: fields from
  y=130, actions at y=306 (26pt tall), divider at y=344, body heading at
  y=357, checkbox rows from y=498, 38pt apart.
- Checkbox, keycap and button fills were sampled against the PNGs (button
  `#383d4c` / `#f5f6f9`, keycap canvas, check cut-out).
- Tests: `markdown` covers which lists become checkbox rows, inline Markdown
  kept in an item, and the header's `N/M done` agreeing with the rows the
  body draws.
- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings`
  (also with `--features visual-check`) and `cargo test` (484 passed after
  the rebase onto 5e5739d5, with and without `visual-check`).
- The workflow chip was the last construction of `Hue::Blue`, so the
  variant, its arm and its test line are gone. `blue_tint` stays because
  the update notice and the contrast table use it.
- On 5e5739d5 the sidebar's search box no longer draws keycaps, so this
  view was the last caller of `style::keycaps`, and the view now draws its
  own keycap. I removed `style::keycaps` and the `keycap_edge` role that
  only it used. `split_chord` stays because `hotkey` uses it.
- Root: `cargo clippy --workspace --all-targets --locked -- -D warnings` and
  `cargo test -p shelbi-app` (including `parity_doc`). The root
  `Cargo.lock` didn't move.

## Not matched, on purpose

- Icons are 20pt (the desktop's `heroicon` size) where the frame draws 18pt
  in the buttons and a 16pt x-mark. The asset guard on this base still
  allows only 20pt, and sibling branches are reworking icon sizes.
- The close x-mark is `icon` (`theme.gray`) where the frame draws it in
  `theme.muted`: the shared quiet icon button sets the color.
- All button icons take `icon`. The frame draws the two arrows at 60% and
  the rest at 90%.
