# changelog-desktop: the Changelog page

Part of the `changelog` feature. The foundation (`feature/changelog`) put the
entry model, link resolution and the shared `ChangelogModel` in place and
routed `View::Changelog` with an empty main area; this draws the page.

## The page

```
 Changelog  October 2026                               [🔍 Search      ]
 ────────────────────────────────────────────────────────────────────────
                Settings polish pass
                October 17, 2026
                Every settings page, dialog and editor now matches ...
                  • General and Terminal, with live previews
                [▤ tui-settings-polish] [⇄ #1942] [◍ shelbi.dev/docs/settings]
                ──────────────────────────────────────────────────────
                Contact Us feedback dialog
                ...
                That's the whole changelog: 5 entries
 ────────────────────────────────────────────────────────────────────────
 ↑/↓ scroll  space page  j/k or ⌥↓/⌥↑ next/previous entry  home/end ...
```

The Figma file draws the sidebar row (the Navigation component's
`rocket-launch`, ⌘L) but no page, so the page follows the app's other views
(`src/views/changelog.rs`):

- The band is the Issues Header's (52pt, `toolbar` fill, the canvas edge on
  its left) with the Searchbox at the right. Its subtitle is the month of the
  entry at the top of the view (a sticky date that fits the band), or
  "N of M entries" while a search is up.
- One centered column, at most 680pt wide (about 75 characters of
  `Desktop/Base`), 32pt either side. Each entry: the title in
  `Desktop/Heading`, loud; the date in `Desktop/Small`, muted; the
  description through the app's Markdown renderer (`markdown::body_with_state`,
  the issue detail's styles); a chip per link; a divider under every entry
  but the last.
- Chips are the Button at Small, Secondary, with the link kind's icon: the
  Issues stack for an issue, `arrows-right-left` for a PR, the globe for any
  other URL. An issue chip opens the issue's detail on the board (the window's
  `open_task`, as an Activity row does); PR and URL chips open the browser.
  A PR number with no known GitHub repository is drawn disabled, with a
  tooltip.
- Empty: the app's `empty_state` ("Nothing has shipped yet. ..."). A search
  that matches nothing says so with a Clear search button.

## Decisions

- **Every link is a chip, and the description is drawn without the
  template's link list.** The shipped `changelog-template.md` ends each
  description with `{{links}}`, a list of bare `issue:<id>` / `#<n>` / URL
  items. Drawing that list and chips would show every link twice, and the
  list's `issue:` items aren't clickable. `ChangelogEntryView::prose`
  (shared, in `shelbi-app`) is the description without a trailing list whose
  every item is one bare link, set apart by a blank line; anything else is
  left whole. Display only; the files are untouched.
- **Search highlights** use the shared `changelog::match_ranges` (the same
  words `set_query` matches, case-insensitive, overlaps merged): the title
  through `StyledText` highlights, the description through the text view's
  range highlights (`TextViewState::set_range_highlights`, re-run when the
  view's parse lands or the query changes), both in `search_match`. A chip
  whose label holds a searched word draws in the Select style.
- **Keys** (listed in the page's footer): ↑/↓ scroll 40pt, Space / ⇧Space
  and PgDn/PgUp a page (keeping 48pt of the last one), Home/End and ⌘↑/⌘↓
  the ends, J/K and ⌥↓/⌥↑ the next and previous entry (to its top; back
  goes to the start of the entry the top is in first), ⌘F and `/` search,
  Esc clears a search (and passes through when there is none). In the search
  field Esc clears and leaves, Enter keeps the query and leaves, as on the
  board.
- **Scrolling and paging**: the entries are a `gpui::list` (only those in
  view are built), wheel lines stepped as Activity's are
  (`board_scroll::wheel_list`). The model shows 50 entries at a time; when
  the row under the last one is laid out, the next page is shown on the
  following frame.
- **Live**: a background worker checks the changelog directory's stamp
  (`ChangelogWatch`) every 750ms and reads only when it changed, so nothing
  touches disk on the UI thread and an entry the orchestrator adds appears on
  its own.
- **Icons**: Changelog takes the Figma's rocket (vendored Heroicons v2.2.0
  `rocket-launch`) in the sidebar and the palette; palette-only Activity gets
  back the bolt the Figma Activity frame draws.

## Verification

- `cargo test` (desktop): the key map, next/previous entry targets, and a
  window test over a sandboxed `SHELBI_HOME` (entries newest first, an issue
  chip emitting `OpenIssue`, an entry added on disk appearing live, ⌘F into
  the search, narrowing, the no-match state, Esc clearing) and the empty
  state. `shelbi-app`: `match_ranges` and `prose`.
- `SHELBI_VISUAL_ONLY=changelog shelbi-desktop --visual-figma <dir>` renders
  the page through the real window in both themes: `changelog-empty`,
  `changelog` (the fixture entries added after the page opened, read live),
  `state-changelog-search` ("board") and `state-changelog-next` (J twice),
  with checks for each. The captures are in
  `~/.shelbi/projects/shelbi/design-refs/changelog-desktop/`.
