import type { ReactNode } from "react"
import {
  ScopedTaskMockup,
  SpecializationMockup,
  WorkflowMockup,
} from "./KanbanMockup"

/**
 * The value-prop triad: tasks, agents, workflows. Each states the mechanism,
 * not the benefit, and pairs its heading + body with a focused Shelbi mockup
 * that shows the mechanism in action. The three render as separate full-width
 * sections stacked down the page, the mockup alternating left/right on desktop
 * and stacking under the copy on mobile. The headings stay H3 — they unpack the
 * solution intro's H2 ("Inbox Zero, for agent work"), so they sit a level below
 * it in the page outline.
 */
const triad: { title: string; body: ReactNode; mockup: ReactNode }[] = [
  {
    title: "Tasks keep work focused.",
    body: "Every item becomes a scoped task before an agent touches it. Agents do their best work on focused chunks, and big features and quick fixes flow through the same system.",
    mockup: <ScopedTaskMockup />,
  },
  {
    title: "Agents provide specialization.",
    body: "Workers execute. Add reviewers that scrutinize: adversarial review, QA, security. Each does one job well, on every task.",
    mockup: <SpecializationMockup />,
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
    mockup: <WorkflowMockup />,
  },
]

export function ValueProps() {
  return (
    <>
      {triad.map((item, i) => {
        // Alternate the mockup side down the page: even rows keep it on the
        // right (copy left), odd rows swap it to the left on desktop. The DOM
        // order is always copy-then-mockup, so on mobile (single column) the
        // copy reads first and the mockup stacks beneath it regardless of side.
        const mockupLeft = i % 2 === 1
        return (
          <section key={item.title} className="border-t border-gray-4">
            <div className="mx-auto grid w-full max-w-6xl grid-cols-1 items-center gap-8 px-4 py-10 lg:grid-cols-2 lg:gap-12 lg:px-6 lg:py-16">
              <div className="flex flex-col gap-3">
                <h3 className="font-sans text-2xl font-semibold tracking-tight text-fg sm:text-3xl">
                  {item.title}
                </h3>
                <p className="text-base leading-relaxed text-gray-7 sm:text-lg">
                  {item.body}
                </p>
              </div>
              <div
                className={`flex justify-center ${mockupLeft ? "lg:order-first" : ""}`}
              >
                {item.mockup}
              </div>
            </div>
          </section>
        )
      })}
    </>
  )
}
