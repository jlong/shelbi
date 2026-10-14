# dt-terminal-complete: the terminal element, finished

## What landed

Everything in Phase 2 the spike left out. The terminal element
(`src/elements/terminal/`) now does:

- **Scrollback.** The wheel (Shelbi's unless the program reports the mouse),
  Shift+wheel (always Shelbi's), Shift+PageUp/PageDown/Home/End, and a
  scrollbar (`scrollbar.rs`): a thin thumb over the right edge, sized to the
  visible share of the buffer; drag it, or click the track to jump. New
  output while scrolled up doesn't move the view, and a "N lines below"
  button (bottom right) counts how far up you are and jumps back. Typing
  returns to the bottom (the TUI rule). On the alternate screen the wheel of
  a program that doesn't report the mouse sends arrow keys (alternate-scroll
  mode, xterm's rule), so the wheel scrolls `less` and `man`.
- **Selection and clipboard.** Drag selects cells, double-click a word,
  triple-click a line, Alt+drag a block; Shift keeps any of them Shelbi's
  while a program reports the mouse (Shift+Alt for a block). Holding a
  selection drag past the top or bottom edge scrolls the scrollback (a line
  per 50 ms) and extends the selection. A finished selection is copied (the TUI's rule); Cmd+C copies; Cmd+V pastes,
  bracketed when the program asked. The selection now lives in the emulator,
  so it stays on its text as output scrolls it into history, and goes away
  when that text is erased or overwritten. Selection works on both screens
  (copying out of `less` or `vim`).
- **Find.** Cmd+F opens a find bar (`search_bar.rs`, top right): search as you
  type (smart case: case-sensitive once the query has a capital), "3 of 12",
  Enter / Shift+Enter (and Cmd+G / Cmd+Shift+G) step to the older / newer
  match and scroll it into view, Escape closes and clears the highlights.
  It starts at the newest match on screen. Matches are re-scanned (debounced,
  200 ms) as output arrives while it is open. On the alternate screen it
  searches what is on screen. IME composition works in the query.
- **IME and dead keys.** The pane's `EntityInputHandler` is complete: marked
  text is drawn over the cells at the cursor, underlined, with the input
  method's selected clause underlined thicker; the candidate window is placed
  under that clause (UTF-16 offsets mapped through display width); the input
  method can read the composition back (`text_for_range`); commit sends the
  text; cancel (`unmark_text`) sends nothing. Dead keys compose through the
  same path (Option+E, E types `é`) now that Option types characters by
  default.
- **Option key** (`desktop.option_as_meta`: `false` default, `true`, `left`,
  `right`, as in Ghostty; live). Option with a named key (Enter, the arrows,
  Backspace, Delete, ...) is always Meta whatever the setting, since those
  have no dead-key meaning: Option+Enter is Claude Code's newline,
  Option+Left/Right/Backspace move and delete by word. Which Option key is
  down comes from `CGEventSourceFlagsState`'s device bits (gpui doesn't say).
- **Links.** With Cmd held, the URL or `path[:line[:col]]` under the pointer
  is underlined and the pointer becomes a hand; Cmd+click opens a URL in the
  browser and a file in the review panel's editor (`shelbi_state::resolve_editor`)
  at that line, in each editor's syntax (`+42` for vi-family, `file:42:5` for
  Helix/Zed/Sublime, `--goto` for VS Code and its forks, `--line` for
  JetBrains, `-l` for TextMate); terminal editors open in Terminal.app, as
  the issue editor already did (`views/external_edit.rs`, now shared). A
  relative path resolves against the session's working directory
  (`session.meta.cwd`); a `git diff` `a/`/`b/` prefix is tried without it; a
  path that doesn't exist opens nothing.
- **Fonts.** `desktop.terminal_font_family` / `terminal_font_size` apply live
  (the settings view sets a `TerminalSettings` global; panes re-measure and
  report the new grid). Geist Mono (regular, bold, italics) and Symbols Nerd
  Font Mono are bundled. A missing glyph falls back to Geist Mono, then the
  Nerd symbols, then Menlo, then the system cascade (CJK, emoji). A configured
  family that isn't installed falls back to Geist Mono.
- **Mouse forwarding.** Clicks, drags (only under button- or any-event
  tracking, `?1002`/`?1003`), and plain motion (only under `?1003`, once per
  cell) reach a program that asked, SGR or X10 encoded; middle and right
  drags too. The ownership policy is the spike's shared gesture (a plain drag
  selects, Alt hands a drag to a reporting program, Shift reclaims).
