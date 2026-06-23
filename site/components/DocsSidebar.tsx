"use client"

import { Bars3Icon, XMarkIcon } from "@heroicons/react/24/outline"
import Link from "next/link"
import { usePathname } from "next/navigation"
import { useEffect, useState } from "react"
import { humanizeSection, type DocSection } from "@/lib/docs"

type DocsSidebarProps = {
  sections: DocSection[]
}

/**
 * Sticky docs sidebar. On `md+` it pins to the viewport beside the article
 * column. On mobile it collapses behind a fixed hamburger trigger that opens
 * a full-height slide-in drawer. Current-page state is communicated with
 * weight + a leading marker — never with hue (see `site/AGENTS.md`).
 */
export function DocsSidebar({ sections }: DocsSidebarProps) {
  const pathname = usePathname()
  const [open, setOpen] = useState(false)

  // Lock background scroll while the drawer is open. Restored on close /
  // unmount so we never leave the user with a frozen page.
  useEffect(() => {
    if (!open) return
    const prev = document.body.style.overflow
    document.body.style.overflow = "hidden"
    return () => {
      document.body.style.overflow = prev
    }
  }, [open])

  // Escape closes the drawer.
  useEffect(() => {
    if (!open) return
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false)
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [open])

  return (
    <>
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-label={open ? "Close navigation" : "Open navigation"}
        aria-expanded={open}
        className="fixed top-1 right-1 z-50 inline-flex h-4 w-4 items-center justify-center rounded-md border border-gray-4 bg-gray-1 text-fg md:hidden"
      >
        {open ? (
          <XMarkIcon className="h-3 w-3" aria-hidden="true" />
        ) : (
          <Bars3Icon className="h-3 w-3" aria-hidden="true" />
        )}
      </button>

      {open ? (
        <div
          aria-hidden="true"
          onClick={() => setOpen(false)}
          className="fixed inset-0 z-30 bg-bg/80 md:hidden"
        />
      ) : null}

      <aside
        className={
          // Desktop: a regular grid column that goes sticky just below the
          // viewport top. Mobile: a fixed slide-in drawer animated via the
          // `open` state, hidden off-canvas when closed.
          [
            "fixed inset-y-0 left-0 z-40 w-[18rem] transform overflow-y-auto border-r border-gray-4 bg-bg px-3 py-4 transition-transform duration-200 ease-out",
            "md:sticky md:top-0 md:z-auto md:h-screen md:w-auto md:transform-none md:border-r-0 md:bg-transparent md:px-0 md:py-4 md:transition-none",
            open ? "translate-x-0" : "-translate-x-full md:translate-x-0",
          ].join(" ")
        }
      >
        <nav aria-label="Documentation">
          <ul className="flex flex-col gap-3">
            {sections.map(({ section, docs }) => (
              <li key={section || "_root"}>
                {section ? (
                  <h2 className="mb-1 font-mono text-xs font-medium tracking-wider text-gray-6 uppercase">
                    {humanizeSection(section)}
                  </h2>
                ) : null}
                <ul className="flex flex-col">
                  {docs.map((doc) => {
                    const isActive = pathname === doc.url
                    return (
                      <li key={doc.url}>
                        <Link
                          href={doc.url}
                          aria-current={isActive ? "page" : undefined}
                          onClick={() => setOpen(false)}
                          className={[
                            "relative block py-0.5 pl-2 text-sm transition-colors",
                            isActive
                              ? "font-semibold text-fg before:absolute before:top-1/2 before:left-0 before:h-2 before:w-px before:-translate-y-1/2 before:bg-fg"
                              : "text-gray-7 hover:text-fg",
                          ].join(" ")}
                        >
                          {doc.title}
                        </Link>
                      </li>
                    )
                  })}
                </ul>
              </li>
            ))}
          </ul>
        </nav>
      </aside>
    </>
  )
}
