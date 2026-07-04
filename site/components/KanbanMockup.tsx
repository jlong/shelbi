"use client"

import { useState } from "react"

/**
 * A macOS-Terminal-window frame around an ASCII-art capture of the Shelbi
 * TUI's full dashboard — sidebar on the left, kanban body on the right.
 * The frame (traffic lights, rounded corners, drop shadow, title bar) is
 * DOM; both inner panels are `<pre>` blocks with character-perfect
 * alignment — the point is that they read as text captured from a real
 * terminal, not a re-implementation of the TUI in HTML.
 *
 * `AppMockup` is the reusable, state-driven engine: hand it an `AppState`
 * describing the board, sidebar, selection, active nav view, and window
 * titles and it renders that scenario. The scenario is the ONLY thing that
 * varies — the rendering (segment model, alignment, palette, frame) is
 * fixed so every mockup reads as the same real terminal capture and can't
 * drift from the running TUI. `KanbanMockup` is a thin preset that renders
 * `defaultAppState`, preserving the marketing landing page verbatim.
 *
 * Colors match what `crates/shelbi-tui/src/sidebar.rs` and
 * `crates/shelbi-tui/src/kanban.rs` emit via ratatui's named
 * `Color::Gray | DarkGray | Blue | Yellow | Magenta | Green | Cyan`
 * palette, resolved to the hex values a modern dark terminal (macOS
 * Terminal / iTerm2 defaults) renders them as. Kanban column-header hue
 * is driven by `StatusCategory` in the crate — Backlog=Gray, Ready=Blue,
 * Active=Yellow, Handoff=Magenta, Done=Green (`category_color()` in
 * `crates/shelbi-tui/src/kanban.rs`). Sidebar decorations mirror
 * `WorkspaceBadge::decoration_color()` and `Row::decoration()` in
 * `crates/shelbi-tui/src/app.rs` — the same rules the palette / sidebar
 * / kanban share so this mockup can't drift from the running TUI.
 *
 * The sidebar nav is interactive: clicking (or Enter/Space on) a nav row
 * swaps the right pane between the Tasks board, the Activity feed, and the
 * Chat transcript. `AppMockup` owns the live `activeView` in local state,
 * seeded from the scenario so first paint matches the preset exactly; the
 * Activity and Chat panes are built through the same Segment/Row monospace
 * engine as the board so they read as captures of the same terminal, not an
 * HTML re-implementation. On small viewports the sidebar hides so the board
 * doesn't force horizontal scroll on phones.
 */

// ── Palette ───────────────────────────────────────────────────────────
// Terminal chrome (window frame) and terminal-body ANSI equivalents.
// Chosen to match how ratatui's named colors render in macOS Terminal
// with a Solarized-adjacent dark scheme — the setup a Shelbi user is
// most likely to see when they run the TUI locally.
const CHROME_BAR_BG = "#2d2d2d"
const CHROME_BAR_BORDER = "#1a1a1a"
const CHROME_TITLE = "#c7c7c7"
const TRAFFIC_RED = "#ff5f57"
const TRAFFIC_YELLOW = "#febc2e"
const TRAFFIC_GREEN = "#28c840"

const TUI_BG = "#1e1e1e"
const TUI_FG = "#e5e5e5"
const TUI_GRAY = "#b8b8b8" // ANSI 7 — column header for `Backlog`, nav labels
const TUI_DARK_GRAY = "#7c7c7c" // ANSI 8 — chrome text (`Tasks · `, ids, footer)
const TUI_DIVIDER = "#4a4a4a" // mid-gray rule — sidebar/board divider, reads on TUI_BG
const TUI_BLUE = "#4a8fd7" // ANSI 4 — `Ready` category (todo)
const TUI_YELLOW = "#dcb767" // ANSI 3 — `Active` category (in_progress)
const TUI_MAGENTA = "#c586c0" // ANSI 5 — `Handoff` (review) + `@workspace`
const TUI_GREEN = "#5fb56d" // ANSI 2 — `Done` category + Working badge
const TUI_CYAN = "#4ec9b0" // ANSI 6 — project name in title bar + ✓ review badge
const TUI_LIGHT_RED = "#ff6e67" // ANSI 9 — foxtrot's avatar tint (`Color::LightRed` in activity.rs)
// Zen-mode band fill: the TUI paints the "ZEN MODE ON" footer bar with an
// explicit `bg(Color::Rgb(0,127,0))` + `fg(White)` bold (`render_zen_row`
// in `crates/shelbi-tui/src/sidebar.rs`) — not a named ANSI color, so it
// renders as this exact hex rather than the brighter ANSI-2 `TUI_GREEN`.
const TUI_ZEN_GREEN = "#007f00" // Rgb(0,127,0) — full-width Zen band fill
// Focused-card highlight: the shared selection gray `theme::SELECTION_BG`
// (`Color::Rgb(63,63,63)`) the TUI paints every selection background with —
// nav, kanban card, review queue, and filter dropdowns all use this one
// value so they can't drift. Selected text keeps an explicit `fg(White)` +
// bold so it stays readable on the gray.
const SEL_BG = "#3f3f3f" // ratatui `bg(theme::SELECTION_BG)` for a focused card
const SEL_FG = "#ffffff" // ratatui `fg(White)` for a focused card
// Sidebar's per-row selection fill uses the same `theme::SELECTION_BG` gray
// as the focused card above — one selection color across every surface.
const SIDEBAR_SEL_BG = SEL_BG

// ── Layout constants ─────────────────────────────────────────────────
// Kanban columns are fixed at 22 monospace cells (20 for card text + a
// 2-cell right gutter) — the same shape the TUI uses at 5-column
// widths. Total board width = 1 leading pad + 5 × 22 = 111 cells.
// Sidebar content width is 28 (matches the TUI's 30-col sidebar minus
// its 1-col horizontal padding on each side).
const COL_W = 22
const TEXT_W = 20
const SIDEBAR_W = 30
/**
 * The board content width for a scenario, in monospace cells: 1 leading pad
 * + one `COL_W` cell per column (so a 5-column board is `1 + 5 * 22 = 111`,
 * the canonical dashboard width the TUI draws). This is the width every view
 * within a single mockup pins to — the Chat and Activity panes size to it so
 * switching nav rows never changes the frame width, even for boards with
 * fewer or more than five columns (e.g. a four-column trunk-based board).
 */
function contentWidth(columns: Column[]): number {
  return 1 + columns.length * COL_W
}

// ── Scenario model ────────────────────────────────────────────────────
// The public `AppState` shape. Every field is pure data — an MDX doc
// author declares (or spreads a preset and tweaks) one of these to show a
// specific interface state, and `AppMockup` renders it through the fixed
// engine below.

export type Category = "gray" | "blue" | "yellow" | "magenta" | "green"

/** One kanban card. `selected` draws the focused-card gray highlight. */
export type Card = {
  title: string
  id: string
  workspace?: string
  selected?: boolean
}

/** One kanban column: a category-colored header over its stack of cards. */
export type Column = {
  label: string
  category: Category
  cards: Card[]
}

/** A workspace row in the sidebar, grouped under its machine. */
export type Workspace = {
  name: string
  state: "idle" | "working"
  agent?: string
}

/** A machine group in the sidebar (`▾ <name>` + its workspaces). */
export type Machine = {
  name: string
  workspaces: Workspace[]
}

/**
 * One entry in a sidebar review section, mirroring `ReviewEntry` /
 * `Row::Review` in the crate. Rendered as two lines: line 1 is the badge +
 * title (Ready adds the `machine:port` served URL right-aligned); line 2 is
 * the `branch`, dim. `location` is set only for a Ready (serving) entry — a
 * Queued one has no slot yet, so no URL.
 */
