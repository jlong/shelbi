# new-tab-launch: the `+` opens a New tab showing the launch screen

## What landed

The tab bar's `+` no longer opens a project menu. It adds a tab titled
**New tab** after the last one and selects it (since `new-tab-plus-morph`
the `+` itself turns into it); its content is the launch
screen (Figma "Startup · Projects and remote sessions" 2539:10949 and
"Startup · No projects yet" 2544:10318), drawn under the tab bar instead of in
a window of its own:

```
┌──────────────────────────────────────────────────────────────────┐
│ ● ● ●  ● shelbi   ● site   ○ New tab  ×  │ + │                    │
├──────────────────────────────────────────────────────────────────┤
│   Welcome to Shelbi              │  Projects on this Mac          │
│   Pick a project or connect      │  📁 shelbi   ~/Workspaces/…     │
│   to a remote session.           │  📁 site     ~/Workspaces/…     │
│                                  │  Recent remote sessions        │
│   [Add project]                  │  devbox  jlong@devbox:… Connect│
│   [Connect to remote]            │                                │
└──────────────────────────────────────────────────────────────────┘
```

- **One view, two hosts** (`views/launch.rs`): `LaunchView` is the launch
  window's root view and the New tab's content. `Host::Window` (`new`) keeps
  the window chrome it always had (its menu bar, translucency, ⌘W closing it,
  the lists starting under the title bar); `Host::Tab` (`in_tab`) leaves those
  to the project window and starts the lists at the panel's inset. Where each
  pick goes is the view's `Deps` (`open`, `add`, and the new `remote`), which
  the window overrides for its tab (`ShelbiWindow::new_tab_deps`). The layout,
  keys (↑/↓, Return) and the Connect dialog are the same code.
- **The tab** (`project_tabs::NEW_TAB`, `/new-tab`: a `/` can't be in a project
  id): one per window. A second `+` selects it. It draws "New tab" beside the
  gray ring (no project, so no status), has no context menu, and closes with
  its `×` (no prompt, nothing stops) or ⌘W; the window moves to its neighbor
  tab, or to the empty-state chooser when it was the only tab. It is never
  saved with the window (`ProjectTabs::projects`), the open set never drops it,
  and Close All Other Projects doesn't count it.
- **While it is selected** the window shows no project (`chooser` is set, as
  for the chooser, plus `new_tab`): the sidebar is hidden and the launch
  screen fills the window under the bar. The menu bar and palette are the
  no-project ones. Hiding or showing the sidebar still toggles (⌘\ and the
  bar's button) and applies to the next project tab.
- **A pick becomes the tab** (`app::new_tab_chose`):
  - a project with no tab opens in the New tab's place, same position,
    selected (`ShelbiWindow::switch_to` from the New tab replaces it; the
    launch screen still starts the daemon and stamps the project first);
  - a project with a tab in this window: that tab is selected, the New tab
    goes;
  - a project with a tab in another window: that window comes up on it, and
    the New tab goes (with its window, if it was the window's only tab);
  - Add project runs the wizard from this window, and its project lands in
    the New tab the same way (`project_added` → `switch_to`);
  - Connect to remote and a recent session's Connect open the remote window
    as the launch window does, and the New tab goes: a window's tabs are one
    machine's projects, so a remote project can't sit in a local window's
    tab. A local project picked in a remote window's New tab opens in a local
    window for the same reason.
- **Drag, reorder, tear off**: superseded by `new-tab-plus-morph`
  ([`new-tab-plus-morph.md`](new-tab-plus-morph.md)): the New tab is now the
  `+` slot itself, pinned at the bar's end, and never drags, reorders or
  tears off (`open_new_tab_window` is gone).
- **Gone**: the `+` project menu (`tab_bar::ProjectMenu`, its keys, the
  `ToggleSwitcher` / `CloseSwitcher` / `AddProject` sidebar events). A bare
  Switch Project (`HostCall::OpenNewTab`) opens the New tab. File ▸ Show Launch
  Screen and the startup launch window are unchanged, and the "launch window
  closes when a project window opens" rule closes only that window (and never
  fires for a New tab's window).

## Tests

- `app_new_tab_tests.rs`: `+` adds and selects one New tab showing the
  launch screen (a second selects it); the launch view's keys; another tab
  keeps the New tab to come back to (click, ⌘N); a launch-screen pick (through
  the real click, a fake daemon) opens in the tab's place; a project with a
  tab here or in another window; the wizard's project; a remote session; `×`,
  ⌘W and the only-tab chooser; never saved, never dropped, not counted by
  Close All Other Projects; tear-off into a picker window and reorder.
- `project_tabs.rs`: `NEW_TAB` survives reconcile and is left out of
  `projects()`; `replace`.
- `tab_bar.rs`: the New tab's ring. `app_tabs_tests.rs`: the `+` opens a New
  tab, no menu.
- Visual: `--visual-figma` saves `state-new-tab` (the old
  `state-sidebar-switcher`); `--visual-review` drops the switcher frames.
