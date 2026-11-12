# project-tabs: a tab per open project

## What landed

The desktop window's project switcher (the project name and up-down chevron
under the sidebar's titlebar row) is replaced by a row of tabs across the top
of the window, one per open project: the Figma `Orchestrator · Dark` frame
(file `pu3hxHgWoCDPCzJ4u6zTQK`, node 2792:33100; the bar is 2792:33339).

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ● ● ●  🔍 ▣ │▓ ● Shelbi ▓│ ● ChartMe │ ● ContextStore │  +  │                 │  52pt
├──────────────────────┬───────────────────────────────────────────────────────┤
│ 💬 Orchestrator   ⌘O │                                                       │
│ ☐ Issues          ⌘I │   (the selected project's main area)                  │
│ 🚀 Changelog      ⌘L │                                                       │
│ Workspaces ⌄         │                                                       │
│ ▶ alpha Developer ⌃1 │                                                       │
└──────────────────────┴───────────────────────────────────────────────────────┘
```

- **The bar** (`views/tab_bar.rs`): 52pt, the full window width, over both
  the column and the main area. Left to right: the native traffic lights,
  the toolbar (Search, which opens the palette, and Toggle sidebar, moved
  here from the sidebar's titlebar row), then a 1pt rule, each tab and a
  rule after it, then the 52pt `+` tab and a last rule. A tab is a 16pt
  green dot and the project's display name (Base Medium) with 16pt / 20pt
  padding and an 8pt gap. The selected tab fills with the main area's color
  and covers the bar's bottom rule, so it reads as joined to the content.
  The bar stays where it is while the sidebar hides and shows; the collapsed
  main area's own titlebar row (`MainTitlebar` and its Show button) is gone.
- **Which tabs** (`project_tabs.rs`, toolkit-free): one per open project
  (`shelbi_state::list_open_projects`, through the catalog), in the order
  they were opened. The order is saved in the window slot
  (`WindowState::tabs` in `desktop-windows.json`), so it survives a restart.
  A project newly found open goes at the end; one quit elsewhere loses its
  tab; the window's own project always has one. The catalog is re-read when
  a window comes forward, when the `+` menu opens, after a project opens and
  after one closes, and every window follows it.
- **Click** a tab: the same `CommandKind::SwitchProject` as the palette.
- **Close**: hovering a tab swaps its dot for an `×` (16pt). A click is the
  existing Quit Project path for that project (confirmed, then the daemon
  quits it), now named Close in the prompt; so is the new File ▸ Close
  Project… item (the registry's `action:quit-project`). Every window loses
  the tab; a window that showed it moves to its right neighbor tab, else
  the left one, else another open project, else the empty-state chooser (as
  before). A remote window shows a tab before closing it, since its link
  reaches only the shown project's far daemon.
- **The `+` tab** opens the shared project menu (`views/project_menu.rs`)
  under it: the "Projects" heading, a row per project without a tab (gray
  ring), a rule, then "Add project" (the wizard). With every project open,
  only "Add project". Picking a project opens it in a new tab and selects
  it. ↑ ↓ Enter Esc work as in the old switcher. File ▸ Switch Project… and
  a bare Switch Project open it. The palette's Projects column is unchanged:
  switching to a project that isn't open opens it and adds its tab.
- **Keys**: ⌘1 to ⌘9 select tabs 1 to 9. Holding ⌘ (alone) shows each tab's
  `⌘N` badge (the frame's hidden `Hotkey`). The sidebar's numbered rows and
  the task panels' views move from ⌘1 to ⌘9 to ⌃1 to ⌃9, and their badges
  read `⌃N`. Settings keeps ⌘N for its pages while it is open; ⌃N does
  nothing there. A ⌃ digit the user's `keys.yaml` binds in its global or
  sidebar keys stays theirs.
- **The sidebar** starts under the bar: its titlebar row and the project
  switcher are gone, and the nav (Orchestrator, Issues, Changelog) is its
  top row, 20pt down as in the frame. A remote window's host line moves
  above the nav. The settings sidebar loses its titlebar row too, and its
  project section's switcher becomes the project's name as a label.

## Decisions

- **Colors from the theme's roles, not the frame's hex.** The bar's
  `slate/700` is the overlay fill (`button/default/background`) over the
  canvas, which lands within a point or two of `#373e53` in Dark and gives
  Light its own light gray; the selected tab is `canvas`, the main area's
  color (the frame's `slate/800` stands for "the main content background").
  No new palette role.
- **The toolbar's x.** The frame puts it at 86pt with its lights ending at
  69; the real macOS lights end at 79 (`sidebar::TRAFFIC_LIGHTS_RIGHT`), so
  the toolbar keeps the frame's 17pt past them: `tab_bar::TOOLBAR_X` = 96.
- **The `×` asks first.** Closing stops the project's agents, so the tab's
  `×` goes through the same confirmation sheet as Quit Project.
- **The chooser and the workflow editor draw no tab bar.** The chooser is
  what a window shows with no project (as before), and the workflow editor
  still takes the whole window with its own titlebar beside the lights.
- **Settings' project switcher became a label** rather than a second way
  into the `+` menu; switching a tab with Settings open still reopens
  Settings on the new project.
- **A remote window's tabs** are the projects it opened (its host's listing
  doesn't say which are open), saved like a local window's.
- **No registry command or view-model field**: the tabs are window chrome,
  so the TUI is unchanged (`parity.md` records the difference).
- `IconButtonSize::Default` went with the Show sidebar button, its only user.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings` and `cargo test`
  in `desktop/` (1148 tests), plus `cargo check --features visual-check`.
- `app_tabs_tests.rs`: the bar's geometry over the sidebar, hiding the
  sidebar keeps it, the saved order and the open set, a restart keeping the
  order, click to switch, the hovered `×` and its confirmation, the `+` menu
  (rows, keys, Add project, every project open), a project opened elsewhere
  getting a tab, ⌘N and the held-⌘ badges, a user's ⌃ digit binding.
  `app_window_tests.rs`: closing selects the right neighbor, else the left.
  The sidebar, fold, column and settings tests moved to ⌃N and the tab
  bar's toggle.
- `--visual-figma` frames (orchestrator, dark and light, and the `+` menu
  state) checked against the frame by eye.
