import type { CSSProperties, ReactNode } from "react"
import {
  ArrowLeftIcon,
  ArrowsRightLeftIcon,
  BoltIcon,
  BoltSlashIcon,
  ChatBubbleOvalLeftEllipsisIcon,
  CheckIcon,
  ChevronUpDownIcon,
  Cog6ToothIcon,
  CommandLineIcon,
  FolderIcon,
  MagnifyingGlassIcon,
  MegaphoneIcon,
  PencilSquareIcon,
  PlusIcon,
  RectangleStackIcon,
  RocketLaunchIcon,
  ServerStackIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline"
import {
  branchUser,
  ClaudeCodePane,
  type AppState,
  type Card,
  type Category,
  type ChatLine,
  type Column,
} from "../KanbanMockup"
import { FigmaIcon, SmallGlyph, StatusGlyph, type FigmaIconKind, type StatusKind } from "./DesktopIcons"

/**
 * The Shelbi desktop app (`desktop/`, gpui) redrawn in HTML for the site's
 * Desktop illustrations. It renders the same `AppState` scenarios the TUI
 * mockups take (`KanbanMockup`), so one story drives both clients, and draws
 * them the way the desktop app does. Every label, glyph, shortcut, size and
 * color here comes from `desktop/src` (and was checked against its
 * `--visual-figma` reference frames):
 *
 * - the sidebar (`views/sidebar.rs`): the 52pt titlebar with the traffic
 *   lights and the Search / Toggle sidebar buttons, the project switcher, the
 *   nav (Orchestrator ⌘O, Issues ⌘I, Changelog ⌘L), Workspaces (grouped by
 *   machine when there are several), Ready for review / Queued for review,
 *   and the footer's Settings and Send Feedback buttons over the Zen pill;
 * - the Issues board (`views/issues.rs`): the Header band (`Filter:` and the
 *   workflow and workspace chips, the freshness banner, Search, New issue),
 *   272pt columns under status headings, cards with a title, the first
 *   paragraph, the workflow badge and branch, and the key-hint footer;
 * - the Orchestrator and Workspace terminals (`views/session_view.rs`): the
 *   Claude Code session inset 16 / 12pt, in the desktop terminal's colors;
 * - the Workspace task panel in the sidebar's place (`views/task_panel.rs`);
 * - the command palette (`views/palette.rs`).
 *
 * Sizes are the app's points, scaled by the `--u` custom property (one
 * point, as a length) that each embedding sets with {@link desktopScale}, so
 * a whole window shrinks to fit its slot with pure CSS: no layout pass, no
 * hydration mismatch. Colors are the `--dt-*` variables in `app/globals.css`
 * (the app's dark and light palettes, swapped by the site's theme).
 */

// ── Units, type, color ────────────────────────────────────────────────

/** `n` desktop points, as a CSS length scaled by the embedding's `--u`. */
export const u = (n: number) => `calc(${n} * var(--u))`

/**
 * The `--u` custom property for a window `width` points wide: one point is
 * `max` px when there is room, shrinking to fit the viewport (less `gutter`
 * px) down to `min` px, below which the embedding scrolls or crops.
 */
export function desktopScale({
  width,
  max,
  min = max,
  gutter = 48,
}: {
  width: number
  max: number
  min?: number
  gutter?: number
}): CSSProperties {
  const fit = `calc((100vw - ${gutter}px) / ${width})`
  return { ["--u" as string]: min === max ? `${max}px` : `clamp(${min}px, ${fit}, ${max}px)` }
}

/** The desktop's text styles (`theme/style.rs` `Type`, Geist): size / line / weight. */
const TYPES = {
  heading: [20, 28, 700],
  base: [14, 18, 400],
  baseMedium: [14, 18, 500],
  baseSemibold: [14, 18, 600],
  small: [13, 18, 400],
  smallMedium: [13, 18, 500],
  xsmall: [12, 16, 400],
  xsmallMedium: [12, 16, 500],
  xsmallSemibold: [12, 16, 600],
} as const
export type DesktopType = keyof typeof TYPES

export function type(name: DesktopType): CSSProperties {
  const [size, line, weight] = TYPES[name]
  return { fontSize: u(size), lineHeight: u(line), fontWeight: weight }
}

/** The palette roles (`theme/palette.rs`), as `--dt-*` variables. */
export const C = {
  canvas: "var(--dt-canvas)",
  sidebar: "var(--dt-sidebar)",
  card: "var(--dt-card)",
  raised: "var(--dt-raised)",
  toolbar: "var(--dt-toolbar)",
  fill: "var(--dt-fill)",
  fillActive: "var(--dt-fill-active)",
  selection: "var(--dt-selection)",
  search: "var(--dt-search)",
  divider: "var(--dt-divider)",
  dividerStrong: "var(--dt-divider-strong)",
  inputBorder: "var(--dt-input-border)",
  fg: "var(--dt-fg)",
  loud: "var(--dt-loud)",
  muted: "var(--dt-muted)",
  icon: "var(--dt-icon)",
  buttonIcon: "var(--dt-button-icon)",
  accent: "var(--dt-accent)",
  cyan: "var(--dt-icon-cyan)",
  green: "var(--dt-icon-green)",
  yellow: "var(--dt-icon-yellow)",
  purple: "var(--dt-icon-purple)",
  success: "var(--dt-success)",
  zen: "var(--dt-zen)",
  successLabel: "var(--dt-success-label)",
  dangerFill: "var(--dt-danger-fill)",
  dangerLabel: "var(--dt-danger-label)",
  selectFill: "var(--dt-select-fill)",
  selectLabel: "var(--dt-select-label)",
  link: "var(--dt-link)",
  focusRing: "var(--dt-focus-ring)",
  focusGlow: "var(--dt-focus-glow)",
  badgeBg: "var(--dt-badge-bg)",
  badgeFg: "var(--dt-badge-fg)",
  scrim: "var(--dt-scrim)",
} as const

/** The quiet glyph color: `icon/primary` at 60% (`sidebar.rs` `quiet_glyph`). */
const QUIET = "color-mix(in srgb, var(--dt-icon) 60%, transparent)"

/** `shadow/raised card` (cards; the light nav chip). */
const RAISED_SHADOW =
  "0 4px 2px var(--dt-shadow), 0 11px 0 -8px var(--dt-shadow), 0 3px 0 -1px var(--dt-shadow), 0 0 0 1px var(--dt-shadow-ring)"
/** The keyboard's card: the 1pt focus ring and its 4pt glow. */
const FOCUS_SHADOW = `0 0 0 ${u(1)} ${C.focusRing}, 0 0 0 ${u(4)} ${C.focusGlow}`

const SANS = "var(--font-sans)"

/** A Heroicon (24/outline, drawn at 20pt as the app's are). */
type HeroIcon = typeof PlusIcon
function Hi({ icon: Icon, size = 20, color }: { icon: HeroIcon; size?: number; color?: string }) {
  return <Icon aria-hidden="true" style={{ width: u(size), height: u(size), flex: "none", color }} />
}

/** A sidebar hotkey (`style::hotkey`): each key 2pt apart, X-Small, in the icon color, 4pt either side. */
export function Hotkey({ chord }: { chord: string }) {
  const keys = [...chord]
  return (
    <span
      style={{ display: "inline-flex", flex: "none", alignItems: "center", gap: u(2), padding: `0 ${u(4)}`, ...type("xsmall"), color: C.icon }}
    >
      {keys.map((k, i) => (
        <span key={i}>{k}</span>
      ))}
    </span>
  )
}

/** A board category as the desktop draws it: its glyph and color (`style::status_glyph_for`). */
export const STATUS: Record<Category, { kind: StatusKind; color: string; panelColor: string; label: string }> = {
  gray: { kind: "backlog", color: C.icon, panelColor: C.icon, label: "backlog" },
  blue: { kind: "todo", color: C.icon, panelColor: C.icon, label: "to do" },
  // In progress is `icon/select` on the board, `icon/cyan` in the task panels.
  yellow: { kind: "in-progress", color: C.accent, panelColor: C.cyan, label: "in progress" },
  magenta: { kind: "review", color: C.purple, panelColor: C.purple, label: "review" },
  green: { kind: "done", color: C.green, panelColor: C.green, label: "done" },
  darkgray: { kind: "canceled", color: C.muted, panelColor: C.muted, label: "canceled" },
}

// ── Window ────────────────────────────────────────────────────────────

const TRAFFIC = ["#ff5f57", "#febc2e", "#28c840"]

/**
 * The window: rounded corners, the macOS traffic lights over the leading
 * column's titlebar (centered 26pt down, 20pt apart), the leading column
 * (the sidebar or a task panel), then the main area. `width` omitted fills
 * the slot (the editor mockups are fluid, as the app's layouts are).
 */
export function DesktopWindow({
  width,
  height,
  leading,
  children,
  overlay,
  label,
}: {
  width?: number
  height: number
  leading?: ReactNode
  children: ReactNode
  overlay?: ReactNode
  label: string
}) {
  return (
    <div
      role="img"
      aria-label={label}
      style={{
        position: "relative",
        display: "flex",
        width: width === undefined ? "100%" : u(width),
        height: u(height),
        borderRadius: u(12),
        overflow: "hidden",
        background: C.canvas,
        color: C.fg,
        fontFamily: SANS,
        ...type("base"),
        boxShadow: "0 24px 48px rgba(0,0,0,0.35), 0 0 0 1px var(--dt-window-edge)",
        WebkitFontSmoothing: "antialiased",
      }}
    >
      {leading}
      <div style={{ position: "relative", display: "flex", flexDirection: "column", flex: 1, minWidth: 0 }}>
        {children}
      </div>
      {overlay}
      <div aria-hidden="true" style={{ position: "absolute", top: u(20), left: u(20), display: "flex", gap: u(8) }}>
        {TRAFFIC.map((c) => (
          <span
            key={c}
            style={{ width: u(12), height: u(12), borderRadius: "50%", background: c, boxShadow: "inset 0 0 0 0.5px rgba(0,0,0,0.25)" }}
          />
        ))}
      </div>
    </div>
  )
}

/** An opacity layer for the hero's loop-boundary fade: the contents fade, the surfaces stay. */
function fadeStyle(opacity?: number): CSSProperties {
  return opacity === undefined ? {} : { opacity, transition: "opacity 500ms ease" }
}

// ── Sidebar ───────────────────────────────────────────────────────────

/** A 32pt toolbar button (the titlebar's and the footer's). */
function ToolbarButton({ children }: { children: ReactNode }) {
  return (
    <span
      style={{ display: "flex", alignItems: "center", justifyContent: "center", width: u(32), height: u(32), borderRadius: u(8), color: C.buttonIcon, flex: "none" }}
    >
      {children}
    </span>
  )
}

/** The leading column's 52pt titlebar: room for the traffic lights, then Search and Toggle sidebar. */
function Titlebar() {
  return (
    <div style={{ display: "flex", flex: "none", alignItems: "center", justifyContent: "flex-end", gap: u(4), height: u(52), paddingRight: u(8) }}>
      <ToolbarButton>
        <Hi icon={MagnifyingGlassIcon} />
      </ToolbarButton>
      <ToolbarButton>
        <FigmaIcon kind="sidebar" size={u(20)} />
      </ToolbarButton>
    </div>
  )
}

/** The footer: Settings and Send Feedback, then the Zen pill (always drawn; on or off). */
function SidebarFooter({ zen }: { zen: boolean }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", flex: "none", gap: u(8), padding: u(8) }}>
      <div style={{ display: "flex", alignItems: "center", gap: u(4) }}>
        <ToolbarButton>
          <Hi icon={Cog6ToothIcon} />
        </ToolbarButton>
        <ToolbarButton>
          <Hi icon={MegaphoneIcon} />
        </ToolbarButton>
      </div>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          height: u(24),
          borderRadius: u(8),
          background: zen ? C.zen : C.fill,
          ...type("smallMedium"),
          color: zen ? C.success : C.fg,
        }}
      >
        {zen ? "Zen mode on" : "Zen mode off"}
      </div>
    </div>
  )
}

