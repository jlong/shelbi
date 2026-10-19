# dt-settings-keybindings-editor: the Keybindings page becomes a bindings editor

## The change

Settings ▸ Keybindings was a header (title, Open keys.yaml, a helper) from
`dt-settings-sections`. It is now the editor John drew in the Keybindings
frame (`2252:28172`):

- The header: **Keybindings** with **Open keys.yaml** (unchanged behavior),
  then the frame's helper, "Click a shortcut to change it. Bindings are
  shared with the TUI." Any binding macOS already owns (⌘W, ⌘Space, ...) is
  still reported under it, as is a failed read, write or open.
- **Search** (the Searchbox at Base, 32pt, 8pt corners) filters rows live by
  action label or by chord in any spelling (`⌃P`, `ctrl+p`, `ctrl-p`,
  `Return`). **All contexts** (the Button at Base with the chevron) opens a
  picker that narrows the list to one context. **Reset all** restores every
  built-in chord and is disabled (40% opacity, inert) when nothing differs
  from the defaults.
- One group per keymap context, every context and action the shared keymap
  defines: Global, Sidebar, Issues board, Issue details, Activity, Command
  palette, with the frame's titles, right-aligned hints, labels and order.
  Each row shows its chords as keycaps (the Button at Small with only its
  Hotkey) with `or` between them. The list scrolls under the header and
  filters.
- **Click a keycap to rebind it.** It reads "Press keys" with the focus ring.
  The next chord replaces it, Esc cancels, Backspace or Delete removes that
  chord. A chord another action in the same context already has is flagged in
  a line under the row ("R is already Refresh. Press R again to move it here,
  or Esc to keep it there."): pressing it again moves it (off the other
  action), Esc keeps both. Nothing is saved until a capture finishes.
- **Hover a row** for a + past its right edge (add another chord, the same
  capture) and, on a row that differs from its built-in chords, a reset
  (arrow-uturn-left). Both sit outside the rows, so the resting layout is the
  frame's (John's call, q-001 below).

Saving writes `~/.shelbi/keys.yaml` in its existing layout, keeping the
user's comments and every entry it doesn't change, and the window takes the
new keymaps at once: its key handling, the palette chord and the menu bar's
key equivalents. Other windows reread `keys.yaml` when they come to the
front, as they already did. The TUI reads the same file on its next start.

## Decisions

- **q-001 (asked, answered):** the frame and the components page draw no
  "add another shortcut" control and no per-row reset. Approved as proposed:
  hover-only + and reset, placed so the resting layout matches the frame.
