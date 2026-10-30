"use client"

import type { CSSProperties, ReactNode } from "react"
import { ArrowLeftIcon, ChevronRightIcon, EllipsisVerticalIcon, PlusIcon } from "@heroicons/react/24/outline"
import { REVIEW_INSTRUCTIONS_SOURCE } from "../KanbanMockup"
import { FigmaIcon, StatusGlyph, type StatusKind } from "./DesktopIcons"
import { C, DesktopWindow, SmallButton, STATUS, type, u } from "./DesktopMockup"

/**
 * The Desktop variants of the value-prop editors: where the TUI shows an
 * agent's `instructions.md` and a workflow's YAML open in vim, the desktop
 * edits the same files in Settings, so these draw Settings ▸ Agents' agent
 * editor (`views/agent_editor.rs`) and Settings ▸ Workflows' Builder
 * (`views/workflow_builder.rs`), with the same content as the TUI mockups:
 * the Adversarial Review agent and the `feature` workflow.
 *
 * Both editors take the whole window (the app hides the sidebar column while
 * one is open, so the header's back button sits beside the traffic lights)
 * and are fluid, as the app's layouts are: the window fills its slot, and
 * one point scales with the slot's width through a container query unit, so
 * the edge-bleeding value-prop panels stay proportioned at any viewport.
 */

/** One point for an editor window in a slot `container-type: inline-size` wide. */
const EDITOR_SCALE = { ["--u" as string]: "clamp(0.3px, calc(100cqw / 900), 0.8px)" } as CSSProperties
const EDITOR_H = 860

/** The editor's header band: back (beside the traffic lights), the name and its badge, `center`, Save changes and ⋮. */
function EditorHeader({ name, badge, center }: { name: string; badge?: string; center?: ReactNode }) {
  return (
    <div style={{ position: "relative", display: "flex", flex: "none", alignItems: "center", height: u(52), padding: `0 ${u(16)} 0 ${u(91)}`, background: C.sidebar, gap: u(8) }}>
      <span style={{ display: "flex", alignItems: "center", justifyContent: "center", width: u(32), height: u(32), color: C.buttonIcon }}>
        <ArrowLeftIcon aria-hidden="true" style={{ width: u(20), height: u(20) }} />
      </span>
      <span style={{ ...type("baseSemibold"), color: C.loud }}>{name}</span>
      {badge ? <span style={{ padding: `0 ${u(4)}`, borderRadius: u(2), background: C.fill, ...type("xsmallMedium"), color: C.fg }}>{badge}</span> : null}
      {center ? <div style={{ position: "absolute", left: "50%", transform: "translateX(-50%)" }}>{center}</div> : null}
      <span style={{ flex: 1 }} />
      <SmallButton select style={{ opacity: 0.5 }}>
        Save changes
      </SmallButton>
      <EllipsisVerticalIcon aria-hidden="true" style={{ width: u(20), height: u(20), color: C.buttonIcon, flex: "none" }} />
    </div>
  )
}

function Field({ label, children, style }: { label: string; children: ReactNode; style?: CSSProperties }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: u(8), ...style }}>
      <span style={{ ...type("base"), color: C.fg }}>{label}</span>
      {children}
    </div>
  )
}

/** A text field: the textbox well (`surface/depth-0`) with its `stroke/dark` edge. */
function TextBox({ value }: { value: string }) {
  return (
    <div style={{ display: "flex", alignItems: "center", height: u(32), padding: `0 ${u(8)}`, borderRadius: u(6), background: C.card, border: `${u(1)} solid ${C.inputBorder}`, ...type("base"), color: C.loud, whiteSpace: "nowrap", overflow: "hidden" }}>
      {value}
    </div>
  )
}

/** A dropdown: the overlay fill, the value, the chevron. */
function Select({ value }: { value: string }) {
  return (
    <div style={{ display: "flex", alignItems: "center", height: u(32), padding: `0 ${u(8)} 0 ${u(12)}`, borderRadius: u(6), background: C.fill, ...type("smallMedium"), color: C.loud }}>
      <span style={{ flex: 1 }}>{value}</span>
      <FigmaIcon kind="chevron-down-small" size={u(20)} color={C.buttonIcon} />
    </div>
  )
}

const sectionTitle = (text: string) => <span style={{ ...type("baseSemibold"), color: C.loud }}>{text}</span>
const footnote = (text: string) => <span style={{ ...type("small"), color: C.muted }}>{text}</span>

