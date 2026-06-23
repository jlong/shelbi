import { ThemeToggle } from "@/components/ThemeToggle"

/**
 * Marketing chrome — the landing page has no top bar of its own (the Hero
 * carries the wordmark) so we float the ThemeToggle in the top-right corner.
 * Fixed positioning sits the toggle above the hero pattern without nudging
 * the hero's vertical centering.
 */
export default function MarketingLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <>
      <div className="fixed top-2 right-3 z-50 flex items-center">
        <ThemeToggle />
      </div>
      {children}
    </>
  )
}
