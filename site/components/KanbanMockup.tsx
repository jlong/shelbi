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
 * `defaultAppState`.
 *
 * Colors match what the TUI emits via ratatui's named
 * `Color::Gray | DarkGray | Blue | Yellow | Magenta | Green | Cyan`
 * palette, resolved to the hex values a modern dark terminal (macOS
 * Terminal / iTerm2 defaults) renders them as. Kanban column-header hue
 * is driven by `StatusCategory` in the crate — Backlog=Gray, Ready=Blue,
 * Active=Yellow, Handoff=Magenta, Done=Green, Archived=DarkGray
 * (`category_color()` in `crates/shelbi-tui/src/kanban.rs`). The sidebar
 * mirrors `crates/shelbi-tui/src/shell/sidebar.rs` over the shared
 * `SidebarModel` in `crates/shelbi-app/src/view.rs` (nav items, workspace
 * badges, review rows), so this mockup can't drift from the running TUI.
 *
 * The sidebar nav is interactive: clicking (or Enter/Space on) a nav row
 * swaps the right pane between the Orchestrator session, the Issues board,
 * and the Changelog, the TUI's three nav items (`default_nav_items`). The
 * Activity feed is reachable in the TUI only from the palette, so here it is
 * a scenario's `activeView` with no nav row. `AppMockup` owns the live
 * `activeView` in local state, seeded from the scenario so first paint matches
 * the preset exactly; every pane is built through the same Segment/Row
 * monospace engine as the board so they read as captures of the same
 * terminal, not an HTML re-implementation. On small viewports the sidebar
 * hides so the board doesn't force horizontal scroll on phones.
 */

// ── Palette ───────────────────────────────────────────────────────────
// Terminal chrome (window frame) and terminal-body ANSI equivalents. Every
// value below the traffic lights is a theme-aware CSS variable resolved in
// `site/app/globals.css`: the `.dark` block holds the original terminal hex
// (how ratatui's named colors render in a Solarized-adjacent dark macOS
// Terminal — the setup a Shelbi user is most likely to see locally), and the
// `:root` defaults hold a light-canvas variant (dark text on a light body,
// darkened/saturated accents that stay legible on white). Because these feed
// inline `color`/`background` styles, the mockups invert with the site's
// `.dark` class for free — dark stays the terminal aesthetic, light becomes a
// light-background capture. Keep the two blocks in globals.css in sync.
const CHROME_BAR_BG = "var(--tui-chrome-bar-bg)"
const CHROME_BAR_BORDER = "var(--tui-chrome-bar-border)"
const CHROME_TITLE = "var(--tui-chrome-title)"
// Traffic lights are the one exception — the macOS window-button dots read the
// same on a light or dark title bar, so they stay literal (not theme-driven).
const TRAFFIC_RED = "#ff5f57"
const TRAFFIC_YELLOW = "#febc2e"
const TRAFFIC_GREEN = "#28c840"

const TUI_BG = "var(--tui-bg)"
const TUI_FG = "var(--tui-fg)"
const TUI_GRAY = "var(--tui-gray)" // ANSI 7 — column header for `Backlog`, nav labels
const TUI_DARK_GRAY = "var(--tui-dark-gray)" // ANSI 8 — chrome text (`Tasks · `, ids, footer)
const TUI_DIVIDER = "var(--tui-divider)" // mid-gray rule — sidebar/board divider, reads on TUI_BG
const TUI_BLUE = "var(--tui-blue)" // ANSI 4 — `Ready` category (todo)
const TUI_YELLOW = "var(--tui-yellow)" // ANSI 3 — `Active` category (in_progress)
const TUI_MAGENTA = "var(--tui-magenta)" // ANSI 5 — `Handoff` (review) + `@workspace`
const TUI_GREEN = "var(--tui-green)" // ANSI 2 — `Done` category + Working badge
const TUI_CYAN = "var(--tui-cyan)" // ANSI 6 — project name in title bar + ✓ review badge
const TUI_LIGHT_RED = "var(--tui-light-red)" // ANSI 9 — foxtrot's avatar tint (`Color::LightRed` in activity.rs)
// Zen-mode band fill: the TUI paints the "ZEN MODE ON" footer bar with an
// explicit `bg(Color::Rgb(0,127,0))` + `fg(White)` bold (`render_zen_row`
// in `crates/shelbi-tui/src/sidebar.rs`). Dark keeps that green band; light
// uses a soft light-green fill so the dark band text (`SEL_FG`, now near-black
// on light) stays legible.
const TUI_ZEN_GREEN = "var(--tui-zen-green)"
// Focused-card highlight: the shared selection gray `theme::SELECTION_BG`
// (`Color::Rgb(63,63,63)`) the TUI paints every selection background with —
// nav, kanban card, review queue, and filter dropdowns all use this one
// value so they can't drift. Selected text keeps an explicit `fg(White)` +
// bold so it stays readable on the gray (near-black on the light fill).
const SEL_BG = "var(--tui-sel-bg)" // ratatui `bg(theme::SELECTION_BG)` for a focused card
const SEL_FG = "var(--tui-sel-fg)" // ratatui `fg(White)` for a focused card
// Sidebar's per-row selection fill uses the same `theme::SELECTION_BG` gray
// as the focused card above — one selection color across every surface.
const SIDEBAR_SEL_BG = SEL_BG
// A subtle fill just above the terminal body (`TUI_BG`) for the injected
// task-body callout in a worker pane — a step off the body so it reads as a
// quiet block rather than a highlight standing off the background.
const TASK_BOX_BG = "var(--tui-task-box-bg)"
// The small workflow-name badge painted on the meta line of each kanban card —
// a neutral gray chip matching the other gray fills in the terminal (the
// selection gray), with contrasting text.
const WORKFLOW_BADGE_BG = "var(--tui-workflow-badge-bg)" // neutral gray, same as SEL_BG
const WORKFLOW_BADGE_FG = "var(--tui-workflow-badge-fg)"
// The sidebar search box's fill, `theme.search` (#292929 dark / #ececec light),
// the same two values as the task-body callout fill above.
const SEARCH_BG = TASK_BOX_BG

/**
 * A half-cell fill: the `▄` / `▀` half-block bleed the TUI draws above and below
 * a filled row (the nav selection, the search box, the toolbar squares) so it
 * reads as half a row taller. Drawn as a two-stop gradient behind blank cells
 * rather than as the block glyphs, which the mono font lacks (its fallback
 * glyphs don't sit flush with the row). `top` / `bottom` are the fills of the
 * row's upper and lower halves (`undefined` = the terminal background).
 */
function halfFill(top: string | undefined, bottom: string | undefined): string | undefined {
  if (!top && !bottom) return undefined
  const t = top ?? "transparent"
  const b = bottom ?? "transparent"
  return `linear-gradient(to bottom, ${t} 50%, ${b} 50%)`
}

// ── Layout constants ─────────────────────────────────────────────────
// Kanban columns are at least 22 monospace cells (20 for card text + a
// 2-cell right gutter) — the same shape the TUI uses at 5-column
// widths. A full five-column board = 1 leading pad + 5 × 22 = 111 cells.
// The sidebar is 30 cells by default (the TUI's sidebar is user-resizable;
// its 1-col inset on each side leaves 28 for content).
const COL_W = 22
// A collapsed column folds down to this narrow strip (the label spelled
// vertically over the hidden-card count) so it sits as a thin gutter beside the
// expanded columns instead of a full column.
const COLLAPSED_W = 6
const SIDEBAR_W = 30
// The board width `AppMockup` lays a scenario out at: the canonical five-column
// width. Like the TUI, expanded columns share what the collapsed strips leave, so
// a board with folded columns still fills the pane instead of shrinking.
const DEFAULT_BOARD_W = 111
/** A single column's natural width in monospace cells — collapsed or full. */
function colWidth(col: Column): number {
  return col.collapsed ? COLLAPSED_W : COL_W
}
/**
 * Each column's rendered width for a scenario. Collapsed columns keep their
 * narrow strip; expanded ones take at least `COL_W` and share any width the
 * scenario's `boardWidth` leaves over (extra cells go to the leftmost columns),
 * the way the TUI splits its board area.
 */
function columnWidths(state: AppState): number[] {
  const natural = state.columns.map(colWidth)
  const naturalTotal = 1 + natural.reduce((a, b) => a + b, 0)
  const target = Math.max(naturalTotal, state.boardWidth ?? 0)
  const expanded = state.columns.filter((c) => !c.collapsed).length
  if (target === naturalTotal || expanded === 0) return natural
  const collapsed = state.columns.length - expanded
  const spare = target - 1 - collapsed * COLLAPSED_W
  const each = Math.floor(spare / expanded)
  let extra = spare - each * expanded
  return state.columns.map((c) => {
    if (c.collapsed) return COLLAPSED_W
    const w = each + (extra > 0 ? 1 : 0)
    if (extra > 0) extra -= 1
    return w
  })
}
/**
 * The board content width for a scenario, in monospace cells: 1 leading pad plus
 * each column's width (see `columnWidths`). A full five-column board is
 * `1 + 5 * 22 = 111`, the canonical dashboard width the TUI draws. This is the
 * width every view within a single mockup pins to — the Orchestrator, Activity
 * and Changelog panes size to it so switching views never changes the frame
 * width.
 */
function contentWidth(state: AppState): number {
  return 1 + columnWidths(state).reduce((w, c) => w + c, 0)
}

/** The GitHub login branches are named after: the `user@` of the window title. */
export function branchUser(state: AppState): string {
  return /^([^@\s]+)@/.exec(state.terminalTitle)?.[1] ?? "you"
}

/** `developer` → `Developer`: the sidebar's agent label (`title_case` in view.rs). */
function titleCase(s: string): string {
  return s ? s[0].toUpperCase() + s.slice(1) : s
}

// ── Scenario model ────────────────────────────────────────────────────
// The public `AppState` shape. Every field is pure data — an MDX doc
// author declares (or spreads a preset and tweaks) one of these to show a
// specific interface state, and `AppMockup` renders it through the fixed
// engine below.

/**
 * A status category's header color: backlog=gray, ready=blue, active=yellow,
 * handoff=magenta, done=green, archived (Canceled, Declined…)=darkgray.
 */
export type Category = "gray" | "blue" | "yellow" | "magenta" | "green" | "darkgray"

/** One kanban card. `selected` draws the focused-card gray highlight. */
export type Card = {
  title: string
  id: string
  /** The workspace the card is assigned to (adds `o workspace` to the hints when selected). */
  workspace?: string
  selected?: boolean
  /**
   * The card's git branch. Omitted, a card in an active or handoff column shows
   * the shipped `{{github_user}}/{{id}}` branch (`<user>/<id>`, the user taken
   * from the window title); other columns show none.
   */
  branch?: string
  /**
   * The issue's first paragraph. The TUI board doesn't draw it; the desktop
   * board's cards show it under the title (see `components/desktop`).
   */
  summary?: string
}

/** One kanban column: a category-colored header over its stack of cards. */
export type Column = {
  label: string
  category: Category
  cards: Card[]
  /**
   * Fold the column down to a narrow strip — the category caret, the label
   * spelled vertically, and the hidden-card count — instead of a full column of
   * cards. Used to push a stage out of the way (e.g. a deep BACKLOG) so the eye
   * stays on the columns where work is actually moving. Its cards still count
   * toward the folded strip's total but aren't drawn.
   */
  collapsed?: boolean
}

/**
 * A dev workspace row in the sidebar. Review-tagged workspaces never appear
 * here: the TUI lists them only through the review sections.
 */
export type Workspace = {
  name: string
  state: "idle" | "working"
  /** The agent's name (`developer`, `qa`…); the row shows it title-cased. */
  agent?: string
}

/**
 * A machine and its workspaces. With more than one machine the sidebar groups
 * rows under `▾ <name>` headers; a single-machine project is a flat list.
 */
export type Machine = {
  name: string
  workspaces: Workspace[]
}

/**
 * One entry in a sidebar review section, mirroring `SidebarRow::Review` in
 * the crate. Rendered as two lines: line 1 is the badge + title (Ready adds
 * the `machine:workspace` review slot right-aligned, when the title leaves
 * room for it); line 2 is `⎇ <branch>`, dim. `location` is set only for a
 * Ready (serving) entry — a Queued one has no slot yet.
 */
export type ReviewEntry = {
  title: string
  /** Branch shown dim on line 2, e.g. `you/<id>`. */
  branch: string
  /** `machine:workspace` review slot, e.g. `hub:review-0` — Ready only. */
  location?: string
}

/**
 * Which right-pane view is active. `chat` (Orchestrator), `tasks` (Issues) and
 * `changelog` are the sidebar nav rows; `activity` is the Activity feed, which
 * the TUI opens from the palette (no nav row); `workspace` is the
 * focused-worker pane (a Claude-Code session for one workspace), reached by
 * selecting a workspace rather than a nav row.
 */
export type NavView = "chat" | "tasks" | "changelog" | "activity" | "workspace"

/**
 * One row in the ⌃P command palette: a left icon, a command label the fuzzy
 * filter matches the typed query against, and a dim right-aligned description.
 */
export type PaletteItem = {
  /** Left icon — an emoji action/nav glyph (💬 📋 ⚡) or the `·` workspace bullet. */
  glyph: string
  /** Command label; the fuzzy filter matches the typed query against this. */
  label: string
  /** Dim, right-aligned description of what the command does. */
  desc: string
}

/**
 * The ⌃P command-palette overlay state. When present on an `AppState`,
 * `AppMockup` draws the centered palette modal over the active view: a `>`
 * prompt showing `query`, the `items` fuzzy-filtered by it (a case-insensitive
 * label subsequence match), and the top surviving match highlighted. The hero
 * animation types `query` from empty up to `alpha` to narrow the list, then
 * clears the palette as the alpha workspace activates.
 */
export type PaletteState = {
  /** Text typed after the `>` prompt; fuzzy-filters `items` by label. */
  query: string
  /** The full command list; the renderer filters it by `query`. */
  items: PaletteItem[]
}

/**
 * The full interface state a single mockup renders. Pass one to
 * `<AppMockup state={...} />`; start from a preset (`defaultAppState`,
 * `starterAppState`) and override only what your scenario changes.
 */
export type AppState = {
  /**
   * macOS window-chrome title, e.g. `you@hub — myapp`. Its `user@` also names
   * the cards' default branches (`<user>/<id>`).
   */
  terminalTitle: string
  /** Project name — Cyan-bold in the sidebar header and the board title. */
  project: string
  /** The open view; the matching nav row (if it has one) is highlighted. */
  activeView: NavView
  /**
   * Board columns, one per status in `statuses.yaml` order (the shipped catalog
   * draws six: BACKLOG, TO DO, IN PROGRESS, REVIEW, DONE and a darkgray
   * CANCELED). Mark an empty column `collapsed`: the TUI folds empty columns to
   * a narrow strip.
   */
  columns: Column[]
  /**
   * Board width in cells (`AppMockup` defaults it to 111). Expanded columns
   * share what the collapsed strips leave; a board whose columns need more than
   * this is drawn at its natural width.
   */
  boardWidth?: number
  /**
   * Workflow name shown as the small badge on each card's meta line (` task `
   * on a shipped project). The TUI draws it only while more than one workflow
   * is loaded and the board isn't filtered to one; omit it to suppress the badge.
   */
  workflow?: string
  /**
   * The workflow the board is filtered to (`⇥` cycles it): the title's
   * `Workflow:` chip reads it in cyan and the cards drop their badge. Omitted,
   * the chip reads `All`.
   */
  workflowFilter?: string
  /**
   * The project loads a single workflow, so the board draws no `Workflow:`
   * chip (the TUI shows it only when there's more than one to filter by).
   */
  singleWorkflow?: boolean
  /** Sidebar machines and their dev workspaces. */
  machines: Machine[]
  /**
   * Sidebar "Ready for Review" entries — Review-status issues loaded on a
   * `review`-tagged workspace and serving (cyan ✓, with a `machine:workspace`
   * location). Empty hides the section.
   */
  readyReview: ReviewEntry[]
  /**
   * Sidebar "Queued for Review" entries — Review-status issues waiting for a
   * free review workspace (dim ·, no location yet). Empty hides the section.
   */
  queuedReview: ReviewEntry[]
  /** Sidebar width in cells (default 30). The TUI's sidebar is resizable. */
  sidebarWidth?: number
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
   * The text currently sitting in the Claude-Code input box at the bottom of the
   * Chat / worker pane. Real Claude Code pins the prompt to the bottom of the
   * pane with the conversation scrolling above it, so this is separate from the
   * transcript (`chatLines`/`workspaceLines`): the hero animation types beat 2's
   * message here character by character, then clears it on "send" as the message
   * moves up into the conversation. Empty (the default) renders an idle prompt.
   */
  chatInput?: string
  /**
   * Pins the terminal body to at least this many rows. Views shorter than the
   * pin are padded with blank rows so the frame height never changes as a
   * scenario's content grows or shrinks — the hero animation sets it once so
   * the frame stays a fixed size across every beat (cards appearing, columns
   * emptying, panes swapping) instead of resizing with the tallest column.
   */
  minBodyRows?: number
  /**
   * When set, the ⌃P command palette renders as a centered box-drawing frame over
   * the active view (the board stays put and undimmed underneath, the console
   * background bleeding around the frame). Omitted (the default) shows no palette.
   * Only the hero animation's ending beats set it — opening the palette, typing to
   * filter the list down to `alpha`, then clearing
   * it as the alpha workspace activates.
   */
  palette?: PaletteState
}

