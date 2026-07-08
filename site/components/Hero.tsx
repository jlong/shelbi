import Link from "next/link"
import type { SVGProps } from "react"
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

const GITHUB_HREF = "https://github.com/jlong/shelbi"

// Heroicons has no brand marks, so the GitHub octocat is inlined here
// (official mark path, fill-based) rather than adding an icon dependency.
function GitHubIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" {...props}>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
    </svg>
  )
}

const CTA_BASE =
  "inline-flex items-center justify-center gap-1 px-3 py-2 font-mono text-sm font-medium outline-none transition-colors focus-visible:[outline-style:solid] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-fg"

export function Hero() {
  return (
    <section data-hero className="relative isolate overflow-hidden">
      <HeroPattern />
      <div className="relative mx-auto flex w-full max-w-4xl flex-col items-center gap-5 px-3 pb-3 pt-12 text-center md:pb-2.5 md:pt-16">
        <WordmarkSvg className="w-full max-w-[240px] text-fg md:max-w-[420px]" />

        <h1 className="max-w-3xl text-balance text-4xl font-semibold leading-tight text-fg sm:text-5xl md:text-6xl">
          Stop babysitting agent tabs.
        </h1>

        <h2 className="max-w-2xl text-balance text-base font-normal leading-relaxed text-gray-7 sm:text-lg">
          With Shelbi, talk to one agent that writes up work as tasks,
          dispatches to workers, and brings finished work back to you for
          review.
        </h2>

        <p className="font-mono text-xs uppercase tracking-[0.25em] text-gray-7 sm:text-sm">
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

        <a
          href={GITHUB_HREF}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1.5 font-mono text-xs text-gray-7 transition-colors hover:text-fg sm:text-sm"
        >
          <GitHubIcon className="h-2.5 w-2.5" aria-hidden="true" />
          Star on GitHub
        </a>
      </div>
    </section>
  )
}
