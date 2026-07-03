"use client"

import { ChevronRightIcon } from "@heroicons/react/24/outline"
import Link from "next/link"
import { usePathname } from "next/navigation"
import { useCallback, useRef, useState } from "react"

import type { DocGuide, DocNavItem, DocNavNode } from "@/lib/docs"

type DocsSidebarProps = {
  nodes: DocNavNode[]
}

/** Collect the keys of every guide that (transitively) contains `pathname`. */
function activeGuideKeys(nodes: DocNavNode[], pathname: string): string[] {
  const keys: string[] = []
  const visit = (items: DocNavNode[]) => {
    for (const item of items) {
      if (item.kind === "guide") {
        const overviewActive = item.url === pathname
        const childActive = containsUrl(item.children, pathname)
        if (overviewActive || childActive) keys.push(item.key)
        visit(item.children)
      } else if (item.kind === "section") {
        visit(item.items)
      }
    }
  }
  visit(nodes)
  return keys
}

/** Whether any nav item in the subtree links to `url`. */
function containsUrl(items: DocNavItem[], url: string): boolean {
  return items.some((item) => {
    if (item.url === url) return true
    return item.kind === "guide" && containsUrl(item.children, url)
  })
}

/**
 * Sticky docs sidebar. Pinned beside the article column at `md+`; on smaller
 * viewports the unified header drawer carries the docs nav, so this component
 * is hidden. Multi-page guides render as collapsible groups (the guide holding
 * the active page auto-expands); single pages render as plain links. Current-
 * page state is communicated with weight + a leading marker — never with hue
 * (see `site/AGENTS.md`). Arrow keys move focus and expand/collapse guides.
 */
export function DocsSidebar({ nodes }: DocsSidebarProps) {
  const pathname = usePathname()
  const navRef = useRef<HTMLElement | null>(null)

  const [expanded, setExpanded] = useState<Set<string>>(
    () => new Set(activeGuideKeys(nodes, pathname)),
  )

  // Keep the active page's guide open as the route changes, without collapsing
  // guides the reader opened by hand. Reconciling during render (rather than in
  // an effect) is React's recommended pattern for state derived from props —
  // the same approach `Header` uses to close its drawer on navigation.
  const [lastPathname, setLastPathname] = useState(pathname)
  if (lastPathname !== pathname) {
    setLastPathname(pathname)
    const active = activeGuideKeys(nodes, pathname)
    if (active.length > 0 && !active.every((key) => expanded.has(key))) {
      const next = new Set(expanded)
      active.forEach((key) => next.add(key))
      setExpanded(next)
    }
  }

  const toggle = useCallback((key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }, [])

  // Roving focus + disclosure control: Up/Down walk every focusable nav
  // element; Left/Right collapse/expand the focused guide toggle.
  const onKeyDown = (event: React.KeyboardEvent) => {
    const { key } = event
    if (key !== "ArrowDown" && key !== "ArrowUp" && key !== "ArrowRight" && key !== "ArrowLeft") {
      return
    }
    const nav = navRef.current
    if (!nav) return
    const focusables = Array.from(
      nav.querySelectorAll<HTMLElement>("a[href], button:not([disabled])"),
    )
    const current = document.activeElement as HTMLElement | null
    const index = current ? focusables.indexOf(current) : -1

    if (key === "ArrowDown" || key === "ArrowUp") {
      event.preventDefault()
      if (focusables.length === 0) return
      const delta = key === "ArrowDown" ? 1 : -1
      const nextIndex = Math.min(Math.max(index + delta, 0), focusables.length - 1)
      focusables[nextIndex]?.focus()
      return
    }

    // Left/Right only act on a guide's toggle button.
    const guideKey = current?.dataset.guideKey
    if (!guideKey) return
    event.preventDefault()
    if (key === "ArrowRight") setExpanded((prev) => new Set(prev).add(guideKey))
    else
      setExpanded((prev) => {
        const next = new Set(prev)
        next.delete(guideKey)
        return next
      })
  }

  return (
    <aside className="hidden md:sticky md:top-0 md:z-auto md:block md:h-screen md:overflow-y-auto md:py-4">
      <nav ref={navRef} aria-label="Documentation" onKeyDown={onKeyDown}>
        <ul className="flex flex-col gap-3">
          <li>
            <ul className="flex flex-col">
              <li>
                <NavLink href="/docs" label="Overview" active={pathname === "/docs"} />
              </li>
            </ul>
          </li>
          {nodes.map((node) => (
            <li key={nodeKey(node)}>
              {node.kind === "section" ? (
                <>
                  <h2 className="mb-1 font-mono text-xs font-medium tracking-wider text-gray-6 uppercase">
                    {node.label}
                  </h2>
                  <ul className="flex flex-col">
                    {node.items.map((item) => (
                      <NavItem
                        key={nodeKey(item)}
                        item={item}
                        pathname={pathname}
                        expanded={expanded}
                        onToggle={toggle}
                      />
                    ))}
                  </ul>
                </>
              ) : (
                <ul className="flex flex-col">
                  <NavItem
                    item={node}
                    pathname={pathname}
                    expanded={expanded}
                    onToggle={toggle}
                  />
                </ul>
              )}
            </li>
          ))}
        </ul>
      </nav>
    </aside>
  )
}

