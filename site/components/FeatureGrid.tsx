import type { ReactNode } from "react"

/**
 * The home page's closing features area: a Clerk-style masonry of bordered
 * cards, each pairing one of the six Shelbi features with a small UI vignette
 * built entirely from HTML/CSS primitives (no images). The masonry is CSS-only
 * — `columns-1 sm:columns-2 lg:columns-3` with `break-inside-avoid` on each
 * card — so the cards flow top-to-bottom in FEATURES order (the mobile
 * single-column reading order) and interlock at varying heights on wider
 * viewports. Every vignette is `aria-hidden` decoration; the `dl`/`dt`/`dd`
 * carries the accessible feature list. Cards alternate vignette-top vs
 * vignette-bottom so the columns stagger rather than align in rows.
 *
 * Each vignette is a faithful, low-contrast slice of the actual Shelbi TUI
 * rather than an abstract texture: the real board columns (TO DO / IN PROGRESS
 * / REVIEW / DONE), the sidebar's `▾ machine` groups with `⏵`/`·` workspace
 * badges, a tmux pane with a status bar and `❯` prompt, the `⎇ shelbi/<id>`
 * branch meta line, real `shelbi` commands, `tasks/*.md` + `workflows/*.yaml`
 * paths, and 7-char commit hashes — all grounded in `crates/shelbi-tui` and
 * the CLI so the mockups can't drift from the running product.
 *
 * Motion is entirely CSS: the card lifts on hover (`hover:` on the `group`
 * card) and each vignette does one tasteful, feature-appropriate thing via
 * `group-hover:` — a card slides one column right, a worker's task types in, a
 * new tmux line prints, review checks draw themselves, the file cursor steps
 * down, the newest commit slides into the log. Ambient life (the working dot,
 * the prompt cursor) uses `motion-safe:animate-pulse`. Every transform / loop
 * is gated behind `motion-safe:` (or reset with `motion-reduce:`) so
 * `prefers-reduced-motion: reduce` gets a still, legible section.
 *
 * All vignette color comes from the site's theme tokens (fg, bg, gray-1..7,
 * font-mono) so the whole section inverts correctly with the light/dark toggle
 * — nothing is hardcoded. The vignettes stay low-contrast so they read as the
 * real UI without pulling focus from the copy.
 */

type Feature = {
  label: string
  body: string
  vignette: ReactNode
  /** Where the vignette sits relative to the copy — staggers the masonry. */
  place: "top" | "bottom"
}

// ── Vignettes ─────────────────────────────────────────────────────────
// Each is a self-contained, decorative CSS mockup echoing a real Shelbi TUI
// surface. Shared primitives keep the contrast and spacing consistent so they
// read as one family of terminal captures.

/** A faint panel the terminal-ish vignettes sit inside. */
function Panel({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={`rounded-lg border border-gray-4 bg-gray-2 p-3 transition-colors duration-200 group-hover:border-gray-5 ${className}`}
      aria-hidden="true"
    >
      {children}
    </div>
  )
}

/**
 * Kanban TUI: a four-column board (TO DO / IN PROGRESS / REVIEW / DONE — the
 * real category labels) with a few task cards, TO DO visibly the fullest. On
 * hover the top TO DO card slides one column right, the way a task is pulled
 * into IN PROGRESS.
 */
function KanbanVignette() {
  const columns = [
    { label: "TO DO", cards: ["API ratelimit", "Mobile nav", "Search paging"] },
    { label: "IN PROGRESS", cards: ["Deploy staging"], branch: "shelbi/deploy-staging-env" },
    { label: "REVIEW", cards: ["OAuth flow"], branch: "shelbi/wire-up-oauth" },
    { label: "DONE", cards: ["Dark mode"] },
  ]
  return (
    <Panel className="font-mono">
      <div className="flex gap-1.5 text-[9px] leading-tight">
        {columns.map((col, ci) => (
          <div key={col.label} className="min-w-0 flex-1">
            <div className="mb-1 truncate font-semibold uppercase tracking-tight text-gray-6">
              {col.label} <span className="text-gray-5">{col.cards.length}</span>
            </div>
            <div className="flex flex-col gap-1">
              {col.cards.map((title, i) => {
                const sliding = ci === 0 && i === 0
                return (
                  <div
                    key={title}
                    className={`overflow-hidden rounded-sm border border-gray-4 bg-gray-3 px-1 py-1 ${
                      sliding
                        ? "relative z-10 transition-transform duration-300 ease-out motion-safe:group-hover:translate-x-full"
                        : ""
                    }`}
                  >
                    <div className="truncate text-gray-7">{title}</div>
                    {col.branch && i === 0 && (
                      <div className="truncate text-gray-5">⎇ {col.branch}</div>
                    )}
                  </div>
                )
              })}
            </div>
          </div>
        ))}
      </div>
    </Panel>
  )
}

