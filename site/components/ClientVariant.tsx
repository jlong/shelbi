import type { ReactNode } from "react"

// The show/hide classes for each client's content, keyed off `<html
// data-client>` (see `lib/client-preference.ts`). Shown as `display: contents`,
// so a wrapper adds no box: its children sit in the parent (a grid cell, a
// flex row, a paragraph) as if unwrapped.
const SHOW_TUI = "contents client-desktop:hidden"
const SHOW_DESKTOP = "hidden client-desktop:contents"

/**
 * Render one child per Shelbi client and show only the chosen one. Both are
 * server-rendered; CSS (`client-tui:` / `client-desktop:` variants keyed off
 * `<html data-client>`) hides the other before first paint, so the page never
 * flashes the wrong client and hydration never mismatches.
 *
 * A hidden variant is `display: none`, so an IntersectionObserver inside it
 * reports it off-screen: the animations pause themselves while their client
 * isn't the one showing. Usable from server and client components alike.
 */
export function ClientVariant({ tui, desktop }: { tui: ReactNode; desktop: ReactNode }) {
  return (
    <>
      <div data-client-variant="tui" className={SHOW_TUI}>
        {tui}
      </div>
      <div data-client-variant="desktop" className={SHOW_DESKTOP}>
        {desktop}
      </div>
    </>
  )
}

/**
 * Docs MDX: content shown only while the TUI is the chosen client. Works as a
 * block (`<Tui>` on its own line around paragraphs, lists, mockups) and inline
 * (`press <Tui>`Ctrl+P`</Tui><Desktop>`⌘K`</Desktop>` inside a sentence), so
 * it renders a `<span>`: valid inside a paragraph, and, as `display:
 * contents`, invisible to the layout around block children. Pair it with a
 * `<Desktop>` sibling; the markdown exports (`lib/llms.ts`) print both,
 * labeled.
 */
export function Tui({ children }: { children: ReactNode }) {
  return (
    <span data-client-variant="tui" className={SHOW_TUI}>
      {children}
    </span>
  )
}

/** Docs MDX: content shown only while the desktop app is the chosen client. See {@link Tui}. */
export function Desktop({ children }: { children: ReactNode }) {
  return (
    <span data-client-variant="desktop" className={SHOW_DESKTOP}>
      {children}
    </span>
  )
}
