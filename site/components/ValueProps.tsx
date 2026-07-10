import type { ReactNode } from "react"
import { BoardAnimation } from "./BoardAnimation"
import { AgentInstructionsMockup, WorkflowConfigMockup } from "./KanbanMockup"

/**
 * The value-prop triad: tasks, agents, workflows. Each states the mechanism,
 * not the benefit, and pairs its heading + body with a focused Shelbi mockup
 * that shows the mechanism in action. The three render as separate full-width
 * sections stacked down the page, the mockup alternating left/right on desktop
 * and stacking under the copy on mobile. The headings stay H3 — they unpack the
 * solution intro's H2 ("Inbox Zero, for agent work"), so they sit a level below
 * it in the page outline.
 */
// `bleed` (the board) sizes to its own natural width and overflows the column.
// `edge` (the vim editors) is a fluid full-bleed: the panel fills from the
// viewport edge to the far side of its column, cropped at the page edge.
const triad: {
  title: string
  body: ReactNode
  mockup: ReactNode
  bleed?: boolean
  edge?: "left" | "right"
}[] = [
  {
    title: "Tasks keep work focused.",
    body: "Every item becomes a scoped task before a worker agent touches it. Agents do their best work in focused chunks. The orchestrator breaks big features down into smaller tasks. Small changes flow through quickly.",
    // The full board hangs off the right edge of the page (see `bleed` below) so
    // it reads as the same live dashboard the hero shows, cropped by the viewport,
    // with work animating rightward through the pipeline.
    mockup: <BoardAnimation />,
    bleed: true,
  },
  {
    title: "Agents provide specialization.",
    body: "Workers build the code. Reviewers scrutinize it: QA, Security, and Adversarial Review ship in the box, each doing one job well. Tailor any of them with custom instructions and skills, or author your own.",
    // Editor bleeds off the left edge (mockup sits on the left on desktop).
    mockup: <AgentInstructionsMockup />,
    edge: "left",
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
    // Editor bleeds off the right edge (mockup sits on the right on desktop).
    mockup: <WorkflowConfigMockup />,
    edge: "right",
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
          <section
            key={item.title}
            // A bleed/edge row lets its mockup run past the content column;
            // `overflow-x-clip` crops it at the viewport edge so it hangs off the
            // page without adding a page scrollbar. The first row drops its top
            // border so the SolutionIntro arrow flows across the seam uninterrupted.
            className={`border-gray-4 ${i === 0 ? "" : "border-t"} ${item.bleed || item.edge ? "overflow-x-clip" : ""}`}
          >
            <div className="mx-auto grid w-full max-w-6xl grid-cols-1 items-center gap-8 px-4 py-10 lg:grid-cols-2 lg:gap-12 lg:px-6 lg:py-16">
              <div className="flex flex-col gap-3">
                <h3 className="font-sans text-2xl font-semibold tracking-tight text-fg sm:text-3xl">
                  {item.title}
                </h3>
                <p className="text-base leading-relaxed text-gray-7 sm:text-lg">
                  {item.body}
                </p>
              </div>
              {item.bleed ? (
                // Board sizes to its full natural width (`w-max`) and overflows
                // this grid cell to the right; the grid track (minmax(0,1fr))
                // stays put and the section's overflow-x-clip crops the spill at
                // the viewport edge, so the board hangs off the page.
                <div className={mockupLeft ? "lg:order-first" : undefined}>
                  <div className="w-max">{item.mockup}</div>
                </div>
              ) : item.edge ? (
                // Edge bleed: the panel spans from the page edge to the far side
                // of its column — `50vw - 3rem` wide (half the viewport minus the
                // grid's half-gutter). A left bleed also pulls its left edge past
                // the centered container to the page edge; a right bleed just
                // overflows rightward. overflow-x-clip on the section crops it.
                <div
                  className={[
                    mockupLeft ? "lg:order-first" : "",
                    "lg:w-[calc(50vw_-_3rem)]",
                    item.edge === "left"
                      ? "lg:ml-[calc(-1*(max(0px,(100vw_-_72rem)/2)_+_3rem))]"
                      : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                >
                  {item.mockup}
                </div>
              ) : (
                <div className={mockupLeft ? "lg:order-first" : undefined}>
                  {item.mockup}
                </div>
              )}
            </div>
          </section>
        )
      })}
    </>
  )
}
