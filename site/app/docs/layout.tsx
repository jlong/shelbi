import Link from "next/link"
import { DocsSidebar } from "@/components/DocsSidebar"
import { Wordmark } from "@/components/Wordmark"
import { getSections } from "@/lib/docs"

/**
 * Reading layout for `/docs/*`: a top bar with the wordmark, a sticky
 * sidebar (mobile drawer below `md`), and the article slot. The
 * on-this-page rail is rendered by the page itself because it needs
 * the doc-specific heading list.
 */
export default function DocsLayout({
  children,
}: {
  children: React.ReactNode
}) {
  const sections = getSections()
  return (
    <div className="flex min-h-screen flex-col">
      <header className="sticky top-0 z-30 border-b border-gray-4 bg-bg">
        <div className="mx-auto flex h-6 w-full max-w-[88rem] items-center justify-between px-3 lg:px-4">
          <Link href="/" aria-label="Shelbi home" className="inline-flex">
            <Wordmark size="sm" />
          </Link>
        </div>
      </header>
      <div className="mx-auto w-full max-w-[88rem] flex-1 px-3 md:grid md:grid-cols-[14rem_minmax(0,1fr)] md:gap-4 md:px-4 lg:gap-6">
        <DocsSidebar sections={sections} />
        <div className="min-w-0 py-4 md:py-6">{children}</div>
      </div>
    </div>
  )
}
