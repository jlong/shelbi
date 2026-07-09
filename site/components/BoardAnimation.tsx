"use client"

import { useEffect, useRef, useState } from "react"
import { BoardMockup, type AppState, type Card, type Column, type Machine } from "./KanbanMockup"

/**
 * A looping, autoplaying board that shows work flowing through the pipeline —
 * TO DO → IN PROGRESS → REVIEW → DONE — one card at a time. It drives the
 * bleeding `BoardMockup` (the same dashboard the hero renders, cropped by the
 * page edge) through a sequence of board states rather than adding any terminal
 * rendering of its own.
 *
 * Unlike a whole-board shuffle, each beat moves exactly ONE card one column: a
 * four-beat cycle walks a single unit of work down the pipeline — the review
 * card lands in DONE, the in-progress card hands off to REVIEW, the top TO DO
 * card is pulled into IN PROGRESS on a worker, then a fresh task drops into TO DO
 * to keep the column stocked. Everything else holds still between beats, so the
 * eye follows one task at a time.
 *
 * The tasks are a fixed ring (`POOL`) indexed modulo its length, so after one
 * full lap the board returns to its starting arrangement and the loop is
 * seamless — no fade or reset seam. The BACKLOG column is collapsed to a narrow
 * strip (the deep queue feeding TO DO, not part of the movement to follow) and
 * the sidebar carries no review sections, so nothing competes with the single
 * card in motion.
 *
 * Accessibility + cost mirror `HeroAnimation`: `prefers-reduced-motion` renders a
 * single static beat with no timers, and an IntersectionObserver pauses the loop
 * while the board is scrolled off-screen.
 */

// The ring of tasks that flow through the board, each with the worker that picks
// it up in IN PROGRESS. Long enough that a full lap never shows the same title
// twice on the board at once (the board holds up to 11 distinct tasks: 3 done +
// 1 review + 1 in-progress + 6 to-do).
type Flow = { title: string; id: string; worker: string }
const POOL: Flow[] = [
  { title: "Add API ratelimit", id: "t-007", worker: "alpha" },
  { title: "Fix mobile nav", id: "t-008", worker: "bravo" },
  { title: "Paginate search API", id: "t-038", worker: "charlie" },
  { title: "Cache user sessions", id: "t-040", worker: "alpha" },
  { title: "Wire webhook retries", id: "t-016", worker: "bravo" },
  { title: "Split OTel spans", id: "t-021", worker: "charlie" },
  { title: "Sync i18n strings", id: "t-026", worker: "alpha" },
  { title: "Add health probes", id: "t-042", worker: "bravo" },
  { title: "Debounce autosave", id: "t-045", worker: "charlie" },
  { title: "Harden token refresh", id: "t-025", worker: "alpha" },
  { title: "Prune stale flags", id: "t-030", worker: "bravo" },
  { title: "Dedupe error reports", id: "t-031", worker: "charlie" },
]
const M = POOL.length
// Four single-card moves per unit of work; one full lap is M of them.
const BEATS = M * 4

// How full TO DO stays: six cards waiting (five in the beat where the top one has
// just been pulled and the refill hasn't landed yet).
const TODO_DEPTH = 6

// The deep queue behind the collapsed BACKLOG strip: never drawn as cards, only
// counted, so the fold shows a believable "(12)" of work waiting to be pulled.
const BACKLOG: Card[] = [
  "Rework onboarding UX",
  "Audit OSS licenses",
  "Draft Q3 roadmap",
  "Migrate CI to arm64",
  "Sunset legacy v1 API",
  "Add SSO for admins",
  "Archive S3 buckets",
  "Refresh brand assets",
  "Migrate to PG 16",
  "Ship dark mode",
  "Retry dead-letters",
  "Add audit logging",
].map((title, i) => ({ title, id: `b-${i}` }))

// Resident IN PROGRESS work that's always underway — long-running tasks on their
// own workers that never move, so IN PROGRESS is never down to a single card and
// several workers stay lit in the sidebar while the one traveling task hops
// through the pipeline. Their workers are distinct from the pool's
// alpha/bravo/charlie so the traveler never collides with a resident.
const RESIDENT_PROGRESS: Card[] = [
  { title: "Trim vendor bundle", id: "t-022", workspace: "delta" },
  { title: "Backfill order index", id: "t-017", workspace: "echo" },
  { title: "Migrate CI to arm64", id: "t-014", workspace: "foxtrot" },
]
const RESIDENT_WORKERS = RESIDENT_PROGRESS.map((c) => c.workspace as string)

// Two machines, matching the mockup convention. The resident workers above are
// always working; the pool worker on the current traveling card lights up on top
// of them, and the rest stay idle.
const MACHINES: { name: string; ws: string[] }[] = [
  { name: "hub", ws: ["alpha", "bravo", "charlie", "delta"] },
  { name: "devbox", ws: ["echo", "foxtrot", "golf"] },
]
function machinesFor(traveler: string | null): Machine[] {
  const working = new Set<string>(RESIDENT_WORKERS)
  if (traveler) working.add(traveler)
  return MACHINES.map((m) => ({
    name: m.name,
    workspaces: m.ws.map((name) =>
      working.has(name)
        ? { name, state: "working" as const, agent: "Developer" }
        : { name, state: "idle" as const },
    ),
  }))
}