/**
 * Workers on any machine: the sidebar's `— Workspaces —` section with `▾ hub`
 * / `▾ devbox` / `▾ laptop` groups, each holding one workspace row with the
 * real `⏵` (working) / `·` (idle) badge. The working worker's dot pulses; on
 * hover its current task types in beneath it.
 */
function MachinesVignette() {
  const machines = [
    { name: "hub", worker: "alpha", working: true, agent: "Developer", task: "Deploy staging env" },
    { name: "devbox", worker: "bravo", working: false },
    { name: "laptop", worker: "charlie", working: false },
  ]
  return (
    <Panel className="font-mono text-[11px]">
      <div className="mb-1.5 text-gray-5">— Workspaces —</div>
      <div className="flex flex-col gap-1">
        {machines.map((m) => (
          <div key={m.name}>
            <div className="text-gray-5">▾ {m.name}</div>
            <div className="flex items-center gap-1.5 pl-2">
              {m.working ? (
                <span className="text-fg motion-safe:animate-pulse">⏵</span>
              ) : (
                <span className="text-gray-5">·</span>
              )}
              <span className={m.working ? "text-gray-7" : "text-gray-6"}>{m.worker}</span>
              <span className="ml-auto text-gray-5">{m.working ? m.agent : "idle"}</span>
            </div>
            {m.task && (
              <div className="max-h-0 overflow-hidden whitespace-nowrap pl-4 text-gray-5 opacity-0 transition-all duration-300 ease-out motion-safe:group-hover:max-h-4 motion-safe:group-hover:opacity-100">
                ⎿ {m.task}
              </div>
            )}
          </div>
        ))}
      </div>
    </Panel>
  )
}

/**
 * Made with tmux: a real tmux pane — a couple of muted output lines, a `❯`
 * prompt with a blinking block cursor, and a status bar along the bottom
 * (session chip + window list + host/clock). On hover a fresh command's output
 * line prints above the prompt.
 */
function TmuxVignette() {
  return (
    <div
      className="overflow-hidden rounded-lg border border-gray-4 bg-gray-2 font-mono text-[10px] leading-relaxed transition-colors duration-200 group-hover:border-gray-5"
      aria-hidden="true"
    >
      <div className="p-3 pb-2">
        <div className="overflow-hidden whitespace-nowrap text-gray-7">
          <span className="text-gray-5">❯ </span>shelbi attach alpha
        </div>
        <div className="overflow-hidden whitespace-nowrap text-gray-5">attached to alpha · pane 1</div>
        <div className="overflow-hidden whitespace-nowrap text-gray-7">
          <span className="text-gray-5">❯ </span>shelbi task start deploy-staging-env
        </div>
        <div className="max-h-0 overflow-hidden whitespace-nowrap text-gray-5 opacity-0 transition-all duration-300 ease-out motion-safe:group-hover:max-h-4 motion-safe:group-hover:opacity-100">
          ✓ deploy-staging-env → in_progress on alpha
        </div>
        <div className="overflow-hidden whitespace-nowrap text-gray-7">
          <span className="text-gray-5">❯ </span>
          <span className="ml-px inline-block h-[1.1em] w-[0.55em] translate-y-[0.15em] bg-gray-6 align-middle motion-safe:animate-pulse" />
        </div>
      </div>
      <div className="flex items-center gap-2 bg-gray-3 px-2 py-1 text-[9px] text-gray-6">
        <span className="rounded-sm bg-fg px-1 text-bg">shelbi-my-project</span>
        <span className="text-gray-5">0:orch</span>
        <span className="text-gray-7">1:agent*</span>
        <span className="ml-auto text-gray-5">&quot;hub&quot; 14:32</span>
      </div>
    </div>
  )
}

/**
 * Review flow: the REVIEW column with the branch that landed there, over the
 * checklist a review agent holds every task to. On hover each check draws
 * itself in, staggered top to bottom.
 */
