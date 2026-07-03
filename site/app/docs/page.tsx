import type { Metadata } from "next"
import Link from "next/link"
import {
  ArrowRightIcon,
  ArrowsRightLeftIcon,
  CommandLineIcon,
  RocketLaunchIcon,
  Squares2X2Icon,
} from "@heroicons/react/24/outline"
import { OG_CARD_SIZE } from "@/components/OgCard"

export const metadata: Metadata = {
  title: "Docs — Shelbi",
  description: "Shelbi documentation.",
  openGraph: {
    images: [{ url: "/og/docs", ...OG_CARD_SIZE, alt: "Shelbi documentation" }],
  },
  twitter: {
    card: "summary_large_image",
    images: [{ url: "/og/docs", ...OG_CARD_SIZE, alt: "Shelbi documentation" }],
  },
}

/**
 * A curated set of the guides worth featuring on the landing. This is a hand-
 * picked short list, not the full nav — the sidebar already carries every page.
 * Kept in sync by hand so the home page can orient a new reader rather than
 * relist what they can already see.
 */
const guides = [
  {
    icon: ArrowsRightLeftIcon,
    title: "Understanding Workflows",
    href: "/docs/guides/understanding-workflows",
    body: "Map a git branching model — trunk-based, git-flow, feature-branch, or forking — onto a Shelbi Workflow: the statuses, transitions, and orchestrator tweaks for each.",
  },
  {
    icon: Squares2X2Icon,
    title: "Workflows",
    href: "/docs/guides/getting-started/workflows",
    body: "The fundamentals every other guide builds on — what a Workflow is, how statuses and git side-effects fit together, and why it's the unit you configure per project.",
  },
]

const concepts = [
  {
    title: "Orchestrator",
    href: "/docs/concepts/orchestrator",
    body: "The agent you talk to — it dispatches tasks and tails the events log.",
  },
  {
    title: "Workspaces",
    href: "/docs/concepts/workspaces",
    body: "Capacity: a tmux pane plus a git worktree the orchestrator loads with work.",
  },
  {
    title: "Agents",
    href: "/docs/concepts/agents",
    body: "A role — system prompt plus skills. Shelbi ships three; you can author more.",
  },
  {
    title: "Zen Mode",
    href: "/docs/concepts/zen-mode",
    body: "Hands-off auto-merge — cleared work lands without waiting on you.",
  },
]

const cli = [
  {
    title: "shelbi task",
    href: "/docs/cli/task",
    body: "Add, list, move, assign, and start tasks on the board.",
  },
  {
    title: "shelbi workspace",
    href: "/docs/cli/workspace",
    body: "Inspect the workspace pool and stop a pane to free a stuck task.",
  },
  {
    title: "shelbi workflow",
    href: "/docs/cli/workflow",
    body: "List, show, scaffold, and edit the per-project workflow YAML.",
  },
  {
    title: "shelbi merge",
    href: "/docs/cli/merge",
    body: "Merge a workspace branch into the default branch, locally or via PR.",
  },
]

