/**
 * A macOS-Terminal-window frame around an ASCII-art capture of the Shelbi
 * TUI's kanban view. The frame (traffic lights, rounded corners, drop
 * shadow, title bar) is DOM; the board body inside is a single `<pre>`
 * with character-perfect alignment — the point is that it reads as text
 * captured from a real terminal, not a re-implementation of the TUI in
 * HTML.
 *
 * Colors match what `crates/shelbi-tui/src/kanban.rs` emits via ratatui's
 * named `Color::Gray | Blue | Yellow | Magenta | Green | DarkGray`
 * palette, resolved to the hex values a modern dark terminal (macOS
 * Terminal / iTerm2 defaults) renders them as. Column-header hue is
 * driven by `StatusCategory` in the crate — Backlog=Gray, Ready=Blue,
 * Active=Yellow, Handoff=Magenta, Done=Green — the same rule used in
 * `category_color()` (crates/shelbi-tui/src/kanban.rs:2230).
 *
 * The mockup stays static — no interaction — and on small viewports the
 * body overflows horizontally under a fixed terminal frame, mirroring
 * the horizontal-scroll fallback PR #110 shipped for the DOM version.
 */

// ── Palette ───────────────────────────────────────────────────────────
// Terminal chrome (window frame) and terminal-body ANSI equivalents.
// Chosen to match how ratatui's named colors render in macOS Terminal
// with a Solarized-adjacent dark scheme — the setup a shelbi user is
// most likely to see when they run the TUI locally.
const CHROME_BAR_BG = "#2d2d2d"
const CHROME_BAR_BORDER = "#1a1a1a"
const CHROME_TITLE = "#c7c7c7"
const TRAFFIC_RED = "#ff5f57"
const TRAFFIC_YELLOW = "#febc2e"
const TRAFFIC_GREEN = "#28c840"

const TUI_BG = "#1e1e1e"
const TUI_FG = "#e5e5e5"
const TUI_GRAY = "#b8b8b8" // ANSI 7 — column header for `Backlog`
const TUI_DARK_GRAY = "#7c7c7c" // ANSI 8 — chrome text (`Tasks · `, ids, footer)
const TUI_BLUE = "#4a8fd7" // ANSI 4 — `Ready` category (todo)
const TUI_YELLOW = "#dcb767" // ANSI 3 — `Active` category (in_progress)
const TUI_MAGENTA = "#c586c0" // ANSI 5 — `Handoff` (review) + `@workspace`
const TUI_GREEN = "#5fb56d" // ANSI 2 — `Done` category
const TUI_CYAN = "#4ec9b0" // ANSI 6 — project name in title bar
const SEL_BG = "#264f78" // ratatui `bg(Blue)` for the focused card
const SEL_FG = "#ffffff" // ratatui `fg(White)` for the focused card

// ── Layout constants ─────────────────────────────────────────────────
// Column widths are fixed at 22 monospace cells (20 for card text + a
// 2-cell right gutter) — the same shape the TUI uses at 5-column
// widths. Total board width = 1 leading pad + 5 × 22 = 111 cells.
const COL_W = 22
const TEXT_W = 20

type Card = {
  title: string
  id: string
  workspace?: string
  selected?: boolean
}

type Category = "gray" | "blue" | "yellow" | "magenta" | "green"

type Column = {
  label: string
  category: Category
  cards: Card[]
}

const CATEGORY_COLOR: Record<Category, string> = {
  gray: TUI_GRAY,
  blue: TUI_BLUE,
  yellow: TUI_YELLOW,
  magenta: TUI_MAGENTA,
  green: TUI_GREEN,
}

