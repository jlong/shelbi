import Link from "next/link"
import { Footer } from "@/components/Footer"
import { Wordmark } from "@/components/Wordmark"

/**
 * Shared chrome for `/vs/*`: sticky top bar with the wordmark and a
 * compact nav, the comparison content in a centered column, and the
 * site-wide footer. Comparisons are short reads — no sidebar.
 */
export default function VsLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <div className="flex min-h-screen flex-col">
      <header className="sticky top-0 z-30 border-b border-gray-4 bg-bg">
        <div className="mx-auto flex h-6 w-full max-w-[88rem] items-center justify-between px-3 lg:px-4">
          <Link href="/" aria-label="Shelbi home" className="inline-flex">
            <Wordmark size="sm" />
          </Link>
          <nav
            aria-label="Primary"
            className="flex items-center gap-3 font-mono text-xs text-gray-7"
          >
            <Link href="/docs" className="transition-colors hover:text-fg">
              Docs
            </Link>
            <Link href="/vs" className="transition-colors hover:text-fg">
              Comparisons
            </Link>
          </nav>
        </div>
      </header>
      <div className="mx-auto w-full max-w-[88rem] flex-1 px-3 py-4 md:py-6 lg:px-4">
        {children}
      </div>
      <Footer />
    </div>
  )
}
