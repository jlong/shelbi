# Desktop theme

The desktop app paints from its own palette, built on **John's Figma file**
(`Shelbi`, fileKey `pu3hxHgWoCDPCzJ4u6zTQK`; mockups at node `2160:1306`,
the Navigation component at `2220:17016`). The Figma file supersedes the
Taskpage-derived palette and shapes this app used before. The TUI keeps the
shared tokens in `crates/shelbi-app/src/theme.rs` unchanged; the desktop
doesn't read them.

- `src/theme/palette.rs`: the Figma variables (`figma`, a dark and a light
  value each, by their Figma names), the colors Figma draws without a
  variable (`figma::measured`), the roles below, and the contrast tests.
- `src/theme/style.rs`: the Figma text styles (`Type`, applied with
  `.type_style(...)`) and the component shapes every view shares (nav chip,
  buttons, icon buttons, dropdowns, segmented controls, keycaps and the
  sidebar's plain hotkeys, project badges, popovers, dialogs, chips, the
  focus ring and glow, empty states, tooltips).
- `src/theme/motion.rs`: the motion: the easing curve, the spring the
  selection chip slides on, and the fades every hover and press runs.
- `src/theme/mod.rs`: the resolved `Theme` global (views read `theme.muted`,
  `theme.sidebar`, ...), the mapping from the shared view models' meanings
  onto Figma colors, the terminal palette, and gpui-kit's widget colors
  (markdown, inputs) set from the same roles.

Views read roles, never a hue or a hex value. The `theme:` setting (`auto`,
`dark`, `light` in `~/.shelbi/config.yaml`) picks the palette, and `auto`
follows the system appearance live.

## The look

**Dark.** The main surface is a deep blue-slate, `theme.canvas` `#1e2330`.
The sidebar is a lighter tint of it (`#acb4cf` at 10% over the canvas,
`#2c3140`). Cards on the Issues board are near black (`#020303`), so they
read as wells cut into the canvas. Body text is `theme.foreground` (a near
white at 70%), titles `theme.white` (`#f8f8fa`), secondary text
`theme.gray`, quiet text `theme.muted` (`#777a86`). The accent (the
selected nav icon) is cyan `#62cdf6`, as is the focus
ring.

**Light.** The main surface is white, the sidebar a light gray-blue tint
(`#f9f9fc`), cards white with Figma's `shadow/raised card`. Text is near
black (`#020303`) at 70% for body, opaque for titles. The accent is pink
`#f21478`; the focus ring is `#31ade9`.

**Both.** Controls (buttons, dropdowns, filter chips, count badges, the
toolbar buttons) are fill only: `theme.background`, a translucent slate
(`#acb4cf` at 18% dark, 12% light) that tints whatever it sits on, with a
hover and a pressed step 10 points stronger each. There are no control
borders. The selected nav row is the canvas, lifted with `shadow/raised card`
in Light (the Navigation component draws the dark chip flat); the selected
segment is `theme.selection`.

## Figma tokens and roles

`palette.rs`'s `figma` module holds each variable as `get_variable_defs`
returns it on the frames (Orchestrator `2160:1307` / `2220:20936`, Issues
`2165:94` / `2220:20975`, Review `2165:275` / `2220:21066`, Activity,
Issue detail, Command palette, Settings). Some frames override a variable:
the Issues frame's `theme.divider` is `#acb4cf33` (the `divider_strong`
role), the Review frame's `theme.background` reads stronger, and the Issue
detail and Activity frames redefine `theme.search`. The roles take the value
most frames use, and a separate role where an override is a distinct use.

Figma defines the Button component's hover and active fills
(`button/*/background-hover`, `-active`) in its Light mode only, 10 and 20
points over `theme.background`. Dark takes the same steps over its own rest
alpha (18% → 28% → 38%). The Danger label has no Dark value either; Dark
uses `focus/danger/glow`'s pink, `#ff66ab`.

Colors Figma draws without a variable are measured from the 1x frames
(1 px = 1 pt) in `figma::measured`: the sidebar tint, the board's card, the
palette's surface, the Issue detail's edge, and the palette's scrim.

## Type (`src/theme/style.rs`)

The Figma text styles, all Geist (`assets/fonts/geist/` bundles 400, 500,
600 and 700, every weight these need). Views name a style with
`.type_style(Type::...)`; the app root sets `Type::Base`.

