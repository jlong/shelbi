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
  sidebar's plain hotkeys, popovers, dialogs, chips, the
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

**Dark.** The main surface is a deep blue-slate, `surface/background`
`#1e2330`. The sidebar is a lighter tint of it (`#acb4cf` at 10% over the
canvas, `#2c3140`). Cards on the Issues board are near black
(`surface/depth-0`, `#020303`), so they read as wells cut into the canvas.
Body text is `text/foreground` (`#f2f3f7` at 85%), titles `text/bold`
(`#f2f3f7`, opaque), secondary text and icons `icon/primary`, quiet text
`text/muted` (`#f2f3f7` at 55%). The accent (the selected nav icon) is cyan
`#62cdf6`, as is the focus ring.

**Light.** The main surface is white, the sidebar a light gray-blue tint
(`#f9f9fc`), cards white with Figma's `shadow/raised card`. Text is near
black (`#020303`) at 70% for body, opaque for titles, `#444c65` at 50% for
quiet text. The accent is pink `#f21478`; the focus ring is `#31ade9`.

**Both.** Controls (buttons, dropdowns, filter chips, count badges, the
toolbar buttons) are fill only: `button/default/background`, a translucent
slate (`#acb4cf` at 18% dark, 12% light) that tints whatever it sits on,
with a hover and a pressed step (28% / 40% dark, 22% / 32% light). There are
no control borders. The selected nav row is the canvas, lifted with
`shadow/raised card` in Light (the Navigation component draws the dark chip
flat); the selected segment is `surface/selection`.

## Figma tokens and roles

`palette.rs`'s `figma` module holds the `03 Desktop` variable collection
(`VariableCollectionId:2162:2`; modes Light `2162:0` and Dark `2162:1`;
the Shelbi Colors page `2196:7154`), one Dark and one Light value per
constant, each documented with its semantic Figma name: `surface/background`,
`surface/depth-*`, `surface/selection`, `surface/overlay`, `text/*`,
`icon/*`, `stroke/*`, `button/*`, `focus/*` and `checkbox/*`. Read from the
file with both modes resolved on 2026-10-22. The constants keep their older
Rust names (`WHITE` is `text/bold`, `GRAY` is `icon/primary`, `DIVIDER_FORM`
is `stroke/dark`, `SEARCH` is `surface/depth-30`, `SEARCH_DETAIL` is
`shadow/hard`); the roles views read are unchanged.

Every value is the source RGB with straight alpha, never a color flattened
onto a background. Figma stores an opacity as a 32-bit float, and a byte
here is that float times 255, rounded: 70% is `b2` and 90% is `e5`, as
Figma's hex reads them, while 50% is `80`.
`token_alphas_are_figmas_opacities` holds every percentage-defined token
to that.

Each Button style keeps its own roles even where the values coincide:
Default (`#acb4cf`), Primary and Select (`Cyan/400`), Success (`Green/500`)
and Danger (`Pink/500`) all step 18% → 28% → 40% in Dark and
12% → 22% → 32% in Light. Every style defines its Dark hover, active and
label (`button/danger/text` is `Pink/400` `#ff66ab`, `button/success/text`
`Green/400` `#86d580`). `button/default/icon` is its own token, opaque in
both modes, louder than `icon/primary` (90%, the `icon` role) and
`icon/secondary` (60%); only button icons take it.

`checkbox/default/border` (an unchecked box's edge, `check_border`) is
`#acb4cf` at 20% in Dark and 50% in Light, and `stroke/dark` (a textbox's
edge, `input_border`) 30% and 50%. They stay two roles. `checkbox/select/*`
defines both modes, its Dark focus glow included.

Colors with no `03 Desktop` variable live in `figma::measured`: the
sidebar tint (the Navigation fill), the palette's raised surface and the
Issue detail's edge, measured from the 1x frames (1 px = 1 pt). Two more are
frame bindings to variables outside the collection, kept at the frames'
values: the Activity and Machines frames' `text/gray` (`#7b85a7` opaque,
the `log_text` and `table_text` roles) and the cards'
`other/workflow_badge_*`. `text/foreground` and `text/muted` used to differ
by frame (the Settings frame's 85% / 55% against an older 70% and an opaque
`#777a86`); the collection now defines one value each, so `form_text` and
`form_hint` read the shared tokens and stay roles of their own only so form
controls can diverge again.

