# dt-icons-mini-outline: every icon is the outline drawing at 20×20

Replaces the three-size rule from [`dt-icon-sizes`](dt-icon-sizes.md) at the
user's direction ("use the Mini outline Heroicons for everything").

## What landed

- `src/assets.rs`: `heroicon(icon)` takes no size and always draws the
  icon's outline SVG at `ICON_SIZE` (20px), `flex_none`. `IconSize` is gone;
  `Icon` carries one path. The module docs state the rule and how to add an
  icon.
- Heroicons v2.2.0 has no 20px outline set, so the `24/outline` drawings are
  rendered at 20×20 (their 1.5px stroke reads as 1.25px). They moved up to
  `assets/icons/heroicons/<name>.svg`, unchanged; the `20/solid` and
  `16/solid` sets and their embedded entries are removed. LICENSE stays.
- Call sites only lost their size argument, plus the layout spots that were
  built around 24 or 16:
  - Icon buttons keep their hit area by padding the smaller icon: the
    collapsed-sidebar menu button and sidebar footer buttons go `p_0p5` →
    `p_1` (28px), the sidebar error button is `ICON_SIZE + 8` (28px), the
    Workspace/Review back buttons are `ICON_SIZE + 12` (32px), and the
    description overlays' close button gains `p_0p5` (24px).
  - Former 16px icons in text rows: the board card's lock badge offset is
    `mt(1)` (was 3) to center on the `text_sm` title line; the card and task
    detail notice rows drop their `mt(2)` (a 20px icon fills a `text_xs`
    line).

## Tests

- `every_heroicon_loads_as_an_outline_drawing_on_a_24_grid`: every icon
  loads from `icons/heroicons/<name>.svg`, is `fill="none"` with a
  `currentColor` stroke, on a 24 viewBox.
- `every_icon_draws_at_20_pixels`: `heroicon` sizes every icon 20×20 and
  never shrinks it.
- `heroicons_are_listed`: one drawing per icon.
- `only_the_icon_helper_draws_icons` (kept): no `svg()` or raw icon path
  outside `assets.rs`.

`--visual-check`, `--visual-views`, `--visual-workspace` and
`--visual-review` frames were checked for alignment and clipping.
