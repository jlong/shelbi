/**
 * Solution intro block for the homepage. Introduces the mental model
 * ("Inbox Zero, for agent work") that the value-prop triad then
 * unpacks. The heading sits below the page H1 (the hero wordmark), so
 * it renders as an H2.
 */
export function SolutionIntro() {
  return (
    <section className="border-t border-gray-4">
      <div className="mx-auto w-full max-w-3xl px-4 py-8 lg:px-6 lg:py-12">
        <div className="flex flex-col gap-4">
          <p className="font-mono text-xs font-medium uppercase tracking-[0.2em] text-accent">With Shelbi</p>
          <h2 className="font-sans text-3xl font-semibold tracking-tight text-fg sm:text-4xl">
            Inbox Zero, for agent work.
          </h2>
          <p className="text-base leading-relaxed text-gray-7 sm:text-lg">
            Dump work on the orchestrator the moment it occurs to you. It
            doesn&apos;t do the work itself, so there&apos;s nothing to
            derail. Each item becomes a focused task, and nothing gets
            dropped. Out of your head. Off your tabs.
          </p>
        </div>
      </div>
    </section>
  )
}
