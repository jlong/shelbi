# tui-settings-keybindings: the bindings editor in the TUI, over a shared model

## The change

Settings ▸ Keybindings was desktop-only (`dt-settings-keybindings-editor`).
The TUI now offers it too (page 2, after General), editing the same
`~/.shelbi/keys.yaml` with the same rules, and a saved change takes effect in
the running TUI at once.

## Shared (`shelbi_app::settings::keybindings`)

`desktop/src/views/keybindings_state.rs` moved here, toolkit-free, and the
desktop page now runs on it:

- **`KeybindingsState`**: the rows by context (`keymap::CONTEXTS`) with their
  merged chords for the shown project, customized and unbound state, the
  search and the context filter (`cycle_context`, `context_label`), the
  capture (`start_capture`, `key`, `cancel`), the held conflict, `remove`,
  `reset`, `reset_all`, and `set_project`.
- **How a chord is spelled is the client's** (`Spell`, a plain `fn`): the
  desktop passes its keycap text (`⌃P`), the TUI its display style. The
  search finds a chord by that spelling and by the Mac, spelled-out and
  `keys.yaml` forms plus a few key names (`Return`, `Escape`, `Page Up`);
  this replaced the desktop's `keys::chord_search_text`. The conflict line
  (`conflict_message`) uses the same spelling.
