# search-back-to-sidebar: Search goes back to the sidebar's header

## What landed

The tab bar's "Search ⌘K" box clashed with the search fields on the screens
under it (the Issues board's own Search, with New issue right below), so it
is gone, and Search is back as an icon button in the column's header row,
following the updated `Navigation` component set (file
`pu3hxHgWoCDPCzJ4u6zTQK`, node 2220:17016). This partly reverses
[`sidebar-project-name-search-collapse`](sidebar-project-name-search-collapse.md)
and `search-box-no-tooltip`: the project's name in the header and the tab
bar's hidden-sidebar toggle stay.

```
Tab bar (nothing at the right end):
│ ● ● ● │▓ ● Shelbi ▓│ ● ChartMe │ + │                                          │

Default / Multi-machine:     Review:                      Settings:
│ My project       🔍 ▣ │   │ ←  ◐ IN REVIEW     🔍 ▣ │   │ ←  Settings       🔍 ▣ │

Workspace task panel (the frame hides Search):
│ ←  ◐ IN PROGRESS      ▣ │
```

- **The tab bar draws no search** at any width or tab count
  (`views/tab_bar.rs`): the box, the collapsed magnifier, `search_fits`,
  `label_widths`, `tab_width` and the box's constants are gone, and so is
  the `palette_chord` prop. The tabs strip runs to the bar's right end, as
  before (it shrinks and truncates tabs that don't fit; no scrolling). The
  hidden-sidebar Toggle sidebar before the first tab is unchanged.
- **The header rows** (`sidebar::header_row(lead, pad_left, search, toggle)`,
  `search: Option<_>`): Search and Toggle sidebar, 32pt toolbar buttons with
  20pt icons, 4pt apart, 8pt from the right edge, at x 204 and 240 in the
  280pt column, as State=Default (2164:4, Toolbar 2196:6837),
  Multi-machine (2679:36984), Review (2220:17158) and Settings (2252:27532)
  place them. State=Workspace (2220:18764) hides its Search, so the
  Workspace panel passes `None` and keeps the toggle alone.
- **Search opens the command palette** and its tooltip names the palette
  chord: `Search (⌘K)` by default (`sidebar::search_tip` over
  `palette::hint_chord`, unchanged, which also feeds the palette footer).

Selectors: `sidebar-search`, `review-search`, `settings-search` (back);
`ws-search` stays absent. Gone: `tab-bar-search`, `tab-bar-search-button`,
`tab-bar-search-label`, `tab-bar-search-chord`.

## Decisions

- **The simplest plumbing back, not all of it.** `sidebar::search_button`
  and the `sidebar::PaletteChord` global (set in `ShelbiWindow::new` and
  `adopt_keymaps`) return; `HeaderAction` doesn't. Only the Review panel
  needs a Search event, so `ReviewEvent` gains `OpenSearch` beside its
  `ToggleSidebar`, and `WorkspaceEvent` keeps `ToggleSidebar` alone. The
  sidebar and Settings use `SidebarEvent::OpenSearch`, as before #2269.
- **The visual run's crowded-tabs state** (`state-tabbar-search-collapsed`)
  and `ShelbiWindow::offscreen_sync_tabs`, which only it used, are gone.
- The shared `tooltip` debug selector from `search-box-no-tooltip` stays:
  the new `the_sidebar_search_opens_the_palette_and_names_its_chord` test
  uses it.

## Tests

- `app_tabs_tests`: `the_tab_bar_has_no_search_at_any_width_or_tab_count`
  (two and four tabs, wide and narrow windows, the sidebar hidden), the
  sidebar header's Search and Toggle geometry, and
  `the_sidebar_search_opens_the_palette_and_names_its_chord`.
- `sidebar` render tests: Search at (204, 13), 32pt, a click reaching the
  host as `OpenSearch`, a long project name ellipsized short of Search.
- `app_column_tests`: the Review panel's Search beside its toggle opens the
  palette; the Workspace panel draws no Search.
- `app_settings_tests`: Settings' Search beside its toggle, and
  `the_settings_search_opens_the_palette`.
