# docs-desktop-tree: the Documentation's nav row and its tree

Part of the Documentation (`Plans/docs.md` in the Shelbi ContextStore space).

```text
┌────────────────────────┬──────────────────────────────────────────────┐
│ ‹  Documentation          ＋ 🗀 ◧ │  Plans › OpenCode integration                │
│ 🔍 Filter               │ ──────────────────────────────────────────── │
│ ▾ 🗀 Plans              │  # OpenCode integration                      │
│ │   📄 Awareness v2     │                                              │
│ │   📄 OpenCode integ…  │  ## Context                                  │
│ │ ▸ 🗀 Archive          │                                              │
│ ▾ 🔬 Research           │                                              │
│   📄 Read me            │                                              │
│ ● Synced · 2m ago      │                                              │
└────────────────────────┴──────────────────────────────────────────────┘
```

## What it does

- **Nav.** `View::Docs` (view id `docs`) is in the shared nav after Issues:
  Orchestrator, Issues, Documentation, Changelog. Its icon is Heroicons `book-open`.
  Showing it pushes the Documentation's tree into the leading column (`Occupant::Docs`
  in `column_swap`, the same slide or Reduce Motion crossfade as Settings and
  the task panels); ‹ or Esc in the tree goes back to the view it was opened
  over, and the sidebar takes the keys again. Like the settings sidebar, the
  tree hides with the app sidebar.
- **Tree** (`views/docs.rs`, over `shelbi_app::docs_tree` and
  `shelbi_app::docs_tree_edit`): rows show a document's title (its first H1),
  the file name in the tooltip; folders first, chevrons, 1pt indent guides
  under each parent's chevron, the folder's emoji from its
  `.contextstore/settings.yml` (`DocsNode.icon`) or the folder drawing (open
  or closed), the document drawing; the open document's row is marked.
- **Keys** (the tree has focus; a field takes its own): ↑ ↓, Home / End
  (⌘↑ / ⌘↓), ← closes a folder or steps out, → opens or steps in, Enter
  opens (a folder opens or closes; Enter on the document already open
  renames it), Space opens, F2 renames, Delete or ⌘⌫ trashes, ⌘N new
  document, ⇧⌘N new folder (taken ahead of File ▸ New Window while the tree
  has focus), ⌘F the filter, ⌘Z undoes the last delete while its bar shows,
  Esc clears the filter, then leaves.
