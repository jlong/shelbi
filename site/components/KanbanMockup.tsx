/**
 * A styled reproduction of the Shelbi TUI's kanban view for the home
 * page. Renders inside a `.dark` scope so the terminal chrome keeps
 * its dark aesthetic even when the site is in light mode — while
 * still resolving through the same `bg`/`fg`/`gray-*` tokens the rest
 * of the site uses. Below `sm` the columns collapse to a horizontal
 * scroll so nothing gets cropped off-screen; the header stays pinned.
 */

type Card = {
  title: string
  workspace?: string
  glyph?: string
}

type ColumnSpec = {
  id: string
  label: string
  cards: Card[]
}

const COLUMNS: ColumnSpec[] = [
  {
    id: "backlog",
    label: "backlog",
    cards: [
      { title: "Rework onboarding copy", glyph: "○" },
      { title: "Audit third-party licenses", glyph: "○" },
      { title: "Draft Q3 roadmap", glyph: "○" },
    ],
  },
  {
    id: "todo",
    label: "todo",
    cards: [
      { title: "Add ratelimit to API", glyph: "◔" },
      { title: "Fix mobile nav overlap", glyph: "◔" },
    ],
  },
  {
    id: "in_progress",
    label: "in_progress",
    cards: [
      { title: "Deploy staging env", workspace: "alpha", glyph: "◐" },
      { title: "Wire up OAuth flow", workspace: "bravo", glyph: "◐" },
    ],
  },
  {
    id: "review",
    label: "review",
    cards: [
      { title: "Cache warm-up on cold start", workspace: "charlie", glyph: "◕" },
    ],
  },
  {
    id: "done",
    label: "done",
    cards: [
      { title: "Migrate to Postgres 16", glyph: "●" },
      { title: "Ship dark-mode toggle", glyph: "●" },
    ],
  },
]

function CardRow({ card }: { card: Card }) {
  return (
    <div className="flex items-start gap-1 border border-gray-4 bg-gray-1 px-1 py-1">
      <span aria-hidden="true" className="text-gray-6 leading-4">
        {card.glyph ?? "·"}
      </span>
      <div className="min-w-0 flex-1">
        <div className="truncate text-fg leading-4">{card.title}</div>
        {card.workspace ? (
          <div className="truncate text-gray-6 leading-4">
            <span className="text-gray-7">↳</span> {card.workspace}
          </div>
        ) : null}
      </div>
    </div>
  )
}

function Column({ column }: { column: ColumnSpec }) {
  return (
    <div className="flex min-w-[9.5rem] flex-1 flex-col gap-1 sm:min-w-0">
      <div className="flex items-baseline justify-between gap-1">
        <span className="uppercase tracking-wider text-fg">{column.label}</span>
        <span className="text-gray-6">{column.cards.length}</span>
      </div>
      <div className="h-px w-full bg-gray-4" />
      <div className="flex flex-col gap-1">
        {column.cards.map((card, i) => (
          <CardRow key={`${column.id}-${i}`} card={card} />
        ))}
      </div>
    </div>
  )
}

export function KanbanMockup() {
  return (
    <section className="border-b border-gray-4 px-3 py-6 sm:py-10">
      <div className="dark mx-auto w-full max-w-5xl overflow-hidden border border-gray-4 bg-bg text-fg shadow-lg">
        {/* Terminal chrome: three-dot faux buttons + title */}
        <div className="flex items-center gap-2 border-b border-gray-4 bg-gray-2 px-2 py-1">
          <div aria-hidden="true" className="flex items-center gap-1">
            <span className="inline-block h-1 w-1 rounded-full bg-gray-5" />
            <span className="inline-block h-1 w-1 rounded-full bg-gray-5" />
            <span className="inline-block h-1 w-1 rounded-full bg-gray-5" />
          </div>
          <span className="font-mono text-xs text-gray-6">
            shelbi — kanban
          </span>
        </div>

        {/* TUI header row */}
        <div className="flex items-center justify-between border-b border-gray-4 bg-bg px-2 py-1 font-mono text-xs">
          <div className="flex items-center gap-1">
            <span className="text-fg">▎</span>
            <span className="text-fg">shelbi</span>
            <span className="text-gray-6">/</span>
            <span className="text-gray-7">acme-web</span>
          </div>
          <div className="text-gray-7">
            zen: <span className="text-fg">on</span>
          </div>
        </div>

        {/* Board — horizontal scroll on small screens */}
        <div className="overflow-x-auto bg-bg font-mono text-[11px] leading-4">
          <div className="flex min-w-max gap-2 p-2 sm:min-w-0 sm:grid sm:grid-cols-5 sm:gap-3 sm:p-3">
            {COLUMNS.map((column) => (
              <Column key={column.id} column={column} />
            ))}
          </div>
        </div>

        {/* TUI footer / hints row */}
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-gray-4 bg-gray-2 px-2 py-1 font-mono text-[11px] text-gray-6">
          <span>
            <span className="text-fg">h/l</span> move
          </span>
          <span>
            <span className="text-fg">j/k</span> select
          </span>
          <span>
            <span className="text-fg">enter</span> open
          </span>
          <span>
            <span className="text-fg">n</span> new task
          </span>
          <span className="ml-auto hidden sm:inline">
            <span className="text-fg">?</span> help
          </span>
        </div>
      </div>
    </section>
  )
}
