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
 * The story leads with the positioning's single most convincing moment: the
 * user types a lazy one-liner at the orchestrator ("the events log rotation
 * thing from yesterday, fix it") and the orchestrator turns it into a crisp,
 * scoped task — title, three acceptance-style scope lines, dispatched to a
 * worker. Lazy in, crisp out. From there (see the beat comments in
 * `buildKeyframes`): a second one-liner is captured to the backlog without
 * touching anything in flight; the board shows the scoped task travel
 * backlog → todo → in-progress as alpha picks it up; the ⌃P palette filters
 * down to alpha and we watch it work; then the finished task lands in REVIEW
 * with a Ready-for-Review sidebar entry, and the loop wraps.
 *
 * The timeline is one flat list of keyframes (`{ state, hold, opacity }`) so
 * durations and content are all tweakable in one place. A single `setTimeout`
 * chain walks the list and wraps back to the start. Typing (beats 2 and 4) is
 * done by feeding `chatInput` the growing message in the bottom input box;
 * the orchestrator's replies hand `AppMockup` a progressively-built transcript
 * — no engine typing logic, just different `chatLines`/`chatInput` per frame.
 *
 * The Chat and worker panes render as real coding-agent sessions: the
 * transcript scrolls above a bottom-pinned `❯` input box and a
 * Model/Cost/Session footer (see `buildClaudeCodeRows` in `KanbanMockup`).
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
 * a static backdrop (see `RESIDENT_*`), and the two new tasks animate through
 * it as the stars of the story.
 */

// ── Story dataset ─────────────────────────────────────────────────────
// Two tasks born from two lazy one-liners. The rotation task is the star: it
// gets scoped, dispatched to alpha, worked, and handed off for review. The
// CLI-help task exists to show capture — it lands in the backlog mid-story and
// stays there, untouched, while the rotation task travels the board.
type Task = { id: string; title: string; worker?: string }

const ROTATE: Task = { id: "rotate-events-log", title: "Rotate events.log", worker: "alpha" }
const CAPTURED: Task = { id: "fix-stale-cli-help", title: "Fix stale CLI help" }

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
// items (workers charlie on hub + golf on devbox, so the IN PROGRESS column is
// never empty and the sidebar always shows live workers), and a few lived-in
// BACKLOG/TO DO items. The beat-1 transcript references this same backdrop —
// the audit-logging and profile-cache items merged overnight (DONE), charlie
// and golf are the long runners, and the ratelimit item is promoted to TO DO —
// so Chat and Tasks read as one system.
//
// Kept deliberately short so the tallest column stays under the pinned
// `minBodyRows` (32): each card is 3 grid rows, so the deepest column here is
// well inside the fixed frame.
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
// machines read as an active system before the rotation task dispatches.
const RESIDENT_WORKING: Record<string, string> = {
  charlie: "Developer",
  golf: "Developer",
}

/**
 * The five-column board. The story's animated cards (`opts`) render on top of
 * the resident backdrop in each column, so the board always looks lived-in: a
 * filled DONE column, an IN PROGRESS column that's never empty, and a couple of
 * standing BACKLOG/TO DO items behind the animated tasks. Column labels are the
 * TUI's uppercase renderings of the default stages (backlog, todo, in-progress,
 * review, done — see `column_label` in `crates/shelbi-tui/src/kanban.rs`).
 */
function cols(opts: {
  backlog?: Card[]
  todo?: Card[]
  progress?: Card[]
  review?: Card[]
}): Column[] {
  return [
    { label: "BACKLOG", category: "gray", cards: [...(opts.backlog ?? []), ...RESIDENT_BACKLOG] },
    { label: "TO DO", category: "blue", cards: [...(opts.todo ?? []), ...RESIDENT_TODO] },
    {
      label: "IN PROGRESS",
      category: "yellow",
      cards: [...(opts.progress ?? []), ...RESIDENT_PROGRESS],
    },
    { label: "REVIEW", category: "magenta", cards: opts.review ?? [] },
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
// The ⌃P palette's command list for the watch-the-worker beats: the three nav
// views, the Zen toggle, then every workspace across both machines (generated
// from `MACHINES` so the hub/devbox split matches the sidebar exactly), then
// the project/session actions. Only labels are fuzzy-matched, so typing
// `alpha` narrows this to the single alpha row (no other label is a
// subsequence of `alpha`), which is then activated.
const PALETTE_ITEMS: PaletteItem[] = [
  { glyph: "💬", label: "Chat", desc: "talk to the orchestrator" },
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

// The query the palette is typed to — narrows the list to the alpha
// workspace, which the loop then activates.
const PALETTE_QUERY = "alpha"

// The scenario every beat spreads from — an established `my-project` project on
// the Chat view. `minBodyRows` pins the pane height so the frame never resizes
// as columns fill/empty or panes swap. It's tuned to hold the framed window at
// a fixed 640px tall: the window is a 28px title bar over the terminal-body
// `<pre>`, whose rows render at the 17px line-height in `PRE_STYLE`. The body
// wraps `minBodyRows` with 4 chrome rows (title + blank + blank + footer), so
// the pre is `minBodyRows + 4` rows; at 32 that's 36 × 17px = 612px, and
// 28 + 612 = 640px exactly. That sits well above the tallest natural content,
// so shorter beats pad with blank rows and taller content clips/scrolls within
// this fixed area — and the headroom gives the Chat and worker transcripts
// room to read as fuller, established sessions.
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
// The two lazy one-liners the user types at the orchestrator. The first is the
// positioning's example verbatim: underspecified, conversational, no title, no
// scope. The second lands mid-dispatch to show capture without derailing.
const REQUEST = "the events log rotation thing from yesterday, fix it"
const REQUEST_2 = "also the cli help output is stale"
const CURSOR = "▊"

function userLine(text: string): ChatLine {
  return { kind: "user", text }
}

// The prior exchange the loop opens on (beat 1): a morning status pass over
// the same work the resident board backdrop shows — the audit-logging and
// profile-cache items merged overnight (they sit in DONE), charlie and golf
// are the long runners (IN PROGRESS), and the ratelimit item gets promoted
// (it sits in TO DO). It establishes the orchestrator as the one agent that
// tracks everything before the lazy request lands, and it's long enough
// (~26 rendered rows against a ~27-row transcript area) that the pane reads
// as the middle of an established session rather than a fresh start. The
// conversation is bottom-anchored in the fixed frame, so the oldest lines
// clip as later beats stream in more output — the newest turns always sit by
// the input box.
const PRIOR_EXCHANGE: ChatLine[] = [
  userLine("Morning. What landed overnight?"),
  { kind: "blank" },
  {
    kind: "prose",
    text: "Two review items merged while you were out: the audit logging task and the profile cache. charlie is still trimming the vendor bundle on hub, and golf is backfilling the order index on devbox.",
  },
  { kind: "blank" },
  userLine("Did the smoke run pass?"),
  { kind: "blank" },
  {
    kind: "prose",
    text: "Green in 41 minutes. The flaky teardown did not reproduce after the retry fix.",
  },
  { kind: "blank" },
  userLine("Anything waiting on me?"),
  { kind: "blank" },
  {
    kind: "prose",
    text: "No. golf is on the last partition, about twenty minutes out. No worker is paused on a question.",
  },
  { kind: "blank" },
  userLine("Close out what merged and queue the ratelimit work next."),
  { kind: "blank" },
  { kind: "prose", text: "Accepting both merged tasks and promoting the ratelimit item." },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: "shelbi task move add-audit-logging done" },
  { kind: "result", text: "✓ add-audit-logging → done" },
  { kind: "tool", name: "Bash", args: "shelbi task promote add-api-ratelimit" },
  { kind: "result", text: "✓ add-api-ratelimit → todo" },
  { kind: "blank" },
  { kind: "prose", text: "Done. add-api-ratelimit is first in line for the next free workspace." },
]

// What the orchestrator streams after the lazy request: recall of yesterday's
// context, a `shelbi task add`, the three scope lines it wrote onto the task,
// then the dispatch call. This is the hero moment — the one-line request above
// it and the crisp task below it sit in the same pane, so the contrast IS the
// frame.
const STREAM_TAIL: ChatLine[] = [
  { kind: "blank" },
  {
    kind: "prose",
    text: "That's the unbounded growth from Tuesday's soak run: the hub appends to events.log and never rotates it. Writing it up.",
  },
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: 'shelbi task add "Rotate events.log at a size threshold"' },
  { kind: "result", text: "✓ rotate-events-log created in backlog (priority 2)" },
  { kind: "blank" },
  { kind: "prose", text: "Scope on the task:" },
  { kind: "prose", text: "· rotate when events.log passes 10 MB, keep the last 5 files" },
  { kind: "prose", text: "· reopen the hub's write handle after rotation, no dropped events" },
  { kind: "prose", text: "· a test that forces a rotation and checks both" },
  { kind: "blank" },
  { kind: "prose", text: "Scoped enough to run unattended. Dispatching to alpha." },
]

// The reply to the second one-liner: captured to the backlog, nothing in
// flight touched — the orchestrator doesn't do the work, so there's nothing
// to derail by dumping more on it.
const STREAM_2_TAIL: ChatLine[] = [
  { kind: "blank" },
  { kind: "tool", name: "Bash", args: 'shelbi task add "Fix stale CLI help"' },
  { kind: "result", text: "✓ fix-stale-cli-help created in backlog (priority 3)" },
  { kind: "blank" },
  { kind: "prose", text: "Captured. It waits in the backlog; nothing in flight was touched." },
]

// The user's first message, once sent, sits in the conversation just below the
// prior exchange; beat 3 streams the orchestrator's reply beneath it. The
// second message stacks under that full reply.
const SENT: ChatLine[] = [...PRIOR_EXCHANGE, { kind: "blank" }, userLine(REQUEST)]
const REPLIED: ChatLine[] = [...SENT, ...STREAM_TAIL]
const SENT_2: ChatLine[] = [...REPLIED, { kind: "blank" }, userLine(REQUEST_2)]

/** Conversation with the first message + the first `n` streamed reply lines. */
function streamUpTo(n: number): ChatLine[] {
  return [...SENT, ...STREAM_TAIL.slice(0, n)]
}

/** Conversation with the second message + the first `n` lines of its reply. */
function stream2UpTo(n: number): ChatLine[] {
  return [...SENT_2, ...STREAM_2_TAIL.slice(0, n)]
}

// The focused worker (alpha) doing believable work on the rotation task. Its
// first turn is the dispatched task itself, rendered as a `❯` user prompt —
// mirroring how a real dispatch seeds the worker's session with the task
// title/body the orchestrator wrote. From there it reads the code the task
// touches, implements rotation + the handle reopen, catches a prune off-by-one,
// then writes the forced-rotation test and runs the suite. It's the payoff of
// the scoping beat: the worker executes the crisp task without asking anything.
// It nearly fills the ~27-row transcript area, so the pane reads as work deep
// in progress rather than a session that just began. (Worker `user` prompts
// render on one row, so the prompt is kept under the ~108-cell pane width.)
const WORKER_BASE: ChatLine[] = [
  {
    kind: "user",
    text: "Rotate events.log when it passes 10 MB. Keep the last 5 files and reopen the hub's write handle.",
  },
  { kind: "blank" },
  { kind: "prose", text: "Reading the task and the code that writes the log." },
  { kind: "tool", name: "Read", args: "tasks/rotate-events-log.md" },
  { kind: "tool", name: "Read", args: "src/events/log.ts" },
  { kind: "tool", name: "Read", args: "src/events/writer.ts" },
  {
    kind: "prose",
    text: "The writer opens one append handle at boot and holds it for the life of the process. I'll check size on each write and rotate behind that call.",
  },
  { kind: "tool", name: "Edit", args: "src/events/rotate.ts" },
  { kind: "prose", text: "+41 -0  ·  rotate() shifts events.log through .1 to .5, prunes the oldest." },
  { kind: "tool", name: "Edit", args: "src/events/writer.ts" },
  { kind: "prose", text: "+18 -3  ·  Size check on write; reopen the handle after each rotation." },
  { kind: "blank" },
  { kind: "prose", text: "The task pins the failure mode: no event may drop between close and reopen." },
  { kind: "tool", name: "Edit", args: "src/events/writer.ts" },
  { kind: "prose", text: "+9 -2  ·  Writes during rotation buffer, then flush to the fresh handle." },
  { kind: "tool", name: "Bash", args: "npm run typecheck" },
  { kind: "result", text: "✓ no type errors" },
  { kind: "blank" },
  { kind: "prose", text: "The prune window was off by one and kept six files. Tightened it." },
  { kind: "tool", name: "Edit", args: "src/events/rotate.ts" },
  { kind: "prose", text: "+2 -2  ·  Prune keeps exactly the last 5." },
  { kind: "blank" },
  { kind: "prose", text: "Now the test the task asks for: force a rotation, check the pruned set, count every line." },
  { kind: "tool", name: "Edit", args: "src/events/rotate.test.ts" },
  { kind: "tool", name: "Bash", args: "npm test -- rotate" },
]
const WORKER_DONE: ChatLine[] = [
  ...WORKER_BASE,
  { kind: "result", text: "✓ 9 passed" },
  { kind: "status", model: "Opus 4.8", ctx: "6%", cost: "$0.31" },
]

// ── Timeline ──────────────────────────────────────────────────────────
type Keyframe = { state: AppState; hold: number; opacity: number }

function frame(over: Partial<AppState>, hold: number, opacity = 1): Keyframe {
  return { state: { ...BASE, ...over }, hold, opacity }
}

// The board mid-dispatch — the rotation task in IN PROGRESS on alpha, the
// captured CLI-help task waiting in BACKLOG. Reused as the base for the
// palette + worker-pane beats and as the reduced-motion static frame.
const DISPATCHED: Partial<AppState> = {
  activeView: "tasks",
  columns: cols({
    backlog: [card(CAPTURED)],
    progress: [card(ROTATE, true)],
  }),
  machines: machinesFor({ ...RESIDENT_WORKING, alpha: "Developer" }),
}

// The closing beat — alpha handed the work off: the rotation task sits in
// REVIEW, alpha is idle again, and the sidebar shows the branch ready to
// review with its served location.
const IN_REVIEW: Partial<AppState> = {
  activeView: "tasks",
  columns: cols({
    backlog: [card(CAPTURED)],
    review: [card(ROTATE)],
  }),
  machines: machinesFor(RESIDENT_WORKING),
  readyReview: [
    { title: "Rotate events.log", branch: "shelbi/rotate-events-log", location: "hub:4001" },
  ],
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

  // Beat 2 — the lazy request, typed character by character in the bottom
  // input box: no title, no scope, "from yesterday" doing all the work.
  for (let i = 1; i <= REQUEST.length; i += 1) {
    k.push(frame(chat(PRIOR_EXCHANGE, REQUEST.slice(0, i) + CURSOR), 30))
  }
  k.push(frame(chat(PRIOR_EXCHANGE, REQUEST + CURSOR), 500)) // hold the full line
  // "Send" — the message jumps up into the conversation and the input clears.
  k.push(frame(chat(SENT, ""), 350))

  // Beat 3 — the orchestrator turns it into a scoped task, streaming into the
  // conversation area above the (now empty) input box: recall of the context,
  // the `task add`, then the three scope lines one at a time (the hero
  // moment, so each bullet gets its own frame and the full scope holds), then
  // the dispatch line.
  k.push(frame(chat(streamUpTo(2), ""), 950)) // recall: "That's the unbounded growth…"
  k.push(frame(chat(streamUpTo(5), ""), 900)) // ✓ rotate-events-log created
  k.push(frame(chat(streamUpTo(8), ""), 550)) // "Scope on the task:" + line 1
  k.push(frame(chat(streamUpTo(9), ""), 550)) // scope line 2
  k.push(frame(chat(streamUpTo(10), ""), 1300)) // scope line 3 + hold the full scope
  k.push(frame(chat(REPLIED, ""), 900)) // "Dispatching to alpha."

  // Beat 4 — a second one-liner lands mid-dispatch and is captured to the
  // backlog: dump work on the orchestrator as it occurs to you; it assigns
  // rather than executes, so nothing derails.
  for (let i = 1; i <= REQUEST_2.length; i += 1) {
    k.push(frame(chat(REPLIED, REQUEST_2.slice(0, i) + CURSOR), 30))
  }
  k.push(frame(chat(REPLIED, REQUEST_2 + CURSOR), 400))
  k.push(frame(chat(SENT_2, ""), 350))
  k.push(frame(chat(stream2UpTo(3), ""), 850)) // ✓ fix-stale-cli-help created
  k.push(frame(chat(stream2UpTo(5), ""), 1100)) // "Captured. … nothing in flight was touched."

  // Beat 5 — switch to Tasks and watch the scoped task travel the board:
  // BACKLOG → TO DO → IN PROGRESS on alpha, the sidebar flipping alpha to
  // working on pickup. The captured CLI-help task stays put in BACKLOG the
  // whole way — the contrast between the two one-liners, on the board.
  k.push(
    frame(
      { activeView: "tasks", columns: cols({ backlog: [card(ROTATE), card(CAPTURED)] }) },
      800,
    ),
  )
  k.push(
    frame(
      {
        activeView: "tasks",
        columns: cols({ backlog: [card(CAPTURED)], todo: [card(ROTATE)] }),
      },
      700,
    ),
  )
  k.push(frame(DISPATCHED, 1150))

  // Beat 6 — open the ⌃P command palette over the dashboard: the centered
  // modal appears with an empty `>` prompt and the full command list, its top
  // row highlighted (the board stays put underneath).
  const paletteFrame = (query: string, hold: number): Keyframe =>
    frame({ ...DISPATCHED, palette: { query, items: PALETTE_ITEMS } }, hold)
  k.push(paletteFrame("", 800))

  // Beat 7 — type the filter query one character at a time; the list fuzzy-
  // filters down as it goes until only the alpha workspace remains, highlighted
  // as the top match. Hold the resolved `alpha` frame a beat longer so it reads.
  for (let i = 1; i <= PALETTE_QUERY.length; i += 1) {
    const done = i === PALETTE_QUERY.length
    k.push(paletteFrame(PALETTE_QUERY.slice(0, i), done ? 950 : 260))
  }

  // Beat 8 — activate alpha (Enter): the palette closes and the view lands on
  // alpha's session, where we WATCH IT WORK the scoped task. Rather than dump
  // the whole transcript at once, the work STREAMS in a few lines per frame and
  // the pane auto-scrolls to follow: it's bottom-anchored against the input
  // box, so as the transcript outgrows the visible area the oldest lines clip
  // off the top and the newest sit by the prompt — reading as live, ongoing
  // work, not a static block. The reveal steps trace the implementation (read
  // the code → rotation + reopen → the no-drop fix → the prune off-by-one →
  // tests) and their holds sum to a sustained ~5s of scrolling before it lands
  // green. The pane wears the same session chrome as the orchestrator Chat —
  // transcript above, an idle input box + footer below, titled with the
  // worker's name.
  const worker = (lines: ChatLine[]): Partial<AppState> => ({
    ...DISPATCHED,
    activeView: "workspace",
    focusedWorkspace: "alpha",
    workspaceLines: lines,
    chatInput: "",
  })
  // A mid-work frame: the first `n` transcript lines with a live `Working…`
  // spinner pinned at the end (its elapsed time + token count climbing), so
  // every streaming frame reads as actively working.
  const streaming = (n: number, secs: number, tokens: number): ChatLine[] => [
    ...WORKER_BASE.slice(0, n),
    { kind: "working", text: `Working… (${secs}s · ↓ ${tokens} tokens)` },
  ]
  // [linesRevealed, hold] per streaming frame — each step lands on a clean beat
  // of the work (a finished read, an edit + its diffstat, a passing check). The
  // final step (n = 25) is the full transcript with the suite running; holds
  // sum to ~5s of continuous scrolling.
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

  // Beat 9 — the work comes back: the board again, the rotation task now in
  // REVIEW, alpha idle, and the sidebar's Ready-for-Review entry showing the
  // branch and its served location. The lazy one-liner is finished work
  // waiting on the human, and the loop's story is complete.
  k.push(frame(IN_REVIEW, 1800))

  // Beat 10 — fade the pane CONTENTS out (the window chrome stays static);
  // wrapping to beat 1 (opacity 1) cross-fades the contents back in on a fresh
  // Chat view, so the window sits still and its contents dissolve/reappear.
  k.push(frame(IN_REVIEW, 650, 0))

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