export type ReviewEntry = {
  title: string
  /** Branch shown dim on line 2, e.g. `shelbi/<id>`. */
  branch: string
  /** `machine:port` served URL — present for Ready, omitted for Queued. */
  location?: string
}

/**
 * Which right-pane view is active — also drives the title-bar label. The
 * first three map to the sidebar nav rows; `workspace` is the focused-worker
 * pane (a Claude-Code session for one workspace), reached by selecting a
 * workspace rather than a nav row, so it carries no nav entry of its own.
 */
export type NavView = "chat" | "tasks" | "activity" | "workspace"

/**
 * The full interface state a single mockup renders. Pass one to
 * `<AppMockup state={...} />`; start from a preset (`defaultAppState`,
 * `starterAppState`) and override only what your scenario changes.
 */
export type AppState = {
  /** macOS window-chrome title, e.g. `jlong@hub — shelbi`. */
  terminalTitle: string
  /** Project name — Cyan-bold in the sidebar header and the title bar. */
  project: string
  /** Highlighted nav row; also selects the title-bar label (Chat/Tasks/Activity). */
  activeView: NavView
  /** Kanban columns rendered in the main board (5-column layout). */
  columns: Column[]
  /** Sidebar machine groups and their workspaces. */
  machines: Machine[]
  /**
   * Sidebar "Ready for Review" entries — tasks loaded on a review workspace
   * and serving (cyan ✓, with a `machine:port` location). Empty hides the
   * section.
   */
  readyReview: ReviewEntry[]
  /**
   * Sidebar "Queued for Review" entries — Review-status tasks waiting for a
   * free review workspace (blue ·, no location yet). Empty hides the section.
   */
  queuedReview: ReviewEntry[]
  /**
   * Whether the sidebar shows the full-width green "ZEN MODE ON" band. Opt-in
   * and defaults to off (undefined/false) — only the hero and docs pages that
   * actually describe Zen Mode turn it on, so every other mockup reads as a
   * normal (non-Zen) session.
   */
  zenMode?: boolean
  /**
   * Overrides the Chat pane transcript. When omitted the built-in orchestrator
   * session (`CHAT_LINES`) renders — the static docs default. The hero
   * animation passes a progressively-revealed transcript here to drive the
   * typing + streaming beats through the same Claude-Code renderer.
   */
  chatLines?: ChatLine[]
  /**
   * For `activeView: "workspace"` — the focused workspace's name, shown as the
   * title-bar label in place of a nav label (e.g. `alpha · my-project`).
   */
  focusedWorkspace?: string
  /**
   * For `activeView: "workspace"` — the focused worker's Claude-Code transcript,
   * rendered through the same engine as the Chat pane so the worker pane reads
   * as a capture of that agent's session.
   */
  workspaceLines?: ChatLine[]
  /**
   * Pins the terminal body to at least this many rows. Views shorter than the
   * pin are padded with blank rows so the frame height never changes as a
   * scenario's content grows or shrinks — the hero animation sets it once so
   * the frame stays a fixed size across every beat (cards appearing, columns
   * emptying, panes swapping) instead of resizing with the tallest column.
   */
  minBodyRows?: number
}

const CATEGORY_COLOR: Record<Category, string> = {
  gray: TUI_GRAY,
  blue: TUI_BLUE,
  yellow: TUI_YELLOW,
  magenta: TUI_MAGENTA,
  green: TUI_GREEN,
}

// Sidebar nav rows, in display order. `label` doubles as the title-bar
// label for the matching `activeView`.
const NAV_ITEMS: { view: NavView; glyph: string; label: string }[] = [
  { view: "chat", glyph: "💬", label: "Chat" },
  { view: "tasks", glyph: "📋", label: "Tasks" },
  { view: "activity", glyph: "⚡", label: "Activity" },
]

// ── Segment model ─────────────────────────────────────────────────────
// A "segment" is one contiguous run of monospace cells with a single
// style. Each row is a list of segments concatenated left-to-right.
type Segment = {
  text: string
  color?: string
  bg?: string
  bold?: boolean
}

/** A full-width blank board row, sized to a scenario's content width. */
function blankRow(width: number): Segment[] {
  return [{ text: " ".repeat(width) }]
}
const SIDEBAR_BLANK_ROW: Segment[] = [{ text: " ".repeat(SIDEBAR_W) }]

/**
 * Truncate `text` to fit inside `width` monospace cells, using the same
 * `…`-suffix rule the TUI's `truncate()` helper uses
 * (crates/shelbi-tui/src/kanban.rs `truncate`).
 */
function truncate(text: string, width: number): string {
  const chars = [...text]
  if (chars.length <= width) return text
  return chars.slice(0, width - 1).join("") + "…"
}

/** Right-pad a text run with spaces so it fills exactly `width` cells. */
function padTo(text: string, width: number): string {
  const len = [...text].length
  return len >= width ? text : text + " ".repeat(width - len)
}

/** Blank column cell — 22 spaces of padding, no color. */
function blankColRow(): Segment[] {
  return [{ text: " ".repeat(COL_W) }]
}

/**
 * Build the visible rows for a single column: header row followed by
 * one entry per card (2 rows each: title + id/@workspace) with a blank
 * row between cards.
 */
function columnRows(col: Column): Segment[][] {
  const rows: Segment[][] = []

  // Header row: `LABEL (N)` in the category color, count in DarkGray,
  // padded to COL_W. Total = "LABEL" + " (N)" — no leading pad because
  // the caller adds the board-level " " before column 1.
  const headerText = `${col.label} (${col.cards.length})`
  const padCount = COL_W - [...headerText].length
  const catColor = CATEGORY_COLOR[col.category]
  rows.push([
    { text: col.label, color: catColor, bold: true },
    { text: ` (${col.cards.length})`, color: TUI_DARK_GRAY },
    { text: " ".repeat(Math.max(0, padCount)) },
  ])

  col.cards.forEach((card, idx) => {
    if (idx > 0) rows.push(blankColRow())

    // Title row — truncated to TEXT_W with ellipsis, right-padded with
    // the 2-cell gutter that lives inside the column's cell.
    const title = truncate(card.title, TEXT_W)
    const titlePad = COL_W - [...title].length
    if (card.selected) {
      rows.push([
        {
          text: padTo(title, COL_W),
          color: SEL_FG,
          bg: SEL_BG,
          bold: true,
        },
      ])
    } else {
      rows.push([
        { text: title, color: TUI_FG, bold: true },
        { text: " ".repeat(Math.max(0, titlePad)) },
      ])
    }

    // Meta row — id (DarkGray) + optional `  @workspace` (Magenta).
    const wsSuffix = card.workspace ? `  @${card.workspace}` : ""
    const metaBaseLen = card.id.length + wsSuffix.length
    const metaPad = COL_W - metaBaseLen
    if (card.selected) {
      // Selected card: the highlight bg spans the full cell — id and
      // workspace both render as white-on-gray instead of their normal
      // hues, matching how ratatui applies `List::highlight_style` to
      // the entire row.
      rows.push([
        {
          text: padTo(`${card.id}${wsSuffix}`, COL_W),
          color: SEL_FG,
          bg: SEL_BG,
        },
      ])
    } else {
      const segs: Segment[] = [{ text: card.id, color: TUI_DARK_GRAY }]
      if (card.workspace) {
        segs.push({ text: "  " })
        segs.push({ text: `@${card.workspace}`, color: TUI_MAGENTA })
      }
      segs.push({ text: " ".repeat(Math.max(0, metaPad)) })
      rows.push(segs)
    }
  })

  return rows
}

