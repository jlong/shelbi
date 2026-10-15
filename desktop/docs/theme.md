# Desktop theme

The desktop app paints from its own palette, built on the **Taskpage** design
system (32pixels' `tailwind.config.js`: its hues, semantic roles and component
shapes). The TUI keeps the shared tokens in `crates/shelbi-app/src/theme.rs`
unchanged; the desktop no longer reads them.

- `src/theme/palette.rs`: Taskpage's hue scales and the roles below, a light
  and a dark value each, with the contrast tests.
- `src/theme/style.rs`: the component shapes every view shares (nav chip,
  buttons, icon buttons, popovers, chips, the selection pill).
- `src/theme/mod.rs`: the resolved `Theme` global (views read
  `theme.text_quiet`, `theme.surface_recessed`, ...), the mapping from the
  shared view models' meanings onto Taskpage hues, the terminal palette, and
  gpui-kit's widget colors (markdown, inputs) set from the same roles.

Views read roles, never a hue or a hex value. The `theme:` setting (`auto`,
`dark`, `light` in `~/.shelbi/config.yaml`) picks the palette, and `auto`
follows the system appearance live, as before.

## Light: Taskpage as is

A recessed slate-100 leading column on a white main surface, black-600
(`#2e3345`) text, slate-600 (`#7b85a7`) icons, the selected nav row a raised
white chip with Taskpage's `shadow-nav`, menus and dialogs white with a ring
and `shadow-lg`. Two deliberate departures, both for contrast: Taskpage's
`quiet` text is black-600 at 65%, which is 4.3:1 on white, so the desktop uses
72%; and its primary-button hover (brand at 80%) drops white text to 4:1, so
the hover is pink-700 instead.

## Dark: derived

Taskpage has no dark theme, so this one is derived to read as the same family:

- **Surfaces** from the black scale: the leading column is black-700
  (`#131418`); the main area, raised surfaces and the selected chip are steps
  20%, 40% and 50% of the way from black-700 to black-600 (`#2e3345`). As in
  light, the column sits a step below the main area and raised surfaces a
  step above it.
- **Text** is slate-100/slate-400 (near white), with the quiet role at 62%.
- **Icons** are slate-500, the secondary ones slate-600.
- **Hues** move up to their 300/400 steps so they keep contrast on dark;
  chip tints are the hue's 500 step at 18%.
- **Fills, rules and rings** are slate-400 at low alpha, so they work on any
  dark surface.
- **The selected nav row** is a lifted surface with a subtle 1px ring, not a
  white chip.
- **The brand fill** stays pink-600 (white text on it keeps 5:1); as a
  foreground (the selection pill, the focus ring) it is pink-400.

## Contrast

`src/theme/palette.rs` tests WCAG AA (4.5:1) for every text role on every
surface it is drawn on, in both themes: the loud, normal and quiet text on all
four surfaces, the selected chip and the hover and selection fills; every hue
used as text on the surfaces; every chip's text on its own tint; and text on
the brand, Zen and error fills. Icons and the focus ring are tested at 3:1
(WCAG 1.4.11). `text_faint` is exempt (placeholders, disabled and decorative
text). `src/theme/mod.rs` tests the terminal's default foreground at AA and
its ANSI hues at 3:1 on the new terminal background.

## Component shapes (`src/theme/style.rs`)

