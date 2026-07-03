import type { Metadata } from "next"
import { DocumentTextIcon } from "@heroicons/react/24/outline"
import { notFound } from "next/navigation"
import { getMDXComponent } from "next-contentlayer2/hooks"
import { allDocs } from "contentlayer/generated"
import {
  extractHeadings,
  getDocBySlug,
  getEditUrl,
  getPrevNext,
} from "@/lib/docs"
import { DocFooter } from "@/components/DocFooter"
import { OnThisPage } from "@/components/OnThisPage"
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
    return { title: "Docs — Shelbi", description: "Shelbi documentation." }
  }
  const ogUrl = `/og/docs/${slug.map(encodeURIComponent).join("/")}`
  const ogImage = { url: ogUrl, ...OG_CARD_SIZE, alt: `${doc.title} — Shelbi docs` }
  return {
    title: `${doc.title} — Shelbi`,
    description: doc.summary,
    openGraph: { images: [ogImage] },
    twitter: { card: "summary_large_image", images: [ogImage] },
  }
}

export default async function DocsPage({ params }: DocsPageProps) {
  const { slug } = await params
  const doc = getDocBySlug(slug)
  if (!doc) notFound()

  const headings = extractHeadings(doc)
  const { prev, next } = getPrevNext(doc)
  const editUrl = getEditUrl(doc)

  // contentlayer compiles MDX to a code string that must be turned into a
  // component at render — the canonical pattern. Code-block HTML is already
  // highlighted at build time, so this only assembles static elements.
  const MDX = getMDXComponent(doc.body.code)

  return (
    <div className="lg:grid lg:grid-cols-[minmax(0,1fr)_12rem] lg:gap-6">
      <article className="min-w-0 max-w-3xl">
        <header className="mb-4 border-b border-gray-4 pb-3">
          <div className="flex items-start justify-between gap-3">
            <h1 className="text-3xl font-semibold tracking-tight text-fg">
              {doc.title}
            </h1>
            {/* Plain <a>, not next/link: the target is a text/markdown route
                handler, so client-side navigation would fetch RSC and break. */}
            <a
              href={`${doc.url}.md`}
              className="mt-1 inline-flex shrink-0 items-center gap-1 whitespace-nowrap font-mono text-xs text-gray-6 transition-colors hover:text-fg"
            >
              <DocumentTextIcon className="h-2 w-2" aria-hidden="true" />
              View as markdown
            </a>
          </div>
        </header>
        <MDX components={mdxComponents} />
        <DocFooter prev={prev} next={next} editUrl={editUrl} />
      </article>
      <div className="hidden lg:block">
        <div className="sticky top-8">
          <OnThisPage headings={headings} />
        </div>
      </div>
    </div>
  )
}
