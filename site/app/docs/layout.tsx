import { DocsScrollReset } from "@/components/DocsScrollReset"
import { DocsSidebar } from "@/components/DocsSidebar"
import { getSections } from "@/lib/docs"

/**
 * Reading layout for `/docs/*`: a sticky sidebar (mobile drawer below
 * `md`) and the article slot. The site-wide header + footer are rendered
 * by the root layout; the on-this-page rail is rendered by the page itself
 * because it needs the doc-specific heading list.
 */
export default function DocsLayout({
  children,
}: {
  children: React.ReactNode
}) {
  const sections = getSections()
  return (
    <div className="mx-auto w-full max-w-[88rem] px-3 md:grid md:grid-cols-[14rem_minmax(0,1fr)] md:gap-4 md:px-4 lg:gap-6">
      <DocsScrollReset />
      <DocsSidebar sections={sections} />
      <div className="min-w-0 py-4 md:py-6">{children}</div>
    </div>
  )
}