function nodeKey(node: DocNavNode): string {
  return `${node.kind}:${node.key}`
}

type NavItemProps = {
  item: DocNavItem
  pathname: string
  expanded: Set<string>
  onToggle: (key: string) => void
}

function NavItem({ item, pathname, expanded, onToggle }: NavItemProps) {
  if (item.kind === "leaf") {
    return (
      <li>
        <NavLink href={item.url} label={item.title} active={pathname === item.url} />
      </li>
    )
  }
  return (
    <GuideGroup
      guide={item}
      pathname={pathname}
      expanded={expanded}
      onToggle={onToggle}
    />
  )
}

type GuideGroupProps = {
  guide: DocGuide
  pathname: string
  expanded: Set<string>
  onToggle: (key: string) => void
}

function GuideGroup({ guide, pathname, expanded, onToggle }: GuideGroupProps) {
  const isOpen = expanded.has(guide.key)
  const panelId = `guide-${guide.key}`
  return (
    <li>
      <button
        type="button"
        data-guide-key={guide.key}
        aria-expanded={isOpen}
        aria-controls={panelId}
        onClick={() => onToggle(guide.key)}
        className="flex w-full items-center gap-1 py-0.5 pl-1 text-left text-sm text-gray-7 transition-colors hover:text-fg"
      >
        <ChevronRightIcon
          aria-hidden="true"
          className={[
            "h-2 w-2 shrink-0 transition-transform",
            isOpen ? "rotate-90" : "",
          ].join(" ")}
        />
        <span className="font-medium">{guide.title}</span>
      </button>
      {isOpen ? (
        <ul id={panelId} className="mt-0.5 mb-1 ml-2 flex flex-col border-l border-gray-4 pl-2">
          <li>
            <NavLink
              href={guide.url}
              label="Overview"
              active={pathname === guide.url}
            />
          </li>
          {guide.children.map((child) => (
            <NavItem
              key={nodeKey(child)}
              item={child}
              pathname={pathname}
              expanded={expanded}
              onToggle={onToggle}
            />
          ))}
        </ul>
      ) : null}
    </li>
  )
}

function NavLink({
  href,
  label,
  active,
}: {
  href: string
  label: string
  active: boolean
}) {
  return (
    <Link
      href={href}
      aria-current={active ? "page" : undefined}
      className={[
        "relative block py-0.5 pl-2 text-sm transition-colors",
        active
          ? "font-semibold text-fg before:absolute before:top-1/2 before:left-0 before:h-2 before:w-px before:-translate-y-1/2 before:bg-fg"
          : "text-gray-7 hover:text-fg",
      ].join(" ")}
    >
      {label}
    </Link>
  )
}