| `Type` | Figma style | Size / line | Weight |
|---|---|---|---|
| `Heading` | Desktop/Heading | 20 / 28 | 700 |
| `Base` | Desktop/Base/Regular | 14 / 18 | 400 |
| `BaseMedium` | Desktop/Base/Medium | 14 / 18 | 500 |
| `BaseSemibold` | Desktop/Base/Semibold | 14 / 18 | 600 |
| `Small` | Desktop/Small/Regular | 13 / 18 | 400 |
| `SmallMedium` | Desktop/Small/Medium | 13 / 18 | 500 |
| `XSmall` | Desktop/X-Small/Regular | 12 / 16 | 400 |
| `XSmallMedium` | Desktop/X-Small/Medium | 12 / 16 | 500 |
| `XSmallSemibold` | Desktop/X-Small/Semibold | 12 / 16 | 600 |

## Terminal

- **Font**: Source Code Pro (Figma "Desktop/Terminal" and "Terminal Bold"),
  bundled under `assets/fonts/source-code-pro/` with its OFL license:
  Regular, Bold, Italic, Bold Italic, so SGR bold and italic use real faces.
  The default is 14pt on 18pt rows (`MIN_LINE_HEIGHT` is 18/14, a floor: a
  taller font keeps its own leading). Geist Mono stays bundled as the first
  glyph fallback, then Symbols Nerd Font Mono, then Menlo. Settings picks
  the family from a list of the bundled faces and the installed monospaced
  families (`elements/terminal/fonts.rs`), each row set in its own font.
