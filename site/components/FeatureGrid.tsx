import type { CSSProperties, ReactNode } from "react"

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
 * Each vignette shares the site's mockup design language (see
 * `KanbanMockup.tsx`): a compact macOS-Terminal frame — traffic-light dots, a
 * `shelbi · <view>` / `jlong@hub — <cmd>` title bar, and a `--tui-bg` canvas —
 * wrapping a full-color slice of the real Shelbi TUI. Color comes from the same
 * theme-aware `--tui-*` ANSI palette the big mockups use, so the category
 * columns read blue / yellow / magenta / green (TO DO / IN PROGRESS / REVIEW /
 * DONE — `category_color()` in the crate), the working `⏵` badge is green, the
 * project/review accents are cyan, and everything inverts with the light/dark
 * toggle for free. Details stay grounded in `crates/shelbi-tui` and the CLI:
 * the real board columns, the sidebar's `— Workspaces —` section with `▾ hub` /
 * `▾ devbox` groups and `⏵`/`·` badges, a tmux pane with a green status bar and
 * `❯` prompt, the `⎇ shelbi/<id>` branch meta, real `shelbi` commands,
 * `tasks/*.md` + `workflows/*.yaml` paths, and 7-char commit hashes.
 *
 * Motion is entirely CSS: the card lifts on hover (`hover:` on the `group`
 * card) and each vignette does one tasteful, feature-appropriate thing via
 * `group-hover:` — a card slides one column right, a worker's task types in, a
 * new tmux line prints, review checks draw themselves, the file cursor steps
 * down, the newest commit slides into the log. Ambient life (the working dot,
 * the prompt cursor) uses `motion-safe:animate-pulse`. Every transform / loop
 * is gated behind `motion-safe:` (or reset with `motion-reduce:`) so
 * `prefers-reduced-motion: reduce` gets a still, legible section.
 */

// ── Palette ───────────────────────────────────────────────────────────
// The same theme-aware `--tui-*` variables the big mockups read (resolved in
// site/app/globals.css: `.dark` holds the terminal hex, `:root` a light-canvas
// variant). Fed to inline `color`/`background` styles, so the vignettes invert
// with the site's `.dark` class the same way `KanbanMockup` does.
const TUI_BG = "var(--tui-bg)"
const TUI_FG = "var(--tui-fg)"
const TUI_GRAY = "var(--tui-gray)" // column headers, nav labels
const TUI_DARK_GRAY = "var(--tui-dark-gray)" // dim chrome — ids, branches, footers
const TUI_DIVIDER = "var(--tui-divider)" // card / rule borders on the canvas
const TUI_BLUE = "var(--tui-blue)" // TO DO category
const TUI_YELLOW = "var(--tui-yellow)" // IN PROGRESS category + commit hashes
const TUI_MAGENTA = "var(--tui-magenta)" // REVIEW category
const TUI_GREEN = "var(--tui-green)" // DONE category + working badge
const TUI_CYAN = "var(--tui-cyan)" // project name + review branch accent
const TUI_SEL_BG = "var(--tui-sel-bg)" // selection / focus fill
const TUI_TASK_BOX_BG = "var(--tui-task-box-bg)" // quiet callout block (kanban cards)
const CHROME_BAR_BG = "var(--tui-chrome-bar-bg)"
const CHROME_BAR_BORDER = "var(--tui-chrome-bar-border)"
const CHROME_TITLE = "var(--tui-chrome-title)"
// Traffic lights read the same on a light or dark title bar, so they stay
// literal (not theme-driven) — matching `TrafficLight` in `KanbanMockup`.
const TRAFFIC = ["#ff5f57", "#febc2e", "#28c840"]

type Feature = {
  label: string
  body: string
  vignette: ReactNode
  /** Where the vignette sits relative to the copy — staggers the masonry. */
  place: "top" | "bottom"
}

// ── Shared frame ──────────────────────────────────────────────────────

/**
 * A compact macOS-Terminal frame — the same chrome the site's big mockups use
 * (`TerminalFrame` in `KanbanMockup.tsx`), scaled down to vignette size:
 * traffic-light dots tucked left, a centered title, a `--tui-bg` canvas. The
 * whole thing is decorative (`aria-hidden`); the border tint and shadow lift
 * a touch on card hover.
 */
