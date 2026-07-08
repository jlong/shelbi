"use client"

import { useEffect, useRef, useState } from "react"

/**
 * The homepage "problem" beat: the mess of terminal tabs, one agent per tab,
 * you as the router. The reader is a developer already living this, so the
 * section only names what they recognize: no argument that parallel agents are
 * good, no reassurance.
 *
 * Five beats sit in a numbered list on the left; a fixed-size panel on the
 * right dramatizes the active beat with a lightweight terminal-tab mockup (one
 * per beat). The section auto-advances through the beats on a timer and loops,
 * highlighting the active row and cross-fading the panel to its mockup. Hover,
 * focus, or click on a row jumps to that beat and syncs the panel. Respecting
 * the same rules as `HeroAnimation`: `prefers-reduced-motion` drops the timer
 * (the first mockup renders statically, hover/click still switches), and an
 * IntersectionObserver pauses the loop while the section is off-screen.
 *
 * The mockups are the PROBLEM, not Shelbi's board, so they are new lightweight
 * illustrations of bare terminal tabs (mono text, `gray-4` hairlines, the
 * `bg-bg`/`text-gray-6/7` palette) rather than a reuse of the AppMockup board.
 * The right panel is pinned to a fixed width and height so it never jumps as
 * the mockup content changes between beats.
 */

const beats: { lead: string; body: string }[] = [
  {
    lead: "Replied to the wrong tab.",
    body: "You typed instructions for one agent into another one's session.",
  },
  {
    lead: "The forgotten paused agent.",
    body: "An agent asked you a question two hours ago. It's still waiting, in a tab you forgot.",
  },
  {
    lead: "You are the scheduler.",
    body: "Who's done? Who's stuck? Who needs you? You've become a human scheduler for your own tools.",
  },
  {
    lead: "The backlog lives in your head.",
    body: "New work occurs to you while every agent is mid-task. Interrupting one derails it, so you carry the idea instead.",
  },
  {
    lead: "The review queue is you.",
    body: "Every new agent makes the pile taller, and you trust it less.",
  },
]

const ADVANCE_MS = 3500

// ── Mockup primitives ─────────────────────────────────────────────────
// A bare terminal window and its parts, styled in the site palette so the
// illustrations read as terminal tabs without pulling in the Shelbi board.

/** The terminal window frame: rounded gray-4 hairline over the page bg. */
function TermWindow({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex h-full w-full flex-col overflow-hidden rounded-lg border border-gray-4 bg-bg">
      {children}
    </div>
  )
}

/** Three restrained traffic-light dots for a window title bar. */
function TrafficLights() {
  return (
    <span className="flex gap-1.5" aria-hidden="true">
      <span className="h-2.5 w-2.5 rounded-full bg-gray-3" />
      <span className="h-2.5 w-2.5 rounded-full bg-gray-3" />
      <span className="h-2.5 w-2.5 rounded-full bg-gray-3" />
    </span>
  )
}

/** A window title bar: traffic lights plus an optional right-aligned label. */
function TitleBar({ label }: { label?: string }) {
  return (
    <div className="flex items-center gap-3 border-b border-gray-4 px-3 py-2 font-mono text-xs text-gray-5">
      <TrafficLights />
      {label ? <span className="ml-auto">{label}</span> : null}
    </div>
  )
}

/** One tab in a tab strip: label plus an optional state glyph. */
function Tab({ label, active, glyph }: { label: string; active?: boolean; glyph?: string }) {
  return (
    <div
      className={`flex items-center gap-1.5 border-r border-gray-4 px-3 py-2 ${
        active ? "bg-gray-2 text-fg" : "text-gray-5"
      }`}
    >
      {glyph ? <span aria-hidden="true">{glyph}</span> : null}
      <span>{label}</span>
    </div>
  )
}

/** A static block cursor (no animation, so it is calm under reduced motion). */
function Cursor({ dim }: { dim?: boolean }) {
  return (
    <span
      aria-hidden="true"
      className={`ml-0.5 inline-block h-3.5 w-1.5 align-middle ${dim ? "bg-gray-4" : "bg-fg"}`}
    />
  )
}

// ── The five beat mockups ─────────────────────────────────────────────

