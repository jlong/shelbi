import type { Metadata } from "next"
import Link from "next/link"
import { notFound } from "next/navigation"
import { getMDXComponent } from "next-contentlayer2/hooks"
import { ArrowRightIcon } from "@heroicons/react/24/outline"
import { mdxComponents } from "@/components/mdx-components"
import { OG_CARD_SIZE } from "@/components/OgCard"
import { getComparisonBySlug, visibleComparisons } from "@/lib/comparisons"

// Rendering a contentlayer MDX `body.code` means turning a compiled code string
// into a component at render time — the canonical contentlayer pattern, which
// react-hooks/static-components cannot model. Scope the exception to this route.
/* eslint-disable react-hooks/static-components */

type ComparisonPageProps = {
  params: Promise<{ slug: string }>
}

export function generateStaticParams() {
  return visibleComparisons.map((comparison) => ({ slug: comparison.slug }))
}

export async function generateMetadata({
  params,
}: ComparisonPageProps): Promise<Metadata> {
  const { slug } = await params
  const comparison = getComparisonBySlug(slug)
  if (!comparison) {
    return {
      title: "Comparisons — shelbi",
      description: "How shelbi compares to other coding-agent tools.",
    }
  }
  const ogUrl = `/og/vs/${encodeURIComponent(slug)}`
  const alt = `shelbi vs ${comparison.competitor}`
  const ogImage = { url: ogUrl, ...OG_CARD_SIZE, alt }
  return {
    title: `shelbi vs ${comparison.competitor} — shelbi`,
    description: comparison.summary,
    openGraph: { images: [ogImage] },
    twitter: { card: "summary_large_image", images: [ogImage] },
  }
}

export default async function ComparisonPage({ params }: ComparisonPageProps) {
  const { slug } = await params
  const comparison = getComparisonBySlug(slug)
  if (!comparison) notFound()

  // contentlayer compiles MDX to a code string that must be turned into a
  // component at render — the canonical pattern. Code-block HTML is already
  // highlighted at build time, so this only assembles static elements.
  const MDX = getMDXComponent(comparison.body.code)

  return (
    <article className="mx-auto max-w-3xl">
      <header className="mb-6 border-b border-gray-4 pb-4">
        <p className="font-mono text-xs uppercase tracking-[0.25em] text-gray-6">
          Comparison
        </p>
        <h1 className="mt-2 text-4xl font-semibold tracking-tight text-fg">
          shelbi vs {comparison.competitor}
        </h1>
        <p className="mt-2 text-balance text-lg leading-relaxed text-gray-7">
          {comparison.summary}
        </p>
      </header>

      <MDX components={mdxComponents} />

      <footer className="mt-8 flex flex-col gap-2 border-t border-gray-4 pt-4 text-sm">
        {comparison.researchUrl ? (
          <Link
            href={comparison.researchUrl}
            className="inline-flex items-center gap-1 text-fg underline underline-offset-4 transition-colors hover:text-gray-7"
          >
            Read the research summary
            <ArrowRightIcon className="h-2 w-2" aria-hidden="true" />
          </Link>
        ) : null}
        <Link
          href={comparison.competitorUrl}
          className="inline-flex items-center gap-1 text-fg underline underline-offset-4 transition-colors hover:text-gray-7"
        >
          Visit {comparison.competitor}
          <ArrowRightIcon className="h-2 w-2" aria-hidden="true" />
        </Link>
        <Link
          href="/vs"
          className="mt-2 inline-flex items-center gap-1 text-gray-7 transition-colors hover:text-fg"
        >
          ← All comparisons
        </Link>
      </footer>
    </article>
  )
}
