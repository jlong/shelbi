import { Footer } from "@/components/Footer"

/**
 * Shared chrome for `/vs/*`: comparison content in a centered column,
 * plus the site-wide footer. Header is rendered by the root layout.
 * Comparisons are short reads — no sidebar.
 */
export default function VsLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <div className="flex min-h-screen flex-col">
      <div className="mx-auto w-full max-w-[88rem] flex-1 px-3 py-4 md:py-6 lg:px-4">
        {children}
      </div>
      <Footer />
    </div>
  )
}