function ReviewVignette() {
  const rows = ["build passes", "lint clean", "tests green", "diff reviewed"]
  return (
    <Panel className="font-mono text-[11px]">
      <div className="mb-2 flex items-center gap-1.5 text-gray-6">
        <span className="text-gray-5">▾</span> REVIEW
        <span className="ml-auto truncate text-gray-5">⎇ shelbi/deploy-staging-env</span>
      </div>
      <div className="flex flex-col gap-1.5">
        {rows.map((label, i) => (
          <div key={label} className="flex items-center gap-2">
            <span className="flex h-3 w-3 items-center justify-center rounded-[3px] border border-gray-4">
              <svg viewBox="0 0 10 10" className="h-2 w-2 text-gray-7" fill="none">
                <path
                  d="M1.5 5.2 4 7.5 8.5 2.5"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  pathLength={1}
                  style={{ transitionDelay: `${i * 110}ms` }}
                  className="[stroke-dasharray:1] [stroke-dashoffset:1] transition-[stroke-dashoffset] duration-300 ease-out motion-safe:group-hover:[stroke-dashoffset:0] motion-reduce:[stroke-dashoffset:0]"
                />
              </svg>
            </span>
            <span className="text-gray-6">{label}</span>
          </div>
        ))}
      </div>
    </Panel>
  )
}

/**
 * Plain-file state: the on-disk tree — `tasks/*.md` and `workflows/*.yaml`
 * under `.shelbi/`, drawn with box-drawing connectors. A faint selection bar
 * sits on the first task and steps down a row on hover.
 */
function FileTreeVignette() {
  const ROW = 16 // px per row; the selection bar and translate step use it
  const selIndex = 2 // the first task file
  const lines = [
    { text: ".shelbi/", muted: true },
    { text: "├ tasks/", muted: true },
    { text: "│ ├ deploy-staging-env.md", muted: false },
    { text: "│ └ wire-up-oauth-flow.md", muted: false },
    { text: "└ workflows/", muted: true },
    { text: "  ├ site.yaml", muted: false },
    { text: "  └ app.yaml", muted: false },
  ]
  return (
    <Panel className="font-mono text-[11px]">
      <div className="relative">
        <div
          className="absolute inset-x-0 rounded-[3px] bg-gray-3 transition-transform duration-300 ease-out motion-safe:group-hover:translate-y-[16px]"
          style={{ height: ROW, top: selIndex * ROW }}
        />
        <div className="relative">
          {lines.map((l) => (
            <div
              key={l.text}
              className={`overflow-hidden whitespace-nowrap ${l.muted ? "text-gray-5" : "text-gray-7"}`}
              style={{ height: ROW, lineHeight: `${ROW}px` }}
            >
              {l.text}
            </div>
          ))}
        </div>
      </div>
    </Panel>
  )
}

/**
 * Open source: an MIT badge over a couple of commit-log lines with real 7-char
 * hashes. On hover the newest commit slides into the top of the log and the
 * star tick fills in.
 */
function OpenSourceVignette() {
  const commits = [
    { hash: "84d863e", msg: "feat(review): serve recipe" },
    { hash: "ae38b70", msg: "fix(tui): clamp sidebar" },
  ]
  return (
    <Panel className="font-mono text-[11px]">
      <div className="mb-2 flex items-center gap-2">
        <span className="inline-block rounded border border-gray-4 bg-gray-3 px-1.5 py-0.5 text-[10px] font-semibold tracking-wide text-gray-7">
          MIT
        </span>
        <span className="text-gray-5">main</span>
        <span className="ml-auto text-gray-4 transition-colors duration-200 group-hover:text-fg">★</span>
      </div>
      <div className="max-h-0 overflow-hidden opacity-0 transition-all duration-300 ease-out motion-safe:group-hover:max-h-5 motion-safe:group-hover:opacity-100">
        <div className="flex items-center gap-2 pb-1.5">
          <span className="text-gray-6">dea6e14</span>
          <span className="truncate text-gray-5">site: rework feature grid</span>
        </div>
      </div>
      <div className="flex flex-col gap-1.5">
        {commits.map((c) => (
          <div key={c.hash} className="flex items-center gap-2">
            <span className="text-gray-6">{c.hash}</span>
            <span className="truncate text-gray-5">{c.msg}</span>
          </div>
        ))}
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
                className="group mb-5 break-inside-avoid rounded-xl border border-gray-4 bg-gray-1 p-5 transition duration-200 ease-out hover:border-gray-5 hover:shadow-md motion-safe:hover:-translate-y-1"
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
