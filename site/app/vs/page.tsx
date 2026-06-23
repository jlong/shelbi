import type { Metadata } from "next"
import Link from "next/link"
import { OG_CARD_SIZE } from "@/components/OgCard"
import { visibleComparisons } from "@/lib/comparisons"

export const metadata: Metadata = {
  title: "Comparisons — Shelbi",
  description:
    "How Shelbi stacks up against other coding-agent tools, side by side.",
  openGraph: {
    images: [{ url: "/og/vs", ...OG_CARD_SIZE, alt: "Shelbi comparisons" }],
  },
  twitter: {
    card: "summary_large_image",
    images: [{ url: "/og/vs", ...OG_CARD_SIZE, alt: "Shelbi comparisons" }],
  },
}

export default function VsIndex() {
  return (
    <main className="mx-auto max-w-3xl">
      <h1 className="text-3xl font-semibold tracking-tight text-fg">
        Comparisons
      </h1>
      <p className="mt-1 mb-6 text-gray-7">
        Honest, side-by-side reads on how Shelbi compares to other
        coding-agent tools — what each does well and when to pick which.
      </p>
      {visibleComparisons.length === 0 ? (
        <p className="text-gray-7">
          No comparisons published yet. Check back soon.
        </p>
      ) : (
        <ul className="flex flex-col gap-1">
          {visibleComparisons.map((comparison) => (
            <li key={comparison.slug}>
              <Link
                href={comparison.url}
                className="flex flex-col rounded-md border border-gray-4 bg-gray-1 px-3 py-2 transition-colors hover:border-gray-5"
              >
                <span className="font-medium text-fg">
                  Shelbi vs {comparison.competitor}
                </span>
                <span className="text-sm text-gray-7">
                  {comparison.summary}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </main>
  )
}
