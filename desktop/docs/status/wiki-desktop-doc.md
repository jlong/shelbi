# wiki-desktop-doc: the Wiki's document, edited in place

Part of the Wiki (`Plans/wiki.md` in the Shelbi ContextStore space). Builds
on `wiki-desktop-tree` ([`wiki-desktop-tree.md`](wiki-desktop-tree.md)).

```text
┌──────────────────────────────────────────────────────────────────────┐
│ Plans › OpenCode integration      Updated by an agent · 2m ago   ⋮  │
│ ⚠ Changed elsewhere while you were editing.     Keep mine   Reload  │
│ ──────────────────────────────────────────────────────────────────── │
│  # OpenCode integration                                              │
│  ## Context                                                          │
│  OpenCode is a terminal coding agent …▏                              │
└──────────────────────────────────────────────────────────────────────┘
```

## What it does

- **Editing in place.** The open document is the task pane's editor
  (`views/doc_editor`, over `shelbi_app::markdown_edit`): no edit mode, the
  caret goes where you click, a click under the text puts it at the end.
  Esc hands the keys back to the tree. Its body is set at the terminal font
  size, on the terminals' surface (see `wiki-desktop-tree.md`, "Looks like
  a terminal").
- **Saves through the hub.** Once the typing pauses
  (`shelbi_app::wiki_doc::SAVE_AFTER`, 600 ms), the whole document goes to
  the hub's wiki service as one `WikiRequest::Write` (`by: Person`), one
  save at a time; ⌘S, Esc, leaving the editor or opening another document
  save at once. The hub commits on its own clock. A new first H1 re-reads
  the tree so its label follows. A refused save says why in the toolbar
  and keeps the text.
- **Live reload.** A pushed `wiki` change to the open document reads it
  again; `shelbi_app::wiki_doc::DocSync` tells our own save's echo (nothing
  happens) from someone else's edit. No unsaved edits: the editor takes it
  in place, caret and undo kept. Unsaved edits: the "Changed elsewhere" bar,
  and nothing saves until it's answered: **Reload** takes theirs, **Keep
  mine** saves ours over it.
- **Toolbar.** The breadcrumb (folders › the document's title); "Updated
  by you / an agent / <git author> · 2m ago"; ⋮ with **Open in Editor** (the
  file on this machine in the user's editor, what's typed saved first;
  dimmed for a project on another machine), **Copy Path** (the wiki path
  `shelbi wiki` takes) and **Show History** (its last 30 commits: subject,
  who, when, short sha).
- **Palette.** Typing searches the wiki too: a Wiki group (and chip,
  between Issues and Changelog Entries) of documents whose title or file
  name fuzzy-matches, then those whose text holds the query (the hub's
  `search`, case-insensitive), each with its folder and the matching line.
  Choosing one runs `CommandKind::OpenWikiDoc`: the Wiki shows, the
  document's folders open, its row is selected and it opens.
- **`wiki:` links.** ⌘-click on `[plan](wiki:Plans/foo.md)` in a task body
  (or a wiki document) dispatches `links::OpenWikiLink`, which the window
  runs as `OpenWikiDoc`. `wiki:/x`, `wiki://x`, `#section` and `%20` read
  as expected (`shelbi_app::wiki_doc::link_path`). A path the wiki doesn't
  have says so over the footer.

## Decisions

- **Who changed it comes from the hub.** Every commit is the hub's, in the
  user's git name, so the git author can't tell an agent from the user. The
  hub now records who made a commit's changes in a `Shelbi-By: person` /
  `Shelbi-By: agent` trailer (one line each, from the writes since the last
  commit), and `Log` answers it as `WikiCommit.by` (absent from an older
  hub, and for a commit made elsewhere: then the git author's name shows).
  Between commits, the pushed change's `source` (`local` / `agent`) says
  who; `WikiWake::Changed` carries it now.
- **The timer is the debounce.** Each edit replaces the save's timer, so
  the one that fires is the last edit's; `DocSync`'s own clock is for a
  client without one.
- **Whole-document writes**, as the CLI's `write` without a section: the
  editor holds the whole text, frontmatter and H1 included.
- **Wiki search runs on the palette's search thread**, through the window's
  link (the far hub for a remote tab): `SearchCorpus.wiki`
  (`palette_search::WikiSearch`, skipped on the wire: each client attaches
  its own). The TUI's palette gets the same group, and opens a result in its
  Wiki (`WikiPage::reveal`).
- **Known race**: the hub has no compare-and-swap write, so an edit made
  elsewhere in the instant between our save's send and its landing can be
  overwritten. The changed-elsewhere bar covers every other order.

## Tests

`views::wiki::doc_tests` (the fake hub): typing saves once it pauses and on
⌘S; a new title relabels the tree; opening another document saves the one
left; a clean document reloads in place (and the line says an agent); our
own save's echo leaves the editor alone; a dirty one shows the bar, and
Reload / Keep mine; a refused save; the updated-by line, Copy Path and Show
History; Open in Editor dimmed remotely; `open_at` before the tree and for
a missing path; Esc saves and hands the keys back. `app::wiki_tests`: a
`wiki:` link runs `OpenWikiDoc`; `OpenWikiDoc` shows the Wiki at the
document (from another view, and while it shows); the palette finds
documents by title and by text. `shelbi_app::wiki_doc` and
`palette_search` unit tests cover the sync, the line, links and ranking;
`shelbi-wiki`'s `log_says_who_made_each_commits_changes` the trailer.
`SHELBI_VISUAL_ONLY=wiki --visual-figma` adds `wiki-history`,
`wiki-changed` and `wiki-palette`.
