import GithubSlugger from "github-slugger"
import { allDocs, type Doc } from "contentlayer/generated"

export { allDocs }
export type { Doc }

/** Docs sorted by their `order` frontmatter, ascending. */
export const sortedDocs: Doc[] = [...allDocs].sort((a, b) => a.order - b.order)

/**
 * Resolve a doc from the `[[...slug]]` route segments, e.g.
 * `["getting-started", "install"]` → the doc whose flattened path is
 * `docs/getting-started/install`.
 */
export function getDocBySlug(slug: string[] | undefined): Doc | undefined {
  const path = ["docs", ...(slug ?? [])].join("/")
  return allDocs.find((doc) => doc._raw.flattenedPath === path)
}

export type DocSection = {
  /** The directory name beneath `docs/` (empty string for top-level docs). */
  section: string
  docs: Doc[]
}

/**
 * Group docs by section, each section's docs sorted by `order`, and the
 * sections themselves ordered by their lowest-ordered doc. Sectionless docs
 * (placed directly in `docs/`) collapse into a leading group with `section: ""`.
 */
export function getSections(): DocSection[] {
  const bySection = new Map<string, Doc[]>()
  for (const doc of sortedDocs) {
    const group = bySection.get(doc.section) ?? []
    group.push(doc)
    bySection.set(doc.section, group)
  }
  return [...bySection.entries()]
    .map(([section, docs]) => ({ section, docs }))
    .sort((a, b) => a.docs[0].order - b.docs[0].order)
}

/** Title-case a kebab-cased section directory name. */
export function humanizeSection(section: string): string {
  return section
    .split("-")
    .map((word) => (word ? word[0].toUpperCase() + word.slice(1) : word))
    .join(" ")
}

export type Heading = { depth: 2 | 3; text: string; id: string }

/**
 * Extract H2/H3 headings from a doc's raw MDX. IDs are generated with the
 * same `github-slugger` that backs `rehype-slug`, so they match the IDs
 * stamped onto the rendered headings — anchor links and scroll-spy stay
 * in sync.
 */
export function extractHeadings(doc: Doc): Heading[] {
  const slugger = new GithubSlugger()
  const lines = doc.body.raw.split("\n")
  const headings: Heading[] = []
  let inFence = false
  for (const line of lines) {
    if (/^\s{0,3}```/.test(line)) {
      inFence = !inFence
      continue
    }
    if (inFence) continue
    const match = /^(#{2,3})\s+(.+?)\s*$/.exec(line)
    if (!match) continue
    const depth = match[1].length as 2 | 3
    const text = stripInlineMarkdown(match[2])
    if (!text) continue
    headings.push({ depth, text, id: slugger.slug(text) })
  }
  return headings
}

function stripInlineMarkdown(text: string): string {
  return text
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .trim()
}

/**
 * Prev/next siblings for a doc, walking the flat reading order produced by
 * `getSections()` — within a section by `order`, then across sections by the
 * same ordering used in the sidebar. Returns `undefined` at the boundaries.
 */
export function getPrevNext(doc: Doc): { prev?: Doc; next?: Doc } {
  const flat = getSections().flatMap((s) => s.docs)
  const idx = flat.findIndex((d) => d.url === doc.url)
  if (idx === -1) return {}
  return {
    prev: idx > 0 ? flat[idx - 1] : undefined,
    next: idx < flat.length - 1 ? flat[idx + 1] : undefined,
  }
}

const REPO_EDIT_BASE = "https://github.com/jlong/shelbi/edit/main/site/content"

/** Resolve the `edit on GitHub` URL for a doc, pointing at its source file. */
export function getEditUrl(doc: Doc): string {
  return `${REPO_EDIT_BASE}/${doc._raw.sourceFilePath}`
}