/**
 * Zip all columns row-by-row into a single board grid. Shorter columns
 * are padded down with blank rows so the grid stays rectangular.
 */
function buildBoardRows(columns: Column[]): Segment[][] {
  const perCol = columns.map(columnRows)
  const maxRows = Math.max(...perCol.map((r) => r.length))
  const grid: Segment[][] = []

  for (let r = 0; r < maxRows; r += 1) {
    const rowSegs: Segment[] = [{ text: " " }]
    for (let c = 0; c < perCol.length; c += 1) {
      const cellRow = perCol[c][r] ?? blankColRow()
      rowSegs.push(...cellRow)
    }
    grid.push(rowSegs)
  }
  return grid
}

/** Title bar row: "<View> · <project>  N total" + right-aligned chips. */
function titleRow(state: AppState): Segment[] {
  const { columns, project, activeView } = state
  const width = contentWidth(columns)
  const total = columns.reduce((n, c) => n + c.cards.length, 0)
  // The focused-worker pane has no nav row, so its label is the workspace name
  // (e.g. `alpha · my-project`); the nav views use their nav label.
  const label =
    activeView === "workspace"
      ? (state.focusedWorkspace ?? "workspace")
      : (NAV_ITEMS.find((n) => n.view === activeView)?.label ?? "Tasks")
  const leftText = ` ${label} · ${project}   ${total} total`
  const workflow = "Workflow: All ▾"
  const workspace = "Workspace: All ▾ "
  const leftLen = [...leftText].length
  const rightLen = [...workflow].length + 2 + [...workspace].length
  const midPad = Math.max(1, width - leftLen - rightLen)
  return [
    { text: ` ${label} · `, color: TUI_DARK_GRAY },
    { text: project, color: TUI_CYAN, bold: true },
    { text: `   ${total} total`, color: TUI_DARK_GRAY },
    { text: " ".repeat(midPad) },
    { text: workflow, color: TUI_DARK_GRAY },
    { text: "  " },
    { text: workspace, color: TUI_DARK_GRAY },
  ]
}

/**
 * Footer keybinding hints — matches `render_footer` in the crate, whose hints
 * are per-view. Each view keeps the same key-in-fg / hint-in-dg pattern and
 * pads to the board `width` so the footer (and the frame) is identical across
 * views.
 */
function footerRow(activeView: NavView, width: number): Segment[] {
  const segs: Segment[] = []
  const push = (t: string, color = TUI_DARK_GRAY) => segs.push({ text: t, color })
  const key = (t: string) => push(t, TUI_FG)
  // [key, hint] pairs per view.
  const hints: [string, string][] =
    activeView === "chat" || activeView === "workspace"
      ? [["⏎", " send   "], ["↑/↓", " scroll   "], ["esc", " tasks"]]
      : activeView === "activity"
        ? [["j/k", " scroll   "], ["r", " refresh   "], ["esc", " tasks"]]
        : [
            ["h/l", " col   "],
            ["j/k", " row   "],
            ["⏎", " open   "],
            ["n", " new   "],
            ["f", " filter   "],
            ["r", " refresh"],
          ]
  push("  ")
  for (const [k, hint] of hints) {
    key(k)
    push(hint)
  }
  const used = segs.reduce((acc, s) => acc + [...s.text].length, 0)
  if (used < width) segs.push({ text: " ".repeat(width - used) })
  return segs
}

// ── Activity & Chat panes ─────────────────────────────────────────────
// Two alternate right-pane views, built as `Segment[][]` through the same
// engine as the board so they read as captures of the real TUI's Activity
// (`crates/shelbi-tui/src/activity.rs`) and Chat (Orchestrator transcript)
// views — same palette, same monospace grid, same character alignment.

/** Pad a run of segments to the board content width so a view fills the pane. */
function bodyRow(segs: Segment[], width: number): Segment[] {
  const used = segs.reduce((acc, s) => acc + [...s.text].length, 0)
  if (used >= width) return segs
  return [...segs, { text: " ".repeat(width - used) }]
}

/**
 * Left content + a right-aligned run, padded to the board `width` with a 1-col
 * trailing margin — the board's title/footer right-alignment shape, reused for
 * Activity timestamps so they right-align at the board edge (never beyond it).
 */
function bodyRightAlignRow(left: Segment[], right: Segment[], width: number): Segment[] {
  const leftW = left.reduce((acc, s) => acc + [...s.text].length, 0)
  const rightW = right.reduce((acc, s) => acc + [...s.text].length, 0)
  const pad = Math.max(1, width - leftW - rightW - 1)
  return [...left, { text: " ".repeat(pad) }, ...right, { text: " " }]
}

/** Greedy word-wrap to `width` monospace cells; returns one string per line. */
function wrapText(text: string, width: number): string[] {
  const lines: string[] = []
  let cur = ""
  for (const word of text.split(" ")) {
    if (cur === "") cur = word
    else if ([...cur].length + 1 + [...word].length <= width) cur += ` ${word}`
    else {
      lines.push(cur)
      cur = word
    }
  }
  if (cur) lines.push(cur)
  return lines
}

/**
 * Pad a view's body to `target` rows with blank board rows so the terminal
 * frame keeps the board's height and doesn't jump when switching views.
 */
function padBodyTo(rows: Segment[][], target: number, width: number): Segment[][] {
  const out = rows.slice(0, target)
  while (out.length < target) out.push(blankRow(width))
  return out
}

// ── Activity avatars ──────────────────────────────────────────────────
// The exact half-block "face" glyph arrays + per-workspace tints from
// `crates/shelbi-tui/src/activity.rs` (`ALPHA_AVATAR` … `FOXTROT_AVATAR`,
// `avatar_for`). Each face is `AVATAR_H` rows of `AVATAR_W` cells; reusing the
// crate's exact arrays means the mockup can't drift from the running TUI.
const AVATAR_W = 4 // activity.rs `AVATAR_W`
const AVATAR_GAP = 2 // activity.rs `AVATAR_GAP` — cells between avatar and text
const ACT_LEAD = 1 // leading pad; the crate uses `horizontal_margin(2)`, but the
// mockup matches the board's 1-col left pad so the pane aligns with the title row.

type Avatar = { rows: [string, string, string]; color: string }

const AVATARS: Record<string, Avatar> = {
  alpha: { rows: ["▄▀▀▄", "█▄▄█", " ▀▀ "], color: TUI_CYAN },
  bravo: { rows: ["▄██▄", "█▄▄█", "▀  ▀"], color: TUI_MAGENTA },
  charlie: { rows: ["▄▀▀▄", "▌▄▄▐", "▀  ▀"], color: TUI_GREEN },
  delta: { rows: ["▄▄▄▄", "▌▄▄▐", "▐  ▌"], color: TUI_YELLOW },
  echo: { rows: ["▄▀▀▄", "█  █", "▀▀▀▀"], color: TUI_BLUE },
  foxtrot: { rows: ["▄  ▄", "█▀▀█", "▐▄▄▌"], color: TUI_LIGHT_RED },
}

// One Activity feed row, mirroring the event kinds `render_task_event` /
// `render_workspace_event` paint. `started`/`finished` are workspace-attributed
// (a face avatar + the crate's white-bold name); `waiting` is a muted
// workspace-state row (events.log has no literal "idle" — the nearest state is
// AwaitingInput, rendered dim); `promoted`/`accepted` are task-only rows drawn
// with a single glyph in the avatar column (★ / ✓) the way `AvatarSlot::Glyph`
// does. `detail` is the dim secondary line; `when` is the right-aligned
// relative timestamp.
type ActivityEvent =
  | { kind: "started"; ws: string; agent?: string; title: string; detail: string; when: string }
  | { kind: "finished"; ws: string; title: string; detail: string; when: string }
  | { kind: "waiting"; ws: string; verb: string; detail?: string; when: string }
  | { kind: "promoted"; title: string; detail: string; when: string }
  | { kind: "accepted"; title: string; detail: string; when: string }