/** "Agents provide specialization": the Adversarial Review agent open in Settings ▸ Agents. */
export function DesktopAgentEditorMockup() {
  return (
    <div style={{ containerType: "inline-size" }}>
      <div style={EDITOR_SCALE}>
        <DesktopWindow height={EDITOR_H} label="The Shelbi desktop app's agent editor, showing the adversarial review agent's instructions">
          <EditorHeader name="adversarial" badge="Default" />
          <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0, gap: u(32), padding: `${u(32)} ${u(32)} 0`, overflow: "hidden" }}>
            <div style={{ display: "flex", gap: u(16) }}>
              <Field label="Name" style={{ width: u(240), flex: "none" }}>
                <TextBox value="adversarial" />
              </Field>
              <Field label="Description" style={{ flex: 1, minWidth: 0 }}>
                <TextBox value="Adversarial code review before a human sees the branch" />
              </Field>
            </div>
            <div style={{ display: "flex", flexWrap: "wrap", gap: u(32), flex: 1, minHeight: 0 }}>
              <div style={{ display: "flex", flexDirection: "column", gap: u(12), flex: `1 1 ${u(360)}`, minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "baseline" }}>
                  {sectionTitle("Instructions")}
                  <span style={{ flex: 1 }} />
                  <span style={{ ...type("small"), color: C.muted }}>Markdown</span>
                </div>
                {footnote("Define how this agent approaches a task and when its work is complete.")}
                <div
                  style={{
                    height: u(330),
                    padding: u(16),
                    borderRadius: u(8),
                    background: C.card,
                    border: `${u(1)} solid ${C.inputBorder}`,
                    overflow: "hidden",
                    fontFamily: "var(--font-mono)",
                    fontSize: u(14),
                    lineHeight: u(18),
                    color: C.fg,
                    whiteSpace: "pre-wrap",
                  }}
                >
                  {REVIEW_INSTRUCTIONS_SOURCE.join("\n")}
                </div>
                {footnote("Shared project instructions are included before these instructions.")}
                <div style={{ display: "flex", alignItems: "center", gap: u(8), marginTop: u(16) }}>
                  {sectionTitle("Skills")}
                  <span style={{ ...type("small"), color: C.muted }}>0</span>
                  <span style={{ flex: 1 }} />
                  <SmallButton style={{ paddingLeft: u(8) }}>
                    <PlusIcon aria-hidden="true" style={{ width: u(18), height: u(18), color: C.buttonIcon }} />
                    Add skill
                  </SmallButton>
                </div>
                <div style={{ display: "flex", flexDirection: "column", gap: u(8), padding: u(16), borderRadius: u(8), border: `${u(1)} solid ${C.inputBorder}` }}>
                  <span style={{ ...type("baseMedium"), color: C.fg }}>No skills added</span>
                  {footnote("Add reusable instructions for tasks this agent performs often.")}
                </div>
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: u(12), flex: `0 1 ${u(380)}`, minWidth: 0 }}>
                {sectionTitle("Runner preferences")}
                {footnote("Choose the runner and defaults recommended for this role.")}
                <Field label="Preferred runner">
                  <Select value="Claude Code" />
                </Field>
                <Field label="Model">
                  <Select value="Use runner default" />
                </Field>
                <Field label="Reasoning effort">
                  <Select value="Use runner default" />
                </Field>
                <Field label="Permissions">
                  <Select value="Use project settings" />
                </Field>
                {footnote("Model and effort apply to Claude Code. Project settings can override these preferences.")}
                <div style={{ height: u(1), background: C.divider, margin: `${u(12)} 0` }} />
                {sectionTitle("Workflow assignments")}
                {footnote("This agent runs when a task enters the assigned status.")}
                <div style={{ display: "flex", alignItems: "center", gap: u(6), height: u(32), padding: `0 ${u(8)} 0 ${u(6)}`, borderRadius: u(6), background: C.fill, ...type("smallMedium"), color: C.loud }}>
                  <StatusGlyph kind="in-progress" size={u(20)} color={C.accent} />
                  <span style={{ flex: 1 }}>feature / Agent Review</span>
                  <ChevronRightIcon aria-hidden="true" style={{ width: u(20), height: u(20), color: C.buttonIcon }} />
                </div>
                {footnote("Manage assignments in Workflows.")}
              </div>
            </div>
          </div>
        </DesktopWindow>
      </div>
    </div>
  )
}