const CATEGORY_COLOR: Record<Category, string> = {
  gray: TUI_GRAY,
  blue: TUI_BLUE,
  yellow: TUI_YELLOW,
  magenta: TUI_MAGENTA,
  green: TUI_GREEN,
  darkgray: TUI_DARK_GRAY,
}

// Sidebar nav rows, in display order: `default_nav_items` in
// `crates/shelbi-app/src/view.rs`, with the glyphs `nav_glyph` gives them.
// Activity and Machines are palette-only, so they have no row here.
const NAV_ITEMS: { view: NavView; glyph: string; label: string }[] = [
  { view: "chat", glyph: "💬", label: "Orchestrator" },
  { view: "tasks", glyph: "📋", label: "Issues" },
  { view: "changelog", glyph: "🚀", label: "Changelog" },
]

// ── Segment model ─────────────────────────────────────────────────────
// A "segment" is one contiguous run of monospace cells with a single
// style. Each row is a list of segments concatenated left-to-right.
type Segment = {
  text: string
  color?: string
  bg?: string
  bold?: boolean
  /**
   * Pin this run to exactly `cells` monospace columns (`ch`), regardless of the
   * glyph's real advance. Used for the command palette's emoji icons (💬 📋 ⚡),
   * which render wider than one cell in the mono font: pinning them to their
   * logical 2-cell width keeps the modal's right border aligned across rows (the
   * glyph is free to bleed into the following gap, which is blank). Omitted rows
   * lay out by their natural text width.
   */
  cells?: number
}

/** A full-width blank board row, sized to a scenario's content width. */
function blankRow(width: number): Segment[] {
  return [{ text: " ".repeat(width) }]
}

/**
 * Force a row to EXACTLY `width` monospace cells: truncate any segment runs that
 * overflow and right-pad short rows with spaces. Board rows are already width-
 * exact by construction, but the Claude-Code panes (Chat / worker) run arbitrary
 * transcript text through, so every row there is clamped so no line can exceed
 * the board width and push the pinned pane wider than the frame.
 */
function clampRow(segs: Segment[], width: number): Segment[] {
  const out: Segment[] = []
  let used = 0
  for (const seg of segs) {
    if (used >= width) break
    const w = segW(seg)
    if (used + w <= width) {
      out.push(seg)
      used += w
    } else if (seg.cells !== undefined) {
      // A pinned glyph that doesn't fit drops whole.
      break
    } else {
      out.push({ ...seg, text: [...seg.text].slice(0, width - used).join("") })
      used = width
      break
    }
  }
  if (used < width) out.push({ text: " ".repeat(width - used) })
  return out
}
/** A segment's width in cells: its pinned `cells`, else one per character. */
function segW(seg: Segment): number {
  return seg.cells ?? [...seg.text].length
}

/** A row's width in cells. */
function rowW(segs: Segment[]): number {
  return segs.reduce((n, s) => n + segW(s), 0)
}

/**
 * Truncate `text` to fit inside `width` monospace cells, using the same
 * `…`-suffix rule the TUI's `truncate()` helper uses
 * (crates/shelbi-tui/src/kanban.rs `truncate`).
 */
function truncate(text: string, width: number): string {
  const chars = [...text]
  if (chars.length <= width) return text
  if (width <= 0) return ""
  return chars.slice(0, width - 1).join("") + "…"
}

/**
 * Wrap `title` to at most two lines within `max` cells, mirroring
 * `wrap_title_two_lines` in crates/shelbi-tui/src/kanban.rs. Returns
 * `[line1, null]` when the title fits on one line (2-row card) and
 * `[line1, line2]` when it wraps (3-row card). Line 1 breaks on the last space
 * that fits so a word isn't split when avoidable, hard-wrapping at `max` when
 * the first token alone overflows; line 2 is the remainder, `…`-truncated.
 */
function wrapTitleTwoLines(title: string, max: number): [string, string | null] {
  if (max <= 0) return ["", null]
  const chars = [...title]
  if (chars.length <= max) return [title, null]
  // The last space at or before `max` (indices 1..=max), so line 1 ends on a
  // word boundary; index 0 is skipped (would leave line 1 empty) and a first
  // token longer than the width hard-wraps at `max`.
  let breakIdx = -1
  for (let i = max; i >= 1; i -= 1) {
    if (chars[i] === " ") {
      breakIdx = i
      break
    }
  }
  const [line1End, remainderStart] = breakIdx >= 0 ? [breakIdx, breakIdx + 1] : [max, max]
  const line1 = chars.slice(0, line1End).join("")
  const remainder = chars.slice(remainderStart).join("")
  return [line1, truncate(remainder, max)]
}

/** Right-pad a text run with spaces so it fills exactly `width` cells. */
function padTo(text: string, width: number): string {
  const len = [...text].length
  return len >= width ? text : text + " ".repeat(width - len)
}

/** A blank column cell `width` spaces wide, no color. */
function blankColRow(width: number): Segment[] {
  return [{ text: " ".repeat(width) }]
}

/**
 * A collapsed column's rows, mirroring `render_collapsed_column`: the label
 * spelled one bold letter per row in the category color (a space in the name is
 * a blank row), then one blank row and the hidden-card count in parens — a thin
 * folded strip pinned to `COLLAPSED_W` cells. Each row is centered within the
 * strip, and the first letter is on row 0 so it lines up with the full columns'
 * header row.
 */
function collapsedColumnRows(col: Column): Segment[][] {
  const cat = CATEGORY_COLOR[col.category]
  const center = (segs: Segment[]): Segment[] => {
    const used = rowW(segs)
    const total = Math.max(0, COLLAPSED_W - used)
    const left = Math.floor(total / 2)
    return [{ text: " ".repeat(left) }, ...segs, { text: " ".repeat(total - left) }]
  }
  const rows: Segment[][] = []
  for (const ch of col.label) {
    rows.push(ch === " " ? center([]) : center([{ text: ch, color: cat, bold: true }]))
  }
  rows.push(center([]))
  rows.push(center([{ text: `(${col.cards.length})`, color: TUI_DARK_GRAY }]))
  return rows
}

/** What a column's cards need from the scenario to draw their meta line. */
type CardMeta = {
  /** The workflow badge text, when cards carry one. */
  badge?: string
  /** The GitHub login default branches are named after. */
  user: string
}

/**
 * Build the visible rows for a single column `width` cells wide: header row
 * followed by one entry per card (title on one or two lines + a meta row) with
 * a blank row between cards. A collapsed column folds to its narrow strip
 * instead.
 */
function columnRows(col: Column, width: number, meta: CardMeta): Segment[][] {
  if (col.collapsed) return collapsedColumnRows(col)
  const rows: Segment[][] = []
  const textW = width - 2

  // Header row: `LABEL (N)` in the category color, count in DarkGray,
  // padded to the column width — no leading pad because the caller adds the
  // board-level " " before column 1.
  // The label truncates (`…`) before it would run into the 2-cell gutter.
  const count = ` (${col.cards.length})`
  const label = truncate(col.label, textW - [...count].length)
  const padCount = width - [...label].length - [...count].length
  const catColor = CATEGORY_COLOR[col.category]
  rows.push([
    { text: label, color: catColor, bold: true },
    { text: count, color: TUI_DARK_GRAY },
    { text: " ".repeat(Math.max(0, padCount)) },
  ])

  col.cards.forEach((card, idx) => {
    if (idx > 0) rows.push(blankColRow(width))

    // Title rows — wrapped to at most two lines within the card's text width
    // (the way the TUI wraps card titles), each bold and right-padded with the
    // 2-cell gutter that lives inside the column's cell.
    const [titleLine1, titleLine2] = wrapTitleTwoLines(card.title, textW)
    for (const tline of titleLine2 === null ? [titleLine1] : [titleLine1, titleLine2]) {
      if (card.selected) {
        rows.push([{ text: padTo(tline, width), color: SEL_FG, bg: SEL_BG, bold: true }])
      } else {
        rows.push([
          { text: tline, color: TUI_FG, bold: true },
          { text: " ".repeat(Math.max(0, width - [...tline].length)) },
        ])
      }
    }

    // Meta row — the workflow name as a gray badge chip, followed by the
    // issue's git branch (`⎇ …`, dim) once one has been cut. Mirrors
    // `card_meta_line` in crates/shelbi-tui/src/kanban.rs. Dispatch cuts the
    // branch (`{{github_user}}/{{id}}` in the shipped workflows), so a card in
    // an active or handoff column shows one; BACKLOG / TO DO cards show just
    // the badge. A card's own `branch` overrides the default.
    const hasBranch = col.category === "yellow" || col.category === "magenta"
    const branchName = card.branch ?? (hasBranch ? `${meta.user}/${card.id}` : undefined)
    const branch = branchName ? `⎇ ${branchName}` : ""
    const segs: Segment[] = []
    let metaUsed = 0
    if (meta.badge) {
      const badge = truncate(` ${meta.badge} `, textW)
      segs.push({ text: badge, bg: WORKFLOW_BADGE_BG, color: WORKFLOW_BADGE_FG })
      metaUsed += [...badge].length
    }
    if (branch) {
      const sep = segs.length ? 1 : 0
      const remaining = textW - metaUsed - sep
      if (remaining > 0) {
        if (sep) segs.push({ text: " " })
        const b = truncate(branch, remaining)
        // Pin `⎇ ` (the mono font lacks the glyph) to its two cells so the
        // fallback glyph can't push the columns to its right off the grid.
        const head = [...b].slice(0, 2).join("")
        segs.push({ text: head, color: TUI_DARK_GRAY, cells: [...head].length })
        segs.push({ text: [...b].slice(2).join(""), color: TUI_DARK_GRAY })
        metaUsed += sep + [...b].length
      }
    }
    if (metaUsed < width) segs.push({ text: " ".repeat(width - metaUsed) })
    if (card.selected) {
      // The whole row takes the selection gray, but the badge keeps its own
      // fill (its bg is distinct from the selection gray on purpose) so it
      // stays visible — matching `theme::WORKFLOW_BADGE_BG`.
      rows.push(
        segs.map((s) =>
          s.bg === WORKFLOW_BADGE_BG ? { ...s, color: SEL_FG } : { ...s, bg: SEL_BG, color: SEL_FG },
        ),
      )
    } else {
      rows.push(segs)
    }
  })

  return rows
}

/**
 * Zip all columns row-by-row into a single board grid. Shorter columns
 * are padded down with blank rows so the grid stays rectangular.
 */
function buildBoardRows(state: AppState): Segment[][] {
  const widths = columnWidths(state)
  const badge =
    state.workflow && !state.workflowFilter && !state.singleWorkflow ? state.workflow : undefined
  const meta: CardMeta = { badge, user: branchUser(state) }
  const perCol = state.columns.map((c, i) => columnRows(c, widths[i], meta))
  const maxRows = Math.max(0, ...perCol.map((r) => r.length))
  const grid: Segment[][] = []

  for (let r = 0; r < maxRows; r += 1) {
    const rowSegs: Segment[] = [{ text: " " }]
    for (let c = 0; c < perCol.length; c += 1) {
      // Pad short columns with a blank cell of that column's own width, so a
      // collapsed column's narrow strip stays narrow on its empty rows too.
      const cellRow = perCol[c][r] ?? blankColRow(widths[c])
      rowSegs.push(...cellRow)
    }
    grid.push(rowSegs)
  }
  return grid
}

/**
 * The Issues board's title row (`render_title` in kanban.rs):
 * `Issues · <project>   N total` on the left, the ` Workflow: All ▾` (only
 * while more than one workflow is loaded) and ` Workspace: All ▾` filter chips
 * flush right. A filtered chip reads its value in bold cyan. The chips drop
 * when the row is too narrow for them, as the TUI's do.
 */
function titleRow(state: AppState): Segment[] {
  const { columns, project } = state
  const width = contentWidth(state)
  const total = columns.reduce((n, c) => n + c.cards.length, 0)
  const left: Segment[] = [
    { text: " Issues · ", color: TUI_DARK_GRAY },
    { text: project, color: TUI_CYAN, bold: true },
    { text: `   ${total} total`, color: TUI_DARK_GRAY },
  ]
  const chips: Segment[] = []
  if (!state.singleWorkflow) {
    chips.push({ text: " Workflow: ", color: TUI_DARK_GRAY })
    chips.push(
      state.workflowFilter
        ? { text: `${state.workflowFilter} ▾`, color: TUI_CYAN, bold: true }
        : { text: "All ▾", color: TUI_DARK_GRAY },
    )
  }
  chips.push({ text: " Workspace: All ▾", color: TUI_DARK_GRAY })
  const leftW = rowW(left)
  const chipsW = rowW(chips)
  // One trailing cell mirrors the board's one leading cell.
  if (leftW + 1 + chipsW + 1 > width) return bodyRow(left, width)
  return [...left, { text: " ".repeat(width - leftW - chipsW - 1) }, ...chips, { text: " " }]
}

/**
 * The Issues board's key-hint line (`render_footer` in kanban.rs), all dim,
 * with the default bindings in the macOS display style (`⏎`, `⇥`, `⇧h`).
 * `o workspace` joins only while the selected card is assigned to a workspace.
 * Hints that don't fit the board width drop whole from the end rather than
 * mid-word.
 */
function boardFooterRow(state: AppState, width: number): Segment[] {
  const hints = [
    "h/l col",
    "j/k row",
    "⏎ open",
    "⇧h/⇧l move col",
    "⇧k/⇧j reorder",
    "⇥ workflow",
    "f filter",
    "/ search",
    "r refresh",
  ]
  const selected = state.columns.flatMap((c) => c.cards).find((c) => c.selected)
  if (selected?.workspace) hints.push("o workspace")
  let text = "  " + hints[0]
  for (const hint of hints.slice(1)) {
    const next = `${text}   ${hint}`
    if ([...next].length + 1 > width) break
    text = next
  }
  return bodyRow([{ text, color: TUI_DARK_GRAY }], width)
}

// ── Activity, Changelog & Orchestrator panes ──────────────────────────
// The alternate right-pane views, built as `Segment[][]` through the same
// engine as the board so they read as captures of the real TUI's Activity,
// Changelog and Orchestrator (Claude Code session) views — same palette, same
// monospace grid, same character alignment.

