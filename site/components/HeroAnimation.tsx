"use client"

import { useEffect, useRef, useState } from "react"
import {
  AppMockup,
  type AppState,
  type Card,
  type ChatLine,
  type Column,
  type Machine,
} from "./KanbanMockup"

/**
 * The autoplaying hero: a looping story of using Shelbi end to end, told as a
 * timeline of beats that mutate an `AppState` over time and render it through
 * the existing `AppMockup` engine (same Segment/Row grid + palette as the docs
 * mockups — this file adds no terminal rendering of its own).
 *
 * The story (see the beat comments in `buildKeyframes`): you talk to the
 * orchestrator in the Chat pane; it breaks a plan into tasks that stream in;
 * you switch to the board; the backlog fills; tasks promote and dispatch to
 * workers; you watch one worker's Claude-Code session; then its contents fade
 * and the loop wraps.
 *
 * The timeline is one flat list of keyframes (`{ state, hold, opacity }`) so
 * durations and content are all tweakable in one place. A single `setTimeout`
 * chain walks the list and wraps back to the start. Typing (beat 2) is done by
 * feeding `chatInput` the growing message in the bottom input box; task
 * streaming (beat 3) hands `AppMockup` a progressively-built transcript — no
 * engine typing logic, just different `chatLines`/`chatInput` per frame.
 *
 * The Chat and worker panes render as real Claude-Code sessions: the transcript
 * scrolls above a bottom-pinned `❯` input box and a Model/Cost/Session footer
 * (see `buildClaudeCodeRows` in `KanbanMockup`).
 *
 * Accessibility + cost: `prefers-reduced-motion` renders a single static frame
 * (the board mid-dispatch) with no timers; an IntersectionObserver pauses the
 * loop while the hero is scrolled off-screen. The frame is pinned to a fixed
 * width (5 columns) and height (`minBodyRows`) so it never resizes as the story
 * advances — shorter content is padded, taller content clips/scrolls — and the
 * loop-reset fade (`opacity`) targets only the pane contents, leaving the window
 * chrome static.
 */

// ── Story dataset ─────────────────────────────────────────────────────
// The `dashboard.md` plan → an Analytics Dashboard feature, six coherent
// tasks. The first three are created via the chat stream and dispatched to
// alpha/bravo/charlie; the other three fill the backlog behind them.
type Task = { id: string; title: string; worker?: string }

const TASKS: Task[] = [
  { id: "metrics-api-endpoint", title: "Metrics API endpoint", worker: "alpha" },
  { id: "chart-components", title: "Chart components (line + bar)", worker: "bravo" },
  { id: "date-range-filter", title: "Date-range filter", worker: "charlie" },
  { id: "csv-export", title: "CSV export" },
  { id: "dashboard-empty-states", title: "Empty + loading states" },
  { id: "dashboard-tests", title: "Dashboard tests" },
]

/** A backlog/board card for a task; `withWorker` attaches its `@workspace`. */
function card(t: Task, withWorker = false): Card {
  return withWorker && t.worker
    ? { title: t.title, id: t.id, workspace: t.worker }
    : { title: t.title, id: t.id }
}

/** The five-column board, with only the columns this story uses populated. */
function cols(opts: { backlog?: Card[]; todo?: Card[]; progress?: Card[] }): Column[] {
  return [
    { label: "BACKLOG", category: "gray", cards: opts.backlog ?? [] },
    { label: "TO DO", category: "blue", cards: opts.todo ?? [] },
    { label: "IN PROGRESS", category: "yellow", cards: opts.progress ?? [] },
    { label: "REVIEW", category: "magenta", cards: [] },
    { label: "DONE", category: "green", cards: [] },
  ]
}