const COLUMNS: Column[] = [
  {
    label: "BACKLOG",
    category: "gray",
    cards: [
      { title: "Rework onboarding copy", id: "t-004" },
      { title: "Audit third-party licenses", id: "t-005" },
      { title: "Draft Q3 roadmap", id: "t-006" },
    ],
  },
  {
    label: "TO DO",
    category: "blue",
    cards: [
      { title: "Add ratelimit to API", id: "t-007" },
      { title: "Fix mobile nav overlap", id: "t-008" },
    ],
  },
  {
    label: "IN PROGRESS",
    category: "yellow",
    cards: [
      { title: "Deploy staging env", id: "t-009", workspace: "alpha" },
      { title: "Wire up OAuth flow", id: "t-010", workspace: "bravo" },
    ],
  },
  {
    label: "REVIEW",
    category: "magenta",
    cards: [
      {
        title: "Cache warm-up on cold start",
        id: "t-011",
        workspace: "charlie",
        selected: true,
      },
    ],
  },
  {
    label: "DONE",
    category: "green",
    cards: [
      { title: "Migrate to Postgres 16", id: "t-012" },
      { title: "Ship dark-mode toggle", id: "t-013" },
    ],
  },
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

const BLANK_ROW: Segment[] = [{ text: " ".repeat(1 + 5 * COL_W) }]

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
      // workspace both render as white-on-blue instead of their normal
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
function buildBoardRows(): Segment[][] {
  const perCol = COLUMNS.map(columnRows)
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

/** Title bar row: "Tasks · shelbi  N total" + right-aligned chips. */
function titleRow(): Segment[] {
  const total = COLUMNS.reduce((n, c) => n + c.cards.length, 0)
  const leftText = ` Tasks · shelbi   ${total} total`
  const workflow = "Workflow: All ▾"
  const workspace = "Workspace: All ▾ "
  const width = 1 + 5 * COL_W
  const leftLen = [...leftText].length
  const rightLen = [...workflow].length + 2 + [...workspace].length
  const midPad = Math.max(1, width - leftLen - rightLen)
  return [
    { text: " Tasks · ", color: TUI_DARK_GRAY },
    { text: "shelbi", color: TUI_CYAN, bold: true },
    { text: `   ${total} total`, color: TUI_DARK_GRAY },
    { text: " ".repeat(midPad) },
    { text: workflow, color: TUI_DARK_GRAY },
    { text: "  " },
    { text: workspace, color: TUI_DARK_GRAY },
  ]
}

/** Footer keybinding hints — matches `render_footer` in the crate. */
function footerRow(): Segment[] {
  const width = 1 + 5 * COL_W
  // Match the real footer's key-in-fg / hint-in-dg pattern.
  const segs: Segment[] = []
  const push = (t: string, color = TUI_DARK_GRAY) => segs.push({ text: t, color })
  const key = (t: string) => push(t, TUI_FG)
  push("  ")
  key("h/l")
  push(" col   ")
  key("j/k")
  push(" row   ")
  key("⏎")
  push(" open   ")
  key("n")
  push(" new   ")
  key("f")
  push(" filter   ")
  key("r")
  push(" refresh")
  const used = segs.reduce((acc, s) => acc + [...s.text].length, 0)
  if (used < width) segs.push({ text: " ".repeat(width - used) })
  return segs
}

function TerminalBody() {
  const rows: Segment[][] = [
    titleRow(),
    BLANK_ROW,
    ...buildBoardRows(),
    BLANK_ROW,
    footerRow(),
  ]

  return (
    <pre
      className="m-0 whitespace-pre px-4 py-3 font-mono"
      style={{
        background: TUI_BG,
        color: TUI_FG,
        fontSize: 12,
        lineHeight: "18px",
        minWidth: "max-content",
      }}
    >
      {rows.map((row, i) => (
        <span key={i}>
          {row.map((seg, j) => {
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
          {"\n"}
        </span>
      ))}
    </pre>
  )
}

function TrafficLight({ color }: { color: string }) {
  return (
    <span
      aria-hidden="true"
      className="inline-block h-3 w-3 rounded-full"
      style={{
        background: color,
        boxShadow: "inset 0 0 0 0.5px rgba(0,0,0,0.25)",
      }}
    />
  )
}

export function KanbanMockup() {
  return (
    <section className="border-b border-gray-4 px-3 py-6 sm:py-10">
      <div className="mx-auto w-full max-w-5xl">
        <div
          className="overflow-hidden rounded-lg shadow-2xl"
          style={{ boxShadow: "0 24px 48px rgba(0,0,0,0.35), 0 0 0 1px rgba(0,0,0,0.4)" }}
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
            <div className="absolute left-3 top-1/2 flex -translate-y-1/2 items-center gap-1.5">
              <TrafficLight color={TRAFFIC_RED} />
              <TrafficLight color={TRAFFIC_YELLOW} />
              <TrafficLight color={TRAFFIC_GREEN} />
            </div>
            <span
              className="font-mono text-xs font-medium"
              style={{ color: CHROME_TITLE }}
            >
              jlong@hub — shelbi
            </span>
          </div>

          {/* Terminal body — a real `<pre>` so the ASCII art reads as
              captured text and every cell aligns to the monospace grid.
              Horizontal overflow scrolls on narrow viewports; the frame
              stays put. */}
          <div className="overflow-x-auto" style={{ background: TUI_BG }}>
            <TerminalBody />
          </div>
        </div>
      </div>
    </section>
  )
}