/** Pool task at ring index `i` (mod M), so beats can index past the ends. */
function at(i: number): Flow {
  return POOL[((i % M) + M) % M]
}

function toCard(i: number, withWorker = false): Card {
  const f = at(i)
  return withWorker ? { title: f.title, id: f.id, workspace: f.worker } : { title: f.title, id: f.id }
}

/**
 * The pipeline contents at global beat `b`, as ring indices per column. `a` is
 * the unit of work at the front of the line (advances one per four-beat cycle);
 * `phase` is which of the cycle's four single-card moves has been applied. Only
 * one column's membership changes between consecutive phases, so exactly one card
 * moves per beat.
 */
function pipelineAt(b: number): {
  done: number[]
  review: number[]
  inprog: number[]
  todo: number[]
} {
  const a = Math.floor(b / 4)
  const phase = b % 4
  const todoFull = Array.from({ length: TODO_DEPTH }, (_, j) => a + 3 + j) // a+3 … a+8
  switch (phase) {
    case 0: // steady: review/in-progress each hold one, to-do full
      return { done: [a, a - 1, a - 2], review: [a + 1], inprog: [a + 2], todo: todoFull }
    case 1: // the review card landed in DONE
      return { done: [a + 1, a, a - 1], review: [], inprog: [a + 2], todo: todoFull }
    case 2: // the in-progress card handed off to REVIEW
      return { done: [a + 1, a, a - 1], review: [a + 2], inprog: [], todo: todoFull }
    default: // phase 3: the top to-do card was pulled into IN PROGRESS
      return {
        done: [a + 1, a, a - 1],
        review: [a + 2],
        inprog: [a + 3],
        todo: todoFull.slice(1), // a+4 … a+8; the refill lands on the next beat
      }
  }
}

/** The five columns at beat `b` — the collapsed backlog, then the flowing stages. */
function columnsAt(b: number): Column[] {
  const p = pipelineAt(b)
  return [
    { label: "BACKLOG", category: "gray", collapsed: true, cards: BACKLOG },
    { label: "TO DO", category: "blue", cards: p.todo.map((i) => toCard(i)) },
    {
      label: "IN PROGRESS",
      category: "yellow",
      // The one traveling card (when present) sits on top of the always-running
      // resident work, so the column stays busy and several workers stay lit.
      cards: [...p.inprog.map((i) => toCard(i, true)), ...RESIDENT_PROGRESS],
    },
    { label: "REVIEW", category: "magenta", cards: p.review.map((i) => toCard(i)) },
    { label: "DONE", category: "green", cards: p.done.map((i) => toCard(i)) },
  ]
}

/** The full board state at beat `b`. */
function stateAt(b: number): AppState {
  const p = pipelineAt(b)
  const traveler = p.inprog.length ? at(p.inprog[0]).worker : null
  return {
    terminalTitle: "jlong@hub — my-project",
    project: "my-project",
    activeView: "tasks",
    // Pin the body height to the same row count the hero uses so this board and
    // the hero dashboard frame stand the same height; shorter beats pad with
    // blank rows rather than resizing the frame.
    minBodyRows: 32,
    workflow: "app",
    columns: columnsAt(b),
    machines: machinesFor(traveler),
    readyReview: [],
    queuedReview: [],
  }
}

// Dwell per beat: long enough to track a single card jumping one column before
// the next move.
const HOLD_MS = 1600

export function BoardAnimation() {
  const [beat, setBeat] = useState(0)
  const [visible, setVisible] = useState(true)
  const [reduced, setReduced] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)

  // Respect prefers-reduced-motion: no autoplay, just a single static beat.
  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)")
    const update = () => setReduced(mq.matches)
    update()
    mq.addEventListener("change", update)
    return () => mq.removeEventListener("change", update)
  }, [])

  // Pause the loop while the board is scrolled off-screen to save CPU.
  useEffect(() => {
    const el = containerRef.current
    if (!el) return
    const obs = new IntersectionObserver(([entry]) => setVisible(entry.isIntersecting), {
      threshold: 0.15,
    })
    obs.observe(el)
    return () => obs.disconnect()
  }, [])

  // Advance one beat after the hold and wrap forever; the modulo ring makes the
  // wrap a normal one-card move, so there's no seam to hide.
  useEffect(() => {
    if (reduced || !visible) return
    const t = window.setTimeout(() => setBeat((k) => (k + 1) % BEATS), HOLD_MS)
    return () => window.clearTimeout(t)
  }, [beat, visible, reduced])

  return (
    <div ref={containerRef}>
      <BoardMockup state={stateAt(reduced ? 0 : beat)} />
    </div>
  )
}