/** The Navigation component's nav item: 34pt, the icon, the label, the hotkey; `current` wears the chip. */
export function NavItem({
  icon,
  label,
  chord,
  current,
}: {
  icon: ReactNode
  label: string
  chord?: string
  current?: boolean
}) {
  return (
    <div
      style={{
        display: "flex",
        flex: "none",
        alignItems: "center",
        gap: u(6),
        padding: `0 ${u(6)}`,
        height: u(34),
        borderRadius: u(6),
        background: current ? C.canvas : undefined,
        boxShadow: current ? "var(--dt-chip-shadow)" : undefined,
        color: current ? C.loud : C.fg,
        fontWeight: current ? 500 : 400,
      }}
    >
      {icon}
      <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{label}</span>
      {chord ? <Hotkey chord={chord} /> : null}
    </div>
  )
}

/** A section header: the label, its count badge (review sections), the chevron. */
function SectionHead({ label, count }: { label: string; count?: number }) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: u(4), padding: `${u(8)} ${u(8)} ${u(4)}` }}>
      <span style={{ ...type("smallMedium"), color: C.muted }}>{label}</span>
      {count !== undefined && (
        <span
          style={{ display: "flex", alignItems: "center", justifyContent: "center", height: u(16), minWidth: u(16), padding: `0 ${u(4)}`, borderRadius: u(4), background: C.fill, ...type("xsmallMedium"), color: C.fg }}
        >
          {count}
        </span>
      )}
      <SmallGlyph kind="chevron-down" size={u(12)} color={QUIET} />
    </div>
  )
}

