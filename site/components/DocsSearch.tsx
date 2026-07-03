"use client"

import { MagnifyingGlassIcon } from "@heroicons/react/24/outline"
import { useRouter } from "next/navigation"
import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react"
import { createPortal } from "react-dom"
import type Fuse from "fuse.js"
import type { SearchRecord } from "@/lib/search"

// Module-scoped so the corpus + Fuse index are fetched and built exactly once
// per session — the first open pays the cost, every reopen is instant. The
// promise doubles as the in-flight guard.
let indexPromise: Promise<Fuse<SearchRecord>> | null = null

async function loadIndex(): Promise<Fuse<SearchRecord>> {
  if (!indexPromise) {
    indexPromise = (async () => {
      const [{ default: FuseCtor }, response] = await Promise.all([
        import("fuse.js"),
        fetch("/docs-search"),
      ])
      if (!response.ok) throw new Error(`search index ${response.status}`)
      const records: SearchRecord[] = await response.json()
      return new FuseCtor(records, {
        // Headings and titles are the strongest signal; body prose the
        // weakest. `ignoreLocation` lets a match anywhere in a long section
        // count, and a moderate threshold keeps fuzzy typos forgiving without
        // drowning results in noise.
        keys: [
          { name: "heading", weight: 5 },
          { name: "pageTitle", weight: 4 },
          { name: "summary", weight: 3 },
          { name: "content", weight: 1 },
        ],
        includeScore: true,
        ignoreLocation: true,
        threshold: 0.4,
        minMatchCharLength: 2,
      })
    })().catch((error) => {
      // Reset so a transient failure (offline first open) can retry next time.
      indexPromise = null
      throw error
    })
  }
  return indexPromise
}

const MAX_RESULTS = 30

type ResultGroup = {
  pageUrl: string
  pageTitle: string
  sectionLabel: string
  items: SearchRecord[]
}

/** Split the query into lowercased terms worth highlighting. */
function queryTerms(query: string): string[] {
  return query
    .toLowerCase()
    .split(/\s+/)
    .map((term) => term.trim())
    .filter((term) => term.length >= 2)
}

/** A short body excerpt centered on the first matching term. */
function makeSnippet(content: string, terms: string[]): string {
  if (!content) return ""
  const lower = content.toLowerCase()
  let idx = -1
  for (const term of terms) {
    const at = lower.indexOf(term)
    if (at !== -1 && (idx === -1 || at < idx)) idx = at
  }
  if (idx === -1) return content.length > 120 ? `${content.slice(0, 120)}…` : content
  const start = Math.max(0, idx - 40)
  const end = Math.min(content.length, idx + 100)
  return `${start > 0 ? "…" : ""}${content.slice(start, end).trim()}${
    end < content.length ? "…" : ""
  }`
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}

/** Render `text` with `terms` wrapped in a monochrome highlight. */
function Highlight({ text, terms }: { text: string; terms: string[] }) {
  if (terms.length === 0 || !text) return <>{text}</>
  // Capturing split interleaves the matched substrings with the gaps, so the
  // matched pieces are exactly those whose lowercased value is one of `terms`.
  const pattern = new RegExp(`(${terms.map(escapeRegExp).join("|")})`, "gi")
  const parts = text.split(pattern)
  return (
    <>
      {parts.map((part, i) =>
        terms.includes(part.toLowerCase()) ? (
          <mark
            key={i}
            className="rounded-[2px] bg-gray-3 font-semibold text-fg"
          >
            {part}
          </mark>
        ) : (
          <span key={i}>{part}</span>
        ),
      )}
    </>
  )
}

/**
 * The ⌘K docs command palette: a header trigger plus a centered modal that
 * searches every `content/docs/**` page with a client-side Fuse.js index
 * (loaded lazily on first open). Mounted once in the header, so its global
 * shortcut works from any page.
 */
