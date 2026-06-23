import { ImageResponse } from "next/og"
import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { notFound } from "next/navigation"
import { OG_CARD_SIZE, OgCard } from "@/components/OgCard"
import { allDocs, getDocBySlug } from "@/lib/docs"

// Per-docs OG card endpoint. Lives outside `/docs/[...slug]/` because Next.js
// forbids any segment after a catch-all, including the `opengraph-image` file
// convention. Each docs page references its card via `openGraph.images` in
// generateMetadata.

export function generateStaticParams() {
  return [
    { slug: [] as string[] },
    ...allDocs.map((doc) => ({
      slug: doc._raw.flattenedPath.replace(/^docs\//, "").split("/"),
    })),
  ]
}

type RouteProps = {
  params: Promise<{ slug?: string[] }>
}

function humanize(section: string): string {
  return section
    .split("-")
    .map((word) => (word ? word[0].toUpperCase() + word.slice(1) : word))
    .join(" ")
}

export async function GET(_request: Request, { params }: RouteProps) {
  const { slug } = await params

  let title: string
  let section: string | undefined
  if (!slug || slug.length === 0) {
    title = "Documentation"
    section = undefined
  } else {
    const doc = getDocBySlug(slug)
    if (!doc) notFound()
    title = doc.title
    section = doc.section ? humanize(doc.section) : undefined
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

  return new ImageResponse(<OgCard title={title} section={section} />, {
    ...OG_CARD_SIZE,
    fonts: [
      { name: "Geist", data: geistRegular, style: "normal", weight: 400 },
      { name: "Geist", data: geistSemiBold, style: "normal", weight: 600 },
      { name: "Geist Mono", data: geistMonoRegular, style: "normal", weight: 400 },
    ],
  })
}