- **Colors**: the background is `canvas`, the default foreground
  `foreground` (flattened over the canvas: `#b2b4bb` dark), bold text in the
  default foreground `text_loud` (`theme.white`, the frames' bold prompt
  lines), dim (SGR 2) text in it `muted` (`theme.muted`, their tool output
  and footers; dim colored text keeps its hue at 60%), the cursor
  `text_loud`, the selection `text_selection` (`theme.selection`), matches
  `search_match` (`theme.search`). ANSI red, green and cyan are `theme.red`,
  `theme.green` and `theme.cyan` (the terminal frames' agent output: the
  Workspace frame's removed line, its added lines and the Review frame's
  checks, the Orchestrator's notices), yellow, blue, magenta and bright black
  `theme.ansi_*`; the other slots keep Tomorrow Night (dark) and a
  GitHub-light set (light). A program's OSC 10 foreground also colors its
  bold and dim text.
- **Inset**: the grid sits 16px in from the main area's leading and
  trailing edges and 12px from its top and bottom (the Orchestrator and
  Workspace frames); the Review frame's reviewer terminal 12px across
  (`session_view::TERMINAL_INSET_*`). The Workspace frames draw a 1px
  `divider` across the top of the main area.

## Component shapes (`src/theme/style.rs`)

| Component | Shape |
|---|---|
| Corners | 8px (`rounded_lg`) on panel rows and surfaces; 12px on the board's cards; 6px (`NAV_RADIUS`) on the sidebar's rows and chip, buttons, dropdowns and segments (Figma's `6`); 8px on the sidebar's 32px toolbar buttons; 12px on the 32px icon buttons; 4px on keycaps and count badges |
| Project switcher | The Navigation component's trigger: 28px, 8px across, 6px corners, the name (Base medium, `foreground`) and the up-down chevron in the muted color; clear at rest, the hover fill, pressed while open. Its menu is a popover 6px below: rows of badge, name and a check on the current project, a rule, then "Add project…"; ↑↓ / Tab move, Enter picks, Esc or a click outside closes |
| Project badge | Two-letter initials (two words, camel case split), bold white, on a fill picked from the name (`badge_fill`); the switcher's menu rows |
| Toolbar button | The sidebar's titlebar (Search, Toggle sidebar) and footer (Settings): 32px square, 8px corners, the 20px icon in `icon`, the hover fill fading in and a loud icon on hover |
| Keycaps | The Button component's Hotkey: one cap holding the key as written (`s`, `E`, `⌘S`), X-Small in `gray`, 16px tall with 4px corners, on the canvas with a 1px `button_keycap_edge` drop (the card detail's action buttons) |
| Hotkey (sidebar) | The Navigation component's: the chord's keys as plain text (`⌘O`), 2px apart, X-Small in `gray`, 4px either side, no caps |
| Nav row | The Navigation component's: 34px tall, 6px inside, 6px gap, the 20px icon (`icon`; `accent` when selected), the label (Base, `foreground`) growing, the hotkey; the current row is the chip, loud and medium weight |
| Nav chip | The canvas; in Light lifted with `shadow/raised card` (0 4 4, 0 11 0 -8, 0 3 0 -1, and a 1px ring), in Dark flat |
| Shadow blurs | Every Figma (or CSS) blur goes through `style::figma_blur`: Figma's blur is twice the Gaussian's sigma and gpui's `blur_radius` is the sigma, so `shadow/raised card`'s 4 draws as 2, the palette's 15 as 7.5, and the popover and dialog drops likewise |
| Selection chip | One chip for the current row; when the selection moves (keys, clicks, route changes) it slides to the new row on a spring with a slight overshoot before the row takes it back. The sidebar, the review and workspace panels' view switches, and every segmented control |
| List row (leading column) | The review and workspace panels: 32px, Small (13 / 18); the same chip when selected. The sidebar's rows follow the Navigation component (see `views/sidebar.rs`): 8px across, 4px above and below, a 12px state glyph |
| Section heading | Base medium, loud; the sidebar's section headers are Small medium in `muted`, with a count badge (`background`, X-Small medium, 16px) and a 12px chevron |
| Button | The Figma Button (Small): 32px tall, 12px across, 6px corners, Small medium, no boundary. Secondary is the Default style (`background`, loud label); Primary the Success style (`success_fill`, `success_label` label: Figma draws no solid brand button); Danger the Danger style (`danger_fill`, the Danger label); Quiet no fill until hovered. Fills fade on hover and press. Compact actions are X-Small: the card detail's are 26px tall, 8px across, X-Small medium, the icon in a 14px slot and the key as one 16px keycap (`button_keycap_edge` drop); the error log's are 28px with their key as keycaps |
| Focus | Figma's focus state: a 1px `focus_ring` and a 4px `focus_glow` outside the control, the glow painted over the ring (so the ring reads a touch paler and 3px of glow show past it) (the board's keyboard card, a dialog's keyboard button, the task panels' back button on the keyboard's row, text fields) |
| Filter chip | The board toolbar's workflow and workspace filters (`filter_chip`): the Figma Button at Small, label only, 28px, 12px across, 6px corners, Small medium; in the Select style (`select_fill`, `select_label`) while its filter is set, else the Default style (`background`, loud); pressed while its menu is open, and the menu opens 6px under it. Unfiltered the chips read `Workflows` and `Workspaces` |
| Search box | The Searchbox component at Small (`search_box`): 28px, 6px corners, the overlay fill (hover fill under the pointer), the 20px magnifying glass in `icon` 6px in, then the input 6px on, Small, a `Search` placeholder in `form_hint`; focused, the 1px `focus_ring` and the 4px `focus_glow` over it, drawn as bands outside the box (`focus_bands`). The board's is 180px wide |
| Toolbar button | The board's New issue: the Figma Button at Small, Default style, 28px tall, the icon in a 12px Icon slot (`toolbar_button`) |
| Status glyph | Figma's `status-*` drawings, 16px (`status_icon`): backlog a dashed circle and to do an open one in `icon`, in progress half filled in `accent`, review three quarters in `icon_purple`, done filled in `icon_green`, canceled (not in Figma) struck through in `muted`. A status heading (`status_heading`: the board's column headers, the task panels) is the glyph, 8px, then the label in caps at X-Small semibold, loud on the current column and muted otherwise |
| Segmented control | The Segmented Control component (`elements::controls::segmented_track` / `segment`): a `search` track, 4px in, 10px corners; items 24px, 12px across, Base in `form_text`, a fill fading in on hover; the selected one Base medium and loud on the canvas with `shadow/raised card`'s drops (`selected_segment`), which slides between items. Activity's kind filter is its own (below) |
| Activity filters | The Activity frame's own: kind segments in a bare row 4px apart (no track), 24px, 8px across, X-Small, muted, the selected one `selection` and loud; then, 16px on, the pickers: `search`'s fill, a 14px icon (the small Heroicons, `icons::FUNNEL_SMALL`, `USER_SMALL`: 1.5px stroke at 14px, as Figma draws them), 8px, an X-Small label, no chevron, icon and label in `log_text` (`filter_segment`, `filter_dropdown`) |
| Icon button | 24 or 32px square, the icon role at rest, the hover fill fading in and a loud icon on hover, the overlay fill while active |
| Labeled control | The Labeled Control component (`labeled_control`): the label (Base, loud), the control, then an optional helper (X-Small, `form_hint`), 6px apart, at most 640px wide. Every Settings row |
| Textbox | The Textbox component (`textbox`): 32px, the `textbox` well (5px corners, 8px across) inside a 1px `input_border` (6px corners), Base in `form_text`, a placeholder in `form_hint`; focused, the edge is `focus_ring` with a 3px `focus_glow` band just outside it, and the caret is `focus_ring` |
| Picker | The Picker component (`picker`): a growing textbox, 8px, a form button; 400px. The font picker ("Choose font") and the Shelbi CLI ("Browse") |
| Form button | `form_button`: the Button component's Default style at the Base size: 32px, 12px across, 6px corners, Base medium, loud, on `background`. `form_icon_button`: its 32px square Icon Button, the 20px icon in `button_icon` (the size stepper's − and +) |
| Checkbox | The Check, Checkbox and Checkbox List components (`check`, `checkbox`, `checkbox_list`): a 12px box with 2px corners stroked 1px outside (as Figma strokes it: 14px edge to edge, 3px corners), unchecked the `textbox` well in an `input_border` edge, checked `check_fill` with the 12pt mark in white; the label Base, loud when checked and `form_text` when not, 8px on, 2px in; focused, the edge turns `focus_ring` (`check_fill` checked) with a 3px glow around it (`focus_glow`, `check_glow` checked). The list stacks them 4px apart, 4px in |
| Menu, popover | `raised`, 8px corners, a 1px `ring` and a soft drop |
| Palette | The Command palette frame: over the `scrim`, 200px down, 788px wide (a 580px commands column, a 160px Projects column, 16px between and either side); `raised`, 8px corners, no ring, `palette_shadow` (0 10 15 -3). A 44px query line (`❯`, a 2x18px `link` caret, the placeholder `muted`), 32px rows (6px across and between, 6px corners: a 20px `icon`, the name in a 220px column in Base medium `foreground`, the description in Base `muted`; the selected row on `background`, its name Base semibold `text_loud`, its description `foreground`), 26px project rows, an X-Small `muted` footer |
| Dialog | Over the `scrim`, `raised`, 8px corners, a 1px ring and a deeper drop |
| Card | `card` with `shadow/raised card`, 12px corners, 12px inside, 6px between the title (Base medium, two lines), the excerpt (X-Small on an 18px line, `foreground`, four lines, 244px wide) and the meta row (the workflow badge, 4px across with 2px corners, then the branch in `muted`); the keyboard's card wears the focus ring and glow instead of the shadow |
| Chip, badge | 20px, 6px across, X-Small medium, the hue's text on its tint; a workflow name is `workflow_badge_text` on `workflow_badge` |
| Zen pill | 42px, the sidebar's width inside 8px, fully rounded: `zen` (the Success fill) over the canvas with a `success` (`text/green`) "Zen mode on"; off, `background` over the canvas with "Zen mode off" in `foreground`. A click toggles Zen |
| Empty state | A muted sentence on why there is nothing, and the action that changes it |
| Tooltip | A small popover, X-Small |
| Terminal | Background and frame are the canvas; the find bar and "Jump to bottom" are popovers |
| Spacing | Figma's scale (2, 4, 6, 8, 12, 16) |

