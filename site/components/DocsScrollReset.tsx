"use client"

import { usePathname } from "next/navigation"
import { useEffect } from "react"

/**
 * Resets window scroll to the top whenever the docs pathname changes.
 * Next.js's `<Link>` preserves scroll position while any part of the
 * shared layout (sticky header + sidebar) is still in the viewport, which
 * after a sidebar click mid-article leaves the incoming page's H1 clipped
 * above the fold. Skip when the URL carries a hash — the browser handles
 * those anchor jumps itself.
 */
export function DocsScrollReset() {
  const pathname = usePathname()
  useEffect(() => {
    if (window.location.hash) return
    window.scrollTo({ top: 0, left: 0, behavior: "auto" })
  }, [pathname])
  return null
}
