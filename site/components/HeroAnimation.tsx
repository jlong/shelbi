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
 * loop-reset fade (`opacity`) targets only the panes' text layer: the glyphs
 * cross-fade while the terminal body background and window chrome stay fully
 * opaque.
 *
 * The board reads as an established project, not an empty demo: a filled DONE
 * column of prior work and a couple of already-running IN PROGRESS items sit as
 * a static backdrop (see `RESIDENT_*`), and the six dashboard tasks animate
 * through it as the stars of the story.
 */

// ── Story dataset ─────────────────────────────────────────────────────
// The `dashboard.md` plan → an Analytics Dashboard feature, six coherent
// tasks. The first three are created via the chat stream; the other three fill
// the backlog behind them. The dispatch beat then assigns the first FOUR across
// both machines — alpha & bravo on the `hub`, echo & foxtrot on the `devbox` —
// so the sidebar shows multi-machine orchestration; the last two wait in TO DO.
type Task = { id: string; title: string; worker?: string }

const TASKS: Task[] = [
  { id: "metrics-api-endpoint", title: "Metrics API endpoint", worker: "alpha" },
  { id: "chart-components", title: "Chart components (line + bar)", worker: "echo" },
  { id: "date-range-filter", title: "Date-range filter", worker: "bravo" },
  { id: "csv-export", title: "CSV export", worker: "foxtrot" },
  { id: "dashboard-empty-states", title: "Empty + loading states" },
  { id: "dashboard-tests", title: "Dashboard tests" },
]

/** A backlog/board card for a task; `withWorker` attaches its `@workspace`. */
function card(t: Task, withWorker = false): Card {
  return withWorker && t.worker
    ? { title: t.title, id: t.id, workspace: t.worker }
    : { title: t.title, id: t.id }
}

// ── Resident backdrop ─────────────────────────────────────────────────
// The board isn't an empty demo — it's an established project with a history.
// These "resident" cards are present in every board beat and never animate: a
// filled DONE column of prior work, a couple of already-running IN PROGRESS
// items (workers charlie on hub + golf on devbox, distinct from the dashboard
// dispatch so the column is never empty and the sidebar always shows live
// workers), and a few lived-in BACKLOG/TO DO items. The six dashboard TASKS are
// still the stars — they animate through this backdrop (created → backlog →
// promoted → in progress), landing among these residents on dispatch.
//
// Kept deliberately short so the tallest column stays under the pinned
// `minBodyRows` (26): each card is 3 grid rows, so the deepest beat — a 6-card
// backlog + 2 residents = 8 cards → 24 rows — still fits without resizing the
// fixed frame.
const RESIDENT_DONE: Card[] = [
  { title: "Migrate to PG 16", id: "t-012" },
  { title: "Ship dark mode", id: "t-013" },
  { title: "Wire up OAuth", id: "t-034" },
  { title: "Add audit logging", id: "t-046" },
  { title: "Redis cache /profile", id: "t-027" },
  { title: "Fix flaky CI tests", id: "t-048" },
]
const RESIDENT_PROGRESS: Card[] = [
  { title: "Trim vendor bundle", id: "t-022", workspace: "charlie" },
  { title: "Backfill order index", id: "t-017", workspace: "golf" },
]
const RESIDENT_BACKLOG: Card[] = [
  { title: "Rework onboarding UX", id: "t-004" },
  { title: "Audit OSS licenses", id: "t-005" },
]
const RESIDENT_TODO: Card[] = [{ title: "Add API ratelimit", id: "t-007" }]

// The resident IN PROGRESS workers, always busy in the sidebar so the two
// machines read as an active system before the dashboard tasks dispatch.
const RESIDENT_WORKING: Record<string, string> = {
  charlie: "Developer",
  golf: "Developer",
}

/**
 * The five-column board. The story's animated cards (`opts`) render on top of
 * the resident backdrop in each column, so the board always looks lived-in: a
 * filled DONE column, an IN PROGRESS column that's never empty, and a couple of
 * standing BACKLOG/TO DO items behind the dashboard tasks.
 */
function cols(opts: { backlog?: Card[]; todo?: Card[]; progress?: Card[] }): Column[] {
  return [
    { label: "BACKLOG", category: "gray", cards: [...(opts.backlog ?? []), ...RESIDENT_BACKLOG] },
    { label: "TO DO", category: "blue", cards: [...(opts.todo ?? []), ...RESIDENT_TODO] },
    {
      label: "IN PROGRESS",
      category: "yellow",
      cards: [...(opts.progress ?? []), ...RESIDENT_PROGRESS],
    },
    { label: "REVIEW", category: "magenta", cards: [] },
    { label: "DONE", category: "green", cards: RESIDENT_DONE },
  ]
}

// Two machines — `hub` (alpha…delta) and `devbox` (echo…golf) — matching the
// mockup convention in `defaultAppState`, so the hero shows multi-machine
// orchestration. Each named workspace in `working` flips to working/<agent> on
// its own machine; every other workspace stays idle.
const MACHINES: { name: string; workspaces: string[] }[] = [
  { name: "hub", workspaces: ["alpha", "bravo", "charlie", "delta"] },
  { name: "devbox", workspaces: ["echo", "foxtrot", "golf"] },
]

/** Build both machine groups; names in `working` flip to working/<agent>, rest idle. */
function machinesFor(working: Record<string, string>): Machine[] {
  return MACHINES.map((m) => ({
    name: m.name,
    workspaces: m.workspaces.map((name) =>
      working[name]
        ? { name, state: "working" as const, agent: working[name] }
        : { name, state: "idle" as const },
    ),
  }))
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
  machines: machinesFor(RESIDENT_WORKING),
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

// Beat 6's end state — the first four tasks dispatched across BOTH machines:
// alpha & bravo (hub) and echo & foxtrot (devbox) each working a task in IN
// PROGRESS, the last two waiting in TO DO. Reused as the base for the
// worker-pane beat and as the reduced-motion static frame.
const DISPATCHED: Partial<AppState> = {
  activeView: "tasks",
  columns: cols({
    todo: TASKS.slice(4).map((t) => card(t)),
    progress: TASKS.slice(0, 4).map((t) => card(t, true)),
  }),
  machines: machinesFor({
    ...RESIDENT_WORKING,
    alpha: "Developer",
    bravo: "Developer",
    echo: "Developer",
    foxtrot: "Developer",
  }),
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

  // Beat 6 — promote + dispatch across BOTH machines. BACKLOG → TO DO, then
  // TO DO → IN PROGRESS one worker at a time, each pickup flipping its sidebar
  // workspace to working. The workers alternate hub (alpha, bravo) and devbox
  // (echo, foxtrot), so the sidebar lights up on both machines at once.
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
  // Dispatch the first three one at a time — metrics → alpha (hub), chart → echo
  // (devbox), date-range → bravo (hub) — accumulating the working set so each
  // frame shows one more workspace busy.
  const working: Record<string, string> = {}
  for (let n = 1; n <= 3; n += 1) {
    const picked = TASKS[n - 1]
    if (picked.worker) working[picked.worker] = "Developer"
    k.push(
      frame(
        {
          activeView: "tasks",
          columns: cols({
            todo: TASKS.slice(n).map((t) => card(t)),
            progress: TASKS.slice(0, n).map((t) => card(t, true)),
          }),
          machines: machinesFor({ ...RESIDENT_WORKING, ...working }),
        },
        750,
      ),
    )
  }
  k.push(frame(DISPATCHED, 950)) // csv-export → foxtrot (devbox); four dispatched across hub + devbox

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
