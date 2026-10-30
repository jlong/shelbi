import type { CSSProperties, ReactNode } from "react"
import {
  ArrowLeftIcon,
  ArrowsRightLeftIcon,
  ChatBubbleOvalLeftEllipsisIcon,
  CheckIcon,
  CommandLineIcon,
  EllipsisVerticalIcon,
  GlobeAltIcon,
  PencilSquareIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline"
import type { Category } from "../KanbanMockup"
import { FigmaIcon, SmallGlyph, StatusGlyph } from "./DesktopIcons"
import { C, Hotkey, NavItem, SmallButton, StatusHeading, STATUS, type, u } from "./DesktopMockup"

/**
 * The Desktop variants of the feature grid's vignettes: small slices of the
 * desktop app, each telling the same story as its TUI vignette (same tasks,
 * workspaces and hover beat) in the app's own UI, built from the same parts
 * as the big desktop mockups (`DesktopMockup.tsx`) at a smaller scale. Every
 * vignette is decorative (`aria-hidden`); hover motion is CSS `group-hover:`
 * gated behind `motion-safe:`, as the TUI vignettes are.
 */

// One point at 0.62px: a ~300px card shows a ~480pt slice of the app. The
// wider slices (the four-column board, the panel beside a terminal) draw at
// 0.52px so a ~580pt slice fits.
const SCALE = 0.62
const WIDE = 0.52
const SANS = "var(--font-sans)"
const QUIET = "color-mix(in srgb, var(--dt-icon) 60%, transparent)"

/** A slice of a desktop window: rounded, the traffic lights over the leading surface. */
function MiniWindow({
  children,
  lights = true,
  background = C.canvas,
  scale = SCALE,
}: {
  children: ReactNode
  lights?: boolean
  background?: string
  scale?: number
}) {
  return (
    <div
      aria-hidden="true"
      className="overflow-hidden shadow-sm transition-shadow duration-200 group-hover:shadow-md"
      style={{ ["--u" as string]: `${scale}px`, position: "relative", borderRadius: u(12), background, color: C.fg, fontFamily: SANS, ...type("base"), boxShadow: "0 0 0 1px var(--dt-window-edge)" }}
    >
      {lights ? (
        <div style={{ position: "absolute", top: u(16), left: u(16), display: "flex", gap: u(8), zIndex: 1 }}>
          {["#ff5f57", "#febc2e", "#28c840"].map((c) => (
            <span key={c} style={{ width: u(12), height: u(12), borderRadius: "50%", background: c }} />
          ))}
        </div>
      ) : null}
      {children}
    </div>
  )
}

const hi = (Icon: typeof CheckIcon, color: string, size = 20) => (
  <Icon aria-hidden="true" style={{ width: u(size), height: u(size), flex: "none", color }} />
)

// ── Kanban board ──────────────────────────────────────────────────────

/**
 * The Issues board: To Do / In Progress / Review / Done under their status
 * headings, cards with the workflow badge and (once dispatched) the branch.
 * On hover the top To Do card slides one column right, the way a task is
 * pulled into In Progress.
 */
export function DesktopKanbanVignette() {
  const columns: { label: string; category: Category; cards: { title: string; branch?: string }[] }[] = [
    { label: "To Do", category: "blue", cards: [{ title: "API ratelimit" }, { title: "Mobile nav" }, { title: "Search paging" }] },
    { label: "In Progress", category: "yellow", cards: [{ title: "Deploy staging", branch: "jlong/deploy-staging" }] },
    { label: "Review", category: "magenta", cards: [{ title: "OAuth flow", branch: "jlong/oauth-flow" }] },
    { label: "Done", category: "green", cards: [{ title: "Dark mode" }] },
  ]
  return (
    <MiniWindow lights={false} scale={WIDE}>
      <div style={{ display: "flex", gap: u(10), padding: `${u(4)} ${u(12)} ${u(14)}` }}>
        {columns.map((col, ci) => (
          <div key={col.label} style={{ flex: 1, minWidth: 0 }}>
            <div style={{ display: "flex", alignItems: "center", height: u(40), overflow: "hidden" }}>
              <StatusHeading category={col.category} label={col.label} count={col.cards.length} current={ci === 0} />
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: u(8) }}>
              {col.cards.map((card, i) => (
                <div
                  key={card.title}
                  className={
                    ci === 0 && i === 0
                      ? "relative z-10 transition-transform duration-300 ease-out motion-safe:group-hover:translate-x-[calc(100%_+_10*var(--u))]"
                      : undefined
                  }
                  style={{ display: "flex", flexDirection: "column", gap: u(6), padding: u(10), borderRadius: u(10), background: C.card, boxShadow: "0 0 0 1px var(--dt-shadow-ring)" }}
                >
                  <span style={{ ...type("smallMedium"), color: C.loud, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{card.title}</span>
                  <span style={{ display: "flex", gap: u(4), ...type("xsmall"), color: C.muted, overflow: "hidden", whiteSpace: "nowrap" }}>
                    <span style={{ padding: `0 ${u(4)}`, borderRadius: u(2), background: C.badgeBg, color: C.badgeFg, flex: "none" }}>app</span>
                    {card.branch ? <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{card.branch}</span> : null}
                  </span>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </MiniWindow>
  )
}

// ── Workers on any machine ────────────────────────────────────────────

function WsRow({ name, agent, chord }: { name: string; agent?: string; chord?: number }) {
  const busy = agent !== undefined
  return (
    <div style={{ display: "flex", alignItems: "center", gap: u(8), padding: `${u(4)} ${u(8)} ${u(4)} ${u(28)}` }}>
      {busy ? (
        <span className="motion-safe:animate-pulse" style={{ display: "flex" }}>
          <SmallGlyph kind="play" size={u(12)} color={C.green} />
        </span>
      ) : (
        <SmallGlyph kind="dot" size={u(12)} color={QUIET} />
      )}
      <span style={{ ...type(busy ? "smallMedium" : "small"), color: busy ? C.loud : C.fg }}>{name}</span>
      <span style={{ flex: 1 }} />
      <span style={{ ...type(busy ? "xsmallSemibold" : "xsmall"), color: busy ? C.fg : C.muted }}>{busy ? agent : "Idle"}</span>
      {chord !== undefined ? <Hotkey chord={`⌘${chord}`} /> : null}
    </div>
  )
}

function MachineGroup({ name }: { name: string }) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: u(4), padding: `${u(4)} ${u(8)}` }}>
      <span style={{ ...type("smallMedium"), color: C.muted }}>{name}</span>
      <SmallGlyph kind="chevron-down" size={u(12)} color={QUIET} />
    </div>
  )
}

/** The sidebar's Workspaces, both machines; a working row's chord counts top to bottom. */
function WorkspaceList({ bravo }: { bravo: boolean }) {
  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", gap: u(4), padding: `${u(8)} ${u(8)} ${u(4)}` }}>
        <span style={{ ...type("smallMedium"), color: C.muted }}>Workspaces</span>
        <SmallGlyph kind="chevron-down" size={u(12)} color={QUIET} />
      </div>
      <MachineGroup name="hub" />
      <WsRow name="alpha" agent="Developer" chord={1} />
      <WsRow name="bravo" agent={bravo ? "Developer" : undefined} chord={bravo ? 2 : undefined} />
      <MachineGroup name="devbox" />
      <WsRow name="charlie" agent="Reviewer" chord={bravo ? 3 : 2} />
      <WsRow name="delta" />
    </div>
  )
}

/**
 * The sidebar's Workspaces grouped by machine (`hub`, `devbox`), each row
 * with its state glyph, its agent or Idle, and a working row's chord. On
 * hover bravo picks up a task: its dot turns to the green play glyph,
 * Idle to Developer, and it takes ⌘2.
 */
export function DesktopMachinesVignette() {
  return (
    <MiniWindow lights={false} background={C.sidebar}>
      <div style={{ display: "grid", padding: `${u(8)} ${u(12)} ${u(12)}` }}>
        <div className="transition-opacity duration-300 ease-out motion-safe:group-hover:opacity-0" style={{ gridArea: "1 / 1" }}>
          <WorkspaceList bravo={false} />
        </div>
        <div className="opacity-0 transition-opacity duration-300 ease-out motion-safe:group-hover:opacity-100" style={{ gridArea: "1 / 1" }}>
          <WorkspaceList bravo />
        </div>
      </div>
    </MiniWindow>
  )
}

// ── Attach from anywhere ──────────────────────────────────────────────

const TERM: CSSProperties = { fontFamily: "var(--font-mono)", fontSize: u(14), lineHeight: u(18), whiteSpace: "pre", overflow: "hidden" }

/**
 * The Workspace view: alpha's task panel (its views, the agent current)
 * beside the agent's own session in a desktop terminal, attached to the same
 * Shelbi session `shelbi attach` reaches. On hover a fresh line of agent
 * output prints above the prompt.
 */
export function DesktopAttachVignette() {
  return (
    <MiniWindow scale={WIDE}>
      <div style={{ display: "flex" }}>
        <div style={{ width: u(180), flex: "none", background: C.sidebar, padding: `${u(44)} ${u(8)} ${u(10)}`, display: "flex", flexDirection: "column", gap: u(4) }}>
          <NavItem icon={hi(ChatBubbleOvalLeftEllipsisIcon, C.accent)} label="Developer" chord="⌘1" current />
          <NavItem icon={hi(ArrowsRightLeftIcon, C.icon)} label="View Diff" chord="⌘2" />
          <NavItem icon={hi(PencilSquareIcon, C.icon)} label="Edit in Vi" chord="⌘3" />
          <NavItem icon={hi(CommandLineIcon, C.icon)} label="Shell" chord="⌘4" />
        </div>
        <div style={{ flex: 1, minWidth: 0, padding: `${u(12)} ${u(16)}`, display: "flex", flexDirection: "column", justifyContent: "flex-end", ...TERM, color: "var(--dt-term-fg)" }}>
          <div style={{ fontWeight: 700, color: "var(--dt-term-bold)" }}>❯ Deploy staging env</div>
          <div>{" "}</div>
          <div>
            <span style={{ color: "var(--dt-term-green)" }}>⏺ </span>
            <span style={{ fontWeight: 700 }}>Bash</span>(npm run migrate)
          </div>
          <div style={{ color: "var(--dt-term-dim)" }}>  ⎿ 4 migrations applied</div>
          <div
            className="max-h-0 opacity-0 transition-all duration-300 ease-out motion-safe:group-hover:max-h-[calc(18*var(--u))] motion-safe:group-hover:opacity-100"
            style={{ color: "var(--dt-term-green)" }}
          >
            ✓ ran migrations · 12 files changed
          </div>
          <div>{" "}</div>
          <div>
            <span style={{ color: "var(--dt-term-cyan)", fontWeight: 700 }}>❯ </span>
            <span className="motion-safe:animate-pulse" style={{ display: "inline-block", width: u(8), height: u(16), verticalAlign: "middle", background: "var(--dt-term-bold)" }} />
          </div>
        </div>
      </div>
    </MiniWindow>
  )
}

// ── Review flow ───────────────────────────────────────────────────────

/**
 * The Review view's task panel (the Navigation component's State=Review):
 * back and the REVIEW status, the task, its views (the Reviewer current,
 * View Diff, Edit in Vi, Web Browser, Shell), then Actions with Approve and
 * Reject. On hover Approve presses, then the actions give way to the
 * "Merging the PR…" line the panel shows while the gated merge runs (the
 * same two-beat timeline as the TUI vignette).
 */
export function DesktopReviewVignette() {
  return (
    <MiniWindow background={C.sidebar}>
      <div style={{ display: "flex", flexDirection: "column", padding: `${u(44)} ${u(12)} ${u(12)}` }}>
        <div style={{ display: "flex", alignItems: "center", gap: u(16), padding: `0 ${u(4)}` }}>
          <span style={{ display: "flex", alignItems: "center", justifyContent: "center", width: u(28), height: u(28), borderRadius: u(6), background: C.fill }}>
            {hi(ArrowLeftIcon, C.buttonIcon)}
          </span>
          <span style={{ display: "flex", alignItems: "center", gap: u(8) }}>
            <StatusGlyph kind="review" size={u(16)} color={C.purple} />
            <span style={{ ...type("xsmallSemibold"), color: C.fg }}>REVIEW</span>
          </span>
        </div>
        <div style={{ ...type("baseMedium"), color: C.loud, padding: `${u(14)} ${u(4)} ${u(12)}` }}>Wire up OAuth</div>
        <div style={{ display: "flex", flexDirection: "column", gap: u(2) }}>
          <NavItem icon={hi(ChatBubbleOvalLeftEllipsisIcon, C.accent)} label="Reviewer" chord="⌘1" current />
          <NavItem icon={hi(ArrowsRightLeftIcon, C.icon)} label="View Diff" chord="⌘2" />
          <NavItem icon={hi(PencilSquareIcon, C.icon)} label="Edit in Vi" chord="⌘3" />
          <NavItem icon={hi(GlobeAltIcon, C.icon)} label="Web Browser" chord="⌘4" />
          <NavItem icon={hi(CommandLineIcon, C.icon)} label="Shell" chord="⌘5" />
        </div>
        <div style={{ ...type("smallMedium"), color: C.muted, padding: `${u(16)} ${u(8)} ${u(4)}` }}>Actions</div>
        <div style={{ display: "grid" }}>
          <div className="motion-safe:group-hover:[animation:desktopApproveLeave_2.4s_ease-in-out_forwards]" style={{ gridArea: "1 / 1", display: "flex", gap: u(12), padding: `${u(8)} 0` }}>
            <span style={{ position: "relative", display: "inline-flex" }}>
              <SmallButton style={{ background: "var(--dt-zen)", color: C.successLabel, paddingLeft: u(8) }}>
                {hi(CheckIcon, C.successLabel, 18)}
                Approve
              </SmallButton>
              {/* The press: Approve's pressed fill fades in over its rest fill. */}
              <span
                className="opacity-0 motion-safe:group-hover:[animation:reviewApprovePress_2.4s_ease-in-out_forwards]"
                style={{ position: "absolute", inset: 0, borderRadius: u(6), background: "color-mix(in srgb, var(--dt-icon-green) 30%, transparent)" }}
              />
            </span>
            <SmallButton style={{ background: C.dangerFill, color: C.dangerLabel, paddingLeft: u(8) }}>
              {hi(XMarkIcon, C.dangerLabel, 18)}
              Reject
            </SmallButton>
          </div>
          <div
            className="opacity-0 motion-safe:group-hover:[animation:reviewMerging_2.4s_ease-in-out_forwards]"
            style={{ gridArea: "1 / 1", alignSelf: "center", padding: `${u(8)} ${u(8)}`, ...type("smallMedium"), color: "var(--dt-warning)" }}
          >
            Merging the PR…
          </div>
        </div>
      </div>
    </MiniWindow>
  )
}

// ── Plain-file state ──────────────────────────────────────────────────

function Checkbox({ checked }: { checked: boolean }) {
  return (
    <span
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        width: u(16),
        height: u(16),
        borderRadius: u(4),
        flex: "none",
        background: checked ? "var(--dt-check-fill)" : C.card,
        boxShadow: checked ? undefined : `inset 0 0 0 ${u(1)} var(--dt-input-border)`,
        color: "#fff",
      }}
    >
      {checked ? (
        <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth={1.75} strokeLinecap="round" strokeLinejoin="round" style={{ width: u(12), height: u(12) }}>
          <path d="M2.5 6.2L5 8.5L9.5 3.5" />
        </svg>
      ) : null}
    </span>
  )
}

