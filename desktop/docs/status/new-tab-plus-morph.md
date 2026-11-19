# new-tab-plus-morph: the `+` turns into the New tab

## What landed

The tab bar's `+` no longer adds a separate "New tab" after the last project
tab. Clicking it turns the `+` slot itself into the New tab: the `+` rotates
45° counterclockwise into an `×` while "New tab" slides out to its right and
the slot widens to fit. The `×` closes it, turning back into the 52pt `+`.
Since the `+` is gone while the New tab is open, a window can only have one
New tab, and the bar shows that.

```
 ● ● ●   │ ● alpha │ ● beta │  +  │                 (no New tab)
 ● ● ●   │ ● alpha │ ● beta │  ×  New tab │         (the `+` turned into it)
```

- **The morph** (`views/tab_bar.rs`, `PlusMorph`): the window records which
  way the slot last turned and when (`ShelbiWindow::plus_morph`); its phase
  runs on `motion::ease_out_expo` over `motion::TAB_MORPH` (240ms, the menu
  range). One glyph turns (Heroicons `plus` rotated by -45° × phase), never
  two cross-fading. The slot's width is the 52pt square plus the phase's
  share of the label's measured width (`TabLayout::plus_label_w`, painted
  from the click on so it is known before it shows), and the label slides
  from under the `×`, clipped there and at the tab's edge. Under Reduce
  Motion every change is instant. Only the slot changes width, and it is
  after every project tab, so no tab moves.
- **Only a click animates**: the `+` and the `×` (or ⌘W on the New tab)
  turn the slot. A project picked on the launch screen takes the New tab's
  place (the last project tab) and a plain `+` appears after it with no
  reverse turn; so does a New tab left because its pick came up in another
  window. `PlusMorph::phase` draws a New tab that came or went without
  `animate` at its end state.
- **Pinned last** (`project_tabs.rs`): `NEW_TAB` is always the last entry
  in the order. `insert` and `insert_at` put projects before it, `move_to`
  never moves it or past it, and `strip()` (the order minus the New tab) is
  what a tab drag reorders and judges gaps against. ⌘N still counts it after
  the projects.
- **No drag, no tear-off**: the slot has no drag handler; the tear-off path
  for the New tab (`open_new_tab_window`) is gone. The drop gap over the
  `+` slot uses its measured width (`TabLayout::tail_w`), so no gap opens
  over a widened New tab, and a tab dropped past it lands before it.
- **Selection**: selected (main area fill) while its launch screen shows,
  unselected like any tab otherwise; a click on its body selects it. The `×`
  hovers like a project tab's (16pt box, hover fill, gray to loud). Tooltips:
  "New tab" on the `+`, "Close tab" on the `×`.
- **Selectors**: `tab-plus` (the slot while it is a `+`), `tab-new-tab`
  (while it is the New tab), `tab-close-new-tab` (its `×`),
  `tab-new-tab-label` (the clipped label).

## Tests

- `app_new_tab_tests.rs`: the slot turns and widens on the curve (hand-run
  clock) with the tabs before it unmoved, turns back on `×`, is instant
  under Reduce Motion (⌘W too), no `+` while the New tab is open, a pick
  turns it into the last project tab with a plain `+` at once, it never
  drags or tears off, and a tab dropped past it lands before it.
- `project_tabs.rs`: `the_new_tab_stays_last`.
- `tab_bar.rs`: `PlusMorph` phases and the slot width.
- Visual frames (checked by eye): `SHELBI_VISUAL_ONLY=sidebar` writes
  `state-plus-morph-open-{0,20,60,120,240}` and
  `state-plus-morph-close-{0,20,60,240}`.
