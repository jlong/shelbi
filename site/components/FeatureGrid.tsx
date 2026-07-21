import type { ReactNode } from "react"

/**
 * The home page's closing features area: a Clerk-style masonry of bordered
 * cards, each pairing one of the six Shelbi features with a small, quiet UI
 * vignette built entirely from HTML/CSS primitives (no images). The masonry is
 * CSS-only — `columns-1 sm:columns-2 lg:columns-3` with `break-inside-avoid`
 * on each card — so the cards flow top-to-bottom in FEATURES order (the mobile
 * single-column reading order) and interlock at varying heights on wider
 * viewports. Every vignette is `aria-hidden` decoration; the `dl`/`dt`/`dd`
 * carries the accessible feature list. Cards alternate vignette-top vs
 * vignette-bottom so the columns stagger rather than align in rows.
 *
 * All vignette color comes from the site's theme tokens (fg, gray-1..7,
 * font-mono) so the whole section inverts correctly with the light/dark
 * toggle — nothing is hardcoded. The vignettes stay low-contrast (gray-5/6 on
 * a faint gray-2 panel) so they read as texture and the copy stays the focus.
 */

type Feature = {
  label: string
  body: string
  vignette: ReactNode
  /** Where the vignette sits relative to the copy — staggers the masonry. */
  place: "top" | "bottom"
}

// ── Vignettes ─────────────────────────────────────────────────────────
// Each is a self-contained, decorative CSS mockup. Shared primitives keep the
// contrast and spacing consistent so they read as one family of textures.

/** A faint panel the terminal-ish vignettes sit inside. */
function Panel({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={`rounded-lg border border-gray-4 bg-gray-2 p-3 ${className}`}
      aria-hidden="true"
    >
      {children}
    </div>
  )
}

/** Kanban TUI: a tiny three-column board with a few muted cards per column. */
function KanbanVignette() {
  const columns = [
    { dot: "bg-gray-5", cards: 2 },
    { dot: "bg-gray-6", cards: 3 },
    { dot: "bg-gray-5", cards: 1 },
  ]
  return (
    <Panel className="flex gap-2">
      {columns.map((col, i) => (
        <div key={i} className="flex-1">
          <div className="mb-2 flex items-center gap-1">
            <span className={`h-1.5 w-1.5 rounded-full ${col.dot}`} />
            <span className="h-1 flex-1 rounded-full bg-gray-4" />
          </div>
          <div className="flex flex-col gap-1.5">
            {Array.from({ length: col.cards }).map((_, c) => (
              <div key={c} className="rounded border border-gray-4 bg-gray-3 px-1.5 py-2">
                <span className="block h-1 w-4/5 rounded-full bg-gray-5" />
              </div>
            ))}
          </div>
        </div>
      ))}
    </Panel>
  )
}

/** Workers on any machine: a short list of machine rows, each with a status dot. */
function MachinesVignette() {
  const machines = [
    { name: "hub", dot: "bg-fg", state: "working" },
    { name: "devbox", dot: "bg-gray-5", state: "idle" },
    { name: "laptop", dot: "bg-gray-5", state: "idle" },
  ]
  return (
    <Panel className="font-mono text-[11px] text-gray-6">
      <div className="flex flex-col gap-2">
        {machines.map((m) => (
          <div key={m.name} className="flex items-center gap-2">
            <span className={`h-1.5 w-1.5 rounded-full ${m.dot}`} />
            <span className="text-gray-7">{m.name}</span>
            <span className="ml-auto text-gray-5">{m.state}</span>
          </div>
        ))}
      </div>
    </Panel>
  )
}

/** Made with tmux: a mini terminal panel with a prompt line and muted output. */
function TmuxVignette() {
  return (
    <Panel className="font-mono text-[11px] leading-relaxed">
      <div className="mb-2 flex gap-1">
        <span className="h-1.5 w-1.5 rounded-full bg-gray-5" />
        <span className="h-1.5 w-1.5 rounded-full bg-gray-5" />
        <span className="h-1.5 w-1.5 rounded-full bg-gray-5" />
      </div>
      <div className="text-gray-7">
        <span className="text-gray-5">❯</span> shelbi attach alpha
      </div>
      <div className="text-gray-5">worker ready · pane 1</div>
      <div className="text-gray-5">running task…</div>
    </Panel>
  )
}

