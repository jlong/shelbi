# sidebar-project-name-search-collapse: the project's name in the header, one Search

## What landed

A follow-up to [`tabbar-search-sidebar-header`](tabbar-search-sidebar-header.md),
from the `Navigation` component set (file `pu3hxHgWoCDPCzJ4u6zTQK`, node
2220:17016; the default sidebar's header row 2196:6837) and the tab bar's
search box (2829:26790 in `Orchestrator · Dark`, 2792:33100).

```
Few tabs:
│ ● ● ● │▓ ● Shelbi ▓│ ● ChartMe │ + │                        [🔍 Search  ⌘K] │

Too many tabs (the box collapses):
│ ● ● ● │▓ ● Shelbi ▓│ ● ChartMe │ ● ContextStore │ ● Website │ ● Docs │ + │ 🔍 │

Sidebar header:          Task panel header:          Settings header:
│ My project       ▣ │   │ ←  ◐ IN PROGRESS    ▣ │   │ ←  Settings          ▣ │
  (Large Semibold)                                    │ My project             │
```

- **The search box collapses** (`views/tab_bar.rs`). `tab_bar::search_fits`
  adds up the bar from the tabs' laid-out widths: the lead (and the Toggle
  sidebar's room while the sidebar is hidden), each tab (`tab_width`: its
  padding, dot and gap around the name measured in Base/Medium by
  `label_widths`, capped at 240pt) and its rule, the `+` tab and its rule,
  then a 16pt gap (`SEARCH_MIN_GAP`), the 170pt box and its 10pt. When that
  is wider than the window, the box draws as a 32pt toolbar button with the
  20pt magnifier (`tab-bar-search-button`) in the same spot, 10pt from the
  right edge and centered in the bar. It opens the palette and its tooltip
  names the chord (`Search (⌘K)`). The window decides it every frame, so
  closing a tab or widening the window brings the box back at once. Tabs
  that still don't fit beside the button shrink and truncate, as before (no
  tab scrolling).
- **The box's fill is `surface/depth-20`**: the overlay fill
  (`theme.background`, `button/default/background`, Slate/400 at 18% in Dark
  and 12% in Light), with the button background's hover and press fills. It
  had been `divider_strong` (`stroke/medium`, 20% in both themes), which read
  brighter than the frame.
- **No Search in the sidebar's header rows.** The default sidebar, the
  Workspace and Review panels and Settings keep only their Toggle sidebar,
  8pt off the right edge. The frames still draw a magnifier there; the user
  asked for it gone, since the tab bar's search box is the one Search.
- **The default sidebar's header says the project's name**
  (`sidebar::header_title`, the model's `project_label`, which is also the
  selected tab's label, a remote window's far project's included) in
  Large/Semibold (Geist 16 / 24, 600, the new `Type::LargeSemibold`), still
  8pt in inside its 28pt label, an ellipsis cutting a long name short of the
  toggle. Plain text: a click does nothing.
- **Settings** sets its project section's name and the header's "Settings"
  title in the same Large/Semibold (the title was 18 / 28 semibold).

Selectors: `tab-bar-search` (the box), `tab-bar-search-button` (the
collapsed button), `sidebar-header-title`, `sidebar-header-name` (the name
inside it), `settings-title`, `settings-project-label`. Gone:
`sidebar-search`, `ws-search`, `review-search`, `settings-search`.

## Decisions

- **Measured, not counted.** The fit is computed from the tabs' names shaped
  by the window's text system, not a tab count, so long and short names and
  the window's width all count. The ⌘N badges (only while ⌘ is held) are
  left out, so holding ⌘ never swaps the box.
- **The search plumbing went with the buttons.** `sidebar::HeaderAction`,
  `sidebar::search_button` and the `sidebar::PaletteChord` global only served
  the header rows' Search, so they are gone; the task panels now emit
  `WorkspaceEvent::ToggleSidebar` / `ReviewEvent::ToggleSidebar`, and
  `sidebar::header_row` takes the lead and the toggle only.
- **The collapsed button has no resting fill**, like every other toolbar
  button; it fades in the button's hover and press fills.
- **The visual run's crowded state** (`state-tabbar-search-collapsed`) uses a
  catalog of ten open projects of its own, then puts the frame's catalog
  back for the other states.
