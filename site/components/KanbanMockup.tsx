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
 * The mockup stays static — no interaction — and on small viewports the
 * sidebar hides so the board doesn't force horizontal scroll on phones.
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
const TUI_GRAY = "#b8b8b8" // ANSI 7 — column header for `Backlog`, nav labels
const TUI_DARK_GRAY = "#7c7c7c" // ANSI 8 — chrome text (`Tasks · `, ids, footer)
const TUI_BLUE = "#4a8fd7" // ANSI 4 — `Ready` category (todo)
const TUI_YELLOW = "#dcb767" // ANSI 3 — `Active` category (in_progress)
const TUI_MAGENTA = "#c586c0" // ANSI 5 — `Handoff` (review) + `@workspace`
const TUI_GREEN = "#5fb56d" // ANSI 2 — `Done` category + Working badge
const TUI_CYAN = "#4ec9b0" // ANSI 6 — project name in title bar + ✓ review badge
const SEL_BG = "#264f78" // ratatui `bg(Blue)` for a focused card
const SEL_FG = "#ffffff" // ratatui `fg(White)` for a focused card
// Sidebar's per-row selection fill: `Color::Rgb(63,63,63)` in
// `sidebar.rs::render_list` — a dark gray band, softer than the kanban
// card's blue highlight.
const SIDEBAR_SEL_BG = "#3f3f3f"

// ── Layout constants ─────────────────────────────────────────────────
// Kanban columns are fixed at 22 monospace cells (20 for card text + a
// 2-cell right gutter) — the same shape the TUI uses at 5-column
// widths. Total board width = 1 leading pad + 5 × 22 = 111 cells.
// Sidebar content width is 28 (matches the TUI's 30-col sidebar minus
// its 1-col horizontal padding on each side).
const COL_W = 22
const TEXT_W = 20
const SIDEBAR_W = 30
// Board width assumes a fixed 5-column layout — the same shape the TUI
// draws at dashboard widths. `titleRow`/`footerRow` pad to this.
const BOARD_W = 1 + 5 * COL_W

// ── Scenario model ────────────────────────────────────────────────────
// The public `AppState` shape. Every field is pure data — an MDX doc
// author declares (or spreads a preset and tweaks) one of these to show a
// specific interface state, and `AppMockup` renders it through the fixed
// engine below.

export type Category = "gray" | "blue" | "yellow" | "magenta" | "green"

/** One kanban card. `selected` draws the focused-card blue highlight. */
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

/** One entry in the sidebar's "Ready for Review" section. */
export type ReviewTask = {
  title: string
  workspace: string
}

/** Which sidebar nav row is active — also drives the title-bar label. */
export type NavView = "chat" | "tasks" | "activity"

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
  /** Sidebar "Ready for Review" entries; empty hides nothing but the rows. */
  review: ReviewTask[]
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

