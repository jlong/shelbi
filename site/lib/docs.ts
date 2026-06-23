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