/** One `hub` machine; each named workspace flips to working/<agent>, rest idle. */
function machinesFor(working: Record<string, string>): Machine[] {
  const names = ["alpha", "bravo", "charlie", "delta"]
  return [
    {
      name: "hub",
      workspaces: names.map((name) =>
        working[name]
          ? { name, state: "working" as const, agent: working[name] }
          : { name, state: "idle" as const },
      ),
    },
  ]
}

// The scenario every beat spreads from — a fresh `my-project` project on the
// Chat view. `minBodyRows` pins the pane height so the frame never resizes as
// columns fill/empty or panes swap. It's set well above the tallest natural
// content (a 6-card backlog is only 18 rows: header + 6×2 card rows + 5 gaps)
// to hold the window at a ~1.6 width:height aspect ratio: the board pane is a
// fixed 111 cells wide (1 + 5×22) which, at the 13px / 0.6em-advance mono font,
// is ~866px; a 1.6:1 window is ~541px tall; minus the 28px title bar that
// leaves ~30 body rows, and the body wraps `minBodyRows` with 4 chrome rows
// (title + blank + blank + footer), so 30 − 4 = 26. Shorter beats pad with
// blank rows; taller content clips/scrolls within this fixed area.
const BASE: AppState = {
  terminalTitle: "jlong@hub — my-project",
  project: "my-project",
  activeView: "chat",
  minBodyRows: 26,
  columns: cols({}),
  machines: machinesFor({}),
  readyReview: [],
  queuedReview: [],
}

// ── Chat + worker transcripts ─────────────────────────────────────────
const USER_MSG = "Take the @dashboard.md plan and break it down into tasks."
const CURSOR = "▊"

function userLine(text: string): ChatLine {
  return { kind: "user", text }
}

// The prior exchange already scrolled into the conversation at beat 1: the user
// and Claude just wrote `dashboard.md` together and Claude finished, so the new
// request (beat 2) reads as the natural next step. Kept short so the input box +
// footer still fit under it in the fixed frame.
const PRIOR_EXCHANGE: ChatLine[] = [
  userLine("Let's plan an analytics dashboard for the app."),
  { kind: "blank" },
  { kind: "prose", text: "On it. I'll draft dashboard.md — metrics, charts, filters, export." },
  { kind: "tool", name: "Write", args: "dashboard.md" },
  { kind: "result", text: "48 lines written" },
  {
    kind: "prose",
    text: "Done. dashboard.md covers a metrics API, chart components, a date-range filter, CSV export, empty/loading states, and tests. Want me to break it into tasks?",
  },
]

// What the orchestrator streams after the user's message: a line of narration
// then three `shelbi task add` calls, each with its `⎿ ✓ … created` result.
const STREAM_TAIL: ChatLine[] = [
  { kind: "blank" },
  { kind: "prose", text: "Reading dashboard.md and breaking it into tasks." },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: 'shelbi task add "Metrics API endpoint"' },
  { kind: "result", text: "✓ metrics-api-endpoint created" },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: 'shelbi task add "Chart components (line + bar)"' },
  { kind: "result", text: "✓ chart-components created" },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: 'shelbi task add "Date-range filter"' },
  { kind: "result", text: "✓ date-range-filter created" },
]

// The user's message, once sent, sits in the conversation just below the prior
// exchange; beat 3 streams the orchestrator's reply beneath it.
const SENT: ChatLine[] = [...PRIOR_EXCHANGE, { kind: "blank" }, userLine(USER_MSG)]

/** Conversation with the sent message + the first `n` streamed reply lines. */
function streamUpTo(n: number): ChatLine[] {
  return [...SENT, ...STREAM_TAIL.slice(0, n)]
}

