export function ProblemSection() {
  return (
    <section aria-labelledby="problem-heading" className="border-t border-gray-4">
      <div className="mx-auto w-full max-w-6xl px-4 py-6 lg:px-6 lg:py-8">
        <h2
          id="problem-heading"
          className="font-sans text-4xl font-semibold tracking-tight text-fg"
        >
          You are the router
        </h2>
        <p className="mt-3 max-w-2xl leading-relaxed text-gray-7">
          One agent per terminal tab, and every tab needs you.
        </p>
      </div>
    </section>
  )
}
