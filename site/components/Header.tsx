"use client"

import { Bars3Icon, XMarkIcon } from "@heroicons/react/24/outline"
import Link from "next/link"
import { usePathname } from "next/navigation"
import { useEffect, useRef, useState } from "react"

import { ThemeToggle } from "./ThemeToggle"
import { Wordmark } from "./Wordmark"

const NAV_LINKS = [
  { label: "Changelog", href: "/docs/changelog" },
  { label: "Docs", href: "/docs" },
] as const

const INSTALL_HREF = "/docs/getting-started/install"

type DrawerNavItem = { label: string; href: string }
type DrawerNavSection = { label: string; items: DrawerNavItem[] }

type HeaderProps = {
  docsSections?: DrawerNavSection[]
}

/**
 * Site-wide sticky header. Wordmark links home on the left; nav text
 * links, theme toggle, and the Install CTA sit on the right. Below `md`
 * the inline nav collapses into a single hamburger-triggered drawer that
 * holds both the main nav and the docs nav (when provided), so mobile
 * users get one unified menu instead of two. The hamburger is the
 * rightmost element; the Install CTA sits immediately to its left.
 */
export function Header({ docsSections = [] }: HeaderProps) {
  const [open, setOpen] = useState(false)
  const pathname = usePathname()
  const triggerRef = useRef<HTMLButtonElement | null>(null)
  const firstLinkRef = useRef<HTMLAnchorElement | null>(null)
  const [lastPathname, setLastPathname] = useState(pathname)

  // Close the drawer when the route changes (e.g. browser back/forward
  // while the drawer is open). In-drawer link clicks already close it
  // via onClick. This is the React-recommended "adjust state during
  // render" pattern, which avoids the cascading effects warning.
  if (lastPathname !== pathname) {
    setLastPathname(pathname)
    if (open) setOpen(false)
  }

  // Escape closes the drawer, and we lock background scroll while it's
  // open so the page underneath doesn't drift when the user pans.
  useEffect(() => {
    if (!open) return
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false)
    }
    window.addEventListener("keydown", onKey)
    const prevOverflow = document.body.style.overflow
    document.body.style.overflow = "hidden"
    return () => {
      window.removeEventListener("keydown", onKey)
      document.body.style.overflow = prevOverflow
    }
  }, [open])

  // Focus management: move focus into the drawer when it opens, return
  // it to the trigger when it closes. `wasOpen` keeps us from stealing
  // focus on the initial mount.
  const wasOpen = useRef(false)
  useEffect(() => {
    if (open) {
      firstLinkRef.current?.focus()
    } else if (wasOpen.current) {
      triggerRef.current?.focus()
    }
    wasOpen.current = open
  }, [open])

  const drawerSections: DrawerNavSection[] = [
    {
      label: "Menu",
      items: NAV_LINKS.map((link) => ({ label: link.label, href: link.href })),
    },
    ...docsSections.filter((section) => section.items.length > 0),
  ]

  return (
    <header className="sticky top-0 z-30 border-b border-gray-4 bg-bg">
      <div className="mx-auto flex h-6 w-full max-w-[88rem] items-center justify-between px-3 lg:px-4">
        <Link href="/" aria-label="Shelbi home" className="inline-flex">
          <Wordmark size="sm" />
        </Link>

        <div className="flex items-center gap-2 sm:gap-3">
          <nav
            aria-label="Primary"
            className="hidden items-center gap-3 font-sans text-sm text-gray-7 md:flex"
          >
            {NAV_LINKS.map((link) => (
              <Link
                key={link.href}
                href={link.href}
                className="transition-colors hover:text-fg"
              >
                {link.label}
              </Link>
            ))}
          </nav>

          <ThemeToggle />

          <Link
            href={INSTALL_HREF}
            className="inline-flex items-center rounded-sm border border-fg bg-fg px-2 py-0.5 font-sans text-sm font-medium text-bg transition-colors hover:bg-transparent hover:text-fg"
          >
            Install
          </Link>

          <button
            ref={triggerRef}
            type="button"
            onClick={() => setOpen((value) => !value)}
            aria-label={open ? "Close menu" : "Open menu"}
            aria-expanded={open}
            aria-controls="site-header-drawer"
            className="inline-flex h-4 w-4 items-center justify-center text-gray-7 transition-colors hover:text-fg md:hidden"
          >
            {open ? (
              <XMarkIcon className="h-3 w-3" aria-hidden="true" />
            ) : (
              <Bars3Icon className="h-3 w-3" aria-hidden="true" />
            )}
          </button>
        </div>
      </div>

      {open ? (
        <>
          <div
            aria-hidden="true"
            onClick={() => setOpen(false)}
            className="fixed inset-x-0 top-6 bottom-0 z-30 bg-bg/80 md:hidden"
          />
          <div
            id="site-header-drawer"
            role="dialog"
            aria-modal="true"
            aria-label="Site navigation"
            className="absolute inset-x-0 top-full z-40 max-h-[calc(100vh-3rem)] overflow-y-auto border-b border-gray-4 bg-bg md:hidden"
          >
            <nav
              aria-label="Mobile"
              className="mx-auto flex w-full max-w-[88rem] flex-col gap-3 px-3 py-3"
            >
              {drawerSections.map((section, sectionIndex) => (
                <div key={section.label || `_section_${sectionIndex}`}>
                  {section.label ? (
                    <h2 className="mb-1 font-mono text-xs font-medium tracking-wider text-gray-6 uppercase">
                      {section.label}
                    </h2>
                  ) : null}
                  <ul className="flex flex-col">
                    {section.items.map((item, itemIndex) => {
                      const isFirst = sectionIndex === 0 && itemIndex === 0
                      const isActive = pathname === item.href
                      return (
                        <li key={item.href}>
                          <Link
                            ref={isFirst ? firstLinkRef : undefined}
                            href={item.href}
                            aria-current={isActive ? "page" : undefined}
                            onClick={() => setOpen(false)}
                            className={[
                              "block py-0.5 font-sans text-base transition-colors",
                              isActive
                                ? "font-semibold text-fg"
                                : "text-gray-7 hover:text-fg",
                            ].join(" ")}
                          >
                            {item.label}
                          </Link>
                        </li>
                      )
                    })}
                  </ul>
                </div>
              ))}
            </nav>
          </div>
        </>
      ) : null}
    </header>
  )
}
