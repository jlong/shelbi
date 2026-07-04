"use client"

import { useEffect, useRef, useState } from "react"
import {
  AppMockup,
  type AppState,
  type Card,
  type ChatLine,
  type Column,
  type Machine,
  type PaletteItem,
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
 * workers; you open the ⌃P command palette and type to filter it down to the
 * alpha workspace, then activate it to watch alpha's Claude-Code session; then
 * its contents fade and the loop wraps.
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
// `minBodyRows` (32): each card is 3 grid rows, so the deepest beat — a 6-card
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

// ── Command palette ───────────────────────────────────────────────────
// The ⌃P palette's command list for the ending beats: the three nav views, the
// Zen toggle, then every workspace across both machines (generated from
// `MACHINES` so the hub/devbox split matches the sidebar exactly), then the
// project/session actions. Only labels are fuzzy-matched, so typing `alpha`
// narrows this to the single alpha row (no other label is a subsequence of
// `alpha`), which is then activated.
const PALETTE_ITEMS: PaletteItem[] = [
  { glyph: "💬", label: "Chat", desc: "the claude pane you talk to" },
  { glyph: "📋", label: "Tasks", desc: "live `shelbi list`" },
  { glyph: "⚡", label: "Activity", desc: "human-readable events feed" },
  { glyph: "⚡", label: "Turn Zen Mode on", desc: "currently off" },
  ...MACHINES.flatMap((m) =>
    m.workspaces.map((name) => ({
      glyph: "·",
      label: name,
      desc: `workspace · ${m.name}`,
    })),
  ),
  { glyph: "⚡", label: "Switch Project", desc: "fuzzy-pick another project and swap the dashboard" },
  { glyph: "⚡", label: "Quit Project", desc: "close every pane and switch to the next project" },
  { glyph: "⚡", label: "Quit Shelbi", desc: "close every Shelbi session on this host" },
]

// The query the palette is typed to at the end — narrows the list to the alpha
// workspace, which the loop then activates.
const PALETTE_QUERY = "alpha"

// The scenario every beat spreads from — a fresh `my-project` project on the
// Chat view. `minBodyRows` pins the pane height so the frame never resizes as
// columns fill/empty or panes swap. It's tuned to hold the framed window at a
// fixed 640px tall: the window is a 28px title bar over the terminal-body
// `<pre>`, whose rows render at the 17px line-height in `PRE_STYLE`. The body
// wraps `minBodyRows` with 4 chrome rows (title + blank + blank + footer), so
// the pre is `minBodyRows + 4` rows; at 32 that's 36 × 17px = 612px, and
// 28 + 612 = 640px exactly. That sits well above the tallest natural content (a
// 6-card backlog + 2 residents = 8 cards → 24 rows), so shorter beats pad with
// blank rows and taller content clips/scrolls within this fixed area — and the
// extra headroom over the old 26-row pin gives the Chat and worker transcripts
// more room to read as fuller, established sessions.
const BASE: AppState = {
  terminalTitle: "jlong@hub — my-project",
  project: "my-project",
  activeView: "chat",
  minBodyRows: 32,
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

// The prior exchange the loop opens on (beat 1) is deliberately long — a
// substantive design conversation about the analytics dashboard: yesterday's
// API-cleanup landing rolls into a back-and-forth on the metric tiles, the
// charts, a CSV export, and the empty/loading states, then Claude writes
// `dashboard.md` and asks to break it up. It's long enough (~35 rendered rows
// against a ~29-row transcript area) that the earliest turns are already
// clipped off the top at beat 1, so the pane reads as the MIDDLE of an
// established session rather than a fresh start. The conversation is bottom-
// anchored in the fixed frame, so those oldest lines stay clipped as later
// beats stream in more output — the newest turns always sit by the input box,
// and the whole design thread lands on the break-it-down prompt (beat 2).
const PRIOR_EXCHANGE: ChatLine[] = [
  userLine("Morning. Where did the API cleanup land yesterday?"),
  { kind: "blank" },
  {
    kind: "prose",
    text: "Both tasks cleared review overnight — the rate-limit and the search pagination work merged to main. The board's clear.",
  },
  { kind: "blank" },
  userLine("Good. Let's design the analytics dashboard we sketched last week."),
  { kind: "blank" },
  {
    kind: "prose",
    text: "Sure. Starting from the mock: a metrics overview across the top, charts beneath, and a date-range filter driving the whole page. What's the headline metric?",
  },
  { kind: "blank" },
  userLine("Signups, active users, and revenue — as big number tiles with week-over-week deltas."),
  { kind: "blank" },
  {
    kind: "prose",
    text: "Got it. I'll have the tiles read from one /api/metrics endpoint so a single request backs the whole header row.",
  },
  { kind: "blank" },
  userLine("And the charts underneath?"),
  { kind: "blank" },
  {
    kind: "prose",
    text: "A line chart for the trend and a bar chart for the plan breakdown. Both share the date range, so changing it re-queries once and both redraw.",
  },
  { kind: "blank" },
  userLine("Add a way to pull the raw numbers out too — finance keeps asking."),
  { kind: "blank" },
  {
    kind: "prose",
    text: "A CSV export scoped to the current filter, then. I'll also handle the empty and loading states so it never paints blank on first load.",
  },
  { kind: "blank" },
  userLine("Perfect. Write it up so we don't lose the thread."),
  { kind: "blank" },
  {
    kind: "prose",
    text: "On it. I'll draft dashboard.md — metrics API, charts, date-range filter, CSV export, empty/loading states, and tests.",
  },
  { kind: "blank" },
  { kind: "tool", name: "Write", args: "dashboard.md" },
  { kind: "result", text: "48 lines written" },
  { kind: "blank" },
  { kind: "prose", text: "Done. dashboard.md covers all six. Want me to break it into tasks?" },
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

// The focused worker (alpha) doing believable work on "Metrics API endpoint".
// Its first turn is the dispatched task itself, rendered as a `❯` user prompt —
// mirroring how a real dispatch seeds the worker's session with the task
// title/body. From there it reads the spec + the routes it's extending,
// implements the query, endpoint, response type, and tests (each edit with a
// diffstat), coalesces the empty-range case, then runs the suite + lands the
// session footer. It's the fullest transcript in the story and the newest
// content the eye lands on at the end of the loop: it fills the ~29-row
// transcript area exactly, so the task prompt sits flush at the very top with
// no blank padding above it and the work stacks solid down to the pinned input
// box — the pane reads as work already deep in progress rather than a session
// that just began. (Worker `user` prompts render on one row, so the prompt is
// kept under the ~108-cell pane width to stay fully readable at the top edge.)
const WORKER_BASE: ChatLine[] = [
  {
    kind: "user",
    text: "Implement the metrics API endpoint from dashboard.md — GET /api/metrics with from/to range params.",
  },
  { kind: "blank" },
  { kind: "prose", text: "Reading the spec and the route I'll extend." },
  { kind: "tool", name: "Read", args: "dashboard.md" },
  { kind: "tool", name: "Read", args: "api/metrics.ts" },
  { kind: "tool", name: "Read", args: "db/queries.ts" },
  {
    kind: "prose",
    text: "The tiles need signups, active users, and revenue in one payload, so I'll aggregate all three in a single range-keyed query.",
  },
  { kind: "tool", name: "Edit", args: "db/queries.ts" },
  { kind: "prose", text: "+29 -2  ·  Added metricsForRange() with a grouped aggregate." },
  { kind: "tool", name: "Edit", args: "api/metrics.ts" },
  {
    kind: "prose",
    text: "+38 -4  ·  Added GET /api/metrics; validates from/to and defaults to the last 30 days.",
  },
  { kind: "blank" },
  { kind: "prose", text: "Wiring the response type through so the client tiles stay typed." },
  { kind: "tool", name: "Edit", args: "types/metrics.ts" },
  { kind: "prose", text: "+11 -0  ·  Exported a MetricsResponse shared by the API and the tiles." },
  { kind: "tool", name: "Bash", args: "npm run typecheck" },
  { kind: "result", text: "✓ no type errors" },
  { kind: "blank" },
  {
    kind: "prose",
    text: "Empty ranges came back null — coalescing to zero so the tiles show 0, not blank.",
  },
  { kind: "tool", name: "Edit", args: "api/metrics.ts" },
  { kind: "prose", text: "+3 -1  ·  Coalesced empty aggregates to zero." },
  { kind: "blank" },
  { kind: "prose", text: "Now the tests — valid ranges, a missing param, and the empty-range case." },
  { kind: "tool", name: "Edit", args: "api/metrics.test.ts" },
  { kind: "tool", name: "Bash", args: "npm test -- metrics" },
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

  // Beat 7 — open the ⌃P command palette over the dashboard: the centered modal
  // appears with an empty `>` prompt and the full command list, its top row
  // highlighted (the board stays put underneath, dimmed by the scrim).
  const paletteFrame = (query: string, hold: number): Keyframe =>
    frame({ ...DISPATCHED, palette: { query, items: PALETTE_ITEMS } }, hold)
  k.push(paletteFrame("", 800))

  // Beat 8 — type the filter query one character at a time; the list fuzzy-
  // filters down as it goes until only the alpha workspace remains, highlighted
  // as the top match. Hold the resolved `alpha` frame a beat longer so it reads.
  for (let i = 1; i <= PALETTE_QUERY.length; i += 1) {
    const done = i === PALETTE_QUERY.length
    k.push(paletteFrame(PALETTE_QUERY.slice(0, i), done ? 950 : 260))
  }

  // Beat 9 — activate alpha (Enter): the palette closes and the view lands on
  // alpha's Claude-Code pane, where we WATCH IT WORK. Rather than dump the whole
  // transcript at once, the work STREAMS in a few lines per frame and the pane
  // auto-scrolls to follow: it's bottom-anchored against the input box, so as the
  // transcript outgrows the visible area the oldest lines clip off the top and
  // the newest sit by the prompt — reading as live, ongoing work, not a static
  // block. The reveal steps trace the implementation (read spec → aggregate query
  // → endpoint → typecheck → empty-range fix → tests) and their holds sum to a
  // sustained ~5s of scrolling before it lands green with the session footer. The
  // pane wears the same Claude-Code chrome as the orchestrator Chat — transcript
  // above, an idle input box + footer below, titled with the worker's name.
  const worker = (lines: ChatLine[]): Partial<AppState> => ({
    ...DISPATCHED,
    activeView: "workspace",
    focusedWorkspace: "alpha",
    workspaceLines: lines,
    chatInput: "",
  })
  // A mid-work frame: the first `n` transcript lines with a live `Working…`
  // spinner pinned at the end (its elapsed time + token count climbing), so every
  // streaming frame reads as actively working.
  const streaming = (n: number, secs: number, tokens: number): ChatLine[] => [
    ...WORKER_BASE.slice(0, n),
    { kind: "working", text: `Working… (${secs}s · ↓ ${tokens} tokens)` },
  ]
  // [linesRevealed, hold] per streaming frame — each step lands on a clean beat
  // of the work (a finished read, an edit + its diffstat, a passing check). The
  // final step (n = 25) is the full transcript with the suite running; holds sum
  // to ~5s of continuous scrolling.
  const STREAM_STEPS: [number, number][] = [
    [3, 600],
    [6, 550],
    [9, 550],
    [11, 500],
    [15, 550],
    [17, 500],
    [21, 550],
    [24, 550],
    [25, 650],
  ]
  STREAM_STEPS.forEach(([n, hold], i) => {
    k.push(frame(worker(streaming(n, i + 1, 128 + i * 112)), hold))
  })
  k.push(frame(worker(WORKER_DONE), 1400)) // lands green with the session footer

  // Beat 10 — fade the pane CONTENTS out (the window chrome stays static);
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