function WorkspaceRow({ name, agent, chord, indent }: { name: string; agent?: string; chord?: number; indent: boolean }) {
  const busy = agent !== undefined
  return (
    <div style={{ display: "flex", alignItems: "center", gap: u(8), padding: `${u(4)} ${u(8)}`, paddingLeft: indent ? u(28) : u(8) }}>
      {busy ? <SmallGlyph kind="play" size={u(12)} color={C.green} /> : <SmallGlyph kind="dot" size={u(12)} color={QUIET} />}
      <span style={{ ...type(busy ? "smallMedium" : "small"), color: busy ? C.loud : C.fg, whiteSpace: "nowrap" }}>{name}</span>
      <span style={{ flex: 1 }} />
      <span style={{ ...type(busy ? "xsmallSemibold" : "xsmall"), color: busy ? C.fg : C.muted }}>{busy ? agent : "Idle"}</span>
      {chord !== undefined && <Hotkey chord={`⌘${chord}`} />}
    </div>
  )
}

function ReviewRow({ title, second, ready, chord }: { title: string; second: string; ready: boolean; chord: number }) {
  return (
    <div style={{ display: "flex", alignItems: "flex-start", gap: u(4), padding: `${u(4)} ${u(8)}` }}>
      <span style={{ display: "flex", alignItems: "center", height: u(16), flex: "none" }}>
        {ready ? <SmallGlyph kind="check" size={u(12)} color={C.cyan} /> : <SmallGlyph kind="dot" size={u(12)} color={QUIET} />}
      </span>
      <div style={{ display: "flex", flexDirection: "column", flex: 1, minWidth: 0, gap: u(2) }}>
        <div style={{ display: "flex", alignItems: "center", gap: u(8) }}>
          <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", ...type("xsmallMedium"), color: C.fg }}>
            {title}
          </span>
          <Hotkey chord={`⌘${chord}`} />
        </div>
        <span style={{ ...type("xsmall"), color: C.muted, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{second}</span>
      </div>
    </div>
  )
}

/** An agent name as the app shows it: first letter uppercased (`view.rs` `title_case`). */
function titleCase(name: string): string {
  return name.charAt(0).toUpperCase() + name.slice(1)
}

/** The sidebar body under the nav: Workspaces, then Ready for review, then Queued for review. */
function SidebarSections({ state }: { state: AppState }) {
  let chord = 1
  const multi = state.machines.length > 1
  const workspaceRows: ReactNode[] = []
  for (const m of state.machines) {
    if (multi) {
      workspaceRows.push(
        <div key={`m-${m.name}`} style={{ display: "flex", alignItems: "center", gap: u(4), padding: `${u(4)} ${u(8)}` }}>
          <span style={{ ...type("smallMedium"), color: C.muted }}>{m.name}</span>
          <SmallGlyph kind="chevron-down" size={u(12)} color={QUIET} />
        </div>,
      )
    }
    for (const w of m.workspaces) {
      const working = w.state === "working"
      workspaceRows.push(
        <WorkspaceRow key={w.name} name={w.name} agent={working ? titleCase(w.agent ?? "developer") : undefined} chord={working ? chord++ : undefined} indent={multi} />,
      )
    }
  }
  const review = (label: string, entries: AppState["readyReview"], ready: boolean) =>
    entries.length === 0 ? null : (
      <div key={label} style={{ display: "flex", flexDirection: "column" }}>
        <div style={{ height: u(16) }} />
        <SectionHead label={label} count={entries.length} />
        {entries.map((e) => (
          <ReviewRow key={e.title} title={e.title} second={e.branch} ready={ready} chord={chord++} />
        ))}
      </div>
    )
  return (
    <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0, overflow: "hidden", padding: `0 ${u(12)} ${u(8)}` }}>
      <div style={{ height: u(8) }} />
      <SectionHead label="Workspaces" />
      {workspaceRows}
      {review("Ready for review", state.readyReview, true)}
      {review("Queued for review", state.queuedReview, false)}
    </div>
  )
}