// Representative `Today` feed for the sample project — same workspaces
// (alpha/bravo/charlie/echo) and task titles as the board, newest first. Titles
// are wrapped in the curly quotes the crate's `title_quoted` uses.
const ACTIVITY_EVENTS: ActivityEvent[] = [
  {
    kind: "finished",
    ws: "charlie",
    title: "Cold-start cache",
    detail: "took 12m · branch: shelbi/cold-start-cache",
    when: "12m ago",
  },
  {
    kind: "started",
    ws: "alpha",
    agent: "Developer",
    title: "Deploy staging env",
    detail: "branch: shelbi/deploy-staging-env · #2",
    when: "18m ago",
  },
  { kind: "waiting", ws: "bravo", verb: "is waiting for input", when: "25m ago" },
  {
    kind: "started",
    ws: "echo",
    agent: "Developer",
    title: "Backfill order index",
    detail: "branch: shelbi/backfill-order-index · #3",
    when: "40m ago",
  },
  { kind: "accepted", title: "Ship dark mode", detail: "moved to done", when: "1h ago" },
]

/**
 * Line 1 of an event row: `[lead][avatar cell][gap][…primary]` with the dim
 * relative timestamp right-aligned, mirroring `paint_row`'s first line. The
 * avatar cell is one row of a face (4 cells) or a glyph padded to 4 cells.
 */
function actPrimaryLine(
  cell: string,
  cellColor: string,
  primary: Segment[],
  when: string,
  width: number,
): Segment[] {
  const left: Segment[] = [
    { text: " ".repeat(ACT_LEAD) },
    { text: cell, color: cellColor },
    { text: " ".repeat(AVATAR_GAP) },
    ...primary,
  ]
  return bodyRightAlignRow(left, [{ text: when, color: TUI_DARK_GRAY }], width)
}

/**
 * A secondary/avatar-only continuation line: `[lead][avatar cell][gap][detail]`,
 * padded to the pane width. `cell` is the next avatar row (faces) or blank
 * spaces (glyph rows), `detail` the dim secondary text (omitted for the third
 * avatar row).
 */
function actContinuationLine(
  cell: string,
  cellColor: string,
  width: number,
  detail?: string,
): Segment[] {
  const segs: Segment[] = [
    { text: " ".repeat(ACT_LEAD) },
    { text: cell, color: cellColor },
  ]
  if (detail !== undefined) {
    segs.push({ text: " ".repeat(AVATAR_GAP) })
    segs.push({ text: detail, color: TUI_DARK_GRAY })
  }
  return bodyRow(segs, width)
}

/** Curly-quote a title the way the crate's `title_quoted` does. */
function quoted(title: string): string {
  return `\u{201C}${title}\u{201D}`
}

/**
 * Render one event into its terminal lines. Face events emit 3 rows (avatar art
 * on all three, primary on row 1, secondary on row 2); glyph events emit 1
 * avatar row (primary) + a blank-avatar secondary — matching `paint_row`'s
 * per-slot line counts.
 */
function activityEventRows(ev: ActivityEvent, width: number): Segment[][] {
  const rows: Segment[][] = []
  const blankCell = " ".repeat(AVATAR_W)

  const faceRows = (av: Avatar, primary: Segment[], detail: string | undefined, when: string) => {
    rows.push(actPrimaryLine(av.rows[0], av.color, primary, when, width))
    rows.push(actContinuationLine(av.rows[1], av.color, width, detail))
    rows.push(actContinuationLine(av.rows[2], av.color, width))
  }
  const glyphRows = (
    glyph: string,
    glyphColor: string,
    primary: Segment[],
    detail: string,
    when: string,
  ) => {
    rows.push(actPrimaryLine(padTo(glyph, AVATAR_W), glyphColor, primary, when, width))
    rows.push(actContinuationLine(blankCell, TUI_DARK_GRAY, width, detail))
  }

  switch (ev.kind) {
    case "started": {
      // White-bold workspace name + optional dim [agent], `started`, curly title.
      const primary: Segment[] = [{ text: ev.ws, color: SEL_FG, bold: true }]
      if (ev.agent) primary.push({ text: ` [${ev.agent}]`, color: TUI_DARK_GRAY })
      primary.push({ text: "  started  ", color: TUI_GRAY })
      primary.push({ text: quoted(ev.title), color: TUI_FG })
      faceRows(AVATARS[ev.ws], primary, ev.detail, ev.when)
      break
    }
    case "finished": {
      faceRows(
        AVATARS[ev.ws],
        [
          { text: ev.ws, color: SEL_FG, bold: true },
          { text: "  finished  ", color: TUI_GRAY },
          { text: quoted(ev.title), color: TUI_FG },
          { text: " — ready for review", color: TUI_CYAN },
        ],
        ev.detail,
        ev.when,
      )
      break
    }
    case "waiting": {
      // Muted workspace-state row — dim bold name + dim verb, face still shown.
      faceRows(
        AVATARS[ev.ws],
        [
          { text: ev.ws, color: TUI_DARK_GRAY, bold: true },
          { text: ` ${ev.verb}`, color: TUI_DARK_GRAY },
        ],
        ev.detail,
        ev.when,
      )
      break
    }
    case "promoted": {
      glyphRows(
        "★",
        TUI_CYAN,
        [
          { text: "Promoted", color: TUI_GRAY, bold: true },
          { text: "  ", color: TUI_GRAY },
          { text: quoted(ev.title), color: TUI_FG },
        ],
        ev.detail,
        ev.when,
      )
      break
    }
    case "accepted": {
      glyphRows(
        "✓",
        TUI_CYAN,
        [
          { text: quoted(ev.title), color: TUI_FG },
          { text: " accepted", color: TUI_GRAY },
        ],
        ev.detail,
        ev.when,
      )
      break
    }
  }
  return rows
}

/** The full-width `── Today ───…` date-bucket header, dim (activity.rs `date_header`). */
function activityDateHeader(label: string, width: number): Segment[] {
  const head = ` ── ${label} `
  const trail = "─".repeat(Math.max(0, width - [...head].length))
  return [{ text: head + trail, color: TUI_DARK_GRAY }]
}

function buildActivityRows(state: AppState): Segment[][] {
  const width = contentWidth(state.columns)
  const rows: Segment[][] = []

  // Date-bucketed, reverse-chronological feed — one `Today` bucket here.
  rows.push(activityDateHeader("Today", width))
  rows.push(blankRow(width))

  for (const ev of ACTIVITY_EVENTS) {
    rows.push(...activityEventRows(ev, width))
    rows.push(blankRow(width))
  }

  // Left unpadded — `TerminalBody` pads every view to a common target height so
  // the frame stays fixed across nav switches (and any `minBodyRows` pin).
  return rows
}

// One line of the Claude Code session the Chat pane hosts — the orchestrator
// agent IS a live Claude Code CLI running in a tmux pane, so this renders the
// real Claude Code UI elements rather than a generic two-speaker chat:
// - `user`  — a `❯ ` prompt turn in the foreground color.
// - `prose` — assistant narration, plain wrapped fg lines.
// - `tool`  — a `⏺ Name(args)` tool-call bullet (green ⏺, bold-ish name).
// - `result`— a `⎿` result line indented under the call, dim, optionally
//             collapsed with a trailing `… +N lines` the way Claude Code folds
//             long output.
// - `working`— the `✻ Working…` spinner line, dim accent.
// - `status`— the two-line footer (`Model: … · Ctx … · Cost …` + `⏵⏵ auto mode`).
// - `blank` — vertical spacing between turns.
export type ChatLine =
  | { kind: "user"; text: string }
  | { kind: "prose"; text: string }
  | { kind: "tool"; name: string; args: string }
  | { kind: "result"; text: string; more?: number }
  | { kind: "working"; text: string }
  | { kind: "status"; model: string; ctx: string; cost: string }
  | { kind: "blank" }

