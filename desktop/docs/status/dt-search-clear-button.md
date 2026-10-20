# dt-search-clear-button: a clear (×) button in the search boxes

## The change

The Issues board's search box (from `dt-issues-toolbar`) could only be
cleared with Esc. It now shows a clear button at its right end while the
query is non-empty:

```
[🔍 sidebar coll        (×)] [+ New issue]
```

- **Shown only while the query is non-empty**, hidden when the box is
  empty, focused or not. That is how a macOS search field shows its cancel
  button (focus alone doesn't bring it up), so focus plays no part.
- **A click clears the query**: the board shows every card again under the
  workflow and workspace filters, the input is emptied, and the keyboard
  stays in the field (the click refocuses it), so the next keystroke starts
  a new search. The click doesn't bubble to the box.
- **The text never runs under it**: the button is a flex sibling after the
  field, so the field (`flex_1`, `min_w_0`) shrinks by the button's width
  plus the box's 6pt gap.
- **Styling**: `style::search_clear`, the app's quiet icon button
  (`icon_button`, `IconButtonSize::Small`: 24pt, 8pt corners) with the
  Heroicons `x-mark` at the box's 20pt icon size: the `icon` role at rest
  (the magnifying glass's color), `text_loud` and the hover fill fading in
  under the pointer, the pressed fill on press, in both themes from the same
  roles. It hangs 2pt into the box's 6pt right padding, so the mark sits 6pt
  in from the right edge as the glass sits 6pt in from the left, and fits the
  28pt (Small) and 32pt (Base) boxes alike.
- **Tooltip** "Clear search". **Esc is unchanged** (clears a query, then
  hands the keys back; Enter keeps it on the board).
- **The Keybindings editor's search** (Settings ▸ Keybindings, the
  Searchbox at Base, the only other box on `style::search_box*`) gets the
  same button; its Esc is unchanged too. The terminal's find bar and the
  palette aren't built on the Searchbox, so they're untouched.

## Decisions

- `x-mark` over `x-circle`: it is the glyph every other dismiss button in
  the app draws (banners, the update notice, the terminal find bar), and it
  is already vendored at 20pt. Swapping to `x-circle` is a one-line change
  in `search_clear` (plus vendoring the drawing) if John prefers the macOS
  look.
- The button is a helper the caller adds (`search_clear`) rather than a new
  `search_box` parameter: each view owns its query and its clear handler,
  and `search_box`'s signature stays as it was.
- Both views factor the clear out of their Esc path (`clear_search`), so Esc
  and the button empty the query the same way.

## Figma: an undrawn state for John

The Components page (`2196:10061`) has one **Searchbox** component set
(`2220:16644`) with only `Hover` × `Size` variants (`Hover=false/true`,
`Size=Small/Default`): no clear-button variant and no filled-query state.
The button follows the app's icon buttons instead. **Undrawn state:** the
Searchbox with a query and its clear button (rest and hover), at both
sizes.

## Tests

- `views::issues::tests::the_clear_button_shows_with_a_query_and_clears_it`:
  the board in its own window with three cards. The button is hidden while
  empty and while merely focused; typing `side` narrows to one card and
  shows it; the field's bounds end before the button's, inside the box; a
  click empties the state and the input, brings all three cards back, hides
  the button and leaves the field focused; the next key starts a new query.
- `app::settings_tests::the_search_clear_button_shows_with_a_query_and_clears_it`:
  the same for Settings ▸ Keybindings through the real window (rows narrowed
  and restored, focus kept, Settings stays on the page).
- The existing Esc tests (`slash_focuses_the_search_and_esc_clears_it`,
  `the_search_and_the_context_picker_narrow_the_list`) still pass.

## Verification

- `cd desktop && cargo build && cargo clippy --all-targets -- -D warnings &&
  cargo clippy --all-targets --features visual-check -- -D warnings &&
  cargo test`: clean, 626 tests pass.
- No shared crate touched; the root workspace and its `Cargo.lock` are
  unchanged.
- `--visual-figma --2x` with `SHELBI_VISUAL_ONLY=issues` and `=settings`:
  0 checks failed, both themes. `state-issues-search-*` (the board with
  `terminal` typed) and `state-keybindings-search-*` (`column` typed) now
  show the button; checked by eye in both themes: the mark sits 6pt in from
  the right as the glass does from the left, the text ends before it, and it
  reads in the glass's gray. Copies under
  `~/.shelbi/projects/shelbi/design-refs/desktop-figma/qa/dt-search-clear-button/`
  (`crop-issues-{dark,light}.png` are the toolbar's right end at 2x).
- Not rendered: the hover state. It is `icon_button`'s, unchanged and
  already in use (the banners' dismiss).
