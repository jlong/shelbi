/**
 * Shared chrome for `/vs/*`: comparison content in a centered column.
 * Header + footer are rendered by the root layout. Comparisons are
 * short reads — no sidebar.
 */
export default function VsLayout({
  children,
}: {
  children: React.ReactNode
}) {
  return (
    <div className="mx-auto w-full max-w-[88rem] px-3 py-4 md:py-6 lg:px-4">
      {children}
    </div>
  )
}