- **`Change`**: `apply` (unchanged: into the layer that already sets the
  action, else `defaults`; Reset all clears `defaults` and the shown
  project's block, keeping other projects and unknown entries) and `save`
  (`keymap::edit::update_keys_file`: the change on the file as it is on disk
  now, comments kept where the shared editor can, a file that doesn't parse
  never rewritten).
- **Only a saved file shows.** `key` and the resets return
  `KeyOutcome::Save(change)` without changing what is on show; the client
  saves it and hands the result to `saved`, which shows the written file on
  success and keeps the rows on failure. So the rows always equal the
  bindings the client runs with. Before, the desktop showed the change
  before the write landed and reread the disk after a failed one.
- **One save at a time.** While a save is out, captures, removes and resets
  don't start (`saving`), so each change is computed from the file the last
  one wrote.
- **A file that doesn't parse is refused** by `load` (with why), and the
  bindings on show stay. The desktop used to show the built-in chords for
  such a file.
- `normalize`: a captured chord in the form `keys.yaml` parses to (a
  terminal's `K` or `⇧K` is `shift-k`), so it compares equal to a bound one.
- `read_keys` (after the loader's one-shot migrations), `keys_yaml_path`,
  `ensure_keys_file` and `KEYS_STUB` moved from the desktop page.

## The TUI (`crates/shelbi-tui/src/shell/keybindings.rs`)

**Layout**, in the main area beside the settings sidebar: the title and a
one-line helper, any error, `Saving…` while a write is out, then the
controls (Search, Context `‹ All contexts ›`, Reset all, Open keys.yaml),
then one group per context with its title and, when there is room, its hint.
A row is its label and its chords right-aligned with `or` between them; a
customized row has a yellow `●` in its gutter, an unbound one reads `None`.
At 80x24 the page is 52 columns; a row whose chords don't fit beside its
label puts them on the next line, and the page scrolls to keep the focused
row and the lines under it in view (`↓ more` says when more is below).

**Keys** (Tab or Ctrl+L from the settings sidebar; every row action is a
key, nothing is hover-only):

- ↑ / ↓ (or the sidebar's nav keys), Page Up / Page Down, Home / End move
  between the controls and the rows.
- On a row, ← / → pick one of its chords or the `+` after them. Enter changes
  the picked chord (or adds one on `+`), `a` adds one, Backspace / Delete
  removes the picked chord, `u` resets a customized row. The picked chord is
  filled; the row says "Customized · u resets it" under it.
- `/` (or typing on the Search control) searches: typing filters live, Ctrl+U
  clears, Esc clears and then gives the keyboard back, Enter / Tab / ↓ go to
  the first match. A paste lands in the search.
- Enter on Context (or `c` on any row or control but Search, ← / → on it) steps the
  filter through every context and back to all of them.
- Enter on Reset all (inert and muted when nothing is customized) restores
  every built-in chord; Enter on Open keys.yaml hands the terminal to the
  user's editor (creating the comments-only stub first when there is no
  file).

**Mouse.** A click on a chord captures it, on `+` or `None` adds one, on a
control runs it; a click anywhere else ends a capture. The wheel scrolls.

**Capture.** It takes every key ahead of the shell (the palette chord, the
focus moves, Ctrl+C, and a session under Settings never sees one). The
capturing chord reads `Press keys…`, with "Press the new shortcut. Esc
cancels, ⌫ removes this one." under the row. As on the desktop: Esc cancels,
Backspace / Delete removes the picked chord, a chord the action already has
changes nothing, and a chord another action in the same context has is
named under the row ("j is already Move selection down. Press j again to
move it here, or Esc to keep it there.") until pressed again. Nothing is
written for a cancelled capture.

**Saving and applying.** A finished change is written off the UI thread
(`SettingsAction::SaveKeys`, `SettingsMsg::KeysSaved`); once it lands the
shell takes the written file's bindings (`ShellState::apply_keymaps`) for
everything at once: its own key routing and the palette chord, the palette's
mode keys, the embedded board, activity feed and task popover (their keys
and footers), the sidebar footer's hints, and Settings' own hints. A failed
save shows `Couldn't save keys.yaml: …` and changes nothing, shown or
running. The page reads `keys.yaml` again whenever it shows and after the
user's editor returns; a read that doesn't parse is refused with
"Your bindings stay as they were until it's fixed." A read asked for while a
save is out runs once the save lands.

**Projects.** The page edits the shown project's override when the project
already sets the action, else `defaults`, like the desktop. Switching
project now runs the new project's bindings (its `projects.<name>` block),
from the file already in effect; before, the TUI kept the launch project's
bindings after a switch.

**Launch.** The page holds the bindings in effect for the whole shell: the
TUI seeds it from `keys.yaml` at launch, so what it lists is what runs.

## Terminal input

- **What a capture records is what the terminal reported.** Without the
  kitty keyboard protocol several chords arrive as other keys: Ctrl+H as
  Backspace (so it removes, as Backspace does), Ctrl+I as Tab, Ctrl+M as
  Enter, Ctrl+[ as Esc, Ctrl+Shift+letter without the Shift. The page
  records the key it got and never guesses the one you meant.
- **Chords this terminal can't send are marked.** A listed chord the
  terminal can't deliver (a ⌘ chord bound in the desktop app, Ctrl+H on a
  legacy terminal, Shift+Enter) shows a `⚠`, and the picked one says why
  under its row. Ctrl+Space is always the TUI's palette key, so a chord
  bound to it says so. The check follows the kitty probe
  (`ShellState::set_caps`).
- **Portable names.** Chords show in the TUI's display style
  (`format_chord`: `⌃P` on macOS, `Ctrl+P` elsewhere), as every other TUI
  hint does.
- **The way out stays.** Settings' own keys are fixed whatever is rebound:
  the arrows, Enter, Tab and Esc always move, open and leave, and Esc always
  ends a capture (a capture can never bind it). Unbinding the sidebar's nav
  and activate keys leaves Settings fully usable (tested).

## Platform differences (intentional)

| | Desktop | TUI |
|---|---|---|
| Add / reset on a row | Hover buttons past the row (q-001 of the desktop task) | Row keys (`a`, `u`) and a visible `+` on the focused row |
| Chord spelling | Keycaps (`⌃`, `P`) | Display style text |
| Context filter | A picker menu | A control that steps through the contexts (`c`, ← / →) |
| Esc in an empty search | Hands the keys back to the settings sidebar | Gives the keyboard back to the page; the next Esc leaves Settings |
| Bindings macOS owns (⌘W, ⌘Space) | Reported under the header | Not applicable |
| Chords the terminal can't send | Not applicable | Marked `⚠`, with why |
| Other windows | Reread `keys.yaml` when they come to the front | One window: the page rereads when it shows and after the editor |

## Decisions

- **No optimistic display, on either client.** The task asks that only
  successfully saved bindings apply, to the settings editor too; the shared
  state now shows a change once written. On the desktop this is a few
  milliseconds later than before.
- **A busy save holds new changes** rather than queueing them: a queued
  change would be computed from rows that don't yet include the one ahead of
  it. Saves are fast, so the hold is rarely visible (`Saving…` says so).
- **Unbinding is allowed**, as on the desktop; Settings' fixed keys are the
  escape route, so no binding is refused for safety.
- **Seeded at launch, not on first visit,** so a file broken after launch
  can't make the page show something other than what runs.
- **No new registry command.** Open keys.yaml is a page action, so command
  dispatch, the scenarios and the smoke tests are unchanged; there is no
  public view-model change for `parity.md`'s field table.
- **`r` is not a row key** (General's reload): reset is `u`.

## Tests

- `shelbi-app` `settings::keybindings` (the moved desktop tests, adapted,
  plus new): listing and customized state, search by label and every
  spelling, the context filter cycling, display order, a change shown only
  once saved and nothing starting while one is out, a failed save keeping
  the rows, a terminal's shifted letter matching the bound chord, Esc /
  Backspace / Delete, remove without a capture, conflicts held, cancelled,
  kept across a reload, and confirmed, a different chord after a conflict
  checked afresh, reset and Reset all, Reset all keeping other projects and
  unknown entries, the project layer, another project's bindings from the
  same file, rebinding to the defaults leaving no override, a broken file
  refused and never edited, a change on the file as it is on disk.
- `shelbi-tui` `shell::tests::keybindings`, every key through
  `ShellState::handle_event`, `keys.yaml` in memory (`MemKeys`): a rebind
  applies to the palette chord, sidebar footer, board and feed and Ctrl+P
  stops opening the palette; a capture takes Ctrl+P, Ctrl+C (held as Quit's
  conflict, nothing quits) and Ctrl+H (focus doesn't move); add, pick,
  remove, replace, Delete in a capture, row reset and Reset all with
  comments kept; a conflict named, cancelled with nothing written, then
  moved; the project layer, other projects kept, and a project switch
  running the other project's bindings; a malformed file refused on save and
  on reread, then taken once fixed; a failed save changing nothing shown or
  running; a save on top of an edit made elsewhere; Open keys.yaml queuing
  the editor and the reread after it; Settings' hints following a rebound
  nav key; Settings usable with its nav keys unbound; one save at a time; a
  read asked for mid-save running after; a paste into the search; capture,
  conflict, search, empty-result and filter states at 80x24; chords this
  terminal can't send marked.
- `shelbi-tui` `shell::keybindings`: which chords a legacy terminal can't
  send, and why.
- The desktop's own window tests (`app::settings_tests`: a rebind taking
  effect in the window, a capture taking ⌘1 and Esc only cancelling, a
  conflict until confirmed, the search and picker) run unchanged against the
  shared state.
- The existing Settings tests now walk past the Keybindings row, and their
  fixture pins the sidebar width (a concurrent test's saved width under a
  shared `SHELBI_HOME` used to shift their layout).

## Verification

See the PR body for the commands and results.
