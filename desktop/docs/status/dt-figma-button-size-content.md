# dt-figma-button-size-content: explicit Button sizes and shared content

## The problem

The shared button helpers mixed size presets. `button`, `button_base` and
`disabled_button` drew a hybrid: 32px tall (Figma's Base height) with
Small medium 13px text, documented as "the Figma Button (Small)". Views that
wanted the real Small (28px) or Base (14px text) overrode height, padding,
gap and type by hand, and each assembled its own icon slots, chevrons and
keycaps. Icons were colored inconsistently: `icon_label_button` painted a
Default icon in the label color, `small_icon_button` in `button_icon`, and
the headers and pickers in `icon`.

## Figma contract

Button component `2220:17600` (audited 2026-10-22; Default variants X-Small
`2220:17545`, Small `2220:17601`, Base `2220:17603`). All Geist medium, 6px
corners, content centered.

| Size | Height | Text | Across | Gap | Icon slot / glyph |
|---|---|---|---|---|---|
| X-Small | 24 | 12 / 16 | 8 | 4 | 14 / 18 |
| Small | 28 | 13 / 18 | 12 | 4 | 12 / 20 |
| Base | 32 | 14 / 18 | 12 | 6 | 14 / 20 |

Content order: left icon (slot `MAX`-aligned, so the glyph hangs past its
left edge), label, Hotkey (`Border=true`: 16 tall, 4 across, 4 corners, 2
between keys, the keycap fill and a 0/1 `button_keycap_edge` drop), right
icon (slot `MIN`-aligned). Hidden pieces take no gap. Disable sets the whole
variant's opacity to 40%.

Screen instances checked on the Mockups page: the Issue detail frame
resizes all its X-Small actions to 26px (kept, as `task_detail::ACTION_H`);
Settings ▸ Agent Details and Integrations stretch Small pickers to 32px
(kept on `agent_editor::select_button`). The Issue detail frame binds
Start, Assign, Edit and Editor's icons to `icon/primary` (an instance-swap
override; the Move arrows keep `button/default/icon`); the app now draws
them all in `button/default/icon` (90% vs 100% of the same slate).

## The change

`theme/style.rs`:

- `ButtonSize { XSmall, Small, Base }` with `height`, `padding`, `gap`,
  `text`, `slot` and `glyph`. `BUTTON_H` is `ButtonSize::Base.height()`.
- `ButtonContent`: `label(..)`, then `.left(icon)`, `.hotkey(key)`,
  `.right(icon)` (each takes `Option` too, and an empty hotkey shows none).
- `content_button` / `disabled_content_button` lay content out in order;
  `button`, `disabled_button`, `icon_label_button`, `small_icon_button`,
  `toolbar_button` and `xsmall_button` are thin wrappers, and `button`,
  `disabled_button`, `icon_label_button` and `button_base` take an explicit
  `ButtonSize`. Enabled and disabled share one private `button_shell`, so a
  disabled button has its enabled twin's geometry and type; it dims once
  (the control at 40%) and has no fill layer, pointer or focus.
- `ButtonColors::icon`: Default (and Quiet) `button_icon`, the tinted styles
  their label's color. Content icons always use it.
- `left_icon_slot` / `right_icon_slot` (and `left_slot` / `right_slot` for a
  status glyph) replace `icon_slot(icon, width, color)`. A slot
  `debug_assert`s the icon draws at its size's glyph (18 at X-Small, 20
  otherwise), so a 20px drawing can't slip into an X-Small button.
- `button_hotkey` is the Hotkey, moved from `task_detail::keycap`.
- `filter_chip` takes the Small shell.

Callers: every control that overrode to 28px is now `Small` with the
override gone; every control at the old 32px is now `Base` (same height,
14px text) unless its Figma frame shows Small. Dialogs (Zen intro, Load for
review, Reject, leave and delete prompts), Activity's Clear filters and Load
older, Machines' Refresh, the bootstrap action and the form buttons are
Base. The workflow and agent editors' Save changes, the agents page's
Save/Cancel, Open keys.yaml, the ANSI Reset, header buttons, row pickers,
the error log's buttons and the builder's Small controls are Small. The
error log's buttons lose their 10px / X-Small regular override for the
Small preset. View-local `header_button`, `picker_button`, `base_button`,
`small_button`, `select_button`, the category picker, the zoom and action
dropdowns and `task_detail::action` now build on the shared content and
slots.

Not changed: the square Icon Button (`icon_button`, `form_icon_button`,
the editors' toolbar and row buttons) and the Workspace Shell toggle (its
own 14px icon and regular label; it now takes the X-Small shell).

## Verification

- `buttons_lay_out_to_the_figma_size_contract` (gpui test): every size with
  label only, left, right, both icons, and a Hotkey, enabled and disabled:
  height, padding, slot widths, gaps between present pieces only, and
  disabled bounds equal to enabled. Breaking the padding fails it.
- `button_glyphs_draw_at_the_sizes_glyph`, `button_icons_take_the_styles_icon_role`,
  `disabled_buttons_dim_once_and_take_no_pointer`, and the existing focus
  tests.
- `--visual-figma` components: the `buttons` sheet (Base styles, Small
  Approve and Reject, X-Small Open and an X-Small Move with its Hotkey,
  across rest, focus, focus+hover, focus+press and disabled, with the
  existing exact ring and glow checks) and a new `button-sizes` sheet
  (`components/button-sizes-<theme>.png`, 1024x744): each size with each
  content mix in Default, Primary, Success, Danger, Select and disabled,
  with pixel checks of each button's top and bottom edges.
  `SHELBI_VISUAL_ONLY=buttons`: 1224 checks, 0 failed, both themes.
- Real screens: the issues board (New issue), the issue detail (Move
  actions), the Review panel (Approve and Reject) and Settings (Browse)
  rendered with `SHELBI_VISUAL_ONLY=issues`, `=panels` and `=settings`.
