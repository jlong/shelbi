# dt-settings-sections: Settings gets its own sidebar and three pages

## The change

Settings used to be one long page beside the app sidebar. Following John's
Figma redesign (frame **Settings · Dark** `2165:545`, sidebar instance
`2165:546`, the Navigation component's State=Settings `2252:27532`), it now
has its own sidebar, which stands in the app sidebar's column while Settings
is open, and three pages:

- **General** (⌘1): Theme, Shelbi CLI, Notifications.
- **Keybindings** (⌘2): the Open keys.yaml button with its helper, and any
  binding macOS already owns, laid out as the header of the Keybindings frame
  (`2252:28172`, header `2260:4318`).
- **Terminal** (⌘3): Terminal font, Terminal font size, Option key in
  terminals, in the Terminal frame's 400pt column (`2252:28494`, column
  `2269:4844`), the Option helper a line per sentence as the frame sets it.

The settings sidebar is the frame's: the titlebar row (traffic lights,
Search, Toggle sidebar), the back button beside the **Settings** title, the
**Global** header, and the three nav rows with their icons and chords. The
selected row wears the app sidebar's chip, with the accent icon. There is no
footer (no Zen pill, no settings gear). The main area has no "Settings" title
any more; the page starts at the frame's 32pt padding.

## Scope (John's call, q-001)

The Mockups page has Keybindings and Terminal frames beyond the Settings
frame, and both draw features that don't exist yet: a bindings editor (search,
a context filter, Reset all, click-a-chord-to-rebind rows grouped by
context) and, on Terminal, an ANSI color editor with Reset and a live
Claude Code preview. I asked; John chose to ship the sidebar, the navigation
and the three pages with the existing controls only, laid out per those
frames, and is filing the bindings editor and the ANSI colors + preview as
follow-up tasks. So:

- Keybindings keeps the current helper ("Shared by the TUI and the desktop
  app."), not the frame's "Click a shortcut to change it…", which describes
  the editor.
- The frame's project section (divider, project switcher, Workflows ⌘4,
  Agents ⌘5, Integrations ⌘7) is left out: those pages don't exist.
  `settings_nav::SECTIONS` is a list of header + pages, and each page's ⌘N is
  fixed per page (the frame numbers the project pages 4, 5 and 7, skipping
  6), so that section is one more entry.

## How

- **`views/settings_nav.rs`** (new): `SettingsPage` (label, icon, fixed
  chord, selector), `SECTIONS`, the toolkit-free `SettingsNav` (the page shown
  and the row the keys moved to; the page outlives a visit) and `render`, the
  sidebar. It reuses the app sidebar's titlebar (`render_titlebar`, now
  `pub(crate)`), its `nav_item` and row painting (`style::row_state_rounded`,
  the sliding chip via its own `SlideTrack`), and the task panels'
  `back_button`. Its clicks go through `SidebarEvent::LeaveSettings` and
  `SidebarEvent::ShowSettingsPage`.
- **The column**: `column_swap::Occupant::Settings` is a third panel beside
  Review and Workspace, so Settings gets the same push in and out (a
  crossfade under Reduce Motion) and goes through the sidebar when it swaps
  with a task panel. Unlike the task panels it is a sidebar: it collapses
  with the app sidebar (`column_swap::column`), so Toggle sidebar hides it and
  the main area's expand button brings it back.
- **The window** (`app.rs`): `open_settings(page)` opens on the asked page or
  the last one; `show_settings_page`; `leave_settings` (Back, Esc) returns to
  the view under Settings exactly as leaving a review does (`ShowView` of
  the client's view, focus back on the app sidebar, which selects that
  view's row: `dt-back-syncs-sidebar`'s `selected()`); opened over a review,
  it reopens the review (`ReviewView::info`, new). `nav_chord` sends ⌘1 to ⌘N
  to the settings pages while the column shows Settings, as it sends them to
  a task panel's views; outside Settings they are the sidebar's rows, as
  before. `settings_key` takes Esc, ↑ / ↓ (and Tab / Shift-Tab, the sidebar
  keymap's other bindings) and Enter, only while the window itself holds
  focus, so a `j` typed into the CLI field stays the field's.
- **`views/settings.rs`**: the view keeps a `page` and draws only that
  page's controls. Every behavior is unchanged: saving, the font picker and
  ⌘+ / ⌘− / ⌘0 sync (the observer runs whatever page is shown), CLI Browse,
  Open keys.yaml, the notification toggles. Under `cfg(test)` it reads no
  config or keymaps, so the window tests never touch the real `~/.shelbi`.
- **Icons**: Heroicons `wrench-screwdriver` (General; the Terminal row's
  `command-line` was already vendored), and Figma's `keyboard-key` drawing
  (Keybindings; Heroicons has none) in `assets/icons/figma/`.

### Values matched to the frame

- Title row: 16pt in, 8pt down, 12pt between the 28pt back button and the
  title. The title is Desktop/Heading as the frame reads it, 18 / 28
  semibold. The app's `Type::Heading` is 20 / 28 bold and its other titles
  (task detail, bootstrap screens) keep it; worth a look in a later pass.
- Header: Base/Medium in `foreground`, 20pt in, 8pt above, 4pt below, 28pt.
- Rows: the app sidebar's nav item (34pt, 6pt in, 4pt apart, 12pt block
  inset). The frame's hotkeys sit at the row's padding with no inset of their
  own (measured: Figma's ⌘2 spans x 243 to 261, the app sidebar's hotkey has
  4pt of padding), so these rows drop it.
