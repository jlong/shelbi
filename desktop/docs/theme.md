# Desktop theme

The desktop app paints from its own palette, built on the **Taskpage** design
system (32pixels' `tailwind.config.js`: its hues, semantic roles and component
shapes). The TUI keeps the shared tokens in `crates/shelbi-app/src/theme.rs`
unchanged; the desktop no longer reads them.

- `src/theme/palette.rs`: Taskpage's hue scales and the roles below, a light
  and a dark value each, with the contrast tests.
- `src/theme/style.rs`: the component shapes every view shares (nav chip,
  buttons, icon buttons, dropdowns, segmented controls, the search box and
  its keycaps, project badges, popovers, dialogs, chips, the selection pill,
  empty states, tooltips).
- `src/theme/motion.rs`: Taskpage's motion: its easing curve, the spring the
  selection chip slides on, and the fades every hover and press runs.
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
- **Controls carry a boundary.** Light's buttons, dropdowns, the search box
  and segmented controls are Taskpage's fill-only slate-300, and their labels
  identify them. On dark a fill that light would need a white label to clear
  4.5:1 on, so instead every control keeps a quiet fill and a 1px
  `control_border` (slate-400 at 42%) that clears 3:1 on every dark surface,
  strengthening on hover and while pressed or open.
- **The selected tab or segment** is a lifted surface (`tab_selected`, a step
  above the selected nav row) with a 1px ring at 3:1 against its track, a
  loud label and semibold weight: never only a tint.
- **The selected nav row** is a lifted surface with a 1px ring, not a white
  chip.
- **The brand fill** stays pink-600 (white text on it keeps 5:1); as a
  foreground (the selection pill, the focus ring) it is pink-400. Its hover
  darkens to pink-700 with a pink-400 boundary, since pink-700 alone falls
  under 3:1 on the dark surfaces.
- **The danger fill** is red-500 (white text 5.2:1) with a red-400 boundary.
- **The Zen band** is `#00803f`, between green-700 and -600: white text keeps
  5:1 and the band 3:1 against the column.

## Contrast

`src/theme/palette.rs` tests WCAG AA (4.5:1) for every text role on every
surface it is drawn on, in both themes: the loud, normal and quiet text on all
four surfaces, the selected chip and the hover and selection fills; every hue
used as text on the surfaces; every chip's text on its own tint; and text on
the brand, Zen and error fills. Icons and the focus ring are tested at 3:1
(WCAG 1.4.11). `text_faint` is exempt (placeholders, disabled and decorative
text).

`controls_clear_contrast_in_every_state` covers the controls on the main,
leading and raised surfaces, at rest, hovered and pressed: the loud and normal
labels at 4.5:1 on the control's fill, the icon at 3:1 (loud once hovered),
and on dark the control's outer edge (its border over its fill) at 3:1
against the surface. It checks the selected segment (its label at 4.5:1, it
sits lighter than its track, and on dark its ring at 3:1 against the track),
the primary and danger buttons (white labels at 4.5:1 in every state, their
edges at 3:1 on dark), a text field's border at 3:1 in both themes, the focus
ring at 3:1, the Zen band at 3:1 on dark, and the search box's keycap label.
`project_badges_carry_white_initials` checks every badge fill at 4.5:1 under
white initials. `src/theme/mod.rs` tests the terminal's default foreground at AA and
its ANSI hues at 3:1 on the new terminal background.

## Component shapes (`src/theme/style.rs`)

