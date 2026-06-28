"use client"

import Link from "next/link"
import { usePathname } from "next/navigation"
import { humanizeSection, type DocSection } from "@/lib/docs"

type DocsSidebarProps = {
  sections: DocSection[]
}

/**
 * Sticky docs sidebar. Pinned beside the article column at `md+`; on
 * smaller viewports the unified header drawer carries the docs nav, so
 * this component is hidden. Current-page state is communicated with
 * weight + a leading marker — never with hue (see `site/AGENTS.md`).
 */
export function DocsSidebar({ sections }: DocsSidebarProps) {
  const pathname = usePathname()

  return (
    <aside className="hidden md:sticky md:top-0 md:z-auto md:block md:h-screen md:overflow-y-auto md:py-4">
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
  )
}
