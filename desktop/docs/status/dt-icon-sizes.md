# dt-icon-sizes: three icon sizes, each from its own Heroicons set

Superseded by [`dt-icons-mini-outline`](dt-icons-mini-outline.md): every icon is now the outline drawing at 20×20.

## What landed

Every icon in the app's own views renders at one of three sizes, and each
size draws from the Heroicons set made for that grid:

| `IconSize` | Pixels | Heroicons v2.2.0 set |
|---|---|---|
| `Standard` | 24×24 | `optimized/24/outline/` |
| `Mini` | 20×20 | `optimized/20/solid/` |
| `Micro` | 16×16 | `optimized/16/solid/` |

Before, the app vendored only the 24px outline set and scaled it to 12, 14,
16 or 20px per view, so a 1.5px-stroke drawing turned to mush at 12px.

- `src/assets.rs` owns the rule: `IconSize` (`px()` gives 24/20/16), `Icon`
  (one symbol, a path per size), and `heroicon(icon, size)`, which returns the
  sized `svg()` (`flex_none`, so a row never squeezes it). One `heroicons!`
  list declares each icon once and generates its `icons::` constant and the
  three embedded drawings.
- The SVGs live in `assets/icons/heroicons/{24,20,16}/`. The 24px files were
  moved, unchanged (byte-identical to the `heroicons@2.2.0` npm package's
  `24/outline`); the 20 and 16 files come from the same package's
  `20/solid` and `16/solid`. All 32 icons the app uses are in all three.
- The per-view `ICON` / `ICON_SIZE` constants are gone. `palette.rs`'s
  `ICON_W` stays: it is the palette's glyph column (text glyphs, not SVGs),
  28px, which also fits a mini icon.
- gpui-kit's internal glyphs (checkbox tick, dropdown caret) are untouched.

## Which size where

Chosen by context, not by rounding the old number:

- **Standard 24**: the sidebar's view icons and its footer buttons
  (settings, collapse), the collapsed window's expand button, the Review
  panel's back button and its Chat/Diff/Editor/Browser switch rows (the
  panel's equivalent of the sidebar's view rows), the description modals'
  close buttons, the bootstrap screen's title warning, the relaunch and
  update bars' status icons.
- **Mini 20**: the board and activity filter chips (and Load older), the
  board toolbar's pending spinner, the sidebar's project chevron, search
  box and project switcher check, the Review folder row and Approve/Reject,
  the Workspace and task-detail labelled icon buttons, the bootstrap action
  button and copy-command button, the banner and update-bar dismiss buttons,
  the terminal find bar.
- **Micro 16**: board card badges (blocked lock, pending spinner, notice
  warning and dismiss), the task-detail notice warning, the activity
  picker's check, the Machines and Review link arrows, the Workspace
  header's remote globe, the sidebar's "polling" marker, the terminal's
  jump-to-bottom arrow.

## Layout adjustments

- Icon buttons that grew to a standard icon use `p_0p5` (28px square) instead
  of `p_1`: the sidebar footer's settings/collapse and the expand-sidebar
  button (which must clear its 38px traffic-light strip). The sidebar's
  error button follows them (`IconSize::Standard.px() + 4px`, was 26px), and
  the Review back button is 32px square (was 28px with a 16px icon).
- Top-aligned icons beside wrapping text re-centred on the first line: the
  card lock `mt 3px` (was 2), the card notice warning and dismiss and the
  task-detail warning `mt 2px`.
- Chips and labelled buttons keep their padding: `text_xs` lines are already
  ~19px tall, so a 20px icon adds under a pixel.

## Guards

`src/assets.rs` tests:

- `every_heroicon_loads_as_an_svg_in_all_three_sizes`: every icon in every
  size loads, sits at `icons/heroicons/<px>/<name>.svg`, has a
  `viewBox="0 0 <px> <px>"`, and is outline only at 24.
- `heroicons_are_listed`: three drawings per icon.
- `icon_sizes_are_24_20_and_16_pixels`: the size-to-pixels mapping.
- `only_the_icon_helper_draws_icons`: no file under `src/` other than
  `assets.rs` contains `svg()` or a raw `icons/heroicons/` path, so no view
  can draw an icon or set its size itself.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
  (301 passed) in `desktop/`.
- The offscreen frames (`--visual-check`, `--visual-views`,
  `--visual-workspace`, `--visual-review`, feature `visual-check`): every
  check passed; the chips, find bar, Review panel, Workspace header and
  activity picker were inspected for alignment and clipping.