- **Quit rows are listed.** The frame's Global and Sidebar groups leave out
  `global.quit` (⌃C) and `sidebar.quit` (Q, ⌃C), but the task asks for every
  action, and the TUI uses both. They come last in their groups ("Quit
  Shelbi", "Quit"). This adds 80pt to the list above Issues board, which is
  most of the figma-diff residual below.
- **Display order of a row's chords.** The frame lists named keys before
  letters (`↑ or K`, `Home or G`, `⇧↑ or ⇧K`), the reverse of the built-in
  lists (`[k, up]`). The row shows them in the frame's order, but `keys.yaml`
  keeps its own (the TUI's hints show the first chord), and a capture edits
  the chord it was opened on.
- **Where an edit goes.** Into the layer that already sets the action (the
  project's `projects.<name>` block when it has one, else `defaults`), else
  `defaults`. Chords equal to the built-in ones clear the override instead,
  so the file only holds what differs and "customized" means "differs from
  the defaults". Reset all clears `defaults` and the current project's block
  for every known action; other projects' blocks and entries Shelbi doesn't
  know are kept.
- **The edit lands on the file as it is on disk** at save time, not on the
  copy the page read, so a hand edit made in between survives.
- **Comment-preserving edits** (`shelbi_state::keymap::edit`): each edit is
  made on the YAML tree (its meaning) and on the text, line by line. The text
  edit is kept only when it parses back to exactly the tree's result;
  anything else (flow mappings, anchors) falls back to re-serializing the
  tree, which is correct but drops comments. A file that doesn't parse is
  never rewritten (the page says so).
- **Shared labels.** `Action::label()` (short, no mode prefix) and
  `keymap::CONTEXTS` (title, hint, reading order) live in `shelbi-state`
  beside the existing `description()`, so the TUI can show the same words.
  The TUI's own help is unchanged.
- **Capture takes every key.** A gpui keystroke interceptor (ahead of
  bindings and the menu bar) feeds the capture, so ⌘ chords, ⌃P, ⌘1 and Esc
  are bindable or cancel instead of acting.
- **Keycap glyphs.** `⌃` and `⌥` are set in Apple Symbols, `⇧` in Geist's
  own fallback, `⌘` in the system font. SF's `⌃` made a ⌃P keycap 45pt where
  the frame's is 41pt. This is a keycap-only rule (`style::keycap_label`):
  the sidebar and palette hotkeys QA'd earlier are unchanged.
- **Escape in the search** clears it, and empty, hands the keys back to the
  settings sidebar (so a second Esc leaves Settings as before).

## Undrawn states (for John to draw)

None of these is in the frame or on the components page. The harness renders
each, dark and light, as `state-keybindings-*`:

- `capture`: a keycap waiting for a chord ("Press keys", focus ring).
- `add`: the + capture, a new keycap after the row's others.
- `conflict`: the conflict line under the row, in the error color.
- `search`: the list narrowed by a query (`column`).
- `picker`: the context picker open (the Activity pickers' menu).
- `customized`: rows that differ from the defaults, Reset all enabled, an
  unbound action ("None" keycap, still clickable to bind).
- Not renderable offscreen: the row hover's + and reset buttons, and the
  empty search result ("No actions or shortcuts match.").

## figma-diff

`scripts/figma-diff.sh` at the default FUZZ 10, full 1440x1024 frame, dark,
against `settings-keybindings-dark.png`. "Before" is `dt-settings-sections`'
header-only page.

| screen | before | after |
|---|---:|---:|
| settings-keybindings-dark | 5.32% | **3.20%** |
| settings-general-dark (unchanged) | 1.08% | 1.08% |
| settings-terminal-dark (unchanged, its ANSI editor is another task) | 49.13% | 49.13% |

Split by region (ImageMagick `compare -metric AE -fuzz 10%`, share of the
region's pixels): the main column above the first Quit row (y 0 to 331)
0.21%, the main column below it 0.56% (the rows shifted down 40 and 80pt
by the two Quit rows), the settings sidebar 0.27%. What's left above the
shift is Geist 1.2 vs 1.7.2 advance widths (the search and the picker button
end 4pt apart from the frame's) and the modifier glyphs' exact shapes.

No light Keybindings frame exists; light renders the dark layout in light
tokens (`settings-keybindings-light.png`, checked by eye).

Images (outside the repo):
`~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-settings-keybindings-editor/`
(the renders, every `state-keybindings-*` state in both themes, and
figma-diff's side, heat and overlay images).

## Tests

- `shelbi-state` `keymap::edit` (keys.yaml round trip): a new file gets the
  documented layout; comments, blank lines and unrelated entries survive an
  edit and a trailing comment on the edited line is kept; a project override
  is edited in its project; the built-in chords clear the override and its
  emptied blocks; clear-all keeps other projects and unknown entries; empty
  lists unbind and awkward chords (`?`, `-`, `1`, `y`, `'`, `#`) are quoted
  and parse back; block lists are replaced whole; flow style falls back; a
  broken or misshapen file is refused; a comment-only stub keeps its
  comments; every action round-trips through the real loader; the file on
  disk is read, edited and written (isolated `SHELBI_HOME`).
- `shelbi-state` `keymap::contexts`: every mode has one context in mode
  order, every action is listed once in its own context, labels are unique
  per context.
- `desktop` `views::keybindings_state` (listing, filtering, capture,
  conflicts): every context and action listed with its chords, a project
  override shown as customized; search by label and by chord spelling, the
  context picker narrowing; capture replaces, adds, Esc cancels, Backspace /
  Delete remove; a conflict is held until confirmed, kept across a reload,
  Esc drops it, a chord in another context is no conflict; an already-bound
  chord changes nothing; reset and Reset all; rebinding to the defaults
  leaves no override; a broken file is left alone; a change applies to the
  file as it is on disk; rows show named keys before letters.
- `desktop` `keys`: keycap spellings as the frame writes them, and chord
  search text.
- `desktop` `app::settings_tests` (headless window): the page lists every
  context with the frame's chord order and no hover buttons in the resting
  layout; clicking ⌃P and pressing ⌘K rebinds the window's palette chord at
  once and Reset all restores it; a capture takes ⌘1 (the page chord) and Esc
  only cancels; a conflict shows on its row until confirmed and the window's
  sidebar keymap follows; typing in the search narrows the list, Esc clears
  then hands keys back, the picker narrows to one context. Under test the
  page never writes the real `~/.shelbi`.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo clippy --all-targets --features visual-check -- -D warnings &&
  cargo test`: clean, 548 tests pass.
- Root (touched `crates/shelbi-state`): `cargo build --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings` clean;
  `cargo check --workspace --locked` passes and the root `Cargo.lock` is
  unchanged; `cargo test --workspace` passes (every crate, 0 failures).
- `--visual-figma` with `SHELBI_VISUAL_ONLY=settings`: 0 checks failed, both
  themes.
