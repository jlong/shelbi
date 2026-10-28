# tui-settings-tables: every settings table as a real table

## The change

The TUI's settings lists drew as loosely aligned text columns with a flat
selection bar and no pointer feedback. Every table now comes from one
widget: a header row over a rule, a `│` between every cell, and the lit row
as a half-block pill that also follows the mouse.

```
   Workspace   │ Machine  │ Tags     │ Slot
  ─────────────┼──────────┼──────────┼────────
   alpha       │ hub      │ frontend │ 0 auto
 ▐ bravo       │ hub      │ No tags  │ 1 auto ▌   <- selected or hovered
 ▐ …/wt/bravo  │ Local    │          │        ▌
```

## The widget (`crates/shelbi-tui/src/table.rs`)

- `Table` takes `Column`s (title, min, max, `fill`, a drop rank, an
  optional title style) and rows of `Cell`s (one or more lines of styled
  spans; a line can keep its end when cut, for paths), plus the selected
  row and whether its list has the keyboard. `lines(width)` returns each
  drawn line with the row it belongs to and the line as drawn under the
  pointer; `layout` and `column_x` say where each column landed (the
  Agents `⋮` click, the Keybindings chord hits).
- A lit row takes its fill across its whole width, capped by `▐` and `▌`
  whose foreground is the fill (the sidebar's half-block selection on its
  side). The selection's fill is `selection`; the pointer's is the new,
  lighter `row_hover`; the selection wins on a row that is both. On the
  selection, muted text lifts to gray and the row's first text goes loud
  and bold while the list is focused; a span with its own fill (a badge, a
  picked chord) keeps it.
- Lines are the table's width plus two columns each side (the cap and a
  column of fill); pages draw them out into their gutter, so the cells'
  text lines up with the page's other text.
- Columns size to content (headers included) between min and max; a fill
  column takes the rest. When even every minimum won't fit, the highest
  drop rank goes first; otherwise the fill column gives first, then the
  column with the most slack, cut with `…`. Nothing wraps.
- An empty table centers a muted note under its header.
- The rules are the new `table_rule` token (dark and light). The light
  audit now treats box-drawing glyphs as shapes, as it already did blocks.

## Hover

- The shell already records the pointer for every mouse event and clears it
  on focus loss; Settings now gets it (none while an overlay is up) and
  picks the row under it at draw time from where each table line actually
  landed, so a scroll or page switch can't leave a stale row lit. A page's
  prompt, menu, modal or form takes the pointer too.
- `SettingsShell::hover_moved` compares the row under the pointer with the
  one last drawn lit; the shell repaints when it changes (Settings already
  repaints on its own mouse events).
- Layers over the page (the workspace and machine forms, the removal
  confirm, the leave prompt) clear the two gutter columns beside them, so
  the selection's caps behind them don't peek out.

## The tables (columns as the desktop's)

- **Workspaces**: Workspace (its worktree under it), Machine (its kind under
  it), Tags, Slot. Narrow drops Tags, then Slot, then Machine.
- **Machines** (the desktop shows a list; its row's fields became
  columns, labeled as its form): Machine, SSH host (the probe's status
  under a remote's host; Local for this computer), Project directory,
  Workspaces. Why a machine is unreachable is said under its row.
- **Workflows**: Workflow (the Default badge, its description under it),
  Statuses, Transitions. An unreadable file is a row saying why.
- **Statuses**: Name (the category's dot), ID, Category, Used in. A rename
  field and a row's error sit under its row.
- **Agents**: Agent (the Customized badge, description under it), Preferred
  runner (Recommended under it), Workflow statuses (wrapped to the column),
  Skills, `⋮`. Narrow drops Skills, then the runner, then the statuses,
  replacing the old stacked layout.
- **Keybindings**: one table per context, headed by the context's title
  and when it applies (the desktop's group header), its labels one width
  across every context so the rules line up, the chords in the second
  column.

## Verified

Buffer tests: the widget (header, rule, selection caps, hover look, tall
rows, cuts and drops, spare width, the empty note, badges, tiny widths),
the Workspaces pill over both lines and its narrow drops, the Agents narrow
drops, and a shell test that the hover follows the pointer, clears off the
table, survives a scroll and a page switch without going stale, hides
under an overlay, and clears on focus loss. Eye check in a sandboxed root
at 160x45, 100x30 and 60x20 for Workspaces, Workflows, Agents and
Keybindings with a hovered row.
