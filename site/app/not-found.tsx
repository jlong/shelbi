import type { Metadata } from "next"
import Link from "next/link"

export const metadata: Metadata = {
  title: "Page not found | Shelbi",
  description: "This page doesn't exist on shelbi.dev.",
}

const LINK_CLASS =
  "text-fg underline underline-offset-4 transition-colors hover:text-gray-7"

/**
 * The site's 404: unmatched URLs plus every `notFound()` from a page. It
 * renders inside the root layout, so the theme boot script, header, and footer
 * all apply. Without this file Next serves its built-in 404, whose inline
 * styles paint the body by `prefers-color-scheme` and ignore the site theme.
 */
export default function NotFound() {
  return (
    <main className="mx-auto w-full max-w-[88rem] px-3 py-8 md:py-12 lg:px-4">
      <h1 className="font-mono text-5xl font-semibold tracking-tight text-fg">
        404
      </h1>
      <p className="mt-2 text-lg text-gray-7">This page doesn&apos;t exist.</p>
      <p className="mt-3 flex flex-wrap items-center gap-x-1">
        <Link href="/" className={LINK_CLASS}>
          Go to the home page
        </Link>
        <span aria-hidden className="text-gray-6">
          ·
        </span>
        <Link href="/docs" className={LINK_CLASS}>
          Browse the docs
        </Link>
      </p>
    </main>
  )
}