| Component | Shape |
|---|---|
| Corners | 8px everywhere (`rounded_lg`); 12px on the 32 and 40px icon buttons |
| Nav row | 40px tall (a 36px chip inset 2px), content 8px inside, 20px icon, 8px gap; the current row is the raised chip, loud and medium weight |
| List row (leading column) | 32px; the same chip when selected |
| Current-selection indicator | A 3x16px brand pill at the row's leading edge, on the keyboard's row (the sidebar while focused, the panels' keyboard row, the palette, lists) |
| Section heading | `text-sm` semibold, loud |
| Button | 32px tall, semibold, 2px border; primary = brand fill with white text, secondary = slate fill with loud text, danger = the error fill; the keyboard's button wears a 2px focus ring |
| Icon button | 24, 32 or 40px square, the icon role at rest, `fill_strong` and a loud icon on hover, `fill` while active |
| Menu, popover, palette, dialog | `surface_raised`, 8px corners, a 1px ring and `shadow-lg`; dialogs over the scrim, menus over nothing |
| Chip, badge | 20px, 6px across, `text-xs` medium, the hue's text on its tint |
| Board | Columns on `surface_recessed` with a rule border; cards raised with a ring and a 1px drop; the keyboard's card wears the focus ring; drop targets light up in the focus color |
| Terminal | Background and frame are the main surface; the find bar and "Jump to bottom" are popovers |
| Spacing | Taskpage's 8px scale (2, 4, 6, 8, 10, 12, 16, 20, 24, 32) |

Taskpage's popover motion (400ms on `cubic-bezier(0.16, 1, 0.3, 1)`) is not
adopted yet: the desktop's menus and popovers open without animation.

## Shared meanings

The shared view models name colors by meaning (the activity feed's `Tone`, a
badge's `DecorationColor`). `Theme::tone`, `Theme::tone_hue` and
`Theme::decoration` map each onto the Taskpage hue for the same meaning:

| Meaning (TUI color) | Desktop role |
|---|---|
| Working, done, success (green) | `green` / `success` |
| Ready for review (cyan) | `cyan` / `info` |
| Awaiting input, in progress, warning (yellow) | `gold` / `warning` |
| Error, blocked (red) | `red` / `error` |
| Failure in the feed (light red) | `orange` |
| Queued, the Ready category (blue) | `blue` |
| Handoff, paused (magenta) | `purple` |
| Idle, dim (dark gray) | `text_quiet` |
| Normal (gray) | `text` |
| Emphasis (white) | `text_loud` |

## Roles

Every role with its light and dark value. A translucent value is
`#rrggbbaa`. `the_theme_doc_lists_every_role` in `src/theme/palette.rs` fails
until a new role has its row here, with the values the palette really has.

### Surfaces

| Role | Light | Dark | Used for |
|---|---|---|---|
| `surface` | `#ffffff` | `#181a20` | The main area; the terminal's background |
| `surface_recessed` | `#f8f8fa` | `#131418` | The leading column (sidebar, review and workspace panels), board columns |
| `surface_sunken` | `#f2f3f7` | `#0f1014` | Wells: inline code, command boxes, text fields |
| `surface_raised` | `#ffffff` | `#22252e` | Cards, menus, popovers, the palette, dialogs |

### Fills

| Role | Light | Dark | Used for |
|---|---|---|---|
| `fill_subtle` | `#f2f3f7` | `#e3e5ed0d` | Row and menu-row hover |
| `fill` | `#eceef3` | `#e3e5ed14` | Secondary buttons, an active icon button, segmented controls, a row's hover in the leading column |
| `fill_strong` | `#e3e5ed` | `#e3e5ed1f` | Icon-button hover, secondary-button hover |

### Text

| Role | Light | Dark | Used for |
|---|---|---|---|
| `text_loud` | `#2e3345` | `#f8f8fa` | Headings, titles, the selected row |
| `text` | `#2e3345` | `#e3e5ed` | Body text and labels |
| `text_quiet` | `#2e3345b8` | `#e3e5ed9e` | Secondary text, hints, timestamps |
| `text_faint` | `#2e334566` | `#e3e5ed59` | Placeholders, disabled, decorative (exempt from AA) |
| `text_on_color` | `#ffffff` | `#ffffff` | Text on the brand, Zen and error fills |

### Icons

| Role | Light | Dark | Used for |
|---|---|---|---|
| `icon` | `#7b85a7` | `#acb4cf` | Icons at rest |
| `icon_secondary` | `#c8cdde` | `#7b85a7` | De-emphasized icons, the terminal scrollbar |

### Rules

| Role | Light | Dark | Used for |
|---|---|---|---|
| `rule` | `#2e33451a` | `#e3e5ed14` | Dividers and hairline borders |
| `rule_strong` | `#2e334526` | `#e3e5ed24` | Input borders, a focused column's border |

### Identity

| Role | Light | Dark | Used for |
|---|---|---|---|
| `brand` | `#db0a60` | `#db0a60` | The primary button |
| `brand_hover` | `#b40041` | `#b40041` | Its hover |
| `brand_text` | `#db0a60` | `#ff66ab` | The current-selection pill, the current project's check |
| `accent` | `#0007a7` | `#8fb4ff` | Taskpage's accent (blue) |
| `link` | `#2830f1` | `#8fb4ff` | Links, More |
| `link_hover` | `#db0a60` | `#ff66ab` | A hovered link |
| `focus_ring` | `#db0a60` | `#ff66ab` | The keyboard-focus ring, the find caret, drop targets |
| `wordmark` | `#0c699c` | `#62cdf6` | The project title in the sidebar, the Shelbi wordmark |

### Meaning

| Role | Light | Dark | Used for |
|---|---|---|---|
| `success` | `#00753d` | `#86d580` | Done, approve, a loaded project |
| `warning` | `#856700` | `#ffca0d` | In progress, awaiting input, stale data |
| `info` | `#0c699c` | `#62cdf6` | Ready for review |
| `error` | `#aa0101` | `#ff8484` | Errors, blocked, a version mismatch |

### Hues (text) and chip tints

| Role | Light | Dark | Used for |
|---|---|---|---|
| `green` | `#00753d` | `#86d580` | Working, Done |
| `green_tint` | `#e2f6e0` | `#5ac3512e` | A green chip's background |
| `gold` | `#856700` | `#ffca0d` | Active, warnings |
| `gold_tint` | `#fff0bc` | `#e0b1092e` | A gold chip's background |
| `cyan` | `#0c699c` | `#62cdf6` | Ready |
| `cyan_tint` | `#e0f5fd` | `#31ade92e` | A cyan chip's background |
| `blue` | `#2830f1` | `#8fb4ff` | The Ready category, queued, workflow chips |
| `blue_tint` | `#dce7fe` | `#1757fd2e` | A blue chip's background |
| `pink` | `#b40041` | `#ff66ab` | Pink text (not the brand fill) |
| `pink_tint` | `#fddceb` | `#f214782e` | A pink chip's background |
| `purple` | `#7019c0` | `#e99bfc` | Handoff, paused |
| `purple_tint` | `#f5e7f9` | `#e374ff2e` | A purple chip's background |
| `red` | `#aa0101` | `#ff8484` | Errors, blocked |
| `red_tint` | `#ffd9d9` | `#db00002e` | A red chip's background |
| `orange` | `#9d4100` | `#ff8f27` | Failures in the activity feed |
| `orange_tint` | `#ffddbd` | `#f96a032e` | A orange chip's background |
| `neutral_tint` | `#eceef3` | `#e3e5ed1a` | A neutral chip's background (counts, tags) |

### Bands

| Role | Light | Dark | Used for |
|---|---|---|---|
| `zen` | `#00753d` | `#00753d` | The `ZEN MODE ON` band |
| `error_fill` | `#db0000` | `#aa0101` | The unread-errors button, the Reject (danger) button |
| `notice` | `#fff0bc` | `#e0b10929` | The relaunch band |

### Selection

| Role | Light | Dark | Used for |
|---|---|---|---|
| `nav_selected` | `#ffffff` | `#262a35` | The selected nav row: the raised chip |
| `nav_selected_ring` | `#00000008` | `#e3e5ed14` | Its 1px ring |
| `nav_shadow` | `#1419470d` | `#0000004d` | Its 2px drop (`shadow-nav`) |
| `row_selected` | `#eceef3` | `#e3e5ed1a` | The keyboard's row in a list, palette or menu |
| `text_selection` | `#cadbfe` | `#2c3a5e` | The terminal's selection |
| `search_match` | `#fde89e` | `#4d4320` | A find match |
| `search_current` | `#ffca0d` | `#8a6d00` | The current find match |

### Elevation

| Role | Light | Dark | Used for |
|---|---|---|---|
| `ring` | `#2e33450d` | `#e3e5ed14` | The 1px ring around raised surfaces |
| `shadow` | `#0000001a` | `#00000066` | `shadow-lg` under menus, popovers, dialogs |
| `scrim` | `#00000040` | `#00000080` | The backdrop behind a dialog |
## Renders

Every `--visual-*` mode renders both themes: with `SHELBI_VISUAL_THEME`
unset it runs itself once per theme into `<dir>/dark/` and `<dir>/light/`;
set it to `dark` or `light` for one. `--visual-views` includes the issues
board (`board.png`, `board-dropdown.png`, `board-detail.png`).