// A believable orchestration session over the sample project: the human asks
// for two pieces of work, the orchestrator adds + dispatches tasks to free
// workspaces (alpha/bravo) via `shelbi` CLI calls, then summarizes. Matches the
// board's task titles and workspaces so Chat and Tasks read as one system.
const CHAT_LINES: ChatLine[] = [
  { kind: "user", text: "Deploy the staging environment and wire up OAuth." },
  { kind: "blank" },
  { kind: "prose", text: "I'll create both tasks and dispatch them to free workspaces." },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: 'shelbi task add "Deploy staging env"' },
  { kind: "result", text: "✓ deploy-staging-env created in backlog" },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: "shelbi task start deploy-staging-env --workspace alpha" },
  { kind: "result", text: "✓ deploy-staging-env → in_progress on alpha", more: 2 },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: "shelbi task start wire-up-oauth-flow --workspace bravo" },
  { kind: "result", text: "✓ wire-up-oauth-flow → in_progress on bravo" },
  { kind: "blank" },
  {
    kind: "prose",
    text: "Both are running now — alpha on staging, bravo on OAuth. I'll merge each as it hands off for review.",
  },
  { kind: "blank" },
  { kind: "working", text: "Working… (8s · ↓ 431 tokens)" },
  { kind: "blank" },
  { kind: "status", model: "Opus 4.8", ctx: "4%", cost: "$0.36" },
]

// Assistant prose wraps at a comfortable measure rather than stretching across
// the full 111-cell pane, the way Claude Code soft-wraps its output.
const CHAT_WRAP_W = 76

function buildChatRows(state: AppState, lines: ChatLine[]): Segment[][] {
  const width = contentWidth(state.columns)
  const row = (segs: Segment[]) => bodyRow(segs, width)
  const rows: Segment[][] = []

  for (const line of lines) {
    switch (line.kind) {
      case "blank":
        rows.push(blankRow(width))
        break
      case "user":
        // `❯ ` prompt (cyan) + the human's message in the foreground color.
        rows.push(
          row([
            { text: " ❯ ", color: TUI_CYAN, bold: true },
            { text: line.text, color: TUI_FG },
          ]),
        )
        break
      case "prose":
        // Assistant narration — indented two cells, wrapped, plain fg.
        for (const wrapped of wrapText(line.text, CHAT_WRAP_W)) {
          rows.push(row([{ text: `  ${wrapped}`, color: TUI_FG }]))
        }
        break
      case "tool":
        // Tool-call bullet: green `⏺`, bold-ish tool name, args in fg.
        rows.push(
          row([
            { text: " ⏺ ", color: TUI_GREEN },
            { text: line.name, color: TUI_FG, bold: true },
            { text: `(${line.args})`, color: TUI_FG },
          ]),
        )
        break
      case "result": {
        // `⎿` result indented under the call, dim; long output collapses to
        // `… +N lines` aligned under the result text, the way Claude Code folds.
        rows.push(
          row([
            { text: "   ⎿ ", color: TUI_DARK_GRAY },
            { text: line.text, color: TUI_DARK_GRAY },
          ]),
        )
        if (line.more) {
          rows.push(row([{ text: `     … +${line.more} lines`, color: TUI_DARK_GRAY }]))
        }
        break
      }
      case "working":
        // Spinner/working line in a dim magenta accent.
        rows.push(
          row([
            { text: " ✻ ", color: TUI_MAGENTA },
            { text: line.text, color: TUI_DARK_GRAY },
          ]),
        )
        break
      case "status":
        // Footer status: model/context/cost line, then the auto-mode hint.
        rows.push(
          row([
            {
              text: `  Model: ${line.model} · Ctx ${line.ctx} · Cost ${line.cost}`,
              color: TUI_DARK_GRAY,
            },
          ]),
        )
        rows.push(
          row([
            { text: "  ⏵⏵ ", color: TUI_GREEN },
            { text: "auto mode on (shift+tab to cycle)", color: TUI_DARK_GRAY },
          ]),
        )
        break
    }
  }

  // Left unpadded — `TerminalBody` pads to the shared target height.
  return rows
}

// ── Sidebar ───────────────────────────────────────────────────────────
// Mirrors `crates/shelbi-tui/src/sidebar.rs::render_list` + the row
// composition in `app.rs::rows()`. One row = one line; each row is a
// list of segments the pre element joins into a monospace grid, same as
// the board.

/** Pad segments to the sidebar's fixed row width so background fills reach the edge. */
function padSidebarRow(segs: Segment[]): Segment[] {
  const used = segs.reduce((acc, s) => acc + [...s.text].length, 0)
  if (used >= SIDEBAR_W) return segs
  return [...segs, { text: " ".repeat(SIDEBAR_W - used) }]
}

/**
 * Right-align a `right` label at column `SIDEBAR_W - 1` (1-col trailing
 * pad matches `sidebar.rs`'s `Margin::horizontal: 1`). Returns a full
 * SIDEBAR_W-wide row. If the two runs together exceed the row's width,
 * the right label is dropped rather than pushing the left off-screen —
 * same fallback `right_align` uses in `sidebar.rs`.
 */
function sidebarRightAlignRow(
  left: Segment[],
  right: string,
  rightColor: string,
  bg?: string,
): Segment[] {
  const leftW = left.reduce((acc, s) => acc + [...s.text].length, 0)
  const rightW = [...right].length
  const trailing = 1 // matches sidebar's 1-col horizontal margin
  if (right && leftW + rightW + 1 + trailing <= SIDEBAR_W) {
    const pad = SIDEBAR_W - leftW - rightW - trailing
    const row = [
      ...left,
      { text: " ".repeat(pad), bg },
      { text: right, color: rightColor, bg },
      { text: " ".repeat(trailing), bg },
    ]
    return row
  }
  return padSidebarRow(left)
}

// One sidebar nav item, resolved from `NAV_ITEMS`.
type NavItem = (typeof NAV_ITEMS)[number]

/**
 * A rendered sidebar row. Most rows are plain `Segment[]` lines; nav rows are
 * emitted as a tagged descriptor instead so `Sidebar` can render them through
 * the interactive `<NavRow>` (which owns hover state and the click/keyboard
 * handlers) while every other row still flows through the static `<Row>`.
 * Plain rows stay `Segment[]` so the many `rows.push(...)` sites are unchanged
 * and the two kinds are told apart with `Array.isArray`.
 */
type SidebarRow = Segment[] | { nav: NavItem; selected: boolean }

/**
 * Segments for one nav row (💬 Chat / 📋 Tasks / ⚡ Activity). `filled` paints
 * the full-row `SIDEBAR_SEL_BG` the TUI's `render_list` gives the selected row;
 * we reuse it for the active row (always) and for hover (affordance). Active
 * text is white-bold like the real selection; a hover-only fill keeps the gray
 * label (the "subtle version") so the active row stays distinguishable.
 */
function navRowSegs(glyph: string, label: string, selected: boolean, filled: boolean): Segment[] {
  const bg = filled ? SIDEBAR_SEL_BG : undefined
  const inner: Segment[] = [
    { text: " ", bg },
    {
      text: `${glyph} ${label}`,
      color: selected ? SEL_FG : TUI_GRAY,
      bg,
      bold: selected,
    },
  ]
  const used = inner.reduce((acc, s) => acc + [...s.text].length, 0)
  if (used < SIDEBAR_W) {
    inner.push({ text: " ".repeat(SIDEBAR_W - used), bg })
  }
  return inner
}