The screens' layouts (row heights, paddings, which style each label takes)
follow the Figma frames screen by screen in the later `dt-figma-*` tasks;
this foundation sets the palette, the type styles and the shared shapes.

## Motion (`src/theme/motion.rs`)

- **Menus and dialogs** open with a slide-and-fade: 8px of travel up into
  place and opacity, on `cubic-bezier(0.16, 1, 0.3, 1)`: 200ms for the
  project switcher's menu (in and out) and the board's and Activity's filter
  menus (in), 400ms for the palette and dialogs, their scrim fading with
  them.
- **The selection chip** slides between rows on a damped spring (damping
  ratio 0.75 at 22 rad/s: framer-motion's `bounce: 0.25`), reaching its row
  in about 200ms with a 3% overshoot and settling by 350ms. A retarget
  mid-slide starts from where the chip is, with its velocity.
- **Hover and press** fade a control's fill over about 200ms (a critically
  damped spring, no overshoot).
- **Reduce Motion** (System Settings ▸ Accessibility ▸ Display) makes all of
  it instant. The app hands the setting to gpui at launch and each time a
  window comes forward, so gpui's own animations end where they would; the
  chip and the menus read it when they start.

## Shared meanings

The shared view models name colors by meaning (the activity feed's `Tone`, a
badge's `DecorationColor`). `Theme::tone` and
`Theme::decoration` map each onto the Figma color for the same meaning:
Activity's log colors (`theme.ansi_*`) for tones, the Navigation component's
status icons for sidebar badges.

| Meaning (TUI color) | Tone role | Badge role |
|---|---|---|
| Done, working (green) | `green` (`[done]`) | `icon_green` |
| Ready for review (cyan) | `cyan` | `icon_cyan` |
| In progress, awaiting input (yellow) | `yellow` (`[in progress]`) | `icon_yellow` |
| Error, blocked, a failure (red, light red) | `red` | `red` |
| Queued, the Ready category (blue) | `blue` (`[ready]`) | `blue` |
| Review, handoff (magenta) | `magenta` (`[review]`) | |
| Dim, a timestamp (dark gray) | `dark_gray` | `muted` |
| Normal (gray) | `foreground` | `foreground` |
| Emphasis (white) | `text_loud` | |

## Roles

Every role with its dark and light value. A translucent value is
`#rrggbbaa`. `the_theme_doc_lists_every_role` in `src/theme/palette.rs` fails
until a new role has its row here, with the values the palette really has.

### Surfaces

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `canvas` | `#1e2330` | `#ffffff` | `theme.canvas`: the main area, the terminal's background |
| `sidebar` | `#2c3140` | `#f9f9fc` | Measured (the Navigation fill): the sidebar, the review and workspace panels |
| `card` | `#020303` | `#ffffff` | Measured (Issues): a board card |
| `raised` | `#2c3140` | `#f9f9fc` | Measured (Command palette): the palette, menus, popovers, tooltips |
| `well` | `#1e2330` | `#ffffff` | `theme.canvas`: the bootstrap screen's command box |
| `toolbar` | `#acb4cf1a` | `#acb4cf14` | `surface/depth-10` (the Issues Header): the board toolbar's band over the canvas |
| `textbox` | `#020303` | `#ffffff` | The Textbox component's well (the dark Settings frame draws `#020303`; white in Light), and an unchecked box's fill |

### Overlay fills

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `background` | `#acb4cf2e` | `#acb4cf1f` | `theme.background`: buttons, dropdowns, chips, count badges, the Zen pill off, a hovered row |
| `background_hover` | `#acb4cf47` | `#acb4cf38` | `button/default/background-hover` (dark derived): a control's hover |
| `background_active` | `#acb4cf66` | `#acb4cf52` | `button/default/background-active`: pressed, or open |
| `select_fill` | `#62cdf62e` | `#62cdf61f` | `button/select/background` (`Cyan/400` at the overlay's alphas): a set filter's chip on the board toolbar |
| `select_fill_hover` | `#62cdf647` | `#62cdf638` | `button/select/background-hover`: that chip hovered |
| `select_fill_active` | `#62cdf666` | `#62cdf652` | `button/select/background-active`: that chip pressed, or its menu open |
| `select_label` | `#b0e6fb` | `#1989c4` | `button/select/text` (`Cyan/300` / `Cyan/600`): the set filter chip's label |
| `selection` | `#acb4cf8c` | `#acb4cf66` | `theme.selection`: the selected segment |
| `search` | `#acb4cf40` | `#acb4cf29` | `theme.search`: Activity's filter chips |
| `keycap` | `#1e2330` | `#ffffff` | The Hotkey's fill (`theme.canvas`) |
| `button_keycap_edge` | `#0203038c` | `#acb4cf99` | The Hotkey's 1px drop inside a button (the Issue detail frame's `theme.search`) |

### Text

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `text_loud` | `#f8f8fa` | `#020303` | `theme.white`: titles, the selected row, a workspace name |
| `foreground` | `#f2f3f7b2` | `#020303b2` | `theme.foreground`: body text and labels |
| `gray` | `#acb4cfe5` | `#7b85a7e5` | `theme.gray`: hotkeys, secondary text |
| `form_text` | `#f2f3f7d9` | `#020303b2` | `theme.foreground` as the Settings frame and the form components set it: a textbox's value, an unselected segment or checkbox |
| `form_hint` | `#f2f3f78c` | `#444c6580` | `theme.muted` as the Settings frame and the form components set it (`text/muted` in the Issues Header): a labeled control's helper, a textbox's placeholder, the board toolbar's `Filter:` and search placeholder |
| `muted` | `#777a86` | `#444c65a6` | `theme.muted`: section headers, branches, hints, idle, placeholders |
| `text_on_color` | `#ffffff` | `#ffffff` | A project badge's initials |
| `log_text` | `#7b85a7` | `#7b85a7` | `theme.gray` as the Activity frame sets it (opaque, both modes): the log's titles, the `you` actor, the pickers' icon and label |
| `table_text` | `#7b85a7` | `#7b85a7` | `theme.gray` as the Machines frame sets it (opaque, both modes): a workspace row's runner and task |

### Icons

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `icon` | `#acb4cfe5` | `#7b85a7e5` | `theme.gray`: icons at rest |
| `button_icon` | `#acb4cf99` | `#7b85a7e5` | The Button component's icon stroke (New issue, the issue detail's actions): `#acb4cf` at 60% in Dark, `theme.gray` in Light |
| `accent` | `#62cdf6` | `#f21478` | `icon/select`: the selected nav icon, the current project's check |
| `icon_cyan` | `#62cdf6` | `#31ade9` | `icon/cyan`: ready for review |
| `icon_green` | `#31d924` | `#5ac351` | `icon/green`: a working workspace |
| `icon_yellow` | `#ffdd64` | `#ffca0d` | `icon/yellow`: awaiting input |
| `icon_purple` | `#e374ff` | `#b247e7` | `icon/purple`: a review status |