const BOARD_BLANK_ROW: Segment[] = [{ text: " ".repeat(BOARD_W) }]
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
function titleRow(columns: Column[], project: string, activeView: NavView): Segment[] {
  const total = columns.reduce((n, c) => n + c.cards.length, 0)
  const label = NAV_ITEMS.find((n) => n.view === activeView)?.label ?? "Tasks"
  const leftText = ` ${label} · ${project}   ${total} total`
  const workflow = "Workflow: All ▾"
  const workspace = "Workspace: All ▾ "
  const leftLen = [...leftText].length
  const rightLen = [...workflow].length + 2 + [...workspace].length
  const midPad = Math.max(1, BOARD_W - leftLen - rightLen)
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

/** Footer keybinding hints — matches `render_footer` in the crate. */
function footerRow(): Segment[] {
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
  if (used < BOARD_W) segs.push({ text: " ".repeat(BOARD_W - used) })
  return segs
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

function buildSidebarRows(state: AppState): Segment[][] {
  const rows: Segment[][] = []

  // Project header — same Cyan Bold `app.project_name` renders at.
  rows.push(
    padSidebarRow([
      { text: " " },
      { text: state.project, color: TUI_CYAN, bold: true },
    ]),
  )
  rows.push(SIDEBAR_BLANK_ROW)

  // Nav rows — 💬 Chat / 📋 Tasks / ⚡ Activity. The active view's row
  // gets the full-row dark-gray fill + white-bold text
  // `sidebar.rs::render_list` applies to the selected row.
  const navRow = (glyph: string, label: string, selected = false): Segment[] => {
    const inner: Segment[] = [
      { text: " ", bg: selected ? SIDEBAR_SEL_BG : undefined },
      {
        text: `${glyph} ${label}`,
        color: selected ? SEL_FG : TUI_GRAY,
        bg: selected ? SIDEBAR_SEL_BG : undefined,
        bold: selected,
      },
    ]
    const used = inner.reduce((acc, s) => acc + [...s.text].length, 0)
    if (used < SIDEBAR_W) {
      inner.push({
        text: " ".repeat(SIDEBAR_W - used),
        bg: selected ? SIDEBAR_SEL_BG : undefined,
      })
    }
    return inner
  }
  for (const nav of NAV_ITEMS) {
    rows.push(navRow(nav.glyph, nav.label, nav.view === state.activeView))
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

  // — Ready for Review — section: one row per entry with Cyan ✓, title,
  // and the workspace right-aligned in DarkGray — same shape `Row::Review`
  // renders via `right_align`. Omitted entirely when nothing is ready.
  if (state.review.length > 0) {
    rows.push(SIDEBAR_BLANK_ROW)
    rows.push(
      padSidebarRow([
        { text: " " },
        { text: "— Ready for Review —", color: TUI_DARK_GRAY },
      ]),
    )
    for (const task of state.review) {
      rows.push(
        sidebarRightAlignRow(
          [
            { text: " " },
            { text: "✓ ", color: TUI_CYAN },
            {
              text: truncate(task.title, SIDEBAR_W - 4 - task.workspace.length),
              color: TUI_GRAY,
            },
          ],
          task.workspace,
          TUI_DARK_GRAY,
        ),
      )
    }
  }

  return rows
}

// ── Panels ────────────────────────────────────────────────────────────

function Row({ segs }: { segs: Segment[] }) {
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
      {"\n"}
    </>
  )
}

const PRE_STYLE: React.CSSProperties = {
  background: TUI_BG,
  color: TUI_FG,
  fontSize: 12,
  lineHeight: "18px",
  minWidth: "max-content",
}

function TerminalBody({ state }: { state: AppState }) {
  const rows: Segment[][] = [
    titleRow(state.columns, state.project, state.activeView),
    BOARD_BLANK_ROW,
    ...buildBoardRows(state.columns),
    BOARD_BLANK_ROW,
    footerRow(),
  ]

  return (
    <pre
      className="m-0 whitespace-pre px-4 py-3 font-mono"
      style={PRE_STYLE}
    >
      {rows.map((row, i) => (
        <Row key={i} segs={row} />
      ))}
    </pre>
  )
}

function Sidebar({ state }: { state: AppState }) {
  const rows = buildSidebarRows(state)
  return (
    <pre
      className="m-0 hidden whitespace-pre border-r py-3 font-mono md:block"
      style={{
        ...PRE_STYLE,
        // CHROME_BAR_BORDER is darker than the terminal body, so against
        // TUI_BG it disappears — the bar bg tone is the one that reads as
        // a line here.
        borderColor: CHROME_BAR_BG,
      }}
    >
      {rows.map((row, i) => (
        <Row key={i} segs={row} />
      ))}
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
  ...overrides
}: { state?: AppState; preset?: PresetName } & Partial<AppState>) {
  const base = state ?? PRESETS[preset]
  const resolved: AppState = { ...base, ...overrides }
  return (
    <section className="border-b border-gray-4 px-3 py-6 sm:py-10">
      {/* w-fit hugs the board's natural width; max-w-full keeps the
          inner overflow-x-auto in charge on viewports narrower than
          the board. */}
      <div className="mx-auto w-fit max-w-full">
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
            <div
              className="absolute left-3 top-1/2 flex -translate-y-1/2 items-center"
              style={{ gap: 8 }}
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
            <Sidebar state={resolved} />
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
 * workspaces, and one ready-for-review entry. `KanbanMockup` renders this
 * verbatim, so the landing page is unchanged.
 */
export const defaultAppState: AppState = {
  terminalTitle: "jlong@hub — shelbi",
  project: "shelbi",
  activeView: "tasks",
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
        {
          title: "Cache warm-up on cold start",
          id: "t-011",
          workspace: "charlie",
          selected: true,
        },
        { title: "CSV idempotency", id: "t-018", workspace: "golf" },
        { title: "Nightly report fix", id: "t-024", workspace: "hotel" },
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
  review: [
    { title: "Cache warm-up on cold start", workspace: "charlie" },
    { title: "CSV idempotency", workspace: "golf" },
    { title: "Nightly report fix", workspace: "hotel" },
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
  review: [],
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