function MiniTerminal({
  title,
  children,
  bodyClassName = "p-2.5",
  bodyStyle,
}: {
  title: string
  children: ReactNode
  bodyClassName?: string
  bodyStyle?: CSSProperties
}) {
  return (
    <div
      aria-hidden="true"
      className="overflow-hidden rounded-md border shadow-sm transition-shadow duration-200 group-hover:shadow-md"
      style={{ borderColor: CHROME_BAR_BORDER }}
    >
      {/* Title bar — traffic lights left, title centered. */}
      <div
        className="relative flex items-center justify-center px-2"
        style={{ background: CHROME_BAR_BG, borderBottom: `1px solid ${CHROME_BAR_BORDER}`, height: 18 }}
      >
        <div className="absolute flex items-center" style={{ left: 6, gap: 4 }}>
          {TRAFFIC.map((c) => (
            <span
              key={c}
              className="inline-block rounded-full"
              style={{ width: 7, height: 7, background: c, boxShadow: "inset 0 0 0 0.5px rgba(0,0,0,0.25)" }}
            />
          ))}
        </div>
        <span className="font-mono text-[9px] font-medium" style={{ color: CHROME_TITLE }}>
          {title}
        </span>
      </div>
      <div className={`font-mono ${bodyClassName}`} style={{ background: TUI_BG, color: TUI_FG, ...bodyStyle }}>
        {children}
      </div>
    </div>
  )
}

// ── Vignettes ─────────────────────────────────────────────────────────
// Each is a self-contained, decorative CSS mockup echoing a real Shelbi TUI
// surface, in the shared terminal frame and full `--tui-*` color.

/**
 * Kanban TUI: a four-column board (TO DO / IN PROGRESS / REVIEW / DONE — the
 * real category labels, each in its `category_color()` accent) with a few task
 * cards, TO DO visibly the fullest. On hover the top TO DO card slides one
 * column right, the way a task is pulled into IN PROGRESS.
 */
function KanbanVignette() {
  const columns = [
    { label: "TO DO", color: TUI_BLUE, cards: ["API ratelimit", "Mobile nav", "Search paging"] },
    { label: "IN PROGRESS", color: TUI_YELLOW, cards: ["Deploy staging"], branch: "shelbi/deploy-staging-env" },
    { label: "REVIEW", color: TUI_MAGENTA, cards: ["OAuth flow"], branch: "shelbi/wire-up-oauth" },
    { label: "DONE", color: TUI_GREEN, cards: ["Dark mode"] },
  ]
  return (
    <MiniTerminal title="shelbi · board">
      <div className="flex gap-1.5 text-[9px] leading-tight">
        {columns.map((col, ci) => (
          <div key={col.label} className="min-w-0 flex-1">
            <div
              className="mb-1 truncate font-semibold uppercase tracking-tight"
              style={{ color: col.color }}
            >
              {col.label} <span style={{ color: TUI_DARK_GRAY }}>{col.cards.length}</span>
            </div>
            <div className="flex flex-col gap-1">
              {col.cards.map((title, i) => {
                const sliding = ci === 0 && i === 0
                return (
                  <div
                    key={title}
                    className={`overflow-hidden rounded-sm border px-1 py-1 ${
                      sliding
                        ? "relative z-10 transition-transform duration-300 ease-out motion-safe:group-hover:translate-x-full"
                        : ""
                    }`}
                    style={{ borderColor: TUI_DIVIDER, background: TUI_TASK_BOX_BG }}
                  >
                    <div className="truncate" style={{ color: TUI_FG }}>
                      {title}
                    </div>
                    {col.branch && i === 0 && (
                      <div className="truncate" style={{ color: TUI_DARK_GRAY }}>
                        ⎇ {col.branch}
                      </div>
                    )}
                  </div>
                )
              })}
            </div>
          </div>
        ))}
      </div>
    </MiniTerminal>
  )
}

/**
 * Workers on any machine: the sidebar's `— Workspaces —` section with `▾ hub`
 * / `▾ devbox` groups, each holding workspace rows with the real `⏵` (working,
 * green) / `·` (idle) badge. The working worker's dot pulses; on hover its
 * current task types in beneath it.
 */