### Rules

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `divider` | `#acb4cf1a` | `#acb4cf1a` | `theme.divider`: dividers and hairlines |
| `divider_strong` | `#acb4cf33` | `#acb4cf33` | The Issues frame's `theme.divider`: the board's rules |
| `input_border` | `#acb4cf4d` | `#acb4cf80` | `theme.divider` as the Settings frame and the form components set it: a textbox's and an unchecked box's edge |

### Links and focus

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `link` | `#30c6ff` | `#1989c4` | `text/link`: links, More |
| `link_hover` | `#f8f8fa` | `#020303` | `theme.white`: a hovered link |
| `focus_ring` | `#62cdf6` | `#31ade9` | `focus/default/ring`: the 1px focus ring, the find caret, drop targets |
| `focus_glow` | `#12adff33` | `#12adff33` | `focus/default/focus-glow`: the 4px glow outside it |
| `check_fill` | `#31ade9` | `#f21478` | `checkbox/select/background` and `/border`: a checked box (Dark from the Settings frame, Light from the components page), and a focused checked box's ring |
| `check_glow` | `#12adff33` | `#ff66ab33` | `checkbox/select/focus-glow` (Light); Figma has no Dark value, so Dark keeps `focus/default/focus-glow` |

### Meaning

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `success` | `#31d924` | `#00753d` | `text/green`: done, approve, the Zen pill's label |
| `warning` | `#dcb767` | `#8a5f04` | `theme.ansi_yellow`: in progress, stale data |
| `info` | `#4a8fd7` | `#2563c9` | `theme.ansi_blue`: ready, informational |
| `error` | `#ff66ab` | `#b40041` | The Danger label: errors, blocked, a version mismatch |