- **Accessibility.** The pane is a focusable AccessKit `Terminal` node named
  after the session, with the visible rows as text runs (line breaks,
  character lengths, word starts) and the cursor as its text selection
  (`a11y.rs`). macOS exposes it as a focused `AXTextArea`. Built only while
  an assistive technology is connected. Scope in `../../README.md`
  ("Accessibility").

### Shared code (`shelbi-term`, used by the TUI where it has the feature)

- `selection`: `SelectionMode` (cell, word, line, block), `Selection::row_span`
  (the paint geometry, also behind `contains`), block extraction, and
  `WORD_SEPARATORS` / `is_word_separator` (the double-click word rule).
- `view`: the selection lives in the emulator's `Term::selection` (rotation
  with scrolling and clearing on erase come from alacritty), plus a text
  check that drops it when the text under it changes in place;
  `begin_selection_mode`; selection and search on both screens (scrollback
  stays normal-screen only); `start_search` from the newest visible match,
  `refresh_search`, `search_position`; `link_at`; `screen_text`.
- `search`: `select_last_at_or_before`, `rerun` (keeps the current match),
  `current_index`.
- `links` (new): URL and path detection over a wrap-joined logical line,
  `Link::row_span`, `path_candidates`.
- `input`: `MouseModes { drag, motion }`, `MouseAction::Move`; drags and
  motion are encoded only in the modes that report them.
- `gesture`: `handle_clicks` (double/triple click), block on Alt, motion,
  alternate scroll.
- `emulator`: `alternate_scroll()`; Shelbi's word separators set as the
  emulator's semantic-escape characters.
- vendored alacritty: one read-only accessor, `Selection::anchor()`
  (`VENDORING.md` divergence 3).
- TUI: `n` / `N` in its scrollback search now step to the older / newer match
  (`search_older` / `search_newer`; the search starts at the newest), and
  crossterm `Moved` events map to `MouseAction::Move` so any-event programs
  get motion there too. Its selection gains the emulator-held rotation and
  works on the alternate screen. No TUI test changed except the search test,
  which now also checks stepping.
- `shelbi-state`: `DesktopConfig::option_as_meta: Option<OptionAsMeta>`
  (YAML `true`/`false`/`left`/`right`).

## Decisions

- **Option types characters by default** (the orchestrator's call, the macOS
  and Ghostty/iTerm default), with `desktop.option_as_meta` in Ghostty's
  shape. The key is `desktop.option_as_meta` as asked; the font keys keep the
  names `dt-shell-sidebar` already shipped (`terminal_font_family`,
  `terminal_font_size`), not the task text's `desktop.font` /
  `desktop.font_size`.
- **Selection in the emulator.** Keeping our own anchor/focus would drift as
  output scrolls (the history ring rotates without a counter we could read).
  alacritty's `Term::selection` already rotates and clears on erase; one
  read-only accessor lets a drag cross back over its start. A text check
  covers overwrites in place (skipped for selections over 2,000 lines).
- **Selection and search on the alternate screen.** The acceptance asks for
  both in `less`; the old shared rule disabled them there. Scrollback stays
  normal-screen only. This changes the TUI too (it can now select in `vim`).
- **Search starts at the newest visible match and Enter goes older**, since a
  terminal is read bottom-up; the TUI's `n` follows.
