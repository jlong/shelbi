# dt-task-wysiwyg-editor

The task pane's title and body are edited where they're read. There is no
edit mode, Edit button, Save / Cancel row or Markdown textarea: a click puts
the caret in the formatted text, and the text doesn't move when it does.
John's requirement: "I want the user to be able to edit the task without
entering an edit mode. It should be WYSIWYG." Frames: Task pane, In progress
(2172:722) and Ready to start (2573:10737), as revised on 2026-10-28.

## What landed

### `shelbi_app::markdown_edit` (toolkit-free)

[`crates/shelbi-app/src/markdown_edit/`](../../../crates/shelbi-app/src/markdown_edit/).
The Markdown source is the model. The editor never keeps a separate
document and never re-serializes one.

- **`blocks::Doc`** lays the source out as rows: a paragraph (soft-wrapped
  lines joined with spaces), a heading (ATX or setext), one list item
  (bullet, ordered, task; nesting by indentation; lazy continuations), a
  quote, a code fence's line, a line of code (fenced or indented), a rule,
  and *raw* rows for what it doesn't render (tables, HTML blocks). Each row
  has its **display** (the source with the syntax hidden: `## `, `- [ ] `,
  `**`, `` ` ``, `[`…`](url)`, escapes' backslashes), its styles, and
  **chunks** that map display offsets to source bytes. A caret between two
  chunks snaps past a line's prefix (typing lands in the text) or before a
  closing delimiter (typing at the end of bold text stays bold). Blank lines
  are gaps; a blank line between blank lines is an empty paragraph the caret
  can sit in (that's what Enter opens, and what an empty body is).
- **`inline`**: a GitHub-flavored subset (escapes, code spans, `*`/`_`
  emphasis with CommonMark's flanking and rule-of-three, `~~`, links,
  autolinks, bare URLs). Images and inline HTML stay visible, muted.
- **`Editor`**: every operation is one source edit and one undo step.
  - Typing and paste insert as written (pasted Markdown comes in formatted;
    typing `# `, `- `, `**x**` formats as you go). A title is one plain line:
    nothing in it is syntax, and a pasted line break becomes a space.
  - Deleting removes the *visible* text, plus the syntax between rows it
    spans (they join) and the delimiters of any mark it empties (deleting
    the last letter of bold text drops its `**`; an escaped character takes
    its backslash). Delimiters of marks it only cuts into stay balanced.
  - Enter: the next list item (number + 1, `[ ]` for a task); out of an
    empty item (outdenting a nested one first); a new code line keeping the
    indent; a fence typed on its own line closes itself; elsewhere a new
    paragraph, a split keeping the formatting open at the split on both
    halves. New paragraphs get blank lines on both sides so they never run
    into a neighbour. Shift+Enter is a hard break (`\` + newline).
  - Backspace at a row's start drops its block formatting first (outdent,
    then the marker / `## ` / `> `), then joins it to the row above.
  - Bold, italic, strikethrough and code toggle over a selection (on where
    any of it isn't, off where all of it is; part of a span comes out of
    it). With nothing selected they apply to what's typed next, and at the
    end of formatted text they step out of it. Block commands rewrite only
    the row prefix (headings, lists numbered in order, tasks, quotes) or
    wrap/unwrap a code fence. Links: make, edit, remove. Checkboxes flip one
    byte. Tab / Shift+Tab nest and un-nest list items.
  - Undo / redo are kept as edits, not snapshots, so a change merged in from
    elsewhere is carried through them (`replace_from_outside` rebases the
    history) instead of being undone with the user's own typing.
  - Input-method compositions edit the text marked, and the finished
    composition is one undo step. UTF-16 offsets for the platform.
  - Copy gives the selection's Markdown (whole marks keep their delimiters).

### Desktop (`desktop/src/views/doc_editor/`)

- **`DocEditor`** (an entity, one per field) and **`DocElement`** (its
  element): lays the doc out with gpui's text shaper at the pane's width,
  in the frames' styles (Desktop/Title for the title; Heading for h1,
  Base/Semibold for the other headings; Base text; the design system's check
  for tasks; bullets and numbers in a 22pt gutter; code and raw source in
  the mono face on a rounded well; quote bar; rule), and paints selection,
  caret (blinking) and the IME's underline.
- **Keys**: the editor stops every key it handles, so `s`, `a`, `e`,
  `h`/`j`/`k`/`l`, the arrows and Enter never reach the board, and typed
  text arrives through the platform input handler (`EntityInputHandler`,
  so compositions, dead keys and press-and-hold work). Arrows (⌥ by word, ⌘
  by line / document), Home/End, Page Up/Down, ⌃A/⌃E, ⌘⌫, ⌥⌫, ⌘Z / ⇧⌘Z,
  ⌘A/C/X/V (and the Edit menu), ⌘S (save now), ⌘↩ (check the item),
  ⌘B / ⌘I / ⌘E / ⇧⌘X, ⌘K, ⌥⌘1–3 / ⌥⌘0, ⇧⌘7 / 8 / 9, ⌥⌘C. Esc hands the keys
  back to the pane (saving; nothing is reverted); Esc goes to the link
  prompt or an input method first.
- **Pointer**: click places the caret, drag selects, double-click a word,
  triple-click a row, Shift+click extends, ⌘+click opens a link, a click on
  a checkbox checks it (without taking the keys).
- **Contextual controls**: a small toolbar over a selection in the body
  (Bold, Italic, Strikethrough, Code, Link, Heading, Bulleted list,
  Checklist; pressed states from the caret's style) and the link prompt (⌘K
  or Link; Enter applies, empty removes).
- **Accessibility**: the title is a `TextInput` labeled "Task title", the
  body a `MultilineTextInput` labeled "Task description"; toolbar buttons
  are labeled buttons.
- **The pane** (`views/task_detail.rs`): the title and body are the two
  editors. Empty, the title shows "Untitled" and the body "Write a
  description…" (muted), and both take typing at once. The quiet save line
  under the title is unchanged (it says a blank title is held back).
- **The board** (`views/issues.rs`, `views/board_state.rs`): the view keeps
  the open task's two editors (`PaneEditors`), made when a task shows and
  dropped (saving) when it goes. Each change goes to the draft
  (`BoardState::edit_text`, which opens the draft if a checkbox was clicked
  without the editor having had the keys); taking the keys opens the draft
  and marks the pane editing (`editor_focused`); leaving saves now
  (`editor_blurred`); Enter in the title and Tab / Shift+Tab move between
  the fields. Text the store changes (someone else's edit merged in, a
  conflict settled, the external editor) and reads with no draft come in
  as one splice, keeping the caret, selection and undo. The Edit command
  (`e`, More > Edit description, the palette's `EditIssue`) puts the keys in
  the body with the caret at the end. More > Open in editor stays.
- Removed: `views/markdown.rs` (the read-only `TextView` body and its
  checklist block), the body `Textarea`, the Done button, and the board's
  editor-only Cmd+S / Esc handling. `ButtonContent::hotkey` is only drawn by
  the Figma sheet now.

## Decisions

- **The source is the model** (a "live" Markdown editor with the syntax
  always hidden) rather than a rich-text document converted to and from
  Markdown. That makes "never churn" and "never drop what you don't
  understand" structural: bytes an edit doesn't touch are untouched, there
  is no serializer to normalize `__x__` into `**x**` or re-wrap lines, and
  tables / HTML / footnotes stay as written (shown as source). The cost is
  that each command has to choose its source edit carefully; the tests pin
  them.
- **Syntax is hidden whether or not the field has the keys**, so focusing
  never moves text (no reveal-markers-under-the-caret mode). Code fence
  lines stay visible (muted mono) so a code block's language and edges
  stay editable.
- **Soft-wrapped lines read as one paragraph** (CommonMark), as the old read
  view showed them. A hard break shows as a new line.
- **No new parser dependency in `shelbi-app`**: the root `Cargo.lock` and
  Rust 1.88 floor are unchanged. The subset parser only decides how text
  looks; it can't lose content.
- **Tab** nests a list item; elsewhere it follows the pane's one order,
  merged with `dt-task-inline-properties` (#1991) in reading order: the
  title, Status, Workflow, Priority, the body, then the session (when the
  pane shows one) or back off the values, and Shift+Tab the reverse. So it
  never traps the keys. Enter in the title still goes straight to the body.
  `IssuesView::pane_tab_key` takes the pane's hops into the editors; the
  editors' `DocEvent::Tab` sets `pane_focus` on the way out.
- **Fonts in a code span** use the mono face at the row's size (a shaped
  line has one size); code blocks use 12pt mono.
- **Focus in tests**: gpui reports focus paths only for the active window,
  so the editor tests activate it. Two fields trading focus can fire the new
  field's focus-in before the old one's focus-out, so a blur while the other
  field has the keys only saves and keeps the pane editing.

## Left for later

- The TUI has no in-place editor (unchanged; the Edit row in `parity.md`
  says so).
- No drag-and-drop of text, no spell-check underline, no find in the body.
- Unsupported constructs are editable as their source but not rendered.

## Verification

- `shelbi-app` `markdown_edit` (56 tests): layout of the shipped Feature /
  Bug templates and the welcome card, round-trip (moving, clicking and
  selecting through a body full of GFM and unsupported constructs changes
  no byte and no revision; an edit touches only its bytes; undo gives the
  original back), unsupported blocks shown as source, typing into syntax,
  Markdown typed in, paste, every Enter / Backspace / Delete case, deletion
  across rows keeping marks balanced, word deletion, bold / italic / code
  toggles, pending formatting and stepping out, block commands, code
  blocks, checkboxes, Tab nesting, links, undo / redo, an outside change
  keeping the caret and the user's undo, a composition as one undo step,
  movement across rows, copy / cut, the empty body. A randomized test runs
  80 sessions of 250–600 random operations (typing, Enter, deletes, moves,
  selections, every command, undo / redo, cut, outside changes, IME) over
  real bodies: no panic, carets on character boundaries, and with no
  outside changes, undoing everything restores the body exactly (also run
  at 400 seeds).
- Desktop view tests (`views::issues`): typing into the body where it reads
  (the editor's bounds don't move on focus; `s a e h j k l` and the arrows
  are text, not board commands; the draft follows every change; ⌘Z;
  Esc saves and keeps the pane; `e` returns with the caret at the end;
  Close saves); a checkbox click checks it into the draft without taking
  the keys; the title edited in place, a blank title held back as Invalid
  while Enter moves to the body, Shift+Tab back through Priority /
  Workflow / Status to the title, a pasted line break as a
  space, ⌘S saving the title; the Edit command focusing the body, and
  focusing and leaving an unchanged task sending nothing.
  `pane_session_tests::output_while_editing_the_description_keeps_the_editor_focused`
  runs on the new editor, and
  `tab_past_the_properties_puts_the_keys_in_the_terminal` walks the merged
  order (title, the three values, body, Shift+Tab to Priority and back,
  then the terminal).
- Desktop: `cargo build`, `cargo clippy --all-targets -- -D warnings` (and
  `--features visual-check`), `cargo test` (all passing).
- Root: `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo check --workspace --locked` (root `Cargo.lock` unchanged),
  `cargo +1.88 check -p shelbi-app --all-targets --locked`,
  `cargo test -p shelbi-app`.
- Looked at `--visual-views` renders in both themes: `board-detail.png`
  (the body as the frames set it), `board-detail-editing.png` (caret after
  the Edit command), `board-detail-format.png` (the toolbar over a
  selection) and `board-detail-unsaved.png` (a failed save's draft and
  Retry). The fixture's `t1` now has a body with the frames' sections.
- Not run: a live session against a real daemon (typing, closing,
  reopening and reading it back from the CLI / TUI). The save path is the
  shared draft's, unchanged here and covered end to end by
  `task-inline-edit-state`'s `control_socket` test; this task only feeds it
  text.