The dark terminal's ANSI sixteen are `04 Terminal`'s (below), not the
`text/ansi_*` tokens, which color the Activity log and the light terminal's
borrowed slots.

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
  `foreground` (`text/foreground`), bold text in the default foreground
  `text_loud` (`text/bold`, the frames' bold prompt lines), dim (SGR 2) text
  in it `muted` (`text/muted`, their tool output and footers; dim colored
  text keeps its hue at 60%), the cursor `text_loud`, the selection
  `text_selection` (`surface/selection`), matches `search_match`
  (`surface/depth-30`). A terminal cell has no alpha, so
  `TerminalPalette::for_palette` composites each translucent role over the
  canvas once, when the theme resolves: the dark foreground is `#f2f3f7` at
  85% over `#1e2330`, `#d2d4d9`; dim text `#f2f3f7` at 55%, `#92959d`; bold
  `#f2f3f7` as is; in Light, dim text is `#444c65` at 50% over white,
  `#a1a5b2`. A change to the shared text tokens reaches the terminal with no
  terminal-side edit. In dark the ANSI sixteen are `04 Terminal`'s
  `ansi/regular/*` and `ansi/bold/*` variables (the Settings ▸ Terminal
  frame's), the Sensible palette (`theme::SENSIBLE`), unchanged by the
  `text/ansi_*` tokens. In light, red, green and cyan are `other/red`,
  `other/green` and `other/cyan`, yellow, blue, magenta and bright black
  `text/ansi_*`, and the other slots keep a GitHub-light set. The user's own colors (Settings ▸ Terminal,
  `desktop.terminal_colors`) go over either. A program's OSC 10 foreground
  also colors its bold and dim text.
- **Inset**: the grid sits 16px in from the main area's leading and
  trailing edges and 12px from its top and bottom (the Orchestrator and
  Workspace frames); the Review frame's reviewer terminal 12px across
  (`session_view::TERMINAL_INSET_*`). The Workspace frames draw a 1px
  `divider` across the top of the main area.

## Component shapes (`src/theme/style.rs`)

| Component | Shape |
|---|---|
| Corners | 8px (`rounded_lg`) on panel rows and surfaces; 12px on the board's cards; 6px (`NAV_RADIUS`) on the sidebar's rows and chip, buttons, dropdowns and segments (Figma's `6`); 8px on the sidebar's 32px toolbar buttons; 12px on the 32px icon buttons; 4px on keycaps and count badges |
| Project switcher | The Navigation component's trigger: 28px, 8px across, 6px corners, the name (Base medium, `foreground`) and the up-down chevron in the muted color; clear at rest, the hover fill, pressed while open. Its menu (the component's open state, 2186:1692) floats 4px below: the `search` tint over the sidebar's color, 6px corners, 4px across and 8px down, no drop; the project list below; ↑↓ / Tab move, Enter picks, Esc or a click outside closes |
| Project list | The switcher's menu and the palette's Projects column (`views/project_menu.rs`): a "Projects" heading (Base medium, `muted`), 26px rows 8px in with 4px gaps (Base, `foreground`): a 16px dot (filled `icon_green` while the project is open, a `gray` ring when it isn't, decided by `shelbi_app::command_model::project_rows`), the name, and a 16px heavy check in `gray` on the current project; an 8px-clear 1px `divider`; then "+ Add project" (a 16px plus in `gray`). No avatars |
| Toolbar button | The sidebar's titlebar (Search, Toggle sidebar) and footer (Settings): 32px square, 8px corners, the 20px icon in `icon`, the hover fill fading in and a loud icon on hover |
| Keycaps | The Button component's Hotkey (`button_hotkey`): one cap holding the key as written (`s`, `E`, `⌘S`), X-Small in `gray`, 16px tall, 4px across, 4px corners, on the canvas with a 1px `button_keycap_edge` drop (the issue detail's actions) |
| Hotkey (sidebar) | The Navigation component's: the chord's keys as plain text (`⌘O`), 2px apart, X-Small in `gray`, 4px either side, no caps |
| Nav row | The Navigation component's: 34px tall, 6px inside, 6px gap, the 20px icon (`icon`; `accent` when selected), the label (Base, `foreground`) growing, the hotkey; the current row is the chip, loud and medium weight |
| Nav chip | The canvas; in Light lifted with `shadow/raised card` (0 4 4, 0 11 0 -8, 0 3 0 -1, and a 1px ring), in Dark flat |
| Shadow blurs | Every Figma (or CSS) blur goes through `style::figma_blur`: Figma's blur is twice the Gaussian's sigma and gpui's `blur_radius` is the sigma, so `shadow/raised card`'s 4 draws as 2, the palette's 15 as 7.5, and the popover and dialog drops likewise |
| Selection chip | One chip for the current row; when the selection moves (keys, clicks, route changes) it slides to the new row on a spring with a slight overshoot before the row takes it back. The sidebar, the review and workspace panels' view switches, and every segmented control |
| List row (leading column) | The review and workspace panels: 32px, Small (13 / 18); the same chip when selected. The sidebar's rows follow the Navigation component (see `views/sidebar.rs`): 8px across, 4px above and below, a 12px state glyph |
| Section heading | Base medium, loud; the sidebar's section headers are Small medium in `muted`, with a count badge (`background`, X-Small medium, 16px) and a 12px chevron |
| Button | The Figma Button component (`2220:17600`) at an explicit `ButtonSize`, every size Geist medium with 6px corners and no boundary: **X-Small** 24px, X-Small medium (12 / 16), 8px across, 4px between, an 18px glyph (the `_BUTTON` drawings) in a 14px slot; **Small** 28px, Small medium (13 / 18), 12px across, 4px between, a 20px glyph in a 12px slot; **Base** 32px, Base medium (14 / 18), 12px across, 6px between, a 20px glyph in a 14px slot. Content (`ButtonContent`) is the component's order: left icon, label, Hotkey, right icon; a hidden piece takes no gap. The left slot is flush right and the right slot flush left, so each glyph hangs past the slot's outer edge. Icons wear the style's icon role (`ButtonColors::icon`): the Default style `button_icon` (`button/default/icon`), the tinted styles their label's color. Disabled (`disabled_button`, `disabled_content_button`) keeps the enabled size and dims the whole control to 40% once. Styles: Secondary is the Default style (`background`, loud label); Primary the Primary style (`primary_fill`, `primary_label`), a dialog's confirming action; Success the Success style (`success_fill`, `success_label`); Danger the Danger style (`danger_fill`, the Danger label); Select the Select flag (`select_fill`, `select_label`); Ghost (`2565:13261`) no fill at rest, `background` on hover, `background_hover` pressed, a loud label and the Default icon gray, its Focus resting on the hover fill (`ButtonColors::focus_fill`) under the default ring, its Select the Select style (a picker's value while its picker is open), its Disable the 40% one; dialogs' Cancel, the pane's Done / Retry, and the task pane's property values. Fills fade on hover and press. Dialogs and forms use Base, headers, toolbars and rows Small, the task panels' Open X-Small |
| Focus | Figma's focus state: a 1px `focus_ring` and a 4px `focus_glow` outside the control, the glow painted over the ring (so the ring reads a touch paler and 3px of glow show past it) (the board's keyboard card, a dialog's keyboard button, the task panels' back button on the keyboard's row, text fields). A focused button wears its style's focus (`ButtonColors::focus`): Success the green `success_focus_ring` and `success_focus_glow`, Danger the pink `danger_focus_ring` and `danger_focus_glow`, Primary, Default, Select and Ghost the default cyan (Select has no Figma focus variant; Ghost's Focus also rests on its hover fill, the one style whose focus changes its fill); every other control keeps the default. Focus adds the ring and nothing else: a button keeps its rest, hover or pressed fill and its label, and the bands sit outside its bounds, so its layout and hit area don't move. A disabled button (40%) takes no hover, press or focus. On a translucent control the ring is drawn as bands (`focus_bands`, `focus_bands_in` for a style's colors), never as a shadow, which gpui paints as a filled box under the control (Figma's own Focus variants swap the Primary label to `button/default/text`; the app doesn't). The widths are the Button component's 12 Focus variants' (a 1px and a 4px outside stroke on 6px corners around the control's own bounds); the `focus/ring-width` variable says 2, but no variant binds it, so the app follows the drawn geometry |
| Filter chip | The board toolbar's workflow and workspace filters (`filter_chip`): the Figma Button at Small, label only, 28px, 12px across, 6px corners, Small medium; in the Select style (`select_fill`, `select_label`) while its filter is set, else the Default style (`background`, loud); pressed while its menu is open, and the menu opens 6px under it. Unfiltered the chips read `Workflows` and `Workspaces` |
| Search box | The Searchbox component at Small (`search_box`): 28px, 6px corners, the overlay fill (hover fill under the pointer), the 20px magnifying glass in `icon` 6px in, then the input 6px on, Small, a `Search` placeholder in `form_hint`; focused, the 1px `focus_ring` and the 4px `focus_glow` over it, drawn as bands outside the box (`focus_bands`). The board's is 180px wide. While the query is non-empty, a clear button (`search_clear`) at its right end: the 20px x-mark as a 24px `icon_button` (the `icon` role, `text_loud` and the hover fill fading in under the pointer), hung 2px into the padding so the mark sits 6px in like the glass; the field shrinks to make room. Figma's Searchbox draws no clear button |
| Toolbar button | The board's New issue: the Figma Button at Small, Default style, with a left icon (`toolbar_button`) |
| Status glyph | Figma's `status-*` drawings, 16px (`status_icon`): backlog a dashed circle and to do an open one in `icon`, in progress half filled in `accent`, review three quarters in `icon_purple`, done filled in `icon_green`, canceled (not in Figma) struck through in `muted`. A status heading (`status_heading`: the board's column headers, the task panels) is the glyph, 8px, then the label in caps at X-Small semibold, loud on the current column and muted otherwise |
| Segmented control | The Segmented Control component (`elements::controls::segmented_track` / `segment`): a `search` track, 4px in, 10px corners; items 24px, 12px across, Base in `form_text`, a fill fading in on hover; the selected one Base medium and loud on the canvas with `shadow/raised card`'s drops (`selected_segment`), which slides between items. Activity's kind filter is its own (below) |
| Activity filters | The Activity frame's own: kind segments in a bare row 4px apart (no track), 24px, 8px across, X-Small, muted, the selected one `selection` and loud; then, 16px on, the pickers: `search`'s fill, a 14px icon (the small Heroicons, `icons::FUNNEL_SMALL`, `USER_SMALL`: 1.5px stroke at 14px, as Figma draws them), 8px, an X-Small label, no chevron, icon and label in `log_text` (`filter_segment`, `filter_dropdown`) |
| Icon button | 24 or 32px square, the icon role at rest, the hover fill fading in and a loud icon on hover, the overlay fill while active |
| Labeled control | The Labeled Control component (`labeled_control`): the label (Base, loud), the control, then an optional helper (X-Small, `form_hint`), 6px apart, at most 640px wide. Every Settings row |
| Textbox | The Textbox component (`textbox`): 32px, the `textbox` well (5px corners, 8px across) inside a 1px `input_border` (6px corners), Base in `form_text`, a placeholder in `form_hint`; focused, the edge is `focus_ring` with a 3px `focus_glow` band just outside it, and the caret is `focus_ring` |
| Picker | The Picker component (`picker`): a growing textbox, 8px, a form button; 400px. The font picker ("Choose font") and the Shelbi CLI ("Browse") |
| Form button | `form_button`: the Button component's Default style at the Base size. `form_icon_button`: its 32px square Icon Button, the 20px icon in `button_icon` (the size stepper's − and +) |
| Checkbox | The Check, Checkbox and Checkbox List components (`check`, `checkbox`, `checkbox_list`): a 12px box with 2px corners stroked 1px outside (as Figma strokes it: 14px edge to edge, 3px corners), unchecked the `textbox` well in a `check_border` edge (`checkbox/default/border`, lighter than a textbox's `input_border` in Dark), checked `check_fill` with the 12pt mark in white; the label Base, loud when checked and `form_text` when not, 8px on, 2px in; focused, the edge turns `focus_ring` (`check_fill` checked) with a 3px glow around it (`focus_glow`, `check_glow` checked). The list stacks them 4px apart, 4px in |
| Menu, popover | `raised`, 8px corners, a 1px `ring` and a soft drop |
| Palette | The Command palette frame: over the `scrim`, 200px down, 788px wide (a 580px commands column, a 160px Projects column, 16px between and either side); `raised`, 8px corners, no ring, `palette_shadow` (0 10 15 -3). A 44px query line (`❯`, a 2x18px `link` caret, the placeholder `muted`), 32px rows (6px across and between, 6px corners: a 20px `icon`, the name in a 220px column in Base medium `foreground`, the description in Base `muted`; the selected row on `background`, its name Base semibold `text_loud`, its description `foreground`), 26px project rows, an X-Small `muted` footer |
| Dialog | Over the `scrim`, `raised`, 8px corners, a 1px ring and a deeper drop |
| Task pane | The Task pane frames (2172:722, 2573:10737): not a dialog, no scrim. 8px in from the main area's top, right and bottom, 640px wide until the user drags its left edge (400px to the main area less 240px; narrower when the window can't fit it), the `canvas` with 12px corners, a 1px `divider` edge drawn inside it and a 32px drop 8px down in `shadow`; slides in from the right over `SLIDE` on the motion curve. The 52px Header (`toolbar`, `Workflow / id` in Base semibold `text_loud`, 28px More and Close with 20px icons), then the scroll 20px in with 20px between sections: the title (Desktop/Title, loud), 6px-apart 28px properties (a 108px Small `muted` label, 12px, the value: the Button at Small in the Ghost style with its chevron, opening a picker under it, the branch in the terminal face at 12px, selectable), a divider, the body, the dates (X-Small, `muted`) |
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