/** Pad a run of segments to the board content width so a view fills the pane. */
function bodyRow(segs: Segment[], width: number): Segment[] {
  const used = rowW(segs)
  if (used >= width) return segs
  return [...segs, { text: " ".repeat(width - used) }]
}

/**
 * Left content + a right-aligned run, padded to the board `width` with a 1-col
 * trailing margin — the board's title/footer right-alignment shape, reused for
 * Activity timestamps so they right-align at the board edge (never beyond it).
 */
function bodyRightAlignRow(left: Segment[], right: Segment[], width: number): Segment[] {
  const leftW = rowW(left)
  const rightW = rowW(right)
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

/**
 * A full-pane view: its scrolling rows (padded to fill the pane) and the rows
 * pinned to the pane's bottom edge.
 */
type Pane = { top: Segment[][]; footer: Segment[][] }

/** Fit a pane's rows to exactly `rows` tall: pad (or clip) the top, keep the footer. */
function fitPane(pane: Pane, rows: number, width: number): Pane {
  const topRows = Math.max(0, rows - pane.footer.length)
  return {
    top: padBodyTo(pane.top, topRows, width).map((r) => clampRow(r, width)),
    footer: pane.footer.map((r) => clampRow(r, width)),
  }
}

// ── Activity ──────────────────────────────────────────────────────────
// The Activity feed (`crates/shelbi-tui/src/activity.rs` over the shared
// `crates/shelbi-app/src/activity.rs` rows), opened from the palette: a
// `Activity   <project>` title, the `All · Zen · Workspaces` filter pills, a
// date-bucketed feed, and the key hints, inside a 2-cell margin. Each feed row
// reads as a sentence: a tinted `[name]` chip, a dim verb, the bright title,
// and the outcome pill + relative time pinned right, over a dim second line.

const ACT_MARGIN = 2 // `horizontal_margin(2)` in `render_full`
const ACT_CHIP_W = 9 // `CHIP_W`: the chip column's minimum width
const ACT_VERB_GAP = 2 // `VERB_GAP`

/** Per-workspace chip tint (`agent_tone` in shelbi-app's activity.rs). */
const AGENT_TONE: Record<string, string> = {
  alpha: TUI_CYAN,
  bravo: TUI_MAGENTA,
  charlie: TUI_GREEN,
  delta: TUI_YELLOW,
  echo: TUI_BLUE,
  foxtrot: TUI_LIGHT_RED,
}

/** One feed row (a `FeedRow`): chip, verb, title, pill, time, second line. */
type ActivityRow = {
  /** `[name]`: a workspace in its tint (bold), or a dim system label (`you`). */
  chip: { label: string; color: string; bold: boolean }
  verb: string
  verbColor?: string
  title?: string
  pill?: { text: string; color: string }
  time: string
  /** The dim second line: `⎇ <branch>` or a detail. */
  secondary?: string
}

/**
 * A representative `Today` feed for the busy default board, newest first:
 * the same workspaces, issues and `<user>/<id>` branches the board shows.
 */
function activityRows(user: string): ActivityRow[] {
  const agent = (ws: string) => ({ label: ws, color: AGENT_TONE[ws] ?? TUI_GRAY, bold: true })
  const system = (label: string) => ({ label, color: TUI_DARK_GRAY, bold: false })
  return [
    {
      chip: agent("charlie"),
      verb: "finished",
      title: "Cold-start cache",
      pill: { text: "ready for review", color: TUI_MAGENTA },
      time: "took 12m · 12m",
      secondary: `⎇ ${user}/t-011`,
    },
    {
      chip: agent("alpha"),
      verb: "is building",
      title: "Deploy staging env",
      pill: { text: "in progress", color: TUI_YELLOW },
      time: "18m",
      secondary: `⎇ ${user}/t-009`,
    },
    { chip: system("bravo"), verb: "awaiting input", verbColor: TUI_BLUE, time: "25m" },
    {
      chip: agent("echo"),
      verb: "is building",
      title: "Backfill order index",
      pill: { text: "in progress", color: TUI_YELLOW },
      time: "40m",
      secondary: `⎇ ${user}/t-017`,
    },
    {
      chip: system("you"),
      verb: "merged",
      title: "Ship dark mode",
      pill: { text: "done", color: TUI_GREEN },
      time: "1h",
      secondary: "moved to done",
    },
  ]
}

/** Lay one feed row out at `width` (`paint_row`): one line plus its second line. */
function activityRowLines(row: ActivityRow, width: number): Segment[][] {
  const chip = `[${row.chip.label}]`
  const chipW = [...chip].length
  const msgX = chipW < ACT_CHIP_W ? ACT_CHIP_W : chipW + 1
  const titleX = row.verb ? msgX + [...row.verb].length + ACT_VERB_GAP : msgX
  const left: Segment[] = [
    { text: chip, color: row.chip.color, bold: row.chip.bold },
    { text: " ".repeat(msgX - chipW) },
    { text: row.verb, color: row.verbColor ?? TUI_GRAY },
  ]
  if (row.title) {
    left.push({ text: " ".repeat(ACT_VERB_GAP) })
    left.push({ text: row.title, color: SEL_FG })
  }
  const right: Segment[] = []
  if (row.pill) right.push({ text: `[${row.pill.text}]`, color: row.pill.color, bold: true })
  if (row.time) {
    if (right.length) right.push({ text: "  " })
    right.push({ text: row.time, color: TUI_DARK_GRAY })
  }
  const pad = Math.max(1, width - rowW(left) - rowW(right))
  const lines: Segment[][] = [[...left, { text: " ".repeat(pad) }, ...right]]
  if (row.secondary) {
    lines.push([
      { text: " ".repeat(titleX) },
      { text: truncate(row.secondary, width - titleX), color: TUI_DARK_GRAY },
    ])
  }
  return lines
}

/** The Activity view: title, filter pills, the feed, and the key hints. */
function buildActivityPane(state: AppState, width: number): Pane {
  const inner = width - 2 * ACT_MARGIN
  const margin = (segs: Segment[]): Segment[] => [{ text: " ".repeat(ACT_MARGIN) }, ...segs]
  const top: Segment[][] = []
  top.push(
    margin([
      { text: "Activity", color: TUI_CYAN, bold: true },
      { text: `   ${state.project}`, color: TUI_DARK_GRAY },
    ]),
  )
  top.push(
    margin([
      { text: "All", color: TUI_CYAN, bold: true },
      { text: "  ·  ", color: TUI_DARK_GRAY },
      { text: "Zen", color: TUI_DARK_GRAY },
      { text: "  ·  ", color: TUI_DARK_GRAY },
      { text: "Workspaces", color: TUI_DARK_GRAY },
    ]),
  )
  top.push([])
  // The `── Today ───…` date-bucket header, dim (`date_header`), then the rows,
  // each followed by a blank line.
  const head = "── Today "
  top.push(margin([{ text: head + "─".repeat(Math.max(0, inner - [...head].length)), color: TUI_DARK_GRAY }]))
  top.push([])
  for (const row of activityRows(branchUser(state))) {
    for (const line of activityRowLines(row, inner)) top.push(margin(line))
    top.push([])
  }
  const hints = "j/k scroll · ⇞/⇟ page · g top · ⇧g bottom · r refresh · a/z/w filter"
  return {
    top: top.map((r) => bodyRow(r, width)),
    footer: [bodyRow(margin([{ text: hints, color: TUI_DARK_GRAY }]), width)],
  }
}

/** Curly-quote a title (the crate's `title_quoted`). */
function quoted(title: string): string {
  return `\u{201C}${title}\u{201D}`
}

// ── Changelog ─────────────────────────────────────────────────────────
// The Changelog page (`crates/shelbi-tui/src/shell/changelog.rs`), the
// sidebar's third nav row: the project's shipped work as a blog in a centered
// reading column. The title row reads `Changelog` and the entry count beside
// the half-block search box; each entry is its bold title with the date at the
// right, a rule, the description, and a row of link chips (`⧉` an issue, `↗` a
// PR). The key hints sit along the bottom.

type ChangelogEntry = {
  title: string
  date: string
  body: string
  links: { glyph: "⧉" | "↗"; label: string }[]
}

const CHANGELOG_ENTRIES: ChangelogEntry[] = [
  {
    title: "Dark mode",
    date: "October 27, 2026",
    body: "The dashboard follows the system theme, and Settings has a toggle to pin light or dark.",
    links: [
      { glyph: "⧉", label: "t-013" },
      { glyph: "↗", label: "#412" },
    ],
  },
  {
    title: "Postgres 16",
    date: "October 24, 2026",
    body: "The primary database runs on Postgres 16, moved over with no downtime. Order lookups are about 30% faster.",
    links: [
      { glyph: "⧉", label: "t-012" },
      { glyph: "↗", label: "#398" },
    ],
  },
  {
    title: "Dead-letter retries",
    date: "October 21, 2026",
    body: "Failed webhook deliveries retry with backoff before they land in the dead-letter queue.",
    links: [
      { glyph: "⧉", label: "t-019" },
      { glyph: "↗", label: "#377" },
    ],
  },
]

const CHANGELOG_MEASURE = 100 // `MAX_MEASURE`
const CHANGELOG_SEARCH_W = 34 // `SEARCH_MAX`
const CHANGELOG_HINTS = "j/k scroll · n/p entry · g/G ends · / search · Tab links · ↵ open"

/** The Changelog view, `rows` tall, laid out as `plan` / `Doc::build` do. */
function buildChangelogPane(width: number, rows: number): Pane {
  const measure = Math.min(width - 4, CHANGELOG_MEASURE)
  const x0 = Math.floor((width - measure) / 2)
  const at = (x: number, segs: Segment[]): Segment[] => [{ text: " ".repeat(x) }, ...segs]
  const top: Segment[][] = []

  // Header: the 3-row search box at the column's right edge, the title row on
  // its middle row.
  const searchW = Math.min(CHANGELOG_SEARCH_W, Math.floor(measure / 2))
  const searchX = x0 + measure + 1 - searchW
  const cap = (upper: string | undefined, lower: string | undefined): Segment[] =>
    at(searchX, [{ text: " ".repeat(searchW), bg: halfFill(upper, lower) }])
  const count = `${CHANGELOG_ENTRIES.length} entries`
  const title: Segment[] = [
    { text: "Changelog", color: SEL_FG, bold: true },
    { text: `  ${count}`, color: TUI_DARK_GRAY },
  ]
  const label: Segment[] = [
    { text: " ", bg: SEARCH_BG },
    { text: "🔍", cells: 2, bg: SEARCH_BG },
    { text: " Search", color: TUI_GRAY, bg: SEARCH_BG },
  ]
  label.push({ text: " ".repeat(Math.max(0, searchW - rowW(label) - 2)), bg: SEARCH_BG })
  label.push({ text: "/", color: TUI_GRAY, bg: SEARCH_BG })
  label.push({ text: " ", bg: SEARCH_BG })
  top.push(cap(undefined, SEARCH_BG))
  top.push([
    ...at(x0, title),
    { text: " ".repeat(Math.max(1, searchX - x0 - rowW(title))) },
    ...label,
  ])
  top.push(cap(SEARCH_BG, undefined))

  // Entries: a gap row, the title with its date at the right, a rule, the
  // description, then a blank row and the link chips.
  for (const e of CHANGELOG_ENTRIES) {
    top.push([])
    const pad = Math.max(2, measure - [...e.title].length - [...e.date].length)
    top.push(
      at(x0, [
        { text: e.title, color: SEL_FG, bold: true },
        { text: " ".repeat(pad) },
        { text: e.date, color: TUI_DARK_GRAY },
      ]),
    )
    top.push(at(x0, [{ text: "─".repeat(measure), color: TUI_DIVIDER }]))
    for (const line of wrapText(e.body, measure)) top.push(at(x0, [{ text: line, color: TUI_FG }]))
    top.push([])
    const chips: Segment[] = []
    e.links.forEach((l, i) => {
      if (i > 0) chips.push({ text: " " })
      chips.push({ text: " " })
      chips.push({ text: l.glyph, color: TUI_CYAN, cells: 1 })
      chips.push({ text: ` ${l.label} `, color: TUI_CYAN })
    })
    top.push(at(x0 - 1, chips))
  }

  // The hints show from 14 rows up, with a blank row above them.
  const footer = rows >= 14 ? [at(x0, [{ text: CHANGELOG_HINTS, color: TUI_DARK_GRAY }])] : []
  return {
    top: top.map((r) => bodyRow(r, width)),
    footer: footer.map((r) => bodyRow(r, width)),
  }
}

// One line of the Claude Code session the Chat pane hosts — the orchestrator
// agent IS a live Claude Code CLI running in a Shelbi session, so this renders
// the real Claude Code UI elements rather than a generic two-speaker chat:
// - `user`  — a `❯ ` prompt turn in the foreground color.
// - `prose` — assistant narration, plain wrapped fg lines.
// - `tool`  — a `⏺ Name(args)` tool-call bullet (green ⏺, bold-ish name).
// - `result`— a `⎿` result line indented under the call, dim, optionally
//             collapsed with a trailing `… +N lines` the way Claude Code folds
//             long output.
// - `working`— the `✻ Working…` spinner line, dim accent.
// - `status`— session stats that drive the pane's pinned footer (`Model: … ·
//             Ctx … · Cost … · Session …`); rendered as chrome at the bottom of
//             the pane rather than inline in the transcript.
// - `blank` — vertical spacing between turns.
export type ChatLine =
  | { kind: "user"; text: string }
  | { kind: "prose"; text: string }
  | { kind: "tool"; name: string; args: string }
  | { kind: "result"; text: string; more?: number }
  | { kind: "working"; text: string }
  | { kind: "status"; model: string; ctx: string; cost: string; session?: string }
  | { kind: "blank" }
  // A dispatched task's markdown body seeded into a worker's session as its
  // opening `❯` prompt: the `# Title`, a short description, and `##` sections
  // (Technical Details, Acceptance Criteria with `- [ ]` items) — the
  // orchestrator's Feature task-body template, rendered with light markdown
  // styling. `body` is pre-wrapped (one buffer row per line).
  | { kind: "task"; body: string }

// A believable orchestration session over the sample project: the human asks
// for two pieces of work, the orchestrator adds + dispatches issues to free
// workspaces (alpha/bravo) via `shelbi issue` CLI calls (their real output),
// then summarizes. Matches the board's issue titles, ids and workspaces so the
// Orchestrator and Issues views read as one system.
const CHAT_LINES: ChatLine[] = [
  { kind: "user", text: "Deploy the staging environment and wire up OAuth." },
  { kind: "blank" },
  { kind: "prose", text: "I'll add both issues and start them on free workspaces." },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: 'shelbi issue add "Deploy staging env" --id t-009' },
  { kind: "result", text: "✓ t-009 created in backlog (priority 1)" },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: "shelbi issue start t-009 --workspace alpha" },
  { kind: "result", text: "→ launching alpha on t-009 (branch: you/t-009, agent: developer)", more: 1 },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: "shelbi issue start t-010 --workspace bravo" },
  { kind: "result", text: "→ launching bravo on t-010 (branch: you/t-010, agent: developer)", more: 1 },
  { kind: "blank" },
  {
    kind: "prose",
    text: "Both are running: alpha on staging, bravo on OAuth. Each one opens a PR when it reaches Review.",
  },
  { kind: "blank" },
  { kind: "working", text: "Working… (8s · ↓ 431 tokens)" },
  { kind: "blank" },
  { kind: "status", model: "Opus 4.8", ctx: "4%", cost: "$0.36" },
]

// Assistant prose wraps at a comfortable measure rather than stretching across
// the full 111-cell pane, the way Claude Code soft-wraps its output.
const CHAT_WRAP_W = 76
// The Claude-Code prompt is a bordered box pinned to the bottom of the pane: a
// rule above it, the `❯` input line, a rule below it, then the two footer lines
// (session stats + auto-mode). That's a fixed 5-row chunk the conversation
// scrolls above — see `buildClaudeCodeRows`.
const CLAUDE_CHROME_ROWS = 5