/** The sidebar: titlebar, project switcher, nav, sections, footer. `current` is the main area's view. */
export function DesktopSidebar({
  state,
  current,
  contentOpacity,
}: {
  state: AppState
  current: "orchestrator" | "issues" | null
  contentOpacity?: number
}) {
  const navIcon = (icon: HeroIcon, on: boolean) => <Hi icon={icon} color={on ? C.accent : C.icon} />
  return (
    <div style={{ display: "flex", flexDirection: "column", flex: "none", width: u(280), height: "100%", background: C.sidebar, color: C.fg }}>
      <Titlebar />
      <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0, ...fadeStyle(contentOpacity) }}>
        <div style={{ display: "flex", flexDirection: "column", justifyContent: "center", flex: "none", minHeight: u(48), padding: `${u(8)} ${u(12)}` }}>
          <div style={{ display: "flex", alignItems: "center", gap: u(4), height: u(28), padding: `0 ${u(8)}`, ...type("baseMedium"), color: C.fg }}>
            {state.project}
            <Hi icon={ChevronUpDownIcon} color={C.muted} />
          </div>
        </div>
        <div style={{ display: "flex", flexDirection: "column", flex: "none", gap: u(4), padding: `${u(8)} ${u(12)}` }}>
          <NavItem icon={navIcon(ChatBubbleOvalLeftEllipsisIcon, current === "orchestrator")} label="Orchestrator" chord="⌘O" current={current === "orchestrator"} />
          <NavItem icon={navIcon(RectangleStackIcon, current === "issues")} label="Issues" chord="⌘I" current={current === "issues"} />
          <NavItem icon={navIcon(RocketLaunchIcon, false)} label="Changelog" chord="⌘L" />
        </div>
        <SidebarSections state={state} />
      </div>
      <SidebarFooter zen={state.zenMode ?? false} />
    </div>
  )
}

// ── Issues board ──────────────────────────────────────────────────────

/** A Small button (28pt, 12pt in, SmallMedium): the filter chips, New issue. */
export function SmallButton({
  children,
  select,
  style,
}: {
  children: ReactNode
  select?: boolean
  style?: CSSProperties
}) {
  return (
    <span
      style={{
        display: "inline-flex",
        flex: "none",
        alignItems: "center",
        justifyContent: "center",
        gap: u(4),
        height: u(28),
        padding: `0 ${u(12)}`,
        borderRadius: u(6),
        background: select ? C.selectFill : C.fill,
        color: select ? C.selectLabel : C.loud,
        whiteSpace: "nowrap",
        ...type("smallMedium"),
        ...style,
      }}
    >
      {children}
    </span>
  )
}

function FilterChip({ label, set }: { label: string; set?: boolean }) {
  return (
    <SmallButton select={set} style={{ paddingRight: u(8) }}>
      {label}
      <FigmaIcon kind="chevron-down-small" size={u(18)} color={set ? C.selectLabel : C.buttonIcon} />
    </SmallButton>
  )
}

/** The Issues Header: `Filter:`, the chips, the freshness banner, Search, New issue. */
function BoardHeader({ banner }: { banner?: string }) {
  return (
    <div
      style={{ display: "flex", flex: "none", alignItems: "center", gap: u(8), height: u(52), paddingLeft: u(12), paddingRight: u(16), background: C.toolbar, borderLeft: `${u(1)} solid ${C.canvas}` }}
    >
      <span style={{ ...type("smallMedium"), color: C.muted, flex: "none" }}>Filter:</span>
      <FilterChip label="Workflows" />
      <FilterChip label="Workspaces" />
      {banner ? <span style={{ ...type("xsmall"), color: C.muted, whiteSpace: "nowrap" }}>{banner}</span> : null}
      <span style={{ flex: 1 }} />
      <span
        style={{ display: "flex", flex: "none", alignItems: "center", gap: u(6), width: u(180), height: u(28), padding: `0 ${u(6)}`, borderRadius: u(6), background: C.fill }}
      >
        <Hi icon={MagnifyingGlassIcon} color={C.icon} />
        <span style={{ ...type("small"), color: C.muted }}>Search</span>
      </span>
      <SmallButton style={{ paddingLeft: u(8) }}>
        <Hi icon={PlusIcon} size={18} color={C.buttonIcon} />
        New issue
      </SmallButton>
    </div>
  )
}

/** The branch a dispatched card shows (`<user>/<id>`, as dispatch cuts them); none before dispatch. */
function branchFor(card: Card, category: Category, user: string): string | undefined {
  return category === "yellow" || category === "magenta" ? `${user}/${card.id}` : undefined
}

export function BoardCard({
  card,
  category,
  workflow,
  user,
}: {
  card: Card
  category: Category
  workflow?: string
  /** The branches' user (`<user>/<id>`): the scenario's `terminalTitle` user. */
  user: string
}) {
  const branch = branchFor(card, category, user)
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        flex: "none",
        gap: u(6),
        padding: u(12),
        borderRadius: u(12),
        background: C.card,
        boxShadow: card.selected ? FOCUS_SHADOW : RAISED_SHADOW,
        overflow: "hidden",
      }}
    >
      <div
        style={{ ...type("baseMedium"), color: C.loud, display: "-webkit-box", WebkitLineClamp: 2, WebkitBoxOrient: "vertical", overflow: "hidden", marginRight: u(-6) }}
      >
        {card.title}
      </div>
      {card.summary ? (
        <div
          style={{ ...type("xsmall"), lineHeight: u(18), color: C.fg, display: "-webkit-box", WebkitLineClamp: 3, WebkitBoxOrient: "vertical", overflow: "hidden" }}
        >
          {card.summary}
        </div>
      ) : null}
      <div style={{ display: "flex", alignItems: "center", gap: u(4), ...type("xsmall"), color: C.muted, overflow: "hidden" }}>
        {workflow ? (
          <span style={{ flex: "none", padding: `0 ${u(4)}`, borderRadius: u(2), background: C.badgeBg, color: C.badgeFg }}>{workflow}</span>
        ) : null}
        {branch ? <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{branch}</span> : null}
      </div>
    </div>
  )
}

/** A column's heading: the status glyph, the label in caps (loud on the keyboard's column), the count. */
export function StatusHeading({ category, label, count, current }: { category: Category; label: string; count?: number; current?: boolean }) {
  const s = STATUS[category]
  return (
    <div style={{ display: "flex", alignItems: "center", gap: u(8), whiteSpace: "nowrap" }}>
      <StatusGlyph kind={s.kind} size={u(16)} color={s.color} />
      <span style={{ ...type("xsmallSemibold"), color: current ? C.loud : C.muted }}>{label.toUpperCase()}</span>
      {count !== undefined ? <span style={{ ...type("xsmall"), color: C.muted }}>{count}</span> : null}
    </div>
  )
}