### Log and terminal hues

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `theme_green` | `#5acd25` | `#367b16` | `theme.green`: ANSI green in the terminal |
| `theme_cyan` | `#00a6b2` | `#007a83` | `theme.cyan`: ANSI cyan in the terminal |
| `theme_red` | `#e04f52` | `#c23b3e` | `theme.red`: ANSI red in the terminal |
| `green` | `#5fb56d` | `#1a7f37` | `theme.ansi_green`: `[done]` |
| `yellow` | `#dcb767` | `#8a5f04` | `theme.ansi_yellow`: `[in progress]` |
| `blue` | `#4a8fd7` | `#2563c9` | `theme.ansi_blue`: `[ready]`, queued |
| `magenta` | `#c586c0` | `#a21caf` | `theme.ansi_magenta`: `[review]`, handoff |
| `cyan` | `#4ec9b0` | `#0f766e` | `theme.ansi_cyan`: a working actor in the log |
| `dark_gray` | `#7c7c7c` | `#6b6b6b` | `theme.ansi_dark_gray`: timestamps, ANSI bright black |
| `red` | `#ff66ab` | `#b40041` | The Danger label: failures and errors as text |

### Chip tints and badges

Each tint is its hue at `theme.background`'s opacity, the rule the Button
component's Success and Danger styles follow.

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `green_tint` | `#5fb56d2e` | `#1a7f371f` | A green chip's background |
| `yellow_tint` | `#dcb7672e` | `#8a5f041f` | A yellow chip's background |
| `cyan_tint` | `#4ec9b02e` | `#0f766e1f` | A cyan chip's background |
| `blue_tint` | `#4a8fd72e` | `#2563c91f` | A blue chip's background |
| `magenta_tint` | `#c586c02e` | `#a21caf1f` | A magenta chip's background |
| `red_tint` | `#ff66ab2e` | `#b400411f` | A red chip's background |
| `workflow_badge` | `#3a4258` | `#d3d8e4` | `theme.workflow_badge_bg`: a card's workflow name, a neutral chip |
| `workflow_badge_text` | `#dcdfe8` | `#2a3142` | `theme.workflow_badge_fg`: its label |

### Buttons

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `success_fill` | `#5ac3512e` | `#5ac3511f` | Button Success: the primary button |
| `success_fill_hover` | `#5ac35147` | `#5ac35138` | `button/success/background-hover` (dark derived) |
| `success_fill_active` | `#5ac35161` | `#5ac35152` | `button/success/background-active` (dark derived) |
| `success_label` | `#86d580` | `#00753d` | Button Success's label and icon: `text/green` in Light, `#86d580` in the dark Review frame |
| `danger_fill` | `#f214782e` | `#f214781f` | Button Danger: the danger button, the unread-errors button |
| `danger_fill_hover` | `#f2147847` | `#f2147838` | `button/danger/background-hover` (dark derived) |
| `danger_fill_active` | `#f2147861` | `#f2147852` | `button/danger/background-active` (dark derived) |