/** 1. A message meant for the API worker, typed into the auth worker's tab. */
function WrongTabMockup() {
  return (
    <TermWindow>
      <div className="flex border-b border-gray-4 font-mono text-xs">
        <Tab label="api-worker" />
        <Tab label="auth-worker" active />
        <div className="flex-1" />
      </div>
      <div className="flex-1 space-y-3 p-4 font-mono text-xs">
        <div className="text-gray-5">auth-worker · verifying JWT refresh</div>
        <div className="leading-relaxed">
          <span className="text-gray-6">❯ </span>
          <span className="text-fg">the /metrics endpoint should paginate by cursor</span>
          <Cursor />
        </div>
      </div>
    </TermWindow>
  )
}

/** 2. An agent's question left unanswered, with a stale "waiting 2h" stamp. */
function ForgottenPausedMockup() {
  return (
    <TermWindow>
      <div className="flex items-center border-b border-gray-4 font-mono text-xs">
        <Tab label="db-worker" active glyph="⏸" />
        <div className="flex-1" />
        <span className="px-3 text-gray-5">waiting 2h</span>
      </div>
      <div className="flex-1 space-y-3 p-4 font-mono text-xs">
        <div className="text-gray-6">
          ⏺ <span className="text-fg">Migrate the orders table</span>
        </div>
        <div className="pl-3 leading-relaxed text-gray-6">
          Should removed rows be a soft delete or a hard delete?
        </div>
        <div className="text-gray-5">
          ❯ <Cursor dim />
        </div>
        <div className="pt-2 text-gray-5">waiting for you · 2h ago</div>
      </div>
    </TermWindow>
  )
}

/** 3. A grid of sessions in mixed states you have to track by hand. */
function SchedulerMockup() {
  const sessions: { name: string; glyph: string; state: string; bold?: boolean }[] = [
    { name: "api-worker", glyph: "⏵", state: "running" },
    { name: "auth-worker", glyph: "‖", state: "waiting" },
    { name: "ui-worker", glyph: "!", state: "stuck", bold: true },
    { name: "db-worker", glyph: "⏵", state: "running" },
    { name: "cache-worker", glyph: "‖", state: "waiting" },
    { name: "tests-worker", glyph: "⏵", state: "running" },
    { name: "docs-worker", glyph: "!", state: "stuck", bold: true },
    { name: "deploy-worker", glyph: "‖", state: "waiting" },
  ]
  return (
    <TermWindow>
      <TitleBar label="8 sessions" />
      <div className="grid flex-1 grid-cols-2 gap-2 p-4 font-mono text-xs">
        {sessions.map((s) => (
          <div
            key={s.name}
            className="flex items-center justify-between rounded border border-gray-4 px-2 py-1.5"
          >
            <span className="truncate text-gray-6">{s.name}</span>
            <span
              className={`ml-2 shrink-0 ${s.bold ? "font-semibold text-fg" : "text-gray-5"}`}
            >
              <span aria-hidden="true">{s.glyph}</span> {s.state}
            </span>
          </div>
        ))}
      </div>
    </TermWindow>
  )
}

/** 4. Every tab busy, so a new idea floats unfiled over the running sessions. */
function BacklogMockup() {
  const busy = ["api-worker", "auth-worker", "ui-worker", "db-worker"]
  return (
    <div className="relative h-full w-full">
      <TermWindow>
        <TitleBar label="all busy" />
        <div className="flex-1 space-y-2 p-4 font-mono text-xs">
          {busy.map((name) => (
            <div key={name} className="flex items-center justify-between">
              <span className="text-gray-6">{name}</span>
              <span className="text-gray-5">
                <span aria-hidden="true">⏵</span> running
              </span>
            </div>
          ))}
        </div>
      </TermWindow>
      <div className="absolute right-3 bottom-3 max-w-[70%] rotate-3 rounded border border-gray-4 bg-gray-1 p-3 font-mono text-xs shadow-lg">
        <div className="text-fg">+ rate-limit the search API</div>
        <div className="mt-1 text-gray-5">unfiled</div>
      </div>
    </div>
  )
}