function BoardColumn({ column, workflow, current, user }: { column: Column; workflow?: string; current: boolean; user: string }) {
  if (column.collapsed) {
    const s = STATUS[column.category]
    return (
      <div style={{ display: "flex", flexDirection: "column", alignItems: "center", flex: "none", width: u(24), gap: u(8), paddingTop: u(14) }}>
        <StatusGlyph kind={s.kind} size={u(16)} color={s.color} />
        <span style={{ writingMode: "vertical-rl", ...type("xsmallSemibold"), color: C.muted, whiteSpace: "nowrap" }}>
          {column.label.toUpperCase()}
          <span style={{ fontWeight: 400, marginTop: u(8), display: "inline-block" }}>{column.cards.length}</span>
        </span>
      </div>
    )
  }
  return (
    <div style={{ display: "flex", flexDirection: "column", flex: "none", width: u(272) }}>
      <div style={{ display: "flex", flex: "none", alignItems: "center", height: u(44), paddingLeft: u(4) }}>
        <StatusHeading category={column.category} label={column.label} count={column.cards.length} current={current} />
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: u(8) }}>
        {column.cards.map((card) => (
          <BoardCard key={card.id} card={card} category={column.category} workflow={workflow} user={user} />
        ))}
        {column.cards.length === 0 ? (
          // An empty column: what goes here and how to put it there.
          <div
            style={{ display: "flex", alignItems: "center", justifyContent: "center", height: u(72), padding: `0 ${u(12)}`, borderRadius: u(12), border: `${u(1)} dashed ${C.dividerStrong}`, ...type("xsmall"), color: C.muted, textAlign: "center" }}
          >
            No issues. Drag a card here.
          </div>
        ) : null}
      </div>
    </div>
  )
}

const BOARD_HINTS =
  "h/l column   k/j card   ↵ open   ⇧h/⇧l move   ⇧k/⇧j reorder   drag to move or reorder   s start   a assign   e edit   E editor   n new"

/** The Issues view: the Header band, the columns, the key-hint footer. */
export function DesktopBoard({ state, contentOpacity }: { state: AppState; contentOpacity?: number }) {
  // The keyboard's column: the one holding the selected card, else TO DO.
  const currentIndex = Math.max(
    state.columns.findIndex((c) => c.cards.some((card) => card.selected)),
    state.columns.findIndex((c) => c.category === "blue"),
  )
  return (
    <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0 }}>
      <BoardHeader banner="board 2s" />
      <div style={{ display: "flex", flex: 1, minHeight: 0, gap: u(12), padding: `0 ${u(12)}`, overflow: "hidden", ...fadeStyle(contentOpacity) }}>
        {state.columns.map((column, i) => (
          <BoardColumn key={column.label} column={column} workflow={state.workflow} current={i === currentIndex} user={branchUser(state)} />
        ))}
      </div>
      <div
        style={{ display: "flex", flex: "none", alignItems: "center", height: u(29), padding: `0 ${u(16)}`, borderTop: `${u(1)} solid ${C.divider}`, ...type("xsmall"), color: C.muted, whiteSpace: "pre", overflow: "hidden" }}
      >
        {BOARD_HINTS}
      </div>
    </div>
  )
}

// ── Activity ──────────────────────────────────────────────────────────

/**
 * One Activity feed row as the desktop words it (`views/activity_labels.rs`):
 * the actor (colored by kind, no brackets), the muted verb, the title, the
 * X-Small second line, and the status tag (where the task is now) and time
 * pinned right.
 */
type ActivityLine = { actor: string; color: string; verb: string; title?: string; tag?: [string, string]; time: string; secondary?: string }

/** The same `Today` feed the TUI's Activity mockup shows (`KanbanMockup` `activityRows`). */
const activityLines = (user: string): ActivityLine[] => [
  { actor: "charlie", color: "var(--dt-term-green)", verb: "finished", title: "Cold-start cache", tag: ["review", "var(--dt-term-magenta)"], time: "12m", secondary: `⎇ ${user}/t-011` },
  { actor: "alpha", color: "var(--dt-term-cyan)", verb: "is building", title: "Deploy staging env", tag: ["in progress", "var(--dt-term-yellow)"], time: "18m", secondary: `⎇ ${user}/t-009` },
  { actor: "bravo", color: C.muted, verb: "awaiting input", time: "25m" },
  { actor: "echo", color: "var(--dt-term-blue)", verb: "is building", title: "Backfill order index", tag: ["in progress", "var(--dt-term-yellow)"], time: "40m", secondary: `⎇ ${user}/t-017` },
  { actor: "you", color: C.fg, verb: "merged", title: "Ship dark mode", tag: ["done", "var(--dt-term-green)"], time: "1h", secondary: "moved to done" },
]

/** The footer's key hints at the default activity keymap (`render_footer`). */
const ACTIVITY_HINTS = "j/k select   enter open   Page Up/Page Down page   g top   G bottom   r refresh   a/z/w filter"

/**
 * The Activity view (`views/activity.rs`): the `Activity` title over the kind
 * filter (All · Tasks · Zen · Workspaces · System) and the project and
 * workspace pickers, the day-bucketed feed, and the key hints.
 */
