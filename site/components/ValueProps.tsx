import type { ReactNode } from "react"

/**
 * The value-prop triad: tasks, agents, workflows. Each states the
 * mechanism, not the benefit. Rendered as H3 cards below the solution
 * intro's H2.
 */
const triad: { title: string; body: ReactNode }[] = [
  {
    title: "Tasks keep work focused.",
    body: "Every item becomes a scoped task before an agent touches it. Agents do their best work on focused chunks, and big features and quick fixes flow through the same system.",
  },
  {
    title: "Agents provide specialization.",
    body: "Workers execute. Add reviewers that scrutinize: adversarial review, QA, security. Each does one job well, on every task.",
  },
  {
    title: "Workflows provide boundaries.",
    body: (
      <>
        Every task moves through stages you define before it reaches you.
        Boundaries are what make autonomy safe. When your gates catch what
        you would, flip{" "}
        <code className="font-mono text-[0.9em] text-fg">shelbi zen on</code>{" "}
        and the orchestrator merges green work itself, then reports what
        landed and what needs you.
      </>
    ),
  },
]

export function ValueProps() {
  return (
    <section className="border-t border-gray-4">
      <div className="grid grid-cols-1 gap-px bg-gray-4 md:grid-cols-3">
        {triad.map((item) => (
          <div key={item.title} className="flex flex-col gap-2 bg-bg p-4">
            <h3 className="text-xl font-semibold text-fg">{item.title}</h3>
            <p className="leading-relaxed text-gray-7">{item.body}</p>
          </div>
        ))}
      </div>
    </section>
  )
}
