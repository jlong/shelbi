# dt-figma-button-focus-colors: Success and Danger buttons focus in their own colors

## The problem

`button_base` drew every focused button with the default cyan ring and glow
(`focus_bands(6.0, theme)`), whatever its style. The Figma Button component
(`2220:17600`, audited 2026-10-22) gives the Success and Danger styles their
own focus: a green ring and glow and a pink ring and glow. A keyboard-focused
Approve (Success) or Reject (Danger) wore cyan around a green or pink button.

## Figma values

Representative Small focus variants: Default `2224:5758`, Success
`2224:5770`, Danger `2224:5782`.

| Style | Ring (Dark / Light) | Glow Dark | Glow Light | Variables |
|---|---|---|---|---|
| Default, Primary | `#62cdf6` / `#31ade9` | `#12adff` at 20% | `#12adff` at 20% | `focus/default/ring`, `focus/default/glow` |
| Success | `#5ac351` (`Green/500`) both | `#86d580` (`Green/400`) at 20% | at 15% | `focus/success/ring`, `focus/success/glow` |
| Danger | `#f21478` (`Pink/500`) both | `#ff66ab` (`Pink/400`) at 25% | at 10% | `focus/danger/ring`, `focus/danger/glow` |

The Default and Primary focus variants still bind an older variable,
`VariableID:2220:20442` (`focus/default/focus-glow`); it resolves to the same
`#12adff` at 20% as the current `focus/default/glow`, so the code reads the
current name.

## The change

- **Palette** (`theme/palette.rs`): four `figma` constants
  (`SUCCESS_FOCUS_RING`, `SUCCESS_FOCUS_GLOW`, `DANGER_FOCUS_RING`,
  `DANGER_FOCUS_GLOW`) and four roles (`success_focus_ring`,
  `success_focus_glow`, `danger_focus_ring`, `danger_focus_glow`). The alpha
  bytes follow the palette's convention (15% is `26`, 10% `1a`, 25% `40`),
  pinned by `token_alphas_are_figmas_opacities`.
- **Style** (`theme/style.rs`): `ButtonColors` gains `focus: FocusColors`
  (a ring and a glow). `button_colors` gives Success and Danger their own,
  and Primary, Secondary (Default), Select and Quiet the default cyan
  (`FocusColors::default`): Select and Quiet have no Figma focus variant, so
  they keep the focus every other control wears.
- `button_base` draws `focus_bands_in(6.0, 0.0, c.focus)`. `focus_bands` and
  `focus_bands_past` are unchanged for their callers (search boxes, fields,
  the keybindings cap, the task panels' back button) and now delegate to
  `focus_bands_in` with the default colors.
- Geometry is unchanged: the 1px ring and the 4px glow over it, both
  absolute bands outside the control's bounds on 6px corners. They don't
  take part in layout, so a focused button's size, position and hit area are
  its unfocused ones, and its fill (rest, hover or pressed) and label never
  depend on focus. No gpui box shadow is drawn under a translucent fill.
- `disabled_button` takes no `focused` argument, so a disabled button (40%
  opacity overall) shows no hover, press or focus.

Every focus-capable wrapper goes through `button_base`: `button`,
`small_icon_button` (the Review panel's Approve and Reject), the Base-size
callers (`button_base` + `Type::BaseMedium`, e.g. the workflow editor's
Delete), and the X-Small shape.

## Discrepancies recorded, not changed

- **`focus/ring-width` says 2**, but all 12 Button focus variants draw a 1px
  outside ring and a 4px outside glow, each with 6px corners on the
  control's own bounds; no variant binds the variable. The app keeps the
  drawn geometry, and no control's ring is widened. Worth fixing in Figma
  (set the variable to 1, or drop it).
- The two Primary slips documented in
  [`dt-primary-button-focus.md`](dt-primary-button-focus.md) are still in
  the component and still not followed: Active binds the rest fill instead
  of `button/primary/background-active` (the app presses with the active
  token), and Focus binds `button/default/text` (the app keeps
  `button/primary/text`).

## Contrast

`focus/success/ring` `#5ac351` is 2.24:1 on white and 2.14 on the light
sidebar and raised surfaces, short of the 3:1 non-text minimum. Kept as
Figma draws it and listed in `docs/theme.md`'s exceptions
(`success_focus_ring on`), like the default ring's own Light shortfall
(2.41). `danger_focus_ring` passes 3:1 in both themes, and the Success ring
passes in Dark.

## Tests

- `theme::style::tests::success_and_danger_buttons_focus_in_their_own_colors`:
  each kind's `ButtonColors::focus` in Dark and Light; Success green and
  Danger pink, the other four exactly the default.
- `theme::style::tests::focus_never_changes_a_buttons_fill_or_label`
  (unchanged): a focused button keeps its label color, sets no fill of its
  own and draws no shadow.
- `theme::palette::tests::roles_are_the_figma_tokens`,
  `token_alphas_are_figmas_opacities`, the contrast and theme-doc tests cover
  the new roles.

## Renders (`--visual-figma`)

`components/buttons-<theme>.png` is now 580x498: ten rows (the six kinds as
`button`, Approve and Reject as `small_icon_button`, a Base Danger Delete,
an X-Small Default Open) by five columns (rest, focused, focused + hovered,
focused + pressed, disabled). Hover and press are held with
`motion::hold_pointer` before the sheet mounts, so each fill layer starts at
that state. The harness checks every cell against the theme's own colors at
the backing scale (before any downsampling), within 3 per channel, at
mid-height:

- 3pt inside the left edge: the state's fill over the page (rest, hover,
  pressed, or the rest fill at 40% when disabled), so focus never tints the
  inside;
- focused cells: 0.5pt outside, the style's glow over its ring over the
  page; 3pt outside, the glow over the page; 4.5pt outside, the page (the
  glow stops at 4pt);
- rest and disabled cells: the page at 0.5pt and 3pt outside (no ring).

180 checks per theme, all passing with exact matches (Light Success
focused: ring `[97, 198, 88]`, glow `[228, 240, 228]` on `#f5f5f5`).
`SHELBI_VISUAL_ONLY=buttons` renders the sheets plus the two Load for review
states (`state-review-confirm-{load,cancel}-focused-<theme>.png`), which
keep their Primary cyan focus.