export function DocsSearch() {
  const [open, setOpen] = useState(false)
  const [isMac, setIsMac] = useState(true)
  const triggerRef = useRef<HTMLButtonElement | null>(null)

  useEffect(() => {
    // One-shot platform probe — `navigator` only exists client-side, so the
    // default ("⌘K") renders on the server and this reconciles after mount.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setIsMac(/Mac|iPhone|iPad|iPod/i.test(navigator.platform))
  }, [])

  const close = useCallback(() => {
    setOpen(false)
    // Restore focus to the trigger the palette was opened from.
    triggerRef.current?.focus()
  }, [])

  // Global shortcut: ⌘K (macOS) / Ctrl+K (elsewhere). Requiring the modifier
  // means it never fires from plain typing, so text inputs keep the keystroke.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault()
        setOpen((value) => !value)
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [])

  const shortcutLabel = isMac ? "⌘K" : "Ctrl K"

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen(true)}
        aria-label="Search docs"
        aria-haspopup="dialog"
        aria-keyshortcuts="Meta+K Control+K"
        className="inline-flex h-4 items-center gap-1 rounded-sm border border-gray-4 px-1 text-gray-6 transition-colors hover:border-gray-5 hover:text-fg sm:gap-1.5 sm:pr-1 sm:pl-1.5"
      >
        <MagnifyingGlassIcon className="h-2 w-2" aria-hidden="true" />
        <span className="hidden font-sans text-sm sm:inline">Search docs</span>
        <kbd
          suppressHydrationWarning
          className="hidden rounded-[3px] border border-gray-4 bg-gray-2 px-1 font-mono text-xs text-gray-6 sm:inline"
        >
          {shortcutLabel}
        </kbd>
      </button>

      {open ? <SearchModal onClose={close} shortcutLabel={shortcutLabel} /> : null}
    </>
  )
}