/**
 * Render just the scrolling transcript above the input box: assistant prose,
 * `⏺` tool bullets, `⎿` results, and the optional `✽ Working…` spinner. `status`
 * lines carry no transcript row — they feed the pinned footer instead — so they
 * are skipped here.
 */
function conversationRows(lines: ChatLine[], width: number): Segment[][] {
  const row = (segs: Segment[]) => bodyRow(segs, width)
  const rows: Segment[][] = []

  for (const line of lines) {
    switch (line.kind) {
      case "blank":
        rows.push(blankRow(width))
        break
      case "user":
        // `❯ ` prompt (cyan) + the human's message in the foreground color — a
        // sent turn that now lives in the transcript (the live input box below
        // is where an unsent message is typed).
        rows.push(
          row([
            { text: " ❯ ", color: TUI_CYAN, bold: true },
            { text: line.text, color: TUI_FG },
          ]),
        )
        break
      case "task": {
        // The dispatched task body, rendered as a quiet full-width box of white
        // text — a callout that reads as the injected task, distinct from the
        // work transcript around it. `body` is pre-wrapped (one source line per
        // row); markdown headings (`#`, `##`) stay bold. The box spans the whole
        // pane width, with a 2-cell left margin and a blank boxed row top/bottom.
        const boxRow = (content: Segment[]): Segment[] => {
          const inner: Segment[] = [{ text: "  ", bg: TASK_BOX_BG }, ...content.map((s) => ({ ...s, bg: TASK_BOX_BG }))]
          const used = inner.reduce((a, s) => a + [...s.text].length, 0)
          if (used < width) inner.push({ text: " ".repeat(width - used), bg: TASK_BOX_BG })
          return inner
        }
        const bodyLines = line.body.split("\n")
        rows.push(boxRow([]))
        for (const ln of bodyLines) {
          const bold = ln.startsWith("# ") || ln.startsWith("## ")
          rows.push(ln === "" ? boxRow([]) : boxRow([{ text: ln, color: SEL_FG, bold }]))
        }
        rows.push(boxRow([]))
        break
      }
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
            { text: " ✽ ", color: TUI_MAGENTA },
            { text: line.text, color: TUI_DARK_GRAY },
          ]),
        )
        break
      case "status":
        // Drives the pinned footer, not a transcript row — skip here.
        break
    }
  }
  return rows
}

/** A full-width dim horizontal rule — the top/bottom border of the input box. */
function claudeRuleRow(width: number): Segment[] {
  return [{ text: "─".repeat(width), color: TUI_DIVIDER }]
}

/**
 * The pinned bottom chrome of a Claude-Code pane (`CLAUDE_CHROME_ROWS` rows): a
 * bordered `❯` input box (rule / input line / rule) over the two-line footer —
 * `Model … · Ctx … · Cost … · Session …` with the `𖠰 <badge>` session badge
 * right-aligned, then the `⏵⏵ auto mode on` hint. Session stats come from a
 * `status` line in the transcript (falling back to sensible defaults) so the
 * footer reads live; the badge is the orchestrator's `hub` or, on a worker
 * pane, the focused workspace name.
 */
function claudeChromeRows(state: AppState, lines: ChatLine[], width: number): Segment[][] {
  const input = state.chatInput ?? ""
  const inputRow = bodyRow(
    [
      { text: " ❯ ", color: TUI_CYAN, bold: true },
      { text: input, color: TUI_FG },
    ],
    width,
  )

  const status = lines.find((l) => l.kind === "status")
  const model = status?.model ?? "Opus 4.8"
  const ctx = status?.ctx ?? "4%"
  const cost = status?.cost ?? "$0.36"
  const session = status?.session ?? "3m"
  const badge =
    state.activeView === "workspace" ? (state.focusedWorkspace ?? "worker") : "hub"
  const footerStats = bodyRightAlignRow(
    [
      {
        text: `  Model: ${model} · Ctx ${ctx} · Cost ${cost} · Session ${session}`,
        color: TUI_DARK_GRAY,
      },
    ],
    [{ text: `𖠰 ${badge}`, color: TUI_DARK_GRAY }],
    width,
  )
  const footerMode = bodyRow(
    [
      { text: "  ⏵⏵ ", color: TUI_GREEN },
      { text: "auto mode on (shift+tab to cycle)", color: TUI_DARK_GRAY },
    ],
    width,
  )

  return [claudeRuleRow(width), inputRow, claudeRuleRow(width), footerStats, footerMode]
}

/**
 * A full Claude-Code pane, sized to exactly `target` rows: the transcript
 * scrolls in the area above a bottom-pinned input box + footer, mirroring the
 * real CLI. The conversation is bottom-anchored against the input box — short
 * transcripts leave blank rows above, long ones clip from the top so the most
 * recent turns stay next to the prompt — so the frame never resizes as the
 * story streams in more output.
 */
function buildClaudeCodeRows(
  state: AppState,
  lines: ChatLine[],
  target: number,
  width: number,
): Segment[][] {
  const chrome = claudeChromeRows(state, lines, width)
  const convArea = Math.max(0, target - CLAUDE_CHROME_ROWS)
  const conv = conversationRows(lines, width)
  const visible =
    conv.length >= convArea
      ? conv.slice(conv.length - convArea)
      : [...Array.from({ length: convArea - conv.length }, () => blankRow(width)), ...conv]
  // Clamp every row to exactly `width` cells so a long transcript line can't
  // widen the pane past the board — the frame width stays constant across beats.
  return [...visible, ...chrome].map((row) => clampRow(row, width))
}

/**
 * A bare Claude-Code session (transcript over the pinned `❯` input box and
 * footer), `rows` tall and `width` cells wide, with no Shelbi TUI chrome around
 * it. The desktop mockups draw their Orchestrator and Workspace terminals with
 * it: Claude Code is the same program in either client's terminal, so its
 * transcript comes from this one engine, and the desktop recolors it by
 * redefining the `--tui-*` variables to its terminal palette around the pane.
 * `style` sets the font metrics (the desktop terminal's size and leading).
 */
export function ClaudeCodePane({
  state,
  lines,
  rows,
  width,
  style,
}: {
  state: AppState
  lines: ChatLine[]
  rows: number
  width: number
  style?: React.CSSProperties
}) {
  return (
    <pre
      className="m-0 whitespace-pre font-mono"
      style={{
        ...PRE_STYLE,
        width: `${width}ch`,
        minWidth: `${width}ch`,
        maxWidth: `${width}ch`,
        overflow: "hidden",
        ...style,
      }}
    >
      {buildClaudeCodeRows(state, lines, rows, width).map((row, i) => (
        <Row key={i} segs={row} />
      ))}
    </pre>
  )
}

// ── Sidebar ───────────────────────────────────────────────────────────
// Mirrors `crates/shelbi-tui/src/shell/sidebar.rs` over `SidebarModel::rows`
// (`crates/shelbi-app/src/view.rs`): a header (the project title and the
// filled `🔍 Search  ⌃P` box that opens the palette), the nav block, the
// Workspaces section, the two review sections, and the footer toolbar (⚙
// Settings, 📣 Send Feedback) with the `ZEN MODE ON` band under it while Zen
// is on. Everything sits inside a 1-col inset on each side (`LIST_INDENT`).
// One row = one line; each row is a list of segments the pre element joins
// into a monospace grid, same as the board.

/**
 * A row inside the 1-col inset: a blank gutter cell, `segs` padded to the inner
 * width (with `bg` behind the padding), and the right gutter cell.
 */
function insetRow(segs: Segment[], w: number, bg?: string): Segment[] {
  const inner = w - 2
  const used = rowW(segs)
  const row: Segment[] = [{ text: " " }, ...segs]
  if (used < inner) row.push({ text: " ".repeat(inner - used), bg })
  row.push({ text: " " })
  return row
}

/**
 * An inset row with `right` right-aligned at the inset's edge, or just `left`
 * when there isn't room for both plus a one-cell gap: the TUI drops the right
 * column rather than push the left off-screen (`right_align` in sidebar.rs).
 */
function insetRightAlignRow(
  left: Segment[],
  right: string,
  rightColor: string,
  w: number,
  bg?: string,
): Segment[] {
  const inner = w - 2
  const leftW = rowW(left)
  const rightW = [...right].length
  if (!right || leftW + rightW + 1 > inner) return insetRow(left, w, bg)
  return insetRow(
    [...left, { text: " ".repeat(inner - leftW - rightW), bg }, { text: right, color: rightColor, bg }],
    w,
    bg,
  )
}

// One sidebar nav item, resolved from `NAV_ITEMS`.
type NavItem = (typeof NAV_ITEMS)[number]

/**
 * A rendered sidebar row. Most rows are plain `Segment[]` lines; nav rows are
 * emitted as a tagged descriptor instead so `Sidebar` can render them through
 * the interactive `<NavRow>` (which owns the click/keyboard handlers) while
 * every other row still flows through the static `<Row>`. Plain rows stay
 * `Segment[]` so the many `rows.push(...)` sites are unchanged and the two
 * kinds are told apart with `Array.isArray`.
 */
type SidebarRow = Segment[] | { nav: NavItem; segs: Segment[] }

/**
 * Segments for one nav row: ` 💬 Orchestrator` inside the inset. `fill` paints
 * the inset-wide pill the TUI gives the selected row (and, here, the hovered
 * one); selected text is white-bold like the real selection, a hover-only fill
 * keeps the gray label so the active row stays distinguishable.
 */
function navRowSegs(nav: NavItem, selected: boolean, fill: string | undefined, w: number): Segment[] {
  const color = selected ? SEL_FG : TUI_GRAY
  return insetRow(
    [
      { text: " ", bg: fill },
      { text: nav.glyph, color, bg: fill, cells: 2 },
      { text: ` ${nav.label}`, color, bg: fill, bold: selected },
    ],
    w,
    fill,
  )
}

/** The 3-row search box (`render_search_box`): `▄` caps, the label row, `▀` caps. */
function searchBoxRows(w: number): Segment[][] {
  const inner = w - 2
  const chord = "⌃P"
  const label: Segment[] = [
    { text: " ", bg: SEARCH_BG },
    { text: "🔍", cells: 2, bg: SEARCH_BG },
    { text: " Search", color: TUI_GRAY, bg: SEARCH_BG },
  ]
  const pad = inner - rowW(label) - [...chord].length - 1
  if (pad > 0) {
    label.push({ text: " ".repeat(pad), bg: SEARCH_BG })
    label.push({ text: chord, color: TUI_GRAY, bg: SEARCH_BG })
  }
  return [
    insetRow([], w, halfFill(undefined, SEARCH_BG)),
    insetRow(label, w, SEARCH_BG),
    insetRow([], w, halfFill(SEARCH_BG, undefined)),
  ]
}

/**
 * The footer toolbar (`render_footer_button`): the ⚙ Settings and 📣 Send
 * Feedback squares, each 5 cells wide with one cell between them, the glyph
 * gray on the selection fill, over a `▄` cap row and under a `▀` one.
 */
function toolbarRows(w: number): Segment[][] {
  const squares: { glyph: string; cells: number }[] = [
    { glyph: "⚙", cells: 1 },
    { glyph: "📣", cells: 2 },
  ]
  const SQUARE_W = 5
  const caps = (upper: string | undefined, lower: string | undefined): Segment[] => {
    const segs: Segment[] = []
    squares.forEach((_, i) => {
      if (i > 0) segs.push({ text: " " })
      segs.push({ text: " ".repeat(SQUARE_W), bg: halfFill(upper, lower) })
    })
    return insetRow(segs, w)
  }
  const faces: Segment[] = []
  squares.forEach((s, i) => {
    if (i > 0) faces.push({ text: " " })
    const left = Math.ceil((SQUARE_W - s.cells) / 2)
    faces.push({ text: " ".repeat(left), bg: SEL_BG })
    faces.push({ text: s.glyph, color: TUI_GRAY, bg: SEL_BG, cells: s.cells })
    faces.push({ text: " ".repeat(SQUARE_W - left - s.cells), bg: SEL_BG })
  })
  return [caps(undefined, SEL_BG), insetRow(faces, w), caps(SEL_BG, undefined)]
}

/**
 * The sidebar split into the scrollable body (header, nav, workspace and
 * review sections) and the footer that stays pinned to the bottom of the
 * sidebar — the toolbar plus, when Zen Mode is on, the full-width green band
 * under it. The `Sidebar` component renders `body` at the top and `footer`
 * flush against the bottom edge, as the TUI lays its footer out bottom-up.
 * `hover` is the nav row under the pointer: it takes a fill too.
 */
function buildSidebarRows(
  state: AppState,
  hover?: NavView,
): { body: SidebarRow[]; footer: SidebarRow[] } {
  const w = state.sidebarWidth ?? SIDEBAR_W
  const blank: Segment[] = [{ text: " ".repeat(w) }]
  const rows: SidebarRow[] = []

  // Header — a blank row, the project title in bold cyan, a blank row, then
  // the search box.
  rows.push(blank)
  rows.push(insetRow([{ text: truncate(state.project, w - 2), color: TUI_CYAN, bold: true }], w))
  rows.push(blank)
  rows.push(...searchBoxRows(w))

  // Nav block — the items with a separator row before, between and after
  // them; a filled item's separators carry the half-row bleed of its fill.
  const fillOf = (nav: NavItem | undefined): string | undefined =>
    nav && (nav.view === state.activeView || nav.view === hover) ? SIDEBAR_SEL_BG : undefined
  NAV_ITEMS.forEach((nav, i) => {
    rows.push(insetRow([], w, halfFill(fillOf(NAV_ITEMS[i - 1]), fillOf(nav))))
    rows.push({ nav, segs: navRowSegs(nav, nav.view === state.activeView, fillOf(nav), w) })
  })
  rows.push(insetRow([], w, halfFill(fillOf(NAV_ITEMS[NAV_ITEMS.length - 1]), undefined)))

  // — Workspaces — section header (muted).
  const workspaces = state.machines.flatMap((m) => m.workspaces)
  if (workspaces.length > 0) {
    rows.push(blank)
    rows.push(insetRow([{ text: "— Workspaces —", color: TUI_DARK_GRAY }], w))
    // More than one machine: a `▾ <machine>` header over each machine's
    // indented rows. A single-machine project is a flat list.
    const grouped = state.machines.filter((m) => m.workspaces.length > 0).length > 1
    for (const m of state.machines) {
      if (grouped) rows.push(insetRow([{ text: `▾ ${m.name}`, color: TUI_GRAY }], w))
      for (const ws of m.workspaces) {
        const badgeGlyph = ws.state === "working" ? "⏵" : "·"
        const badgeColor = ws.state === "working" ? TUI_GREEN : TUI_DARK_GRAY
        // The focused-worker pane selects a workspace (rather than a nav row):
        // that row takes the selection fill across the inset, its text keeping
        // its color as the TUI's does.
        const selected = state.activeView === "workspace" && ws.name === state.focusedWorkspace
        const bg = selected ? SIDEBAR_SEL_BG : undefined
        rows.push(
          insetRightAlignRow(
            [
              { text: grouped ? "  " : "", bg },
              { text: badgeGlyph, color: badgeColor, bg, cells: 1 },
              { text: " ", bg },
              { text: ws.name, color: TUI_GRAY, bg },
            ],
            ws.agent ? titleCase(ws.agent) : "idle",
            TUI_DARK_GRAY,
            w,
            bg,
          ),
        )
      }
    }
  }

  // The Review state splits into two sidebar sections, matching
  // `SidebarModel::rows`: "Ready for Review" (loaded on a review workspace,
  // serving) then "Queued for Review" (waiting for a free slot). Each is
  // omitted when empty.
  const pushReviewSection = (label: string, entries: ReviewEntry[], ready: boolean) => {
    if (entries.length === 0) return
    rows.push(blank)
    rows.push(insetRow([{ text: `— ${label} —`, color: TUI_DARK_GRAY }], w))
    for (const entry of entries) {
      rows.push(...reviewEntryRows(entry, ready, w))
    }
  }
  pushReviewSection("Ready for Review", state.readyReview, true)
  pushReviewSection("Queued for Review", state.queuedReview, false)

  // Footer, laid out bottom-up like `footer_rows`: the toolbar (its `▄` row,
  // the squares, its `▀` row) and, only while Zen is on, the full-width green
  // "ZEN MODE ON" band on the bottom row. Returned separately so `Sidebar` can
  // pin it to the bottom edge.
  const footer: SidebarRow[] = [...toolbarRows(w)]
  if (state.zenMode) footer.push(zenBandRow(w))

  return { body: rows, footer }
}