| Component | Shape |
|---|---|
| Corners | 8px everywhere (`rounded_lg`); 12px on the 32 and 40px icon buttons and the search box; 6px on segments, badges and keycaps |
| Project switcher | ContextStore's `SpacePicker`: 36px, 8px across, 8px corners, a 24px initials badge, the name at 13px medium, the up-down chevron in the quiet color; the control fill, stronger on hover and pressed while open. Its menu is a popover 6px below, at least the button's width, drawn over the sidebar (nothing moves): 36px rows of badge, name and a check on the current project, a rule, then "Add project…"; ↑↓ / Tab move, Enter picks, Esc or a click outside closes |
| Project badge | Two-letter initials (two words, camel case split), bold white, on a Taskpage hue picked from the name ([`badge_fill`]) |
| Search box | Taskpage's `SearchBox`: the control fill with 12px corners, 4px gap, a magnifying glass, "Search" in the normal text color growing to fill, then the palette chord as keycaps; hover fades to the hover fill |
| Keycaps | Taskpage's `Shortcut`: one 18px cap per key, 11px medium uppercase, `keycap` with a 1px `keycap_edge` drop |
| Nav row | 40px tall (a 36px chip inset 2px), content 8px inside, 20px icon, 8px gap; the current row is the raised chip, loud and medium weight, its icon loud |
| Selection chip | One chip for the current row; when the selection moves (keys, clicks, route changes) it slides to the new row on a spring with a slight overshoot (`NAV_SPRING`, about 250ms) before the row takes it back. The sidebar, the review and workspace panels' view switches, and every segmented control |
| List row (leading column) | 32px; the same chip when selected |
| Current-selection indicator | A 3x16px brand pill at the row's leading edge, on the keyboard's row (the sidebar while focused, the panels' keyboard row, the palette, lists); it rides the sliding chip |
| Section heading | `text-sm` semibold, loud |
| Button | 32px tall, semibold, a 1px boundary; primary = brand fill with white text, secondary = the control fill and boundary with loud text, danger = the error fill with its own boundary, quiet = no fill until hovered; fills fade on hover and press; the keyboard's button wears a 2px focus ring. Compact actions (the card detail, the error log) are 28px and `text-xs`, with their key as keycaps |
| Dropdown | A button that opens a menu (the board's and activity's filters): the control fill and boundary, an icon, a medium label, a chevron; pressed while its menu is open, and the menu opens 6px under it |
| Segmented control | A track (the control fill and boundary, 2px inside) of 24px segments (28px in Settings); the selected one is `tab_selected` with its ring and `shadow-nav`, loud and semibold; the others normal text, a fill fading in on hover |
| Icon button | 24, 32 or 40px square, the icon role at rest, the hover fill fading in and a loud icon on hover, the control fill while active; a tooltip names it where the icon alone might not |
| Text field | `input_border` (3:1 in both themes), 8px corners; the focus ring while it has the keyboard |
| Menu, popover | `surface_raised`, 8px corners, a 1px ring and `shadow-lg`; menu rows fade a fill in on hover |
| Palette, dialog | Taskpage's command dialog: over the scrim (slate-700 at 50%, no blur in gpui), `surface_raised`, 8px corners, a 1px ring and `shadow-xl`. The palette sits a fifth of the way down the window, 672px wide (`max-w-2xl`: Taskpage's `max-w-xl` plus room for the Projects column) |
| Chip, badge | 20px, 6px across, `text-xs` medium, the hue's text on its tint |
| Empty state | A quiet sentence on why there is nothing, and the action that changes it (clear the filters, refresh, write a description); an empty board column is a dashed well saying a card can be dragged in |
| Tooltip | A small popover, `text-xs`, on icon buttons and on text cut short (a long reachability error, a PR link) |
| Board | Columns on `surface_recessed` with a rule border; cards raised with a ring and a 1px drop; the keyboard's card wears the focus ring; drop targets light up in the focus color |
| Terminal | Background and frame are the main surface; the grid is inset 12px from the leading edge and 8px top and bottom; the find bar and "Jump to bottom" are popovers |
| Spacing | Taskpage's 8px scale (2, 4, 6, 8, 10, 12, 16, 20, 24, 32) |

## Motion (`src/theme/motion.rs`)

- **Menus and dialogs** open with Taskpage's slide-and-fade: 8px of travel up
  into place and opacity, on `cubic-bezier(0.16, 1, 0.3, 1)`: 200ms for the
  project switcher's menu (in and out) and the board's and Activity's filter
  menus (in), 400ms for the palette and dialogs, their scrim fading with
  them.
