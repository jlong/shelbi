import type { Metadata } from "next"
import Link from "next/link"
import { getSections, humanizeSection } from "@/lib/docs"
import { OG_CARD_SIZE } from "@/components/OgCard"

export const metadata: Metadata = {
  title: "Docs — shelbi",
  description: "shelbi documentation.",
  openGraph: {
    images: [{ url: "/og/docs", ...OG_CARD_SIZE, alt: "shelbi documentation" }],
  },
  twitter: {
    card: "summary_large_image",
    images: [{ url: "/og/docs", ...OG_CARD_SIZE, alt: "shelbi documentation" }],
  },
}

export default function DocsIndex() {
  const sections = getSections()
  return (
    <main className="max-w-3xl">
      <h1 className="text-3xl font-semibold tracking-tight text-fg">Docs</h1>
      <p className="mt-1 mb-6 text-gray-7">Guides and reference for shelbi.</p>
      <div className="flex flex-col gap-4">
        {sections.map(({ section, docs }) => (
          <section key={section || "_root"}>
            {section ? (
              <h2 className="mb-2 text-sm font-semibold tracking-wide text-gray-6 uppercase">
                {humanizeSection(section)}
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