/** Review flow: a checklist panel with the first row checked and the rest pending. */
function ReviewVignette() {
  const rows = [true, false, false]
  return (
    <Panel className="flex flex-col gap-2">
      {rows.map((done, i) => (
        <div key={i} className="flex items-center gap-2">
          <span
            className={`flex h-3 w-3 items-center justify-center rounded-[3px] border ${
              done ? "border-fg" : "border-gray-4"
            }`}
          >
            {done && (
              <svg viewBox="0 0 10 10" className="h-2 w-2 text-fg" fill="none">
                <path
                  d="M1.5 5.2 4 7.5 8.5 2.5"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            )}
          </span>
          <span
            className={`h-1.5 rounded-full ${done ? "bg-gray-6" : "bg-gray-4"}`}
            style={{ width: `${70 - i * 12}%` }}
          />
        </div>
      ))}
    </Panel>
  )
}

/** Plain-file state: a tiny file tree of tasks + workflows in font-mono. */
function FileTreeVignette() {
  const lines = [
    { text: "tasks/", muted: true, indent: 0 },
    { text: "ship-dark-mode.md", muted: false, indent: 1 },
    { text: "deploy-staging.md", muted: false, indent: 1 },
    { text: "workflows/", muted: true, indent: 0 },
    { text: "site.yaml", muted: false, indent: 1 },
  ]
  return (
    <Panel className="font-mono text-[11px] leading-relaxed">
      {lines.map((line, i) => (
        <div
          key={i}
          className={line.muted ? "text-gray-5" : "text-gray-7"}
          style={{ paddingLeft: `${line.indent * 0.9}rem` }}
        >
          {line.indent > 0 && <span className="text-gray-4">└ </span>}
          {line.text}
        </div>
      ))}
    </Panel>
  )
}

/** Open source: an MIT badge over a couple of muted commit-log lines. */
function OpenSourceVignette() {
  return (
    <Panel className="font-mono text-[11px]">
      <span className="inline-block rounded border border-gray-4 bg-gray-3 px-1.5 py-0.5 text-[10px] font-semibold tracking-wide text-gray-7">
        MIT
      </span>
      <div className="mt-2 flex flex-col gap-1.5 text-gray-5">
        <div className="flex items-center gap-2">
          <span className="text-gray-6">c41c3bf</span>
          <span className="h-1 flex-1 rounded-full bg-gray-4" />
        </div>
        <div className="flex items-center gap-2">
          <span className="text-gray-6">8f0e484</span>
          <span className="h-1 flex-1 rounded-full bg-gray-4" />
        </div>
      </div>
    </Panel>
  )
}

const FEATURES: Feature[] = [
  {
    label: "Kanban TUI",
    body: "Every task is a card on a board in your terminal, so status is on screen instead of in your head.",
    vignette: <KanbanVignette />,
    place: "top",
  },
  {
    label: "Workers on any machine",
    body: "Any box you can SSH into can take tasks. If it runs tmux and an agent CLI, it's a worker.",
    vignette: <MachinesVignette />,
    place: "bottom",
  },
  {
    label: "Made with tmux",
    body: "Every worker runs in a real tmux pane. Attach to a session to watch an agent work or type to it directly.",
    vignette: <TmuxVignette />,
    place: "top",
  },
  {
    label: "Review flow",
    body: "Finished tasks land in the review column and wait for you. Assign a review agent to any column to hold every task to the same bar.",
    vignette: <ReviewVignette />,
    place: "bottom",
  },
  {
    label: "Plain-file state",
    body: "Tasks are markdown and workflows are YAML, stored in your repo. No database, no cloud, just files you can read, grep, and commit.",
    vignette: <FileTreeVignette />,
    place: "top",
  },
  {
    label: "Open source",
    body: "The whole system is MIT licensed on GitHub. You can read every line that runs on your machines.",
    vignette: <OpenSourceVignette />,
    place: "bottom",
  },
]

export function FeatureGrid() {
  return (
    <section aria-labelledby="feature-grid-heading" className="border-t border-gray-4">
      <div className="mx-auto w-full max-w-6xl px-4 py-6 lg:px-6 lg:py-8">
        <h2
          id="feature-grid-heading"
          className="font-sans text-4xl font-semibold tracking-tight text-fg"
        >
          Features
        </h2>
        <dl className="mt-5 gap-5 columns-1 sm:columns-2 lg:columns-3">
          {FEATURES.map((feature) => {
            const copy = (
              <>
                <dt className="font-semibold text-fg">{feature.label}</dt>
                <dd className="mt-1 leading-relaxed text-gray-7">{feature.body}</dd>
              </>
            )
            return (
              <div
                key={feature.label}
                className="mb-5 break-inside-avoid rounded-xl border border-gray-4 bg-gray-1 p-5"
              >
                {feature.place === "top" ? (
                  <>
                    <div className="mb-4">{feature.vignette}</div>
                    {copy}
                  </>
                ) : (
                  <>
                    {copy}
                    <div className="mt-4">{feature.vignette}</div>
                  </>
                )}
              </div>
            )
          })}
        </dl>
      </div>
    </section>
  )
}