- **The selection chip** slides between rows on a damped spring (damping
  ratio 0.75 at 22 rad/s: framer-motion's `bounce: 0.25`), reaching its row
  in about 200ms with a 3% overshoot and settling by 350ms. A retarget
  mid-slide starts from where the chip is, with its velocity.
- **Hover and press** fade a control's fill over about 200ms (a critically
  damped spring, no overshoot), Taskpage's `transition-colors`.
- **Reduce Motion** (System Settings ▸ Accessibility ▸ Display) makes all of
  it instant. The app hands the setting to gpui at launch and each time a
  window comes forward, so gpui's own animations end where they would; the
  chip and the menus read it when they start.

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
| `fill` | `#eceef3` | `#e3e5ed14` | A row's hover in the leading column, the keyboard's row |
| `fill_strong` | `#e3e5ed` | `#e3e5ed1f` | A row's pressed fill |

### Controls

| Role | Light | Dark | Used for |
|---|---|---|---|
| `control_fill` | `#eceef3` | `#e3e5ed1a` | Secondary buttons, dropdowns, the search box, the project switcher, a segmented control's track, an active icon button |
| `control_fill_hover` | `#e3e5ed` | `#e3e5ed29` | Their hover; an icon button's hover |
| `control_fill_pressed` | `#acb4cf73` | `#e3e5ed38` | Pressed, or open (a dropdown or the switcher with its menu showing) |
| `control_border` | `#eceef3` | `#e3e5ed6b` | A control's 1px boundary: invisible in light (fill only, as Taskpage), 3:1 on every dark surface |
| `control_border_hover` | `#e3e5ed` | `#e3e5ed94` | The boundary on hover, pressed and open |
| `input_border` | `#7b85a7` | `#e3e5ed6b` | A text field's border, a checkbox's (3:1 in both themes) |
| `tab_selected` | `#ffffff` | `#3a3f4f` | The selected segment of a segmented control |
| `tab_selected_ring` | `#2e334514` | `#e3e5ed80` | Its 1px ring (3:1 against the track in dark) |
| `keycap` | `#ffffff` | `#e3e5ed24` | A keycap in a shortcut hint |
| `keycap_edge` | `#7b85a74d` | `#00000099` | The keycap's 1px bottom edge |

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
| `brand_hover_border` | `#b40041` | `#ff66ab` | Its boundary on hover |
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
| `zen` | `#00753d` | `#00803f` | The `ZEN MODE ON` band |
| `error_fill` | `#db0000` | `#db0000` | The unread-errors button, the danger button |
| `danger_border` | `#db0000` | `#ff3737` | The danger button's boundary |
| `notice` | `#fff0bc` | `#e0b10929` | The relaunch band |

### Selection

| Role | Light | Dark | Used for |
|---|---|---|---|
| `nav_selected` | `#ffffff` | `#2a2e3a` | The selected nav row: the raised chip (it slides) |
| `nav_selected_ring` | `#00000008` | `#e3e5ed24` | Its 1px ring |
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
| `scrim` | `#444c6580` | `#131418b3` | The backdrop behind the palette and dialogs (Taskpage's slate-700, stronger for the missing blur) |

### Project badges

Not roles: the same eight fills in both themes, picked per project by
`badge_fill` (an FNV-1a hash of the name), each dark enough for white
initials at 4.5:1: pink-600 `#db0a60`, blue-600 `#2830f1`, purple-700
`#7019c0`, green-700 `#00753d`, cyan `#0c699c`, orange-700 `#9d4100`, red-600
`#aa0101`, gold-700 `#856700`. There is no project logo yet, so every project
gets its initials.
## Renders

Every `--visual-*` mode renders both themes: with `SHELBI_VISUAL_THEME`
unset it runs itself once per theme into `<dir>/dark/` and `<dir>/light/`;
set it to `dark` or `light` for one. `--visual-views` includes the issues
board (`board.png`, `board-dropdown.png`, `board-detail.png`), Settings, the
bootstrap screens and the notice bars; `--visual-review` the project
switcher (`sidebar-switcher-mid.png`, `sidebar-switcher.png`), the nav chip
mid-slide (`sidebar-slide-mid.png`) and the palette sliding in
(`palette-entering.png`).
