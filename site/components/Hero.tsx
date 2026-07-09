import Link from "next/link"
import { ArrowRightIcon } from "@heroicons/react/24/outline"

const CTA_BASE =
  "inline-flex items-center justify-center gap-1 rounded-sm px-3 py-2 font-mono text-sm font-medium outline-none transition-colors focus-visible:[outline-style:solid] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-fg"

export function Hero() {
  return (
    <section data-hero className="relative">
      <div className="relative mx-auto flex w-full max-w-4xl flex-col items-center gap-5 px-3 pb-3 pt-6 text-center md:pb-2.5 md:pt-8">
        <h1 className="max-w-4xl text-balance text-4xl font-semibold tracking-tight text-fg sm:text-5xl md:text-7xl">
          Stop babysitting agents.
        </h1>

        <h2 className="max-w-2xl text-base font-normal leading-relaxed text-gray-7 sm:text-lg">
          Tired of managing agents in terminal tabs? Struggling to keep track
          of which one needs attention and which one&apos;s stalled? Try
          Shelbi, an open source, personal agent orchestrator built on tmux.
        </h2>

        <p className="-my-2.5 font-mono text-xs uppercase tracking-[0.25em] text-accent sm:text-sm">
          open source · made with tmux · multi-machine
        </p>

        <div className="mt-2 flex w-full flex-col gap-2 sm:w-auto sm:flex-row sm:gap-3">
          <Link
            href="/docs/guides/getting-started/install"
            className={`${CTA_BASE} bg-fg text-bg hover:bg-gray-7`}
          >
            Install now
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
