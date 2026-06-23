import { allComparisons, type Comparison } from "contentlayer/generated"

export { allComparisons }
export type { Comparison }

/**
 * Visible comparisons — everything in `content/vs/` except templates
 * (`_*` slugs). Sorted alphabetically by competitor name so the index
 * order is stable as new competitors get added.
 */
export const visibleComparisons: Comparison[] = [...allComparisons]
  .filter((c) => !c.slug.startsWith("_"))
  .sort((a, b) => a.competitor.localeCompare(b.competitor))

/** Resolve a comparison by its public slug. */
export function getComparisonBySlug(slug: string): Comparison | undefined {
  return visibleComparisons.find((c) => c.slug === slug)
}
