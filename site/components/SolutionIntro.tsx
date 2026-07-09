/**
 * Solution intro block for the homepage. Introduces the mental model
 * ("Inbox Zero, for agent work") that the value-prop triad then
 * unpacks. The heading sits below the page H1 (the hero wordmark), so
 * it renders as an H2.
 */
export function SolutionIntro() {
  return (
    <section className="relative border-t border-gray-4">
      <div className="mx-auto w-full max-w-4xl px-4 py-8 lg:px-6 lg:py-12">
        <div className="flex flex-col items-center gap-4 text-center">
          <p className="-mb-1.5 font-mono text-base font-medium uppercase tracking-[0.2em] text-accent">Meet Shelbi</p>
          <h2 className="font-sans text-5xl font-semibold tracking-tight text-fg">
            This is Inbox Zero for agent work.
          </h2>
          <p className="text-base leading-relaxed text-gray-7 sm:text-lg">
            Have an idea? Spot a bug? Simply talk to the Shelbi orchestrator
            agent the moment it occurs to you. The orchestrator doesn&apos;t
            do the work itself so it won&apos;t get derailed. Each item is
            written up in a standard format. Nothing small or big gets
            dropped. Let the agent write it up for you!
          </p>
        </div>
      </div>

      {/* A hand-drawn, looping down arrow (exported from design, recolored to
          the accent via currentColor) straddling the bottom border into the
          next section to pull the eye downward — centered on the section's
          bottom edge and hung half its height below it. Decorative + non-
          interactive, so hidden from AT and click-through. */}
      <div
        className="pointer-events-none absolute bottom-0 left-1/2 z-10 -translate-x-1/2 translate-y-1/2 text-accent"
        aria-hidden="true"
      >
        <svg
          viewBox="-2 -1 50 110"
          fill="none"
          stroke="currentColor"
          strokeWidth={3}
          strokeLinecap="round"
          strokeLinejoin="round"
          className="h-[90px] w-auto lg:h-16"
        >
          <path d="M19.0799 2.25C14.5799 25.25 37.5798 33.75 28.5799 50.75C18.649 69.5084 -1.68552 61.3994 2.57992 47.75C5.07992 39.75 17.5799 37.75 22.5799 54.75C26.1962 67.0452 26.7466 84.25 25.5799 103.75M7.07994 85.25L25.5799 105.75L43.0799 85.25" />
        </svg>
      </div>
    </section>
  )
}
