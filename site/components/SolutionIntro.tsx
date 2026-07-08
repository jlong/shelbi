export function SolutionIntro() {
  return (
    <section aria-labelledby="solution-heading" className="border-t border-gray-4">
      <div className="mx-auto w-full max-w-6xl px-4 py-6 lg:px-6 lg:py-8">
        <h2
          id="solution-heading"
          className="font-sans text-4xl font-semibold tracking-tight text-fg"
        >
          Inbox Zero, for agent work
        </h2>
        <p className="mt-3 max-w-2xl leading-relaxed text-gray-7">
          Dump work on the orchestrator the moment it occurs to you. It
          assigns the work instead of doing it, so there is nothing to derail.
        </p>
      </div>
    </section>
  )
}
