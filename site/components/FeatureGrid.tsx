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
        <p className="mt-3 max-w-2xl leading-relaxed text-gray-7">
          Kanban TUI, workers on any machine, plain-file state, open source.
        </p>
      </div>
    </section>
  )
}
