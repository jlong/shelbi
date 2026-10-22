# dt-primary-button-focus: the Primary button, and focus that only adds the ring

## The problem

John's screenshot of **Load for review**: the keyboard-focused **Load**
button drew a bright cyan fill with a near-invisible light label, plus the
ring.

```
┌ Load for review ──────────────────────────────┐
│ Adopt Command Grille as the Shelbi desktop... │
│ Load onto review workspace review?            │
│                         [▓Load▓] [ Cancel ]   │   before: solid cyan, label lost
│                         (Load)   [ Cancel ]   │   after: Primary tint, cyan label, ring + glow
└───────────────────────────────────────────────┘
```

**Cause:** nothing in the code swapped the fill on focus. `button_base`
drew focus as `focus_shadow` (a 1px ring and a 4px glow as box shadows),
and gpui paints a box shadow as a **filled box under the element**. Every
Button style's fill is translucent (an overlay tint at 12 to 40%), so the
shadow's cyan showed through the whole button. With the shadow back in
place, the harness reads the focused fill as `#55c8f8` against a resting
`#2a4254` (dark): John's solid cyan. The same thing happened on every
focused button, in every style.

## The fix

- **Focus is ring-only.** `button_base` draws the ring and glow as
  `focus_bands` (two bordered rings outside the button, already used by the
  search box), not as a shadow. The fill layer and the label color don't
  depend on `focused` at all, so a focused button keeps exactly its resting,
  hovered or pressed fill and label. This covers `button`, `small_icon_button`
  (the Review panel's Approve and Reject) and every other caller of
  `button_base`.
- **A real Primary style.** The code's `ButtonKind::Primary` was the Figma
  Button's **Success** style (green), because Figma had no Primary when it
  was written. It's renamed `ButtonKind::Success`, and a new
  `ButtonKind::Primary` paints the Figma **Style=Primary** variant from four
  new palette roles (`primary_fill`, `primary_fill_hover`,
  `primary_fill_active`, `primary_label`).

## Figma values (Button component set `2220:17600`, Components page `2196:10061`)

`Style=Primary`, read with `use_figma` (variables resolved per mode):

| Property | Variable | Dark | Light |
|---|---|---|---|
| Fill | `button/primary/background` | `Cyan/400` `#62cdf6` at 18% (`#62cdf62e`) | at 12% (`#62cdf61f`) |
| Hover fill | `button/primary/background-hover` | at 28% (`#62cdf647`) | at 22% (`#62cdf638`) |
| Pressed fill | `button/primary/background-active` | at 40% (`#62cdf666`) | at 32% (`#62cdf652`) |
| Label, icon | `button/primary/text`, `button/primary/icon` | `Cyan/300` `#b0e6fb` | `Cyan/600` `#1989c4` |
| Radius | | 6 | 6 |
| Padding | | X-Small 8 / 4 (24 tall), Small 12 / 4 (28 tall), Base 12 / 4 (32 tall) | same |
| Label type | | Geist Medium 12 / 13 / 14 (X-Small / Small / Base) | same |
| Disabled | | the whole button at 40% opacity | same |
| Focus | | `Focus Ring` + `Focus Glow` rectangles outside the button (`focus/default/ring`: `Cyan/400` dark, `Cyan/500` `#31ade9` light; `focus/default/glow`: `#12adff` at 20%) | same |

These are the Select style's values exactly (`button/select/*` aliases the
same `Cyan/400` at the same alphas, and the same text colors), so the
existing `select_*` roles already matched. Primary gets its own roles and
tokens anyway (`figma::PRIMARY_*`), so the two can diverge in Figma without a
code change here; `roles_are_the_figma_tokens` pins both.

Radius, padding and type match `button_base` already (6px corners, 12px
across, Small medium), so the only change for Primary is its colors.

### Two Figma slips the code doesn't follow

- **`Focus=true` swaps the Primary label** to `button/default/text`
  (`#020303` in Light, `#f2f3f7` in Dark): the Primary variants look copied
  from Default and kept its text binding. That is exactly the unreadable
  focused label, and the task rules it out, so the app keeps
  `button/primary/text` when focused. (Success and Danger focus variants
  keep their own label colors, as the app does.)
- **`Active=true` binds the rest fill** (`button/primary/background`, 12%),
  where every other style's Active variant binds its `background-active`.
  The `button/primary/background-active` variable exists (32% / 40%), so the
  press uses it, like every other style.

Both are worth fixing in the Figma file.

### Contrast

`button/primary/text` on its own fill is short of AA 4.5 in Light (3.61 at
rest on white, 3.46 at rest in a dialog, 3.09 pressed there) and on the
dark pressed fill (4.02); AA at rest and hovered in Dark. As with
`select_label` (the same colors), the Figma value is kept and listed in
`docs/theme.md`'s exceptions (`primary_label on`) rather than shifted. It
stays readable in every state, focused included (no focus state changes
it).

## Buttons changed to Primary

| Where | Button | Was |
|---|---|---|
| `views/overlay/review_confirm.rs` (Load for review) | **Load** | `Primary` (= Success green) |
| `views/overlay/zen_intro.rs` (Zen mode intro) | **Enable** | `Primary` (= Success green) |
| `views/workflow_editor.rs` (unsaved-changes prompt) | **Save** | `Select` (same colors) |
| `views/workflow_editor.rs` (editor titlebar, and its disabled form while clean or saving) | **Save changes** | `Select` (same colors); Figma draws it `Style=Primary` in every editor frame |

## Buttons renamed to Success (look unchanged)

The old `Primary` callers that aren't a dialog's confirming action keep the
green Success style, now under its Figma name:

| Where | Button | Why not Primary |
|---|---|---|
| `views/review.rs` (Review panel) | **Approve** | Figma draws it `Style=Success` (Review frames, both modes) |
| `views/task_detail.rs` (issue detail, editing) | **Save** | Not a confirmation prompt, and no Figma frame draws it; kept as it was. A one-word change to Primary if John wants the card's Save to match the editor's |
| `views/bootstrap.rs` (bootstrap screen) | the screen's action (Try Again, Relaunch Shelbi) | Not a dialog; kept as it was |
| `views/bootstrap.rs` (relaunch bar) | **Relaunch Shelbi** | Same |

Unchanged on purpose: Cancel, Dismiss and Discard keep Default (or Quiet);
Reject (`review_reject.rs`) and the destructive buttons keep **Danger**, the
confirming action of a destructive prompt; Builder's **Add transition** in
the new-transition inspector keeps Select (`Style=Default, Select=true` in
its Figma frame). `add_project.rs` and `error_log.rs` have no confirming
button (the form commits on Enter; the log's buttons are Secondary actions).

## Focus audit (every button style)

| Control | Where | Focus before | Now |
|---|---|---|---|
| `button` / `button_base` (every Button style) | `style.rs` | ring + glow as a **shadow**: filled the translucent fill with cyan | ring + glow as `focus_bands`; fill and label independent of focus |
| `small_icon_button` (Approve, Reject) | `style.rs` | via `button_base` (shadow) | via `button_base` (bands) |
| `icon_label_button`, `xsmall_button`, `toolbar_button` | `style.rs` | no focus state | unchanged |
| `form_button` (Browse ...) | `controls.rs` | no focus state (`button_base`, `false`) | unchanged |
| `form_icon_button` | `controls.rs` | no focus state | unchanged |
| `icon_button` / `tinted_icon_button` | `style.rs` | no focus state (`active` is a held state, not focus) | unchanged |
| `filter_chip` | `style.rs` | no focus state (`open` = pressed) | unchanged |
| `filter_segment`, `segment_chip`, `controls::segment` | `style.rs`, `controls.rs` | no focus state (selection is not focus) | unchanged |
| `filter_dropdown`, picker triggers | `style.rs`, `controls.rs` | no focus state (`open` = pressed) | unchanged |
| Keycaps (`hotkey`, `keycap_label`, the keybindings cap) | `style.rs`, `keybindings.rs` | no focus fill; a capturing cap (the editor's focus) already drew `focus_bands` | unchanged |
| Slider thumb | `controls.rs` | ring + glow as a shadow, **under an opaque thumb** (`opaque().canvas`), so it never showed through | unchanged |
| Task panels' back button | `task_panel.rs` | shadow, plus **an opaque sidebar fill under the button while focused** to hide the shadow (a focus-dependent fill) | `focus_bands`; no focus fill |
| Builder's action handles | `workflow_builder.rs` | ring + glow as bands | unchanged |
| Search boxes | `style.rs` | bands | unchanged |
| Textbox, checkbox | `controls.rs` | edge turns to the ring + a glow band (Figma's Textbox / Checkbox focus) | unchanged |

Two dialog **text fields** had the same shadow under a fill that is
translucent while the window is (`theme.canvas`), so they got bands too
(not buttons, but the same bug in the same dialogs): **Add project**'s
fields (`focus_bands_past`, which steps the bands past the field's own 1px
border, since an absolute child is placed from inside the border) and
**Reject**'s reason area (bands on a holder, since the area clips its
overflow).

Not buttons and left alone: the board's keyboard card (an opaque card, its
`focus_shadow` reads as Figma's selected card); list rows marked as the
keyboard's row with the selection fill (task panels' worktree row and More
link, the Review panel's waiting browser row): that fill is the row
selection the sidebar uses, not a button's focus.

## Tests

- `theme::style::tests::the_primary_button_is_the_figma_primary_style`: the
  Primary kind's fill, hover, pressed and label in Dark and Light are the
  Figma values above; Success keeps the green it had.
- `theme::style::tests::focus_never_changes_a_buttons_fill_or_label`: for
  every kind in both themes, a focused `button` has the same label color as
  at rest (the kind's own), sets no background of its own (the fill is the
  hover layer, built from the same three colors either way), and draws no
  box shadow; `small_icon_button` and `icon_label_button` draw none either.
- `theme::palette::tests::roles_are_the_figma_tokens` pins the four primary
  roles; the contrast and theme-doc tests cover them (two new exceptions,
  four new rows in `docs/theme.md`).

## Renders (`--visual-figma`)

- `components/buttons-<theme>.png`: a new sheet, every kind (Primary,
  Success, Default, Danger, Select, Quiet) at rest and keyboard-focused side
  by side. The harness checks each pair: the pixel 3pt inside the focused
  button's edge matches the resting one (fill unchanged), and the pixel 3pt
  outside it differs (ring and glow drawn). All 24 checks pass. With the old
  shadow restored, all six fill checks fail (dark: `#55c8f8` focused vs
  `#2a4254` at rest for Primary).
- `state-review-confirm-load-focused-<theme>.png`: Load for review with one
  free slot, as it opens (the keyboard on Load): the Primary tint, the cyan
  label, the ring and glow.
- `state-review-confirm-cancel-focused-<theme>.png`: the same after Tab:
  Load at rest in the Primary style, Cancel ringed on its Default fill.
- `SHELBI_VISUAL_ONLY=buttons` renders just these (the component sheets and
  the two dialog states).

## Verification

`cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo clippy
--all-targets --features visual-check -- -D warnings` clean; `cargo test`
698 passed. The full `--visual-figma` run (both themes, 186 frames) reports
0 failed checks.

## Not changed

- gpui-component's own `primary` color (`theme/mod.rs`, for gpui-component
  widgets) still maps to the Success roles; no app button reads it, and
  remapping it would restyle gpui-component internals outside this task.
