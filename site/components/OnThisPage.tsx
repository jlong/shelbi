"use client"

import { useEffect, useState } from "react"
import type { Heading } from "@/lib/docs"

type OnThisPageProps = {
  headings: Heading[]
}

/**
 * Sticky on-this-page rail. Renders H2/H3 headings as anchor links and
 * tracks the currently-visible heading with an IntersectionObserver so
 * the active row stays in sync with scroll. Active state is bold weight
 * (per the strict-mono palette — no hue).
 */
export function OnThisPage({ headings }: OnThisPageProps) {
  const [activeId, setActiveId] = useState<string | null>(headings[0]?.id ?? null)

  useEffect(() => {
    if (headings.length === 0) return

    const ids = headings.map((h) => h.id)
    const elements = ids
      .map((id) => document.getElementById(id))
      .filter((el): el is HTMLElement => el !== null)
    if (elements.length === 0) return

    // Track which headings are currently inside a narrow band near the top
    // of the viewport. The highest one in that band wins; if none are in the
    // band we fall back to the last one above it (so scrolling past the last
    // heading still highlights it instead of clearing).
    const visible = new Set<string>()
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) visible.add(entry.target.id)
          else visible.delete(entry.target.id)
        }
        const firstVisible = ids.find((id) => visible.has(id))
        if (firstVisible) {
          setActiveId(firstVisible)
          return
        }
        // Nothing in the band — pick the last heading that has scrolled past
        // the band's top edge.
        let fallback: string | null = null
        for (const el of elements) {
          if (el.getBoundingClientRect().top < 120) fallback = el.id
        }
        if (fallback) setActiveId(fallback)
      },
      {
        // A band running from just below the top of the viewport down to
        // ~60% — keeps the active row aligned with what the reader is
        // actually reading rather than with whatever's at the very top.
        rootMargin: "-80px 0px -40% 0px",
        threshold: 0,
      },
    )

    for (const el of elements) observer.observe(el)
    return () => observer.disconnect()
  }, [headings])

  if (headings.length === 0) return null

  return (
    <nav
      aria-label="On this page"
      className="text-sm"
    >
      <h2 className="mb-1 font-mono text-xs font-medium tracking-wider text-gray-6 uppercase">
        On this page
      </h2>
      <ul className="flex flex-col">
        {headings.map((heading) => {
          const isActive = heading.id === activeId
          return (
            <li key={heading.id}>
              <a
                href={`#${heading.id}`}
                aria-current={isActive ? "location" : undefined}
                className={[
                  "block py-0.5 transition-colors",
                  heading.depth === 3 ? "pl-3" : "",
                  isActive ? "font-semibold text-fg" : "text-gray-7 hover:text-fg",
                ].join(" ")}
              >
                {heading.text}
              </a>
            </li>
          )
        })}
      </ul>
    </nav>
  )
}