## Window translucency (`src/theme/window.rs`)

`desktop.window_opacity` / `desktop.window_blur` (Settings ▸ General) make
the windows see-through. The opacity is applied when the theme resolves
(`Theme::with_opacity`), never as an opacity layer over a view:

- **Surfaces take it as their alpha**: `canvas` (the main area, the
  bootstrap screen, the chooser) and `sidebar` (the leading column). The
  terminals' default background follows them (a program's own OSC 11
  background takes the opacity too); cells with a color of their own stay
  opaque, as in Ghostty.
- **Each region is coated once.** The window shell, the column and the main
  area paint the surfaces; a view or panel that sits on the same surface
  (Activity, the board, Settings, a terminal pane, the sidebar's panels)
  paints `theme.nested(role)`, the role while opaque (where it still hides
  what slides under it) and nothing while translucent. `well` is the glass
  inside its divider.
- **Opaque fills on a surface become tints of the glass** (`src/theme/glass.rs`,
  `glass::on_glass` names each role's treatment). For each one the theme
  solves the tint that, laid over the opaque surface it sits on, paints its
  solid color exactly (`glass::unblend`: per channel the alpha that reaches
  it with the tint at 0 or 1, the largest of the three), then scales that
  alpha by the window opacity. At 100% nothing changes; below it the
  blurred desktop shows through the fill as it does through the glass.

  | Role | On the glass |
  |---|---|
  | `canvas`, `sidebar` | the window opacity as alpha |
  | `well` | clear (the glass inside its divider) |
  | `card`, `textbox`, `workflow_badge` | tint over `canvas` |
  | `nav_selected` (also the Zen pill's chip) | tint over `sidebar` |
  | `keycap` | itself at the window opacity (it sits on any button's fill) |
  | every other role | kept |

  Kept: the fills that are already tints (`background*`, `select_fill*`,
  `selection`, `search`, `toolbar`, `row_selected`, the chip tints, the
  `primary` / `success` / `danger` fills, `zen`, `notice`, the search and
  selection highlights), which let the glass through by themselves; text,
  icons, borders, focus rings and glows, shadows and the scrim, which stay as
  readable as opaque; and `raised`, the surface menus, popovers and the
  palette draw on. Contrast resolves first, so a tint is of the adjusted
  color.
- **A fill on another fill** is tinted over that fill, not the canvas: the
  board's workflow badge on a card is `theme.fill_on(theme.workflow_badge,
  theme.card)`, so the stack paints what the opaque stack does. A textbox's
  edge is a fill under its well while opaque; on the glass it is a 1px
  border around the well instead (`controls::edged`), so the edge doesn't
  show through the tinted well. The board's selected card wears its focus
  ring inside its edge (`focus_shadow_inset`): a drop shadow is a filled box
  under the card and would show through it.
- **Overlays stay opaque**: the dialogs (Feedback, workspace, Connect to
  Remote, the machine form, their text fields through
  `dialog_text_field`), the palette, menus, popovers and tooltips paint from
  `theme.opaque()`, as do the fills a drop shadow sits under (the selected
  segment, a slider's thumb, the board's drag ghost, the focused back
  button) and the terminal's notice band.
- **The task pane over the board is glass.** It paints no surface of its
  own (`nested(canvas)`) and its fills are tints; the board clips its header,
  columns and footer short of the pane (`task_detail::board_cover`, the
  pane's final place, anchored to the right so nothing reflows), so only the
  glass is under it. Over Review and Workspace (More) the pane covers a live
  session, so there it is an opaque overlay, its title and body editors too
  (`DocEditor::set_on_glass`).
- **Alpha blends with "over"**: gpui-pre 0.3.9's Metal pipelines add each
  primitive's alpha to the framebuffer's, which saturates any fill over the
  glass to opaque; `platform::metal_blend` corrects that at pipeline creation
  ([gpui.md](gpui.md#breakage-log)). The window server's blur
  (`platform::blur`) applies once to everything behind the window's
  translucent pixels, glass and fills alike.
- **Reduce Transparency** keeps every window opaque, read at launch and each
  time a window comes forward.

## Contrast (`src/theme/contrast.rs`)

`desktop.contrast` (Settings ▸ General's Contrast slider, 0 to 100%) moves
the surfaces and what is drawn on them apart. Like the opacity it is applied
when the theme resolves (`Palette::with_contrast`, through
`Theme::resolve_with`), never as an overlay, and before the opacity, so a
translucent window's surfaces are the adjusted colors at the window's alpha.
0 is the palette exactly; every role moves in proportion to the setting, by
what it is for:

| Class | Roles | Dark at 100 | Light at 100 |
|---|---|---|---|
| Surface | `canvas`, `sidebar`, `card`, `raised`, `well`, `textbox`, `keycap`, `nav_selected`, `workflow_badge` | down to a black canvas, each keeping its contrast ratio to the canvas (`canvas` `#1e2330` → `#000000`, `sidebar` `#2c3140` → `#171a24`, `workflow_badge` `#3a4258` → `#292f40`; `card` and `textbox`, darker than the canvas, meet it at black) | 50% toward white (`sidebar` `#f9f9fc` → `#fcfcfe`; white stays white) |
| Text | `text_loud`, `foreground`, `gray`, `form_text`, `form_hint`, `muted`, `log_text`, `table_text`, `icon`, `button_icon`, `dark_gray`, `workflow_badge_text`, `link_hover` | half toward white, and alpha half toward opaque (`muted` 55% → 78%) | half toward black, alpha half toward opaque (`muted` `#444c65` at 50% → `#222633` at 75%) |
| Border | `divider`, `divider_strong`, `input_border`, `check_border`, `ring` | half toward white, alpha doubled (`divider` 10% → 20%) | half toward black, alpha doubled |
| Hue | links, `accent`, the status icons and text, the log hues, focus rings, the colored button labels | a fifth toward white (hue kept) | a fifth toward black (hue kept) |
| Kept | the translucent fills laid over a surface (rows, chips, button fills and tints, selection, glows, shadows, scrim), `check_fill`, `text_on_color`, `button_keycap_edge` | unchanged: they composite darker over the darker surfaces by themselves, and their rest / hover / pressed steps stay as far apart | unchanged |

The light theme's surfaces are mostly white already, so its separation comes
mostly from the text, icons and rules. Body text on the canvas goes from
10.6:1 to 13.3:1 at 50 and 16.8:1 at 100 in Dark, muted text from 5.2:1 to
7.8:1 and 11.7:1; in Light, body text 8.2:1 to 11.0:1 and 14.8:1, muted
text 2.5:1 to 3.8:1 and 6.7:1.

The tests hold every pairing the WCAG checks above cover to never losing
contrast as the setting rises, in both themes, but one: the log's dark gray
on the selected Machines row's band (1.21:1 at 0), the only text drawn darker
than its band in Dark, crosses the band near 15% before it clears it, and at
100 reads better than at 0.

The terminals don't take it: their palette is the contrast-0 palette's at
every setting (default foreground, background and the ANSI sixteen), and a
pane hosting a terminal paints that background (`Theme::terminal_canvas`) so
the grid's insets match it. While the window is translucent a terminal's
default background is the window's glass, as with the opacity alone.

## Shared meanings

The shared view models name colors by meaning (the activity feed's `Tone`, a
badge's `DecorationColor`). `Theme::tone` and
`Theme::decoration` map each onto the Figma color for the same meaning:
Activity's log colors (`text/ansi_*`) for tones, the Navigation component's
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
| `canvas` | `#1e2330` | `#ffffff` | `surface/background`: the main area, the terminal's background |
| `sidebar` | `#2c3140` | `#f9f9fc` | Measured (the Navigation fill): the sidebar, the review and workspace panels |
| `card` | `#020303` | `#ffffff` | `surface/depth-0`: a board card |
| `raised` | `#2c3140` | `#f9f9fc` | Measured (Command palette): the palette, menus, popovers, tooltips |
| `well` | `#1e2330` | `#ffffff` | `surface/background`: the bootstrap screen's command box |
| `toolbar` | `#acb4cf1a` | `#acb4cf14` | `surface/depth-10` (the Issues Header): the board toolbar's band over the canvas |
| `textbox` | `#020303` | `#ffffff` | `surface/depth-0` (`checkbox/default/background` aliases it): the Textbox component's well, and an unchecked box's fill |

### Overlay fills

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `background` | `#acb4cf2e` | `#acb4cf1f` | `button/default/background` (`surface/hover`): buttons, dropdowns, chips, count badges, the Zen pill off, a hovered row |
| `background_hover` | `#acb4cf47` | `#acb4cf38` | `button/default/background-hover`: a control's hover |
| `background_active` | `#acb4cf66` | `#acb4cf52` | `button/default/background-active`: pressed, or open |
| `select_fill` | `#62cdf62e` | `#62cdf61f` | `button/select/background` (`Cyan/400` at the overlay's alphas): a set filter's chip on the board toolbar |
| `select_fill_hover` | `#62cdf647` | `#62cdf638` | `button/select/background-hover`: that chip hovered |
| `select_fill_active` | `#62cdf666` | `#62cdf652` | `button/select/background-active`: that chip pressed, or its menu open |
| `select_label` | `#b0e6fb` | `#1989c4` | `button/select/text` (`Cyan/300` / `Cyan/600`): the set filter chip's label |
| `selection` | `#acb4cf8c` | `#acb4cf66` | `surface/selection`: the selected segment |
| `search` | `#acb4cf40` | `#acb4cf29` | `surface/depth-30`: Activity's filter chips, the segmented track |
| `keycap` | `#1e2330` | `#ffffff` | The Hotkey's fill (`surface/background`) |
| `button_keycap_edge` | `#0203038c` | `#acb4cf99` | `shadow/hard`: the Hotkey's 1px drop inside a button (the Issue detail's actions) |

### Text

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `text_loud` | `#f2f3f7` | `#020303` | `text/bold` (`button/default/text` aliases it): titles, the selected row, a workspace name, a Default button's label |
| `foreground` | `#f2f3f7d9` | `#020303b2` | `text/foreground`: body text and labels |
| `gray` | `#acb4cfe5` | `#7b85a7e5` | `icon/primary`'s color, as the frames draw secondary text: hotkeys, keycaps |
| `form_text` | `#f2f3f7d9` | `#020303b2` | `text/foreground`, a role of its own: a textbox's value, an unselected segment or checkbox |
| `form_hint` | `#f2f3f78c` | `#444c6580` | `text/muted`, a role of its own: a labeled control's helper, a textbox's placeholder, the board toolbar's `Filter:` and search placeholder |
| `muted` | `#f2f3f78c` | `#444c6580` | `text/muted`: section headers, branches, hints, idle, placeholders |
| `text_on_color` | `#ffffff` | `#ffffff` | `other/on_color`: the check mark |
| `log_text` | `#7b85a7` | `#7b85a7` | The Activity frame's `text/gray` (not in `03 Desktop`; opaque, both modes): the log's titles, the `you` actor, the pickers' icon and label |
| `table_text` | `#7b85a7` | `#7b85a7` | The Machines frame's `text/gray` (the same): a workspace row's runner and task |

### Icons

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `icon` | `#acb4cfe5` | `#7b85a7e5` | `icon/primary` (90%): icons at rest |
| `button_icon` | `#acb4cf` | `#7b85a7` | `button/default/icon` (opaque, not `icon/primary`'s 90% or `icon/secondary`'s 60%): a button's icon (New issue, the issue detail's actions, the size stepper) |
| `accent` | `#62cdf6` | `#f21478` | `icon/select`: the selected nav icon, the current project's check |
| `icon_cyan` | `#62cdf6` | `#31ade9` | `icon/cyan`: ready for review |
| `icon_green` | `#31d924` | `#5ac351` | `icon/green`: a working workspace |
| `icon_yellow` | `#ffdd64` | `#ffca0d` | `icon/yellow`: awaiting input |
| `icon_purple` | `#e374ff` | `#b247e7` | `icon/purple`: a review status |
| `progress` | `#62cdf6` | `#f21478` | `progress/default` (`icon/cyan` / `icon/red`): the Add Project checklist's spinner, fading to transparent along its tail |

### Rules

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `divider` | `#acb4cf1a` | `#acb4cf1a` | `stroke/light`: dividers and hairlines |
| `divider_strong` | `#acb4cf33` | `#acb4cf33` | `stroke/medium`: the board's rules |
| `input_border` | `#acb4cf4d` | `#acb4cf80` | `stroke/dark`: a textbox's edge, the other form fields' |
| `check_border` | `#acb4cf33` | `#acb4cf80` | `checkbox/default/border` (`#acb4cf` at 20% / 50%): an unchecked box's edge, apart from a textbox's in Dark |

### Links and focus

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `link` | `#30c6ff` | `#1989c4` | `text/link`: links, More |
| `link_hover` | `#f2f3f7` | `#020303` | `text/bold`: a hovered link |
| `focus_ring` | `#62cdf6` | `#31ade9` | `focus/default/ring`: the 1px focus ring, the find caret, drop targets |
| `focus_glow` | `#12adff33` | `#12adff33` | `focus/default/glow`: the 4px glow outside it |
| `success_focus_ring` | `#5ac351` | `#5ac351` | `focus/success/ring` (`Green/500`): a focused Success button's 1px ring (Approve, Relaunch) |
| `success_focus_glow` | `#86d58033` | `#86d58026` | `focus/success/glow` (`Green/400` at 20% / 15%): its 4px glow |
| `danger_focus_ring` | `#f21478` | `#f21478` | `focus/danger/ring` (`Pink/500`): a focused Danger button's 1px ring (Reject, Delete) |
| `danger_focus_glow` | `#ff66ab40` | `#ff66ab1a` | `focus/danger/glow` (`Pink/400` at 25% / 10%): its 4px glow |
| `check_fill` | `#31ade9` | `#f21478` | `checkbox/select/background` and `/border` (`Cyan/500` / `Pink/500`): a checked box, and a focused checked box's ring |
| `check_glow` | `#12adff33` | `#ff66ab33` | `checkbox/select/focus-glow`: a focused checked box's glow |

### Meaning

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `success` | `#31d924` | `#00753d` | `text/green`: done, approve, the Zen pill's label |
| `warning` | `#dbc300` | `#8a5f04` | `text/ansi_yellow`: in progress, stale data |
| `info` | `#5893d6` | `#2563c9` | `text/ansi_blue`: ready, informational |
| `error` | `#ff66ab` | `#b40041` | `button/danger/text` (`Pink/400` / `Pink/700`): errors, blocked, a version mismatch |

### Log and terminal hues

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `theme_green` | `#5acd25` | `#367b16` | `other/green`: ANSI green in the light terminal, the Machines states |
| `theme_cyan` | `#00a6b2` | `#007a83` | `other/cyan`: ANSI cyan in the light terminal, a review slot |
| `theme_red` | `#e04f52` | `#c23b3e` | `other/red` (`text/ansi_red`): ANSI red in the light terminal |
| `green` | `#5acd25` | `#1a7f37` | `text/ansi_green`: `[done]` |
| `yellow` | `#dbc300` | `#8a5f04` | `text/ansi_yellow`: `[in progress]` |
| `blue` | `#5893d6` | `#2563c9` | `text/ansi_blue`: `[ready]`, queued |
| `magenta` | `#d259d7` | `#a21caf` | `text/ansi_magenta`: `[review]`, handoff |
| `cyan` | `#00a6b2` | `#0f766e` | `text/ansi_cyan`: a working actor in the log |
| `dark_gray` | `#666666` | `#6b6b6b` | `text/ansi_dark_gray`: timestamps, ANSI bright black in light |
| `red` | `#ff66ab` | `#b40041` | `button/danger/text`: failures and errors as text |

The dark terminal's ANSI sixteen (`theme::SENSIBLE`, the Terminal frame's
`ansi/*` variables; Settings ▸ Terminal labels the bright eight Bold, and
its Reset restores these):

| Color | Regular (0 to 7) | Bold (8 to 15) |
|---|---|---|
| Black | `#000000` | `#666666` |
| Red | `#e04f52` | `#e69589` |
| Green | `#5acd25` | `#9bdaa6` |
| Yellow | `#dbc300` | `#e6d77b` |
| Blue | `#5893d6` | `#5abdff` |
| Magenta | `#d259d7` | `#dc8de6` |
| Cyan | `#00a6b2` | `#82e6e5` |
| White | `#bababa` | `#ffffff` |

### Chip tints and badges

Each tint is its hue at `button/default/background`'s opacity (18% / 12%),
the rule the Button component's Success and Danger styles follow.

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `green_tint` | `#5acd252e` | `#1a7f371f` | A green chip's background |
| `yellow_tint` | `#dbc3002e` | `#8a5f041f` | A yellow chip's background |
| `cyan_tint` | `#00a6b22e` | `#0f766e1f` | A cyan chip's background |
| `blue_tint` | `#5893d62e` | `#2563c91f` | A blue chip's background |
| `magenta_tint` | `#d259d72e` | `#a21caf1f` | A magenta chip's background |
| `red_tint` | `#ff66ab2e` | `#b400411f` | A red chip's background |
| `workflow_badge` | `#3a4258` | `#d3d8e4` | The cards' `other/workflow_badge_bg` (not in `03 Desktop`): a card's workflow name, a neutral chip |
| `workflow_badge_text` | `#dcdfe8` | `#2a3142` | The cards' `other/workflow_badge_fg` (the same): its label |

### Buttons

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `primary_fill` | `#62cdf62e` | `#62cdf61f` | `button/primary/background` (`Cyan/400` at the overlay's alphas): Button Primary, a dialog's confirming action |
| `primary_fill_hover` | `#62cdf647` | `#62cdf638` | `button/primary/background-hover` |
| `primary_fill_active` | `#62cdf666` | `#62cdf652` | `button/primary/background-active` (the component's Active variant binds the rest fill; the variable is the press) |
| `primary_label` | `#b0e6fb` | `#1989c4` | `button/primary/text` and `button/primary/icon` (`Cyan/300` / `Cyan/600`) |
| `success_fill` | `#5ac3512e` | `#5ac3511f` | `button/success/background` (`Green/500` at 18% / 12%): Button Success, Approve, the bootstrap and relaunch actions, the card detail's Save |
| `success_fill_hover` | `#5ac35147` | `#5ac35138` | `button/success/background-hover` |
| `success_fill_active` | `#5ac35166` | `#5ac35152` | `button/success/background-active` (40% / 32%) |
| `success_label` | `#86d580` | `#00753d` | `button/success/text` and `/icon` (`Green/400` / `Green/700`) |
| `danger_fill` | `#f214782e` | `#f214781f` | `button/danger/background` (`Pink/500` at 18% / 12%): the danger button, the unread-errors button |
| `danger_fill_hover` | `#f2147847` | `#f2147838` | `button/danger/background-hover` |
| `danger_fill_active` | `#f2147866` | `#f2147852` | `button/danger/background-active` (40% / 32%) |

### Bands

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `zen` | `#5ac3512e` | `#5ac3511f` | `surface/green` (the Success fill's values): the Zen pill |
| `notice` | `#ffdd642e` | `#ffca0d1f` | `icon/yellow` at `button/default/background`'s opacity: the relaunch band |

### Selection

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `nav_selected` | `#1e2330` | `#ffffff` | `surface/background`: the selected nav chip (it slides) |
| `row_selected` | `#acb4cf2e` | `#acb4cf66` | Activity's selected row: `surface/hover` dark, `surface/selection` light, as drawn |
| `text_selection` | `#acb4cf8c` | `#acb4cf66` | `surface/selection`: the terminal's selection |
| `search_match` | `#acb4cf40` | `#acb4cf29` | `surface/depth-30`: a find match |
| `search_current` | `#ffdd6466` | `#ffca0d66` | `icon/yellow` at 40% (Figma draws no current match) |

### Elevation

| Role | Dark | Light | Source, used for |
|---|---|---|---|
| `ring` | `#acb4cf1a` | `#0a1e5108` | Measured (Issue detail's edge; `stroke/light`'s value in Dark): the 1px ring around raised surfaces |
| `shadow` | `#0a1e5105` | `#0a1e5105` | `shadow/raised card`'s drops (menus, popovers and dialogs too) |
| `shadow_ring` | `#0a1e5108` | `#0a1e5108` | `shadow/raised card`'s 1px ring |
| `palette_shadow` | `#00000040` | `#00000040` | The Command palette's drop effect (`0 10 15 -3`): the palette over the scrim |
| `scrim` | `#02030359` | `#444c65a6` | `surface/overlay`: the backdrop behind the palette and dialogs |

## Contrast

`src/theme/palette.rs` checks every pairing the desktop draws, in both
themes: the loud, body, gray and muted text on the canvas, sidebar, card and
raised surfaces and on a hovered or selected row over them; every meaning
and log hue as text on the surfaces; every chip's text on its tint; a
workflow badge; each button style's label on its rest, hover and pressed
fill; the selected segment's label; a keycap's label; the Zen pill's label.
Text is held to WCAG AA (4.5:1). Icons, the focus ring, a text field's edge
and the selected nav icon are held to 3:1 (WCAG 1.4.11). `src/theme/mod.rs`
checks the terminal's default foreground at AA and its ANSI hues and dim
text at 3:1 on the canvas, except Light's dim text: `text/muted` (`#444c65`
at 50%) over white is 2.46, held to a 2.4 floor.

### Exceptions

The design keeps these Figma values where they fall short; nothing is
shifted away from Figma to pass. Each entry covers every pairing whose name
starts with it. `contrast_shortfalls_are_exactly_the_documented_exceptions`
fails on a shortfall no entry covers and on an entry that covers nothing,
and `the_theme_doc_lists_every_contrast_exception` checks each row here,
with its worst ratio. **Flagged for John.**

| Theme | Pairing | Worst | Why |
|---|---|---|---|
| dark | muted on | 2.42 | `text/muted` (`#f2f3f7` at 55%): AA on the canvas (5.24) and the sidebar (4.72), 3.69 on a hovered row over the sidebar, 4.19 over the canvas. Section headers, branches, idle labels. 2.42 is an idle state on the selected Machines row (`surface/selection`), a row the frame doesn't draw selected |
| dark | dark_gray on | 1.21 | `text/ansi_dark_gray` `#666666`: Activity's timestamps, 2.73 on the canvas, 2.26 on the sidebar, 3.60 on a card. 1.21 is the branch on the selected Machines row, as the frame draws it |
| dark | log_text on | 2.53 | The Activity frame's `text/gray` `#7b85a7`: the log's titles, 4.30 on the canvas, 2.96 on the selected row; the pickers' labels, 2.53 on their `search` fill |
| dark | info on | 4.05 | `text/ansi_blue` `#5893d6` as the info color: AA on the canvas (4.90), 4.05 on the sidebar and raised surfaces |
| dark | blue on | 3.76 | The same blue as `[ready]`: 4.05 on the sidebar, 3.76 on its chip tint |
| dark | gray on background | 3.84 | `icon/primary`'s color (hotkeys) on a hovered row over the sidebar |
| dark | gray on row_selected | 3.84 | The same on the selected row over the sidebar |
| dark | magenta on | 3.64 | `text/ansi_magenta` `#d259d7` as `[review]`: AA on the canvas (4.60), 3.80 on the sidebar and raised surfaces, 3.64 on its (derived) chip tint |
| dark | cyan on | 4.04 | `text/ansi_cyan` `#00a6b2` as a working actor: AA on the canvas (5.30), 4.38 on the sidebar and raised surfaces, 4.04 on its (derived) chip tint |
| dark | red on red_tint | 4.39 | The Danger pink on its (derived) chip tint |
| dark | text_loud on selection | 3.91 | `text/bold` `#f2f3f7` on the selected segment (`surface/selection`) over the sidebar; 4.27 on the canvas |
| dark | theme_green on selection | 2.30 | The selected Machines row: `working` and `● reachable` in `other/green` |
| dark | theme_cyan on selection | 1.60 | The same row: a review slot's `review` in `other/cyan`. The lowest pairing the desktop draws |
| dark | success_label on hover button | 4.35 | `button/success/text` (`#86d580`) on the Success button hovered, over the sidebar |
| dark | success_label on pressed button | 3.48 | The same on the Success button pressed (`button/success/background-active`, 40%): 3.94 over the canvas, 3.48 over the sidebar |
| dark | error on rest button | 4.30 | `button/danger/text` on the Danger fill over the sidebar (AA over the canvas) |
| dark | error on hover button | 3.94 | The same, hovered |
| dark | error on pressed button | 3.45 | The same, pressed (`button/danger/background-active`, 40%): 3.85 over the canvas |
| dark | input_border on | 1.76 | `stroke/dark` (`#acb4cf` at 30%) as a textbox's edge (Settings); the textbox also differs by its near-black well and its label |
| dark | text_on_color on check_fill | 2.53 | The check mark (white) on the dark checked box (`#31ade9`), as the Settings frame draws it; the box itself is 6.19 on the canvas |
| dark | check_border on | 1.37 | `checkbox/default/border` (`#acb4cf` at 20%) around an unchecked box's near-black well; the box also differs by its well and its label |
| dark | table_text on | 1.30 | The Machines frame's `text/gray` `#7b85a7`: a row's runner and task, 4.30 on the canvas, 1.30 on the selected row (`surface/selection` over the canvas), as the frame draws it |
| dark | form_hint on search box | 3.69 | `text/muted` (`#f2f3f7` at 55%) as the board search's `Search` placeholder, on the Searchbox fill over the toolbar band; AA on the band itself |
| dark | select_label on | 3.98 | `button/select/text` (`Cyan/300`) on a set filter chip pressed (its menu open); AA at rest and hovered |
| dark | primary_label on | 4.02 | `button/primary/text` (`Cyan/300`) on the Primary button pressed over the sidebar or a dialog; AA at rest and hovered |
| light | muted on | 2.23 | `text/muted` (`#444c65` at 50%): 2.46 on white, 2.42 on the sidebar, 2.23 on the selected row. Also the terminal's dim text (2.46) |
| light | gray on | 2.39 | `icon/primary`'s color (`#7b85a7` at 90%): 3.12 on white; hotkeys, keycaps, secondary text |
| light | log_text on | 2.80 | The same `#7b85a7` in Light: 3.65 on white, 2.80 on the selected row |
| light | icon on | 2.80 | Icons in `icon/primary` on the sidebar (3.12 on white); 2.80 for the board search's magnifying glass on the Searchbox fill over the toolbar band |
| light | link on | 3.69 | `text/link` `#1989c4`: 3.88 on white |
| light | green on green_tint | 4.30 | `[done]` on its (derived) chip tint |
| light | success_label on pressed button | 4.36 | `button/success/text` on the Success button pressed |
| light | error on pressed button | 3.98 | `button/danger/text` `#b40041` on the Danger button pressed |
| light | focus_ring on | 2.41 | `focus/default/ring` `#31ade9`: 2.53 on white; the 4px glow widens it |
| light | success_focus_ring on | 2.14 | `focus/success/ring` `#5ac351` (`Green/500`) around a focused Success button: 2.24 on white, 2.14 on the sidebar and raised surfaces; the 4px glow widens it and the button's own green fill and label carry it |
| light | dark_gray on selection | 4.08 | The selected Machines row: the branch in `text/ansi_dark_gray` |
| light | theme_green on selection | 4.02 | The same row: `working` in `other/green` (5.0 on white) |
| light | theme_cyan on selection | 3.91 | The same row: `review` in `other/cyan` |
| light | input_border on | 1.38 | `stroke/dark` (`#acb4cf` at 50%) as a textbox's edge |
| light | form_hint on | 2.37 | `text/muted` (`#444c65` at 50%) in the forms: a helper line, a placeholder; the board toolbar's `Filter:` and its search's placeholder (2.37 on the Searchbox fill over the band) |
| light | check_border on | 1.41 | `checkbox/default/border` (`#acb4cf` at 50%) around an unchecked box |
| light | table_text on | 2.80 | The same `#7b85a7`: 3.65 on white, 2.80 on the selected row |
| light | select_label on | 3.08 | `button/select/text` (`Cyan/600` `#1989c4`) on a set filter chip over the toolbar band: 3.45 at rest, 3.25 hovered, 3.08 pressed, as the Header draws it |
| light | primary_label on | 3.09 | `button/primary/text` (`Cyan/600` `#1989c4`) on the Primary button: 3.61 at rest on white, 3.46 at rest in a dialog, 3.09 pressed there, as the Button component draws it |

`button_icon` is no longer an exception: `button/default/icon` is opaque, so
the Button's icon holds 3:1 on every fill (4.35 dark, 3.23 light at worst).

## Renders

Every `--visual-*` mode renders both themes: with `SHELBI_VISUAL_THEME`
unset it runs itself once per theme into `<dir>/dark/` and `<dir>/light/`;
set it to `dark` or `light` for one. `--visual-views` includes the issues
board (`board.png`, `board-dropdown.png`, `board-detail.png`), Settings, the
bootstrap screens and the notice bars; `--visual-review` the project
switcher (`sidebar-switcher-mid.png`, `sidebar-switcher.png`), the nav chip
mid-slide (`sidebar-slide-mid.png`) and the palette sliding in
(`palette-entering.png`).
