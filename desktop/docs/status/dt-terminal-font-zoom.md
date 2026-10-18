# dt-terminal-font-zoom: ⌘+ / ⌘− / ⌘0 size the terminal font

## What landed

⌘+ (and ⌘=) grows the terminal font by a point, ⌘− shrinks it by a point,
and ⌘0 resets it to the default (14pt, the Figma terminal size), as in
Terminal, iTerm and Ghostty. The View menu has the same three items:

```
View
  Command Palette        ⌃P
  Hide Sidebar           ⌃⌘S
  ─────────────
  Focus Sidebar          ⌃H
  Focus Main Area        ⌃L
  ─────────────
  Bigger                 ⌘+     (new)
  Smaller                ⌘-     (new)
  Actual Size            ⌘0     (new)
  ─────────────
  Turn Zen Mode On       ⌥Z
  Error Log
```

Window ▸ Zoom stays the window zoom (it was never in the View menu; the
font items sit in View, where Terminal puts them).

- **The step** (`elements/terminal/settings.rs`, `FontZoom::apply`): a point
  at a time within `ZOOM_RANGE` (8 to 32). At a range end the key does
  nothing (no error). A size set in Settings outside the range (Settings
  accepts 6 to 72) steps back inside on a key toward the range (⌘− at 40 gives
  32) and stays put on a key away from it. Actual Size writes "no override"
  (`desktop.terminal_font_size` removed), not 14, so the default can move
  later; with no override set it does nothing.
- **Every pane at once** (`TerminalSettings::zoom`): the step sets the
  `TerminalSettings` global, the same one a Settings save sets. Every
  `TerminalPane` observes it (`settings_changed`): the cell is re-measured on
  the next frame and the new grid size is reported to the PTY through the
  existing resize path. That covers every window (the global is app-wide)
  and every kind of pane (orchestrator, workspace, review, shell). A pane
  that isn't drawn (another view) re-measures when it is next shown.
- **Persisted** (`SizeWriter`): the new size is written to
  `desktop.terminal_font_size` through `shelbi_state::update_user_config`,
  off the UI thread, one write at a time; presses faster than the writes
  coalesce and the last size lands. Settings reads the same key, so the
  next launch and the Settings form agree. An open Settings view follows a
  zoom live (`SettingsView::font_size_changed`, observing the global), so its
  next save keeps the zoomed size instead of writing back the old one.
  `TerminalSettings` gained `font_size_setting` (what the config sets, `None`
  for the default) so the form can show a blank field after ⌘0 and "14"
  after a ⌘− that lands on 14.
- **The chords are Shelbi's** (`keys::font_zoom`, `app.rs`): the window's
  key handler takes them first, wherever focus is (an overlay doesn't bind
  them, so they work with the palette up too). `takes_from_terminal`
  includes them, so the chat's capture phase takes them before its terminal.
  The terminal's own key path (`elements/terminal/keys.rs::classify`)
  already ignores every Cmd chord but ⌘C ⌘V ⌘F, so nothing reaches the
  program either way. ⌘+ on a US layout is Shift+=, which macOS (and gpui)
  reports as `+` without Shift; `font_zoom` accepts `⌘+`, `⌘=` and a
  `⌘⇧=` a layout might keep the Shift on.
- **The menu items** are `AppCommand::FontBigger` / `FontSmaller` /
  `FontActualSize`: client actions, not registry commands (the TUI has no
  font to size, so there is no `CommandKind` and no parity row). They go
  through `MenuInvoke` with the existing never-matching label bindings for
  their key equivalents, and `menus::run_app_command` runs them (no window
  needed). `menus::tree::keystroke` now renders a `-` key (`cmd--`; gpui's
  parser takes a trailing `-` as the key), which it used to refuse.

## Verified

- `cargo build`, `cargo clippy --all-targets -- -D warnings` (also with
  `--features visual-check`), `cargo test`: 433 passed after the rebase onto dt-figma-activity. No `crates/` change,
  so the root workspace checks don't apply.
- New tests: `FontZoom::apply` steps and ends
  (`zoom_steps_a_point_within_8_to_32_and_actual_size_clears_the_setting`),
  the global moving at once and the last size saved
  (`zoom_applies_at_once_and_saves_the_last_size`), the chord table
  (`cmd_plus_equals_minus_and_zero_are_the_font_zoom`), the terminal
  ignoring the chords (`cmd_chords_are_the_apps_except_copy_paste_and_find`),
  the View menu (`view_has_the_terminal_font_zoom_beside_the_window_zoom`),
  the `-` keystroke round trip, and the window path end to end: keys through
  the real window (also with the palette up) and the menu items
  (`the_font_zoom_chords_and_menu_items_step_the_terminal_font`).
- `--visual-check` (a live pane, both themes): 0 failed, including the
  existing "a font size change re-measures the cell and reports the new
  grid" (the path the zoom drives) and a new row: `cmd-+ cmd-= cmd-- cmd-0`
  sends nothing to the program.
- No Figma screen changes: the only visible change is the native View menu,
  which AppKit draws, so there is nothing to diff against the reference
  PNGs.

## Not verified

- A hand run in the real app: posted key events don't reach a background
  app on this machine, so the native menu's key equivalents (⌘+ shown as
  `⌘+`) and a real US-layout ⌘+ press were not driven by hand. The gpui
  event conversion was read (`gpui-pre-macos` `events.rs`: Shift+= under Cmd
  is reported as key `+`, Shift cleared) and the tests use that form.
