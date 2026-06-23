"use client"

import { Bars3Icon, XMarkIcon } from "@heroicons/react/24/outline"
import Link from "next/link"
import { useEffect, useState } from "react"
import { Wordmark } from "./Wordmark"

const NAV_LINKS = [
  { label: "Changelog", href: "/docs/changelog" },
  { label: "Docs", href: "/docs" },
] as const

const INSTALL_HREF = "/docs/getting-started/install"

/**
 * Site-wide sticky header. Wordmark links home on the left; nav text
 * links + the Install CTA sit on the right. Below `sm`, the text links
 * collapse into a hamburger-triggered drawer — the Install button stays
 * visible because it's the primary CTA.
 */
export function Header() {
  const [open, setOpen] = useState(false)

  useEffect(() => {
    if (!open) return
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false)
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [open])

  return (
    <header className="sticky top-0 z-30 border-b border-gray-4 bg-bg">
      <div className="mx-auto flex h-6 w-full max-w-[88rem] items-center justify-between px-3 lg:px-4">
        <Link href="/" aria-label="Shelbi home" className="inline-flex">
          <Wordmark size="sm" />
        </Link>

        <div className="flex items-center gap-2 sm:gap-3">
          <nav
            aria-label="Primary"
            className="hidden items-center gap-3 font-sans text-sm text-gray-7 sm:flex"
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

          <button
            type="button"
            onClick={() => setOpen((value) => !value)}
            aria-label={open ? "Close navigation" : "Open navigation"}
            aria-expanded={open}
            aria-controls="site-header-drawer"
            className="inline-flex h-4 w-4 items-center justify-center text-gray-7 transition-colors hover:text-fg sm:hidden"
          >
            {open ? (
              <XMarkIcon className="h-3 w-3" aria-hidden="true" />
            ) : (
              <Bars3Icon className="h-3 w-3" aria-hidden="true" />
            )}
          </button>

          <Link
            href={INSTALL_HREF}
            className="inline-flex items-center rounded-sm border border-fg bg-fg px-2 py-0.5 font-sans text-sm font-medium text-bg transition-colors hover:bg-transparent hover:text-fg"
          >
            Install
          </Link>
        </div>
      </div>

      {open ? (
        <>
          <div
            aria-hidden="true"
            onClick={() => setOpen(false)}
            className="fixed inset-x-0 top-6 bottom-0 z-30 bg-bg/80 sm:hidden"
          />
          <div
            id="site-header-drawer"
            className="absolute inset-x-0 top-full z-40 border-b border-gray-4 bg-bg sm:hidden"
          >
            <nav
              aria-label="Primary mobile"
              className="mx-auto flex w-full max-w-[88rem] flex-col gap-2 px-3 py-3"
            >
              {NAV_LINKS.map((link) => (
                <Link
                  key={link.href}
                  href={link.href}
                  onClick={() => setOpen(false)}
                  className="font-sans text-base text-gray-7 transition-colors hover:text-fg"
                >
                  {link.label}
                </Link>
              ))}
            </nav>
          </div>
        </>
      ) : null}
    </header>
  )
}
