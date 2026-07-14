"use client"

import { AnimatePresence, motion, useInView, useReducedMotion } from "motion/react"
import { useEffect, useRef, useState } from "react"
import {
  BoardMockup,
  TerminalFrame,
  type AppState,
  type Column,
  type Machine,
} from "./KanbanMockup"
import { BANNER_LINES } from "./Wordmark"

const STAGE_MS = 6000

const WORKSPACES: Machine[] = [
  {
    name: "hub",
    workspaces: ["alpha", "bravo", "charlie", "delta", "review"].map((name) => ({
      name,
      state: "idle" as const,
    })),
  },
]

function columns(welcomeColumn: "BACKLOG" | "TO DO"): Column[] {
  return [
    {
      label: "BACKLOG",
      category: "gray",
      cards:
        welcomeColumn === "BACKLOG"
          ? [{ title: "Welcome to Shelbi", id: "welcome-to-shelbi", selected: true }]
          : [],
    },
    {
      label: "TO DO",
      category: "blue",
      cards:
        welcomeColumn === "TO DO"
          ? [{ title: "Welcome to Shelbi", id: "welcome-to-shelbi", workspace: "alpha" }]
          : [],
    },
    { label: "IN PROGRESS", category: "yellow", cards: [] },
    { label: "REVIEW", category: "magenta", cards: [] },
    { label: "DONE", category: "green", cards: [] },
  ]
}

function dashboardState(dispatched: boolean): AppState {
  return {
    terminalTitle: "you@laptop - myapp",
    project: "myapp",
    activeView: "tasks",
    workflow: "task",
    columns: columns(dispatched ? "TO DO" : "BACKLOG"),
    machines: dispatched
      ? [
          {
            ...WORKSPACES[0],
            workspaces: WORKSPACES[0].workspaces.map((workspace) =>
              workspace.name === "alpha"
                ? { ...workspace, state: "working", agent: "Developer" }
                : workspace,
            ),
          },
        ]
      : WORKSPACES,
    readyReview: [],
    queuedReview: [],
    minBodyRows: 32,
  }
}

const INSTALL_SCREEN = [
  "$ brew install jlong/shelbi/shelbi && shelbi",
  "",
  ...BANNER_LINES,
  "an open-source agent orchestrator for the terminal",
]

const PREFLIGHT_SCREEN = [
  "  ✓ git repo            ~/code/myapp",
  "  ✓ default branch      main",
  "  ✓ remote              github.com:you/myapp.git",
  "  ✓ agent               codex 0.27.0 on PATH",
  "  ✓ tmux                3.5a",
  "  ✓ machine             10 cores, recommending 4 workspaces",
]

const CARD_WIDTH = 58
const CARD_TITLE = "─ myapp "
const cardRow = (content = "") => `  │${content.padEnd(CARD_WIDTH)}│`
const CARD_SCREEN = [
  `  ┌${CARD_TITLE}${"─".repeat(CARD_WIDTH - CARD_TITLE.length)}┐`,
  cardRow(),
  cardRow("  repo        ~/code/myapp (main)"),
  cardRow("  github      github.com:you/myapp.git"),
  cardRow("  agent       codex"),
  cardRow("  workspaces  alpha bravo charlie delta + review (hub)"),
  cardRow("  workflows   task (branch → PR → review) · subtask"),
  cardRow("  agents      orchestrator · developer · review"),
  cardRow("              (+ qa, security, adversarial, opt-in)"),
  cardRow(),
  cardRow('  Everything above is editable later: Ctrl+Space → "Edit"'),
  `  └${"─".repeat(CARD_WIDTH)}┘`,
  "",
  "  Enter launch    c customize    q quit",
]

const STATIC_SCREEN = [
  "$ brew install jlong/shelbi/shelbi && shelbi",
  "",
  ...PREFLIGHT_SCREEN,
  "",
  "  One detected plan: Enter launch    c customize    q quit",
  "  ✓ Project myapp created.",
  "",
  "  BACKLOG  Welcome to Shelbi",
  "  Ctrl+P palette · type E to edit settings",
]

const STAGES = [
  { label: "Install and start", kind: "terminal" as const, lines: INSTALL_SCREEN },
  { label: "Visible preflight", kind: "terminal" as const, lines: PREFLIGHT_SCREEN },
  { label: "One confirmation", kind: "terminal" as const, lines: CARD_SCREEN },
  { label: "Dashboard ready", kind: "dashboard" as const, dispatched: false },
  { label: "Welcome card dispatches", kind: "dashboard" as const, dispatched: true },
]

function TerminalScreen({ lines, compact = false }: { lines: string[]; compact?: boolean }) {
  return (
    <TerminalFrame title="you@laptop - myapp" bodyClassName="overflow-x-auto">
      <pre
        className="m-0 min-w-[92ch] whitespace-pre border-0 bg-transparent p-4 font-mono text-xs leading-relaxed text-[var(--tui-fg)] sm:p-5 sm:text-sm"
        style={{ minHeight: compact ? undefined : 612 }}
      >
        {lines.join("\n")}
      </pre>
    </TerminalFrame>
  )
}

function AccessibleStory() {
  return (
    <ol className="sr-only">
      <li>Install and start with brew install jlong/shelbi/shelbi and shelbi.</li>
      <li>Shelbi visibly checks the Git repo, branch, remote, runner, tmux, and machine.</li>
      <li>The detected plan offers Enter to launch, c to customize, or q to quit.</li>
      <li>The dashboard opens with idle workspaces and a Welcome to Shelbi card in Backlog.</li>
      <li>Promoting the Welcome card dispatches it to the alpha workspace.</li>
    </ol>
  )
}

export function HeroAnimation() {
  const [stage, setStage] = useState(0)
  const containerRef = useRef<HTMLElement>(null)
  const inView = useInView(containerRef, { amount: 0.15 })
  const reducedMotion = useReducedMotion()

  useEffect(() => {
    if (!inView || reducedMotion) return
    const timer = window.setTimeout(() => {
      setStage((current) => (current + 1) % STAGES.length)
    }, STAGE_MS)
    return () => window.clearTimeout(timer)
  }, [inView, reducedMotion, stage])

  if (reducedMotion) {
    return (
      <section ref={containerRef} className="border-b border-gray-4 px-3 py-6 sm:py-10">
        <div className="mx-auto w-fit max-w-full">
          <p className="mb-2 font-mono text-xs text-gray-7">First run, at a glance</p>
          <TerminalScreen lines={STATIC_SCREEN} compact />
        </div>
        <AccessibleStory />
      </section>
    )
  }

  const current = STAGES[stage]
  return (
    <section ref={containerRef} className="border-b border-gray-4 px-3 py-6 sm:py-10">
      <div className="mx-auto mb-2 flex w-full max-w-6xl items-baseline justify-between gap-2 font-mono text-xs text-gray-7">
        <p>{current.label}</p>
        <p className="tabular-nums">{stage + 1} / {STAGES.length}</p>
      </div>
      <div className="mx-auto w-fit max-w-full" aria-hidden="true">
        <AnimatePresence initial={false} mode="wait">
          <motion.div
            key={stage}
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.16, ease: "easeOut" }}
          >
            {current.kind === "terminal" ? (
              <TerminalScreen lines={current.lines} />
            ) : (
              <BoardMockup state={dashboardState(current.dispatched)} />
            )}
          </motion.div>
        </AnimatePresence>
      </div>
      <AccessibleStory />
    </section>
  )
}
