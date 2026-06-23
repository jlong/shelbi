import type { Metadata } from "next"
import { notFound } from "next/navigation"
import { getMDXComponent } from "next-contentlayer2/hooks"
import { allDocs } from "contentlayer/generated"
import { getDocBySlug } from "@/lib/docs"
import { mdxComponents } from "@/components/mdx-components"
import { OG_CARD_SIZE } from "@/components/OgCard"

// Rendering a contentlayer MDX `body.code` means turning a compiled code string
// into a component at render time — the canonical contentlayer pattern, which
// react-hooks/static-components cannot model. Scope the exception to this route.
/* eslint-disable react-hooks/static-components */

type DocsPageProps = {
  params: Promise<{ slug: string[] }>
}

export function generateStaticParams() {
  return allDocs.map((doc) => ({
    slug: doc._raw.flattenedPath.replace(/^docs\//, "").split("/"),
  }))
}

export async function generateMetadata({
  params,
}: DocsPageProps): Promise<Metadata> {
  const { slug } = await params
  const doc = getDocBySlug(slug)
  if (!doc) {
    return { title: "Docs — shelbi", description: "shelbi documentation." }
  }
  const ogUrl = `/og/docs/${slug.map(encodeURIComponent).join("/")}`
  const ogImage = { url: ogUrl, ...OG_CARD_SIZE, alt: `${doc.title} — shelbi docs` }
  return {
    title: `${doc.title} — shelbi`,
    description: doc.summary,
    openGraph: { images: [ogImage] },
    twitter: { card: "summary_large_image", images: [ogImage] },
  }
}

export default async function DocsPage({ params }: DocsPageProps) {
  const { slug } = await params
  const doc = getDocBySlug(slug)
  if (!doc) notFound()

  // contentlayer compiles MDX to a code string that must be turned into a
  // component at render — the canonical pattern. Code-block HTML is already
  // highlighted at build time, so this only assembles static elements.
  const MDX = getMDXComponent(doc.body.code)
  return (
    <main className="mx-auto max-w-3xl px-3 py-8 font-sans">
      <article>
        <header className="mb-6 border-b border-gray-4 pb-3">
          <h1 className="text-3xl font-semibold tracking-tight text-fg">
            {doc.title}
          </h1>
          <p className="mt-1 text-gray-7">{doc.summary}</p>
        </header>
        <MDX components={mdxComponents} />
      </article>
    </main>
  )
}