export function DesktopActivity({ state }: { state: AppState }) {
  const segment = (label: string, on: boolean) => (
    <span key={label} style={{ display: "flex", alignItems: "center", height: u(24), padding: `0 ${u(10)}`, borderRadius: u(6), background: on ? C.card : "transparent", ...type("smallMedium"), color: on ? C.loud : C.muted }}>
      {label}
    </span>
  )
  return (
    <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0 }}>
      <div style={{ display: "flex", flexDirection: "column", flex: "none", gap: u(8), padding: `${u(8)} ${u(16)}`, borderBottom: `${u(1)} solid ${C.divider}` }}>
        <div style={{ display: "flex", alignItems: "center", gap: u(8) }}>
          <span style={{ ...type("baseMedium"), color: C.loud }}>Activity</span>
          <span style={{ ...type("xsmall"), color: C.muted }}>{state.project}</span>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: u(4) }}>
          <span style={{ display: "flex", gap: u(2), padding: u(2), borderRadius: u(8), background: C.fill }}>
            {["All", "Tasks", "Zen", "Workspaces", "System"].map((label) => segment(label, label === "All"))}
          </span>
          <span style={{ width: u(8) }} />
          <FilterChip label={state.project} />
          <FilterChip label="All workspaces" />
        </div>
      </div>
      <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0, overflow: "hidden", padding: `0 ${u(16)}` }}>
        <div style={{ display: "flex", alignItems: "center", gap: u(8), height: u(40), flex: "none", ...type("xsmall"), color: C.muted }}>
          <span style={{ width: u(12), height: u(1), background: C.divider }} />
          Today
          <span style={{ flex: 1, height: u(1), background: C.divider }} />
        </div>
        {activityLines(branchUser(state)).map((line, i) => (
          <div key={i} style={{ display: "flex", alignItems: "flex-start", flex: "none", padding: `${u(4)} ${u(8)}`, borderRadius: u(6), background: i === 0 ? C.selection : "transparent" }}>
            <span style={{ flex: "none", minWidth: u(88), paddingRight: u(8), color: line.color }}>{line.actor}</span>
            <span style={{ flex: "none", paddingRight: u(8), color: C.muted }}>{line.verb}</span>
            <span style={{ display: "flex", flexDirection: "column", flex: 1, minWidth: 0, gap: u(2) }}>
              <span style={{ color: C.fg }}>{line.title}</span>
              {line.secondary ? <span style={{ ...type("xsmall"), color: C.muted }}>{line.secondary}</span> : null}
            </span>
            <span style={{ display: "flex", alignItems: "center", flex: "none", gap: u(8), paddingLeft: u(8) }}>
              {line.tag ? <span style={{ ...type("baseMedium"), color: line.tag[1] }}>[{line.tag[0]}]</span> : null}
              <span style={{ color: C.muted }}>{line.time}</span>
            </span>
          </div>
        ))}
      </div>
      <div style={{ display: "flex", flex: "none", alignItems: "center", height: u(27), padding: `0 ${u(16)}`, borderTop: `${u(1)} solid ${C.divider}`, ...type("xsmall"), color: C.muted, whiteSpace: "pre", overflow: "hidden" }}>
        {ACTIVITY_HINTS}
      </div>
    </div>
  )
}

// ── Terminals ─────────────────────────────────────────────────────────

/**
 * The desktop terminal's colors over the shared Claude Code engine: the
 * `--tui-*` names it paints with, redefined to the desktop terminal palette
 * (the canvas background, the composited foreground and dim text, the ANSI
 * slots). See `theme/mod.rs` `TerminalPalette`.
 */
const TERMINAL_VARS = {
  "--tui-bg": "var(--dt-canvas)",
  "--tui-fg": "var(--dt-term-fg)",
  "--tui-gray": "var(--dt-term-fg)",
  "--tui-dark-gray": "var(--dt-term-dim)",
  "--tui-divider": "var(--dt-term-dim)",
  "--tui-cyan": "var(--dt-term-cyan)",
  "--tui-green": "var(--dt-term-green)",
  "--tui-magenta": "var(--dt-term-magenta)",
  "--tui-yellow": "var(--dt-term-yellow)",
  "--tui-blue": "var(--dt-term-blue)",
  "--tui-light-red": "var(--dt-term-red)",
  "--tui-sel-fg": "var(--dt-term-bold)",
  "--tui-task-box-bg": "var(--dt-term-box)",
} as CSSProperties

/** The terminal font: Source Code Pro 14 on 18pt rows, a cell 0.6em wide. */
const TERM_SIZE = 14
const TERM_LINE = 18
const TERM_CELL = TERM_SIZE * 0.6

/**
 * A Claude Code session in a desktop terminal, `width` × `height` points:
 * the grid inset 16pt across and 12pt down, filling as many rows and cells
 * as fit.
 */
export function DesktopTerminal({
  state,
  lines,
  width,
  height,
  topRule,
  contentOpacity,
}: {
  state: AppState
  lines: ChatLine[]
  width: number
  height: number
  topRule?: boolean
  contentOpacity?: number
}) {
  const rows = Math.floor((height - 24) / TERM_LINE)
  const cells = Math.floor((width - 32) / TERM_CELL)
  return (
    <div
      style={{
        flex: 1,
        minHeight: 0,
        padding: `${u(12)} ${u(16)}`,
        overflow: "hidden",
        borderTop: topRule ? `${u(1)} solid ${C.divider}` : undefined,
        ...TERMINAL_VARS,
      }}
    >
      <div className="dt-terminal" style={fadeStyle(contentOpacity)}>
        <ClaudeCodePane
          state={state}
          lines={lines}
          rows={rows}
          width={cells}
          style={{ background: "transparent", fontSize: u(TERM_SIZE), lineHeight: u(TERM_LINE), fontFamily: "var(--font-mono)" }}
        />
      </div>
    </div>
  )
}

// ── Workspace task panel ──────────────────────────────────────────────

/** A task panel's status heading: In progress's glyph is `icon/cyan` here, the label `foreground`. */
function PanelStatus({ category }: { category: Category }) {
  const s = STATUS[category]
  return (
    <div style={{ display: "flex", alignItems: "center", gap: u(8) }}>
      <StatusGlyph kind={s.kind} size={u(16)} color={s.panelColor} />
      <span style={{ ...type("xsmallSemibold"), color: C.fg }}>{s.label.toUpperCase()}</span>
    </div>
  )
}

/**
 * The Workspace view's leading column (the Navigation component's
 * State=Workspace): back, the task's status, title and summary with More,
 * the worktree folder with Open, then the views (the agent ⌘1, View Diff ⌘2,
 * Edit in Vi ⌘3, Shell ⌘4) over the sidebar's footer.
 */