// The focused worker (alpha) doing believable work on "Metrics API endpoint":
// a read, an edit with a diffstat, then a passing test run + the session footer.
const WORKER_BASE: ChatLine[] = [
  { kind: "tool", name: "Read", args: "api/metrics.ts" },
  { kind: "tool", name: "Edit", args: "api/metrics.ts" },
  { kind: "prose", text: "+38 -4  ·  Added GET /api/metrics with range params." },
  { kind: "tool", name: "Bash", args: "npm test -- metrics" },
]
const WORKER_RUNNING: ChatLine[] = [
  ...WORKER_BASE,
  { kind: "working", text: "Working… (3s · ↓ 128 tokens)" },
]
const WORKER_DONE: ChatLine[] = [
  ...WORKER_BASE,
  { kind: "result", text: "✓ 12 passed" },
  { kind: "status", model: "Opus 4.8", ctx: "7%", cost: "$0.42" },
]

// ── Timeline ──────────────────────────────────────────────────────────
type Keyframe = { state: AppState; hold: number; opacity: number }

function frame(over: Partial<AppState>, hold: number, opacity = 1): Keyframe {
  return { state: { ...BASE, ...over }, hold, opacity }
}

// Beat 6's end state — every task dispatched: alpha/bravo/charlie each working a
// task in IN PROGRESS, the other three waiting in TO DO. Reused as the base for
// the worker-pane beat and as the reduced-motion static frame.
const DISPATCHED: Partial<AppState> = {
  activeView: "tasks",
  columns: cols({
    todo: TASKS.slice(3).map((t) => card(t)),
    progress: TASKS.slice(0, 3).map((t) => card(t, true)),
  }),
  machines: machinesFor({ alpha: "Developer", bravo: "Developer", charlie: "Developer" }),
}

/** The reduced-motion representative frame: the board mid-dispatch. */
const STATIC_STATE: AppState = { ...BASE, ...DISPATCHED }

function buildKeyframes(): Keyframe[] {
  const k: Keyframe[] = []
  // A Chat beat: the conversation transcript above, and whatever's sitting in
  // the bottom input box (`input`) — empty once a message is sent.
  const chat = (lines: ChatLine[], input: string): Partial<AppState> => ({
    activeView: "chat",
    chatLines: lines,
    chatInput: input,
  })

  // Beat 1 — Chat view: the prior exchange is already scrolled into the
  // conversation and the bottom input box waits empty with a blinking cursor.
  k.push(frame(chat(PRIOR_EXCHANGE, CURSOR), 900))

  // Beat 2 — type the user's message character by character in the bottom input
  // box (the conversation above is unchanged).
  for (let i = 1; i <= USER_MSG.length; i += 1) {
    k.push(frame(chat(PRIOR_EXCHANGE, USER_MSG.slice(0, i) + CURSOR), 30))
  }
  k.push(frame(chat(PRIOR_EXCHANGE, USER_MSG + CURSOR), 450)) // hold the full line
  // "Send" — the message jumps up into the conversation and the input clears.
  k.push(frame(chat(SENT, ""), 350))

  // Beat 3 — the orchestrator streams task creations one at a time into the
  // conversation area above the (now empty) input box. Reveal the narration,
  // then each `Bash(shelbi task add …)` + `✓ … created` in turn.
  k.push(frame(chat(streamUpTo(2), ""), 700)) // narration
  k.push(frame(chat(streamUpTo(5), ""), 750)) // metrics-api-endpoint
  k.push(frame(chat(streamUpTo(8), ""), 750)) // chart-components
  k.push(frame(chat(streamUpTo(11), ""), 950)) // date-range-filter

  // Beat 4 — switch to Tasks; the three created tasks are already in BACKLOG.
  k.push(
    frame({ activeView: "tasks", columns: cols({ backlog: TASKS.slice(0, 3).map((t) => card(t)) }) }, 1000),
  )

  // Beat 5 — the other three tasks pop into the backlog, one at a time.
  for (let n = 4; n <= 6; n += 1) {
    k.push(
      frame(
        { activeView: "tasks", columns: cols({ backlog: TASKS.slice(0, n).map((t) => card(t)) }) },
        n === 6 ? 800 : 550,
      ),
    )
  }

  // Beat 6 — promote + dispatch. BACKLOG → TO DO, then TO DO → IN PROGRESS one
  // worker at a time, each pickup flipping its sidebar workspace to working.
  k.push(
    frame(
      {
        activeView: "tasks",
        columns: cols({
          backlog: TASKS.slice(3).map((t) => card(t)),
          todo: TASKS.slice(0, 3).map((t) => card(t)),
        }),
      },
      750,
    ),
  )
  k.push(frame({ activeView: "tasks", columns: cols({ todo: TASKS.map((t) => card(t)) }) }, 700))
  k.push(
    frame(
      {
        activeView: "tasks",
        columns: cols({
          todo: TASKS.slice(1).map((t) => card(t)),
          progress: [card(TASKS[0], true)],
        }),
        machines: machinesFor({ alpha: "Developer" }),
      },
      750,
    ),
  )
  k.push(
    frame(
      {
        activeView: "tasks",
        columns: cols({
          todo: TASKS.slice(2).map((t) => card(t)),
          progress: TASKS.slice(0, 2).map((t) => card(t, true)),
        }),
        machines: machinesFor({ alpha: "Developer", bravo: "Developer" }),
      },
      750,
    ),
  )
  k.push(frame(DISPATCHED, 950)) // date-range-filter → charlie; all dispatched

  // Beat 7 — observe a worker: focus alpha's Claude-Code pane on its task,
  // running the test then landing green with the session footer. The pane wears
  // the same Claude-Code chrome as the orchestrator Chat — transcript above, an
  // idle (empty) input box + footer below, titled with the worker's name.
  const worker = (lines: ChatLine[]): Partial<AppState> => ({
    ...DISPATCHED,
    activeView: "workspace",
    focusedWorkspace: "alpha",
    workspaceLines: lines,
    chatInput: "",
  })
  k.push(frame(worker(WORKER_RUNNING), 1300))
  k.push(frame(worker(WORKER_DONE), 1900))

  // Beat 8 — fade the pane CONTENTS out (the window chrome stays static);
  // wrapping to beat 1 (opacity 1) cross-fades the contents back in on a fresh
  // Chat view, so the window sits still and its contents dissolve/reappear.
  k.push(frame(worker(WORKER_DONE), 650, 0))

  return k
}