function MachinesVignette() {
  const machines = [
    {
      name: "hub",
      workspaces: [
        { name: "alpha", agent: "Developer", working: true, task: "Deploy staging env" },
        { name: "bravo", agent: "idle", working: false },
      ],
    },
    {
      name: "devbox",
      workspaces: [{ name: "charlie", agent: "Reviewer", working: true }],
    },
  ]
  return (
    <MiniTerminal title="shelbi · workspaces">
      <div className="text-[11px] leading-tight">
        <div className="mb-1.5" style={{ color: TUI_DARK_GRAY }}>
          — Workspaces —
        </div>
        <div className="flex flex-col gap-1.5">
          {machines.map((m) => (
            <div key={m.name}>
              <div style={{ color: TUI_GRAY }}>▾ {m.name}</div>
              {m.workspaces.map((w) => (
                <div key={w.name}>
                  <div className="flex items-center gap-1.5 pl-2">
                    {w.working ? (
                      <span className="motion-safe:animate-pulse" style={{ color: TUI_GREEN }}>
                        ⏵
                      </span>
                    ) : (
                      <span style={{ color: TUI_DARK_GRAY }}>·</span>
                    )}
                    <span style={{ color: w.working ? TUI_FG : TUI_GRAY }}>{w.name}</span>
                    <span className="ml-auto" style={{ color: TUI_DARK_GRAY }}>
                      {w.agent}
                    </span>
                  </div>
                  {w.task && (
                    <div
                      className="max-h-0 overflow-hidden whitespace-nowrap pl-4 opacity-0 transition-all duration-300 ease-out motion-safe:group-hover:max-h-4 motion-safe:group-hover:opacity-100"
                      style={{ color: TUI_DARK_GRAY }}
                    >
                      ⎿ {w.task}
                    </div>
                  )}
                </div>
              ))}
            </div>
          ))}
        </div>
      </div>
    </MiniTerminal>
  )
}

/**
 * Made with tmux: a real tmux pane — a couple of muted output lines, a `❯`
 * prompt with a blinking block cursor, and the iconic green tmux status bar
 * along the bottom (session name, window list, host/clock). On hover a fresh
 * command's output line prints above the prompt.
 */
function TmuxVignette() {
  return (
    <MiniTerminal title="jlong@hub — tmux" bodyClassName="text-[10px] leading-relaxed">
      <div className="p-2.5 pb-2">
        <div className="overflow-hidden whitespace-nowrap" style={{ color: TUI_FG }}>
          <span style={{ color: TUI_GREEN }}>❯ </span>shelbi attach alpha
        </div>
        <div className="overflow-hidden whitespace-nowrap" style={{ color: TUI_DARK_GRAY }}>
          attached to alpha · pane 1
        </div>
        <div className="overflow-hidden whitespace-nowrap" style={{ color: TUI_FG }}>
          <span style={{ color: TUI_GREEN }}>❯ </span>shelbi task start deploy-staging-env
        </div>
        <div
          className="max-h-0 overflow-hidden whitespace-nowrap opacity-0 transition-all duration-300 ease-out motion-safe:group-hover:max-h-4 motion-safe:group-hover:opacity-100"
          style={{ color: TUI_GREEN }}
        >
          ✓ deploy-staging-env → in_progress on alpha
        </div>
        <div className="overflow-hidden whitespace-nowrap" style={{ color: TUI_FG }}>
          <span style={{ color: TUI_GREEN }}>❯ </span>
          <span
            className="ml-px inline-block h-[1.1em] w-[0.55em] translate-y-[0.15em] align-middle motion-safe:animate-pulse"
            style={{ background: TUI_FG }}
          />
        </div>
      </div>
      {/* The classic green tmux status bar: session name, window list, clock. */}
      <div
        className="flex items-center gap-2 px-2 py-1 text-[9px]"
        style={{ background: TUI_GREEN, color: TUI_BG }}
      >
        <span className="font-semibold">[shelbi]</span>
        <span style={{ opacity: 0.75 }}>0:orch</span>
        <span className="font-semibold">1:agent*</span>
        <span className="ml-auto" style={{ opacity: 0.75 }}>
          &quot;hub&quot; 14:32
        </span>
      </div>
    </MiniTerminal>
  )
}