export function WorkspacePanel({
  state,
  workspace,
  contentOpacity,
}: {
  state: AppState
  workspace: string
  contentOpacity?: number
}) {
  let task: { card: Card; category: Category } | undefined
  for (const col of state.columns) {
    const card = col.cards.find((c) => c.workspace === workspace)
    if (card) task = { card, category: col.category }
  }
  const agent =
    titleCase(state.machines.flatMap((m) => m.workspaces).find((w) => w.name === workspace)?.agent ?? "developer")
  return (
    <div style={{ display: "flex", flexDirection: "column", flex: "none", width: u(280), height: "100%", background: C.sidebar, color: C.fg }}>
      <Titlebar />
      <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0, ...fadeStyle(contentOpacity) }}>
        <div style={{ display: "flex", flexDirection: "column", gap: u(10), padding: `${u(8)} ${u(16)} 0 ${u(16)}` }}>
          <div style={{ display: "flex", alignItems: "center", gap: u(16) }}>
            <span style={{ display: "flex", alignItems: "center", justifyContent: "center", width: u(28), height: u(28), borderRadius: u(6), background: C.fill, color: C.buttonIcon }}>
              <Hi icon={ArrowLeftIcon} />
            </span>
            {task ? <PanelStatus category={task.category} /> : null}
          </div>
          {task ? (
            <div style={{ display: "flex", flexDirection: "column", gap: u(6), marginTop: u(6) }}>
              <span style={{ ...type("baseMedium"), color: C.loud }}>{task.card.title}</span>
              {task.card.summary ? (
                <span style={{ ...type("xsmall"), lineHeight: u(18), color: C.fg, display: "-webkit-box", WebkitLineClamp: 5, WebkitBoxOrient: "vertical", overflow: "hidden" }}>
                  {task.card.summary}
                </span>
              ) : null}
              <span style={{ ...type("xsmallMedium"), color: C.link }}>More</span>
            </div>
          ) : null}
          <div style={{ display: "flex", alignItems: "center", gap: u(8), marginTop: u(10) }}>
            <Hi icon={FolderIcon} color={C.icon} />
            <span style={{ flex: 1, minWidth: 0, ...type("xsmall"), color: C.fg, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              …/{state.project}/.shelbi/wt/{workspace}
            </span>
            <span style={{ display: "flex", alignItems: "center", height: u(24), padding: `0 ${u(8)}`, borderRadius: u(6), background: C.fill, ...type("xsmallMedium"), color: C.loud }}>
              Open
            </span>
          </div>
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: u(4), padding: `${u(24)} ${u(12)} ${u(8)}` }}>
          <NavItem icon={<Hi icon={ChatBubbleOvalLeftEllipsisIcon} color={C.accent} />} label={agent} chord="⌘1" current />
          <NavItem icon={<Hi icon={ArrowsRightLeftIcon} color={C.icon} />} label="View Diff" chord="⌘2" />
          <NavItem icon={<Hi icon={PencilSquareIcon} color={C.icon} />} label="Edit in Vi" chord="⌘3" />
          <NavItem icon={<Hi icon={CommandLineIcon} color={C.icon} />} label="Shell" chord="⌘4" />
        </div>
      </div>
      <SidebarFooter zen={state.zenMode ?? false} />
    </div>
  )
}

// ── Command palette ───────────────────────────────────────────────────

type PaletteRow = { icon: ReactNode; name: string; detail: string }

/** The commands an empty query lists, in the frame's order and copy (`palette.rs` `DEFAULT_LIST`). */
function defaultCommands(zen: boolean): PaletteRow[] {
  const icon = (i: HeroIcon) => <Hi icon={i} color={C.icon} />
  const figma = (k: FigmaIconKind) => <FigmaIcon kind={k} size={u(20)} color={C.icon} />
  return [
    { icon: icon(ChatBubbleOvalLeftEllipsisIcon), name: "Orchestrator", detail: "Chat with your Orchestrator agent" },
    { icon: icon(RectangleStackIcon), name: "Issues", detail: "Manage what Shelbi is working on here" },
    { icon: icon(RocketLaunchIcon), name: "Changelog", detail: "Review what your agents have finished" },
    { icon: icon(BoltIcon), name: "Activity", detail: "See what’s happened recently" },
    { icon: icon(ServerStackIcon), name: "Machines", detail: "See the machines running your agents" },
    {
      icon: figma(zen ? "toggle-on" : "toggle-off"),
      name: zen ? "Turn Zen Mode off" : "Turn Zen Mode on",
      detail: "Let Shelbi handle your entire workflow",
    },
    { icon: figma("page-curl"), name: "Error log", detail: "Review recent errors and warnings" },
    { icon: icon(BoltSlashIcon), name: "Detach", detail: "Leave Shelbi running in the background" },
    { icon: icon(XMarkIcon), name: "Close project", detail: "Stop this project’s agents and close it" },
    { icon: figma("flag-finish"), name: "Quit Shelbi", detail: "Shut down Shelbi" },
  ]
}

/** A case-insensitive subsequence match, the palette's fuzzy filter. */
function fuzzy(query: string, text: string): boolean {
  const q = query.toLowerCase()
  const t = text.toLowerCase()
  let i = 0
  for (const ch of t) if (ch === q[i]) i += 1
  return i >= q.length
}

