# tab-context-menu: a project tab's context menu

## What landed

Right-clicking a project tab opens a small menu at the pointer:

```
│▓ ● Shelbi ▓│ ● ChartMe │ ● Website │ + │
               ┌──────────────────────────────┐
               │ Close Project                │   <- right-click on "ChartMe"
               │ Close All Other Projects     │
               └──────────────────────────────┘
```

- **The menu** (`views/tab_bar.rs`, `TabMenu` / `TabMenuProps`): placed at
  the pointer 2pt under it (`elements::placed`, which flips and shifts it to
  stay in the window), 240pt wide, on the project menu's fill with its 26pt
  rows (`project_menu::text_row` / `disabled_text_row`), sliding and fading
  in as the `+` menu does. A press outside it (`Placed::on_dismiss`) or Esc
  closes it; ↑↓ and Enter work too. The `+` tab has no menu. The right-click
  sets the menu's anchor itself, then sends `SidebarEvent::OpenTabMenu`.
- **Close Project** closes that tab's project through the `×` path
  (`confirm_close_project`): the Close Project sheet, the daemon quits it,
  every window drops its tab. It needn't be the shown tab.
- **Close All Other Projects** (`confirm_close_other_projects`) asks once,
  with one sheet naming how many and which projects close and that their
  agents stop (`shelbi_app::close_others::confirmation`, the TUI's copy too).
  On confirm the right-clicked tab is selected first, then each other tab's
  project quits in turn through the same quit and its tab goes from every
  window (`project_quit_landed`, shared with Close Project). With one tab the
  row is dimmed and does nothing.
- **The palette and the menu bar**: `action:close-other-projects`
  ("Close All Other Projects", Quit Project's 🏁 / `×` icon) is a shared
  registry command, listed while another project is open
  (`CommandModel::open_others`: this window's other tabs). It keeps the shown
  project. File ▸ Close All Other Projects… runs it, gray with one tab.
- **The TUI**: the same registry command in its palette, over the open
  projects other than its own (`PaletteInputs::open_others`). One confirmation
  dialog (`overlay/close_others.rs`, the review dialog's buttons), then the
  quits run off the UI thread (`ShellLifecycle::close_project`) and the status
  line says how it went. The TUI stays on its project.
- **Remote windows**: Close Project still shows the tab before closing it.
  Close All Other Projects quits each far project over a control stream
  opened for that project (`ProjectLink::quit_far_sibling`), which the relay
  lets quit its own project, so no tab has to be shown first.

## Tests

- `app_tabs_tests.rs`: the menu opens at the pointer on a tab and not on `+`,
  closes on Esc and on an outside press; Close Project asks for the
  right-clicked tab's project while another is shown; Close All Other
  Projects asks once, naming them, and selects the kept tab; the dimmed row
  and the palette's availability with one tab; the palette's command keeps
  the shown project after one sheet.
- `app_remote_tests.rs`: a remote window's Close All Other Projects quits the
  far sibling only, after one sheet, and stays on its project.
- `crates/shelbi-app/tests/command_scenarios.rs`: the command's effect,
  "close other" / "close all" find it, hidden with no other project open.
- `crates/shelbi-tui`: the dialog's keys and clicks, and the shell's one
  confirmation then a quit per project.