function buildSidebarRows(state: AppState): SidebarRow[] {
  const rows: SidebarRow[] = []

  // Project header — same Cyan Bold `app.project_name` renders at.
  rows.push(
    padSidebarRow([
      { text: " " },
      { text: state.project, color: TUI_CYAN, bold: true },
    ]),
  )
  rows.push(SIDEBAR_BLANK_ROW)

  // Nav rows — 💬 Chat / 📋 Tasks / ⚡ Activity. Emitted as tagged descriptors
  // so `Sidebar` renders them through the interactive `<NavRow>`; the active
  // view's row gets the full-row fill + white-bold text `sidebar.rs::render_list`
  // applies to the selected row, and `<NavRow>` adds the hover fill on top.
  for (const nav of NAV_ITEMS) {
    rows.push({ nav, selected: nav.view === state.activeView })
  }
  rows.push(SIDEBAR_BLANK_ROW)

  // — Workspaces — section header (DarkGray).
  rows.push(
    padSidebarRow([
      { text: " " },
      { text: "— Workspaces —", color: TUI_DARK_GRAY },
    ]),
  )

  // Per-machine group header + indented workspace rows, matching the
  // `▾ <machine>` / `  <badge> <name>   <right-label>` shape from
  // `sidebar.rs`.
  for (const m of state.machines) {
    rows.push(
      padSidebarRow([
        { text: " " },
        { text: `▾ ${m.name}`, color: TUI_DARK_GRAY },
      ]),
    )
    for (const w of m.workspaces) {
      const badgeGlyph = w.state === "working" ? "⏵" : "·"
      const badgeColor = w.state === "working" ? TUI_GREEN : TUI_DARK_GRAY
      const rightLabel = w.agent ?? "idle"
      rows.push(
        sidebarRightAlignRow(
          [
            { text: " " },
            { text: "  " }, // machine-group indent
            { text: `${badgeGlyph} `, color: badgeColor },
            { text: w.name, color: TUI_GRAY },
          ],
          rightLabel,
          TUI_DARK_GRAY,
        ),
      )
    }
  }

  // The Review state splits into two sidebar sections, matching `app.rs::rows`:
  // "Ready for Review" (loaded on a review workspace, serving) then "Queued
  // for Review" (waiting for a free slot). Each is omitted when empty.
  const pushReviewSection = (label: string, entries: ReviewEntry[], ready: boolean) => {
    if (entries.length === 0) return
    rows.push(SIDEBAR_BLANK_ROW)
    rows.push(
      padSidebarRow([
        { text: " " },
        { text: `— ${label} —`, color: TUI_DARK_GRAY },
      ]),
    )
    for (const entry of entries) {
      rows.push(...reviewEntryRows(entry, ready))
    }
  }
  pushReviewSection("Ready for Review", state.readyReview, true)
  pushReviewSection("Queued for Review", state.queuedReview, false)

  // Footer, anchored to the bottom of the sidebar like `render_footer` /
  // `render_zen_row` in `sidebar.rs`: the dim `^P palette  q quit` keybind
  // line, then — only when the scenario opts into Zen Mode — a blank row and
  // the full-width green "ZEN MODE ON" band the TUI paints while Zen is on.
  // The band bypasses the 1-col horizontal margin so its green fill reaches
  // both sidebar edges, matching the crate. It's opt-in (default off) so it
  // shows only on the hero and Zen-describing docs, not every mockup.
  rows.push(SIDEBAR_BLANK_ROW)
  rows.push(
    padSidebarRow([
      { text: " " },
      { text: "^P palette  q quit", color: TUI_DARK_GRAY },
    ]),
  )
  if (state.zenMode) {
    rows.push(SIDEBAR_BLANK_ROW)
    rows.push(zenBandRow())
  }

  return rows
}

/**
 * The full-width "ZEN MODE ON" band, mirroring `render_zen_row` in
 * `sidebar.rs`: a single edge-to-edge green row (`bg(Rgb(0,127,0))`) with
 * white-bold text, the label left-aligned behind one leading space and the
 * rest of the row filled to the sidebar edge. One green segment so the fill
 * is continuous across the whole width.
 */
function zenBandRow(): Segment[] {
  const label = " ZEN MODE ON"
  const pad = Math.max(0, SIDEBAR_W - [...label].length)
  return [
    {
      text: label + " ".repeat(pad),
      color: SEL_FG,
      bg: TUI_ZEN_GREEN,
      bold: true,
    },
  ]
}

/**
 * The two rows for one review entry, mirroring `Row::Review` in `sidebar.rs`.
 * Line 1: the decoration badge + title, with the `machine:port` served URL
 * right-aligned when the task is Ready (loaded on a review worktree). Line 2:
 * the branch, dim. Ready uses a cyan `✓` and carries a location; Queued uses a
 * blue `·` and has none yet. Titles/branches truncate the same way the TUI
 * clips them against the pane width, so alignment stays character-perfect.
 */
function reviewEntryRows(entry: ReviewEntry, ready: boolean): Segment[][] {
  const glyph = ready ? "✓" : "·"
  const glyphColor = ready ? TUI_CYAN : TUI_BLUE
  const location = ready ? entry.location : undefined

  // Reserve columns for the 1-col sidebar pad, the 2-col badge, a 1-col gap,
  // the 1-col trailing margin, and the right-aligned location, so the title
  // truncates instead of forcing `sidebarRightAlignRow` to drop the URL.
  const titleWidth = location
    ? SIDEBAR_W - 5 - [...location].length
    : SIDEBAR_W - 3
  const left: Segment[] = [
    { text: " " },
    { text: `${glyph} `, color: glyphColor },
    { text: truncate(entry.title, Math.max(1, titleWidth)), color: TUI_GRAY },
  ]
  const line1 = location
    ? sidebarRightAlignRow(left, location, TUI_DARK_GRAY)
    : padSidebarRow(left)

  // Branch under the title, indented past the badge column, dim in both
  // states — clipped to the sidebar width like the TUI does.
  const line2 = padSidebarRow([
    { text: " " },
    { text: `  ${truncate(entry.branch, SIDEBAR_W - 3)}`, color: TUI_DARK_GRAY },
  ])
  return [line1, line2]
}

// ── Panels ────────────────────────────────────────────────────────────

/** The `<span>` runs for one row's segments — shared by `Row` and `NavRow`. */
function SegSpans({ segs }: { segs: Segment[] }) {
  return (
    <>
      {segs.map((seg, j) => {
        const style: React.CSSProperties = {}
        if (seg.color) style.color = seg.color
        if (seg.bg) style.background = seg.bg
        if (seg.bold) style.fontWeight = 700
        return (
          <span key={j} style={style}>
            {seg.text}
          </span>
        )
      })}
    </>
  )
}

function Row({ segs }: { segs: Segment[] }) {
  return (
    <>
      <SegSpans segs={segs} />
      {"\n"}
    </>
  )
}

/**
 * An interactive sidebar nav row. Renders through the same segment spans as a
 * static `Row` (so the monospace grid is untouched) but wraps them in a
 * focusable `role="button"` span: click or Enter/Space selects the view, and
 * hover/focus paints the `SIDEBAR_SEL_BG` fill as a discoverability affordance.
 * The trailing newline stays outside the button so the button's box hugs the
 * row content.
 */
