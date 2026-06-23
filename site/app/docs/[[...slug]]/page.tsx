import type { Metadata } from "next"
import Link from "next/link"
import { notFound } from "next/navigation"
import { getMDXComponent } from "next-contentlayer2/hooks"
import { allDocs } from "contentlayer/generated"
import { getDocBySlug, getSections } from "@/lib/docs"
import { mdxComponents } from "@/components/mdx-components"

// Rendering a contentlayer MDX `body.code` means turning a compiled code string
// into a component at render time — the canonical contentlayer pattern, which
// react-hooks/static-components cannot model. Scope the exception to this route.
/* eslint-disable react-hooks/static-components */

type DocsPageProps = {
  params: Promise<{ slug?: string[] }>
}

export function generateStaticParams() {
  return [
    { slug: [] as string[] },
    ...allDocs.map((doc) => ({
      slug: doc._raw.flattenedPath.replace(/^docs\//, "").split("/"),
    })),
  ]
}

export async function generateMetadata({
  params,
}: DocsPageProps): Promise<Metadata> {
  const { slug } = await params
  const doc = getDocBySlug(slug)
  if (!doc) {
    return { title: "Docs — shelbi", description: "shelbi documentation." }
  }
  return { title: `${doc.title} — shelbi`, description: doc.summary }
}

export default async function DocsPage({ params }: DocsPageProps) {
  const { slug } = await params

  // The optional catch-all serves /docs itself — render the section index.
  if (!slug || slug.length === 0) {
    return <DocsIndex />
  }

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

function humanize(section: string): string {
  return section
    .split("-")
    .map((word) => (word ? word[0].toUpperCase() + word.slice(1) : word))
    .join(" ")
}

function DocsIndex() {
  const sections = getSections()
  return (
    <main className="mx-auto max-w-3xl px-3 py-8 font-sans">
      <h1 className="text-3xl font-semibold tracking-tight text-fg">Docs</h1>
      <p className="mt-1 mb-6 text-gray-7">Guides and reference for shelbi.</p>
      <div className="flex flex-col gap-4">
        {sections.map(({ section, docs }) => (
          <section key={section || "_root"}>
            {section ? (
              <h2 className="mb-2 text-sm font-semibold tracking-wide text-gray-6 uppercase">
                {humanize(section)}
              </h2>
            ) : null}
            <ul className="flex flex-col gap-1">
              {docs.map((doc) => (
                <li key={doc.url}>
                  <Link
                    href={doc.url}
                    className="flex flex-col rounded-md border border-gray-4 bg-gray-1 px-3 py-2 transition-colors hover:border-gray-5"
                  >
                    <span className="font-medium text-fg">{doc.title}</span>
                    <span className="text-sm text-gray-7">{doc.summary}</span>
                  </Link>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </main>
  )
}
