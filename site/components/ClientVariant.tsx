import type { ReactNode } from "react"

/**
 * Render one child per Shelbi client and show only the chosen one. Both are
 * server-rendered; CSS (`client-tui:` / `client-desktop:` variants keyed off
 * `<html data-client>`) hides the other before first paint, so the page never
 * flashes the wrong client and hydration never mismatches. The wrappers are
 * `display: contents` while shown, so they add no box to the layout: the
 * child sits in its parent (a grid cell, a flex row) as if unwrapped.
 *
 * A hidden variant is `display: none`, so an IntersectionObserver inside it
 * reports it off-screen: the animations pause themselves while their client
 * isn't the one showing. Usable from server and client components alike.
 */
export function ClientVariant({ tui, desktop }: { tui: ReactNode; desktop: ReactNode }) {
  return (
    <>
      <div data-client-variant="tui" className="contents client-desktop:hidden">
        {tui}
      </div>
      <div data-client-variant="desktop" className="hidden client-desktop:contents">
        {desktop}
      </div>
    </>
  )
}