// ── Workflow Builder ──────────────────────────────────────────────────

type GraphNode = { id: string; name: string; kind: StatusKind; owner: string; x: number; y: number; initial?: boolean }

// The `feature` workflow the TUI mockup opens in vim: backlog → in-progress →
// agent-review → qa → security → review → done, with the developer,
// adversarial, QA and security agents owning the agent stages. Laid out in a
// snake so the whole pipeline fits the slot: three 200pt columns 104pt apart,
// 88pt rows 88pt apart.
const NODE_W = 200
const NODE_H = 88
const COL = NODE_W + 104
const ROW = NODE_H + 88
const NODES: GraphNode[] = [
  { id: "backlog", name: "Backlog", kind: "backlog", owner: "User · None", x: 0, y: 0, initial: true },
  { id: "in-progress", name: "In Progress", kind: "in-progress", owner: "Agent · developer", x: 0, y: ROW },
  { id: "agent-review", name: "Agent Review", kind: "in-progress", owner: "Agent · adversarial", x: COL, y: ROW },
  { id: "qa", name: "QA", kind: "in-progress", owner: "Agent · qa", x: 2 * COL, y: ROW },
  { id: "security", name: "Security", kind: "in-progress", owner: "Agent · security", x: 2 * COL, y: 2 * ROW },
  { id: "review", name: "Review", kind: "review", owner: "User · None", x: COL, y: 2 * ROW },
  { id: "done", name: "Done", kind: "done", owner: "User · None", x: 0, y: 2 * ROW },
]
const GRAPH_W = 2 * COL + NODE_W
const GRAPH_H = 2 * ROW + NODE_H

// Each transition, with the actions it runs (`workflow_editor.rs`
// `action_label`), drawn as the Builder draws one: a 1pt line ending in an
// open arrowhead, its actions over it.
const EDGES: { from: string; to: string; actions?: string[] }[] = [
  { from: "backlog", to: "in-progress" },
  { from: "in-progress", to: "agent-review", actions: ["Push branch", "Open PR"] },
  { from: "agent-review", to: "qa" },
  { from: "qa", to: "security" },
  { from: "security", to: "review" },
  { from: "review", to: "done", actions: ["Merge", "Delete branch"] },
]

/** The edge's endpoints: side to side across a row, bottom to top down a column. */
function edgeLine(from: GraphNode, to: GraphNode) {
  if (from.y === to.y) {
    const y = from.y + NODE_H / 2
    return from.x < to.x
      ? { x1: from.x + NODE_W, y1: y, x2: to.x, y2: y }
      : { x1: from.x, y1: y, x2: to.x + NODE_W, y2: y }
  }
  const x = from.x + NODE_W / 2
  return { x1: x, y1: from.y + NODE_H, x2: x, y2: to.y }
}

function Graph() {
  const byId = Object.fromEntries(NODES.map((n) => [n.id, n]))
  return (
    <div style={{ position: "relative", width: u(GRAPH_W), height: u(GRAPH_H), flex: "none" }}>
      <svg aria-hidden="true" viewBox={`0 0 ${GRAPH_W} ${GRAPH_H}`} style={{ position: "absolute", inset: 0, width: "100%", height: "100%", overflow: "visible" }}>
        {EDGES.map((e) => {
          const { x1, y1, x2, y2 } = edgeLine(byId[e.from], byId[e.to])
          const dx = Math.sign(x2 - x1)
          const dy = Math.sign(y2 - y1)
          // The open arrowhead: two 5pt strokes back from the tip.
          const head = dx
            ? `M${x2 - 5 * dx} ${y2 - 4}L${x2} ${y2}L${x2 - 5 * dx} ${y2 + 4}`
            : `M${x2 - 4} ${y2 - 5 * dy}L${x2} ${y2}L${x2 + 4} ${y2 - 5 * dy}`
          return (
            <g key={`${e.from}-${e.to}`} fill="none" stroke="var(--dt-icon)" strokeWidth={1.25} strokeLinecap="round" strokeLinejoin="round">
              <line x1={x1} y1={y1} x2={x2} y2={y2} />
              <path d={head} />
            </g>
          )
        })}
      </svg>
      {EDGES.filter((e) => e.actions).map((e) => {
        const { x1, y1, x2 } = edgeLine(byId[e.from], byId[e.to])
        return (
          <div
            key={`label-${e.from}`}
            style={{ position: "absolute", left: u(Math.min(x1, x2)), width: u(Math.abs(x2 - x1)), top: u(y1 - 40), display: "flex", flexDirection: "column", alignItems: "center", ...type("xsmall"), color: C.fg }}
          >
            {e.actions!.map((a) => (
              <span key={a}>{a}</span>
            ))}
          </div>
        )
      })}
      {NODES.map((n) => {
        const status = Object.values(STATUS).find((s) => s.kind === n.kind)!
        return (
          <div
            key={n.id}
            style={{ position: "absolute", left: u(n.x), top: u(n.y), width: u(NODE_W), height: u(NODE_H), padding: u(16), borderRadius: u(4), background: C.fill, display: "flex", flexDirection: "column", gap: u(8) }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: u(8) }}>
              <StatusGlyph kind={n.kind} size={u(20)} color={status.color} />
              <span style={{ ...type("baseSemibold"), color: C.loud }}>{n.name}</span>
            </div>
            <div style={{ display: "flex", gap: u(8), ...type("xsmall"), color: C.muted }}>
              {n.owner}
              {n.initial ? <span style={{ color: C.link }}>Initial</span> : null}
            </div>
          </div>
        )
      })}
    </div>
  )
}