function Prop({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div style={{ display: "flex", alignItems: "center", minHeight: u(30) }}>
      <span style={{ width: u(110), flex: "none", ...type("small"), color: C.muted }}>{label}</span>
      {children}
    </div>
  )
}

/**
 * A task open in the task pane (`views/task_detail.rs`): the task's
 * markdown file drawn as a document, its frontmatter as properties (status,
 * workflow, branch) over the Description and Acceptance criteria sections.
 * On hover the next acceptance item checks off, the way an edit lands back
 * in the file.
 */
export function DesktopTaskFileVignette() {
  const status = STATUS.yellow
  return (
    <MiniWindow lights={false}>
      <div style={{ display: "flex", alignItems: "center", gap: u(8), height: u(52), padding: `0 ${u(12)}`, background: C.toolbar }}>
        <span style={{ ...type("baseSemibold"), color: C.loud, flex: 1 }}>Task / metrics-api-endpoint</span>
        {hi(EllipsisVerticalIcon, C.buttonIcon)}
        {hi(XMarkIcon, C.buttonIcon)}
      </div>
      <div style={{ display: "flex", flexDirection: "column", padding: `${u(16)} ${u(20)} ${u(18)}` }}>
        <div style={{ fontSize: u(24), lineHeight: u(32), fontWeight: 600, color: C.loud, marginBottom: u(12) }}>Metrics API endpoint</div>
        <Prop label="Status">
          <span style={{ display: "flex", alignItems: "center", gap: u(6), ...type("smallMedium"), color: C.loud }}>
            <StatusGlyph kind={status.kind} size={u(16)} color={status.panelColor} />
            In Progress
            <FigmaIcon kind="chevron-down-small" size={u(18)} color={C.buttonIcon} />
          </span>
        </Prop>
        <Prop label="Workflow">
          <span style={{ display: "flex", alignItems: "center", gap: u(6), ...type("smallMedium"), color: C.loud }}>
            app
            <FigmaIcon kind="chevron-down-small" size={u(18)} color={C.buttonIcon} />
          </span>
        </Prop>
        <Prop label="Branch">
          <span style={{ fontFamily: "var(--font-mono)", fontSize: u(13), color: C.fg }}>jlong/metrics-api-endpoint</span>
        </Prop>
        <div style={{ height: u(1), background: C.divider, margin: `${u(12)} 0` }} />
        <div style={{ ...type("baseSemibold"), color: C.loud, marginBottom: u(6) }}>Acceptance criteria</div>
        {["Returns signups, active users, and revenue", "Respects from/to; defaults to 30 days"].map((t) => (
          <div key={t} style={{ display: "flex", alignItems: "center", gap: u(10), height: u(28), ...type("base"), color: C.fg }}>
            <Checkbox checked />
            {t}
          </div>
        ))}
        <div style={{ display: "flex", alignItems: "center", gap: u(10), height: u(28), ...type("base"), color: C.fg }}>
          <span style={{ display: "grid" }}>
            <span className="transition-opacity duration-300 motion-safe:group-hover:opacity-0" style={{ gridArea: "1 / 1" }}>
              <Checkbox checked={false} />
            </span>
            <span className="opacity-0 transition-opacity duration-300 motion-safe:group-hover:opacity-100" style={{ gridArea: "1 / 1" }}>
              <Checkbox checked />
            </span>
          </span>
          Empty ranges read 0, not null
        </div>
      </div>
    </MiniWindow>
  )
}