function SearchModal({
  onClose,
  shortcutLabel,
}: {
  onClose: () => void
  shortcutLabel: string
}) {
  const router = useRouter()
  const listId = useId()
  const inputRef = useRef<HTMLInputElement | null>(null)
  const listRef = useRef<HTMLUListElement | null>(null)

  const [fuse, setFuse] = useState<Fuse<SearchRecord> | null>(null)
  const [loadError, setLoadError] = useState(false)
  const [query, setQuery] = useState("")
  const [selected, setSelected] = useState(0)
  const [lastQuery, setLastQuery] = useState(query)

  // Lazy-load the index on first open; cached module-side for later opens.
  useEffect(() => {
    let active = true
    loadIndex().then(
      (instance) => {
        if (active) setFuse(instance)
      },
      () => {
        if (active) setLoadError(true)
      },
    )
    return () => {
      active = false
    }
  }, [])

  // Focus the input on open and lock background scroll while open.
  useEffect(() => {
    inputRef.current?.focus()
    const prevOverflow = document.body.style.overflow
    document.body.style.overflow = "hidden"
    return () => {
      document.body.style.overflow = prevOverflow
    }
  }, [])

  const terms = useMemo(() => queryTerms(query), [query])

  const groups = useMemo<ResultGroup[]>(() => {
    const trimmed = query.trim()
    if (!fuse || trimmed.length === 0) return []
    const hits = fuse.search(trimmed, { limit: MAX_RESULTS }).map((h) => h.item)

    const order: string[] = []
    const byPage = new Map<string, SearchRecord[]>()
    for (const record of hits) {
      if (!byPage.has(record.pageUrl)) {
        byPage.set(record.pageUrl, [])
        order.push(record.pageUrl)
      }
      byPage.get(record.pageUrl)!.push(record)
    }
    return order.map((pageUrl) => {
      const items = byPage.get(pageUrl)!
      // Page-level record leads its group; heading hits follow in rank order.
      items.sort(
        (a, b) => (a.heading === null ? 0 : 1) - (b.heading === null ? 0 : 1),
      )
      return {
        pageUrl,
        pageTitle: items[0].pageTitle,
        sectionLabel: items[0].sectionLabel,
        items,
      }
    })
  }, [fuse, query])

  // Flat list mirrors visual order for keyboard navigation.
  const flat = useMemo(() => groups.flatMap((group) => group.items), [groups])

  // Reset the highlight whenever the query changes — done during render (the
  // React-recommended "adjust state" pattern) rather than in an effect.
  if (lastQuery !== query) {
    setLastQuery(query)
    setSelected(0)
  }

  const optionId = useCallback(
    (index: number) => `${listId}-option-${index}`,
    [listId],
  )

  // Keep the highlighted option scrolled into view.
  useEffect(() => {
    if (flat.length === 0) return
    document
      .getElementById(optionId(selected))
      ?.scrollIntoView({ block: "nearest" })
  }, [selected, flat.length, optionId])

  const go = useCallback(
    (record: SearchRecord) => {
      onClose()
      router.push(record.url)
    },
    [onClose, router],
  )

  const onKeyDown = (event: React.KeyboardEvent) => {
    switch (event.key) {
      case "Escape":
        event.preventDefault()
        onClose()
        break
      case "ArrowDown":
        event.preventDefault()
        setSelected((i) => (flat.length ? Math.min(i + 1, flat.length - 1) : 0))
        break
      case "ArrowUp":
        event.preventDefault()
        setSelected((i) => Math.max(i - 1, 0))
        break
      case "Enter": {
        event.preventDefault()
        const record = flat[selected]
        if (record) go(record)
        break
      }
      case "Tab":
        // Focus trap: the input is the palette's only tab stop, so keep it.
        event.preventDefault()
        inputRef.current?.focus()
        break
      default:
        break
    }
  }

  const hasQuery = query.trim().length > 0
  let flatIndex = -1

  const modal = (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center p-2 sm:p-4"
      role="presentation"
      onKeyDown={onKeyDown}
    >
      <div
        aria-hidden="true"
        onClick={onClose}
        className="absolute inset-0 bg-bg/70 backdrop-blur-sm"
      />

      <div
        role="dialog"
        aria-modal="true"
        aria-label="Search documentation"
        className="relative z-10 mt-[8vh] flex max-h-[70vh] w-full max-w-xl flex-col overflow-hidden rounded-md border border-gray-4 bg-bg shadow-2xl"
      >
        {/* Search input row */}
        <div className="flex items-center gap-1.5 border-b border-gray-4 px-2">
          <MagnifyingGlassIcon
            className="h-2.5 w-2.5 shrink-0 text-gray-6"
            aria-hidden="true"
          />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search the documentation…"
            role="combobox"
            aria-expanded={flat.length > 0}
            aria-controls={listId}
            aria-autocomplete="list"
            aria-activedescendant={
              flat.length > 0 ? optionId(selected) : undefined
            }
            autoComplete="off"
            autoCorrect="off"
            spellCheck={false}
            className="h-5 w-full bg-transparent font-sans text-base text-fg outline-none placeholder:text-gray-5"
          />
        </div>

        {/* Results / states */}
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
          {loadError ? (
            <p className="px-2 py-3 text-center font-sans text-sm text-gray-6">
              Couldn’t load the search index. Check your connection and try
              again.
            </p>
          ) : !hasQuery ? (
            <div className="px-2 py-4 text-center font-sans text-sm text-gray-6">
              {fuse
                ? "Search titles, sections, and page content."
                : "Loading search…"}
            </div>
          ) : flat.length === 0 ? (
            <p className="px-2 py-4 text-center font-sans text-sm text-gray-6">
              No results for{" "}
              <span className="font-medium text-fg">“{query.trim()}”</span>.
            </p>
          ) : (
            <ul ref={listRef} id={listId} role="listbox" className="py-1">
              {groups.map((group) => (
                <li key={group.pageUrl} role="presentation">
                  <div className="px-2 pt-1.5 pb-0.5 font-mono text-xs font-medium tracking-wider text-gray-6 uppercase">
                    {group.sectionLabel}
                    <span className="text-gray-5"> › </span>
                    {group.pageTitle}
                  </div>
                  <ul role="presentation">
                    {group.items.map((record) => {
                      flatIndex += 1
                      const index = flatIndex
                      const isSelected = index === selected
                      const isPage = record.heading === null
                      const title = isPage ? record.pageTitle : record.heading!
                      const snippet = isPage
                        ? record.summary ?? makeSnippet(record.content, terms)
                        : makeSnippet(record.content, terms)
                      return (
                        <li key={record.id} role="presentation">
                          <button
                            type="button"
                            id={optionId(index)}
                            role="option"
                            aria-selected={isSelected}
                            onClick={() => go(record)}
                            onMouseMove={() => setSelected(index)}
                            className={`block w-full px-2 py-1 text-left transition-colors ${
                              isSelected ? "bg-gray-2" : "bg-transparent"
                            }`}
                          >
                            <div className="flex items-baseline gap-1.5">
                              {!isPage ? (
                                <span
                                  aria-hidden="true"
                                  className="font-mono text-xs text-gray-5"
                                >
                                  #
                                </span>
                              ) : null}
                              <span className="font-sans text-sm font-medium text-fg">
                                <Highlight text={title} terms={terms} />
                              </span>
                            </div>
                            {snippet ? (
                              <p className="mt-0.5 line-clamp-1 font-sans text-xs text-gray-6">
                                <Highlight text={snippet} terms={terms} />
                              </p>
                            ) : null}
                          </button>
                        </li>
                      )
                    })}
                  </ul>
                </li>
              ))}
            </ul>
          )}
        </div>

        {/* Keyboard legend */}
        <div className="flex items-center gap-2 border-t border-gray-4 px-2 py-1 font-sans text-xs text-gray-6">
          <LegendKey label="↑↓" text="Navigate" />
          <LegendKey label="↵" text="Open" />
          <LegendKey label="Esc" text="Close" />
          <span className="ml-auto hidden sm:inline">{shortcutLabel}</span>
        </div>
      </div>
    </div>
  )

  return createPortal(modal, document.body)
}

function LegendKey({ label, text }: { label: string; text: string }) {
  return (
    <span className="inline-flex items-center gap-1">
      <kbd className="rounded-[3px] border border-gray-4 bg-gray-2 px-1 font-mono text-gray-6">
        {label}
      </kbd>
      <span className="hidden sm:inline">{text}</span>
    </span>
  )
}