/**
 * The full-width "ZEN MODE ON" band, mirroring `render_zen_row` in
 * `sidebar.rs`: a single edge-to-edge green row with white-bold text, the
 * label left-aligned behind one leading space and the rest of the row filled
 * to the sidebar edge. One green segment so the fill is continuous across the
 * whole width.
 */
function zenBandRow(w: number): Segment[] {
  const label = " ZEN MODE ON"
  const pad = Math.max(0, w - [...label].length)
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
 * Line 1: the badge + title (truncated to the inset), with the
 * `machine:workspace` slot right-aligned when the task is Ready and the title
 * leaves room for it (otherwise the slot drops, as in the TUI). Line 2:
 * `⎇ <branch>`, dim. Ready uses a bold cyan `✓`; Queued a dim `·`.
 */
function reviewEntryRows(entry: ReviewEntry, ready: boolean, w: number): Segment[][] {
  const inner = w - 2
  const glyph: Segment = ready
    ? { text: "✓", color: TUI_CYAN, bold: true, cells: 1 }
    : { text: "·", color: TUI_DARK_GRAY, cells: 1 }
  const left: Segment[] = [glyph, { text: " " }, { text: truncate(entry.title, inner - 2), color: TUI_GRAY }]
  const location = ready ? entry.location : undefined
  const line1 = location ? insetRightAlignRow(left, location, TUI_DARK_GRAY, w) : insetRow(left, w)
  const line2 = insetRow(
    [
      { text: "  " },
      { text: "⎇ ", color: TUI_DARK_GRAY, cells: 2 },
      { text: truncate(entry.branch, inner - 4), color: TUI_DARK_GRAY },
    ],
    w,
  )
  return [line1, line2]
}

/** How many rows the sidebar draws for a scenario (body plus footer). */
function sidebarRowCount(state: AppState): number {
  const { body, footer } = buildSidebarRows(state)
  return body.length + footer.length
}

// ── Command palette ───────────────────────────────────────────────────
// The centered ⌃P command palette overlay: a thin box-drawing-style frame
// (`shelbi · <project>` title) with a `>` fuzzy-filter prompt over the command
// list — the nav views, a Zen toggle, every workspace, and the project/session
// actions. It's drawn as an absolutely-positioned box centered over the dashboard
// (the board stays put and undimmed underneath, the console background bleeding
// around the frame) rather than as a pane in the monospace grid, so the frame
// width/height are untouched. The hero types the
// prompt from empty to `alpha` to filter the list down before activating that
// workspace. `AppState.palette` carries the query + item list; the renderer
// fuzzy-filters and highlights the top match.

const PAL_INNER = 52 // text columns of content between the box's 1-cell side margins
// The palette reads as a box-drawing frame on the console, not a modal card: its
// interior is the same background as the terminal body (`TUI_BG`) so the console
// bleeds continuously under and around the border, and the frame + internal rules
// are the console's muted divider gray (`TUI_DIVIDER`) — the same thin line the
// sidebar/board rules use — not a bright card edge.
const PAL_BG = TUI_BG // interior fill = terminal body, so no panel lifts off the board
const PAL_BORDER = TUI_DIVIDER // thin muted console rule for the frame + internal rules

/**
 * Monospace cell width of `text`, counting the palette's emoji icons (💬 📋 ⚡,
 * and the ⏵/↑↓-range action glyphs) as the 2 cells a terminal renders them so
 * the right-aligned descriptions and the box's right border line up. `·` and
 * plain ASCII count as 1.
 */
function palWidth(text: string): number {
  let w = 0
  for (const ch of text) {
    const cp = ch.codePointAt(0) ?? 0
    w += cp >= 0x1f000 || (cp >= 0x2600 && cp <= 0x27bf) ? 2 : 1
  }
  return w
}

/** Case-insensitive subsequence match: `query`'s chars appear in order in `text`. */
function paletteMatch(query: string, text: string): boolean {
  const q = [...query.toLowerCase()]
  if (q.length === 0) return true
  let qi = 0
  for (const ch of text.toLowerCase()) {
    if (ch === q[qi]) {
      qi += 1
      if (qi === q.length) return true
    }
  }
  return false
}

/**
 * Pad one row's inner segments to `PAL_INNER` columns, with a one-cell margin on
 * each side, so every row is the same width and the description column and the
 * box's right edge stay aligned. The frame itself is a CSS border on the section
 * div (see `PaletteOverlay`), not a `│` glyph, so this row carries no border
 * characters. `fill` backs the whole row edge-to-edge — `PAL_BG` normally,
 * `SEL_BG` for the highlighted row.
 */
function palRow(inner: Segment[], fill: string = PAL_BG): Segment[] {
  const used = inner.reduce((a, s) => a + (s.cells ?? palWidth(s.text)), 0)
  const pad = Math.max(0, PAL_INNER - used)
  return [
    { text: " ", bg: fill },
    ...inner.map((s) => ({ ...s, bg: s.bg ?? fill })),
    { text: " ".repeat(pad + 1), bg: fill },
  ]
}

/** The title row `shelbi · <project>`, sitting above the `>` prompt. */
function palTitleRow(project: string): Segment[] {
  return palRow([
    { text: "shelbi", color: TUI_FG, bold: true },
    { text: " · ", color: TUI_DARK_GRAY },
    { text: project, color: TUI_CYAN, bold: true },
  ])
}

/** The `>` fuzzy-filter prompt row: the typed query + a block cursor. */
function palPromptRow(query: string): Segment[] {
  return palRow([
    { text: "> ", color: TUI_CYAN, bold: true },
    { text: query, color: TUI_FG },
    { text: "▊", color: TUI_CYAN },
  ])
}

/** One command row: `<icon>  <label>            <description>` (top match highlighted). */
function palItemRow(item: PaletteItem, selected: boolean): Segment[] {
  const fill = selected ? SEL_BG : PAL_BG
  const glyphW = palWidth(item.glyph)
  const gap = "  "
  const leftW = glyphW + gap.length + [...item.label].length
  // Right-align the description at the box edge, truncating (…-suffix) any that's
  // too long to leave at least a one-column gap after the label — so a long
  // action description can't collide with its label or push the right border out.
  const desc = truncate(item.desc, Math.max(1, PAL_INNER - leftW - 1))
  const pad = Math.max(1, PAL_INNER - leftW - [...desc].length)
  const inner: Segment[] = [
    // Pin the icon to its logical width so a double-width emoji can't shift the
    // description column or the right border.
    { text: item.glyph, color: item.glyph === "·" ? TUI_DARK_GRAY : undefined, cells: glyphW },
    { text: gap },
    { text: item.label, color: selected ? SEL_FG : TUI_FG, bold: selected },
    { text: " ".repeat(pad) },
    { text: desc, color: selected ? SEL_FG : TUI_DARK_GRAY },
  ]
  return palRow(inner, fill)
}

/** The footer keybinding hint row: keys in fg, connectors dim. */
function palFooterRow(): Segment[] {
  const dim = TUI_DARK_GRAY
  return palRow([
    { text: "↑↓", color: TUI_FG },
    { text: " navigate · ", color: dim },
    { text: "Enter", color: TUI_FG },
    { text: " activate · ", color: dim },
    { text: "Esc / Ctrl+P", color: TUI_FG },
    { text: " close", color: dim },
  ])
}

/**
 * Pin every non-ASCII glyph in each palette row to its logical cell width so the
 * modal renders as a strict monospace grid, keeping the description column and
 * the box's right edge aligned across rows.
 *
 * The mono font (Geist Mono) has no block (`▊`), arrow (`↑↓`) or middle-dot
 * glyphs, so the browser substitutes a fallback whose advance is a hair wider
 * than one cell. Left as natural text, a row carrying several of these drifts
 * wider than a plain-ASCII row, so their right edges no longer line up under the
 * CSS border. Pinning each substituted glyph to its `palWidth` (via the same
 * fixed-`cells` inline-block the emoji icons use) forces a true grid: the wider
 * glyph bleeds harmlessly past its cell but the column advance is exact, so
 * every row shares the same width. ASCII runs are left as natural text (they
 * already advance one cell) to keep the label/description columns and the
 * selected-row fill untouched.
 */
function pinPaletteCells(rows: Segment[][]): Segment[][] {
  return rows.map((row) =>
    row.flatMap((seg) => {
      if (seg.cells !== undefined) return [seg] // already pinned (emoji icon)
      const out: Segment[] = []
      let ascii = ""
      const flush = () => {
        if (ascii) out.push({ ...seg, text: ascii })
        ascii = ""
      }
      for (const ch of seg.text) {
        if ((ch.codePointAt(0) ?? 0) > 0x7f) {
          flush()
          out.push({ ...seg, text: ch, cells: palWidth(ch) })
        } else {
          ascii += ch
        }
      }
      flush()
      return out
    }),
  )
}

/**
 * Build the palette modal's rows, grouped into the three CSS-bordered sections
 * `PaletteOverlay` stacks: `header` (the title + `>` prompt), `list` (the
 * fuzzy-filtered command rows, top match highlighted), and `footer` (the
 * keybinding hints). The frame and the rules between these sections are CSS
 * borders on the section divs — not box-drawing rows — so there are no border,
 * divider, or corner glyphs here.
 */
function buildPaletteSections(
  palette: PaletteState,
  project: string,
): { header: Segment[][]; list: Segment[][]; footer: Segment[][] } {
  const matches = palette.items.filter((it) => paletteMatch(palette.query, it.label))
  const list =
    matches.length === 0
      ? [palRow([{ text: "no matching commands", color: TUI_DARK_GRAY }])]
      : matches.map((it, i) => palItemRow(it, i === 0))
  return {
    header: pinPaletteCells([palTitleRow(project), palPromptRow(palette.query)]),
    list: pinPaletteCells(list),
    footer: pinPaletteCells([palFooterRow()]),
  }
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
        if (seg.bg || seg.cells !== undefined) {
          // A filled run is a full-line-height box, so stacked fills (a
          // two-line selected card, the nav pill and its half-row bleed, the
          // search box) meet with no seam: an inline span's background only
          // covers the font's content box, a little shorter than the line.
          style.display = "inline-block"
          style.height = LINE_H
          style.verticalAlign = "top"
        }
        if (seg.cells !== undefined) {
          // Fixed-width cell: pin the layout advance to `cells` ch so a
          // wider-than-mono glyph (palette emoji) can't shift the columns after
          // it; the glyph bleeds harmlessly into the following blank gap.
          style.width = `${seg.cells}ch`
        }
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
 * hover/focus reports up to `Sidebar`, which paints the `SIDEBAR_SEL_BG` pill
 * (and its half-row bleed on the separator rows) as a discoverability
 * affordance. The trailing newline stays outside the button so the button's
 * box hugs the row content.
 */
function NavRow({
  nav,
  segs,
  selected,
  onSelect,
  onHover,
}: {
  nav: NavItem
  segs: Segment[]
  selected: boolean
  onSelect: (view: NavView) => void
  onHover: (view: NavView | undefined) => void
}) {
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
        onMouseEnter={() => onHover(nav.view)}
        onMouseLeave={() => onHover(undefined)}
        onFocus={() => onHover(nav.view)}
        onBlur={() => onHover(undefined)}
      >
        <SegSpans segs={segs} />
      </span>
      {"\n"}
    </>
  )
}

// One terminal row's height; filled runs size their boxes to it (`SegSpans`).
const LINE_H = "17px"

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
  lineHeight: LINE_H,
  minWidth: "max-content",
}

/**
 * Opacity + transition applied to a pane's `<pre>` (its text/glyph layer) so the
 * hero can cross-fade the rendered rows at the loop boundary. The pane's own
 * background is `TUI_BG` — the same fill the solid container carries behind it —
 * so fading the pane dissolves only the glyphs (and their selection highlights),
 * while the identical `TUI_BG` behind keeps the terminal body reading fully
 * opaque. Omitted (the docs default) leaves the pane fully opaque with no
 * transition.
 */
function contentFadeStyle(contentOpacity?: number): React.CSSProperties {
  return {
    opacity: contentOpacity,
    transition: contentOpacity === undefined ? undefined : "opacity 500ms ease",
  }
}

function TerminalBody({ state, contentOpacity }: { state: AppState; contentOpacity?: number }) {
  // The right pane swaps on the active view. Every view is padded to a common
  // target height so the terminal frame stays a fixed size when switching views
  // (and, for the hero animation, across every beat as content grows/shrinks):
  // the natural board height, floored by any `minBodyRows` pin.
  const width = contentWidth(state)
  const boardRows = buildBoardRows(state)
  const target = Math.max(boardRows.length, state.minBodyRows ?? 0)
  // The Issues board carries a two-row header (title + blank) and a two-row
  // footer (blank + key hints) around its columns: `target + 4` rows in all.
  // Every other view is a full pane laid out to the same height, so the frame
  // never resizes as views swap — or to the sidebar's height when that's
  // taller, so a pane's own bottom chrome (the Claude Code input box, the key
  // hints) sits on the frame's bottom edge.
  let bodyRows: Segment[][]
  let footerRows: Segment[][]
  if (state.activeView === "tasks") {
    bodyRows = [titleRow(state), blankRow(width), ...padBodyTo(boardRows, target, width)]
    footerRows = [blankRow(width), boardFooterRow(state, width)]
  } else {
    const rows = Math.max(target + 4, sidebarRowCount(state))
    const pane: Pane =
      state.activeView === "activity"
        ? buildActivityPane(state, width)
        : state.activeView === "changelog"
          ? buildChangelogPane(width, rows)
          : {
              // The Orchestrator and worker panes are full Claude Code
              // sessions: no Shelbi header or hint bar, the session fills the
              // pane flush the way the real CLI does.
              top: buildClaudeCodeRows(
                state,
                (state.activeView === "chat" ? state.chatLines : state.workspaceLines) ?? CHAT_LINES,
                rows,
                width,
              ),
              footer: [],
            }
    const fitted = fitPane(pane, rows, width)
    bodyRows = fitted.top
    footerRows = fitted.footer
  }

  return (
    <pre
      // `flex flex-col` so the footer group can be pinned to the bottom edge
      // (`mt-auto` below), the same way `Sidebar` pins its toolbar — the hint
      // bar stays at the bottom of the pane rather than floating right under
      // the content when the pane is taller than its content.
      className="m-0 flex flex-col whitespace-pre font-mono"
      // Pin the pane to exactly `width` cells (`ch` == the monospace advance) so
      // the frame width is identical across every view. `minWidth: max-content`
      // would otherwise size the pane to its widest *rendered* line, and the
      // Claude Code panes contain glyphs the mono font lacks (the `𖠰` footer
      // badge, `⏺`/`❯`/`✽` tool bullets) that fall back to a wider face — making
      // those panes render wider than the pure-ASCII board. Clipping any such
      // overflow (`overflow: hidden`) only ever trims trailing padding, never
      // content.
      style={{
        ...PRE_STYLE,
        width: `${width}ch`,
        minWidth: `${width}ch`,
        maxWidth: `${width}ch`,
        overflow: "hidden",
        ...contentFadeStyle(contentOpacity),
      }}
    >
      <div>
        {bodyRows.map((row, i) => (
          <Row key={i} segs={row} />
        ))}
      </div>
      {/* Pinned to the bottom of the pane; the auto top margin absorbs the
          slack when a taller sibling stretches this `<pre>` past its content. */}
      <div className="mt-auto">
        {footerRows.map((row, i) => (
          <Row key={i} segs={row} />
        ))}
      </div>
    </pre>
  )
}