/**
 * Review flow: the REVIEW column (magenta) with the branch that landed there,
 * over the checklist a review agent holds every task to. On hover each check
 * draws itself in green, staggered top to bottom.
 */
function ReviewVignette() {
  const rows = ["build passes", "lint clean", "tests green", "diff reviewed"]
  return (
    <MiniTerminal title="shelbi · review">
      <div className="text-[11px]">
        <div className="mb-2 flex items-center gap-1.5" style={{ color: TUI_MAGENTA }}>
          <span>▾</span> REVIEW
          <span className="ml-auto truncate" style={{ color: TUI_CYAN }}>
            ⎇ shelbi/deploy-staging-env
          </span>
        </div>
        <div className="flex flex-col gap-1.5">
          {rows.map((label, i) => (
            <div key={label} className="flex items-center gap-2">
              <span
                className="flex h-3 w-3 items-center justify-center rounded-[3px] border"
                style={{ borderColor: TUI_DIVIDER }}
              >
                <svg viewBox="0 0 10 10" className="h-2 w-2" fill="none" style={{ color: TUI_GREEN }}>
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
              <span style={{ color: TUI_GRAY }}>{label}</span>
            </div>
          ))}
        </div>
      </div>
    </MiniTerminal>
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
    { text: ".shelbi/", dir: true },
    { text: "├ tasks/", dir: true },
    { text: "│ ├ deploy-staging-env.md", dir: false },
    { text: "│ └ wire-up-oauth-flow.md", dir: false },
    { text: "└ workflows/", dir: true },
    { text: "  ├ site.yaml", dir: false },
    { text: "  └ app.yaml", dir: false },
  ]
  return (
    <MiniTerminal title="jlong@hub — .shelbi">
      <div className="relative text-[11px]">
        <div
          className="absolute inset-x-0 rounded-[3px] transition-transform duration-300 ease-out motion-safe:group-hover:translate-y-[16px]"
          style={{ height: ROW, top: selIndex * ROW, background: TUI_SEL_BG }}
        />
        <div className="relative">
          {lines.map((l) => (
            <div
              key={l.text}
              className="overflow-hidden whitespace-nowrap"
              style={{ height: ROW, lineHeight: `${ROW}px`, color: l.dir ? TUI_DARK_GRAY : TUI_FG }}
            >
              {l.text}
            </div>
          ))}
        </div>
      </div>
    </MiniTerminal>
  )
}

/**
 * Open source: an MIT badge over a couple of commit-log lines with real 7-char
 * hashes (git's yellow). On hover the newest commit slides into the top of the
 * log and the star tick fills in.
 */
function OpenSourceVignette() {
  const commits = [
    { hash: "84d863e", msg: "feat(review): serve recipe" },
    { hash: "ae38b70", msg: "fix(tui): clamp sidebar" },
  ]
  return (
    <MiniTerminal title="jlong@hub — git log">
      <div className="text-[11px]">
        <div className="mb-2 flex items-center gap-2">
          <span
            className="inline-block rounded border px-1.5 py-0.5 text-[10px] font-semibold tracking-wide"
            style={{ borderColor: TUI_DIVIDER, color: TUI_FG }}
          >
            MIT
          </span>
          <span style={{ color: TUI_CYAN }}>main</span>
          <span className="ml-auto text-[color:var(--tui-divider)] transition-colors duration-200 group-hover:text-[color:var(--tui-yellow)]">
            ★
          </span>
        </div>
        <div className="max-h-0 overflow-hidden opacity-0 transition-all duration-300 ease-out motion-safe:group-hover:max-h-5 motion-safe:group-hover:opacity-100">
          <div className="flex items-center gap-2 pb-1.5">
            <span style={{ color: TUI_YELLOW }}>dea6e14</span>
            <span className="truncate" style={{ color: TUI_GRAY }}>
              site: rework feature grid
            </span>
          </div>
        </div>
        <div className="flex flex-col gap-1.5">
          {commits.map((c) => (
            <div key={c.hash} className="flex items-center gap-2">
              <span style={{ color: TUI_YELLOW }}>{c.hash}</span>
              <span className="truncate" style={{ color: TUI_GRAY }}>
                {c.msg}
              </span>
            </div>
          ))}
        </div>
      </div>
    </MiniTerminal>
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