- **Alternate scroll only when the program isn't reporting the mouse**
  (xterm's rule), so Shift+wheel over full-screen Claude Code doesn't become
  arrow keys.
- **Copy on finished selection** stays (the TUI's rule), including a
  double/triple click.
- **Words** break at whitespace, quotes, brackets, `,`, `|`, `:`, `;`, and
  `│`, so `src/main.rs:42` double-clicks to `src/main.rs` (alacritty's
  default set plus `;`).
- **Link rules lean conservative**: a bare `README.md` isn't a link (no slash,
  no line number), nor `and/or`, `e.g.`, `v1.2.3`. URLs drop trailing
  punctuation and unbalanced closing brackets.
- **Bundled fonts are also registered with CoreText** (process scope), since
  gpui's added fonts can't be fallbacks (see `../gpui.md`). Symbols Nerd Font
  Mono adds 2.4 MB to the binary (Ghostty and WezTerm bundle it too).
- **The find bar is the pane's own overlay**, not a gpui-kit `Input`: the pane
  keeps focus, so typing, IME, and paste route through the one input handler,
  and nothing reaches the session while the bar is open.
- **Accessibility scope**: visible rows only, no live region (a streaming
  agent would be unlistenable), no attributes. Announcing output is a
  follow-up if users ask.

## Measurements

Frame times, offscreen (`--visual-check`, release, 128x35 grid, Apple M4),
before (`dt-spike-terminal`'s run) and after. The hub was loaded both times
(load average 50 to 70 from other workers), so compare loosely.

| Case | Before | After |
|---|---|---|
| Idle redraw | avg 131 µs, p95 160 µs, 0 rows rebuilt | avg 17 µs, p95 24 µs, 0 rows rebuilt |
| Flood frame (64 KiB chunks) | avg 49 µs, p95 70 µs | p50 45 µs, p95 58 µs (avg 1.4 ms: one 58 ms preemption outlier in 46 frames) |

No regression: the after run's idle redraw is lower than the spike's
(scheduler noise dominates at this scale), and the flood's p50/p95 match.
The per-frame work added is small: the highlight spans are computed per
visible row only when a selection, search, or hovered link exists; the
accessibility text is built only while an assistive technology is
connected; the scrollbar is one quad.

## Verification

From `desktop/`:

- `cargo build`: ok
- `cargo clippy --all-targets -- -D warnings`: ok
- `cargo clippy --all-targets --features visual-check -- -D warnings`: ok
Rebased (2026-10-14) onto `jlong/desktop-app` at `2ec92ffd` (`dt-chat-view`,
`dt-board-dnd`, `dt-activity-machines`, `dt-packaging`,
`dt-palette-overlays`, `dt-workspace-view`). `dt-chat-view` had added `session::wire` and `PaneEvent`;
`Connected` now carries `cwd: Option<PathBuf>` (set by `connect`, `None`
through `wire`). Everything below was re-run on the rebased tree.

- `cargo test`: 238 passed (new in this task: scrollbar geometry, find-bar state and keys,
  selection/search/link highlight geometry, accessibility runs and caret,
  terminal settings, Option-as-Meta by side, editor line syntax, a stub
  editor opened at `path:42:5`, UTF-16 range mapping)
- `cargo run --features visual-check -- --visual-check <dir>`: 79
  checks, 0 failed (debug and release). The Workspace view's
  `--visual-workspace` harness (from `dt-workspace-view`) also passes on the
  rebased tree. New checks: bundled Geist Mono; every CJK, emoji,
  symbol, and Nerd glyph found in a font (the check fails without the
  CoreText registration: LastResort draws three Nerd icons with one shared
  glyph); a live font-size change re-measures and reports the grid; Option
  typing characters vs. a live switch to Meta; Option+Backspace/arrows/Delete
  always Meta; IME composition with a selected clause, read-back, candidate
  placement, commit, cancel; a dead key; click/drag/motion forwarding under
  `?1000`/`?1002`/`?1003` and X10; double/triple-click and Alt-drag block
  copies; Cmd+C; a selection drag held above the grid scrolling back and
  extending; a selection surviving 80 lines of output and dropped on
  erase; output not moving a scrolled view; scrollbar thumb drag and track
  click; "lines below" button; typing returns to the bottom; the find bar
  (open, type, smart case, Enter/Shift+Enter/Cmd+G, no session input, Escape);
  Cmd-hover underline and Cmd+click on a URL and on `src/main.rs:12:3`; the
  alternate screen (wheel as arrows, selection).

From the repository root (`shelbi-term`, `shelbi-state`, `shelbi-tui`, and the
vendored alacritty changed):

- `cargo check --workspace --locked`: ok; the root `Cargo.lock` is unchanged
- `cargo build --workspace`: ok
- `cargo clippy --workspace --all-targets -- -D warnings`: ok
- `cargo +1.88 check --workspace --all-targets --locked` (the CLI's MSRV): ok
- `cargo test --workspace --no-fail-fast` (with `TASK_ID`/`PROJECT` and the
  hub variables unset), on the final rebased tree: 4030 passed, 1 failed:
  `shelbi --test session_survival` timed out waiting 10 s for a spawned
  session's socket at load average ~55; rerun alone it passes (it doesn't use
  the terminal code). Earlier runs on older bases: 3966 and 4013 passed, 0
  failed. One earlier run also caught a real one, the TUI's
  `alt_drag_forwards_to_the_program`, which assumed a drag is reported under
  click-only `?1000`; it now enables `?1002` as vim does.
- `shelbi-term`: 136 passed.

By hand, in the real app (`--spike` on a sandboxed session: `SHELBI_HOME` in
`/tmp`, the hub's socket variables unset, zsh then `less`; keyboard input
posted to the app's own process only, screenshots with `screencapture -l`):

- Typing, paste (`echo pasted-ok-$((6*7))` pasted and run), Shift+PageUp
  scrollback with the scrollbar and "45 lines below", late output not moving
  the view, Shift+End back down.
- Option+E, E typed `é` at the zsh prompt (a real dead key through macOS).
- Cmd+F: "history 9" found 11, two Enters to "9 of 11" with the current
  match highlighted, Escape closed it.
- `less notes.txt`: PageDown paged it; Cmd+F on the alternate screen found
  the 21 on-screen matches, newest first; `q` back to the shell.
- Accessibility, read through the macOS accessibility API (what VoiceOver
  uses), from a separate process: a focused `AXTextArea` labelled with the
  session name; `AXRangeForLine` / `AXStringForRange` return each visible row
  as its own line (`"history 112\n"`, ...); the insertion point is on the
  prompt's line; while `less` runs the text is `less`'s screen.
- vim and Claude Code mouse setup: vim 9's startup bytes (captured from a
  real session through `script`: `?1006;1000h` then `?1002h`) and Claude
  Code's full-screen renderer's (`?1000h ?1002h ?1003h ?1006h`, read from its
  bundled renderer) are replayed in `shelbi-term` gesture tests: clicks and
  the wheel are forwarded as SGR, vim gets Alt+drag motion but no plain
  motion, Claude Code gets plain motion, a plain drag stays a Shelbi
  selection, Shift+wheel over Claude Code sends nothing.

## What's left

- **Not driven by hand:** mouse input in the live window (macOS doesn't
  deliver mouse events posted to a background app's process, and moving the
  real pointer or activating the window while someone was using the machine
  wasn't acceptable), so live selection, scrollbar dragging, Cmd+click, and
  mouse forwarding in a running vim / Claude Code were exercised through
  gpui's real dispatch offscreen instead (the visual check) and with the
  programs' real mode sequences in `shelbi-term`. Japanese IME live: no
  Japanese input source is enabled on this Mac; composition was driven
  through the input handler the platform calls. VoiceOver speech itself
  wasn't turned on; the tree it reads was checked through the accessibility
  API. Opening a real browser tab for a URL wasn't done (gpui's `open_url`).
- **Not in scope / follow-ups:** box-drawing and block elements are still
  font glyphs (hairline gaps at 17 px rows); ZWJ emoji sequences render as
  separate glyphs; the bell is ignored; xterm's modifyOtherKeys (vim sends `ESC[>4;2m`) isn't
  implemented (the kitty protocol is); SGR-pixels mouse mode (1016) isn't
  supported (alacritty doesn't parse it, so programs don't get it).
- **The Chat and Workspace panes don't know their session's working
  directory** (they attach through `shelbi_app::session`, whose `Connected`
  doesn't carry it), so a relative path Cmd+clicked there resolves nowhere;
  absolute and `~/` paths open. Carrying `meta.cwd` through
  `shelbi_app::session` is a small follow-up. (`--spike` and any pane opened
  with `TerminalPane::connect` have it.)
- `dt-terminal-perf-multiclient`: the full performance pass.