function NavRow({
  nav,
  selected,
  onSelect,
}: {
  nav: NavItem
  selected: boolean
  onSelect: (view: NavView) => void
}) {
  const [hover, setHover] = useState(false)
  const segs = navRowSegs(nav.glyph, nav.label, selected, selected || hover)
  return (
    <>
      <span
        role="button"
        tabIndex={0}
        aria-label={`${nav.label} view`}
        aria-pressed={selected}
        style={{ cursor: "pointer" }}
        onClick={() => onSelect(nav.view)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault()
            onSelect(nav.view)
          }
        }}
        onMouseEnter={() => setHover(true)}
        onMouseLeave={() => setHover(false)}
        onFocus={() => setHover(true)}
        onBlur={() => setHover(false)}
      >
        <SegSpans segs={segs} />
      </span>
      {"\n"}
    </>
  )
}

const PRE_STYLE: React.CSSProperties = {
  background: TUI_BG,
  color: TUI_FG,
  fontSize: 13,
  // Line-height is kept tight so consecutive highlighted rows' backgrounds
  // connect into one continuous fill. An inline span's background only paints
  // the font's content box, and any leading above/below it is left unfilled —
  // at a looser line-height that unfilled leading shows as a gap between a
  // selection's stacked rows (the two-line focused card, the sidebar nav /
  // review selections). Geist Mono's content box is (ascent 1005 + descent
  // 295) / 1000 em ≈ 16.9px at this 13px size, so 17px makes adjacent rows'
  // backgrounds overlap ~0.1px — connected past sub-pixel rounding — while
  // line-height ~1.31 keeps the text readable and the character grid uncramped.
  lineHeight: "17px",
  minWidth: "max-content",
}

function TerminalBody({ state }: { state: AppState }) {
  // The right pane swaps on the active view. Every view is padded to a common
  // target height so the terminal frame stays a fixed size when switching views
  // (and, for the hero animation, across every beat as content grows/shrinks):
  // the natural board height, floored by any `minBodyRows` pin.
  const width = contentWidth(state.columns)
  const boardRows = buildBoardRows(state.columns)
  const target = Math.max(boardRows.length, state.minBodyRows ?? 0)
  const raw: Segment[][] =
    state.activeView === "activity"
      ? buildActivityRows(state)
      : state.activeView === "chat"
        ? buildChatRows(state, state.chatLines ?? CHAT_LINES)
        : state.activeView === "workspace"
          ? buildChatRows(state, state.workspaceLines ?? CHAT_LINES)
          : boardRows
  const body = padBodyTo(raw, target, width)
  const rows: Segment[][] = [
    titleRow(state),
    blankRow(width),
    ...body,
    blankRow(width),
    footerRow(state.activeView, width),
  ]

  return (
    <pre
      className="m-0 whitespace-pre font-mono"
      style={PRE_STYLE}
    >
      {rows.map((row, i) => (
        <Row key={i} segs={row} />
      ))}
    </pre>
  )
}

function Sidebar({
  state,
  onSelectView,
}: {
  state: AppState
  onSelectView: (view: NavView) => void
}) {
  const rows = buildSidebarRows(state)
  return (
    <pre
      className="m-0 hidden whitespace-pre border-r font-mono md:block"
      style={{
        ...PRE_STYLE,
        // CHROME_BAR_BG/BORDER sit too close to TUI_BG to read as a rule;
        // a mid-gray in the TUI gray family gives a distinct-but-tasteful
        // divider against the dark terminal body.
        borderColor: TUI_DIVIDER,
      }}
    >
      {rows.map((row, i) =>
        Array.isArray(row) ? (
          <Row key={i} segs={row} />
        ) : (
          <NavRow key={i} nav={row.nav} selected={row.selected} onSelect={onSelectView} />
        ),
      )}
    </pre>
  )
}

function TrafficLight({ color }: { color: string }) {
  // Real macOS traffic lights are ~12px diameter with ~8px spacing at
  // 1× scale; sized inline so the site's 8px-based Tailwind spacing
  // scale can't push these to 24px+ like the old `h-3 w-3` did.
  return (
    <span
      aria-hidden="true"
      className="inline-block rounded-full"
      style={{
        width: 12,
        height: 12,
        background: color,
        boxShadow: "inset 0 0 0 0.5px rgba(0,0,0,0.25)",
      }}
    />
  )
}

/**
 * Render a Shelbi TUI dashboard scenario inside a macOS-Terminal frame.
 * The look is fixed — only the scenario varies. Three ways to drive it:
 *
 *   <AppMockup state={defaultAppState} />              — explicit full state
 *   <AppMockup preset="starter" />                     — a named preset, import-free
 *   <AppMockup preset="starter" activeView="chat" />   — preset + inline tweaks
 *
 * `state` wins when given; otherwise the named `preset` (default:
 * "default") is the base. Any extra `AppState` fields passed alongside are
 * shallow-merged on top — the ergonomic path for MDX docs, which can't
 * import the preset objects but can drop a tag and override a field.
 */
export function AppMockup({
  state,
  preset = "default",
  frameOpacity,
  ...overrides
}: {
  state?: AppState
  preset?: PresetName
  /**
   * Opacity applied to the terminal frame, with a short transition — the hero
   * animation drives this to fade the whole frame out and back in at the loop
   * boundary. Omitted (the docs default) leaves the frame fully opaque.
   */
  frameOpacity?: number
} & Partial<AppState>) {
  const base = state ?? PRESETS[preset]
  const merged: AppState = { ...base, ...overrides }
  // The active view is live, interactive state — seeded from the scenario so
  // first paint matches the preset exactly (the hero stays on Tasks), then
  // driven by clicking the sidebar nav. It also re-syncs when the scenario's
  // own `activeView` changes, so a timeline that mutates the state over time
  // (the hero animation) drives the pane too. Everything else stays data.
  // The sync uses React's "adjust state while rendering" pattern (compare the
  // scenario's view to the last one we saw, reset on change) rather than an
  // effect, so a scenario change is reflected in the same render with no extra
  // commit — and a within-scenario nav click still wins until the next change.
  const [activeView, setActiveView] = useState<NavView>(merged.activeView)
  const [seenView, setSeenView] = useState<NavView>(merged.activeView)
  if (merged.activeView !== seenView) {
    setSeenView(merged.activeView)
    setActiveView(merged.activeView)
  }
  const resolved: AppState = { ...merged, activeView }
  return (
    <section className="border-b border-gray-4 px-3 py-6 sm:py-10">
      {/* w-fit hugs the board's natural width; max-w-full keeps the
          inner overflow-x-auto in charge on viewports narrower than
          the board. */}
      <div className="mx-auto w-fit max-w-full">
        <div
          className="overflow-hidden rounded-lg shadow-2xl"
          style={{
            boxShadow: "0 24px 48px rgba(0,0,0,0.35), 0 0 0 1px rgba(0,0,0,0.4)",
            opacity: frameOpacity,
            transition: frameOpacity === undefined ? undefined : "opacity 500ms ease",
          }}
        >
          {/* macOS Terminal title bar — traffic lights left, title centered. */}
          <div
            className="relative flex items-center justify-center border-b px-3"
            style={{
              background: CHROME_BAR_BG,
              borderColor: CHROME_BAR_BORDER,
              height: 28,
            }}
          >
            <div
              className="absolute flex items-center"
              // Equal top/left inset so the red light is equidistant from the
              // top and left edges — tucked into the corner. 8px keeps it
              // inside the 28px bar (8 + 12 + 8 = 28) with no height change.
              style={{ top: 8, left: 8, gap: 8 }}
            >
              <TrafficLight color={TRAFFIC_RED} />
              <TrafficLight color={TRAFFIC_YELLOW} />
              <TrafficLight color={TRAFFIC_GREEN} />
            </div>
            <span
              className="font-mono text-xs font-medium"
              style={{ color: CHROME_TITLE }}
            >
              {resolved.terminalTitle}
            </span>
          </div>

          {/* Terminal body — two `<pre>` blocks (sidebar + board) sit
              side-by-side inside one dark surface, so the whole panel
              reads as a single captured terminal frame. The board `<pre>`
              overflows horizontally on narrow viewports; the sidebar
              hides below `md` so phones don't get a cramped strip beside
              a cropped board. */}
          <div
            className="flex overflow-x-auto"
            style={{ background: TUI_BG }}
          >
            <Sidebar state={resolved} onSelectView={setActiveView} />
            <TerminalBody state={resolved} />
          </div>
        </div>
      </div>
    </section>
  )
}