function Sidebar({
  state,
  onSelectView,
  contentOpacity,
}: {
  state: AppState
  onSelectView: (view: NavView) => void
  contentOpacity?: number
}) {
  const [hover, setHover] = useState<NavView | undefined>(undefined)
  const { body, footer } = buildSidebarRows(state, hover)
  const w = state.sidebarWidth ?? SIDEBAR_W
  const renderRow = (row: SidebarRow, i: number) =>
    Array.isArray(row) ? (
      <Row key={i} segs={row} />
    ) : (
      <NavRow
        key={i}
        nav={row.nav}
        segs={row.segs}
        selected={row.nav.view === state.activeView}
        onSelect={onSelectView}
        onHover={setHover}
      />
    )
  return (
    <pre
      // `flex flex-col` so the footer group can be pushed to the bottom edge
      // (`mt-auto`) — the board pane is taller than the sidebar's content, and
      // the flex row stretches this `<pre>` to that height, so the footer
      // otherwise floats right under the last workspace row instead of sitting
      // at the bottom of the window like the TUI's `render_footer` does.
      className="m-0 hidden flex-col whitespace-pre border-r font-mono md:flex"
      style={{
        ...PRE_STYLE,
        // CHROME_BAR_BG/BORDER sit too close to TUI_BG to read as a rule;
        // a mid-gray in the TUI gray family gives a distinct-but-tasteful
        // divider against the dark terminal body.
        borderColor: TUI_DIVIDER,
        // Pin the sidebar to its fixed cell width so the frame width never
        // shifts between beats: the working badge (`⏵`) and nav emoji fall back
        // to wider glyphs than the mono cell, so `max-content` would let a beat
        // with working workspaces render a hair wider than an all-idle one.
        // Clipping only trims trailing padding, never a label.
        width: `${w}ch`,
        minWidth: `${w}ch`,
        maxWidth: `${w}ch`,
        overflow: "hidden",
        ...contentFadeStyle(contentOpacity),
      }}
    >
      <div>{body.map(renderRow)}</div>
      {/* Pinned to the bottom of the sidebar; the auto top margin absorbs the
          slack between the workspace list and the bottom edge. */}
      <div className="mt-auto">{footer.map(renderRow)}</div>
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
 * One CSS-bordered section of the palette modal: its monospace rows rendered as
 * a `<pre>`, each row a block `<span>` so they stack without a trailing blank
 * line (the frame is drawn by the parent div's border, so an extra newline would
 * show as a gap inside the box). `borderTop` draws the internal rule that
 * separates this section from the one above it.
 */
function PaletteSection({ rows, borderTop }: { rows: Segment[][]; borderTop?: boolean }) {
  return (
    <pre
      className="m-0 whitespace-pre font-mono"
      style={{
        ...PRE_STYLE,
        // Inherit the box's PAL_BG (segments still paint their own SEL_BG fill).
        background: "transparent",
        borderTop: borderTop ? `1px solid ${PAL_BORDER}` : undefined,
      }}
    >
      {rows.map((row, i) => (
        <span key={i} style={{ display: "block" }}>
          <SegSpans segs={row} />
        </span>
      ))}
    </pre>
  )
}

/**
 * The centered ⌃P command-palette, drawn over the terminal body so it reads like
 * a box-drawing frame on the console — not a modal card floating above it.
 * Non-interactive — the hero drives its query via keyframes.
 *
 * There's no scrim, fill lift, or drop shadow: the box's interior is the same
 * `TUI_BG` as the terminal body and the board underneath is left undimmed, so the
 * console background is continuous under and around the thin `TUI_DIVIDER` border
 * — the way a box-drawing glyph is just a stroke inside a cell that otherwise
 * shows the same background. The frame is a CSS `border` (clean corners, no `│ ─`
 * junction jank); the internal rules between the query header, the command list,
 * and the footer hints are `border-top`s on the sections. The rows keep the
 * monospace grid for their content (icon + label + description).
 */
function PaletteOverlay({ palette, project }: { palette: PaletteState; project: string }) {
  const { header, list, footer } = buildPaletteSections(palette, project)
  return (
    <div
      className="absolute inset-0 flex justify-center"
      // Top-anchored so the box's top edge stays put while the list shrinks from
      // the bottom as the query narrows it — the way a real fuzzy picker filters.
      // No scrim: the board stays at full console brightness on both sides of the
      // border so the terminal background bleeds continuously around the frame.
      style={{ alignItems: "flex-start", background: "transparent", pointerEvents: "none" }}
    >
      <div
        style={{
          marginTop: 48,
          // Interior = terminal body, so the same background reads on both sides
          // of the thin border and the box doesn't lift off the console as a card.
          background: PAL_BG,
          border: `1px solid ${PAL_BORDER}`,
          // A tight radius keeps the corners clean (like a rounded box-drawing
          // junction) without the card-like softness of a large modal radius.
          borderRadius: 3,
          // Clip the section fills (and the internal rules) to the rounded corners.
          overflow: "hidden",
          minWidth: "max-content",
        }}
      >
        <PaletteSection rows={header} />
        <PaletteSection rows={list} borderTop />
        <PaletteSection rows={footer} borderTop />
      </div>
    </div>
  )
}

/**
 * The macOS-Terminal window chrome — rounded frame, drop shadow, and the title
 * bar with its three traffic lights — around an arbitrary dark terminal body.
 * `AppMockup` wraps the full sidebar+board dashboard in it; the compact homepage
 * value-prop fragments (`ScopedTaskMockup` and friends) wrap a single small
 * `<pre>` in the same frame, so a focused illustration reads as a capture from
 * the same terminal as the hero without re-implementing any chrome. `bodyClassName`
 * styles the body wrapper — the dashboard passes `relative flex …` so the sidebar
 * and board sit side by side under the palette overlay; a fragment leaves the
 * default so its single pane just scrolls if it ever overflows.
 */
export function TerminalFrame({
  title,
  bodyClassName = "overflow-x-auto",
  children,
}: {
  title: string
  bodyClassName?: string
  children: React.ReactNode
}) {
  return (
    <div
      className="overflow-hidden rounded-lg shadow-2xl"
      style={{ boxShadow: "0 24px 48px rgba(0,0,0,0.35), 0 0 0 1px rgba(0,0,0,0.4)" }}
    >
      {/* macOS Terminal title bar — traffic lights left, title centered. */}
      <div
        className="relative flex items-center justify-center border-b px-3"
        style={{ background: CHROME_BAR_BG, borderColor: CHROME_BAR_BORDER, height: 28 }}
      >
        <div
          className="absolute flex items-center"
          // Equal top/left inset so the red light is equidistant from the top
          // and left edges — tucked into the corner. 8px keeps it inside the
          // 28px bar (8 + 12 + 8 = 28) with no height change.
          style={{ top: 8, left: 8, gap: 8 }}
        >
          <TrafficLight color={TRAFFIC_RED} />
          <TrafficLight color={TRAFFIC_YELLOW} />
          <TrafficLight color={TRAFFIC_GREEN} />
        </div>
        <span className="font-mono text-xs font-medium" style={{ color: CHROME_TITLE }}>
          {title}
        </span>
      </div>
      <div className={bodyClassName} style={{ background: TUI_BG }}>
        {children}
      </div>
    </div>
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
 * import the preset objects but can drop a tag and override a field. Every
 * field you don't pass comes from the base, so a scenario that replaces the
 * columns should also set `readyReview`, `queuedReview`, `zenMode` and
 * `workflow` to match them. Props that aren't `AppState` fields (an old
 * `review={[]}`, say) are ignored. The board lays out at `DEFAULT_BOARD_W`
 * cells unless the scenario sets `boardWidth`.
 */
export function AppMockup({
  state,
  preset = "default",
  contentOpacity,
  ...overrides
}: {
  state?: AppState
  preset?: PresetName
  /**
   * Opacity applied to the terminal's inner content (the sidebar + pane), with a
   * short transition — the hero animation drives this to cross-fade the pane
   * contents at the loop boundary while the window chrome (frame, title bar,
   * traffic lights) stays fully visible and static. Omitted (the docs default)
   * leaves the content fully opaque.
   */
  contentOpacity?: number
} & Partial<AppState>) {
  const base = state ?? PRESETS[preset]
  const merged: AppState = { boardWidth: DEFAULT_BOARD_W, ...base, ...overrides }
  return (
    <section className="border-b border-gray-4 px-3 py-6 sm:py-10">
      {/* w-fit hugs the board's natural width; max-w-full keeps the
          inner overflow-x-auto in charge on viewports narrower than
          the board. */}
      <div className="mx-auto w-fit max-w-full">
        <DashboardFrame merged={merged} contentOpacity={contentOpacity} />
      </div>
    </section>
  )
}

/**
 * The stateful sidebar + board dashboard inside a macOS-Terminal frame, with no
 * outer `<section>` wrapper. Owns the live `activeView` — seeded from the
 * scenario so first paint matches the preset exactly (the hero stays on Tasks),
 * driven by clicking the sidebar nav, and re-synced when the scenario's own
 * `activeView` changes so a timeline that mutates the state over time (the hero
 * animation) drives the pane too. The sync uses React's "adjust state while
 * rendering" pattern (compare the scenario's view to the last one we saw, reset
 * on change) rather than an effect, so a scenario change is reflected in the same
 * render with no extra commit — and a within-scenario nav click still wins until
 * the next change.
 *
 * Both `AppMockup` (centered in its own section) and `BoardMockup` (bare, for
 * embeds that bleed past the page edge) render this, so there is one dashboard
 * implementation the two can't drift apart from. `bodyClassName` is forwarded to
 * `TerminalFrame`: the two `<pre>` blocks (sidebar + board) sit side by side in
 * one dark surface so the panel reads as a single captured terminal, and an
 * embed can drop the default `overflow-x-auto` when it means the board to bleed
 * rather than scroll inside a clipped box. The solid terminal-body fill stays
 * fully opaque; only the two panes' text layers cross-fade at the loop boundary
 * via `contentOpacity` (each pane's own background is this same `TUI_BG`, so
 * fading a pane dissolves its glyphs while the background reads opaque).
 */
function DashboardFrame({
  merged,
  contentOpacity,
  bodyClassName = "relative flex overflow-x-auto",
}: {
  merged: AppState
  contentOpacity?: number
  bodyClassName?: string
}) {
  const [activeView, setActiveView] = useState<NavView>(merged.activeView)
  const [seenView, setSeenView] = useState<NavView>(merged.activeView)
  if (merged.activeView !== seenView) {
    setSeenView(merged.activeView)
    setActiveView(merged.activeView)
  }
  const resolved: AppState = { ...merged, activeView }
  return (
    <TerminalFrame title={resolved.terminalTitle} bodyClassName={bodyClassName}>
      <Sidebar state={resolved} onSelectView={setActiveView} contentOpacity={contentOpacity} />
      <TerminalBody state={resolved} contentOpacity={contentOpacity} />
      {resolved.palette ? (
        <PaletteOverlay palette={resolved.palette} project={resolved.project} />
      ) : null}
    </TerminalFrame>
  )
}

/**
 * The full dashboard board (sidebar + kanban) in its terminal frame, with no
 * outer section wrapper — for embedding beside copy where the board is meant to
 * bleed off the page edge instead of sitting centered in its own section. Takes
 * the same `state` / `preset` / field-override props as `AppMockup` and defaults
 * to the busy `defaultAppState` board. Uses `relative flex` (no `overflow-x-auto`)
 * so the frame sizes to the board's full width and bleeds past the container edge
 * rather than scrolling inside a clipped box — the embedding layout is
 * responsible for clipping the overflow at the page edge.
 */
export function BoardMockup({
  state,
  preset = "default",
  ...overrides
}: {
  state?: AppState
  preset?: PresetName
} & Partial<AppState>) {
  const base = state ?? PRESETS[preset]
  const merged: AppState = { ...base, ...overrides }
  return <DashboardFrame merged={merged} bodyClassName="relative flex" />
}

// ── Presets ───────────────────────────────────────────────────────────
// Named starting points a doc page (or the landing page) imports and
// tweaks with a spread, so each scenario is a diff from a full board
// rather than a hand-built one. Add more as docs need them.

/**
 * A busy project on the shipped `task` workflow: the six-column board (the
 * empty CANCELED column folded to a strip) with a selected review card, two
 * machines of dev workspaces, and both review sections populated — issues
 * Ready for Review (served on a `review`-tagged workspace, which the sidebar
 * lists only there) and Queued for Review (waiting for a free slot). The docs'
 * default preset; `KanbanMockup` renders it verbatim.
 */
export const defaultAppState: AppState = {
  terminalTitle: "you@hub — my-project",
  project: "my-project",
  activeView: "tasks",
  // A fresh project ships two workflows (`task` and `subtask`), so cards carry
  // the workflow badge and the board shows the `Workflow:` chip.
  workflow: "task",
  zenMode: false,
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
        { title: "Cold-start cache", id: "t-011", workspace: "review-0", selected: true },
        { title: "CSV import fix", id: "t-018", workspace: "review-1" },
        { title: "Nightly report", id: "t-024", workspace: "review-2" },
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
    { label: "CANCELED", category: "darkgray", cards: [], collapsed: true },
  ],
  // The sidebar's dev workspaces: the four on an IN PROGRESS card are working
  // (Developer); the rest are idle. The three review slots (`review-0..2`) are
  // `review`-tagged, so they show only through the review sections below.
  machines: [
    {
      name: "hub",
      workspaces: [
        { name: "alpha", state: "working", agent: "developer" },
        { name: "bravo", state: "working", agent: "developer" },
        { name: "charlie", state: "idle" },
        { name: "delta", state: "idle" },
      ],
    },
    {
      name: "devbox",
      workspaces: [
        { name: "echo", state: "working", agent: "developer" },
        { name: "foxtrot", state: "working", agent: "developer" },
        { name: "golf", state: "idle" },
      ],
    },
  ],
  // Ready = the three served REVIEW cards, each loaded on a review workspace
  // (its `machine:workspace` drops beside a title too long to leave room).
  // Queued = the two Review cards still waiting, because all three review
  // workspaces are busy.
  readyReview: [
    { title: "Cold-start cache", branch: "you/t-011", location: "hub:review-0" },
    { title: "CSV import fix", branch: "you/t-018", location: "devbox:review-1" },
    { title: "Nightly report", branch: "you/t-024", location: "devbox:review-2" },
  ],
  queuedReview: [
    { title: "Validate webhook payloads", branch: "you/t-023" },
    { title: "Harden token refresh", branch: "you/t-025" },
  ],
}

/**
 * A freshly-initialized project: the hub machine, one idle workspace, a couple
 * of backlog cards and nothing else moving yet — the shape a getting-started
 * doc wants when it walks through the first task. The empty columns fold to
 * strips, as they do in the TUI. Spread and tweak it
 * (`{ ...starterAppState, activeView: "chat" }`) to show a specific
 * early-onboarding state.
 */
export const starterAppState: AppState = {
  terminalTitle: "you@hub — myproject",
  project: "myproject",
  activeView: "tasks",
  workflow: "task",
  columns: [
    {
      label: "BACKLOG",
      category: "gray",
      cards: [
        { title: "Add a health check", id: "add-a-health-check" },
        { title: "Write the README", id: "write-the-readme" },
      ],
    },
    { label: "TO DO", category: "blue", cards: [], collapsed: true },
    { label: "IN PROGRESS", category: "yellow", cards: [], collapsed: true },
    { label: "REVIEW", category: "magenta", cards: [], collapsed: true },
    { label: "DONE", category: "green", cards: [], collapsed: true },
    { label: "CANCELED", category: "darkgray", cards: [], collapsed: true },
  ],
  machines: [
    {
      name: "hub",
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

// ── Value-prop fragments ──────────────────────────────────────────────
// Compact, single-pane illustrations for the homepage value-prop sections.
// Each is a focused fragment (a scoped task, a workspace roster, a workflow
// strip) rather than the full dashboard — small enough to sit beside copy
// without dominating it — but built from the same palette, the same `Segment`
// rows, and the same `Row` renderer inside the same `TerminalFrame` chrome as
// the hero's `AppMockup`, so they read as captures from the same terminal.

// Fixed, sensible fragment width: wide enough for the workflow stage strip
// (`BACKLOG → … → DONE`), narrow enough to stay a supporting illustration.
const FRAG_W = 50

/**
 * A fragment's single terminal pane: its `Segment` rows rendered through the
 * same `Row` engine as the dashboard panes, pinned to `FRAG_W` cells with a
 * little breathing room so the text isn't flush against the frame.
 */
function FragmentPane({ rows }: { rows: Segment[][] }) {
  return (
    <pre
      className="m-0 whitespace-pre font-mono"
      style={{
        ...PRE_STYLE,
        width: `${FRAG_W}ch`,
        minWidth: `${FRAG_W}ch`,
        maxWidth: `${FRAG_W}ch`,
        padding: "12px 14px",
        overflow: "hidden",
      }}
    >
      {rows.map((row, i) => (
        <Row key={i} segs={row} />
      ))}
    </pre>
  )
}

/**
 * Fragment 1 — "Tasks keep work focused." A lazy one-liner at the `❯` prompt
 * becoming a scoped task: a titled card with a couple of scope lines, the way
 * the orchestrator turns an offhand request into focused work before an agent
 * touches it.
 */
function scopedTaskRows(): Segment[][] {
  const rows: Segment[][] = []
  const scope = (text: string) =>
    rows.push(
      bodyRow(
        [
          { text: "   ◇ ", color: TUI_MAGENTA },
          { text, color: TUI_GRAY },
        ],
        FRAG_W,
      ),
    )

  rows.push(
    bodyRow(
      [
        { text: " ❯ ", color: TUI_CYAN, bold: true },
        { text: "ship the csv export thing", color: TUI_FG },
      ],
      FRAG_W,
    ),
  )
  rows.push(blankRow(FRAG_W))
  rows.push(
    bodyRow(
      [
        { text: " ⏺ ", color: TUI_GREEN },
        { text: "Scoped and added to the backlog:", color: TUI_FG },
      ],
      FRAG_W,
    ),
  )
  rows.push(blankRow(FRAG_W))
  rows.push(
    bodyRow([{ text: "   Export orders to CSV", color: TUI_FG, bold: true }], FRAG_W),
  )
  rows.push(bodyRow([{ text: "   export-orders-csv", color: TUI_DARK_GRAY }], FRAG_W))
  rows.push(blankRow(FRAG_W))
  scope("GET /account/orders.csv, streamed")
  scope("Columns: date, sku, qty, total")
  scope("Done when it handles 10k+ rows")
  return rows
}

/**
 * Fragment 2 — "Agents provide specialization." The workspace roster with each
 * workspace's agent right of its name: developers execute while reviewers, QA,
 * and security scrutinize, each doing one job on the work that reaches it.
 */
function specializationRows(): Segment[][] {
  const rows: Segment[][] = []
  const NAME_W = 10
  const workspace = (name: string, agent: string, agentColor: string) =>
    rows.push(
      bodyRow(
        [
          { text: " ⏵ ", color: TUI_GREEN },
          { text: padTo(name, NAME_W), color: TUI_GRAY },
          { text: agent, color: agentColor },
        ],
        FRAG_W,
      ),
    )

  rows.push(bodyRow([{ text: " — Workspaces —", color: TUI_DARK_GRAY }], FRAG_W))
  rows.push(blankRow(FRAG_W))
  workspace("alpha", "Developer", TUI_FG)
  workspace("bravo", "Developer", TUI_FG)
  workspace("charlie", "Reviewer", TUI_MAGENTA)
  workspace("delta", "QA", TUI_BLUE)
  workspace("echo", "Security", TUI_CYAN)
  return rows
}

/**
 * Fragment 3 — "Workflows provide boundaries." The default workflow's real
 * stages as a pipeline strip with the gate at REVIEW, then Zen Mode reporting
 * what it merged and what it held for you — boundaries that make autonomy safe.
 */
function workflowRows(): Segment[][] {
  const rows: Segment[][] = []
  const arrow: Segment = { text: " → ", color: TUI_DARK_GRAY }

  rows.push(
    bodyRow(
      [
        { text: " " },
        { text: "BACKLOG", color: TUI_GRAY, bold: true },
        arrow,
        { text: "TO DO", color: TUI_BLUE, bold: true },
        arrow,
        { text: "IN PROGRESS", color: TUI_YELLOW, bold: true },
        arrow,
        { text: "REVIEW", color: TUI_MAGENTA, bold: true },
        arrow,
        { text: "DONE", color: TUI_GREEN, bold: true },
      ],
      FRAG_W,
    ),
  )
  rows.push(blankRow(FRAG_W))
  rows.push(
    bodyRow(
      [
        { text: " gate at ", color: TUI_DARK_GRAY },
        { text: "REVIEW", color: TUI_MAGENTA },
        { text: ": checks green · no danger paths", color: TUI_DARK_GRAY },
      ],
      FRAG_W,
    ),
  )
  rows.push(blankRow(FRAG_W))
  rows.push(
    bodyRow(
      [
        { text: " ⏵⏵ ", color: TUI_GREEN },
        { text: "zen mode on", color: TUI_DARK_GRAY },
      ],
      FRAG_W,
    ),
  )
  rows.push(
    bodyRow(
      [
        { text: "  ⏺ ", color: TUI_GREEN },
        { text: "merged ", color: TUI_FG },
        { text: quoted("Cold-start cache"), color: TUI_FG },
        { text: "  · checks green", color: TUI_DARK_GRAY },
      ],
      FRAG_W,
    ),
  )
  rows.push(
    bodyRow(
      [
        { text: "  ⎿ ", color: TUI_DARK_GRAY },
        { text: "held ", color: TUI_FG },
        { text: quoted("Harden token refresh"), color: TUI_FG },
        { text: "  · needs you", color: TUI_DARK_GRAY },
      ],
      FRAG_W,
    ),
  )
  return rows
}

/** "Tasks keep work focused" — a lazy request becoming a scoped task. */
export function ScopedTaskMockup() {
  return (
    <TerminalFrame title="shelbi · task">
      <FragmentPane rows={scopedTaskRows()} />
    </TerminalFrame>
  )
}

/** "Agents provide specialization" — the workspace roster, one agent per job. */
export function SpecializationMockup() {
  return (
    <TerminalFrame title="shelbi · workspaces">
      <FragmentPane rows={specializationRows()} />
    </TerminalFrame>
  )
}

/** "Workflows provide boundaries" — the default stages, the gate, the Zen report. */
export function WorkflowMockup() {
  return (
    <TerminalFrame title="shelbi · workflow">
      <FragmentPane rows={workflowRows()} />
    </TerminalFrame>
  )
}

// ── Editor (vim) fragments ────────────────────────────────────────────
// "Specialization / boundaries" examples rendered as files open in vim: an
// agent's brief (`instructions.md`) and the workflow definition (`workflow.yml`).
// They make the point that an agent's job and a task's path are editable files,
// not baked in — built from the same Segment/Row engine, palette, and terminal
// frame as the other mockups. Unlike the fixed-width fragment panes, the vim
// window is laid out fluid: a full-width buffer `<pre>` with a real full-width
// status bar, so it fills whatever width its container gives it (the landing
// page bleeds it edge to edge) and stands `VIM_TOTAL_PX` tall — the same height
// as the animated board beside it.

const VIM_TOTAL_PX = 640 // the window stands this tall (title bar + body)
const VIM_TITLE_PX = 28 // TerminalFrame title-bar height
const VIM_LINE_PX = 17 // matches PRE_STYLE line-height
const VIM_GUTTER = 4 // 3-cell right-aligned line number + 1 trailing space
// Enough `~` end-of-buffer rows that the buffer always fills its flex area (any
// extra clip); the file's real lines sit above them.
const VIM_BUFFER_FILL = 40

/** One rendered line of the buffer, with light syntax coloring (no gutter). */
type VimLine = Segment[]
const VIM_BLANK: VimLine = []

// The wrap width the prose/bullets are folded to. Comfortably inside the window
// at its common column widths; longer lines just clip like any vim buffer.
const VIM_WRAP = 54

/** A block of the source markdown doc, before it's wrapped into buffer lines. */
type DocBlock =
  | { k: "h"; t: string } // heading (`#`/`##`) — cyan bold
  | { k: "p"; t: string } // paragraph — wrapped, fg
  | { k: "li"; t: string } // bullet — wrapped, hanging indent, bold lead term
  | { k: "blank" }

// The real Adversarial Review agent instructions.md, verbatim from
// site/content/docs/guides/doing-more-with-agents/adversarial-review-agent.mdx —
// the sample role prompt a reader pastes to get a genuine skeptic. Only the top
// of the file shows in the window; the rest scrolls off, like any real prompt.
const REVIEW_DOC: DocBlock[] = [
  { k: "h", t: "# Adversarial Review" },
  { k: "blank" },
  {
    k: "p",
    t: "You are an adversarial code reviewer. A developer agent has finished a task and believes its branch is ready. Your job is not to confirm that belief — it is to try to prove the change is wrong before a human spends attention on it. Assume there is a bug until you have looked hard enough to say otherwise.",
  },
  { k: "blank" },
  { k: "h", t: "## What to review" },
  { k: "blank" },
  {
    k: "p",
    t: "Review the diff on the current task's branch against the base branch — only what this task changed, plus the code that change touches. You are looking for defects the developer and the tests missed, not for style nits the preamble's checks already cover.",
  },
  { k: "blank" },
  { k: "h", t: "## How to review" },
  { k: "blank" },
  { k: "p", t: "Work through every changed hunk and actively try to falsify it:" },
  { k: "blank" },
  {
    k: "li",
    t: "Correctness — Does it do what the task asked? Trace the non-obvious paths by hand. Off-by-one, wrong operator, inverted condition, a branch that silently does nothing.",
  },
  {
    k: "li",
    t: "Security — Untrusted input reaching a query, a shell, a path, or a template. Missing authz checks. Secrets in logs or errors. Unsafe defaults.",
  },
  {
    k: "li",
    t: "Error handling — What happens when the call fails, the input is empty, the list is huge, the value is null, two requests race?",
  },
  {
    k: "li",
    t: "Edge cases — Boundaries, empty and maximal inputs, concurrency, encoding, time zones — whatever this code plausibly meets in production.",
  },
  {
    k: "li",
    t: "Test coverage — Do the tests actually exercise the new behavior, or do they assert around it? Find a real input the change gets wrong that no test would catch, and treat that as a finding.",
  },
  { k: "blank" },
  { k: "h", t: "## How to report" },
  { k: "blank" },
  {
    k: "p",
    t: "Write your findings as a structured review on the branch — a markdown ## Adversarial Review section, per this project's convention. For each finding:",
  },
  { k: "blank" },
  { k: "li", t: "Severity — blocker, major, or minor." },
  { k: "li", t: "Location — path/to/file.rs:120 (or a range)." },
  {
    k: "li",
    t: "The problem — what breaks, and the concrete input or sequence that triggers it. A finding with a repro beats a vague worry.",
  },
  { k: "li", t: "Suggested fix — one line, when the fix is obvious." },
  { k: "blank" },
  { k: "p", t: "Order findings by severity, blockers first." },
  { k: "blank" },
  { k: "h", t: "## Signing off or bouncing" },
  { k: "blank" },
  {
    k: "p",
    t: "Every review ends one of two ways. Decide from the highest-severity finding. If nothing rises to a blocker, sign off and hand off forward. If any finding is a blocker, write up the findings and bounce the task back to the developer.",
  },
]

/**
 * Wrap the source doc into colored buffer lines: headings cyan-bold, paragraphs
 * wrapped in the foreground color, bullets wrapped with a hanging indent and the
 * lead term (before the ` — `) bolded — the light markdown rendering the other
 * vim buffer uses, applied to the real doc.
 */
function docToVimLines(blocks: DocBlock[]): VimLine[] {
  const out: VimLine[] = []
  for (const b of blocks) {
    if (b.k === "blank") {
      out.push(VIM_BLANK)
    } else if (b.k === "h") {
      out.push([{ text: b.t, color: TUI_CYAN, bold: true }])
    } else if (b.k === "p") {
      for (const line of wrapText(b.t, VIM_WRAP)) out.push([{ text: line, color: TUI_FG }])
    } else {
      wrapText(b.t, VIM_WRAP - 2).forEach((line, i) => {
        if (i > 0) {
          out.push([{ text: `  ${line}`, color: TUI_FG }])
          return
        }
        const dash = line.indexOf(" — ")
        out.push(
          dash > 0
            ? [
                { text: "- ", color: TUI_DARK_GRAY },
                { text: line.slice(0, dash), color: TUI_FG, bold: true },
                { text: line.slice(dash), color: TUI_FG },
              ]
            : [
                { text: "- ", color: TUI_DARK_GRAY },
                { text: line, color: TUI_FG },
              ],
        )
      })
    }
  }
  return out
}

const REVIEW_INSTRUCTIONS: VimLine[] = docToVimLines(REVIEW_DOC)

/**
 * The same Adversarial Review instructions as raw markdown source, one string
 * per paragraph-level block (`#` headings, paragraphs, `- ` bullets, and `""`
 * for a blank line): what the desktop's agent editor shows in its
 * Instructions field, where the file is plain text rather than a vim buffer.
 */
export const REVIEW_INSTRUCTIONS_SOURCE: string[] = REVIEW_DOC.map((b) =>
  b.k === "blank" ? "" : b.k === "li" ? `- ${b.t}` : b.t,
)

/**
 * The buffer rows for a file: a right-aligned line-number gutter beside each
 * line, then `~` end-of-buffer rows filling down. Left-aligned and unpadded —
 * the pane is fluid, so the rest of each line is just terminal background, the
 * way vim leaves it.
 */
function buildVimBuffer(lines: VimLine[]): Segment[][] {
  const rows: Segment[][] = lines.map((line, i) => [
    { text: `${i + 1}`.padStart(VIM_GUTTER - 1) + " ", color: TUI_DARK_GRAY },
    ...line,
  ])
  while (rows.length < VIM_BUFFER_FILL) rows.push([{ text: "~", color: TUI_BLUE }])
  return rows
}

// Neo-tree-style file explorer for the vim window's left split: expander chevrons
// (▾ open / ▸ closed) on directories, real Seti-UI file-type icons, and git-status
// symbols RIGHT-ALIGNED at the panel edge (modified `M` yellow, added `A` green) —
// the neo-tree "right-aligned symbols" look. The root is cyan, directories
// blue-bold, files foreground, the open file gets a full-row selection bar.
const NERD_W = 30
const NERD_AGENTS = ["adversarial", "developer", "orchestrator", "qa", "review", "security"]

type NeoNode = {
  depth: number
  name: string
  dir: boolean
  expanded?: boolean
  git?: "M" | "A"
  active?: boolean
}

// File icons from Seti-UI (github.com/elviswolcott/seti-icons), inlined as SVG
// strings — the package's recommended approach for static sites — so the sidebar
// shows real file-type icons without bundling its ~100K icon set. seti-icons
// ships no folder icon, so directories use the Octicons `file-directory-fill`
// glyph (a common neo-tree folder choice). Icon tints are mapped from Seti's
// color keywords to the terminal palette.
const SETI_MD =
  '<svg viewBox="0 0 32 32"><path d="M20.7 6.7v9.9h3.8c-2.9 3-5.8 5.9-8.7 8.8-2.7-2.8-5.6-5.8-8.4-8.7h3.5V6.6c1.3.9 4.4 3.1 5 3.1.6 0 3.6-2.2 4.8-3z"/></svg>'
const SETI_JSON =
  '<svg viewBox="0 0 32 32"><path d="M7.5 15.1c1.5 0 1.7-.8 1.7-1.5 0-.6-.1-1.1-.1-1.7S9 10.7 9 10.2c0-2.1 1.3-3 3.4-3h.8v1.9h-.4c-1 0-1.3.6-1.3 1.6 0 .4.1.8.1 1.3 0 .4.1.9.1 1.5 0 1.7-.7 2.3-1.9 2.6 1.2.3 1.9.9 1.9 2.6 0 .6-.1 1.1-.1 1.5 0 .4-.1.9-.1 1.2 0 1 .3 1.6 1.3 1.6h.4v1.9h-.8c-2 0-3.3-.8-3.3-3 0-.6 0-1.1.1-1.7.1-.6.1-1.2.1-1.7 0-.6-.2-1.5-1.7-1.5l-.1-1.9zm17 1.7c-1.5 0-1.7.9-1.7 1.5s.1 1.1.1 1.7c.1.6.1 1.2.1 1.7 0 2.2-1.4 3-3.4 3h-.8V23h.4c1 0 1.3-.6 1.3-1.6 0-.4 0-.8-.1-1.2 0-.5-.1-1-.1-1.5 0-1.7.7-2.3 1.9-2.6-1.2-.3-1.9-.9-1.9-2.6 0-.6.1-1.1.1-1.5.1-.5.1-.9.1-1.3 0-1-.4-1.5-1.3-1.6h-.4V7.2h.8c2.1 0 3.4.9 3.4 3 0 .6-.1 1.1-.1 1.7-.1.6-.1 1.2-.1 1.7 0 .7.2 1.5 1.7 1.5v1.7z"/></svg>'
const SETI_DEFAULT =
  '<svg viewBox="0 0 1200 1000"><path d="M394.1 537.8h411.7v54.7H394.1v-54.7zm0-130.3H624v54.7H394.1v-54.7zm0-130.3h411.7v54.7H394.1v-54.7zm0 390.9H700v54.7H394.1v-54.7z"/></svg>'
// Octicons file-directory-fill (github.com/primer/octicons), MIT-licensed.
const FOLDER_SVG =
  '<svg viewBox="0 0 16 16"><path d="M1.75 1A1.75 1.75 0 0 0 0 2.75v10.5C0 14.216.784 15 1.75 15h12.5A1.75 1.75 0 0 0 16 13.25v-8.5A1.75 1.75 0 0 0 14.25 3H7.5a.25.25 0 0 1-.2-.1l-.9-1.2C6.07 1.26 5.55 1 5 1H1.75Z"/></svg>'

type IconSpec = { svg: string; color: string }
function iconFor(node: NeoNode): IconSpec {
  if (node.dir) return { svg: FOLDER_SVG, color: TUI_BLUE }
  if (node.name.endsWith(".md")) return { svg: SETI_MD, color: TUI_BLUE }
  if (node.name.endsWith(".json")) return { svg: SETI_JSON, color: TUI_YELLOW }
  return { svg: SETI_DEFAULT, color: TUI_FG }
}

/** An inline Seti SVG icon, sized to the row and tinted via `currentColor`. */
function SetiIcon({ spec }: { spec: IconSpec }) {
  return (
    <span
      aria-hidden="true"
      style={{ display: "inline-flex", flex: "none", width: 14, height: 14, marginRight: "0.4ch", color: spec.color }}
      dangerouslySetInnerHTML={{
        __html: spec.svg.replace("<svg ", '<svg style="width:100%;height:100%;fill:currentColor" '),
      }}
    />
  )
}

/** One neo-tree row: indent + chevron + Seti icon + name, git symbol right-aligned. */
function NeoRow({ node }: { node: NeoNode }) {
  const bg = node.active ? SEL_BG : undefined
  const indent = " " + "  ".repeat(node.depth) // 1-col panel margin + per-depth indent
  const marker = node.dir ? (node.expanded ? "▾ " : "▸ ") : "  "
  const nameColor = node.active
    ? SEL_FG
    : node.depth === 0
      ? TUI_CYAN
      : node.dir
        ? TUI_BLUE
        : TUI_FG
  return (
    <div
      className="flex items-center whitespace-pre"
      style={{ height: VIM_LINE_PX, background: bg, paddingRight: "1ch" }}
    >
      <span style={{ color: TUI_DARK_GRAY }}>{indent + marker}</span>
      <SetiIcon spec={iconFor(node)} />
      <span style={{ color: nameColor, fontWeight: node.dir ? 700 : 400 }}>{node.name}</span>
      {node.git ? (
        <span
          style={{
            marginLeft: "auto",
            color: node.active ? SEL_FG : node.git === "A" ? TUI_GREEN : TUI_YELLOW,
            fontWeight: 700,
          }}
        >
          {node.git}
        </span>
      ) : null}
    </div>
  )
}

/** The neo-tree sidebar panel: a fixed-width, bordered column of rows. */
function NeoTreeSidebar({ nodes }: { nodes: NeoNode[] }) {
  return (
    <div
      className="hidden shrink-0 overflow-hidden border-r font-mono md:block"
      style={{
        width: `${NERD_W}ch`,
        minWidth: `${NERD_W}ch`,
        background: TUI_BG,
        borderColor: TUI_DIVIDER,
        fontSize: 13,
        lineHeight: `${VIM_LINE_PX}px`,
      }}
    >
      {nodes.map((node, i) => (
        <NeoRow key={i} node={node} />
      ))}
    </div>
  )
}

/**
 * Build the `agents/` tree nodes, highlighting `<activeDir>/<activeFile>` and
 * overlaying a believable git status: the shipped `adversarial` reviewer being
 * customized shows modified (`M`). All six presets ship, so nothing is a fresh
 * git-add.
 */
function agentsTreeNodes(activeDir: string, activeFile: string): NeoNode[] {
  const nodes: NeoNode[] = [{ depth: 0, name: "agents", dir: true, expanded: true }]
  for (const agent of NERD_AGENTS) {
    // All six presets ship, so none is a fresh git-add; the open file is the
    // shipped `adversarial` preset being customized, shown as git-modified.
    const modified = agent === "adversarial"
    const dirGit = modified ? "M" : undefined
    nodes.push({ depth: 1, name: agent, dir: true, expanded: true, git: dirGit })
    nodes.push({
      depth: 2,
      name: "instructions.md",
      dir: false,
      git: modified ? "M" : undefined,
      active: agent === activeDir && activeFile === "instructions.md",
    })
    nodes.push({ depth: 2, name: "settings.json", dir: false })
    nodes.push({ depth: 2, name: "skills", dir: true, expanded: false })
  }
  return nodes
}

/**
 * A full vim editor window: a fluid-width buffer `<pre>` that fills whatever
 * width its container gives it, an optional NERDTree-style file-explorer sidebar
 * split on the left, a reverse-video status line (filename left, ruler right),
 * and the file-open message on the command line — the whole window
 * `VIM_TOTAL_PX` tall, with the split row flexing to fill whatever the two
 * bottom lines leave. The sidebar hides below `md` so a narrow window isn't
 * cramped.
 */
function VimWindow({
  lines,
  filename,
  sidebar,
}: {
  lines: VimLine[]
  filename: string
  sidebar?: React.ReactNode
}) {
  const bytes = lines.reduce(
    (n, l) => n + l.reduce((m, s) => m + [...s.text].length, 0) + 1,
    0,
  )
  const mono: React.CSSProperties = { fontSize: 13, lineHeight: `${VIM_LINE_PX}px`, height: VIM_LINE_PX }
  return (
    <div className="w-full">
      <TerminalFrame title={`jlong@hub — nvim ${filename}`}>
        <div className="flex flex-col" style={{ height: VIM_TOTAL_PX - VIM_TITLE_PX }}>
          <div className="flex flex-1 overflow-hidden">
            {sidebar}
            <pre
              className="m-0 flex-1 overflow-hidden whitespace-pre font-mono"
              style={{ ...PRE_STYLE, width: "100%", minWidth: 0 }}
            >
              {buildVimBuffer(lines).map((row, i) => (
                <Row key={i} segs={row} />
              ))}
            </pre>
          </div>
          {/* Reverse-video status line, full width: filename left, ruler right. */}
          <div
            className="flex shrink-0 items-center justify-between whitespace-pre font-mono"
            style={{
              ...mono,
              background: TUI_GRAY,
              color: TUI_BG,
              paddingLeft: "1ch",
              paddingRight: "1ch",
            }}
          >
            <span style={{ fontWeight: 700 }}>{filename}</span>
            <span>1,1&nbsp;&nbsp;&nbsp;&nbsp;All</span>
          </div>
          {/* Command line: the message vim prints when the file opens. */}
          <div
            className="shrink-0 whitespace-pre font-mono"
            style={{ ...mono, color: TUI_DARK_GRAY, paddingLeft: "1ch" }}
          >
            {`"${filename}" ${lines.length}L, ${bytes}B`}
          </div>
        </div>
      </TerminalFrame>
    </div>
  )
}

/** "Agents provide specialization" — an agent's instructions.md open in vim. */
export function AgentInstructionsMockup() {
  return (
    <VimWindow
      lines={REVIEW_INSTRUCTIONS}
      filename="instructions.md"
      sidebar={<NeoTreeSidebar nodes={agentsTreeNodes("adversarial", "instructions.md")} />}
    />
  )
}

// ── Workflow config (vim) fragment ────────────────────────────────────
// A boundaries example: a real Shelbi workflow open in vim — the `feature`
// workflow (the app-feature shape). It uses the actual schema: reference-only
// `statuses` (id + owner + optional agent), `transitions` with hub-side actions,
// and a per-workflow `git` override, matching `default_workflow_yaml` /
// `scaffold.rs` in the crate. Status ids carry their kanban category colors so
// the config ties back to the board.

const YKEY = TUI_CYAN // YAML keys
const YDIM = TUI_DARK_GRAY // comments + flow-map punctuation
const STATUS_CAT: Record<string, string> = {
  backlog: TUI_GRAY,
  todo: TUI_BLUE,
  "in-progress": TUI_YELLOW,
  "agent-review": TUI_MAGENTA,
  qa: TUI_BLUE,
  security: TUI_CYAN,
  review: TUI_MAGENTA,
  done: TUI_GREEN,
}
const STATUS_AGENT: Record<string, string> = {
  orchestrator: TUI_CYAN,
  developer: TUI_YELLOW,
  adversarial: TUI_MAGENTA,
  qa: TUI_BLUE,
  security: TUI_CYAN,
}
const catColor = (id: string): string => STATUS_CAT[id] ?? TUI_FG
const ID_W = 12 // pad ids / `from` so the following field aligns down the block

const yKey = (key: string, val?: string, valColor = TUI_FG): VimLine =>
  val === undefined
    ? [{ text: `${key}:`, color: YKEY }]
    : [
        { text: `${key}: `, color: YKEY },
        { text: val, color: valColor, bold: valColor !== TUI_FG },
      ]

/** `  - { id: <id>, owner: <owner>[, agent: <agent>] }` */
const yStatus = (id: string, owner: string, agent?: string): VimLine => {
  const segs: VimLine = [
    { text: "  - { ", color: YDIM },
    { text: "id: ", color: YKEY },
    { text: id, color: catColor(id), bold: true },
    { text: `,${" ".repeat(Math.max(1, ID_W + 1 - id.length))}`, color: YDIM },
    { text: "owner: ", color: YKEY },
    { text: owner, color: TUI_FG },
  ]
  if (agent) {
    segs.push({ text: ", ", color: YDIM })
    segs.push({ text: "agent: ", color: YKEY })
    segs.push({ text: agent, color: STATUS_AGENT[agent] ?? TUI_FG })
  }
  segs.push({ text: " }", color: YDIM })
  return segs
}

/** `  - { from: <from>, to: <to>[, actions: [<a>, ...]] }` */
const yTransition = (from: string, to: string, actions?: string[]): VimLine => {
  const segs: VimLine = [
    { text: "  - { ", color: YDIM },
    { text: "from: ", color: YKEY },
    { text: from, color: catColor(from), bold: true },
    { text: `,${" ".repeat(Math.max(1, ID_W + 1 - from.length))}`, color: YDIM },
    { text: "to: ", color: YKEY },
    { text: to, color: catColor(to), bold: true },
  ]
  if (actions) {
    segs.push({ text: ", ", color: YDIM })
    segs.push({ text: "actions: ", color: YKEY })
    segs.push({ text: "[", color: YDIM })
    actions.forEach((a, i) => {
      if (i) segs.push({ text: ", ", color: YDIM })
      segs.push({ text: a, color: TUI_GREEN })
    })
    segs.push({ text: "]", color: YDIM })
  }
  segs.push({ text: " }", color: YDIM })
  return segs
}

/** A 2-space-indented `  key: <value…>` under a mapping, with optional inline comment. */
const yGit = (key: string, value: VimLine, note?: string): VimLine => {
  const segs: VimLine = [{ text: `  ${key}: `, color: YKEY }, ...value]
  if (note) segs.push({ text: `   # ${note}`, color: YDIM })
  return segs
}

/** A folded block scalar: `key: >` then its indented, wrapped lines. */
const yFolded = (key: string, lines: string[]): VimLine[] => [
  [
    { text: `${key}: `, color: YKEY },
    { text: ">", color: YDIM },
  ],
  ...lines.map((t): VimLine => [{ text: `  ${t}`, color: TUI_FG }]),
]

// The `feature` workflow — the app-feature shape: a whole feature travels
// backlog → in-progress → agent-review → qa → security → review → done. A
// developer builds it on a branch off main; the shipped adversarial, QA, and
// security agents each gate the diff before a human signs off and it merges.
// Real Shelbi schema: reference-only `statuses`, `transitions` with hub-side
// actions, and a per-workflow `git` override.
const WORKFLOW_CONFIG: VimLine[] = [
  yKey("name", "feature"),
  ...yFolded("description", [
    "A full app feature, from idea to merged. A",
    "developer builds it on a branch off main; then",
    "adversarial review, QA, and security each gate",
    "it before a human signs off and it merges.",
  ]),
  VIM_BLANK,
  yKey("initial_status", "backlog", catColor("backlog")),
  VIM_BLANK,
  yKey("statuses"),
  yStatus("backlog", "user"),
  yStatus("in-progress", "agent", "developer"),
  yStatus("agent-review", "agent", "adversarial"),
  yStatus("qa", "agent", "qa"),
  yStatus("security", "agent", "security"),
  yStatus("review", "user"),
  yStatus("done", "user"),
  VIM_BLANK,
  yKey("transitions"),
  yTransition("backlog", "in-progress"),
  yTransition("in-progress", "agent-review", ["push_branch", "open_pr"]),
  yTransition("agent-review", "qa"),
  yTransition("qa", "security"),
  yTransition("security", "review"),
  yTransition("review", "done", ["merge", "delete_branch"]),
  VIM_BLANK,
  yKey("git"),
  yGit("base_branch", [{ text: "main", color: TUI_FG }]),
  yGit("merge_strategy", [{ text: "squash", color: TUI_FG }]),
]

/** "Workflows provide boundaries" — the feature workflow open in vim. */
export function WorkflowConfigMockup() {
  return <VimWindow lines={WORKFLOW_CONFIG} filename="feature.yaml" />
}

/**
 * Thin preset used by the marketing landing page. Renders `AppMockup` with
 * the original hardcoded scenario so the page stays visually identical.
 */
export function KanbanMockup() {
  return <AppMockup state={defaultAppState} />
}