- **Rename in place**: the name (a document's without `.md`) in a field over
  the row, selected; Return or leaving the field moves it, Esc keeps the old
  name. A bad name (empty, a slash, a leading dot) is refused in the field.
  The selection, the open folders and the open document follow the move.
- **New**: ⌘N / ⇧⌘N / the header's buttons / the menu make `Untitled.md`
  (`Untitled 2.md`, …) or `New Folder` in the selected folder (the top level
  from the empty space's menu), open it, and rename it in place.
- **Delete** trashes through the hub (`DocsRequest::Trash`, which keeps the
  bytes in the hub's memory) and shows an Undo bar over the footer for 8s;
  Undo (or ⌘Z) puts it back (`DocsRequest::Untrash`).
- **Drag**: a row's top and bottom quarters drop before and after it, its
  middle into it (a document's halves before and after). Dropping into a
  folder moves; a folder dropped before or after a sibling folder reorders
  them (`DocsRequest::Reorder`, the docs' `folderOrder`), moving it first if
  it came from elsewhere. Documents have no order of their own (pinned, then
  by title, as ContextStore sorts them), so a document dropped beside a row
  only changes folders. The space under the rows is the top level.
- **Context menu** (right-click a row, or the empty space for the top
  level): New Document, New Folder, Rename, Duplicate
  (`plan copy.md`), Move to… (the folders, minus where it is and what's
  inside it), Reveal in Finder (dimmed for a project on another machine),
  Copy Path (the docs-relative path, what `shelbi docs` takes), Delete.
- **Filter**: matches titles and file names; a match's folders open.
- **Kept per project**: the open folders and the open document ride the
  project's view state (`docs_expanded`, `docs_document` in `ProjectViewState`).
- **Live**: while the Documentation shows it follows the hub's pushed changes on its
  own stream (`shelbi_app::docs_sub`: `docs`, `docs-status`, `docs-open`), so
  a `shelbi docs new` from an agent re-reads the tree (and the open document
  when it changed) within a quarter second of the hub's event; a status
  change re-reads the footer's line; `shelbi docs open <path>` selects and
  opens it. It pings the hub's `Sync(Active)` once a minute so the hub pulls
  on its fast clock. Remote project tabs ask the far hub through the link and
  follow its stream over the relay.
- **Main area**: the open document under a breadcrumb, read-only, with the
  Changelog's Markdown renderer (the in-place editor is `docs-desktop-document`).
  With none open, how many documents there are. A project without docs
  shows the set-up screen: the two sources and "Create docs/planning" /
  "Use docs/", which write the `docs:` block and ask the hub to set it up
  (the shared Settings ▸ Documentation form's Set up).
- **Looks like a terminal** (`docs-doc-terminal-look`): the whole main area
  (breadcrumb, document, set-up screen, conflict inspector) paints the
  surface a terminal pane does (`views::docs::surface`, the theme's
  `terminal_canvas` through `nested`: the terminals' own background whatever
  the contrast, the window's glass while translucent), in every theme. The
  document's body text is the terminal font size
  (`TerminalSettings::font_size`, `desktop.terminal_font_size`, default
  `DEFAULT_FONT_SIZE`): `DocEditor::set_body_size` scales headings, code,
  list gutters and the gaps between blocks from it. The Documentation observes the
  `TerminalSettings` global, so View ▸ Bigger / Smaller / Actual Size and
  Settings' size resize an open document at once. Only the size changes: the
  document keeps the UI's prose font, and the toolbar, bars and the tree
  keep the UI's sizes. Other `DocEditor`s (the task pane) stay at scale 1.

## Decisions

- **One entity, two halves.** `DocsView` renders the column (its `Render`)
  and the main area (`render_main`), so the tree's selection and the shown
  document can't disagree.
- **Its own change stream** (`docs_sub`) rather than the feed's
  `ChangeWake::Docs`, opened only while the Documentation shows, so no other view
  pays for docs edits, and it carries `docs-open` too.
- **Trash in the hub's memory**, not the platform trash: the service is the
  docs' one writer (and may be on another machine), and the docs' git
  history keeps every committed version anyway. It keeps the last 32
  removals until the hub restarts.
- **Copy Path copies the docs-relative path**, which means the same thing on
  every machine and is what `shelbi docs` and agents take.
- **The tree's header has only New Document, New Folder and Toggle
  sidebar** after the title: no ⋮ (its New Document, New Folder and Reveal
  in Finder are the header's buttons, ⌘N / ⇧⌘N and the context menu) and
  no Search (the palette, or the sidebar's Search, finds docs
  documents).

## Tests

`views::docs::tests` (a fake hub, `views::docs::fake`): the rows and first
open level, arrow keys and Enter, clicks, F2 rename and Esc, ⌘N / ⇧⌘N,
Delete and Undo, the context menu and Move to…, Duplicate and Copy Path,
drops (move, reorder, into itself refused), the filter, pushed changes,
folder emoji, the set-up screen. `views::docs::doc_tests`: the document's
scale tracks `TerminalSettings` (and its height with it), the main area's
surface is the terminals' in light and dark, at a contrast and on glass. `app::docs_tests`: the nav row's place and
icon, the push and ‹ / Esc back, the state kept per project, the one-click
set-up writing the `docs:` block. `SHELBI_VISUAL_ONLY=docs --visual-figma`
renders `docs`, `docs-menu`, `docs-rename`, `docs-undo` and `docs-setup`.