export default function DocsIndex() {
  return (
    <main className="max-w-3xl">
      <h1 className="text-3xl font-semibold tracking-tight text-fg">Docs</h1>
      <p className="mt-2 max-w-2xl text-gray-7">
        Shelbi is an agent orchestrator for the terminal — a board of tasks, a
        pool of git workspaces, and an orchestrator that keeps them loaded with
        work. These docs take you from install to a running loop, then explain
        the pieces and the CLI behind it.
      </p>

      {/* Start here — the single most important entry point. */}
      <Link
        href="/docs/guides/getting-started"
        className="group mt-6 flex flex-col rounded-md border border-gray-4 bg-gray-1 p-3 transition-colors hover:border-gray-5"
      >
        <span className="inline-flex items-center gap-1 text-xs font-semibold tracking-wide text-gray-6 uppercase">
          <RocketLaunchIcon className="h-2 w-2" aria-hidden="true" />
          Start here
        </span>
        <span className="mt-1 text-xl font-semibold text-fg">
          Getting Started
        </span>
        <span className="mt-1 max-w-2xl text-gray-7">
          The shortest path from an empty machine to the loop running on your
          own repo — install the binary, set up a project, run a task, scale to
          a pool of workspaces, and hand the loop to the orchestrator.
        </span>
        <span className="mt-2 inline-flex items-center gap-1 text-sm text-gray-7 group-hover:font-semibold group-hover:text-fg">
          Start the guide
          <ArrowRightIcon className="h-2 w-2" aria-hidden="true" />
        </span>
      </Link>

      {/* Featured guides. */}
      <h2 className="mt-8 mb-2 text-sm font-semibold tracking-wide text-gray-6 uppercase">
        Guides
      </h2>
      <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
        {guides.map((guide) => (
          <Link
            key={guide.href}
            href={guide.href}
            className="group flex flex-col rounded-md border border-gray-4 bg-gray-1 p-3 transition-colors hover:border-gray-5"
          >
            <guide.icon className="h-3 w-3 text-fg" aria-hidden="true" />
            <span className="mt-1 font-semibold text-fg">{guide.title}</span>
            <span className="mt-1 text-sm text-gray-7">{guide.body}</span>
          </Link>
        ))}
      </div>

      {/* Secondary entry points — a few high-value links, not the whole tree. */}
      <div className="mt-8 grid grid-cols-1 gap-6 sm:grid-cols-2">
        <section>
          <h2 className="mb-2 text-sm font-semibold tracking-wide text-gray-6 uppercase">
            Concepts
          </h2>
          <ul className="flex flex-col gap-1">
            {concepts.map((item) => (
              <li key={item.href}>
                <Link
                  href={item.href}
                  className="group flex flex-col rounded-md px-2 py-1 transition-colors hover:bg-gray-1"
                >
                  <span className="font-medium text-fg group-hover:underline">
                    {item.title}
                  </span>
                  <span className="text-sm text-gray-7">{item.body}</span>
                </Link>
              </li>
            ))}
          </ul>
        </section>
        <section>
          <h2 className="mb-2 inline-flex items-center gap-1 text-sm font-semibold tracking-wide text-gray-6 uppercase">
            <CommandLineIcon className="h-2 w-2" aria-hidden="true" />
            CLI Reference
          </h2>
          <ul className="flex flex-col gap-1">
            {cli.map((item) => (
              <li key={item.href}>
                <Link
                  href={item.href}
                  className="group flex flex-col rounded-md px-2 py-1 transition-colors hover:bg-gray-1"
                >
                  <span className="font-mono text-sm font-medium text-fg group-hover:underline">
                    {item.title}
                  </span>
                  <span className="text-sm text-gray-7">{item.body}</span>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      </div>

      {/* AI-ergonomics — the audience hands these docs to agents. */}
      <section className="mt-8 rounded-md border border-gray-4 bg-gray-1 p-3">
        <h2 className="text-sm font-semibold tracking-wide text-gray-6 uppercase">
          Reading with an agent
        </h2>
        <p className="mt-1 text-sm text-gray-7">
          Every page is available as clean markdown — append{" "}
          <code className="font-mono text-fg">.md</code> to any docs URL. Hand
          the whole corpus to a coding agent, or grab a ready-made prompt.
        </p>
        <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-sm">
          <Link
            href="/docs/ai-prompts"
            className="font-medium text-fg hover:underline"
          >
            AI prompts
          </Link>
          <a href="/llms.txt" className="font-mono text-gray-7 hover:text-fg">
            /llms.txt
          </a>
          <a
            href="/llms-full.txt"
            className="font-mono text-gray-7 hover:text-fg"
          >
            /llms-full.txt
          </a>
        </div>
      </section>
    </main>
  )
}