### Bands

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `zen` | `#5ac3512e` | `#5ac3511f` | The Zen pill (the Success fill; measured `#294036` / `#ebf7ea` over the canvas) |
| `notice` | `#ffdd642e` | `#ffca0d1f` | `icon/yellow` at `theme.background`'s opacity: the relaunch band |

### Selection

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `nav_selected` | `#1e2330` | `#ffffff` | `theme.canvas`: the selected nav chip (it slides) |
| `row_selected` | `#acb4cf2e` | `#acb4cf66` | Activity's selected row: `theme.background` dark, `theme.selection` light, as drawn |
| `text_selection` | `#acb4cf8c` | `#acb4cf66` | `theme.selection`: the terminal's selection |
| `search_match` | `#acb4cf40` | `#acb4cf29` | `theme.search`: a find match |
| `search_current` | `#ffdd6466` | `#ffca0d66` | `icon/yellow` at 40% (Figma draws no current match) |

### Elevation

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `ring` | `#acb4cf1a` | `#0a1e5108` | Measured (Issue detail's edge): the 1px ring around raised surfaces |
| `shadow` | `#0a1e5105` | `#0a1e5105` | `shadow/raised card`'s drops (menus, popovers and dialogs too) |
| `shadow_ring` | `#0a1e5108` | `#0a1e5108` | `shadow/raised card`'s 1px ring |
| `palette_shadow` | `#00000040` | `#00000040` | The Command palette's drop effect (`0 10 15 -3`): the palette over the scrim |
| `scrim` | `#02030359` | `#444c65a6` | Measured (Command palette): the backdrop behind the palette and dialogs |

### Project badges

Not roles: the same eight fills in both themes, picked per project by
`badge_fill` (an FNV-1a hash of the name), each dark enough for white
initials at 4.5:1: `#db0a60`, `#2830f1`, `#7019c0`, `#00753d`, `#0c699c`,
`#9d4100`, `#aa0101`, `#856700`. The Figma Navigation component draws the
project name without a badge, so the switcher's trigger has none; its menu's
rows keep their initials.

## Contrast

`src/theme/palette.rs` checks every pairing the desktop draws, in both
themes: the loud, body, gray and muted text on the canvas, sidebar, card and
raised surfaces and on a hovered or selected row over them; every meaning
and log hue as text on the surfaces; every chip's text on its tint; a
workflow badge; each button style's label on its rest, hover and pressed
fill; the selected segment's label; a keycap's label; the Zen pill's label.
Text is held to WCAG AA (4.5:1). Icons, the focus ring, a text field's edge
and the selected nav icon are held to 3:1 (WCAG 1.4.11). `src/theme/mod.rs`
checks the terminal's default foreground at AA and its ANSI hues at 3:1 on
the canvas. `project_badges_carry_white_initials` checks every badge fill.

### Exceptions

The design keeps these Figma values where they fall short; nothing is
shifted away from Figma to pass. Each entry covers every pairing whose name
starts with it. `contrast_shortfalls_are_exactly_the_documented_exceptions`
fails on a shortfall no entry covers and on an entry that covers nothing,
and `the_theme_doc_lists_every_contrast_exception` checks each row here,
with its worst ratio. **Flagged for John.**

| Theme | Pairing | Worst | Why |
|---|---|---|---|
| dark | muted on | 1.11 | `theme.muted` `#777a86`: 3.67 on the canvas, 3.03 on the sidebar, 2.10 on a hovered row there. Section headers, branches, idle labels. 1.11 is an idle state on the selected Machines row (`theme.selection`), a row the frame doesn't draw selected |
| dark | dark_gray on | 1.13 | `theme.ansi_dark_gray`: Activity's timestamps, 3.76 on the canvas, 3.10 on the sidebar. 1.13 is the branch on the selected Machines row, as the frame draws it |
| dark | log_text on | 2.53 | The Activity frame's `theme.gray` `#7b85a7`: the log's titles, 4.30 on the canvas, 2.96 on the selected row; the pickers' labels, 2.53 on their `search` fill |
| dark | info on | 3.83 | `theme.ansi_blue` as the info color: AA on the canvas (4.7), short on the sidebar and raised surfaces |
| dark | blue on | 3.61 | The same blue as `[ready]`, and on its chip tint |
| dark | gray on background | 3.84 | `theme.gray` (hotkeys) on a hovered row over the sidebar |
| dark | gray on row_selected | 3.84 | `theme.gray` on the selected row over the sidebar |
| dark | magenta on magenta_tint | 4.19 | `[review]` on its (derived) chip tint |
| dark | red on red_tint | 4.39 | The Danger pink on its (derived) chip tint |
| dark | text_loud on selection | 4.08 | `theme.white` on the selected segment (`theme.selection`) over the sidebar; 4.46 on the canvas |
| dark | theme_green on selection | 2.30 | The same row: `working` and `● reachable` in `theme.green` |
| dark | theme_cyan on selection | 1.60 | The same row: a review slot's `review` in `theme.cyan`. The lowest pairing the desktop draws |
| dark | success_label on hover button | 4.35 | the Success label (`#86d580`) on the Success button's (derived dark) hover |
| dark | success_label on pressed button | 3.58 | the Success label on the Success button pressed |
| dark | error on rest button | 4.30 | The Danger label on the Danger fill over the sidebar (AA over the canvas) |
| dark | error on hover button | 3.94 | The same, hovered |
| dark | error on pressed button | 3.53 | The same, pressed |
| dark | button_icon on | 2.58 | The Button component's icon (`#acb4cf` at 60%) on a button's fill over the sidebar; decorative beside the button's label |
| dark | input_border on | 1.76 | `theme.divider` (`#acb4cf` at 30%) as a textbox's edge (Settings); the textbox also differs by its near-black well and its label |
| dark | text_on_color on check_fill | 2.53 | The check mark (white) on the dark checked box (`#31ade9`), as the Settings frame draws it; the box itself is 6.19 on the canvas |
| dark | form_hint on search box | 3.69 | `text/muted` (`#f2f3f7` at 55%) as the board search's `Search` placeholder, on the Searchbox fill over the toolbar band; AA on the band itself |
| dark | select_label on | 3.98 | `button/select/text` (`Cyan/300`) on a set filter chip pressed (its menu open); AA at rest and hovered |
| dark | table_text on | 1.30 | The Machines frame's `theme.gray` `#7b85a7`: a row's runner and task, 4.30 on the canvas, 1.30 on the selected row (`theme.selection` over the canvas), as the frame draws it |
| light | muted on | 2.95 | `theme.muted` (`#444c65` at 65%): 3.44 on white, 2.95 on the selected row |
| light | gray on | 2.39 | `theme.gray` (`#7b85a7` at 90%): 3.12 on white; hotkeys, keycaps, secondary text |
| light | log_text on | 2.80 | The same `#7b85a7` in Light: 3.65 on white, 2.80 on the selected row |
| light | icon on | 2.80 | Icons in `theme.gray` on the sidebar (3.12 on white); 2.80 for the board search's magnifying glass on the Searchbox fill over the toolbar band |
| light | link on | 3.69 | `text/link` `#1989c4`: 3.88 on white |
| light | green on green_tint | 4.30 | `[done]` on its (derived) chip tint |
| light | success_label on pressed button | 4.36 | `text/green` on the Success button pressed |
| light | error on pressed button | 3.98 | `#b40041` on the Danger button pressed |
| light | focus_ring on | 2.41 | `focus/default/ring` `#31ade9`: 2.53 on white; the 4px glow widens it |
| light | dark_gray on selection | 4.08 | The selected Machines row: the branch in `theme.ansi_dark_gray` |
| light | theme_green on selection | 4.02 | The same row: `working` in `theme.green` (5.0 on white) |
| light | theme_cyan on selection | 3.91 | The same row: `review` in `theme.cyan` |
| light | button_icon on | 2.80 | `theme.gray` as the Button component's icon, on a button's fill (2.80 for New issue's plus over the toolbar band); decorative beside the label |
| light | input_border on | 1.38 | `theme.divider` (`#acb4cf` at 50%) as a textbox's edge |
| light | form_hint on | 2.37 | The form components' `theme.muted` (`#444c65` at 50%): a helper line, a placeholder; the board toolbar's `Filter:` and its search's placeholder (2.37 on the Searchbox fill over the band) |
| light | select_label on | 3.08 | `button/select/text` (`Cyan/600` `#1989c4`) on a set filter chip over the toolbar band: 3.45 at rest, 3.25 hovered, 3.08 pressed, as the Header draws it |
| light | table_text on | 2.80 | The same `#7b85a7`: 3.65 on white, 2.80 on the selected row |

## Renders

Every `--visual-*` mode renders both themes: with `SHELBI_VISUAL_THEME`
unset it runs itself once per theme into `<dir>/dark/` and `<dir>/light/`;
set it to `dark` or `light` for one. `--visual-views` includes the issues
board (`board.png`, `board-dropdown.png`, `board-detail.png`), Settings, the
bootstrap screens and the notice bars; `--visual-review` the project
switcher (`sidebar-switcher-mid.png`, `sidebar-switcher.png`), the nav chip
mid-slide (`sidebar-slide-mid.png`) and the palette sliding in
(`palette-entering.png`).