function PaletteResult({ row, selected }: { row: PaletteRow; selected: boolean }) {
  return (
    <div
      style={{ display: "flex", alignItems: "center", gap: u(6), height: u(32), padding: `0 ${u(6)}`, borderRadius: u(6), background: selected ? C.fill : undefined, ...type("base") }}
    >
      <span style={{ display: "flex", alignItems: "center", justifyContent: "center", width: u(20), flex: "none" }}>{row.icon}</span>
      <span style={{ width: u(220), flex: "none", ...type(selected ? "baseSemibold" : "baseMedium"), color: selected ? C.loud : C.fg, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
        {row.name}
      </span>
      <span style={{ flex: 1, minWidth: 0, color: C.fg, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{row.detail}</span>
    </div>
  )
}

/**
 * The command palette (⌃P) over the window's scrim: the `❯` query line,
 * then on an empty query the frame's commands beside the Projects column,
 * or, once something is typed, the result filter tabs (All, Issues,
 * Changelog Entries, Commands, each with its count) over the matching
 * commands, workspaces included (`command.rs` lists every workspace,
 * waiting for a query, with its current task as the detail).
 */
export function DesktopPalette({ state, top = 160 }: { state: AppState; top?: number }) {
  const palette = state.palette
  if (!palette) return null
  const query = palette.query
  const workspaces: PaletteRow[] = state.machines.flatMap((m) =>
    m.workspaces.map((w) => {
      const task = state.columns.flatMap((c) => c.cards).find((c) => c.workspace === w.name)
      return {
        icon: <Hi icon={CommandLineIcon} color={w.state === "working" ? C.green : C.icon} />,
        name: w.name,
        detail: task?.id ?? "",
      }
    }),
  )
  const commands = query
    ? [...defaultCommands(state.zenMode ?? false), ...workspaces].filter((r) => fuzzy(query, r.name))
    : defaultCommands(state.zenMode ?? false)
  const footer = query
    ? "↑↓ navigate · Tab filter · Enter open · Esc / ⌃P close"
    : "↑↓ navigate · Enter activate · Tab projects · Esc / ⌃P close"
  const tabs: [string, number][] = [
    ["All", commands.length],
    ["Issues", 0],
    ["Changelog Entries", 0],
    ["Commands", commands.length],
  ]
  return (
    <div style={{ position: "absolute", inset: 0, background: C.scrim, display: "flex", justifyContent: "center", alignItems: "flex-start" }}>
      <div
        style={{
          marginTop: u(top),
          width: u(788),
          maxWidth: `calc(100% - ${u(32)})`,
          borderRadius: u(12),
          background: C.raised,
          boxShadow: `0 ${u(10)} ${u(15)} ${u(-3)} rgba(0,0,0,0.25), 0 0 0 ${u(1)} var(--dt-ring)`,
          color: C.fg,
          overflow: "hidden",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: u(8), height: u(44), padding: `0 ${u(16)}`, ...type("base") }}>
          <span style={{ color: C.loud, fontWeight: 600 }}>❯</span>
          <span style={{ display: "flex", alignItems: "center", color: query ? C.loud : C.muted }}>
            {query || "Type a command or search"}
            <span style={{ width: u(1.5), height: u(18), background: C.focusRing, marginLeft: u(1) }} />
          </span>
        </div>
        {query ? (
          <div style={{ padding: `0 ${u(16)} ${u(8)}` }}>
            <div style={{ display: "inline-flex", gap: u(4), padding: u(3), borderRadius: u(8), background: C.search }}>
              {tabs.map(([label, count], i) => (
                <span
                  key={label}
                  style={{ display: "flex", alignItems: "center", gap: u(6), height: u(26), padding: `0 ${u(12)}`, borderRadius: u(6), background: i === 0 ? C.canvas : undefined, ...type("baseMedium"), color: i === 0 ? C.loud : C.fg }}
                >
                  {label}
                  <span style={{ fontWeight: 400, color: C.muted }}>{count}</span>
                </span>
              ))}
            </div>
          </div>
        ) : null}
        <div style={{ display: "flex", gap: u(16), padding: `0 ${u(16)}` }}>
          <div style={{ display: "flex", flexDirection: "column", flex: 1, minWidth: 0 }}>
            {query ? (
              <div style={{ display: "flex", alignItems: "flex-end", height: u(28), padding: `0 ${u(6)} ${u(4)}`, ...type("smallMedium"), color: C.muted }}>
                Commands
              </div>
            ) : null}
            {commands.map((row, i) => (
              <PaletteResult key={row.name} row={row} selected={i === 0} />
            ))}
          </div>
          {query ? null : (
            <div style={{ display: "flex", flexDirection: "column", flex: "none", width: u(160), gap: u(2) }}>
              <div style={{ display: "flex", alignItems: "center", height: u(28), padding: `0 ${u(6)}`, ...type("smallMedium"), color: C.muted }}>Projects</div>
              <div style={{ display: "flex", alignItems: "center", gap: u(8), height: u(26), padding: `0 ${u(6)}`, color: C.fg }}>
                <span style={{ width: u(10), height: u(10), borderRadius: "50%", background: C.green, flex: "none" }} />
                <span style={{ flex: 1 }}>{state.project}</span>
                <Hi icon={CheckIcon} size={16} color={C.icon} />
              </div>
              <div style={{ height: u(1), background: C.divider, margin: `${u(8)} ${u(6)}` }} />
              <div style={{ display: "flex", alignItems: "center", gap: u(6), height: u(26), padding: `0 ${u(6)}`, color: C.fg }}>
                <Hi icon={PlusIcon} size={16} color={C.icon} />
                Add project
              </div>
            </div>
          )}
        </div>
        <div style={{ padding: `${u(8)} ${u(16)} ${u(12)}`, ...type("xsmall"), color: C.muted }}>{footer}</div>
      </div>
    </div>
  )
}

// ── The whole app ─────────────────────────────────────────────────────

/**
 * One `AppState` scenario as the desktop app shows it: the Orchestrator
 * (`chat`), the Issues board (`tasks`), the Activity view (`activity`, which
 * the desktop keeps off the sidebar), or the Workspace view (`workspace`: the task panel in the
 * sidebar's place, the agent's terminal beside it), with the command palette
 * over it while `palette` is set. `contentOpacity` fades the contents (not
 * the surfaces) for the hero's loop.
 */
export function DesktopAppMockup({
  state,
  width,
  height,
  contentOpacity,
  label = "The Shelbi desktop app",
}: {
  state: AppState
  width: number
  height: number
  contentOpacity?: number
  label?: string
}) {
  const view = state.activeView
  const mainW = width - 280
  if (view === "workspace" && state.focusedWorkspace) {
    return (
      <DesktopWindow
        width={width}
        height={height}
        label={label}
        leading={<WorkspacePanel state={state} workspace={state.focusedWorkspace} contentOpacity={contentOpacity} />}
        overlay={<DesktopPalette state={state} />}
      >
        <DesktopTerminal state={state} lines={state.workspaceLines ?? []} width={mainW} height={height} topRule contentOpacity={contentOpacity} />
      </DesktopWindow>
    )
  }
  const chat = view === "chat"
  return (
    <DesktopWindow
      width={width}
      height={height}
      label={label}
      leading={<DesktopSidebar state={state} current={chat ? "orchestrator" : "issues"} contentOpacity={contentOpacity} />}
      overlay={<DesktopPalette state={state} />}
    >
      {chat ? (
        <DesktopTerminal state={state} lines={state.chatLines ?? []} width={mainW} height={height} contentOpacity={contentOpacity} />
      ) : view === "activity" ? (
        <DesktopActivity state={state} />
      ) : (
        <DesktopBoard state={state} contentOpacity={contentOpacity} />
      )}
    </DesktopWindow>
  )
}
