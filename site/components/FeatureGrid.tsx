const FEATURES = [
  {
    label: "Kanban TUI",
    body: "Every task is a card on a board in your terminal, so status is on screen instead of in your head.",
  },
  {
    label: "Workers on any machine",
    body: "Any box you can SSH into can take tasks. If it runs tmux and an agent CLI, it's a worker.",
  },
  {
    label: "Made with tmux",
    body: "Every worker runs in a real tmux pane. Attach to a session to watch an agent work or type to it directly.",
  },
  {
    label: "Review flow",
    body: "Finished tasks land in the review column and wait for you. Assign a review agent to any column to hold every task to the same bar.",
  },
  {
    label: "Plain-file state",
    body: "Tasks are markdown and workflows are YAML, stored in your repo. No database, no cloud, just files you can read, grep, and commit.",
  },
  {
    label: "Open source",
    body: "The whole system is MIT licensed on GitHub. You can read every line that runs on your machines.",
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
        <dl className="mt-5 grid gap-x-5 gap-y-4 sm:grid-cols-2 lg:grid-cols-3">
          {FEATURES.map((feature) => (
            <div key={feature.label}>
              <dt className="font-semibold text-fg">{feature.label}</dt>
              <dd className="mt-1 leading-relaxed text-gray-7">{feature.body}</dd>
            </div>
          ))}
        </dl>
      </div>
    </section>
  )
}
