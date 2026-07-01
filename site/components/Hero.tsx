import Link from "next/link"
import { ArrowRightIcon } from "@heroicons/react/24/outline"
import { BlockDivider, WordmarkSvg } from "./Wordmark"

/**
 * Decorative field of evenly-spaced block characters, faded at the
 * edges so the wordmark always reads first. Sits behind the hero
 * content via absolute positioning and is hidden from screen readers.
 */
function HeroPattern() {
  return (
    <div
      aria-hidden="true"
      className="pointer-events-none absolute inset-0 overflow-hidden text-gray-3 [mask-image:radial-gradient(ellipse_at_center,black_10%,transparent_75%)]"
    >
      <div className="flex h-full flex-col justify-between py-2">
        {Array.from({ length: 14 }).map((_, i) => (
          <BlockDivider key={i} cells={64} height={4} gapRatio={1.5} />
        ))}
      </div>
    </div>
  )
}

const CTA_BASE =
  "inline-flex items-center justify-center gap-1 px-3 py-2 font-mono text-sm font-medium outline-none transition-colors focus-visible:[outline-style:solid] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-fg"

export function Hero() {
  return (
    <section
      data-hero
      className="relative isolate overflow-hidden border-b border-gray-4"
    >
      <HeroPattern />
      <div className="relative mx-auto flex w-full max-w-4xl flex-col items-center gap-5 px-3 py-12 text-center md:py-16">
        <h1 className="w-full max-w-[320px] text-fg md:max-w-[680px]">
          <span className="sr-only">Shelbi</span>
          <WordmarkSvg className="w-full" aria-hidden="true" />
        </h1>

        <p className="font-mono text-xs uppercase tracking-[0.25em] text-gray-7 sm:text-sm">
          A Kanban board for your coding agents
        </p>

        <p className="max-w-2xl text-balance text-base leading-relaxed text-gray-7 sm:text-lg">
          An open-source agent orchestrator for the terminal. Drop tasks
          in the backlog and a named pool of workspaces picks them up in
          parallel — locally, or on any SSH-reachable machine. Finished
          work lands in a review column for you to inspect, or flip on
          Zen Mode and let cleared branches auto-merge.
        </p>

        <div className="mt-2 flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:gap-3">
          <Link
            href="/docs/getting-started/install"
            className={`${CTA_BASE} bg-fg text-bg hover:bg-gray-7`}
          >
            Install shelbi
            <ArrowRightIcon className="h-2 w-2" aria-hidden="true" />
          </Link>
          <Link
            href="/docs"
            className={`${CTA_BASE} border border-gray-5 text-fg hover:border-fg hover:bg-gray-2`}
          >
            Read the docs
          </Link>
        </div>
      </div>
    </section>
  )
}
