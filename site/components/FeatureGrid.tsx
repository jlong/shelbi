import {
  ClipboardDocumentCheckIcon,
  CodeBracketIcon,
  CommandLineIcon,
  DocumentTextIcon,
  ServerStackIcon,
  ViewColumnsIcon,
} from "@heroicons/react/24/outline"
import type { ComponentType, SVGProps } from "react"

type IconType = ComponentType<SVGProps<SVGSVGElement>>

const FEATURES: { label: string; body: string; Icon: IconType }[] = [
  {
    label: "Kanban TUI",
    body: "Every task is a card on a board in your terminal, so status is on screen instead of in your head.",
    Icon: ViewColumnsIcon,
  },
  {
    label: "Workers on any machine",
    body: "Any box you can SSH into can take tasks. If it runs tmux and an agent CLI, it's a worker.",
    Icon: ServerStackIcon,
  },
  {
    label: "Made with tmux",
    body: "Every worker runs in a real tmux pane. Attach to a session to watch an agent work or type to it directly.",
    Icon: CommandLineIcon,
  },
  {
    label: "Review flow",
    body: "Finished tasks land in the review column and wait for you. Assign a review agent to any column to hold every task to the same bar.",
    Icon: ClipboardDocumentCheckIcon,
  },
  {
    label: "Plain-file state",
    body: "Tasks are markdown and workflows are YAML, stored in your repo. No database, no cloud, just files you can read, grep, and commit.",
    Icon: DocumentTextIcon,
  },
  {
    label: "Open source",
    body: "The whole system is MIT licensed on GitHub. You can read every line that runs on your machines.",
    Icon: CodeBracketIcon,
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
              <feature.Icon className="h-3 w-3 text-gray-6" aria-hidden="true" />
              <dt className="mt-2 font-semibold text-fg">{feature.label}</dt>
              <dd className="mt-1 leading-relaxed text-gray-7">{feature.body}</dd>
            </div>
          ))}
        </dl>
      </div>
    </section>
  )
}
