import { ImageResponse } from "next/og"
import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { notFound } from "next/navigation"
import { OG_CARD_SIZE, OgCard } from "@/components/OgCard"
import { getComparisonBySlug, visibleComparisons } from "@/lib/comparisons"

// Per-comparison OG card endpoint. Lives outside `/vs/[slug]/` because Next.js
// forbids any segment after a catch-all, including the `opengraph-image` file
// convention. Each comparison page references its card via `openGraph.images`
// in generateMetadata.

export function generateStaticParams() {
  return [
    { slug: [] as string[] },
    ...visibleComparisons.map((comparison) => ({ slug: [comparison.slug] })),
  ]
}

type RouteProps = {
  params: Promise<{ slug?: string[] }>
}

export async function GET(_request: Request, { params }: RouteProps) {
  const { slug } = await params

  let title: string
  if (!slug || slug.length === 0) {
    title = "Comparisons"
  } else {
    const comparison = getComparisonBySlug(slug[0])
    if (!comparison) notFound()
    title = `shelbi vs ${comparison.competitor}`
  }

  const [geistRegular, geistSemiBold, geistMonoRegular] = await Promise.all([
    readFile(
      join(process.cwd(), "node_modules/geist/dist/fonts/geist-sans/Geist-Regular.ttf"),
    ),
    readFile(
      join(process.cwd(), "node_modules/geist/dist/fonts/geist-sans/Geist-SemiBold.ttf"),
    ),
    readFile(
      join(process.cwd(), "node_modules/geist/dist/fonts/geist-mono/GeistMono-Regular.ttf"),
    ),
  ])

  return new ImageResponse(<OgCard title={title} section="Comparison" />, {
    ...OG_CARD_SIZE,
    fonts: [
      { name: "Geist", data: geistRegular, style: "normal", weight: 400 },
      { name: "Geist", data: geistSemiBold, style: "normal", weight: 600 },
      { name: "Geist Mono", data: geistMonoRegular, style: "normal", weight: 400 },
    ],
  })
}