const KEYFRAMES = buildKeyframes()

export function HeroAnimation() {
  const [index, setIndex] = useState(0)
  const [visible, setVisible] = useState(true)
  const [reduced, setReduced] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)

  // Respect prefers-reduced-motion: no autoplay, just the static frame.
  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)")
    const update = () => setReduced(mq.matches)
    update()
    mq.addEventListener("change", update)
    return () => mq.removeEventListener("change", update)
  }, [])

  // Pause the loop while the hero is scrolled off-screen to save CPU.
  useEffect(() => {
    const el = containerRef.current
    if (!el) return
    const obs = new IntersectionObserver(([entry]) => setVisible(entry.isIntersecting), {
      threshold: 0.15,
    })
    obs.observe(el)
    return () => obs.disconnect()
  }, [])

  // Advance the timeline: schedule the next frame after the current hold, and
  // wrap forever. Re-runs on visibility/reduced changes so pausing (cleanup
  // clears the pending timer) and resuming both fall out naturally.
  useEffect(() => {
    if (reduced || !visible) return
    const t = window.setTimeout(() => {
      setIndex((i) => (i + 1) % KEYFRAMES.length)
    }, KEYFRAMES[index].hold)
    return () => window.clearTimeout(t)
  }, [index, visible, reduced])

  if (reduced) {
    return (
      <div ref={containerRef}>
        <AppMockup state={STATIC_STATE} />
      </div>
    )
  }

  const kf = KEYFRAMES[index]
  return (
    <div ref={containerRef}>
      <AppMockup state={kf.state} contentOpacity={kf.opacity} />
    </div>
  )
}