/** 5. A review pile taller than comfortable, fading off the bottom edge. */
function ReviewQueueMockup() {
  const prs: { title: string; add: number; del: number }[] = [
    { title: "feat: metrics API endpoint", add: 142, del: 8 },
    { title: "feat: chart components", add: 210, del: 4 },
    { title: "feat: date-range filter", add: 88, del: 12 },
    { title: "feat: CSV export", add: 64, del: 2 },
    { title: "fix: empty and loading states", add: 51, del: 9 },
  ]
  return (
    <TermWindow>
      <TitleBar label="review queue" />
      <div className="relative flex-1 overflow-hidden">
        <div className="space-y-2 p-4 font-mono text-xs">
          {prs.map((pr) => (
            <div
              key={pr.title}
              className="flex items-center justify-between rounded border border-gray-4 px-2 py-1.5"
            >
              <span className="truncate text-gray-6">{pr.title}</span>
              <span className="ml-2 shrink-0 text-gray-5">
                +{pr.add} -{pr.del}
              </span>
            </div>
          ))}
        </div>
        <div className="pointer-events-none absolute inset-x-0 bottom-0 h-20 bg-gradient-to-t from-bg to-transparent" />
        <div className="absolute right-4 bottom-2 font-mono text-xs text-gray-5">+7 more</div>
      </div>
    </TermWindow>
  )
}

const MOCKUPS = [
  WrongTabMockup,
  ForgottenPausedMockup,
  SchedulerMockup,
  BacklogMockup,
  ReviewQueueMockup,
]

export function ProblemSection() {
  const [active, setActive] = useState(0)
  const [visible, setVisible] = useState(true)
  const [reduced, setReduced] = useState(false)
  const containerRef = useRef<HTMLElement>(null)

  // Respect prefers-reduced-motion: no autoplay, just the first mockup (and
  // whatever the reader hovers/clicks to).
  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)")
    const update = () => setReduced(mq.matches)
    update()
    mq.addEventListener("change", update)
    return () => mq.removeEventListener("change", update)
  }, [])

  // Pause the loop while the section is scrolled off-screen to save CPU,
  // mirroring the HeroAnimation pattern.
  useEffect(() => {
    const el = containerRef.current
    if (!el) return
    const obs = new IntersectionObserver(([entry]) => setVisible(entry.isIntersecting), {
      threshold: 0.15,
    })
    obs.observe(el)
    return () => obs.disconnect()
  }, [])

  // Auto-advance and loop. Re-runs on active/visibility/reduced changes, so a
  // hover/click jump (which sets `active`) restarts the dwell from that beat,
  // and pausing/resuming falls out of the cleanup naturally.
  useEffect(() => {
    if (reduced || !visible) return
    const t = window.setTimeout(() => {
      setActive((i) => (i + 1) % beats.length)
    }, ADVANCE_MS)
    return () => window.clearTimeout(t)
  }, [active, visible, reduced])

  return (
    <section ref={containerRef} className="border-t border-gray-4">
      <div className="mx-auto w-full max-w-6xl px-4 py-6 lg:px-6 lg:py-8">
        <p className="font-mono text-xs font-medium uppercase tracking-[0.2em] text-accent">
          The old way
        </p>
        <h2 className="mt-2 max-w-3xl font-sans text-4xl font-semibold tracking-tight text-fg">
          One agent per tab. You&apos;re the router.
        </h2>

        <div className="mt-8 lg:grid lg:grid-cols-2 lg:gap-12">
          <ol className="border-t border-gray-4">
            {beats.map((beat, i) => {
              const isActive = i === active
              return (
                <li key={beat.lead} className="border-b border-gray-4">
                  <button
                    type="button"
                    aria-current={isActive ? "true" : undefined}
                    onMouseEnter={() => setActive(i)}
                    onFocus={() => setActive(i)}
                    onClick={() => setActive(i)}
                    className={`grid w-full grid-cols-[2rem_1fr] items-baseline gap-4 py-4 text-left transition-opacity focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-gray-5 sm:gap-6 ${
                      isActive ? "opacity-100" : "opacity-45 hover:opacity-70"
                    }`}
                  >
                    <span
                      className="font-mono text-sm tabular-nums text-gray-6"
                      aria-hidden="true"
                    >
                      {String(i + 1).padStart(2, "0")}
                    </span>
                    <span className="block text-base leading-relaxed">
                      <span className="font-semibold text-fg">{beat.lead}</span>{" "}
                      <span className="text-gray-7">{beat.body}</span>
                    </span>
                  </button>
                </li>
              )
            })}
          </ol>

          {/* Fixed-size illustrative panel; decorative, so hidden from AT. */}
          <div
            className="mt-8 hidden md:flex lg:mt-0 lg:items-center"
            aria-hidden="true"
          >
            <div className="relative mx-auto h-[22rem] w-full max-w-md">
              {MOCKUPS.map((Mockup, i) => (
                <div
                  key={i}
                  className={`absolute inset-0 transition-opacity duration-500 ${
                    i === active ? "opacity-100" : "opacity-0"
                  }`}
                >
                  <Mockup />
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </section>
  )
}
