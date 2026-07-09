"use client"

import { Bars3Icon, XMarkIcon } from "@heroicons/react/24/outline"
import Link from "next/link"
import { usePathname } from "next/navigation"
import { useEffect, useRef, useState, type SVGProps } from "react"

import { DocsSearch } from "./DocsSearch"
import { ThemeToggle } from "./ThemeToggle"
import { WordmarkSvg } from "./Wordmark"

const NAV_LINKS = [
  { label: "Documentation", href: "/docs" },
  { label: "Changelog", href: "/docs/changelog" },
  { label: "Discord", href: "/discord" },
] as const

const INSTALL_HREF = "/docs/guides/getting-started/install"

// Header bar height in px — used to shrink the IntersectionObserver
// root so the hero is considered "off-screen" the moment its bottom
// passes under the header (not when it reaches the viewport top).
const HEADER_HEIGHT_PX = 72

const GITHUB_HREF = "https://github.com/jlong/shelbi"

// Heroicons has no brand marks, so the GitHub octocat is inlined here
// (official mark path, fill-based) rather than adding an icon dependency.
function GitHubIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" {...props}>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z" />
    </svg>
  )
}

type DrawerNavItem = { label: string; href: string }
type DrawerNavSection = { label: string; items: DrawerNavItem[] }

type HeaderProps = {
  docsSections?: DrawerNavSection[]
}

/**
 * Site-wide sticky header. Wordmark links home on the left; nav text
 * links sit center-right, with the Install CTA to their right and the
 * theme toggle flush to the far right on desktop. Below `md`, the
 * inline nav collapses into a single hamburger-triggered drawer that
 * holds both the main nav and the docs nav (when provided), so mobile
 * users get one unified menu instead of two. The hamburger is the
 * rightmost element on mobile; the Install CTA sits immediately to
 * its left.
 *
 * The logo is always visible. On the home page only, the header's
 * bottom border fades in once the user scrolls past the hero, so the
 * chrome reads as borderless over the hero and gains a divider once
 * content scrolls beneath it. Every other route keeps the border at
 * all scroll positions.
 */
export function Header({ docsSections = [] }: HeaderProps) {
  const pathname = usePathname()
  const isHome = pathname === "/"

  const [open, setOpen] = useState(false)
  const triggerRef = useRef<HTMLButtonElement | null>(null)
  const firstLinkRef = useRef<HTMLAnchorElement | null>(null)
  const [lastPathname, setLastPathname] = useState(pathname)

  // SSR-safe default keyed off the route — non-home pages render
  // opaque in the very first server frame so there's no transparent
  // flash before the client effect runs. Home defaults to transparent
  // (matches the at-top state); the IntersectionObserver below flips
  // it once the hero scrolls out of view.
  const [scrolled, setScrolled] = useState(!isHome)

  // Close the drawer when the route changes (e.g. browser back/forward
  // while the drawer is open). In-drawer link clicks already close it
  // via onClick. This is the React-recommended "adjust state during
  // render" pattern, which avoids the cascading effects warning.
  if (lastPathname !== pathname) {
    setLastPathname(pathname)
    if (open) setOpen(false)
  }

  useEffect(() => {
    // Route-driven sync — we have to reconcile the chrome with the
    // new route synchronously when `isHome` changes. The eslint rule
    // warns about the shape, not the cost; one render per nav is fine
    // and the alternatives (deriving from pathname + state) hide the
    // observer lifecycle. Same trade-off ThemeToggle makes.
    /* eslint-disable react-hooks/set-state-in-effect */
    if (!isHome) {
      setScrolled(true)
      return
    }
    setScrolled(false)

    const hero = document.querySelector<HTMLElement>("[data-hero]")
    if (!hero) {
      // Safety: if the hero isn't on the page, fall back to opaque so
      // the chrome doesn't get permanently stuck in the transparent
      // state on some hypothetical alt home layout.
      setScrolled(true)
      return
    }
    /* eslint-enable react-hooks/set-state-in-effect */

    const observer = new IntersectionObserver(
      (entries) => {
        const entry = entries[0]
        if (!entry) return
        // While ANY part of the hero is visible below the header,
        // the header stays transparent. Once the hero's bottom crosses
        // above the header's bottom, the header becomes opaque.
        setScrolled(!entry.isIntersecting)
      },
      {
        rootMargin: `-${HEADER_HEIGHT_PX}px 0px 0px 0px`,
        threshold: 0,
      },
    )
    observer.observe(hero)
    return () => observer.disconnect()
  }, [isHome])

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

  const transparent = !scrolled

  return (
    <header
      className={`sticky top-0 z-30 bg-bg border-b transition-[border-color] duration-200 ${
        transparent ? "border-transparent" : "border-gray-4"
      }`}
    >
      <div className="flex w-full items-center gap-2 p-2.5 sm:gap-3">
        <Link
          href="/"
          aria-label="Shelbi home"
          className="inline-flex shrink-0 text-fg"
        >
          <WordmarkSvg
            style={{ height: "20px", width: `${(20 * 684) / 108}px` }}
          />
        </Link>

        {/* Mobile cluster: hugs right via ml-auto. On sm+ it switches to
            `display: contents` so its children become direct flex items
            of the outer header bar — that lets `order` + `ml-auto`
            below split the row into the [logo][nav install][toggle]
            three-region desktop layout. */}
        <div className="ml-auto flex items-center gap-2 md:contents">
          {/* Docs command palette trigger. On desktop it leads the right
              cluster (carries the `ml-auto` that used to sit on the nav);
              on mobile it collapses to a magnifier icon. */}
          <div className="flex items-center md:order-2 md:ml-auto">
            <DocsSearch />
          </div>

          <nav
            aria-label="Primary"
            className="hidden items-center gap-3 font-sans text-sm text-gray-7 md:flex md:order-2"
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

          {/* Shares order-4 with the theme toggle wrapper below; DOM
              order keeps it to the toggle's left on desktop. */}
          <a
            href={GITHUB_HREF}
            target="_blank"
            rel="noopener noreferrer"
            aria-label="Shelbi on GitHub"
            className="inline-flex h-3 w-3 items-center justify-center text-gray-7 transition-colors hover:text-fg md:order-4"
          >
            <GitHubIcon className="h-2.5 w-2.5" aria-hidden="true" />
          </a>

          {/* Wrapped so the desktop `order` class can sit on a real
              flex item — ThemeToggle's own root isn't a single element
              we can style. */}
          <div className="flex items-center md:order-4">
            <ThemeToggle />
          </div>

          <Link
            href={INSTALL_HREF}
            className="inline-flex items-center rounded-sm border border-fg bg-fg px-2 py-0.5 font-sans text-sm font-medium text-bg transition-colors hover:bg-transparent hover:text-fg md:order-3"
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
            className="fixed inset-x-0 top-9 bottom-0 z-30 bg-bg/80 md:hidden"
          />
          <div
            id="site-header-drawer"
            role="dialog"
            aria-modal="true"
            aria-label="Site navigation"
            className="absolute inset-x-0 top-full z-40 max-h-[calc(100vh-72px)] overflow-y-auto border-b border-gray-4 bg-bg md:hidden"
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