// ── Presets ───────────────────────────────────────────────────────────
// Named starting points a doc page (or the landing page) imports and
// tweaks with a spread, so each scenario is a diff from a full board
// rather than a hand-built one. Add more as docs need them.

/**
 * The full five-column dashboard the marketing landing page has always
 * shown: a busy board with a selected review card, two machines of
 * workspaces, and both review sections populated — tasks Ready for Review
 * (served on a review workspace) and Queued for Review (waiting for a free
 * slot). `KanbanMockup` renders this verbatim.
 */
export const defaultAppState: AppState = {
  terminalTitle: "jlong@hub — my-project",
  project: "my-project",
  activeView: "tasks",
  // The hero showcases a busy, autonomous system — Zen Mode on, so the
  // full-width green band is part of the landing-page capture.
  zenMode: true,
  columns: [
    {
      label: "BACKLOG",
      category: "gray",
      cards: [
        // The queued columns (BACKLOG / TO DO) run deep on purpose — a
        // full backlog is what a working system looks like, and the
        // tallest column sets the window height, giving the framed
        // terminal its ~1.6:1 aspect ratio instead of a flat strip.
        { title: "Rework onboarding UX", id: "t-004" },
        { title: "Audit OSS licenses", id: "t-005" },
        { title: "Draft Q3 roadmap", id: "t-006" },
        { title: "Migrate CI to arm64", id: "t-014" },
        { title: "Sunset legacy v1 API", id: "t-020" },
        { title: "Add SSO for admins", id: "t-028" },
        { title: "Prune stale flags", id: "t-030" },
        { title: "Dedupe error reports", id: "t-031" },
        { title: "Archive S3 buckets", id: "t-033" },
        { title: "Refresh brand assets", id: "t-036" },
      ],
    },
    {
      label: "TO DO",
      category: "blue",
      cards: [
        { title: "Add API ratelimit", id: "t-007" },
        { title: "Fix mobile nav", id: "t-008" },
        { title: "Wire webhook retries", id: "t-016" },
        { title: "Split OTel spans", id: "t-021" },
        { title: "Sync i18n strings", id: "t-026" },
        { title: "Paginate search API", id: "t-038" },
        { title: "Cache user sessions", id: "t-040" },
        { title: "Add health probes", id: "t-042" },
        { title: "Debounce autosave", id: "t-045" },
      ],
    },
    {
      label: "IN PROGRESS",
      category: "yellow",
      cards: [
        { title: "Deploy staging env", id: "t-009", workspace: "alpha" },
        { title: "Wire up OAuth flow", id: "t-010", workspace: "bravo" },
        { title: "Backfill order index", id: "t-017", workspace: "echo" },
        { title: "Trim vendor bundle", id: "t-022", workspace: "foxtrot" },
      ],
    },
    {
      label: "REVIEW",
      category: "magenta",
      cards: [
        // Served on a review workspace (→ Ready for Review in the sidebar).
        { title: "Cold-start cache", id: "t-011", workspace: "charlie", selected: true },
        { title: "CSV import fix", id: "t-018", workspace: "golf" },
        { title: "Nightly report", id: "t-024", workspace: "hotel" },
        // No review workspace free yet (→ Queued for Review in the sidebar).
        { title: "Validate webhook payloads", id: "t-023" },
        { title: "Harden token refresh", id: "t-025" },
      ],
    },
    {
      label: "DONE",
      category: "green",
      cards: [
        { title: "Migrate to PG 16", id: "t-012" },
        { title: "Ship dark mode", id: "t-013" },
        { title: "Retry dead-letters", id: "t-019" },
        { title: "Redis cache /profile", id: "t-027" },
        { title: "Add audit logging", id: "t-046" },
        { title: "Fix flaky CI tests", id: "t-048" },
      ],
    },
  ],
  // Sidebar mirrors the board: every workspace shown on an IN PROGRESS or
  // REVIEW card is `working` here (in-progress → Developer, review →
  // Reviewer), leaving only two idle workspaces so the system reads busy.
  machines: [
    {
      name: "hub",
      workspaces: [
        { name: "alpha", state: "working", agent: "Developer" },
        { name: "bravo", state: "working", agent: "Developer" },
        { name: "charlie", state: "working", agent: "Reviewer" },
        { name: "delta", state: "idle" },
      ],
    },
    {
      name: "devbox",
      workspaces: [
        { name: "echo", state: "working", agent: "Developer" },
        { name: "foxtrot", state: "working", agent: "Developer" },
        { name: "golf", state: "working", agent: "Reviewer" },
        { name: "hotel", state: "working", agent: "Reviewer" },
        { name: "india", state: "idle" },
      ],
    },
  ],
  // Ready = the three served REVIEW cards, each loaded on a review workspace
  // and serving at its `machine:port`. Queued = the two Review-status cards
  // still waiting, because all three review workspaces are busy.
  readyReview: [
    { title: "Cold-start cache", branch: "shelbi/cold-start-cache", location: "hub:3000" },
    { title: "CSV import fix", branch: "shelbi/csv-import-fix", location: "devbox:3000" },
    { title: "Nightly report", branch: "shelbi/nightly-report", location: "devbox:3001" },
  ],
  queuedReview: [
    { title: "Validate webhook payloads", branch: "shelbi/validate-webhook-payloads" },
    { title: "Harden token refresh", branch: "shelbi/harden-token-refresh" },
  ],
}

/**
 * A freshly-initialized project: one machine, one idle workspace, a couple
 * of backlog cards and nothing else moving yet — the shape a
 * getting-started doc wants when it walks through the first task. Spread
 * and tweak it (`{ ...starterAppState, activeView: "chat" }`) to show a
 * specific early-onboarding state.
 */
export const starterAppState: AppState = {
  terminalTitle: "you@laptop — myproject",
  project: "myproject",
  activeView: "tasks",
  columns: [
    {
      label: "BACKLOG",
      category: "gray",
      cards: [
        { title: "Add a health check", id: "t-001" },
        { title: "Write the README", id: "t-002" },
      ],
    },
    { label: "TO DO", category: "blue", cards: [] },
    { label: "IN PROGRESS", category: "yellow", cards: [] },
    { label: "REVIEW", category: "magenta", cards: [] },
    { label: "DONE", category: "green", cards: [] },
  ],
  machines: [
    {
      name: "local",
      workspaces: [{ name: "alpha", state: "idle" }],
    },
  ],
  readyReview: [],
  queuedReview: [],
}

/** Preset scenarios addressable by name from `<AppMockup preset="…" />`. */
export const PRESETS = {
  default: defaultAppState,
  starter: starterAppState,
} satisfies Record<string, AppState>

/** A key of {@link PRESETS} — the `preset` prop's accepted values. */
export type PresetName = keyof typeof PRESETS

/**
 * Thin preset used by the marketing landing page. Renders `AppMockup` with
 * the original hardcoded scenario so the page stays visually identical.
 */
export function KanbanMockup() {
  return <AppMockup state={defaultAppState} />
}