/** The Builder | Form segmented control, Builder chosen. */
function ModeSwitch() {
  return (
    <div style={{ display: "flex", gap: u(2), padding: u(3), borderRadius: u(8), background: C.search }}>
      {["Builder", "Form"].map((m, i) => (
        <span key={m} style={{ display: "flex", alignItems: "center", height: u(26), padding: `0 ${u(12)}`, borderRadius: u(6), background: i === 0 ? C.canvas : undefined, ...type("baseMedium"), color: i === 0 ? C.loud : C.fg }}>
          {m}
        </span>
      ))}
    </div>
  )
}

const HINT = "Hover over a status, then drag a handle to add a transition."

/** "Workflows provide boundaries": the `feature` workflow open in the Workflow Builder. */
export function DesktopWorkflowBuilderMockup() {
  return (
    <div style={{ containerType: "inline-size" }}>
      <div style={EDITOR_SCALE}>
        <DesktopWindow height={EDITOR_H} label="The Shelbi desktop app's workflow builder, showing the feature workflow's statuses and transitions">
          <EditorHeader name="feature" center={<ModeSwitch />} />
          <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0, gap: u(16), padding: u(24), overflow: "hidden" }}>
            <div style={{ display: "flex", alignItems: "center", gap: u(8), flex: "none" }}>
              <SmallButton style={{ paddingLeft: u(8) }}>
                <PlusIcon aria-hidden="true" style={{ width: u(18), height: u(18), color: C.buttonIcon }} />
                Add status
              </SmallButton>
              <SmallButton style={{ paddingLeft: u(8) }}>
                <PlusIcon aria-hidden="true" style={{ width: u(18), height: u(18), color: C.buttonIcon }} />
                Add transition
              </SmallButton>
              <span style={{ ...type("small"), color: C.muted, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", minWidth: 0 }}>{HINT}</span>
              <span style={{ flex: 1 }} />
              <SmallButton>Fit</SmallButton>
              <SmallButton style={{ paddingRight: u(8) }}>
                100%
                <FigmaIcon kind="chevron-down-small" size={u(18)} color={C.buttonIcon} />
              </SmallButton>
            </div>
            <div
              style={{ flex: 1, minHeight: 0, borderRadius: u(4), border: `${u(1)} solid ${C.divider}`, display: "flex", flexDirection: "column", alignItems: "center", padding: `${u(28)} ${u(16)}`, gap: u(48), overflow: "hidden" }}
            >
              <div style={{ display: "flex", flexDirection: "column", gap: u(24) }}>
                <span style={{ ...type("small"), color: C.muted }}>{HINT}</span>
                <Graph />
                <div style={{ display: "flex", gap: u(40), ...type("small"), color: C.muted }}>
                  <span style={{ display: "flex", alignItems: "center", gap: u(8) }}>
                    <span style={{ width: u(18), borderTop: `${u(1.5)} dashed ${C.icon}` }} />
                    Status order
                  </span>
                  <span style={{ display: "flex", alignItems: "center", gap: u(8) }}>
                    <span style={{ width: u(18), borderTop: `${u(1.5)} solid ${C.icon}` }} />
                    Transition actions
                  </span>
                </div>
              </div>
            </div>
          </div>
        </DesktopWindow>
      </div>
    </div>
  )
}
