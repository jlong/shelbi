# tabbar-search-sidebar-header: the search box and the sidebar's header row

## What landed

A follow-up to [`project-tabs`](project-tabs.md), from the Figma
`Orchestrator · Dark` frame (file `pu3hxHgWoCDPCzJ4u6zTQK`, node 2792:33100;
the tab bar 2792:33339, its search box 2829:26790) and the `Navigation`
component set (2220:17016: State=Default 2164:4, Workspace 2220:18764,
Review 2220:17158, Settings 2252:27532).

```
┌──────────────────────────────────────────────────────────────────────────────────────┐
│ ● ● ● │▓ ● Shelbi ▓│ ● ChartMe │ ● ContextStore │ ● Website │ + │       [🔍 Search  ⌃P] │ 52pt
├──────────────────────┬───────────────────────────────────────────────────────────────┤
│ Project        🔍 ▣  │  58pt header row                                              │
│ 💬 Orchestrator   ⌘O │                                                               │
│ ☐ Issues          ⌘I │                                                               │
│ 🚀 Changelog      ⌘L │                                                               │
```

- **The tab bar** (`views/tab_bar.rs`) has no left toolbar. The first tab's
  rule sits 17pt past the real traffic lights (`tab_bar::LEAD_X`, 96pt), as
  the frame puts it 17pt past its drawn ones.
- **The search box** at the bar's right end: 170 x 32, 10pt in from the
  window's edge, centered in the bar, on `surface/depth-20` (the theme's
  `divider_strong`, Slate/400 at 20%) with 6pt corners, 6pt / 8pt padding and
  a 6pt gap: the magnifier, "Search" (Base on a 22pt line, muted), and the
  palette's chord from the keymap in a key cap (Source Code Pro 12 / 16,
  muted, 8pt either side). It is a button, not a field: a click opens the
  palette. Hover deepens its fill and turns the magnifier loud, as the
  toolbar buttons do.
- **The header row** (`sidebar::header_row`): 58pt, the content 12pt in
  (16pt for the task panels' back button), then Search and Toggle sidebar,
  32pt with 20pt icons, 4pt apart and 8pt off the right edge. The default
  sidebar's reads "Project" (Base Medium, a 28pt label 8pt in; plain text).
  The nav follows with its 8pt top padding (the frame's 2164:21 at y 58).
  The Workspace and Review panels keep their back button and status
  (`task_panel::status_row`, now the header row) and Settings its back
  button and title; each adds the two buttons.
- **Hidden sidebar.** The header goes with the sidebar. While it is hidden,
  a 32pt Toggle sidebar button sits in the tab bar at `LEAD_X`, before the
  first tab (16pt to its rule). Its room opens and closes on the sidebar's
  own reveal (`TOGGLE_ROOM` scaled by how hidden the sidebar is, the button
  fading with it), so the tabs step right as the sidebar leaves and back as
  it returns. View ▸ Toggle Sidebar (⌃⌘S) is unchanged.

Selectors: `tab-bar-search` (the box), `tab-bar-search-label`,
`tab-bar-search-chord`, `tab-bar-sidebar` (only while hidden),
`sidebar-header`, `sidebar-header-title`, `sidebar-search`,
`sidebar-toggle`, `settings-header`, `settings-search`, `settings-toggle`,
`ws-search`, `ws-toggle`, `review-search`, `review-toggle`.

## Decisions

- **The task panels emit, the window acts.** The Workspace and Review views
  aren't the window, so their header buttons emit
  `WorkspaceEvent::Header` / `ReviewEvent::Header` (`sidebar::HeaderAction`),
  which the window maps to the palette and the sidebar toggle. Their Search
  tooltips read the palette chord from a `sidebar::PaletteChord` global the
  window sets with its keymaps, rather than plumbing it into each view.
- **A panel's toggle says "Toggle sidebar".** A task panel also stands in a
  hidden sidebar's column, so its button can show or hide; the sidebar's and
  Settings' say "Hide the sidebar", the tab bar's "Show the sidebar".
- **No 44pt gap in Settings.** State=Settings keeps an empty 44pt
  "Project and search" frame between its header row and Global, left from
  the old switcher row. The header row replaces that row, so the sections
  follow it directly, as the other states' content does. The title now sits
  at the frame's 12pt past the back button (it had 4pt more).
- **No key cap shadow.** The frame's Kbd has a 7% drop shadow under its text
  that doesn't read at 1x; gpui's shadow is a filled box, so it is left out.
- **Chords stay ⌃1 to ⌃9** on the sidebar's rows; the frames' ⌘N badges
  predate `project-tabs`.
