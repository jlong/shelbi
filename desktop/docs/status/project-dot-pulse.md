# project-dot-pulse: the project dot shows work in progress

## What landed

A project's status dot (the desktop's tabs, the `+` menu and the chooser,
both palettes' Projects column) now says whether the project is loaded and
whether an agent is at work in it. It no longer marks the current project:
the selected tab and the menu's check do that.

```
tabs:      │ ◉ Shelbi │ ● ChartMe │ ● Website │ + │     ◉ = green dot breathing (work in progress)
+ menu:    │ ○ docs-site │ ○ old-app │                 ● = solid green (loaded, idle)
                                                       ○ = gray ring (not loaded)
```

- **The rule** (`shelbi_app::overlay::palette::ProjectIndicator`):
  `Working`, `LoadedIdle`, `Unloaded`, decided by
  `project_indicator(loaded, working)`. `command_model::project_rows` applies
  it for every list from a `ProjectLiveness` (the open set, and the loaded
  projects with work in progress); being current only sets
  `ProjectEntry::current`, the menu's check.
- **Work in progress** (`shelbi_state::project_is_working`): one of the
  project's dev workspaces holds an active card and reads as the sidebar's
  green Working badge (`slot_reads_working`: its `status.yaml` says working
  for that card, or hasn't caught up with a fresh dispatch yet). It reads the
  board as `BoardOccupancy::read` does (the daemon's index for a remote
  backend) and the busy slots' status, for every open project. No board
  polling.
- **Kept current** (desktop, `projects.rs`): the catalog holds the liveness.
  `Catalog::watch_changes` follows the daemon's change stream for every
  project (`shelbi_app::changes::spawn_all`, a hub `subscribe` with no
  project); after each burst of board or workspace changes it re-reads the
  liveness and, when it moved, every window syncs its tabs and repaints
  (`app::liveness_changed`). No timer.
- **The pulse**: `Theme::project_pulse` (the shared tokens' raised cosine,
  `pulse_trough` to `pulse_peak`) at `shelbi_app::theme::PROJECT_PULSE_PERIOD`
  (1143ms, the TUI's). The desktop animates only the 16pt `circle-solid`
  disc's color (`views/project_menu.rs::dot`), starting at the peak, so the
  frame gpui holds under Reduce Motion (a repeating animation's start) is the
  brightest. The TUI keeps its glyphs: a pulsing `●`, a solid green `●`, a
  gray `○`.
- **Visual check**: `state-tabbar-working` (`SHELBI_VISUAL_ONLY=sidebar`)
  draws a working tab beside idle ones, under Reduce Motion.
