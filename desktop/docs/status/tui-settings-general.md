# tui-settings-general: the TUI's General and Terminal pages

## The change

The TUI's Settings (`tui-settings-shell`) had one read-only page. It now
edits the global preferences that apply to it, with the desktop app's
immediate-save pattern, and gains a Terminal page that says what the host
terminal owns:

- **General**: the appearance (Auto, Dark, Light) and the external editor
  command, then the summary of where the settings live it had before.
- **Terminal**: what the host terminal owns, its sixteen ANSI colors as
  sessions draw them, and a short preview session. Nothing on it edits.

## Shared

- **`shelbi_state::update_user_config`** now patches only what an edit
  changed. It reads `config.yaml` fresh, applies the edit to the typed config,
  and writes the difference as a text patch through `yaml_edit` (`merge`
  then `patch`, the patcher the workflow, agent and pool editors use). So
  comments (the `shelbi init` scaffold's included), key order, a value's own
  spelling (`window_opacity: 0.85`) and keys this build doesn't model survive,
  at the top level and inside `desktop:`. Before, the whole `desktop:` block
  was replaced from the typed struct (dropping a newer client's keys there)
  and every comment was lost. An edit that changes nothing writes nothing. A
  file that isn't YAML, isn't a mapping, or holds a value this build can't
  read is an error and stays as it is, as does the file when the write fails.
  The desktop's saves go through the same function and get all of this.
- **`shelbi_state::resolve_editor_source`**: the editor precedence (the
  setting, `$EDITOR`, `vim`) over given values, with the layer that won
  (`EditorSource`). `resolve_editor` is unchanged and still the launcher.
- **`shelbi_app::settings::general`** (toolkit-free):
  - `THEMES`, `theme_label` and `SaveStatus` (Idle, Saving, Saved,
    Failed), moved from the desktop's Settings, which now uses them (its
    status line gains "Saving…" while a write is in flight).
  - `Preferences` (what `config.yaml` says: theme, editor) and `Change`
    (one narrow edit: `apply` touches only its key, `write` runs it through
    `update_user_config`).
  - `GeneralForm`: the page's state. Picking a theme or committing the
    editor returns a `Change` and shows it pending; `saved_result` takes the
    write's outcome, keeping what was written or going back to what the file
    holds and saying why. One save at a time; a read that lands while a save
    is in flight is ignored (the save's result is newer). A file that
    doesn't read takes no edits. `LineInput` is the one-line field.
- **`shelbi_app::settings::terminal_preview`**: the desktop's preview
  session (the Terminal frame's Claude Code transcript as a canned ANSI
  stream) moved here, plus `short`, a 17-row cut that keeps every color the
  full one shows. The desktop's `views/terminal_preview.rs` is a wrapper over
  `stream`; its emulator tests stay there.
- **`summary::GlobalSummary`** carries `preferences` and `env_editor` in
  place of the resolved theme and editor.

No command, keymap action, view-model field or shipped default changed, so
there is no new scenario, smoke entry, `parity.md` field row or config-upgrade
sniffer.

## The TUI

**Theme is live.** `theme::install` was first-call-wins over a `OnceLock`; it
is now an atomic the next frame reads, so every surface (Settings, the
sidebar, the board, Activity, overlays, the session chrome around a pane)
paints in the new theme on the frame after the write lands. The one place
that kept a resolved color across frames, the workspace panel's header status
(`WsStatus.color`), now keeps a `StatusTone` and resolves it as it draws.
Sessions are untouched: no reconnect, no relaunch, nothing sent to them (a
real-PTY test checks the pane is the same object and the agent received
nothing). Auto stays dark, as documented: a terminal doesn't report its
appearance, and the page says so in one line.

**General, top to bottom.** The status line (Saving…, Saved to
`~/.shelbi/config.yaml`, or the error in red). Appearance: the Theme row, a
segmented control (←/→ on the row, Enter or Space for the next, or a click on
a segment). Editor: the Command row (the override, or "Not set"), what Shelbi
runs and where it comes from ("code --wait (your setting)", "nvim
($EDITOR)", "vim (the default)"), with the override set what it would fall
back to, and "Clear the editor setting". Then This computer (folder,
`config.yaml`, `keys.yaml`, and Edit config.yaml in your editor), the
project, and its Open in editor rows.

**The editor commits on purpose.** Enter on Command starts typing; while
typing every key is the field's (letters, digits, `r`, `q`, `j`/`k`, Tab), a
paste or a dropped path types into it, and the hint band says "⏎ save · Esc
put back". Enter saves (trimmed; blank clears the override), Esc puts the
saved one back, and nothing is written per keystroke. Arguments are kept as
typed and split by the existing resolver at launch. Leaving Settings, another
page, or another project with a typed but unsaved command goes through the
shared unsaved-edits prompt, whose Save writes it.

**Reading.** Settings reads `config.yaml` again every time it opens and
when the editor it launched returns, and the TUI follows the theme the file
now holds (an edit made in a text editor or the desktop app). A file that
doesn't read shows why, offers no controls, and keeps Edit config.yaml.

**Terminal.** Explains that the host terminal owns the font and its size,
the ANSI colors, window opacity and blur, and keyboard options such as
Option as Meta; that Shelbi never changes the terminal's colors; that the
theme colors only Shelbi's own lists and panels; and that the desktop app's
terminal settings (`desktop:`) don't apply here. The swatches (regular over
bright) are painted through the sessions' own color mapping
(`terminal_view::ansi_swatches`, the same `named_to_rat` a session's cells
go through), so they show the host's palette exactly as a session's colors
draw. The preview runs the
shared short session through a real emulator and draws it with the sessions'
own cell mapping (`TerminalPane::render`), one line per row. It wraps
swatches onto more rows below 8 columns and narrows the preview with the
page.

**Writes in tests** go to an in-memory `FixtureConfig` shared by the test
shell's reader and writer (failures injectable); nothing touches
`~/.shelbi`.

## Platform differences (by design)

- **Theme**: the desktop follows the system on Auto and applies a pick before
  the write; the TUI is dark on Auto and applies a pick when the write lands
  (so a failed save never looks applied).
- **General**: the TUI edits the editor command and keeps its summary; the
  desktop edits its CLI path, notifications and window effects, which are its
  own, and has no editor field.
- **Terminal**: the desktop edits and previews its own terminals; the TUI
  explains and shows the host's. It has no font, window-effect, CLI or
  notification control and never writes `desktop:` or sends palette escape
  sequences.

## Not done

- The desktop still saves its whole General/Terminal form per change (now
  narrowed to the keys that differ from the file, but from a form that may be
  stale); moving it to per-control `Change`s is a follow-up.
- A palette navigation over a typed, unsaved editor command still leaves
  without asking (as before for any page).
- Not hand-walked in a live TUI against a sandboxed root; the tests drive the
  real event path (`handle_event`, `draw_frame` on `TestBackend`) and a real
  PTY.

## Tests

- `shelbi-state`: `update_user_config` patches only what changed (scaffold
  comments, unknown top-level and `desktop:` keys, untouched spellings,
  arguments, clearing, returning to the original bytes, no-op writes nothing),
  leaves an unreadable file alone (bad YAML, a scalar, a list, a bad value),
  reports a failed write and keeps the file; `resolve_editor_source`.
- `shelbi-app`: `settings::general` (pending, saved and failed theme; the
  editor's commit, fallback and clear; unreadable files; a late read during
  a save; narrow changes; the line input; labels), `terminal_preview`.
- `shelbi-tui`: `theme` (install switches, Auto is dark); `shell::settings`
  (keys and clicks on the theme, pending and failed copy, the field taking
  every key, paste, Esc, the prompt's Save writing the command, `$EDITOR`
  fallback, an unreadable file, light readability while typing, the Terminal
  page's copy, no controls, the sixteen host colors, the preview's colors
  through the session mapping, widths down to 1); `shell::tests` (a live
  theme change through the event path repainting Settings, the session
  chrome, the palette and the board in light, back to dark on Auto; a failed
  write keeping the file and the look; an outside edit read on entering
  Settings and after the editor, and a file that stops reading; the editor
  with arguments, no write per keystroke, clear to the fallback);
  `pty_input_tests` (a theme change leaves the live session's pane in place,
  sends it nothing, and typing reaches it after Back).
- Desktop: its existing Settings, preview and smoke tests over the shared
  `SaveStatus`, labels and stream.