- General: the Theme track now hugs its items (176pt in Figma) instead of the
  old 400pt.
- Keybindings: the title (Base/Medium, loud) and the Button at Small (28pt,
  13pt medium) on one row, the helper (X-Small, `muted`) 6pt below, 640pt
  wide.

## figma-diff

`scripts/figma-diff.sh` at the default FUZZ 10 on the full 1440x1024 frame,
dark, against the new references (`settings-general-dark.png`,
`settings-keybindings-dark.png`, `settings-terminal-dark.png`). "Before" is
`origin/main`'s Settings render (the single page with the app sidebar)
against the same references.

| screen | before | after |
|---|---:|---:|
| settings-general-dark (the Settings frame) | 7.65% | **1.08%** |
| settings-keybindings-dark | 9.84% | 5.32% |
| settings-terminal-dark | 52.80% | 49.13% |

Split by column on General (ImageMagick `compare -metric AE -fuzz 10%`):
the sidebar (x 0 to 280) differs on 0.29% of its pixels, the main column
on 0.22%.

What's left on General is the frame's project section (left out, see
above), the traffic lights (the offscreen window draws none), and the Geist
1.2 vs 1.7.2 advance widths `dt-figma-qa-activity` measured. Keybindings' and
Terminal's numbers are high by design: their frames draw the bindings list,
the ANSI colors and the preview, which are the follow-up tasks.

The light frame (`2220:21264`) is stale (the old single page with the app
sidebar), so there is no light reference to diff. Light renders the dark
frame's layout in the light tokens, as the previous Settings task did (the
selected icon in `icon/select` pink, the check in pink).

The harness's Settings fixture now renders the three pages as
`settings-general`, `settings-keybindings` and `settings-terminal` (named as
the new references), and `state-settings-cli-focused` on General.
`--visual-views` renders `settings.png` (General), `settings-keybindings.png`,
`settings-terminal.png`, and the font picker and stepper states on Terminal.

Images (outside the repo):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-settings-sections/`
(before, after, and figma-diff's side, heat and overlay images).

## Tests

`src/app_settings_tests.rs` (window, headless, injected clock):

- Settings takes the column with its own sidebar (back button, rows, no
  footer, the titlebar buttons), General selected, the keys on the window.
- Each page shows its controls and only those.
- ⌘2 / ⌘3 switch pages inside Settings (⌘9 does nothing), and ⌘1 opens the
  sidebar's first numbered row again once Settings is left.
- ↓ moves the selection without opening, stops at the last row, Enter opens
  it; ↑ + Enter goes back.
- A row click shows its page.
- Back and Esc each return to the view under Settings (Issues) with the app
  sidebar back, Issues' row selected, the sidebar's keys working.
- Settings reopens on the page last used.
- Opened over a review, leaving reopens the review panel.
- Toggle sidebar hides the settings sidebar and the expand button brings it
  back.
- A `j` typed into the CLI field doesn't move the settings selection.

`views::settings_nav::tests`: page numbering, the keyboard walk, reopening
on the last page.

## Not done

- No palette or menu entry opens a specific page ("Settings: Terminal").
  Settings isn't a registry command (⌘, is an app menu item), and adding
  per-page entries would mean a desktop-only `CommandKind` the TUI has no
  Settings for. ⌘, then ⌘3 gets there.
- The TUI has no Settings view, so there is nothing to keep at parity, and
  no `shelbi_app::view` model changed.

## Verified

`cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
cargo clippy --all-targets --features visual-check -- -D warnings &&
cargo test` (all pass), then `--visual-figma` (`SHELBI_VISUAL_ONLY=settings`,
both themes, 0 checks failed) and `figma-diff.sh` as above.
